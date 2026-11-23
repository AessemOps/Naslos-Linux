//! The MCP tool registry (FR-MCP): a declarative table mapping tool names to
//! the existing API routes, plus the dispatcher that calls them through the
//! internal router.
//!
//! The registry stays data-only; validation and error shapes come from the
//! real handlers. `tools/list` filters by the calling token's scope;
//! `tools/call` re-checks the scope and, for `full` (destructive) tools,
//! requires `confirm` to equal the resolved target argument.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request};
use rmcp::model::JsonObject;
use serde_json::Value;

use crate::auth::UserInfo;
use crate::mcp::store::{Scope, TokenAuth};
use crate::state::AppState;

/// One registry entry. `path` may contain `{param}` placeholders, resolved
/// from the call arguments (the same value the `confirm` check compares
/// against, so a `full` tool cannot be redirected between the check and the
/// dispatch).
#[derive(Clone)]
pub struct ToolDef {
    pub name: &'static str,
    pub desc: &'static str,
    pub level: Scope,
    pub method: Method,
    pub path: &'static str,
    /// Argument names mapped into the query string (GET/DELETE tools).
    pub query: &'static [&'static str],
    /// `full` (destructive) tools must be confirmed; `tools/call` refuses the
    /// dispatch unless `args.confirm` equals this target.
    pub confirm: Confirm,
    /// JSON Schema for the arguments object.
    pub schema: fn() -> Value,
    /// Special tools (no route dispatch): handled in [`dispatch`].
    pub special: Special,
}

/// What the `confirm` argument of a destructive tool must equal.
#[derive(Default, Debug, Clone, Copy, PartialEq)]
pub enum Confirm {
    #[default]
    /// Not a destructive tool.
    No,
    /// The named argument (also used in the path/query), e.g. `name`. The
    /// argument must be a non-empty string; an absent/empty target is
    /// refused rather than silently skipping the confirm check (R-025).
    Arg(&'static str),
    /// A fixed literal, for targets that are not an argument (`reboot`).
    Literal(&'static str),
    /// The named argument normally, but a fixed literal when `flag` is true,
    /// so an irreversible variant cannot be confirmed by repeating a name
    /// (R-025, `buddy_identity_create {replace: true}`).
    ArgOrLiteral {
        arg: &'static str,
        flag: &'static str,
        literal: &'static str,
    },
}

#[derive(Default, Clone, Copy, PartialEq)]
pub enum Special {
    #[default]
    None,
    /// Bounded pod-log snapshot (`pods.rs`).
    PodLogs,
}

impl ToolDef {
    /// The MCP `Tool` for `tools/list`, with scope-appropriate annotations.
    pub fn mcp_tool(&self) -> rmcp::model::Tool {
        let mut tool = rmcp::model::Tool::new(self.name, self.desc, json_object((self.schema)()));
        tool.annotations = Some(
            rmcp::model::ToolAnnotations::new()
                .read_only(self.level == Scope::Read)
                .destructive(self.level == Scope::Full)
                .idempotent(false),
        );
        tool
    }
}

/// Every tool an operator may reach with a `write` token. Reviewed one by
/// one (R-009): identity, credential and egress tools are `full`, so adding a
/// `Write` tool means adding it here (and reviewing the entry) or the
/// registry test fails.
pub const WRITE_ALLOWLIST: &[&str] = &[
    // App lifecycle. Chart sources are operator-configured; no identity,
    // credential or arbitrary-URL field is writable through these.
    "app_install",
    "app_update",
    "app_exposure_set",
    "app_dns_reconcile",
    "app_namespaces_set",
    // Storage.
    "dataset_create",
    "dataset_register",
    "pool_import",
    // Shares (data access, not identities).
    "share_create",
    "share_update",
    "share_apply",
    "share_folder_create",
    // Virtual machines. `vm_image_import_url` is `full` (egress).
    "vm_create",
    "vm_update",
    "vm_start",
    "vm_stop",
    "vm_restart",
    "vm_snapshot_create",
];

/// `Full` tools whose control is the scope itself rather than a repeat-the-
/// target confirm. They still need an explicit reviewed entry so a new
/// `Full` tool cannot silently ship without one or the other.
pub const FULL_NO_CONFIRM_ALLOWLIST: &[&str] = &[
    "user_create",
    "user_update",
    "user_set_password",
    "user_enable",
    "user_disable",
    "group_create",
    "group_update",
    "notifications_set",
    "notifications_test",
    "domain_create",
    "domain_update",
    "domain_sso_set",
    "ddns_create",
    "ddns_update",
    "ddns_run",
    "source_add",
    "source_refresh",
    "vm_image_import_url",
    "buddy_peer_add",
    // Buddy sends and schedules take an arbitrary receiver base URL, so they
    // are `full` (arbitrary egress); the scope is their control (B-9).
    "buddy_send",
    "buddy_schedule_create",
    "system_updates_check",
];

/// Tools whose classification must stay `full`: identities, credentials and
/// egress (R-009). Kept explicit so a future level change is a test failure.
pub const FULL_ONLY_TOOLS: &[&str] = &[
    "user_create",
    "user_update",
    "user_set_password",
    "user_enable",
    "user_disable",
    "group_create",
    "group_update",
    "user_delete",
    "group_delete",
    "notifications_set",
    "notifications_test",
    "domain_create",
    "domain_update",
    "domain_sso_set",
    "ddns_create",
    "ddns_update",
    "ddns_run",
    "source_add",
    "source_refresh",
    "vm_image_import_url",
    "buddy_peer_add",
    // Arbitrary egress: a buddy send or schedule ships sealed backups to the
    // URL it is given.
    "buddy_send",
    "buddy_schedule_create",
    "system_updates_check",
];

pub fn all_tools() -> &'static [ToolDef] {
    use std::sync::OnceLock;
    static ALL: OnceLock<Vec<ToolDef>> = OnceLock::new();
    ALL.get_or_init(|| {
        let mut tools = Vec::new();
        tools.extend_from_slice(system::TOOLS);
        tools.extend_from_slice(disks::TOOLS);
        tools.extend_from_slice(datasets::TOOLS);
        tools.extend_from_slice(shares::TOOLS);
        tools.extend_from_slice(users::TOOLS);
        tools.extend_from_slice(apps::TOOLS);
        tools.extend_from_slice(domains::TOOLS);
        tools.extend_from_slice(network::TOOLS);
        tools.extend_from_slice(notifications::TOOLS);
        tools.extend_from_slice(virt::TOOLS);
        tools.extend_from_slice(buddy::TOOLS);
        tools.extend_from_slice(pods::TOOLS);
        tools
    })
}

pub fn find_tool(name: &str) -> Option<&'static ToolDef> {
    all_tools().iter().find(|t| t.name == name)
}

/// What a dispatch produced: the HTTP status plus the response body text
/// (the API's JSON result or error shape).
#[derive(Debug)]
pub struct Outcome {
    pub status: u16,
    pub body: String,
}

/// Why `tools/call` refused before dispatching. These map onto
/// `CallToolResult::error` — the caller's LLM must read them — not onto
/// JSON-RPC protocol errors.
#[derive(Debug)]
pub enum CallError {
    UnknownTool,
    Scope { have: Scope, need: Scope },
    Confirm { target: String },
    ConfirmTarget { param: &'static str },
    BadPath { param: &'static str },
    AuditUnavailable(String),
    AuditWriteFailed(String),
    Internal(String),
}

impl CallError {
    pub fn message(&self) -> String {
        match self {
            CallError::UnknownTool => "unknown tool".into(),
            CallError::Scope { have, need } => format!(
                "this token's scope is {:?} but the tool requires {:?}",
                have.as_str(),
                need.as_str()
            ),
            CallError::Confirm { target } => format!(
                "destructive tool: the 'confirm' argument must be exactly {target:?} \
                 (the target you intend to act on)"
            ),
            CallError::ConfirmTarget { param } => format!(
                "destructive tool: the {param:?} argument is required and must be a \
                 non-empty string, and 'confirm' must repeat it exactly"
            ),
            CallError::BadPath { param } => {
                format!("the {param:?} argument is required and must be a string")
            }
            CallError::AuditUnavailable(e) => format!(
                "audit unavailable: this tool did not run; mutating calls are refused \
                 until the audit log is repaired ({e})"
            ),
            CallError::AuditWriteFailed(e) => format!(
                "audit_write_failed: the tool may have been applied, but its outcome \
                 could not be recorded ({e})"
            ),
            CallError::Internal(e) => format!("dispatch failed: {e}"),
        }
    }
}

/// Resolve `{param}` placeholders from the arguments. Every placeholder must
/// be present as a non-empty string; the resolved value is what the
/// `confirm` check compares against (no second lookup).
fn resolve_path(template: &str, args: &JsonObject) -> Result<String, CallError> {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some((head, tail)) = rest.split_once('{') {
        let (param, after) = tail.split_once('}').ok_or(CallError::Internal(
            "malformed path template in the tool registry".into(),
        ))?;
        let value = args
            .get(param)
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .ok_or(CallError::BadPath {
                param: static_param(param),
            })?;
        out.push_str(head);
        push_encoded(&mut out, value);
        rest = after;
    }
    out.push_str(rest);
    Ok(out)
}

/// A confirm target argument: present, a string and non-empty.
fn confirm_target(args: &JsonObject, param: &str) -> Option<String> {
    args.get(param)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

/// Destructive tools whose confirm target is a tuple of the named arguments,
/// joined with `@` in order: `vm_snapshot_restore` = `<name>@<snap>` and
/// `dataset_delete` = `<name>@<recursive>`. Repeating only one field would let
/// a confirmation approve a different snapshot or a recursive delete (B-14).
const CONFIRM_TUPLE_TOOLS: &[(&str, &[&str])] = &[
    ("vm_snapshot_restore", &["name", "snap"]),
    ("dataset_delete", &["name", "recursive"]),
];

/// Destructive tools whose confirm target is
/// `<argument>@<sha256 of the arguments without confirm>`. The hash binds the
/// confirmation to the whole payload, so a caller cannot confirm a link while
/// changing its addressing, gateway or MTU (B-14).
const CONFIRM_PAYLOAD_HASH_TOOLS: &[(&str, &str)] = &[("network_change_apply", "link")];

/// A non-empty argument rendered into a confirm tuple: strings as-is,
/// booleans as `true`/`false`, numbers in decimal.
fn confirm_value(args: &JsonObject, param: &str) -> Option<String> {
    match args.get(param) {
        Some(Value::String(text)) if !text.is_empty() => Some(text.clone()),
        Some(Value::Bool(flag)) => Some(flag.to_string()),
        Some(Value::Number(number)) => Some(number.to_string()),
        _ => None,
    }
}

/// JSON with object keys sorted, so the payload hash does not depend on the
/// client's key order.
fn canonical_json(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String((*key).clone()).to_string());
                out.push(':');
                if let Some(value) = map.get(*key) {
                    canonical_json(value, out);
                }
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                canonical_json(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// The `<sha256>` half of a payload-confirm target. Shared with the tests so
/// they build the same target the dispatcher checks.
fn payload_confirm_hash(args: &JsonObject) -> String {
    let mut without_confirm = args.clone();
    without_confirm.remove("confirm");
    let mut canonical = String::new();
    canonical_json(&Value::Object(without_confirm), &mut canonical);
    use sha2::Digest;
    let digest = sha2::Sha256::digest(canonical.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The value an API handler expects in its own `?confirm=` query argument.
/// Some destructive routes (dataset/pool delete, VM delete, VM snapshot
/// restore) re-check `confirm` themselves; the MCP safety target can be a
/// tuple or payload hash, so the handler gets the primary target value (the
/// first tuple field / the link), never the composite string.
fn handler_confirm_value(tool: &ToolDef, args: &JsonObject) -> Option<String> {
    if let Some((_, fields)) = CONFIRM_TUPLE_TOOLS
        .iter()
        .find(|(name, _)| *name == tool.name)
    {
        return fields.first().and_then(|field| confirm_value(args, field));
    }
    if let Some((_, field)) = CONFIRM_PAYLOAD_HASH_TOOLS
        .iter()
        .find(|(name, _)| *name == tool.name)
    {
        return confirm_value(args, field);
    }
    match tool.confirm {
        Confirm::No => None,
        Confirm::Arg(param) => confirm_target(args, param),
        Confirm::Literal(literal) => Some(literal.to_string()),
        Confirm::ArgOrLiteral { arg, flag, literal } => {
            if args.get(flag).and_then(Value::as_bool).unwrap_or(false) {
                Some(literal.to_string())
            } else {
                confirm_target(args, arg)
            }
        }
    }
}

/// The multi-field confirm target for `tool`, when its registry `Confirm`
/// would repeat only one field of a multi-field action.
fn tuple_confirm_target(tool: &ToolDef, args: &JsonObject) -> Option<Result<String, CallError>> {
    if let Some((_, fields)) = CONFIRM_TUPLE_TOOLS
        .iter()
        .find(|(name, _)| *name == tool.name)
    {
        let mut values = Vec::with_capacity(fields.len());
        for field in *fields {
            match confirm_value(args, field) {
                Some(value) => values.push(value),
                None => {
                    return Some(Err(CallError::ConfirmTarget {
                        param: static_param(field),
                    }))
                }
            }
        }
        return Some(Ok(values.join("@")));
    }
    if let Some((_, field)) = CONFIRM_PAYLOAD_HASH_TOOLS
        .iter()
        .find(|(name, _)| *name == tool.name)
    {
        return Some(match confirm_value(args, field) {
            Some(value) => Ok(format!("{value}@{}", payload_confirm_hash(args))),
            None => Err(CallError::ConfirmTarget {
                param: static_param(field),
            }),
        });
    }
    None
}

/// The registry only uses a fixed set of placeholder names; map any future
/// stragglers onto `name` so the error text stays `&'static`.
fn static_param(param: &str) -> &'static str {
    match param {
        "pool" => "pool",
        "uid" => "uid",
        "cn" => "cn",
        "id" => "id",
        "service" => "service",
        "snap" => "snap",
        "recursive" => "recursive",
        _ => "name",
    }
}

/// Percent-encode a path segment (RFC 3986), so an argument can never
/// reshape the path structure.
fn push_encoded(out: &mut String, value: &str) {
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
}

fn query_string(tool: &ToolDef, args: &JsonObject) -> String {
    if tool.query.is_empty() {
        return String::new();
    }
    let mut pairs = Vec::new();
    for key in tool.query {
        let Some(value) = args.get(*key) else {
            continue;
        };
        let encoded = match value {
            Value::String(text) => Some(text.clone()),
            Value::Bool(flag) => Some(flag.to_string()),
            Value::Number(number) => Some(number.to_string()),
            _ => None,
        };
        if let Some(text) = encoded {
            let mut pair = String::with_capacity(key.len() + text.len() + 2);
            push_encoded(&mut pair, key);
            pair.push('=');
            push_encoded(&mut pair, &text);
            pairs.push(pair);
        }
    }
    if pairs.is_empty() {
        String::new()
    } else {
        format!("?{}", pairs.join("&"))
    }
}

/// Dispatch one `tools/call`. All rejections are fallible-by-return: nothing
/// here may panic (the release profile is `panic = "abort"`, see the Buddy
/// note in `server/mod.rs`, AUD-2611-T6-1).
pub async fn dispatch(
    state: &Arc<AppState>,
    token: &TokenAuth,
    name: &str,
    args: &JsonObject,
) -> Result<Outcome, CallError> {
    let Some(tool) = find_tool(name) else {
        record_mcp_refusal(state, token, name, None, "failed", "unknown_tool").await;
        return Err(CallError::UnknownTool);
    };
    dispatch_tool(state, token, tool, args).await
}

/// The dispatch half of [`dispatch`], taking the registry entry directly so
/// tests can drive a fabricated (deliberately misclassified) tool.
async fn dispatch_tool(
    state: &Arc<AppState>,
    token: &TokenAuth,
    tool: &ToolDef,
    args: &JsonObject,
) -> Result<Outcome, CallError> {
    let name = tool.name;

    // Scope: re-checked here even though tools/list filtered, because a
    // client may call an unlisted tool.
    if !token.scope.allows(tool.level) {
        record_mcp_refusal(state, token, name, Some(tool), "denied", "scope").await;
        return Err(CallError::Scope {
            have: token.scope,
            need: tool.level,
        });
    }

    // Destructive tools: confirm must equal the resolved target, checked
    // before any dispatch. The target comes from the same resolved argument
    // used in the path/query, never a second lookup. A missing or empty
    // target is refused: it must never make the check vacuous (R-025).
    // Tools whose action spans several fields use a tuple or payload-hash
    // target instead of the single registry argument (B-14).
    let target = match tuple_confirm_target(tool, args) {
        Some(Ok(target)) => target,
        Some(Err(err)) => {
            record_mcp_refusal(state, token, name, Some(tool), "denied", "confirm_target").await;
            return Err(err);
        }
        None => match tool.confirm {
            Confirm::No => String::new(),
            Confirm::Arg(param) => match confirm_target(args, param) {
                Some(value) => value,
                None => {
                    record_mcp_refusal(state, token, name, Some(tool), "denied", "confirm_target")
                        .await;
                    return Err(CallError::ConfirmTarget {
                        param: static_param(param),
                    });
                }
            },
            Confirm::Literal(lit) => lit.to_string(),
            Confirm::ArgOrLiteral { arg, flag, literal } => {
                if args.get(flag).and_then(Value::as_bool).unwrap_or(false) {
                    literal.to_string()
                } else {
                    match confirm_target(args, arg) {
                        Some(value) => value,
                        None => {
                            record_mcp_refusal(
                                state,
                                token,
                                name,
                                Some(tool),
                                "denied",
                                "confirm_target",
                            )
                            .await;
                            return Err(CallError::ConfirmTarget {
                                param: static_param(arg),
                            });
                        }
                    }
                }
            }
        },
    };
    if !target.is_empty() {
        let confirm = args.get("confirm").and_then(Value::as_str).unwrap_or("");
        if confirm != target {
            record_mcp_refusal(state, token, name, Some(tool), "denied", "confirm_required").await;
            return Err(CallError::Confirm { target });
        }
    }

    match tool.special {
        Special::PodLogs => {
            let outcome = pods::pod_logs(state, args).await;
            let status = match &outcome {
                Ok(result) => result.status,
                Err(_) => 400,
            };
            record_mcp_read(state, token, tool, tool.path, status).await;
            return outcome;
        }
        Special::None => {}
    }

    let path = match resolve_path(tool.path, args) {
        Ok(path) => path,
        Err(err) => {
            record_mcp_refusal(state, token, name, Some(tool), "failed", "bad_path").await;
            return Err(err);
        }
    };
    let mut uri = format!("{path}{}", query_string(tool, args));
    // Several destructive handlers re-check `?confirm=` against the primary
    // target. The MCP safety target may be a tuple or payload hash, so append
    // the plain primary value the handler expects (B-14).
    if let Some(confirm) = handler_confirm_value(tool, args) {
        uri.push(if uri.contains('?') { '&' } else { '?' });
        uri.push_str("confirm=");
        push_encoded(&mut uri, &confirm);
    }
    let audit_target = crate::rbac::action_target(tool.path, &path);
    let audit = crate::audit::Pending::mcp(
        &token.id,
        &token.name,
        token.scope.as_str(),
        tool.name,
        &tool.method,
        tool.path,
        &path,
        audit_target,
    );
    // A mutation is never dispatched without a durable begin record. Reads
    // get one durable end record after dispatch instead.
    let mut audit = if crate::rbac::is_unsafe(&tool.method) {
        Some(state.audit.begin_async(audit).await.map_err(|e| {
            degrade_mcp(state, &e);
            CallError::AuditUnavailable(e)
        })?)
    } else {
        None
    };

    let mut builder = Request::builder().method(tool.method.clone()).uri(&uri);
    // GET/DELETE tools take their input as path segments and query params;
    // a request body would never be read by those handlers.
    let body = match tool.method {
        Method::GET | Method::DELETE => Body::empty(),
        _ => {
            // The internal router skips require_auth, but the handlers' own
            // JSON extraction still needs the content type.
            builder = builder.header(axum::http::header::CONTENT_TYPE, "application/json");
            Body::from(serde_json::to_vec(args).map_err(|e| CallError::Internal(e.to_string()))?)
        }
    };

    // The synthetic identity handlers see for this call. `require_role` is
    // not mounted on the internal router; the identity exists so handlers
    // that audit the acting user (or check the admin group) behave exactly
    // as on `/api`.
    let identity = Arc::new(UserInfo {
        username: format!("mcp:{}", token.name),
        groups: token.scope.synthetic_groups(),
        email: String::new(),
        display_name: format!("MCP token {} ({})", token.name, token.scope.as_str()),
    });

    let request = builder
        .extension(identity)
        .body(body)
        .map_err(|e| CallError::Internal(e.to_string()))?;

    // Built per call: the router is a cheap Arc-allocation stack with all
    // state in `AppState`, and caching it on the state would create an
    // `AppState` -> cache -> `AppState` reference cycle.
    let router = crate::server::internal_router(state.clone());
    use tower::ServiceExt;
    let response = match router.oneshot(request).await {
        Ok(response) => response,
        // `Router::oneshot` is infallible; the arm exists so this can never
        // panic (`panic = "abort"` in release, see the Buddy note).
        Err(infallible) => match infallible {},
    };

    let status = response.status().as_u16();
    let body_bytes = axum::body::to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .map_err(|e| CallError::Internal(e.to_string()))?;
    let outcome = Outcome {
        status,
        body: String::from_utf8_lossy(&body_bytes).into_owned(),
    };

    match audit.take() {
        Some(pending) => {
            if let Err(e) = state.audit.finish_async(&pending, status).await {
                degrade_mcp(state, &e);
                return Err(CallError::AuditWriteFailed(e));
            }
        }
        None => {
            // A read: the outcome is useful even if the log write fails, but
            // the subsystem still has to degrade so mutations stop.
            record_mcp_read(state, token, tool, &path, status).await;
        }
    }
    Ok(outcome)
}

#[allow(clippy::too_many_arguments)]
async fn record_mcp_refusal(
    state: &Arc<AppState>,
    token: &TokenAuth,
    tool_name: &str,
    tool: Option<&ToolDef>,
    outcome_state: &str,
    error_code: &str,
) {
    let method = tool.map(|tool| tool.method.clone()).unwrap_or(Method::POST);
    let route = tool.map(|tool| tool.path).unwrap_or("/mcp/tools/call");
    let pending = crate::audit::Pending::mcp(
        &token.id,
        &token.name,
        token.scope.as_str(),
        tool_name,
        &method,
        route,
        route,
        Some(tool_name.to_string()),
    );
    if let Err(e) = state
        .audit
        .record_refusal_async(&pending, outcome_state, error_code)
        .await
    {
        degrade_mcp(state, &e);
    }
}

async fn record_mcp_read(
    state: &Arc<AppState>,
    token: &TokenAuth,
    tool: &ToolDef,
    path: &str,
    status: u16,
) {
    let pending = crate::audit::Pending::mcp(
        &token.id,
        &token.name,
        token.scope.as_str(),
        tool.name,
        &tool.method,
        tool.path,
        path,
        crate::rbac::action_target(tool.path, path),
    );
    if let Err(e) = state.audit.record_once_async(&pending, status).await {
        degrade_mcp(state, &e);
    }
}

fn degrade_mcp(state: &Arc<AppState>, reason: &str) {
    if state.audit.mark_degraded(reason) {
        crate::audit::spawn_degraded_alert(state.clone(), reason.to_string());
    }
}

fn json_object(v: Value) -> JsonObject {
    match v {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    }
}

// ---- Schema helpers (JSON Schema for tool arguments) ----------------------
//
// Hand-rolled, tiny, and shared by every domain table so the registry stays
// readable data.

pub(crate) fn schema(required: &[&str], props: &[(&str, Value)]) -> Value {
    let properties: serde_json::Map<String, Value> = props
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

pub(crate) fn s(desc: &str) -> Value {
    serde_json::json!({ "type": "string", "description": desc })
}

pub(crate) fn s_enum(desc: &str, values: &[&str]) -> Value {
    serde_json::json!({ "type": "string", "description": desc, "enum": values })
}

pub(crate) fn n(desc: &str) -> Value {
    serde_json::json!({ "type": "integer", "description": desc })
}

pub(crate) fn b(desc: &str) -> Value {
    serde_json::json!({ "type": "boolean", "description": desc })
}

pub(crate) fn arr(desc: &str, item: Value) -> Value {
    serde_json::json!({ "type": "array", "description": desc, "items": item })
}

/// A string-to-string map argument (ZFS options, provider fields, ...).
pub(crate) fn str_map(desc: &str) -> Value {
    serde_json::json!({
        "type": "object",
        "description": desc,
        "additionalProperties": { "type": "string" }
    })
}

/// A free-form object argument (chart values, exposure patches, ...).
pub(crate) fn free_map(desc: &str) -> Value {
    serde_json::json!({ "type": "object", "description": desc })
}

/// The `confirm` argument every `full` tool documents.
pub(crate) fn confirm_field() -> (&'static str, Value) {
    (
        "confirm",
        s("Safety check: repeat the exact target value (the same value as the tool's target argument). Required for destructive tools."),
    )
}

/// The `confirm` argument of a destructive tool whose target spans several
/// arguments: they are joined with `@` in the given order.
pub(crate) fn confirm_tuple(fields: &[&str]) -> (&'static str, Value) {
    let expected = fields.join("@");
    let example = fields
        .iter()
        .map(|field| format!("<{field}>"))
        .collect::<Vec<_>>()
        .join("@");
    (
        "confirm",
        s(&format!(
            "Safety check: repeat the target fields joined by '@' in this order: {expected}. \
             For example {example:?}."
        )),
    )
}

/// The `confirm` argument of a destructive tool whose payload is a whole
/// request body: `<argument>@<sha256 of the arguments without confirm>`.
pub(crate) fn confirm_payload(argument: &str) -> (&'static str, Value) {
    (
        "confirm",
        s(&format!(
            "Safety check: \"<{argument}>@<sha256>\". After '{argument}', add '@' and the lowercase hex \
             SHA-256 of the compact JSON arguments object without the confirm field, with object keys \
             sorted. This binds the confirmation to the whole payload, not only the link."
        )),
    )
}

/// A `confirm` argument that must be one fixed literal.
pub(crate) fn confirm_literal(literal: &str) -> (&'static str, Value) {
    (
        "confirm",
        s_enum(
            "Safety check: this destructive tool requires confirm to be exactly this value.",
            &[literal],
        ),
    )
}

/// Read tools take no arguments.
pub(crate) fn no_args() -> Value {
    schema(&[], &[])
}

/// Registry shorthand. Defined before the domain modules so they can use it.
macro_rules! tool {
    ($name:literal, $desc:literal, $level:ident, $method:ident, $path:literal, $schema:expr) => {
        ToolDef {
            name: $name,
            desc: $desc,
            level: Scope::$level,
            method: Method::$method,
            path: $path,
            query: &[],
            confirm: Confirm::No,
            schema: $schema,
            special: Special::None,
        }
    };
    ($name:literal, $desc:literal, $level:ident, $method:ident, $path:literal,
     query = [$($q:literal),* $(,)?], $schema:expr) => {
        ToolDef {
            name: $name,
            desc: $desc,
            level: Scope::$level,
            method: Method::$method,
            path: $path,
            query: &[$($q),*],
            confirm: Confirm::No,
            schema: $schema,
            special: Special::None,
        }
    };
    ($name:literal, $desc:literal, $level:ident, $method:ident, $path:literal,
     confirm = $confirm:expr, $schema:expr) => {
        ToolDef {
            name: $name,
            desc: $desc,
            level: Scope::$level,
            method: Method::$method,
            path: $path,
            query: &[],
            confirm: $confirm,
            schema: $schema,
            special: Special::None,
        }
    };
    ($name:literal, $desc:literal, $level:ident, $method:ident, $path:literal,
     query = [$($q:literal),* $(,)?], confirm = $confirm:expr, $schema:expr) => {
        ToolDef {
            name: $name,
            desc: $desc,
            level: Scope::$level,
            method: Method::$method,
            path: $path,
            query: &[$($q),*],
            confirm: $confirm,
            schema: $schema,
            special: Special::None,
        }
    };
}
// The `tool!` macro is visible to the domain modules through macro_rules
// textual scoping (declared above their `pub mod` lines below); no re-export
// needed.

// Declared after the `tool!` macro so the domain tables can use it.
pub mod apps;
pub mod buddy;
pub mod datasets;
pub mod disks;
pub mod domains;
pub mod network;
pub mod notifications;
pub mod pods;
pub mod shares;
pub mod system;
pub mod users;
pub mod virt;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::store::TokenAuth;

    fn token(name: &str, scope: Scope) -> TokenAuth {
        TokenAuth {
            id: format!("tok-{name}"),
            name: name.to_string(),
            scope,
        }
    }

    #[tokio::test]
    async fn mcp_read_calls_are_audited_without_arguments() {
        let state = Arc::new(AppState::for_test("test-secret"));
        let args = serde_json::json!({
            "token": "canary-mcp-argument",
            "password": "canary-mcp-password",
        })
        .as_object()
        .cloned()
        .unwrap();
        let outcome = dispatch(
            &state,
            &token("reader", Scope::Read),
            "dashboard_get",
            &args,
        )
        .await
        .expect("dashboard_get dispatches");
        assert_eq!(outcome.status, 200);
        let records = state
            .audit
            .query(&crate::audit::AuditQuery::default())
            .unwrap()
            .records;
        assert_eq!(records.len(), 1, "one durable record per MCP read");
        let record = &records[0];
        assert_eq!(record["actor"]["kind"], "mcp");
        assert_eq!(record["actor"]["id"], "tok-reader");
        assert_eq!(record["actor"]["name"], "reader");
        assert_eq!(record["actor"]["auth"], "mcp-token");
        assert_eq!(record["action"]["target"], "dashboard_get");
        assert_eq!(record["action"]["name"], "mcp.tool_call");
        assert_eq!(record["outcome"]["state"], "succeeded");
        let serialized = serde_json::to_string(record).unwrap();
        assert!(!serialized.contains("canary-mcp-argument"));
        assert!(!serialized.contains("canary-mcp-password"));
    }

    #[tokio::test]
    async fn refused_mcp_calls_are_audited_without_arguments() {
        let state = Arc::new(AppState::for_test("test-secret"));
        let args = serde_json::json!({
            "name": "canary-refused-tool",
            "password": "canary-refused-password",
        })
        .as_object()
        .cloned()
        .unwrap();

        let scope_err = dispatch(&state, &token("reader", Scope::Read), "share_create", &args)
            .await
            .expect_err("a read token cannot call a write tool");
        assert!(
            matches!(scope_err, CallError::Scope { .. }),
            "{scope_err:?}"
        );

        let unknown_err = dispatch(
            &state,
            &token("reader", Scope::Read),
            "not_a_real_tool",
            &args,
        )
        .await
        .expect_err("the tool does not exist");
        assert!(
            matches!(unknown_err, CallError::UnknownTool),
            "{unknown_err:?}"
        );

        let records = state
            .audit
            .query(&crate::audit::AuditQuery::default())
            .unwrap()
            .records;
        assert_eq!(records.len(), 2, "one record per refused call: {records:?}");
        assert_eq!(records[0]["outcome"]["state"], "denied");
        assert_eq!(records[0]["outcome"]["errorCode"], "scope");
        assert_eq!(records[0]["action"]["target"], "share_create");
        assert_eq!(records[1]["outcome"]["state"], "failed");
        assert_eq!(records[1]["outcome"]["errorCode"], "unknown_tool");
        assert_eq!(records[1]["action"]["target"], "not_a_real_tool");
        let serialized = serde_json::to_string(&records).unwrap();
        assert!(!serialized.contains("canary-refused"));
        assert!(!serialized.contains("canary-refused-password"));
    }

    #[test]
    fn every_write_tool_is_on_the_reviewed_allowlist() {
        for tool in all_tools() {
            if tool.level == Scope::Write {
                assert!(
                    WRITE_ALLOWLIST.contains(&tool.name),
                    "{} is a Write tool but is not on the reviewed allowlist; \
                     classify it Full or add it with a rationale",
                    tool.name
                );
            } else {
                assert!(
                    !WRITE_ALLOWLIST.contains(&tool.name),
                    "{} is allowlisted for Write but is classified {:?}",
                    tool.name,
                    tool.level
                );
            }
        }
        for name in WRITE_ALLOWLIST {
            let tool = find_tool(name).unwrap_or_else(|| panic!("{name} is not a tool"));
            assert_eq!(tool.level, Scope::Write, "{name} must be Write");
        }
    }

    #[test]
    fn identity_credential_and_egress_tools_are_full() {
        for name in FULL_ONLY_TOOLS {
            let tool = find_tool(name).unwrap_or_else(|| panic!("{name} is not a tool"));
            assert_eq!(tool.level, Scope::Full, "{name} must be Full (R-009)");
        }
    }

    #[tokio::test]
    async fn write_token_cannot_create_or_update_identities() {
        let state = Arc::new(AppState::for_test("test-secret"));
        let writer = token("writer", Scope::Write);
        // An empty argument object is enough: the scope check runs before any
        // path/confirm validation, so a Scope error proves nothing reached
        // the internal router.
        let args = JsonObject::new();
        for name in FULL_ONLY_TOOLS {
            let err = dispatch(&state, &writer, name, &args)
                .await
                .expect_err("a write token must not reach a Full tool");
            assert!(
                matches!(err, CallError::Scope { .. }),
                "{name} dispatched under a write token: {err:?}"
            );
        }
        let records = state
            .audit
            .query(&crate::audit::AuditQuery::default())
            .unwrap()
            .records;
        assert_eq!(records.len(), FULL_ONLY_TOOLS.len());
        assert!(records.iter().all(|r| r["outcome"]["errorCode"] == "scope"));
    }

    #[tokio::test]
    async fn destructive_tools_refuse_an_empty_target() {
        let state = Arc::new(AppState::for_test("test-secret"));
        let full = token("full", Scope::Full);

        let mut missing = JsonObject::new();
        missing.insert("confirm".into(), serde_json::json!(""));
        let err = dispatch(&state, &full, "share_delete", &missing)
            .await
            .expect_err("a missing target must not skip the confirm check");
        assert!(matches!(err, CallError::ConfirmTarget { .. }), "{err:?}");

        let mut empty = JsonObject::new();
        empty.insert("name".into(), serde_json::json!(""));
        empty.insert("confirm".into(), serde_json::json!(""));
        let err = dispatch(&state, &full, "share_delete", &empty)
            .await
            .expect_err("an empty target must not skip the confirm check");
        assert!(matches!(err, CallError::ConfirmTarget { .. }), "{err:?}");

        let mut no_confirm = JsonObject::new();
        no_confirm.insert("name".into(), serde_json::json!("data"));
        let err = dispatch(&state, &full, "share_delete", &no_confirm)
            .await
            .expect_err("confirm is still required");
        assert!(matches!(err, CallError::Confirm { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn replacing_the_buddy_identity_requires_the_literal_confirm() {
        let state = Arc::new(AppState::for_test("test-secret"));
        let full = token("full", Scope::Full);

        let mut args = JsonObject::new();
        args.insert("name".into(), serde_json::json!("naslos"));
        args.insert("replace".into(), serde_json::json!(true));
        args.insert("confirm".into(), serde_json::json!("naslos"));
        let err = dispatch(&state, &full, "buddy_identity_create", &args)
            .await
            .expect_err("replace needs the literal confirm, not the name");
        assert!(
            matches!(&err, CallError::Confirm { target } if target == "replace"),
            "{err:?}"
        );
    }

    #[test]
    fn system_upgrade_start_accepts_the_real_operation_enum() {
        let tool = find_tool("system_upgrade_start").expect("the tool exists");
        let schema = (tool.schema)();
        let ops = schema["properties"]["op"]["enum"]
            .as_array()
            .expect("op is an enum");
        let values: Vec<&str> = ops.iter().filter_map(serde_json::Value::as_str).collect();
        assert_eq!(values, vec!["naslos", "talos", "kubernetes"]);
    }

    #[tokio::test]
    async fn mcp_mutations_fail_closed_when_audit_is_degraded() {
        let state = Arc::new(AppState::for_test("test-secret"));
        assert!(state.audit.mark_degraded("test failure"));
        let args = serde_json::json!({
            "name": "canary-share",
            "path": "/canary-share",
        })
        .as_object()
        .cloned()
        .unwrap();
        let err = dispatch(
            &state,
            &token("writer", Scope::Write),
            "share_create",
            &args,
        )
        .await
        .expect_err("a degraded audit sink must refuse the tool");
        assert!(matches!(err, CallError::AuditUnavailable(_)), "{err:?}");
        assert!(state
            .audit
            .query(&crate::audit::AuditQuery::default())
            .unwrap()
            .records
            .is_empty());
    }

    /// Whether a schema declares an argument that is an http(s) URL (by name
    /// or description), recursively through nested items/objects.
    fn schema_takes_url(value: &Value) -> bool {
        match value {
            Value::Object(map) => {
                if let Some(properties) = map.get("properties").and_then(Value::as_object) {
                    for (name, spec) in properties {
                        let name = name.to_ascii_lowercase();
                        if name.contains("url")
                            || name.contains("uri")
                            || name.contains("receiver")
                            || name.contains("endpoint")
                            || name.contains("webhook")
                        {
                            return true;
                        }
                        if let Some(description) = spec.get("description").and_then(Value::as_str) {
                            let description = description.to_ascii_lowercase();
                            if description.contains("http://")
                                || description.contains("https://")
                                || description.contains(" url")
                                || description.contains("url ")
                            {
                                return true;
                            }
                        }
                        if schema_takes_url(spec) {
                            return true;
                        }
                    }
                }
                map.values().any(schema_takes_url)
            }
            Value::Array(items) => items.iter().any(schema_takes_url),
            _ => false,
        }
    }

    /// Arbitrary egress is `full`: no `write` token may make the API fetch or
    /// push to a caller-supplied http(s) URL (B-9).
    #[test]
    fn no_write_scoped_tool_takes_an_http_url() {
        for tool in all_tools() {
            if tool.level != Scope::Write {
                continue;
            }
            assert!(
                !schema_takes_url(&(tool.schema)()),
                "{} is Write-scoped but declares a URL argument; arbitrary egress is Full",
                tool.name
            );
        }
    }

    /// Every `full` tool whose control is the scope instead of a confirm target
    /// is on the reviewed allowlist, and the allowlist holds only those tools.
    #[test]
    fn the_full_no_confirm_allowlist_matches_the_registry() {
        for tool in all_tools() {
            let properties = (tool.schema)()
                .get("properties")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let declares_confirm = properties.contains_key("confirm");
            if tool.level == Scope::Full && !declares_confirm {
                assert!(
                    FULL_NO_CONFIRM_ALLOWLIST.contains(&tool.name),
                    "{} is full, declares no confirm, and is not on the reviewed allowlist",
                    tool.name
                );
            }
            if declares_confirm {
                assert!(
                    !FULL_NO_CONFIRM_ALLOWLIST.contains(&tool.name),
                    "{} declares confirm but is listed as a no-confirm full tool",
                    tool.name
                );
            }
        }
        for name in FULL_NO_CONFIRM_ALLOWLIST {
            let tool = find_tool(name).unwrap_or_else(|| panic!("{name} is not a tool"));
            assert_eq!(tool.level, Scope::Full, "{name} must be Full");
            assert_eq!(tool.confirm, Confirm::No, "{name} must have no confirm");
        }
    }

    /// Multi-field destructive tools require the whole target, not one field
    /// (B-14).
    #[tokio::test]
    async fn multi_field_destructive_tools_confirm_the_whole_target() {
        let state = Arc::new(AppState::for_test("test-secret"));
        let full = token("full", Scope::Full);

        // vm_snapshot_restore: <name>@<snapshot>, and the handler still gets
        // its plain ?confirm=<name> query.
        let mut vm = JsonObject::new();
        vm.insert("name".into(), serde_json::json!("vm-a"));
        vm.insert("snap".into(), serde_json::json!("snap-1"));
        vm.insert("confirm".into(), serde_json::json!("vm-a"));
        let err = dispatch(&state, &full, "vm_snapshot_restore", &vm)
            .await
            .expect_err("confirming only the VM name is under-specified");
        assert!(
            matches!(&err, CallError::Confirm { target } if target == "vm-a@snap-1"),
            "{err:?}"
        );
        vm.insert("confirm".into(), serde_json::json!("vm-a@snap-1"));
        let outcome = dispatch(&state, &full, "vm_snapshot_restore", &vm)
            .await
            .expect("the tuple confirm reaches the handler");
        assert_eq!(outcome.status, 404, "unknown VM in the test store");

        // dataset_delete: <name>@<recursive>, with recursive required.
        let mut dataset = JsonObject::new();
        dataset.insert("name".into(), serde_json::json!("test/data"));
        dataset.insert("recursive".into(), serde_json::json!(true));
        dataset.insert("confirm".into(), serde_json::json!("test/data"));
        let err = dispatch(&state, &full, "dataset_delete", &dataset)
            .await
            .expect_err("confirming only the dataset name is under-specified");
        assert!(
            matches!(&err, CallError::Confirm { target } if target == "test/data@true"),
            "{err:?}"
        );
        dataset.insert("confirm".into(), serde_json::json!("test/data@true"));
        let outcome = dispatch(&state, &full, "dataset_delete", &dataset)
            .await
            .expect("the tuple confirm reaches the handler");
        assert_eq!(outcome.status, 502, "no agent in the test state");

        let mut missing = JsonObject::new();
        missing.insert("name".into(), serde_json::json!("test/data"));
        missing.insert("confirm".into(), serde_json::json!("test/data"));
        let err = dispatch(&state, &full, "dataset_delete", &missing)
            .await
            .expect_err("the recursive flag is part of the confirm target");
        assert!(
            matches!(err, CallError::ConfirmTarget { param: "recursive" }),
            "{err:?}"
        );

        // network_change_apply: <link>@<sha256 of the payload>.
        let mut network = JsonObject::new();
        network.insert("link".into(), serde_json::json!("eth0"));
        network.insert("addressing".into(), serde_json::json!("static"));
        network.insert("addresses".into(), serde_json::json!(["192.0.2.10/24"]));
        let expected = format!("eth0@{}", payload_confirm_hash(&network));
        network.insert("confirm".into(), serde_json::json!("eth0"));
        let err = dispatch(&state, &full, "network_change_apply", &network)
            .await
            .expect_err("confirming only the link is under-specified");
        assert!(
            matches!(&err, CallError::Confirm { target } if target == &expected),
            "{err:?}"
        );
        network.insert("confirm".into(), serde_json::Value::String(expected));
        let outcome = dispatch(&state, &full, "network_change_apply", &network)
            .await
            .expect("the payload-hash confirm reaches the handler");
        assert_eq!(
            outcome.status, 503,
            "Talos is unavailable in the test state"
        );
    }

    /// A deliberately misclassified tool proves there is no second role layer
    /// on the internal router: a `write` (operator) token reaches an
    /// admin-only handler, so the registry scope table is the enforcement and
    /// `docs/mcp.md` must not claim otherwise (B-13).
    #[tokio::test]
    async fn a_misclassified_write_tool_reaches_the_handler_so_scope_is_the_enforcement() {
        static MISCLASSIFIED: ToolDef = ToolDef {
            name: "misclassified_user_delete",
            desc: "test-only: an admin-only route deliberately classified Write",
            level: Scope::Write,
            method: Method::DELETE,
            path: "/api/users/{uid}",
            query: &["uid"],
            confirm: Confirm::No,
            schema: no_args,
            special: Special::None,
        };
        let state = Arc::new(AppState::for_test("test-secret"));
        let mut args = JsonObject::new();
        args.insert("uid".into(), serde_json::json!("probe"));

        let err = dispatch_tool(&state, &token("reader", Scope::Read), &MISCLASSIFIED, &args)
            .await
            .expect_err("a read token is below the tool's declared scope");
        assert!(matches!(err, CallError::Scope { .. }), "{err:?}");

        let outcome = dispatch_tool(
            &state,
            &token("writer", Scope::Write),
            &MISCLASSIFIED,
            &args,
        )
        .await
        .expect("the scope table admits a Write tool to a Write token");
        assert_eq!(
            outcome.status, 503,
            "the operator-scoped synthetic identity reached the admin-only handler \
             (LDAP is unconfigured here): require_role is not mounted on the internal router"
        );
    }

    /// Restores an environment variable when it drops, including on an
    /// assertion panic, so an env-mutating test cannot leak a temp path into
    /// the other lib tests that run in parallel.
    struct EnvGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: std::path::PathBuf) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    fn sample_string(tool: &str, property: &str, public_key: &str) -> String {
        match property {
            "dataset" => "test/data".to_string(),
            "source" => "naslos-test/data".to_string(),
            "receiver" => "https://127.0.0.1:1".to_string(),
            "publicKey" => public_key.to_string(),
            "name" if tool == "buddy_peer_add" => "probe-peer".to_string(),
            "name" => "probe-identity".to_string(),
            "id" => "probe-id".to_string(),
            "chain" => "probe-chain".to_string(),
            "runAt" => "03:00".to_string(),
            "allowedSources" => "naslos-test/".to_string(),
            _ => "probe".to_string(),
        }
    }

    /// Build schema-shaped arguments: every declared property at its declared
    /// type (enum first value, booleans false, integers 1, arrays of one
    /// sample string) plus the confirm value the tool requires.
    fn schema_shaped_args(tool: &ToolDef, public_key: &str) -> JsonObject {
        let schema = (tool.schema)();
        let properties = schema
            .get("properties")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut args = JsonObject::new();
        for (name, spec) in &properties {
            let value = if let Some(first) = spec
                .get("enum")
                .and_then(Value::as_array)
                .and_then(|values| values.first())
                .and_then(Value::as_str)
            {
                Value::String(first.to_string())
            } else {
                match spec.get("type").and_then(Value::as_str) {
                    Some("string") => Value::String(sample_string(tool.name, name, public_key)),
                    Some("boolean") => Value::Bool(false),
                    Some("integer") => Value::Number(1.into()),
                    Some("array") => Value::Array(vec![Value::String(sample_string(
                        tool.name, name, public_key,
                    ))]),
                    Some("object") => Value::Object(JsonObject::new()),
                    _ => Value::Null,
                }
            };
            args.insert(name.clone(), value);
        }
        if let Some(confirm) = handler_confirm_value(tool, &args) {
            args.insert("confirm".into(), Value::String(confirm));
        }
        args
    }

    fn assert_not_a_deserialization_error(tool: &str, outcome: &Outcome) {
        if outcome.status != 400 {
            return;
        }
        let error = serde_json::from_str::<Value>(&outcome.body)
            .ok()
            .and_then(|value| {
                value
                    .get("error")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default()
            .to_ascii_lowercase();
        for needle in [
            "invalid type",
            "missing field",
            "unknown field",
            "invalid value",
            "invalid length",
        ] {
            assert!(
                !error.contains(needle),
                "{tool}: schema-shaped arguments failed to deserialize: {error}"
            );
        }
    }

    /// Every buddy tool must accept arguments shaped exactly like its declared
    /// schema; the previous tests never exercised request deserialization, so
    /// schema/handler drift stayed invisible (B-12).
    #[tokio::test]
    async fn buddy_tools_accept_schema_shaped_arguments() {
        use crate::buddy::auth::Authenticator;
        use crate::buddy::keys::new_identity;
        use crate::buddy::{Peer, PeerStore, Store};

        let dir = tempfile::tempdir().expect("tempdir");
        let identity = Arc::new(new_identity("probe").expect("identity"));
        let peers = Arc::new(PeerStore::new(""));
        peers
            .add(Peer {
                name: "sender".into(),
                public_key: identity.public_key.clone(),
                enabled: true,
                ..Default::default()
            })
            .expect("add the sender peer");
        let store = Arc::new(Store::new(&dir.path().display().to_string()));
        let auth = Arc::new(Authenticator::new(peers.clone()));
        let mut receiver = crate::server::buddy::Receiver::new(store, peers, auth, "naslos-test");
        receiver.enroll_token = String::new();

        let mut state = AppState::for_test("test-secret");
        state.buddy = Some(Arc::new(receiver));
        // Let buddy_send/buddy_restore get past their agent check; the
        // identity file below is still absent, so they stop before any I/O.
        state.agent = Some(Arc::new(
            crate::agent::Client::new("http://127.0.0.1:1", "", "").expect("agent client"),
        ));
        let state = Arc::new(state);
        // Restored by the guard (even on a panic), so no dangling temp path is
        // left for parallel tests.
        let _buddy_identity = EnvGuard::set("BUDDY_IDENTITY", dir.path().join("identity.json"));
        let full = token("full", Scope::Full);

        for tool in crate::mcp::tools::buddy::TOOLS {
            let args = schema_shaped_args(tool, &identity.public_key);
            let outcome = dispatch(&state, &full, tool.name, &args)
                .await
                .unwrap_or_else(|err| {
                    panic!(
                        "{}: schema-shaped arguments were refused before dispatch: {}",
                        tool.name,
                        err.message()
                    )
                });
            assert_not_a_deserialization_error(tool.name, &outcome);
        }
    }
}
