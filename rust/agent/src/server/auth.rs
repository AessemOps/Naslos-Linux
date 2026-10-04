//! Bearer-token authentication. The agent is privileged and host-networked, so
//! every route except `/health` must prove it is the API. There is no opt-out.

use super::error::write_error;
use super::AppState;
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use std::sync::Arc;
use subtle::ConstantTimeEq;

pub async fn require_auth(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    const PREFIX: &str = "Bearer ";
    let token = state.auth_token.as_str();

    let provided = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    let authorized = match provided {
        Some(value) if !token.is_empty() => match value.strip_prefix(PREFIX) {
            Some(rest) => rest.as_bytes().ct_eq(token.as_bytes()).into(),
            None => false,
        },
        _ => false,
    };

    if !authorized {
        return write_error(StatusCode::UNAUTHORIZED, "missing or invalid agent token");
    }
    next.run(req).await
}
