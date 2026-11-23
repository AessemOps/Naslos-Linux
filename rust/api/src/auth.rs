//! Authentication middleware (port of `api/internal/auth`).
//!
//! It trusts `Remote-User`/`Remote-Groups` headers ONLY from Traefik's pod CIDR,
//! and only when the request also carries the shared secret Traefik's
//! `proxy-identity` middleware injects. The CIDR check alone cannot distinguish
//! Traefik from any other pod in the cluster; the secret can (SEC-1/NAS-001).

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
    if !proxy_checks_pass(&state, &req) {
        return unauthorized();
    }

    // Extract the identity once the proxy checks pass, before the CSRF and
    // content-type refusals: those happen after authentication, so they are
    // recorded with the real actor instead of vanishing unaudited.
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

    // Reject cross-origin state changes (CSRF) and non-JSON request bodies.
    // Browsers always send `Origin`/`Sec-Fetch-Site` on cross-origin requests;
    // non-browser clients send neither, so the API/CLI contract is unchanged.
    if !same_origin_request(&req) {
        let pending = denial_pending(&req, &user);
        record_denial(&state, pending, "csrf").await;
        return forbidden("Cross-origin request refused");
    }
    if !request_body_is_json(&req) {
        let pending = denial_pending(&req, &user);
        record_denial(&state, pending, "content_type").await;
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Unsupported Media Type\n",
        )
            .into_response_plain();
    }

    req.extensions_mut().insert(user);

    next.run(req).await
}

/// Build the denial record from the request. Everything is copied here, on
/// purpose: holding `&Request` across the audit `.await` would make the
/// middleware future non-`Send`.
fn denial_pending(req: &Request, user: &UserInfo) -> crate::audit::Pending {
    let request_id = req
        .extensions()
        .get::<crate::audit::RequestId>()
        .map(|id| id.0.clone())
        .unwrap_or_else(|| {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            format!("auth-{nanos}")
        });
    let path = req.uri().path();
    let template = crate::rbac::route_template(req.method(), path);
    let peer = req
        .extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|info| info.0.ip().to_string());
    let user_agent = req
        .headers()
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.chars()
                .take(160)
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect::<String>()
        });
    crate::audit::Pending::user(
        user,
        req.method(),
        path,
        template,
        peer,
        user_agent,
        request_id,
    )
}

/// Record a refusal that happens after the proxy checks (the actor is known)
/// but before the audit middleware runs, so a denied CSRF/content-type request
/// is still attributable. A failed record degrades the log exactly like a
/// failed middleware write.
async fn record_denial(state: &Arc<AppState>, pending: crate::audit::Pending, error_code: &str) {
    if let Err(e) = state
        .audit
        .record_refusal_async(&pending, "denied", error_code)
        .await
    {
        if state.audit.note_write_failure(&e) {
            crate::audit::spawn_degraded_alert(state.clone(), e);
        }
    }
}

/// The SEC-10 trust checks, shared by `/api` (`require_auth`) and the `/mcp`
/// bearer guard: the proxy secret must be configured, the peer address must
/// be inside the trusted Traefik CIDR, and the request must carry the exact
/// secret. `/mcp` bypasses Authelia but never these checks.
pub(crate) fn proxy_checks_pass(state: &AppState, req: &Request) -> bool {
    // An unconfigured secret must never authorize anything.
    if state.proxy_secret.is_empty() {
        return false;
    }

    // Only trust auth headers from Traefik's network...
    if !is_trusted(state, req) {
        return false;
    }

    // ...and only when the unforgeable proxy secret proves the request came
    // through the proxy.
    let provided = req
        .headers()
        .get(PROXY_SECRET_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    provided
        .as_bytes()
        .ct_eq(state.proxy_secret.as_bytes())
        .unwrap_u8()
        == 1
}

pub(crate) fn unauthorized() -> Response {
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
    // The chunked image upload (FR-VMT) streams raw `application/octet-stream`
    // bodies. This is the only route exempt from the JSON requirement: the
    // same-origin (CSRF) checks above still apply, and octet-stream is not a
    // CORS-safelisted content type either, so a cross-origin POST still
    // forces a preflight.
    if req.uri().path().starts_with("/api/virt/images/upload") {
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
    // `Host` only. `X-Forwarded-Host` is client-influenced unless every hop
    // strips it, and a value that disagrees with Host would let a same-site
    // check pass for an attacker-chosen name. Traefik preserves Host.
    header_str(req, HOST.as_str()).map(|v| v.trim().to_string())
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::routing::post;
    use axum::Router;
    use std::net::SocketAddr;
    use tower::ServiceExt;

    fn request(site: Option<&str>, content_type: Option<&str>) -> Request {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/api/shares")
            .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 5555u16))))
            .header(PROXY_SECRET_HEADER, "test-secret")
            .header("Remote-User", "boss")
            .header("Remote-Groups", "naslos_admins");
        if let Some(site) = site {
            builder = builder.header("sec-fetch-site", site);
        }
        if let Some(content_type) = content_type {
            builder = builder.header(CONTENT_TYPE, content_type);
        }
        builder.body(Body::empty()).unwrap()
    }

    fn app(state: Arc<AppState>) -> Router {
        Router::new()
            .route("/api/shares", post(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(state, require_auth))
    }

    fn denials(state: &Arc<AppState>) -> Vec<serde_json::Value> {
        state
            .audit
            .query(&crate::audit::AuditQuery::default())
            .expect("audit query")
            .records
            .into_iter()
            .filter(|record| record["outcome"]["state"] == "denied")
            .collect()
    }

    #[tokio::test]
    async fn a_csrf_denial_is_audited_with_the_authenticated_actor() {
        let state = Arc::new(AppState::for_test("test-secret"));
        let response = app(state.clone())
            .oneshot(request(Some("cross-site"), Some("application/json")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let denials = denials(&state);
        assert_eq!(denials.len(), 1, "{denials:?}");
        assert_eq!(denials[0]["actor"]["name"], "boss");
        assert_eq!(denials[0]["outcome"]["errorCode"], "csrf");
        assert_eq!(denials[0]["source"]["method"], "POST");
        assert_eq!(denials[0]["source"]["route"], "/api/shares");
    }

    #[tokio::test]
    async fn a_content_type_denial_is_audited_with_the_authenticated_actor() {
        let state = Arc::new(AppState::for_test("test-secret"));
        let response = app(state.clone())
            .oneshot(request(None, Some("text/plain")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

        let denials = denials(&state);
        assert_eq!(denials.len(), 1, "{denials:?}");
        assert_eq!(denials[0]["actor"]["name"], "boss");
        assert_eq!(denials[0]["outcome"]["errorCode"], "content_type");
    }

    #[tokio::test]
    async fn an_untrusted_peer_is_refused_without_an_audit_record() {
        let state = Arc::new(AppState::for_test("test-secret"));
        let mut req = request(Some("same-origin"), Some("application/json"));
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([10, 9, 9, 9], 5555u16))));
        let response = app(state.clone()).oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(
            denials(&state).is_empty(),
            "an unauthenticated peer must not invent an actor"
        );
    }
}
