//! Contract tests: the route surface, auth gates and JSON shapes must match the
//! Go server (api/internal/server/routes_test.go and the metrics contract).

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use naslos_api::server::build_router;
use naslos_api::state::AppState;
use serde_json::Value;
use std::net::SocketAddr;
use std::sync::Arc;
use tower::ServiceExt;

const SECRET: &str = "test-proxy-secret";

fn state() -> Arc<AppState> {
    Arc::new(AppState::for_test(SECRET))
}

async fn send(req: Request<Body>) -> (StatusCode, Value, String) {
    send_state(state(), req).await
}

async fn send_state(state: Arc<AppState>, req: Request<Body>) -> (StatusCode, Value, String) {
    let resp = build_router(state).oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let raw = String::from_utf8_lossy(&bytes).to_string();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value, raw)
}

fn get(path: &str, secret: bool, user: Option<&str>, groups: Option<&str>) -> Request<Body> {
    let mut b = Request::builder().method("GET").uri(path);
    if secret {
        b = b.header("X-Naslos-Proxy-Secret", SECRET);
    }
    if let Some(u) = user {
        b = b.header("Remote-User", u);
    }
    if let Some(g) = groups {
        b = b.header("Remote-Groups", g);
    }
    let mut req = b.body(Body::empty()).unwrap();
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40000))));
    req
}

#[tokio::test]
async fn health_and_ready_are_public() {
    for path in ["/api/health", "/api/ready"] {
        let (status, body, raw) = send(get(path, false, None, None)).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(body["status"], "ok");
        assert!(raw.ends_with('\n'), "Go Encode appends a newline");
    }
}

#[tokio::test]
async fn owner_routes_fail_closed_without_secret_or_identity() {
    // No secret at all.
    let (status, _b, _r) = send(get("/api/metrics", false, Some("someone"), None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // Secret but no identity.
    let (status, _b, _r) = send(get("/api/metrics", true, None, None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn dashboard_and_metrics_are_any_authenticated_user() {
    for path in ["/api/dashboard", "/api/metrics"] {
        let (status, _b, _r) = send(get(path, true, Some("someone"), Some("naslos_users"))).await;
        assert_eq!(status, StatusCode::OK, "{path} as a plain user");
    }
}

#[tokio::test]
async fn admin_routes_reject_a_plain_user_and_reach_the_handler_for_an_admin() {
    // A plain authenticated user is forbidden on an admin route.
    let (status, _b, _r) = send(get(
        "/api/pods",
        true,
        Some("someone"),
        Some("naslos_users"),
    ))
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // An admin passes the gate and reaches the real handler. kubectl is not
    // configured in this test, so the handler reports unavailability; the
    // point is that it is no longer a 501 stub.
    let (status, body, _r) = send(get(
        "/api/pods",
        true,
        Some("admin"),
        Some("naslos_admins,naslos_users"),
    ))
    .await;
    assert_ne!(status, StatusCode::FORBIDDEN);
    assert_ne!(status, StatusCode::NOT_IMPLEMENTED);
    assert_ne!(body["error"], "not implemented in the Rust port yet");
}

#[tokio::test]
async fn unknown_api_route_is_authenticated_then_not_found() {
    // Guarded by the owner mux, so it 401s without the secret (default-deny)
    // rather than leaking route existence.
    let (status, _b, _r) = send(get("/api/not-a-route", false, None, None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn auth_me_returns_identity_when_ldap_is_configured() {
    // A plaintext (non-dialing) identity client is enough to make the handler
    // report the identity from the trusted headers.
    let client = naslos_api::identity::Client::new(&naslos_api::identity::Config {
        host: "localhost".into(),
        port: 389,
        base_dn: "dc=naslos,dc=local".into(),
        bind_dn: "cn=admin".into(),
        bind_pass: "x".into(),
        ca_cert_path: String::new(),
        use_tls: false,
    })
    .unwrap();
    let mut st = AppState::for_test(SECRET);
    st.identity = Some(Arc::new(client));
    let st = Arc::new(st);

    let (status, body, _r) = send_state(
        st,
        get(
            "/api/auth/me",
            true,
            Some("alice"),
            Some("naslos_admins, naslos_users"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["username"], "alice");
    // Go joins groups into a comma-separated string.
    assert_eq!(body["groups"], "naslos_admins,naslos_users");
}

#[tokio::test]
async fn auth_me_is_503_without_ldap() {
    let (status, body, _r) = send(get(
        "/api/auth/me",
        true,
        Some("alice"),
        Some("naslos_admins"),
    ))
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body["error"].as_str().unwrap().contains("Identity/LDAP"));
}

#[tokio::test]
async fn dashboard_shape_matches_go() {
    let (status, body, _r) = send(get("/api/dashboard", true, Some("someone"), None)).await;
    assert_eq!(status, StatusCode::OK);
    for key in [
        "cpu",
        "memory",
        "disk",
        "zfs",
        "system",
        "network",
        "updatedAt",
    ] {
        assert!(body.get(key).is_some(), "dashboard missing {key}");
    }
    assert!(body["network"]["interfaces"].is_null());
    assert!(body["zfs"]["pools"].is_null());
    // Un-collected metrics: Go's zero time.Time, not null.
    assert_eq!(body["updatedAt"], "0001-01-01T00:00:00Z");
}

#[tokio::test]
async fn metrics_shape_matches_go() {
    let (status, body, _r) = send(get("/api/metrics", true, Some("someone"), None)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["cpu"]["usagePercent"].is_number());
    assert!(body["system"]["talosVersion"].is_string());
    assert_eq!(body["updatedAt"], "0001-01-01T00:00:00Z");
}

fn with_peer(req: Request<Body>, peer: [u8; 4]) -> Request<Body> {
    let mut req = req;
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from((peer, 40000))));
    req
}

#[tokio::test]
async fn auth_fails_closed_without_a_peer_address() {
    // Same trusted secret and identity, but no ConnectInfo: the CIDR cannot be
    // evaluated, so the request must not be trusted.
    let req = Request::builder()
        .method("GET")
        .uri("/api/metrics")
        .header("X-Naslos-Proxy-Secret", SECRET)
        .header("Remote-User", "admin")
        .header("Remote-Groups", "naslos_admins")
        .body(Body::empty())
        .unwrap();
    let (status, body, _r) = send(req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body, Value::Null);
}

#[tokio::test]
async fn auth_rejects_an_untrusted_peer_even_with_the_secret() {
    let req = with_peer(
        get("/api/metrics", true, Some("admin"), None),
        [203, 0, 113, 5],
    );
    let (status, _b, _r) = send(req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn cross_site_state_change_is_refused() {
    let req = Request::builder()
        .method("POST")
        .uri("/api/shares/apply")
        .header("X-Naslos-Proxy-Secret", SECRET)
        .header("Remote-User", "admin")
        .header("Remote-Groups", "naslos_admins")
        .header("Sec-Fetch-Site", "cross-site")
        .body(Body::empty())
        .unwrap();
    let (status, _b, _r) = send(with_peer(req, [127, 0, 0, 1])).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn same_origin_state_change_is_allowed() {
    let req = Request::builder()
        .method("POST")
        .uri("/api/shares/apply")
        .header("X-Naslos-Proxy-Secret", SECRET)
        .header("Remote-User", "admin")
        .header("Remote-Groups", "naslos_admins")
        .header("Sec-Fetch-Site", "same-origin")
        .body(Body::empty())
        .unwrap();
    let (status, _b, _r) = send(with_peer(req, [127, 0, 0, 1])).await;
    assert_ne!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn foreign_origin_on_a_write_is_refused() {
    let req = Request::builder()
        .method("POST")
        .uri("/api/shares/apply")
        .header("X-Naslos-Proxy-Secret", SECRET)
        .header("Remote-User", "admin")
        .header("Remote-Groups", "naslos_admins")
        .header("Host", "naslos.local")
        .header("Origin", "https://evil.naslos.local")
        .body(Body::empty())
        .unwrap();
    let (status, _b, _r) = send(with_peer(req, [127, 0, 0, 1])).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn text_plain_json_body_is_refused() {
    let req = Request::builder()
        .method("POST")
        .uri("/api/shares/apply")
        .header("X-Naslos-Proxy-Secret", SECRET)
        .header("Remote-User", "admin")
        .header("Remote-Groups", "naslos_admins")
        .header("Content-Type", "text/plain")
        .body(Body::from("{\"apply\":true}"))
        .unwrap();
    let (status, _b, _r) = send(with_peer(req, [127, 0, 0, 1])).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn unknown_routes_return_json_404() {
    let (status, body, _r) = send(with_peer(
        get("/does-not-exist", false, None, None),
        [127, 0, 0, 1],
    ))
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "not found");
}

#[tokio::test]
async fn a_wrong_method_returns_json_405() {
    let req = Request::builder()
        .method("POST")
        .uri("/api/health")
        .body(Body::empty())
        .unwrap();
    let (status, body, _r) = send(with_peer(req, [127, 0, 0, 1])).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(body["error"], "method not allowed");
}

#[tokio::test]
async fn readiness_reports_the_identity_state() {
    // for_test has no identity backend.
    let (status, body, _r) = send(with_peer(
        get("/api/ready", false, None, None),
        [127, 0, 0, 1],
    ))
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ldap"], "down");
}

#[tokio::test]
async fn the_dataset_registry_requires_the_agent_for_registration() {
    let req = Request::builder()
        .method("POST")
        .uri("/api/datasets/registry")
        .header("X-Naslos-Proxy-Secret", SECRET)
        .header("Remote-User", "admin")
        .header("Remote-Groups", "naslos_admins")
        .header("Content-Type", "application/json")
        .body(Body::from(
            r#"{"name":"media","hostPath":"/var/mnt/tank/media","shared":false}"#,
        ))
        .unwrap();
    let (status, body, _r) = send(with_peer(req, [127, 0, 0, 1])).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body["error"].as_str().unwrap_or_default().contains("agent"));
}
