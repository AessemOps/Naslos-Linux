//! S1 handlers: health, ready, auth/me, dashboard, metrics, and the 501
//! placeholder for routes whose ported handler is not in this slice.

use crate::auth::user_from_request;
use crate::json;
use crate::state::AppState;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::Response;
use std::sync::Arc;

pub async fn health() -> Response {
    json(StatusCode::OK, serde_json::json!({ "status": "ok" }))
}

/// Readiness never fails when LDAP is down (the UI and the API's non-identity
/// routes keep working), but the reported state is measured instead of being
/// hard-coded (SEC-2026-H3).
///
/// The probe is cached inside the identity client: this route is public, and a
/// search per request would queue on the single LDAP connection and become a
/// cheap way to block every identity-backed admin operation.
pub async fn ready(State(state): State<Arc<AppState>>) -> Response {
    let ldap = match &state.identity {
        None => "down",
        Some(identity) => {
            match tokio::time::timeout(std::time::Duration::from_secs(2), identity.ready()).await {
                Ok(Ok(())) => "up",
                _ => "down",
            }
        }
    };
    json(
        StatusCode::OK,
        serde_json::json!({ "status": "ok", "ldap": ldap }),
    )
}

/// `GET /api/auth/me` — the caller's identity from the trusted headers.
pub async fn auth_me(State(state): State<Arc<AppState>>, req: Request) -> Response {
    if state.identity.is_none() {
        return json(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({ "error": "Identity/LDAP is not configured (check LDAP_HOST, LDAP_BIND_PASS_FILE, and that OpenLDAP is running)" }),
        );
    }
    let Some(user) = user_from_request(&req) else {
        return json(
            StatusCode::UNAUTHORIZED,
            serde_json::json!({ "error": "unauthorized" }),
        );
    };
    json(
        StatusCode::OK,
        serde_json::json!({
            "username": user.username,
            "groups": user.groups.join(","),
            "email": user.email,
            "displayName": user.display_name,
        }),
    )
}

/// `GET /api/dashboard`
pub async fn dashboard(State(state): State<Arc<AppState>>) -> Response {
    json(StatusCode::OK, state.metrics.dashboard_data())
}

/// `GET /api/metrics`
pub async fn metrics(State(state): State<Arc<AppState>>) -> Response {
    json(StatusCode::OK, state.metrics.full_data())
}

/// A route whose Go handler is not yet ported. Deliberately explicit: the route
/// and its auth are correct, the behavior is not implemented yet.
pub async fn not_ported() -> Response {
    json(
        super::NOT_PORTED_STATUS.into_status(),
        serde_json::json!({ "error": "not implemented in the Rust port yet" }),
    )
}

trait IntoStatus {
    fn into_status(self) -> StatusCode;
}

impl IntoStatus for u16 {
    fn into_status(self) -> StatusCode {
        StatusCode::from_u16(self).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
    }
}
