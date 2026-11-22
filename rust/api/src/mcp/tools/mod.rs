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
#[derive(Default, Clone, Copy, PartialEq)]
pub enum Confirm {
    #[default]
    /// Not a destructive tool.
    No,
    /// The named argument (also used in the path/query), e.g. `name`.
    Arg(&'static str),
    /// A fixed literal, for targets that are not an argument (`reboot`).
    Literal(&'static str),
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
        record_mcp_refusal(state, token, name, None, "failed", "unknown_tool");
        return Err(CallError::UnknownTool);
    };

    // Scope: re-checked here even though tools/list filtered, because a
    // client may call an unlisted tool.
    if !token.scope.allows(tool.level) {
        record_mcp_refusal(state, token, name, Some(tool), "denied", "scope");
        return Err(CallError::Scope {
            have: token.scope,
            need: tool.level,
        });
    }

    // Destructive tools: confirm must equal the resolved target, checked
    // before any dispatch. The target comes from the same resolved argument
    // used in the path/query, never a second lookup.
    let target = match tool.confirm {
        Confirm::No => String::new(),
        Confirm::Arg(param) => args
            .get(param)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        Confirm::Literal(lit) => lit.to_string(),
    };
    if !target.is_empty() {
        let confirm = args.get("confirm").and_then(Value::as_str).unwrap_or("");
        if confirm != target {
            record_mcp_refusal(state, token, name, Some(tool), "denied", "confirm_required");
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
            record_mcp_read(state, token, tool, tool.path, status);
            return outcome;
        }
        Special::None => {}
    }

    let path = match resolve_path(tool.path, args) {
        Ok(path) => path,
        Err(err) => {
            record_mcp_refusal(state, token, name, Some(tool), "failed", "bad_path");
            return Err(err);
        }
    };
    let uri = format!("{path}{}", query_string(tool, args));
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
        Some(state.audit.begin(audit).map_err(|e| {
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
        groups: vec!["naslos_admins".to_string()],
        email: String::new(),
        display_name: format!("MCP token {}", token.name),
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
            if let Err(e) = state.audit.finish(&pending, status) {
                degrade_mcp(state, &e);
                return Err(CallError::AuditWriteFailed(e));
            }
        }
        None => {
            // A read: the outcome is useful even if the log write fails, but
            // the subsystem still has to degrade so mutations stop.
            record_mcp_read(state, token, tool, &path, status);
        }
    }
    Ok(outcome)
}

#[allow(clippy::too_many_arguments)]
fn record_mcp_refusal(
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
        .record_refusal(&pending, outcome_state, error_code)
    {
        degrade_mcp(state, &e);
    }
}

fn record_mcp_read(
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
    if let Err(e) = state.audit.record_once(&pending, status) {
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
}
