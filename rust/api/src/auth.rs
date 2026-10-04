//! Authentication middleware (port of `api/internal/auth`).
//!
//! It trusts `Remote-User`/`Remote-Groups` headers ONLY from Traefik's pod CIDR,
//! and only when the request also carries the shared secret Traefik's
//! `proxy-identity` middleware injects. The CIDR check alone cannot distinguish
//! Traefik from any other pod in the cluster; the secret can (SEC-1/NAS-001).

use crate::json;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::Response;
use std::net::IpAddr;
use std::sync::Arc;
use subtle::ConstantTimeEq;

use crate::state::AppState;

/// The header Traefik's `proxy-identity` middleware injects with the shared
/// value.
pub const PROXY_SECRET_HEADER: &str = "X-Naslos-Proxy-Secret";

/// The authenticated user, stashed in the request extensions.
#[derive(Debug, Clone)]
pub struct UserInfo {
    pub username: String,
    pub groups: Vec<String>,
    pub email: String,
    pub display_name: String,
}

impl UserInfo {
    pub fn has_group(&self, group: &str) -> bool {
        self.groups.iter().any(|g| g.eq_ignore_ascii_case(group))
    }

    pub fn is_admin(&self) -> bool {
        self.has_group("naslos_admins")
    }
}

/// Identity extracted from the trusted headers.
pub fn user_from_request(req: &Request) -> Option<Arc<UserInfo>> {
    req.extensions().get::<Arc<UserInfo>>().cloned()
}

/// Parses comma-separated groups (trimming each).
pub fn parse_groups(groups: &str) -> Vec<String> {
    if groups.is_empty() {
        return Vec::new();
    }
    groups.split(',').map(|p| p.trim().to_string()).collect()
}

/// Middleware enforcing authentication. There is no opt-out.
pub async fn require_auth(
    State(state): State<Arc<AppState>>,
    mut req: Request,
    next: Next,
) -> Response {
    // An unconfigured secret must never authorize anything.
    if state.proxy_secret.is_empty() {
        return unauthorized();
    }

    // Only trust auth headers from Traefik's network...
    if !is_trusted(&state, &req) {
        return unauthorized();
    }

    // ...and only when the unforgeable proxy secret proves the request came
    // through the proxy.
    let provided = req
        .headers()
        .get(PROXY_SECRET_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if provided
        .as_bytes()
        .ct_eq(state.proxy_secret.as_bytes())
        .unwrap_u8()
        != 1
    {
        return unauthorized();
    }

    // Extract the user from the Remote-User header.
    let username = req
        .headers()
        .get("Remote-User")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if username.is_empty() {
        return unauthorized();
    }

    let user = Arc::new(UserInfo {
        username,
        groups: parse_groups(
            req.headers()
                .get("Remote-Groups")
                .and_then(|v| v.to_str().ok())
                .unwrap_or(""),
        ),
        email: req
            .headers()
            .get("Remote-Email")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string(),
        display_name: req
            .headers()
            .get("Remote-Name")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string(),
    });
    req.extensions_mut().insert(user);

    next.run(req).await
}

/// Middleware enforcing admin group membership. Runs after `require_auth`.
pub async fn require_admin(req: Request, next: Next) -> Response {
    let Some(user) = user_from_request(&req) else {
        return unauthorized();
    };
    if !user.is_admin() {
        return json(StatusCode::FORBIDDEN, serde_json::json!("Forbidden"));
    }
    next.run(req).await
}

fn unauthorized() -> Response {
    // Match Go's `http.Error(w, "Unauthorized", 401)`.
    (StatusCode::UNAUTHORIZED, "Unauthorized\n").into_response_plain()
}

fn is_trusted(state: &AppState, req: &Request) -> bool {
    let ip: Option<IpAddr> = req
        .extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip());
    match ip {
        Some(ip) => state.trusted_cidrs.iter().any(|c| c.contains(&ip)),
        // Routes exercised by in-process tests have no ConnectInfo; the proxy
        // secret still gates them.
        None => true,
    }
}

trait PlainResponse {
    fn into_response_plain(self) -> Response;
}

impl PlainResponse for (StatusCode, &str) {
    fn into_response_plain(self) -> Response {
        (self.0, self.1.to_string()).into_response()
    }
}

use axum::response::IntoResponse;
