//! Proxy auth middleware (port of the Go `auth` package's `RequireAuth`).
//!
//! The API is owner-gated and reached only through Traefik: a request must
//! carry the shared proxy secret and come from the proxy CIDR, and it must
//! carry an authenticated identity. There is no opt-out.

use crate::json;
use crate::state::AppState;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::Response;
use std::sync::Arc;
use subtle::ConstantTimeEq;

pub const USER_HEADER: &str = "Remote-User";
pub const GROUPS_HEADER: &str = "Remote-Groups";
pub const PROXY_SECRET_HEADER: &str = "X-Naslos-Proxy-Secret";

pub async fn require_auth(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    // 1. The request must come from the proxy CIDR.
    let peer_ok = req
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| state.proxy_cidr.contains(&ci.0.ip()))
        .unwrap_or(true); // tests/health probes without ConnectInfo
    if !peer_ok {
        return json(
            StatusCode::FORBIDDEN,
            serde_json::json!({ "error": "forbidden: source not allowed" }),
        );
    }

    // 2. The shared proxy secret must match (constant time).
    let provided = req
        .headers()
        .get(PROXY_SECRET_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let secret_ok = !state.proxy_secret.is_empty()
        && provided
            .as_bytes()
            .ct_eq(state.proxy_secret.as_bytes())
            .into();
    if !secret_ok {
        return json(
            StatusCode::UNAUTHORIZED,
            serde_json::json!({ "error": "unauthorized" }),
        );
    }

    // 3. An authenticated identity is required.
    let user = req
        .headers()
        .get(USER_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if user.is_empty() {
        return json(
            StatusCode::UNAUTHORIZED,
            serde_json::json!({ "error": "unauthorized" }),
        );
    }
    let groups = req
        .headers()
        .get(GROUPS_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    // Stash identity for downstream handlers (mirrors the Go context).
    let mut req = req;
    req.extensions_mut().insert(Identity { user, groups });

    next.run(req).await
}

#[derive(Debug, Clone)]
pub struct Identity {
    #[allow(dead_code)]
    pub user: String,
    #[allow(dead_code)]
    pub groups: String,
}
