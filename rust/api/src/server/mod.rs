//! The API HTTP server (port of the `api/internal/server` router + the S1
//! handler set). Every owner-facing route lives on the `owner` router behind
//! `require_auth`; `/api/auth/me`, `/api/dashboard` and `/api/metrics` are
//! auth-but-not-admin; the rest of `/api/` is admin-only. Routes whose ported
//! handler is not in this slice return a documented 501 Not Implemented so the
//! route surface (and its auth) already matches Go.

pub mod app_jobs;
pub mod apps;
pub mod buddy;
pub mod buddy_owner;
pub mod buddy_restore;
pub mod buddy_schedules;
pub mod buddy_send;
pub mod catalog;
pub mod config_export;
pub mod dataset_registry;
pub mod ddns;
pub mod disks;
pub mod domains;
pub mod handlers;
pub mod mcp;
pub mod network;
pub mod notifications;
pub mod pods;
pub mod providers;
pub mod replication;
pub mod reports;
pub mod shares;
pub mod storage_schedules;
pub mod system;
pub mod users;
pub mod virt;
pub mod ws;
pub mod zfs;

use crate::auth;
use crate::state::AppState;
use axum::http::StatusCode;
use axum::middleware;
use axum::response::Response;
use axum::routing::{any, get, post};
use axum::Router;
use std::sync::Arc;

/// The status a not-yet-ported owner route returns.
pub const NOT_PORTED_STATUS: u16 = 501;

/// Auth-but-not-admin routes (any authenticated user). No auth layers: the
/// public router applies them, the MCP internal router injects a synthetic
/// identity instead.
fn user_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/auth/me", get(handlers::auth_me))
        .route("/api/dashboard", get(handlers::dashboard))
        .route("/api/metrics", get(handlers::metrics))
}

/// Admin-only owner routes. Handlers not in S1 answer 501. No auth layers for
/// the same reason as `user_routes`.
fn owner_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/catalog", get(catalog::catalog_get))
        .route("/api/catalog/{name}", get(catalog::catalog_app_get))
        .route("/api/apps", get(apps::apps_get).post(apps::apps_post))
        .route("/api/apps/jobs", get(apps::app_jobs_get))
        .route("/api/apps/jobs/{id}", get(apps::app_job_detail_get))
        // Registered before `{name}`: axum matches in order here, so these
        // would otherwise be parsed as app names.
        .route("/api/apps/updates", get(apps::app_updates_get))
        .route("/api/apps/usage", get(apps::app_usage_get))
        .route(
            "/api/apps/namespaces",
            get(apps::app_namespaces_get).put(apps::app_namespaces_put),
        )
        .route(
            "/api/apps/{name}",
            get(apps::app_detail_get)
                .put(apps::app_detail_put)
                .delete(apps::app_detail_delete),
        )
        .route(
            "/api/apps/{name}/exposure",
            get(apps::app_exposure_get).put(apps::app_exposure_put),
        )
        .route(
            "/api/apps/{name}/dns/reconcile",
            post(apps::app_dns_reconcile_post),
        )
        .route("/api/apps/{name}/services", get(apps::app_services_get))
        .route("/api/apps/{name}/pods", get(apps::app_pods_get))
        .route("/api/apps/{name}/quota", get(apps::app_quota_get))
        .route(
            "/api/sources",
            get(catalog::sources_get).post(catalog::sources_post),
        )
        .route("/api/sources/refresh", post(catalog::sources_refresh))
        .route(
            "/api/sources/{name}",
            get(catalog::source_detail_get).delete(catalog::source_detail_delete),
        )
        .route(
            "/api/domains",
            get(domains::domains_get).post(domains::domains_post),
        )
        .route(
            "/api/domains/{name}/certificate",
            get(domains::domain_certificate_get),
        )
        .route("/api/domains/{name}/sso", post(domains::domain_sso_post))
        .route(
            "/api/domains/{name}",
            get(domains::domain_detail_get)
                .put(domains::domain_detail_put)
                .delete(domains::domain_detail_delete),
        )
        .route("/api/providers", get(providers::providers_get))
        .route("/api/ddns", get(ddns::ddns_get).post(ddns::ddns_post))
        .route("/api/ddns/{id}/run", post(ddns::ddns_run_post))
        .route(
            "/api/ddns/{id}",
            get(ddns::ddns_detail_get)
                .put(ddns::ddns_detail_put)
                .delete(ddns::ddns_detail_delete),
        )
        .route("/api/system/version", get(system::version))
        .route("/api/system/config/export", get(config_export::export))
        .route("/api/system/updates", get(system::updates_get))
        .route("/api/system/updates/check", post(system::updates_check))
        .route("/api/system/reboot", post(system::reboot_post))
        .route("/api/system/operations", post(system::operation_post))
        .route(
            "/api/system/operations/current",
            get(system::operation_current),
        )
        // Bounded historical reporting (FR-RPT). Read-only and admin-only.
        .route("/api/reports", get(reports::reports_get))
        .route("/api/reports/series", get(reports::reports_series_get))
        .route("/api/disks", get(disks::disks_get))
        .route("/api/disks/health", get(disks::disks_health_get))
        .route("/api/disks/recommend", post(disks::disks_recommend))
        .route(
            "/api/volumes/zfs",
            get(zfs::pools_get).post(zfs::pools_post),
        )
        .route(
            "/api/volumes/zfs/import",
            get(zfs::import_get).post(zfs::import_post),
        )
        .route("/api/volumes/zfs/{pool}/health", get(zfs::pool_health_get))
        .route(
            "/api/volumes/zfs/{pool}/scrub",
            post(storage_schedules::pool_scrub_post),
        )
        .route(
            "/api/volumes/zfs/{pool}/devices",
            post(zfs::pool_devices_post),
        )
        .route(
            "/api/volumes/zfs/{pool}",
            get(zfs::pool_detail_get).delete(zfs::pool_detail_delete),
        )
        .route(
            "/api/storage/schedules",
            get(storage_schedules::schedules_get)
                .post(storage_schedules::schedules_post)
                .delete(storage_schedules::schedules_delete),
        )
        .route("/api/replication/send", post(replication::send_post))
        .route("/api/replication/jobs", get(replication::jobs_get))
        .route(
            "/api/replication/jobs/{id}",
            get(replication::job_detail_get).delete(replication::job_detail_delete),
        )
        .route(
            "/api/datasets",
            get(zfs::datasets_get)
                .post(zfs::datasets_post)
                .delete(zfs::datasets_delete),
        )
        .route(
            "/api/zvols",
            get(zfs::zvols_get)
                .post(zfs::zvols_post)
                .put(zfs::zvols_resize)
                .delete(zfs::zvols_delete),
        )
        .route("/api/virt/capabilities", get(virt::capabilities_get))
        .route("/api/virt/vms", get(virt::vms_get).post(virt::vms_post))
        .route(
            "/api/virt/vms/{name}",
            get(virt::vm_detail_get)
                .put(virt::vm_detail_put)
                .delete(virt::vm_detail_delete),
        )
        .route("/api/virt/vms/{name}/start", post(virt::vm_start_post))
        .route("/api/virt/vms/{name}/stop", post(virt::vm_stop_post))
        .route(
            "/api/virt/vms/{name}/force-stop",
            post(virt::vm_force_stop_post),
        )
        .route("/api/virt/vms/{name}/restart", post(virt::vm_restart_post))
        .route(
            "/api/virt/vms/{name}/snapshots",
            get(virt::vm_snapshots_get).post(virt::vm_snapshot_post),
        )
        .route(
            "/api/virt/vms/{name}/snapshots/{snap}/restore",
            post(virt::vm_snapshot_restore_post),
        )
        .route("/api/virt/images", get(virt::images_get))
        .route("/api/virt/images/upload", post(virt::image_upload_post))
        .route("/api/virt/images/import", post(virt::image_import_post))
        .route(
            "/api/virt/images/{name}",
            axum::routing::delete(virt::image_delete),
        )
        .route("/api/ws/vm/{name}/vnc", get(virt::vm_vnc_ws))
        .route("/api/ws/logs", get(ws::logs_ws))
        .route("/api/pods", get(pods::pods_get))
        .route("/api/namespaces", get(pods::namespaces_get))
        .route("/api/ws/exec", get(ws::exec_ws))
        .route(
            "/api/shares",
            get(shares::shares_get).post(shares::shares_post),
        )
        .route("/api/shares/paths", get(shares::share_paths_get))
        .route(
            "/api/shares/folders",
            get(shares::share_folders_get)
                .post(shares::share_folders_post)
                .delete(shares::share_folders_delete),
        )
        .route("/api/shares/status", get(shares::shares_status_get))
        .route(
            "/api/shares/apply",
            post(shares::shares_apply_post).put(shares::shares_apply_post),
        )
        .route("/api/shares/config/samba", get(shares::samba_config_get))
        .route("/api/shares/config/nfs", get(shares::nfs_config_get))
        .route(
            "/api/shares/{name}",
            get(shares::share_detail_get)
                .put(shares::share_detail_put)
                .delete(shares::share_detail_delete),
        )
        .route(
            "/api/notifications",
            get(notifications::notifications_get).put(notifications::notifications_put),
        )
        .route(
            "/api/notifications/test",
            post(notifications::notification_test_post),
        )
        .route(
            "/api/notifications/webhook",
            get(notifications::webhook_get)
                .put(notifications::webhook_put)
                .delete(notifications::webhook_delete),
        )
        .route(
            "/api/notifications/webhook/test",
            post(notifications::webhook_test_post),
        )
        // Host network settings (FR-NET). Literal change routes before the
        // `{id}` wildcard, and all of them before the `/api/{*rest}` fallback.
        .route("/api/network", get(network::network_get))
        .route("/api/network/changes", post(network::change_post))
        .route(
            "/api/network/changes/{id}/revert",
            post(network::change_revert_post),
        )
        .route("/api/network/changes/{id}", get(network::change_detail_get))
        .route("/api/buddy/status", get(buddy_owner::status))
        .route(
            "/api/buddy/peers",
            get(buddy_owner::peers_get)
                .post(buddy_owner::peers_post)
                .delete(buddy_owner::peers_delete),
        )
        .route(
            "/api/buddy/identity",
            get(buddy_owner::identity_get).post(buddy_owner::identity_post),
        )
        .route("/api/buddy/send", post(buddy_send::send_post))
        .route("/api/buddy/restore", post(buddy_restore::restore))
        .route("/api/buddy/jobs", get(buddy_send::jobs_get))
        .route(
            "/api/buddy/jobs/{id}",
            get(buddy_send::job_detail_get).delete(buddy_send::job_detail_delete),
        )
        .route(
            "/api/buddy/schedules",
            get(buddy_schedules::schedules_get)
                .post(buddy_schedules::schedules_post)
                .delete(buddy_schedules::schedules_delete),
        )
        .route("/api/users", get(users::users_get).post(users::users_post))
        .route(
            "/api/users/{uid}",
            get(users::user_detail_get)
                .put(users::user_detail_put)
                .delete(users::user_detail_delete),
        )
        .route("/api/users/{uid}/password", post(users::user_password_post))
        .route("/api/users/{uid}/{action}", post(users::user_enable_post))
        .route(
            "/api/groups",
            get(users::groups_get).post(users::groups_post),
        )
        .route(
            "/api/datasets/registry",
            get(dataset_registry::list).post(dataset_registry::create),
        )
        .route(
            "/api/datasets/registry/{name}",
            axum::routing::delete(dataset_registry::delete),
        )
        .route(
            "/api/groups/{cn}",
            get(users::group_detail_get)
                .put(users::group_detail_put)
                .delete(users::group_detail_delete),
        )
        // The MCP settings surface (FR-MCP). Mounted on the owner router
        // like every other admin route; the MCP endpoint itself is a
        // separate public route at `/mcp` guarded by bearer tokens.
        .route("/api/mcp", get(mcp::mcp_get).put(mcp::mcp_put))
        .route("/api/mcp/tokens", post(mcp::tokens_post))
        .route(
            "/api/mcp/tokens/{id}",
            axum::routing::delete(mcp::token_delete),
        )
        // Unknown /api paths stay behind auth so their existence is not
        // leaked; the public fallback below only serves non-API paths.
        .route("/api/{*rest}", any(not_found))
}

/// The full authenticated route surface, built for `build_router` (auth
/// layers applied) and `internal_router` (synthetic MCP identity) alike.
pub fn build_router(state: Arc<AppState>) -> Router {
    // Public: health probes.
    let mut public: Router<Arc<AppState>> = Router::new()
        .route("/api/health", get(handlers::health))
        .route("/api/ready", get(handlers::ready));

    // Buddy peers authenticate with their own Ed25519 key, not the proxy.
    //
    // Mount the receiver routes only when a receiver exists. The `BuddyState`
    // extractor requires one, and the release profile is `panic = "abort"`, so
    // mounting them unconditionally turned any request into an API-process
    // abort on a Buddy-disabled install (AUD-2611-T6-1). A disabled install
    // answers 503 instead.
    if state.buddy.is_some() {
        public = public
            .route("/api/buddy/v1/status", get(buddy::status))
            .route("/api/buddy/v1/enroll", post(buddy::enroll))
            .route("/api/buddy/v1/backups", get(buddy::backups))
            .route(
                "/api/buddy/v1/chunks/{*source}",
                get(buddy::chunks).put(buddy::chunks),
            )
            .route("/api/buddy/v1/chains/{*source}", get(buddy::chains))
            .route("/api/buddy/v1/sequence/{*source}", get(buddy::sequence))
            .route(
                "/api/buddy/v1/manifest/{*source}",
                get(buddy::manifest).put(buddy::manifest),
            )
            .route("/api/buddy/v1/prune/{*source}", post(buddy::prune));
    } else {
        public = public
            .route("/api/buddy/v1", any(buddy_disabled))
            .route("/api/buddy/v1/{*rest}", any(buddy_disabled));
    }

    // Auth-but-not-admin (any authenticated user).
    let user = user_routes().layer(middleware::from_fn_with_state(
        state.clone(),
        auth::require_auth,
    ));

    // Admin-only owner routes. Handlers not in S1 answer 501.
    let owner = owner_routes()
        .layer(middleware::from_fn(auth::require_admin))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth,
        ));

    // The MCP endpoint (FR-MCP). Public reachability, but the guard rejects
    // everything without a valid bearer token, so it is never actually
    // anonymous: disabled -> 404, no/invalid token -> 401, blocked -> 429.
    // The proxy-secret/CIDR checks still apply (SEC-10 has no opt-out).
    let mcp: Router<Arc<AppState>> = Router::new()
        .route("/mcp", any(crate::mcp::service::entry))
        .route("/mcp/", any(crate::mcp::service::entry))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::mcp::auth::guard,
        ));

    public
        .merge(user)
        .merge(owner)
        .merge(mcp)
        .with_state(state)
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
}

/// The MCP tool dispatcher's view of the API: every authenticated route
/// without the auth middlewares. It is NEVER mounted on the HTTP listener —
/// only `build_router` is — and the dispatch inserts a synthetic
/// `mcp:<token>` identity per call, so tools get validated, owner-gated
/// handler behavior (validation, agent/vmd calls, error shapes) for free.
///
/// The security model rests on this function not being reachable from the
/// network: `tests::forged_identity_cannot_bypass_the_public_auth_layers`
/// proves client-injected identities cannot bypass `build_router`'s layers.
pub fn internal_router(state: Arc<AppState>) -> Router {
    owner_routes()
        .merge(user_routes())
        .with_state(state)
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
}

/// Answer for the Buddy peer routes on an install where the receiver is off
/// (`BUDDY_RECEIVE_PATH` unset / `buddy.enabled=false`).
async fn buddy_disabled() -> Response {
    crate::json(
        StatusCode::SERVICE_UNAVAILABLE,
        serde_json::json!({ "error": "Buddy Backup is disabled on this instance" }),
    )
}

async fn not_found() -> Response {
    crate::json(
        StatusCode::NOT_FOUND,
        serde_json::json!({ "error": "not found" }),
    )
}

async fn method_not_allowed() -> Response {
    crate::json(
        StatusCode::METHOD_NOT_ALLOWED,
        serde_json::json!({ "error": "method not allowed" }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::http::Request;
    use std::net::SocketAddr;
    use tower::ServiceExt;

    /// Dummy values for the registry probe and the auth sweep: realistic
    /// placeholders for `{param}` segments.
    fn dummy_args() -> serde_json::Map<String, serde_json::Value> {
        let mut map = serde_json::Map::new();
        for key in ["name", "pool", "uid", "cn", "id", "snap"] {
            map.insert(key.into(), serde_json::json!("mcp-probe"));
        }
        map.insert("service".into(), serde_json::json!("samba"));
        map.insert("action".into(), serde_json::json!("enable"));
        map
    }

    /// Resolve `{param}` and add the query args a tool declares.
    fn probe_uri(path: &str, query: &[&str]) -> String {
        let args = dummy_args();
        let mut out = String::new();
        let mut rest = path;
        while let Some((head, tail)) = rest.split_once('{') {
            let (param, after) = tail.split_once('}').unwrap();
            let value = args
                .get(param)
                .and_then(serde_json::Value::as_str)
                .filter(|v| !v.is_empty())
                .unwrap_or("mcp-probe");
            out.push_str(head);
            out.push_str(value);
            rest = after;
        }
        out.push_str(rest);
        if !query.is_empty() {
            let pairs: Vec<String> = query
                .iter()
                .map(|key| {
                    format!(
                        "{key}={}",
                        args.get(*key)
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("mcp-probe")
                    )
                })
                .collect();
            out.push('?');
            out.push_str(&pairs.join("&"));
        }
        out
    }

    /// The owner/user route inventory: every MCP registry entry (resolved
    /// with dummy values) plus the routes that are not tools (WS endpoints,
    /// the settings API surface).
    fn route_inventory() -> Vec<(axum::http::Method, String)> {
        let mut out = Vec::new();
        for tool in crate::mcp::tools::all_tools() {
            if tool.special != crate::mcp::tools::Special::None {
                continue; // no route to match (pod_logs)
            }
            out.push((tool.method.clone(), probe_uri(tool.path, tool.query)));
        }
        for (method, path) in [
            (axum::http::Method::GET, "/api/ws/logs".to_string()),
            (axum::http::Method::GET, "/api/ws/exec".to_string()),
            (
                axum::http::Method::GET,
                "/api/ws/vm/mcp-probe/vnc".to_string(),
            ),
            (
                axum::http::Method::GET,
                "/api/system/config/export".to_string(),
            ),
            (axum::http::Method::GET, "/api/mcp".to_string()),
            (axum::http::Method::PUT, "/api/mcp".to_string()),
            (axum::http::Method::POST, "/api/mcp/tokens".to_string()),
            (
                axum::http::Method::DELETE,
                "/api/mcp/tokens/mcp-probe".to_string(),
            ),
            (axum::http::Method::GET, "/api/reports".to_string()),
            (
                axum::http::Method::GET,
                "/api/reports/series?range=24h".to_string(),
            ),
        ] {
            out.push((method, path));
        }
        out
    }

    fn signed(
        secret: Option<&str>,
        user: &str,
        groups: &str,
        method: axum::http::Method,
        uri: &str,
        with_identity: bool,
    ) -> Request<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 55555u16))));
        if let Some(secret) = secret {
            builder = builder.header(auth::PROXY_SECRET_HEADER, secret);
        }
        if !user.is_empty() {
            builder = builder.header("Remote-User", user);
        }
        if !groups.is_empty() {
            builder = builder.header("Remote-Groups", groups);
        }
        let mut request = builder.body(Body::empty()).unwrap();
        if with_identity {
            request.extensions_mut().insert(Arc::new(auth::UserInfo {
                username: "forged-admin".into(),
                groups: vec!["naslos_admins".into()],
                email: String::new(),
                display_name: String::new(),
            }));
        }
        request
    }

    #[tokio::test]
    async fn every_route_answers_401_without_the_proxy_secret() {
        let state = Arc::new(AppState::for_test("test-secret"));
        let app = build_router(state);
        for (method, uri) in route_inventory() {
            let response = app
                .clone()
                .oneshot(signed(None, "", "", method.clone(), &uri, false))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {uri} must be 401 without the proxy secret"
            );
        }
    }

    #[tokio::test]
    async fn every_owner_route_is_403_for_non_admins() {
        let state = Arc::new(AppState::for_test("test-secret"));
        let app = build_router(state);
        for (method, uri) in route_inventory() {
            if uri.starts_with("/api/auth/")
                || uri.starts_with("/api/dashboard")
                || uri.starts_with("/api/metrics")
            {
                // User routes: authenticated, not admin-gated.
                let response = app
                    .clone()
                    .oneshot(signed(
                        Some("test-secret"),
                        "viewer",
                        "naslos_users",
                        method.clone(),
                        &uri,
                        false,
                    ))
                    .await
                    .unwrap();
                assert_ne!(
                    response.status(),
                    StatusCode::FORBIDDEN,
                    "{method} {uri} must not be admin-gated"
                );
                continue;
            }
            let response = app
                .clone()
                .oneshot(signed(
                    Some("test-secret"),
                    "viewer",
                    "naslos_users",
                    method.clone(),
                    &uri,
                    false,
                ))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "{method} {uri} must be 403 for a non-admin"
            );
        }
    }

    #[tokio::test]
    async fn admin_requests_are_not_blocked_by_the_auth_layers() {
        // Writes carry a non-JSON content type so the auth layers' JSON guard
        // (which runs after the identity checks) rejects them with 415: the
        // sweep proves the auth gates pass without executing any handler.
        let state = Arc::new(AppState::for_test("test-secret"));
        let app = build_router(state);
        for (method, uri) in route_inventory() {
            let mut request = signed(
                Some("test-secret"),
                "boss",
                "naslos_admins",
                method.clone(),
                &uri,
                false,
            );
            if matches!(method, axum::http::Method::POST | axum::http::Method::PUT) {
                request.headers_mut().insert(
                    axum::http::header::CONTENT_TYPE,
                    axum::http::HeaderValue::from_static("text/plain"),
                );
                *request.body_mut() = Body::from("not json");
            }
            let response = app.clone().oneshot(request).await.unwrap();
            assert_ne!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{} {uri} must not 401 for an admin",
                method
            );
            assert_ne!(
                response.status(),
                StatusCode::FORBIDDEN,
                "{} {uri} must not 403 for an admin",
                method
            );
        }
    }

    /// The MCP security model: only `build_router` is ever mounted, and a
    /// client can never inject request extensions over the wire. This test
    /// forges an admin identity in the extensions the way the internal
    /// dispatch does, then proves `build_router`'s layers ignore it: the
    /// headers still decide.
    #[tokio::test]
    async fn forged_identity_cannot_bypass_the_public_auth_layers() {
        let state = Arc::new(AppState::for_test("test-secret"));
        let app = build_router(state);
        for (method, uri) in route_inventory() {
            // With a forged identity but no secret: still 401.
            let response = app
                .clone()
                .oneshot(signed(None, "", "", method.clone(), &uri, true))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {uri}: a forged identity must not replace the proxy secret"
            );
            // With a forged identity AND the secret but no admin group:
            // still 403 on owner routes.
            if uri.starts_with("/api/auth/")
                || uri.starts_with("/api/dashboard")
                || uri.starts_with("/api/metrics")
            {
                continue;
            }
            let response = app
                .clone()
                .oneshot(signed(
                    Some("test-secret"),
                    "viewer",
                    "naslos_users",
                    method.clone(),
                    &uri,
                    true,
                ))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "{method} {uri}: a forged identity must not satisfy require_admin"
            );
        }
    }

    /// The internal router serves the same routes without auth, is never
    /// built by `build_router`, and keeps the public-only routes out.
    #[tokio::test]
    async fn internal_router_answers_with_a_synthetic_identity_and_hides_public_routes() {
        let state = Arc::new(AppState::for_test("test-secret"));
        let app = internal_router(state.clone());

        // Synthetic identity: answers without any proxy headers.
        let request = Request::builder()
            .uri("/api/dashboard")
            .extension(Arc::new(auth::UserInfo {
                username: "mcp:probe".into(),
                groups: vec!["naslos_admins".into()],
                email: String::new(),
                display_name: String::new(),
            }))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Public-only routes are absent from the internal router.
        for uri in ["/api/health", "/api/ready"] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::NOT_FOUND,
                "{uri} must not exist on the internal router"
            );
        }
    }

    /// Every registry entry must match a real route. The `/api/{*rest}`
    /// catch-all (any method) makes status-only probing useless: a wrong
    /// path or method is indistinguishable from an unknown one by status.
    /// The discriminant is the catch-all's exact body — no handler emits
    /// it — so a probe that answers anything else proves the specific
    /// (method, path) matched and reached a handler.
    #[tokio::test]
    async fn every_registry_tool_matches_a_real_route() {
        const CATCHALL_BODY: &str = "{\"error\":\"not found\"}\n";
        let state = Arc::new(AppState::for_test("test-secret"));
        let app = internal_router(state);

        for tool in crate::mcp::tools::all_tools() {
            if tool.special != crate::mcp::tools::Special::None {
                continue; // handled inside the dispatcher, no route
            }
            if tool.name == "buddy_identity_create" {
                // identity_post swallows an unreadable body (unwrap_or_default)
                // and then writes a key file to the host; probing it would
                // create /var/lib/naslos/buddy-identity.json. Its path is
                // verified by the buddy_identity_get probe on the same
                // pattern, and its method sits on that same route line.
                continue;
            }
            let uri = probe_uri(tool.path, tool.query);

            // Writes send an extractor-breaking body (wrong content type):
            // the handler's own rejection proves the route matched. GET and
            // DELETE probe empty; the handler answers against the inert
            // for_test state.
            let builder = Request::builder()
                .method(tool.method.clone())
                .uri(&uri)
                .header(axum::http::header::CONTENT_TYPE, "text/plain");
            let probe = if matches!(
                tool.method,
                axum::http::Method::POST | axum::http::Method::PUT
            ) {
                builder.body(Body::from("not json"))
            } else {
                builder.body(Body::empty())
            }
            .unwrap();
            let response = app.clone().oneshot(probe).await.unwrap();
            let status = response.status();
            let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
                .await
                .unwrap();
            let fell_through =
                status == StatusCode::NOT_FOUND && body.as_ref() == CATCHALL_BODY.as_bytes();
            assert!(
                !fell_through,
                "{} -> {} {}: only the /api catch-all answered, so the tool's \
                 method+path matches no route",
                tool.name, tool.method, uri
            );
        }
    }

    /// The destructive-tool confirm rule: the registry must never offer a
    /// `full` tool without a confirm target, and every confirm target is an
    /// argument the tool's schema actually declares.
    #[test]
    fn full_tools_always_declare_a_confirm_target() {
        for tool in crate::mcp::tools::all_tools() {
            let schema = (tool.schema)();
            let props = schema
                .get("properties")
                .and_then(serde_json::Value::as_object)
                .cloned()
                .unwrap_or_default();
            match tool.confirm {
                crate::mcp::tools::Confirm::No => {
                    if tool.level == crate::mcp::store::Scope::Full {
                        panic!("{} is full but has no confirm target", tool.name);
                    }
                }
                crate::mcp::tools::Confirm::Arg(param) => {
                    assert!(
                        props.contains_key(param),
                        "{}: confirm target {param} is missing from the schema",
                        tool.name
                    );
                    assert!(
                        props.contains_key("confirm"),
                        "{}: the schema does not declare the confirm argument",
                        tool.name
                    );
                }
                crate::mcp::tools::Confirm::Literal(_) => {
                    assert!(
                        props.contains_key("confirm"),
                        "{}: the schema does not declare the confirm argument",
                        tool.name
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn buddy_routes_are_unavailable_when_the_receiver_is_disabled() {
        let state = Arc::new(AppState::for_test("test-secret"));
        let app = build_router(state);
        for uri in [
            "/api/buddy/v1/status",
            "/api/buddy/v1/enroll",
            "/api/buddy/v1/chunks/media/app",
            "/api/buddy/v1/manifest/media/app",
        ] {
            let request = Request::builder().uri(uri).body(Body::empty()).unwrap();
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(
                response.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "{uri} must be 503 when the receiver is disabled"
            );
        }
    }

    #[tokio::test]
    async fn health_stays_public_when_buddy_is_disabled() {
        let state = Arc::new(AppState::for_test("test-secret"));
        let request = Request::builder()
            .uri("/api/health")
            .body(Body::empty())
            .unwrap();
        let response = build_router(state).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
