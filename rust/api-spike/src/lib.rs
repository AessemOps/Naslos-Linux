//! Naslos API Rust spike (Phase 2 of the Go→Rust plan).
//!
//! A deliberately minimal API serving ONLY `/api/health`, `/api/ready`,
//! `/api/dashboard` and `/api/metrics`, drawn from the Go server's contracts
//! (`api/internal/server/{server,metrics}.go`, `api/internal/metrics`). Its
//! single purpose is to measure a realistic Rust API's resident memory against
//! the Go API (~98 MiB working set / 18.8 MiB RSS live) before committing to the
//! full 20-package port.

pub mod auth;
pub mod metrics;
pub mod state;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{middleware, Router};
pub use state::AppState;
use std::sync::Arc;

/// Build the application router. `/api/health` and `/api/ready` are public;
/// every other `/api/` route sits behind the proxy auth middleware, so a new
/// owner route is authenticated by default.
pub fn build_router(state: Arc<AppState>) -> Router {
    let public: Router<Arc<AppState>> = Router::new()
        .route("/api/health", get(health))
        .route("/api/ready", get(ready));

    let owner: Router<Arc<AppState>> = Router::new()
        .route("/api/dashboard", get(metrics::dashboard))
        .route("/api/metrics", get(metrics::metrics))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth,
        ));

    public.merge(owner).with_state(state)
}

async fn health(State(_state): State<Arc<AppState>>) -> Response {
    json(StatusCode::OK, serde_json::json!({ "status": "ok" }))
}

/// Mirrors the Go handler: readiness never fails when LDAP is down; the LDAP
/// state is reported for observability.
async fn ready(State(_state): State<Arc<AppState>>) -> Response {
    json(
        StatusCode::OK,
        serde_json::json!({ "status": "ok", "ldap": "down" }),
    )
}

/// Serialize like Go's `json.NewEncoder.Encode`: JSON plus a trailing newline.
pub fn json(status: StatusCode, value: serde_json::Value) -> Response {
    let mut body = serde_json::to_vec(&value).unwrap_or_default();
    body.push(b'\n');
    (
        status,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response()
}
