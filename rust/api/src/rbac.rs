//! Application authorization (FR-RBAC): role derivation from the trusted
//! `Remote-Groups` header and the per-route minimum-role table.
//!
//! Phase 2 is deliberately behavior-preserving: every existing owner route is
//! classified `Admin`, so `naslos_admins` keeps its full surface and the
//! baseline (`naslos_users`) keeps answering `403`. Operator/read-only route
//! delegation is Phase 4 and changes only the `minimum` values in
//! [`ROUTES`](static@ROUTES); the middleware and the contract tests stay put.
//!
//! Roles are derived per request from `Remote-Groups`, which `require_auth`
//! only accepts through the SEC-3/SEC-10 proxy gate. Nothing else (email,
//! display name, path, query, body or a client-supplied header) grants a
//! role, and the result is never cached.

use axum::extract::{Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::Response;

/// The privileged role hierarchy. The authenticated baseline is below every
/// privileged role; it is not itself a privileged role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    Baseline,
    ReadOnly,
    Operator,
    Admin,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Baseline => "baseline",
            Role::ReadOnly => "read-only",
            Role::Operator => "operator",
            Role::Admin => "admin",
        }
    }

    /// Whether this role satisfies the required minimum. Roles are ordered, so
    /// a higher role is always a superset of the lower ones.
    pub fn allows(self, minimum: Role) -> bool {
        self >= minimum
    }
}

/// Derive the effective role from the trusted group list. Precedence is
/// admin, then operator, then read-only; every other group (including
/// `naslos_users`) is the authenticated baseline. Matching is
/// case-insensitive.
pub fn role_for_groups(groups: &[String]) -> Role {
    let has = |group: &str| groups.iter().any(|g| g.eq_ignore_ascii_case(group));
    if has("naslos_admins") {
        Role::Admin
    } else if has("naslos_operators") {
        Role::Operator
    } else if has("naslos_readonly") {
        Role::ReadOnly
    } else {
        Role::Baseline
    }
}

/// One `(method, path)` entry in the authoritative route table.
#[derive(Debug, Clone, Copy)]
pub struct RouteRule {
    pub method: &'static str,
    pub path: &'static str,
    pub minimum: Role,
}

const BASELINE: Role = Role::Baseline;
const ADMIN: Role = Role::Admin;

macro_rules! route {
    ($method:literal, $path:literal, $role:ident) => {
        RouteRule {
            method: $method,
            path: $path,
            minimum: $role,
        }
    };
}

/// The authoritative `(method, path) → minimum role` table for every route on
/// the authenticated router. Literal paths are listed before a parameterized
/// path that could also match them (the same order as the router).
///
/// **Phase 2:** every owner route is `Admin`; only the user routes are
/// `Baseline`. Phase 4 lowers individual entries to `ReadOnly`/`Operator` with
/// tests, never by an implicit default.
pub static ROUTES: &[RouteRule] = &[
    // Auth-but-not-admin.
    route!("GET", "/api/auth/me", BASELINE),
    route!("GET", "/api/dashboard", BASELINE),
    route!("GET", "/api/metrics", BASELINE),
    route!("GET", "/api/reports", ADMIN),
    route!("GET", "/api/reports/series", ADMIN),
    // App catalog and installed apps. Literal routes before `{name}`.
    route!("GET", "/api/catalog", ADMIN),
    route!("GET", "/api/catalog/{name}", ADMIN),
    route!("GET", "/api/apps", ADMIN),
    route!("POST", "/api/apps", ADMIN),
    route!("GET", "/api/apps/jobs", ADMIN),
    route!("GET", "/api/apps/jobs/{id}", ADMIN),
    route!("GET", "/api/apps/updates", ADMIN),
    route!("GET", "/api/apps/usage", ADMIN),
    route!("GET", "/api/apps/namespaces", ADMIN),
    route!("PUT", "/api/apps/namespaces", ADMIN),
    route!("GET", "/api/apps/{name}", ADMIN),
    route!("PUT", "/api/apps/{name}", ADMIN),
    route!("DELETE", "/api/apps/{name}", ADMIN),
    route!("GET", "/api/apps/{name}/exposure", ADMIN),
    route!("PUT", "/api/apps/{name}/exposure", ADMIN),
    route!("POST", "/api/apps/{name}/dns/reconcile", ADMIN),
    route!("GET", "/api/apps/{name}/services", ADMIN),
    route!("GET", "/api/apps/{name}/pods", ADMIN),
    route!("GET", "/api/apps/{name}/quota", ADMIN),
    // Chart sources.
    route!("GET", "/api/sources", ADMIN),
    route!("POST", "/api/sources", ADMIN),
    route!("POST", "/api/sources/refresh", ADMIN),
    route!("GET", "/api/sources/{name}", ADMIN),
    route!("DELETE", "/api/sources/{name}", ADMIN),
    // Domains and certificates.
    route!("GET", "/api/domains", ADMIN),
    route!("POST", "/api/domains", ADMIN),
    route!("GET", "/api/domains/{name}/certificate", ADMIN),
    route!("POST", "/api/domains/{name}/sso", ADMIN),
    route!("GET", "/api/domains/{name}", ADMIN),
    route!("PUT", "/api/domains/{name}", ADMIN),
    route!("DELETE", "/api/domains/{name}", ADMIN),
    // DNS providers and dynamic DNS.
    route!("GET", "/api/providers", ADMIN),
    route!("GET", "/api/ddns", ADMIN),
    route!("POST", "/api/ddns", ADMIN),
    route!("POST", "/api/ddns/{id}/run", ADMIN),
    route!("GET", "/api/ddns/{id}", ADMIN),
    route!("PUT", "/api/ddns/{id}", ADMIN),
    route!("DELETE", "/api/ddns/{id}", ADMIN),
    // System.
    route!("GET", "/api/system/version", ADMIN),
    route!("GET", "/api/system/config/export", ADMIN),
    route!("POST", "/api/system/config/import", ADMIN),
    route!("GET", "/api/system/updates", ADMIN),
    route!("POST", "/api/system/updates/check", ADMIN),
    route!("POST", "/api/system/reboot", ADMIN),
    route!("POST", "/api/system/operations", ADMIN),
    route!("GET", "/api/system/operations/current", ADMIN),
    // Disks and pools.
    route!("GET", "/api/disks", ADMIN),
    route!("GET", "/api/disks/health", ADMIN),
    route!("POST", "/api/disks/recommend", ADMIN),
    route!("GET", "/api/volumes/zfs", ADMIN),
    route!("POST", "/api/volumes/zfs", ADMIN),
    route!("GET", "/api/volumes/zfs/import", ADMIN),
    route!("POST", "/api/volumes/zfs/import", ADMIN),
    route!("GET", "/api/volumes/zfs/{pool}/health", ADMIN),
    route!("POST", "/api/volumes/zfs/{pool}/scrub", ADMIN),
    route!("POST", "/api/volumes/zfs/{pool}/devices", ADMIN),
    route!("GET", "/api/volumes/zfs/{pool}", ADMIN),
    route!("DELETE", "/api/volumes/zfs/{pool}", ADMIN),
    // Storage schedules, replication, datasets and zvols.
    route!("GET", "/api/storage/schedules", ADMIN),
    route!("POST", "/api/storage/schedules", ADMIN),
    route!("DELETE", "/api/storage/schedules", ADMIN),
    route!("POST", "/api/replication/send", ADMIN),
    route!("GET", "/api/replication/jobs", ADMIN),
    route!("GET", "/api/replication/jobs/{id}", ADMIN),
    route!("DELETE", "/api/replication/jobs/{id}", ADMIN),
    route!("GET", "/api/datasets", ADMIN),
    route!("POST", "/api/datasets", ADMIN),
    route!("DELETE", "/api/datasets", ADMIN),
    route!("GET", "/api/datasets/keystatus", ADMIN),
    route!("POST", "/api/datasets/unlock", ADMIN),
    route!("POST", "/api/datasets/lock", ADMIN),
    route!("GET", "/api/zvols", ADMIN),
    route!("POST", "/api/zvols", ADMIN),
    route!("PUT", "/api/zvols", ADMIN),
    route!("DELETE", "/api/zvols", ADMIN),
    route!("GET", "/api/datasets/registry", ADMIN),
    route!("POST", "/api/datasets/registry", ADMIN),
    route!("DELETE", "/api/datasets/registry/{name}", ADMIN),
    // Virtual machines and the image library.
    route!("GET", "/api/virt/capabilities", ADMIN),
    route!("GET", "/api/virt/vms", ADMIN),
    route!("POST", "/api/virt/vms", ADMIN),
    route!("GET", "/api/virt/vms/{name}", ADMIN),
    route!("PUT", "/api/virt/vms/{name}", ADMIN),
    route!("DELETE", "/api/virt/vms/{name}", ADMIN),
    route!("POST", "/api/virt/vms/{name}/start", ADMIN),
    route!("POST", "/api/virt/vms/{name}/stop", ADMIN),
    route!("POST", "/api/virt/vms/{name}/force-stop", ADMIN),
    route!("POST", "/api/virt/vms/{name}/restart", ADMIN),
    route!("GET", "/api/virt/vms/{name}/snapshots", ADMIN),
    route!("POST", "/api/virt/vms/{name}/snapshots", ADMIN),
    route!(
        "POST",
        "/api/virt/vms/{name}/snapshots/{snap}/restore",
        ADMIN
    ),
    route!("GET", "/api/virt/images", ADMIN),
    route!("POST", "/api/virt/images/upload", ADMIN),
    route!("DELETE", "/api/virt/images/upload", ADMIN),
    route!("POST", "/api/virt/images/import", ADMIN),
    route!("DELETE", "/api/virt/images/{name}", ADMIN),
    // WebSockets and pods.
    route!("GET", "/api/ws/vm/{name}/vnc", ADMIN),
    route!("GET", "/api/ws/logs", ADMIN),
    route!("GET", "/api/ws/exec", ADMIN),
    route!("GET", "/api/pods", ADMIN),
    route!("GET", "/api/namespaces", ADMIN),
    // Shares. Literal routes before `{name}`.
    route!("GET", "/api/shares", ADMIN),
    route!("POST", "/api/shares", ADMIN),
    route!("GET", "/api/shares/paths", ADMIN),
    route!("GET", "/api/shares/folders", ADMIN),
    route!("POST", "/api/shares/folders", ADMIN),
    route!("DELETE", "/api/shares/folders", ADMIN),
    route!("GET", "/api/shares/status", ADMIN),
    route!("POST", "/api/shares/apply", ADMIN),
    route!("PUT", "/api/shares/apply", ADMIN),
    route!("GET", "/api/shares/config/samba", ADMIN),
    route!("GET", "/api/shares/config/nfs", ADMIN),
    route!("GET", "/api/shares/{name}", ADMIN),
    route!("PUT", "/api/shares/{name}", ADMIN),
    route!("DELETE", "/api/shares/{name}", ADMIN),
    // Notifications and alerts.
    route!("GET", "/api/notifications", ADMIN),
    route!("PUT", "/api/notifications", ADMIN),
    route!("POST", "/api/notifications/test", ADMIN),
    route!("GET", "/api/notifications/webhook", ADMIN),
    route!("PUT", "/api/notifications/webhook", ADMIN),
    route!("DELETE", "/api/notifications/webhook", ADMIN),
    route!("POST", "/api/notifications/webhook/test", ADMIN),
    route!("GET", "/api/notifications/smtp", ADMIN),
    route!("PUT", "/api/notifications/smtp", ADMIN),
    route!("DELETE", "/api/notifications/smtp", ADMIN),
    route!("POST", "/api/notifications/smtp/test", ADMIN),
    route!("GET", "/api/notifications/syslog", ADMIN),
    route!("PUT", "/api/notifications/syslog", ADMIN),
    route!("DELETE", "/api/notifications/syslog", ADMIN),
    route!("POST", "/api/notifications/syslog/test", ADMIN),
    // Host network.
    route!("GET", "/api/network", ADMIN),
    route!("POST", "/api/network/changes", ADMIN),
    route!("POST", "/api/network/changes/{id}/revert", ADMIN),
    route!("GET", "/api/network/changes/{id}", ADMIN),
    // Buddy owner surfaces.
    route!("GET", "/api/buddy/status", ADMIN),
    route!("GET", "/api/buddy/peers", ADMIN),
    route!("POST", "/api/buddy/peers", ADMIN),
    route!("DELETE", "/api/buddy/peers", ADMIN),
    route!("GET", "/api/buddy/identity", ADMIN),
    route!("POST", "/api/buddy/identity", ADMIN),
    route!("POST", "/api/buddy/send", ADMIN),
    route!("POST", "/api/buddy/restore", ADMIN),
    route!("GET", "/api/buddy/jobs", ADMIN),
    route!("GET", "/api/buddy/jobs/{id}", ADMIN),
    route!("DELETE", "/api/buddy/jobs/{id}", ADMIN),
    route!("GET", "/api/buddy/schedules", ADMIN),
    route!("POST", "/api/buddy/schedules", ADMIN),
    route!("DELETE", "/api/buddy/schedules", ADMIN),
    // Cloud backup (FR-REP-11/12/13): owner-only like every Buddy route. The
    // literal `/test` suffix is listed before the `{id}` wildcard.
    route!("GET", "/api/cloud/destinations", ADMIN),
    route!("POST", "/api/cloud/destinations", ADMIN),
    route!("POST", "/api/cloud/destinations/{id}/test", ADMIN),
    route!("GET", "/api/cloud/destinations/{id}", ADMIN),
    route!("PUT", "/api/cloud/destinations/{id}", ADMIN),
    route!("DELETE", "/api/cloud/destinations/{id}", ADMIN),
    route!("POST", "/api/cloud/send", ADMIN),
    route!("POST", "/api/cloud/restore", ADMIN),
    route!("POST", "/api/cloud/verify", ADMIN),
    route!("GET", "/api/cloud/jobs", ADMIN),
    route!("GET", "/api/cloud/jobs/{id}", ADMIN),
    route!("DELETE", "/api/cloud/jobs/{id}", ADMIN),
    route!("GET", "/api/cloud/chains", ADMIN),
    route!("GET", "/api/cloud/schedules", ADMIN),
    route!("POST", "/api/cloud/schedules", ADMIN),
    route!("DELETE", "/api/cloud/schedules", ADMIN),
    // Identity. Listings stay admin in Phase 2; every mutation is owner-only.
    route!("GET", "/api/users", ADMIN),
    route!("POST", "/api/users", ADMIN),
    route!("GET", "/api/users/{uid}", ADMIN),
    route!("PUT", "/api/users/{uid}", ADMIN),
    route!("DELETE", "/api/users/{uid}", ADMIN),
    route!("POST", "/api/users/{uid}/password", ADMIN),
    route!("POST", "/api/users/{uid}/enable", ADMIN),
    route!("POST", "/api/users/{uid}/disable", ADMIN),
    route!("POST", "/api/users/{uid}/{action}", ADMIN),
    route!("GET", "/api/groups", ADMIN),
    route!("POST", "/api/groups", ADMIN),
    route!("GET", "/api/groups/{cn}", ADMIN),
    route!("PUT", "/api/groups/{cn}", ADMIN),
    route!("DELETE", "/api/groups/{cn}", ADMIN),
    // MCP settings and tokens.
    route!("GET", "/api/mcp", ADMIN),
    route!("PUT", "/api/mcp", ADMIN),
    route!("POST", "/api/mcp/tokens", ADMIN),
    route!("DELETE", "/api/mcp/tokens/{id}", ADMIN),
    // Audit trail (FR-AUD-09). Read-only surface; every operation is audited.
    route!("GET", "/api/audit", ADMIN),
    route!("GET", "/api/audit/verify", ADMIN),
    route!("GET", "/api/audit/export", ADMIN),
];

/// Whether `template` matches the concrete `path`. Supports `{param}` segments
/// and a terminal `{*rest}` catch-all, mirroring the axum route syntax used in
/// `server/mod.rs`.
fn path_matches(template: &str, path: &str) -> bool {
    let template: Vec<&str> = trim_path(template);
    let actual: Vec<&str> = trim_path(path);
    for (index, segment) in template.iter().enumerate() {
        if let Some(rest) = segment.strip_prefix("{*") {
            // `{*rest}` matches one or more remaining segments.
            return rest.ends_with('}') && actual.len() > index;
        }
        if index >= actual.len() {
            return false;
        }
        if segment.starts_with('{') && segment.ends_with('}') {
            if actual[index].is_empty() {
                return false;
            }
            continue;
        }
        if *segment != actual[index] {
            return false;
        }
    }
    actual.len() == template.len()
}

fn trim_path(path: &str) -> Vec<&str> {
    let path = path.split_once('?').map(|(path, _)| path).unwrap_or(path);
    path.trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect()
}

/// Middleware enforcing the per-route minimum role. It runs after
/// `require_auth` (a missing identity stays `401`) and before the handler; an
/// authenticated user with an insufficient role gets the existing JSON
/// `"Forbidden"` body with `403`.
///
/// A denied **safe** method (GET/HEAD/OPTIONS) is recorded as a durable
/// `denied`/`role` audit end record; unsafe methods are already covered by the
/// audit middleware's begin/end pair (R-027-core).
pub async fn require_role(
    State(state): State<std::sync::Arc<crate::state::AppState>>,
    mut req: Request,
    next: Next,
) -> Response {
    let Some(user) = crate::auth::user_from_request(&req) else {
        return crate::auth::unauthorized();
    };
    let minimum = minimum_role(req.method(), req.uri().path());
    let role = role_for_groups(&user.groups);
    // Stash the effective role for handlers that only need it for audit or
    // presentation. Enforcement stays in this middleware.
    req.extensions_mut().insert(role);
    if !role.allows(minimum) {
        if !is_unsafe(req.method()) {
            crate::audit::record_role_denied(&state, &user, req.method(), req.uri().path()).await;
        }
        return crate::json(StatusCode::FORBIDDEN, serde_json::json!("Forbidden"));
    }
    next.run(req).await
}

/// Whether a request must produce a durable begin/end pair: everything unsafe
/// plus the security-relevant audit reads (FR-AUD-09).
pub fn is_unsafe(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

/// The explicit rule for `(method, path)`, if the table has one.
///
/// `HEAD`/`OPTIONS` follow the path's `GET` rule: they are safe methods and no
/// route declares a different role for them.
pub fn route_rule(method: &Method, path: &str) -> Option<&'static RouteRule> {
    let lookup = if matches!(method.as_str(), "HEAD" | "OPTIONS") {
        "GET"
    } else {
        method.as_str()
    };
    ROUTES
        .iter()
        .find(|rule| rule.method == lookup && path_matches(rule.path, path))
}

/// The minimum role for a request. Deny-by-default: an unknown path or an
/// unclassified method is `Admin`, never public or read-only.
pub fn minimum_role(method: &Method, path: &str) -> Role {
    route_rule(method, path)
        .map(|rule| rule.minimum)
        .unwrap_or(Role::Admin)
}

/// The canonical route template for a request, used for audit aggregation.
/// Unknown paths map onto the `/api/{*rest}` catch-all.
pub fn route_template(method: &Method, path: &str) -> &'static str {
    route_rule(method, path)
        .map(|rule| rule.path)
        .or_else(|| {
            trim_path(path)
                .first()
                .filter(|first| **first == "api")
                .map(|_| "/api/{*rest}")
        })
        .unwrap_or("/api/{*rest}")
}

/// Whether the table explicitly classifies this `(method, path)`. Contract
/// tests use this to fail when a route is added without a classification,
/// even though the runtime default is a safe `Admin`.
pub fn is_explicitly_classified(method: &Method, path: &str) -> bool {
    route_rule(method, path).is_some()
}

/// Stable dotted action name for the audit record (`share.create`,
/// `system.reboot`). Derived from the route template only, never from a body
/// or query string.
pub fn action_name(method: &Method, template: &str) -> String {
    let mut parts: Vec<&str> = trim_path(template)
        .into_iter()
        .filter(|segment| *segment != "api")
        .filter(|segment| !(segment.starts_with('{') && segment.ends_with('}')))
        .filter(|segment| !segment.starts_with("{*"))
        .collect();
    if parts.is_empty() {
        parts.push("api");
    }
    let base = parts.join(".");
    let last = parts.last().copied().unwrap_or_default();
    if is_action_verb(last) {
        return base;
    }
    let suffix = match method.as_str() {
        "GET" => "read",
        "POST" => "create",
        "PUT" | "PATCH" => "update",
        "DELETE" => "delete",
        _ => "request",
    };
    format!("{base}.{suffix}")
}

fn is_action_verb(segment: &str) -> bool {
    matches!(
        segment,
        "apply"
            | "check"
            | "disable"
            | "enable"
            | "force-stop"
            | "import"
            | "password"
            | "prune"
            | "reboot"
            | "reconcile"
            | "refresh"
            | "restore"
            | "revert"
            | "run"
            | "scrub"
            | "send"
            | "sso"
            | "start"
            | "stop"
            | "test"
            | "upload"
            | "verify"
    )
}

/// The first path parameter's concrete value: the resolved target
/// (share/app/VM/user/pool name). `None` for collection routes.
pub fn action_target(template: &str, path: &str) -> Option<String> {
    let template = trim_path(template);
    let actual = trim_path(path);
    for (index, segment) in template.iter().enumerate() {
        if segment.starts_with('{') && segment.ends_with('}') && !segment.starts_with("{*") {
            return actual.get(index).map(|value| (*value).to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn groups(raw: &[&str]) -> Vec<String> {
        raw.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn role_precedence_is_admin_operator_readonly() {
        assert_eq!(role_for_groups(&groups(&["naslos_admins"])), Role::Admin);
        assert_eq!(
            role_for_groups(&groups(&["naslos_users", "naslos_operators"])),
            Role::Operator
        );
        assert_eq!(
            role_for_groups(&groups(&["naslos_users", "naslos_readonly"])),
            Role::ReadOnly
        );
        assert_eq!(
            role_for_groups(&groups(&[
                "naslos_readonly",
                "naslos_operators",
                "naslos_admins"
            ])),
            Role::Admin
        );
        assert_eq!(
            role_for_groups(&groups(&["NASLOS_ADMINS"])),
            Role::Admin,
            "group matching is case-insensitive"
        );
    }

    #[test]
    fn naslos_users_stays_baseline() {
        assert_eq!(role_for_groups(&groups(&["naslos_users"])), Role::Baseline);
        assert_eq!(role_for_groups(&groups(&[])), Role::Baseline);
        assert_eq!(
            role_for_groups(&groups(&["naslos_readonly_extra"])),
            Role::Baseline,
            "a look-alike group must not grant a role"
        );
    }

    #[test]
    fn role_mapping_ignores_untrusted_identity_sources() {
        // Only group names count; an email or display name cannot carry a role.
        assert_eq!(
            role_for_groups(&groups(&["admin@example.com", "naslos_admins"])),
            Role::Admin
        );
        assert_eq!(
            role_for_groups(&groups(&["admin@example.com", "Administrator"])),
            Role::Baseline
        );
    }

    #[test]
    fn role_hierarchy_is_monotonic() {
        assert!(Role::Admin.allows(Role::Operator));
        assert!(Role::Admin.allows(Role::ReadOnly));
        assert!(Role::Admin.allows(Role::Admin));
        assert!(Role::Operator.allows(Role::ReadOnly));
        assert!(!Role::ReadOnly.allows(Role::Operator));
        assert!(!Role::Baseline.allows(Role::ReadOnly));
    }

    #[test]
    fn user_routes_are_baseline_and_owner_routes_are_admin() {
        assert_eq!(minimum_role(&Method::GET, "/api/auth/me"), Role::Baseline);
        assert_eq!(minimum_role(&Method::GET, "/api/dashboard"), Role::Baseline);
        assert_eq!(minimum_role(&Method::GET, "/api/shares/team"), Role::Admin);
        assert_eq!(minimum_role(&Method::POST, "/api/shares"), Role::Admin);
    }

    #[test]
    fn unclassified_route_falls_into_the_admin_bucket() {
        assert_eq!(
            minimum_role(&Method::GET, "/api/no-such-route"),
            Role::Admin
        );
        assert_eq!(
            minimum_role(&Method::POST, "/api/no-such-route"),
            Role::Admin
        );
        // A method the table does not list is also admin, never read-only.
        assert_eq!(minimum_role(&Method::PATCH, "/api/shares"), Role::Admin);
        assert!(!is_explicitly_classified(&Method::PATCH, "/api/shares"));
    }

    #[test]
    fn literal_routes_win_over_parameterized_patterns() {
        assert_eq!(
            route_template(&Method::GET, "/api/apps/updates"),
            "/api/apps/updates"
        );
        assert_eq!(
            route_template(&Method::GET, "/api/apps/calibre-web"),
            "/api/apps/{name}"
        );
        assert_eq!(
            route_template(&Method::GET, "/api/shares/config/samba"),
            "/api/shares/config/samba"
        );
        assert_eq!(
            route_template(&Method::GET, "/api/volumes/zfs/import"),
            "/api/volumes/zfs/import"
        );
        assert_eq!(
            route_template(&Method::GET, "/api/volumes/zfs/tank"),
            "/api/volumes/zfs/{pool}"
        );
    }

    #[test]
    fn action_names_are_stable_and_dotted() {
        assert_eq!(action_name(&Method::POST, "/api/shares"), "shares.create");
        assert_eq!(
            action_name(&Method::PUT, "/api/shares/{name}"),
            "shares.update"
        );
        assert_eq!(
            action_name(&Method::DELETE, "/api/shares/{name}"),
            "shares.delete"
        );
        assert_eq!(
            action_name(&Method::POST, "/api/system/reboot"),
            "system.reboot"
        );
        assert_eq!(
            action_name(&Method::POST, "/api/users/{uid}/password"),
            "users.password"
        );
        assert_eq!(
            action_name(&Method::POST, "/api/virt/vms/{name}/start"),
            "virt.vms.start"
        );
    }

    #[test]
    fn action_target_is_the_first_path_parameter() {
        assert_eq!(
            action_target("/api/shares/{name}", "/api/shares/team"),
            Some("team".to_string())
        );
        assert_eq!(
            action_target("/api/users/{uid}/password", "/api/users/alice/password"),
            Some("alice".to_string())
        );
        assert_eq!(action_target("/api/shares", "/api/shares"), None);
    }

    #[test]
    fn head_and_options_follow_the_get_rule() {
        assert!(is_explicitly_classified(&Method::HEAD, "/api/auth/me"));
        assert_eq!(
            minimum_role(&Method::OPTIONS, "/api/auth/me"),
            Role::Baseline
        );
    }
}
