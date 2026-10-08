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

/// Readiness never fails when LDAP is down (ported behavior); the LDAP state is
/// reported for observability. S1 has no identity backend yet, so it is "down".
pub async fn ready() -> Response {
    json(
        StatusCode::OK,
        serde_json::json!({ "status": "ok", "ldap": "down" }),
    )
}

/// `GET /api/auth/me` — the caller's identity from the trusted headers.
pub async fn auth_me(req: Request) -> Response {
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
            "groups": user.groups,
            "email": user.email,
            "displayName": user.display_name,
            "isAdmin": user.is_admin(),
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
