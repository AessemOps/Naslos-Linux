//! Authentication middleware (port of `api/internal/auth`).
//!
//! It trusts `Remote-User`/`Remote-Groups` headers ONLY from Traefik's pod CIDR,
//! and only when the request also carries the shared secret Traefik's
//! `proxy-identity` middleware injects. The CIDR check alone cannot distinguish
//! Traefik from any other pod in the cluster; the secret can (SEC-1/NAS-001).

use crate::json;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE, HOST, TRANSFER_ENCODING};
use axum::http::{Method, StatusCode, Uri};
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

    // Reject cross-origin state changes (CSRF) and non-JSON request bodies.
    // Browsers always send `Origin`/`Sec-Fetch-Site` on cross-origin requests;
    // non-browser clients send neither, so the API/CLI contract is unchanged.
    if !same_origin_request(&req) {
        return forbidden("Cross-origin request refused");
    }
    if !request_body_is_json(&req) {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Unsupported Media Type\n",
        )
            .into_response_plain();
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

fn forbidden(msg: &str) -> Response {
    (StatusCode::FORBIDDEN, msg).into_response_plain()
}

fn is_trusted(state: &AppState, req: &Request) -> bool {
    let ip: Option<IpAddr> = req
        .extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip());
    match ip {
        Some(ip) => state.trusted_cidrs.iter().any(|c| c.contains(&ip)),
        // Fail closed: without a peer address the CIDR is unknowable. Every
        // production listener uses `into_make_service_with_connect_info`, so
        // this only affects a mis-wired server (which must be fixed, not
        // trusted) and tests, which attach a fake peer address.
        None => false,
    }
}

fn is_safe_method(method: &Method) -> bool {
    matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

/// CSRF defence for unsafe methods.
///
/// A cross-site page cannot set an arbitrary `Content-Type` without a preflight
/// (blocked by the absence of CORS) and browsers label the request with
/// `Sec-Fetch-Site`/`Origin`. Requests that carry neither header come from a
/// non-browser client (API, CLI, tests) and are not subject to CSRF.
fn same_origin_request(req: &Request) -> bool {
    if is_safe_method(req.method()) {
        return true;
    }
    if let Some(site) = header_str(req, "sec-fetch-site") {
        return site.eq_ignore_ascii_case("same-origin");
    }
    let Some(origin) = header_str(req, "origin") else {
        return true;
    };
    let Some(origin_authority) = origin_authority(origin) else {
        // `Origin: null` (sandboxed iframe, file://) never matches.
        return false;
    };
    match request_authority(req) {
        Some(authority) => authority.eq_ignore_ascii_case(&origin_authority),
        None => false,
    }
}

/// A body-bearing unsafe request must declare `application/json`. This forces a
/// CORS preflight for any cross-origin JSON request and keeps the API's
/// existing contract (every UI write already sends this header).
fn request_body_is_json(req: &Request) -> bool {
    if is_safe_method(req.method()) {
        return true;
    }
    let declared = req
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let has_body = declared > 0
        || req.headers().contains_key(TRANSFER_ENCODING)
        || req.headers().contains_key(CONTENT_TYPE);
    if !has_body {
        return true;
    }
    req.headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("application/json")
        })
        .unwrap_or(false)
}

fn header_str<'a>(req: &'a Request, name: &str) -> Option<&'a str> {
    req.headers().get(name).and_then(|v| v.to_str().ok())
}

fn request_authority(req: &Request) -> Option<String> {
    // Traefik forwards the client's Host; `X-Forwarded-Host` is the fallback for
    // deployments that rewrite it.
    header_str(req, "x-forwarded-host")
        .and_then(|v| v.split(',').next())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .or_else(|| header_str(req, HOST.as_str()).map(|v| v.trim().to_string()))
}

fn origin_authority(origin: &str) -> Option<String> {
    let uri: Uri = origin.parse().ok()?;
    match uri.scheme_str()? {
        "http" | "https" => {}
        _ => return None,
    }
    uri.authority().map(|a| a.to_string())
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
