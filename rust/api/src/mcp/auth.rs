//! The `/mcp` guard (FR-MCP, SEC-13): the Traefik trust checks, the bearer
//! token, the Origin check and the per-IP failure backoff. It runs on the
//! public router; everything it passes reaches the rmcp service in
//! [`super::service`].

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::auth::proxy_checks_pass;
use crate::state::AppState;

use std::sync::Arc;

/// Middleware guarding `/mcp`. Order matters:
///
/// 1. SEC-10 first: the proxy secret and trusted CIDR have no opt-out, so
///    `/mcp` is reachable only through Traefik even though it skips
///    Authelia.
/// 2. A disabled endpoint answers 404, so an off-by-default install does not
///    advertise the surface.
/// 3. A present `Origin` must be the apex (or the request's own host): MCP
///    DNS-rebinding guidance.
/// 4. Backoff, then the bearer token itself.
///
/// Every rejection is fallible-by-return; nothing here may panic because the
/// release profile is `panic = "abort"` (see the Buddy note in
/// `server/mod.rs`, AUD-2611-T6-1).
pub async fn guard(State(state): State<Arc<AppState>>, mut req: Request, next: Next) -> Response {
    if !proxy_checks_pass(&state, &req) {
        return unauthorized();
    }

    if !state.mcp.store.enabled() {
        return not_found();
    }

    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.ip());
    // Fail closed like `require_auth`: production always attaches the peer
    // address, so a request without one means a mis-wired server.
    let Some(ip) = peer else {
        return unauthorized();
    };

    if !origin_is_apex(&state, req.headers()) {
        return forbidden("Origin header is not allowed");
    }

    // Verify first: a valid token is never blocked by another client's failed
    // attempts (R-024). Only an invalid token consults the backoff table, and
    // its failures are counted against both the socket peer and the presented
    // token digest so one noisy client cannot lock out every token behind the
    // same proxy address.
    let presented = bearer_token(req.headers()).unwrap_or_default();
    match state.mcp.store.verify(&presented) {
        Some(auth) => {
            state
                .mcp
                .failures
                .record_success(&crate::mcp::FailureTrack::ip_key(ip));
            state
                .mcp
                .failures
                .record_success(&crate::mcp::FailureTrack::token_key(&presented));
            // Debounced flush inside the store.
            state.mcp.store.record_last_used(&auth.id);
            req.extensions_mut().insert(Arc::new(auth));
            next.run(req).await
        }
        None => {
            let ip_key = crate::mcp::FailureTrack::ip_key(ip);
            let token_key = crate::mcp::FailureTrack::token_key(&presented);
            if state.mcp.failures.is_blocked(&ip_key) || state.mcp.failures.is_blocked(&token_key) {
                return too_many_requests();
            }
            state.mcp.failures.record_failure(&ip_key);
            state.mcp.failures.record_failure(&token_key);
            unauthorized()
        }
    }
}

/// `Authorization: Bearer nmcp_…` (ASCII, case-sensitive scheme per RFC 6750).
fn bearer_token(headers: &axum::http::HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let rest = value.strip_prefix("Bearer ")?;
    Some(rest.trim().to_string())
}

/// A present `Origin` must be the configured apex (or the request's own
/// host, which is the apex behind Traefik). Non-browser MCP clients send no
/// `Origin` and always pass — same rule as the terminal WebSockets.
fn origin_is_apex(_state: &AppState, headers: &axum::http::HeaderMap) -> bool {
    crate::server::ws::origin_allowed(headers, &[])
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(
            header::WWW_AUTHENTICATE,
            "Bearer realm=\"naslos-mcp\", error=\"invalid_token\"",
        )],
        "Unauthorized\n",
    )
        .into_response()
}

fn forbidden(msg: &str) -> Response {
    (StatusCode::FORBIDDEN, msg.to_string()).into_response()
}

fn too_many_requests() -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, "300")],
        "Too many failed attempts; retry later\n",
    )
        .into_response()
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "Not Found\n").into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::store::{Scope, TokenAuth};
    use axum::body::Body;
    use axum::middleware;
    use axum::routing::any;
    use axum::Router;
    use tower::ServiceExt;

    fn app() -> (Router, Arc<AppState>, String) {
        let state = AppState::for_test("test-secret");
        state.mcp.store.set_enabled(true).unwrap();
        let (_, token) = state
            .mcp
            .store
            .create_token("probe", Scope::Read, None, "admin")
            .unwrap();
        let state = Arc::new(state);
        let router = Router::new()
            .route("/mcp", any(|| async { "ok" }))
            .layer(middleware::from_fn_with_state(state.clone(), guard))
            .with_state(state.clone());
        (router, state, token)
    }

    fn request(secret: Option<&str>, bearer: Option<&str>) -> Request {
        let mut builder = Request::builder()
            .uri("/mcp")
            .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 44444u16))));
        if let Some(secret) = secret {
            builder = builder.header(crate::auth::PROXY_SECRET_HEADER, secret);
        }
        if let Some(bearer) = bearer {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
        }
        builder.body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn the_full_chain_rejects_every_shortcut() {
        let (app, _, token) = app();

        // No proxy secret: 401 even with a valid token (SEC-10).
        let resp = app
            .clone()
            .oneshot(request(None, Some(&token)))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // Secret but no token: 401 with a Bearer challenge.
        let resp = app
            .clone()
            .oneshot(request(Some("test-secret"), None))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert!(resp.headers().contains_key(header::WWW_AUTHENTICATE));

        // Bad token: 401.
        let resp = app
            .clone()
            .oneshot(request(Some("test-secret"), Some("nmcp_wrong")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // Everything present: passes.
        let resp = app
            .clone()
            .oneshot(request(Some("test-secret"), Some(&token)))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn disabled_answers_404() {
        let (app, state, token) = app();
        state.mcp.store.set_enabled(false).unwrap();
        let resp = app
            .oneshot(request(Some("test-secret"), Some(&token)))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn failures_block_the_bad_token_but_never_a_valid_one() {
        let (app, _, token) = app();
        for _ in 0..10 {
            let resp = app
                .clone()
                .oneshot(request(Some("test-secret"), Some("nmcp_no")))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        }
        // The bad token is throttled.
        let resp = app
            .clone()
            .oneshot(request(Some("test-secret"), Some("nmcp_no")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(resp.headers().contains_key(header::RETRY_AFTER));
        // A valid token from the same peer still works (R-024).
        let resp = app
            .clone()
            .oneshot(request(Some("test-secret"), Some(&token)))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn a_foreign_origin_is_refused() {
        let (app, _, token) = app();
        let request = Request::builder()
            .uri("/mcp")
            .header("origin", "https://evil.example")
            .header(crate::auth::PROXY_SECRET_HEADER, "test-secret")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 44444u16))))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(request).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn a_forged_identity_cannot_skip_the_guard() {
        let (app, _, _) = app();
        // Even a request carrying a TokenAuth extension server-side style
        // still needs the real bearer: the guard never reads extensions.
        let mut request = request(Some("test-secret"), None);
        request.extensions_mut().insert(Arc::new(TokenAuth {
            id: "x".into(),
            name: "forged".into(),
            scope: Scope::Full,
        }));
        let resp = app.oneshot(request).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
