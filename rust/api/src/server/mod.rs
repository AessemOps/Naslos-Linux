//! The API HTTP server (port of the `api/internal/server` router + the S1
//! handler set). Every owner-facing route lives on the `owner` router behind
//! `require_auth`; `/api/auth/me`, `/api/dashboard` and `/api/metrics` are
//! auth-but-not-admin; the rest of `/api/` is admin-only. Routes whose ported
//! handler is not in this slice return a documented 501 Not Implemented so the
//! route surface (and its auth) already matches Go.

pub mod app_jobs;
pub mod apps;
pub mod catalog;
pub mod ddns;
pub mod disks;
pub mod domains;
pub mod handlers;
pub mod providers;
pub mod shares;
pub mod users;
pub mod zfs;

use crate::auth;
use crate::state::AppState;
use axum::middleware;
use axum::routing::{any, get, post};
use axum::Router;
use std::sync::Arc;

/// The status a not-yet-ported owner route returns.
pub const NOT_PORTED_STATUS: u16 = 501;

pub fn build_router(state: Arc<AppState>) -> Router {
    // Public: health probes.
    let public: Router<Arc<AppState>> = Router::new()
        .route("/api/health", get(handlers::health))
        .route("/api/ready", get(handlers::ready));

    // Auth-but-not-admin (any authenticated user).
    let user: Router<Arc<AppState>> = Router::new()
        .route("/api/auth/me", get(handlers::auth_me))
        .route("/api/dashboard", get(handlers::dashboard))
        .route("/api/metrics", get(handlers::metrics))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth,
        ));

    // Admin-only owner routes. Handlers not in S1 answer 501.
    let owner: Router<Arc<AppState>> = Router::new()
        .route("/api/catalog", get(catalog::catalog_get))
        .route("/api/catalog/{name}", get(catalog::catalog_app_get))
        .route("/api/apps", get(apps::apps_get).post(apps::apps_post))
        .route("/api/apps/jobs", get(apps::app_jobs_get))
        .route("/api/apps/jobs/{id}", get(apps::app_job_detail_get))
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
        .route("/api/apps/{name}/services", get(apps::app_services_get))
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
        .route("/api/disks", get(disks::disks_get))
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
            "/api/volumes/zfs/{pool}/devices",
            post(zfs::pool_devices_post),
        )
        .route(
            "/api/volumes/zfs/{pool}",
            get(zfs::pool_detail_get).delete(zfs::pool_detail_delete),
        )
        .route(
            "/api/datasets",
            get(zfs::datasets_get)
                .post(zfs::datasets_post)
                .delete(zfs::datasets_delete),
        )
        .route("/api/ws/logs", any(handlers::not_ported))
        .route("/api/pods", any(handlers::not_ported))
        .route("/api/namespaces", any(handlers::not_ported))
        .route("/api/ws/exec", any(handlers::not_ported))
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
        .route("/api/notifications", any(handlers::not_ported))
        .route("/api/notifications/test", any(handlers::not_ported))
        .route("/api/buddy/status", any(handlers::not_ported))
        .route("/api/buddy/peers", any(handlers::not_ported))
        .route("/api/buddy/identity", any(handlers::not_ported))
        .route("/api/buddy/send", any(handlers::not_ported))
        .route("/api/buddy/restore", any(handlers::not_ported))
        .route("/api/buddy/jobs", any(handlers::not_ported))
        .route("/api/buddy/jobs/{*rest}", any(handlers::not_ported))
        .route("/api/buddy/schedules", any(handlers::not_ported))
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
            "/api/groups/{cn}",
            get(users::group_detail_get)
                .put(users::group_detail_put)
                .delete(users::group_detail_delete),
        )
        .layer(middleware::from_fn(auth::require_admin))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth,
        ));

    public.merge(user).merge(owner).with_state(state)
}
