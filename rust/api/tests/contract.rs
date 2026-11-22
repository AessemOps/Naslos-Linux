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

/// A git backend that must never be called by the config-export contract test.
struct NoopGit;

#[async_trait::async_trait]
impl naslos_api::chartsrepo::GitBackend for NoopGit {
    async fn sync(
        &self,
        _url: &str,
        _branch: &str,
        _commit: &str,
        _dir: &str,
        _auth: &naslos_api::chartsrepo::GitAuth,
    ) -> Result<(), String> {
        Err("git must not run in the config-export test".to_string())
    }
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
    // The CSRF gate let it through. The handler itself may still be unable to
    // apply the config in this test process (no agent), but it must not be an
    // auth refusal.
    assert_ne!(status, StatusCode::FORBIDDEN);
    assert_ne!(status, StatusCode::UNAUTHORIZED);
}

/// Every protected route fails closed for: no secret, a wrong secret, and a
/// forwarded identity without the secret. The owner routes must not leak route
/// existence before the gate.
#[tokio::test]
async fn protected_routes_fail_closed_for_all_unauthenticated_shapes() {
    let routes = [
        ("GET", "/api/metrics"),
        ("GET", "/api/disks/health"),
        ("GET", "/api/users"),
        ("POST", "/api/users"),
        ("GET", "/api/shares"),
        ("POST", "/api/shares/apply"),
        ("DELETE", "/api/volumes/zfs/tank?confirm=tank"),
        ("GET", "/api/zvols"),
        ("POST", "/api/apps"),
        ("GET", "/api/virt/capabilities"),
        ("GET", "/api/virt/vms"),
        ("POST", "/api/virt/vms"),
        ("GET", "/api/virt/images?pool=tank"),
        ("POST", "/api/virt/images/import"),
        ("GET", "/api/notifications/webhook"),
        ("PUT", "/api/notifications/webhook"),
        ("DELETE", "/api/notifications/webhook"),
        ("POST", "/api/notifications/webhook/test"),
        ("GET", "/api/notifications/smtp"),
        ("PUT", "/api/notifications/smtp"),
        ("DELETE", "/api/notifications/smtp"),
        ("POST", "/api/notifications/smtp/test"),
        ("GET", "/api/notifications/syslog"),
        ("PUT", "/api/notifications/syslog"),
        ("DELETE", "/api/notifications/syslog"),
        ("POST", "/api/notifications/syslog/test"),
    ];
    for (method, path) in routes {
        for (secret, user) in [
            (None, Some("admin")),                 // forwarded identity, no secret
            (Some("wrong-secret"), Some("admin")), // wrong secret
            (None, None),                          // nothing
        ] {
            let mut b = Request::builder().method(method).uri(path);
            if let Some(secret) = secret {
                b = b.header("X-Naslos-Proxy-Secret", secret);
            }
            if let Some(user) = user {
                b = b.header("Remote-User", user);
                b = b.header("Remote-Groups", "naslos_admins,naslos_users");
            }
            let req = with_peer(b.body(Body::empty()).unwrap(), [127, 0, 0, 1]);
            let (status, _b, _r) = send(req).await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "{method} {path} with secret={secret:?} user={user:?}"
            );
        }
    }
}

fn ws_request(path: &str, secret: Option<&str>, origin: Option<&str>) -> Request<Body> {
    let mut b = Request::builder()
        .method("GET")
        .uri(path)
        .header("Host", "naslos.local")
        .header("Upgrade", "websocket")
        .header("Connection", "Upgrade")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", "AAAAAAAAAAAAAAAAAAAAAA==")
        .header("Remote-User", "admin")
        .header("Remote-Groups", "naslos_admins");
    if let Some(secret) = secret {
        b = b.header("X-Naslos-Proxy-Secret", secret);
    }
    if let Some(origin) = origin {
        b = b.header("Origin", origin);
    }
    with_peer(b.body(Body::empty()).unwrap(), [127, 0, 0, 1])
}

#[tokio::test]
async fn terminal_websocket_requires_the_proxy_secret() {
    let path = "/api/ws/exec?namespace=naslos-privileged&pod=naslos-terminal";
    let (status, _b, _r) = send(ws_request(path, None, Some("https://naslos.local"))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _b, _r) = send(ws_request(
        path,
        Some("wrong"),
        Some("https://naslos.local"),
    ))
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn terminal_websocket_refuses_a_foreign_origin() {
    let path = "/api/ws/exec?namespace=naslos-privileged&pod=naslos-terminal";
    let (status, _b, _r) = send(ws_request(path, Some(SECRET), Some("https://evil.example"))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
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

// ---- Virtual machines (FR-VMT) -------------------------------------------

#[tokio::test]
async fn virt_routes_report_unavailable_without_a_supervisor() {
    // No vmd client in the test state: capabilities and the image library
    // answer 503 rather than pretending the feature works.
    for path in ["/api/virt/capabilities", "/api/virt/images?pool=tank"] {
        let (status, body, _r) = send(get(path, true, Some("admin"), Some("naslos_admins"))).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{path}");
        assert!(body["error"]
            .as_str()
            .unwrap_or("")
            .contains("virtual machines"));
    }
    // The VM list still answers: records are the source of truth and vmd
    // being unreachable is reported per-VM as state "unknown".
    let (status, body, _r) = send(get(
        "/api/virt/vms",
        true,
        Some("admin"),
        Some("naslos_admins"),
    ))
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!([]));
}

#[tokio::test]
async fn vm_routes_answer_404_for_an_unknown_vm() {
    for path in [
        "/api/virt/vms/ghost",
        "/api/virt/vms/ghost/snapshots",
        "/api/ws/vm/ghost/vnc",
    ] {
        let (status, body, _r) = send(get(path, true, Some("admin"), Some("naslos_admins"))).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(body["error"], "VM \"ghost\" does not exist");
    }
}

#[tokio::test]
async fn vm_create_validates_before_touching_anything() {
    let req = Request::builder()
        .method("POST")
        .uri("/api/virt/vms")
        .header("X-Naslos-Proxy-Secret", SECRET)
        .header("Remote-User", "admin")
        .header("Remote-Groups", "naslos_admins")
        .header("Host", "naslos.local")
        .header("Content-Type", "application/json")
        .body(Body::from(
            r#"{"name":"UPPER","cpu":2,"memMiB":2048,"pool":"tank","disk":{"sizeGB":40}}"#,
        ))
        .unwrap();
    let (status, body, _r) = send(with_peer(req, [127, 0, 0, 1])).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"].as_str().unwrap_or("").contains("VM name"),
        "the error should name the invalid field: {}",
        body["error"]
    );
}

#[tokio::test]
async fn vm_create_rejects_secure_boot_without_uefi() {
    let req = Request::builder()
        .method("POST")
        .uri("/api/virt/vms")
        .header("X-Naslos-Proxy-Secret", SECRET)
        .header("Remote-User", "admin")
        .header("Remote-Groups", "naslos_admins")
        .header("Content-Type", "application/json")
        .body(Body::from(
            r#"{"name":"sb-bios","cpu":2,"memMiB":2048,"pool":"tank","firmware":"bios","secureBoot":true,"disk":{"sizeGB":40}}"#,
        ))
        .unwrap();
    let (status, body, _r) = send(with_peer(req, [127, 0, 0, 1])).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]
            .as_str()
            .unwrap_or("")
            .contains("secureBoot requires UEFI"),
        "the error should explain the firmware requirement: {}",
        body["error"]
    );
}

#[tokio::test]
async fn image_upload_accepts_octet_stream_bodies() {
    // The chunked upload is the one non-JSON body the API accepts; the
    // content-type carve-out is scoped to /api/virt/images/upload only.
    let upload = Request::builder()
        .method("POST")
        .uri("/api/virt/images/upload?pool=tank&name=a.iso&offset=0")
        .header("X-Naslos-Proxy-Secret", SECRET)
        .header("Remote-User", "admin")
        .header("Remote-Groups", "naslos_admins")
        .header("Host", "naslos.local")
        .header("Content-Type", "application/octet-stream")
        .body(Body::from(vec![0u8; 4]))
        .unwrap();
    let (status, _b, _r) = send(with_peer(upload, [127, 0, 0, 1])).await;
    // No vmd client: 503 from the handler, NOT 415 from the middleware.
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);

    // The carve-out must not leak onto other POST routes.
    let other = Request::builder()
        .method("POST")
        .uri("/api/shares/apply")
        .header("X-Naslos-Proxy-Secret", SECRET)
        .header("Remote-User", "admin")
        .header("Remote-Groups", "naslos_admins")
        .header("Content-Type", "application/octet-stream")
        .body(Body::from("{}"))
        .unwrap();
    let (status, _b, _r) = send(with_peer(other, [127, 0, 0, 1])).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn console_websocket_refuses_foreign_origins_and_requires_auth() {
    let path = "/api/ws/vm/some-vm/vnc";
    let (status, _b, _r) = send(ws_request(path, None, Some("https://naslos.local"))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _b, _r) = send(ws_request(path, Some(SECRET), Some("https://evil.example"))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
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

fn admin_request(method: &str, path: &str, body: Option<&str>) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header("X-Naslos-Proxy-Secret", SECRET)
        .header("Remote-User", "admin")
        .header("Remote-Groups", "naslos_admins");
    if body.is_some() {
        b = b.header("Content-Type", "application/json");
    }
    with_peer(
        b.body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
            .unwrap(),
        [127, 0, 0, 1],
    )
}

#[tokio::test]
async fn webhook_channel_crud_never_returns_the_token() {
    // One state across the sequence: the channel is persisted in the manager.
    let st = Arc::new(AppState::for_test(SECRET));
    let (status, body, _raw) = send_state(
        st.clone(),
        admin_request("GET", "/api/notifications/webhook", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], false);
    assert_eq!(body["url"], "");
    assert_eq!(body["hasAuthToken"], false);
    assert!(body.get("authToken").is_none());

    // Bad scheme and an SSRF-blocked address are both client errors.
    for url in ["ftp://1.1.1.1/hook", "http://127.0.0.1/hook"] {
        let payload = format!(
            r#"{{"enabled":true,"url":"{url}","enabledEvents":["disk_failure"],"minSeverity":"warning"}}"#
        );
        let (status, _b, _r) = send_state(
            st.clone(),
            admin_request("PUT", "/api/notifications/webhook", Some(&payload)),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{url}");
    }

    let payload = r#"{"enabled":true,"url":"https://1.1.1.1/hook","authToken":"contract-secret","enabledEvents":["disk_failure"],"minSeverity":"error"}"#;
    let (status, _b, _r) = send_state(
        st.clone(),
        admin_request("PUT", "/api/notifications/webhook", Some(payload)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body, raw) = send_state(
        st.clone(),
        admin_request("GET", "/api/notifications/webhook", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], true);
    assert_eq!(body["url"], "https://1.1.1.1/hook");
    assert_eq!(body["hasAuthToken"], true);
    assert_eq!(body["minSeverity"], "error");
    assert_eq!(body["enabledEvents"][0], "disk_failure");
    assert!(body.get("authToken").is_none());
    assert!(!raw.contains("contract-secret"));

    // Omitting authToken keeps the stored one (the UI's default save shape).
    let payload = r#"{"enabled":true,"url":"https://1.1.1.1/hook","enabledEvents":["disk_failure"],"minSeverity":"error"}"#;
    let (status, _b, _r) = send_state(
        st.clone(),
        admin_request("PUT", "/api/notifications/webhook", Some(payload)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body, _raw) = send_state(
        st.clone(),
        admin_request("GET", "/api/notifications/webhook", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["hasAuthToken"], true);

    // DELETE removes the channel and its token.
    let (status, _b, _r) = send_state(
        st.clone(),
        admin_request("DELETE", "/api/notifications/webhook", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body, _raw) =
        send_state(st, admin_request("GET", "/api/notifications/webhook", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], false);
    assert_eq!(body["hasAuthToken"], false);
}

#[tokio::test]
async fn webhook_test_route_requires_enabled_and_guards_at_send_time() {
    // Disabled: an explicit client error rather than a false success.
    let (status, body, _r) = send(admin_request(
        "POST",
        "/api/notifications/webhook/test",
        None,
    ))
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "webhook channel is disabled");

    // Simulate a settings file that bypassed save-time validation: the send
    // path must still refuse the non-public target, and the token must not
    // appear in the error.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("webhook.json");
    std::fs::write(
        &path,
        r#"{"enabled":true,"url":"http://127.0.0.1/hook","authToken":"send-secret","enabledEvents":["disk_failure"],"minSeverity":"warning"}"#,
    )
    .unwrap();
    let mut st = AppState::for_test(SECRET);
    st.webhook = Arc::new(naslos_api::webhook::Manager::new(path.to_str().unwrap()));
    let (status, body, raw) = send_state(
        Arc::new(st),
        admin_request("POST", "/api/notifications/webhook/test", None),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(body["error"].as_str().unwrap().contains("non-public"));
    assert!(!raw.contains("send-secret"));
}

#[tokio::test]
async fn smtp_channel_crud_never_returns_the_password() {
    let st = Arc::new(AppState::for_test(SECRET));
    let (status, body, _raw) = send_state(
        st.clone(),
        admin_request("GET", "/api/notifications/smtp", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], false);
    assert_eq!(body["host"], "");
    assert_eq!(body["port"], 587);
    assert_eq!(body["tls"], "starttls");
    assert_eq!(body["hasPassword"], false);
    assert!(body.get("password").is_none());

    // Loopback, an unknown TLS mode and credentials over plaintext are all
    // client errors and must leave the defaults intact.
    for payload in [
        r#"{"enabled":true,"host":"127.0.0.1","from":"naslos@example.com","to":["ops@example.com"]}"#,
        r#"{"enabled":true,"host":"1.1.1.1","tls":"plain","from":"naslos@example.com","to":["ops@example.com"]}"#,
        r#"{"enabled":true,"host":"1.1.1.1","tls":"none","username":"relay","password":"pass","from":"naslos@example.com","to":["ops@example.com"]}"#,
        r#"{"enabled":true,"host":"1.1.1.1","from":"not-a-mailbox","to":["ops@example.com"]}"#,
    ] {
        let (status, _b, _r) = send_state(
            st.clone(),
            admin_request("PUT", "/api/notifications/smtp", Some(payload)),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{payload}");
    }

    let payload = r#"{"enabled":true,"host":"1.1.1.1","port":587,"tls":"starttls","username":"relay","password":"contract-smtp-secret","from":"naslos@example.com","to":["ops@example.com"],"enabledEvents":["disk_failure"],"minSeverity":"error"}"#;
    let (status, _b, _r) = send_state(
        st.clone(),
        admin_request("PUT", "/api/notifications/smtp", Some(payload)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body, raw) = send_state(
        st.clone(),
        admin_request("GET", "/api/notifications/smtp", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], true);
    assert_eq!(body["host"], "1.1.1.1");
    assert_eq!(body["username"], "relay");
    assert_eq!(body["hasPassword"], true);
    assert_eq!(body["from"], "naslos@example.com");
    assert_eq!(body["to"][0], "ops@example.com");
    assert_eq!(body["minSeverity"], "error");
    assert!(body.get("password").is_none());
    assert!(!raw.contains("contract-smtp-secret"));

    // Omitting the password keeps the stored one.
    let payload = r#"{"enabled":true,"host":"1.1.1.1","username":"relay","from":"naslos@example.com","to":["ops@example.com"],"enabledEvents":["disk_failure"],"minSeverity":"error"}"#;
    let (status, _b, _r) = send_state(
        st.clone(),
        admin_request("PUT", "/api/notifications/smtp", Some(payload)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body, _raw) = send_state(
        st.clone(),
        admin_request("GET", "/api/notifications/smtp", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["hasPassword"], true);

    // An explicit empty string clears it.
    let payload = r#"{"enabled":true,"host":"1.1.1.1","username":"relay","password":"","from":"naslos@example.com","to":["ops@example.com"],"enabledEvents":["disk_failure"],"minSeverity":"error"}"#;
    let (status, _b, _r) = send_state(
        st.clone(),
        admin_request("PUT", "/api/notifications/smtp", Some(payload)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body, _raw) = send_state(
        st.clone(),
        admin_request("GET", "/api/notifications/smtp", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["hasPassword"], false);

    let (status, _b, _r) = send_state(
        st.clone(),
        admin_request("DELETE", "/api/notifications/smtp", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body, _raw) =
        send_state(st, admin_request("GET", "/api/notifications/smtp", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], false);
    assert_eq!(body["hasPassword"], false);
}

#[tokio::test]
async fn smtp_test_route_requires_enabled_and_guards_at_send_time() {
    let (status, body, _r) =
        send(admin_request("POST", "/api/notifications/smtp/test", None)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "smtp channel is disabled");

    // A settings file that bypassed save-time validation must still be
    // refused by the send path, without leaking the password or the host.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("smtp.json");
    std::fs::write(
        &path,
        r#"{"enabled":true,"host":"127.0.0.1","port":587,"tls":"starttls","username":"relay","password":"send-secret","from":"naslos@example.com","to":["ops@example.com"],"enabledEvents":["disk_failure"],"minSeverity":"warning"}"#,
    )
    .unwrap();
    let mut st = AppState::for_test(SECRET);
    st.smtp = Arc::new(naslos_api::smtp::Manager::new(path.to_str().unwrap()));
    let (status, body, raw) = send_state(
        Arc::new(st),
        admin_request("POST", "/api/notifications/smtp/test", None),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(body["error"].as_str().unwrap().contains("non-routable"));
    assert!(!raw.contains("send-secret"));
    assert!(!raw.contains("127.0.0.1"));
}

#[tokio::test]
async fn syslog_channel_crud_and_test_route() {
    let st = Arc::new(AppState::for_test(SECRET));
    let (status, body, _raw) = send_state(
        st.clone(),
        admin_request("GET", "/api/notifications/syslog", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], false);
    assert_eq!(body["protocol"], "udp");
    assert_eq!(body["port"], 514);
    assert_eq!(body["facility"], "local0");

    for payload in [
        r#"{"enabled":true,"protocol":"carrier-pigeon","host":"192.168.1.10"}"#,
        r#"{"enabled":true,"protocol":"udp","host":"192.168.1.10","facility":"bogus"}"#,
        r#"{"enabled":true,"protocol":"udp","host":"127.0.0.1"}"#,
    ] {
        let (status, _b, _r) = send_state(
            st.clone(),
            admin_request("PUT", "/api/notifications/syslog", Some(payload)),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{payload}");
    }

    let payload = r#"{"enabled":true,"protocol":"tcp","host":"192.168.1.10","port":514,"facility":"local3","enabledEvents":["zfs_health"],"minSeverity":"warning"}"#;
    let (status, _b, _r) = send_state(
        st.clone(),
        admin_request("PUT", "/api/notifications/syslog", Some(payload)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body, _raw) = send_state(
        st.clone(),
        admin_request("GET", "/api/notifications/syslog", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], true);
    assert_eq!(body["protocol"], "tcp");
    assert_eq!(body["host"], "192.168.1.10");
    assert_eq!(body["facility"], "local3");
    assert_eq!(body["enabledEvents"][0], "zfs_health");

    let (status, _b, _r) = send_state(
        st.clone(),
        admin_request("DELETE", "/api/notifications/syslog", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body, _raw) = send_state(
        st.clone(),
        admin_request("GET", "/api/notifications/syslog", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], false);

    let (status, body, _r) = send(admin_request(
        "POST",
        "/api/notifications/syslog/test",
        None,
    ))
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "syslog channel is disabled");

    // A bypassed settings file is refused at send time, without the host in
    // the error.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("syslog.json");
    std::fs::write(
        &path,
        r#"{"enabled":true,"protocol":"udp","host":"127.0.0.1","port":514,"facility":"local0","enabledEvents":["disk_failure"],"minSeverity":"warning"}"#,
    )
    .unwrap();
    let mut st = AppState::for_test(SECRET);
    st.syslog = Arc::new(naslos_api::syslog::Manager::new(path.to_str().unwrap()));
    let (status, body, raw) = send_state(
        Arc::new(st),
        admin_request("POST", "/api/notifications/syslog/test", None),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(body["error"].as_str().unwrap().contains("non-routable"));
    assert!(!raw.contains("127.0.0.1"));
}

fn admin_json(method: &str, uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("X-Naslos-Proxy-Secret", SECRET)
        .header("Remote-User", "admin")
        .header("Remote-Groups", "naslos_admins")
        .header("Sec-Fetch-Site", "same-origin")
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn storage_schedules_validate_and_round_trip() {
    let st = state();

    // A schedule needs an explicit kind.
    let (status, body, _r) = send_state(
        st.clone(),
        with_peer(
            admin_json(
                "POST",
                "/api/storage/schedules",
                r#"{"target":"tank/data"}"#,
            ),
            [127, 0, 0, 1],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap_or_default().contains("kind"));

    // A client may not invent a schedule id: ids are server-generated because
    // they become snapshot retention prefixes.
    let invented =
        r#"{"id":"a-b","kind":"snapshot","target":"tank/data","cadence":"daily","runAt":"02:30"}"#;
    let (status, body, _r) = send_state(
        st.clone(),
        with_peer(
            admin_json("POST", "/api/storage/schedules", invented),
            [127, 0, 0, 1],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]
        .as_str()
        .unwrap_or_default()
        .contains("unknown schedule id"));

    // Snapshot targets are dataset paths; a bare pool or a traversal is refused.
    for target in ["data", "../tank/data", "tank/data@snap"] {
        let body_json = format!(
            r#"{{"kind":"snapshot","target":"{target}","cadence":"daily","runAt":"02:30"}}"#
        );
        let (status, body, _r) = send_state(
            st.clone(),
            with_peer(
                admin_json("POST", "/api/storage/schedules", &body_json),
                [127, 0, 0, 1],
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{target}");
        assert!(!body["error"].as_str().unwrap_or_default().is_empty());
    }

    // A scrub schedule is daily/weekly and cannot retain snapshots.
    let hourly = r#"{"kind":"scrub","target":"tank","cadence":"hourly"}"#;
    let (status, body, _r) = send_state(
        st.clone(),
        with_peer(
            admin_json("POST", "/api/storage/schedules", hourly),
            [127, 0, 0, 1],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]
        .as_str()
        .unwrap_or_default()
        .contains("daily or weekly"));

    let keep =
        r#"{"kind":"scrub","target":"tank","cadence":"weekly","runAt":"Sun 02:30","keep":3}"#;
    let (status, _body, _r) = send_state(
        st.clone(),
        with_peer(
            admin_json("POST", "/api/storage/schedules", keep),
            [127, 0, 0, 1],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // A valid schedule is created, listed and deleted.
    let valid = r#"{"kind":"snapshot","target":"tank/data","cadence":"daily","runAt":"02:30","keep":7,"recursive":true}"#;
    let (status, body, _r) = send_state(
        st.clone(),
        with_peer(
            admin_json("POST", "/api/storage/schedules", valid),
            [127, 0, 0, 1],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = body["schedule"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(!id.is_empty());
    assert!(body["schedule"]["nextRun"].is_string());

    let (status, body, _r) = send_state(
        st.clone(),
        with_peer(
            get(
                "/api/storage/schedules",
                true,
                Some("admin"),
                Some("naslos_admins"),
            ),
            [127, 0, 0, 1],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["schedules"].as_array().map(Vec::len), Some(1));

    let (status, _body, _r) = send_state(
        st.clone(),
        with_peer(
            admin_json("DELETE", &format!("/api/storage/schedules?id={id}"), ""),
            [127, 0, 0, 1],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn manual_scrub_validates_and_needs_the_agent() {
    let st = state();
    let (status, _body, _r) = send_state(
        st.clone(),
        with_peer(
            admin_json("POST", "/api/volumes/zfs/bad-name!/scrub", ""),
            [127, 0, 0, 1],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // No agent client in the test process: the route fails closed rather than
    // pretending a scrub started.
    let (status, body, _r) = send_state(
        st,
        with_peer(
            admin_json("POST", "/api/volumes/zfs/tank/scrub", ""),
            [127, 0, 0, 1],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body["error"].as_str().unwrap_or_default().contains("agent"));
}

#[tokio::test]
async fn config_export_requires_an_admin_and_emits_the_versioned_shape() {
    // No proxy secret and no identity: fail closed.
    let (status, _b, _r) = send(get("/api/system/config/export", false, None, None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // A plain authenticated user is not an owner.
    let (status, _b, _r) = send(get(
        "/api/system/config/export",
        true,
        Some("viewer"),
        Some("naslos_users"),
    ))
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let mut st = AppState::for_test(SECRET);
    {
        let mut shares = st.shares.lock().unwrap();
        shares
            .create(&naslos_api::shares::CreateShareRequest {
                name: "media".into(),
                path: "/var/mnt/tank/media".into(),
                protocol: naslos_api::shares::Protocol::Smb,
                description: "Media".into(),
                browseable: Some(true),
                enabled: Some(true),
                ..Default::default()
            })
            .unwrap();
    }
    st.storage_schedules
        .put(naslos_api::server::storage_schedules::StorageSchedule {
            id: "snap-1".into(),
            kind: naslos_api::server::storage_schedules::ScheduleKind::Snapshot,
            target: "tank/media".into(),
            cadence: naslos_api::schedule::Cadence::Daily,
            run_at: "02:30".into(),
            keep: 3,
            last_error: "CANARY-SECRET-MUST-NOT-EXPORT".into(),
            ..Default::default()
        })
        .unwrap();
    st.buddy_schedules
        .put(naslos_api::server::buddy_schedules::ScheduleEntry {
            id: "buddy-1".into(),
            dataset: "tank/media".into(),
            source: "tank/media".into(),
            receiver: "peer-a".into(),
            receiver_results: std::collections::HashMap::from([(
                "peer-a".into(),
                "CANARY-SECRET-MUST-NOT-EXPORT".into(),
            )]),
            cadence: naslos_api::schedule::Cadence::Weekly,
            run_at: "Sun 03:00".into(),
            prune_keep: 5,
            enabled: true,
            ..Default::default()
        })
        .unwrap();
    st.domains
        .upsert(naslos_api::certs::Domain {
            base_domain: "example.test".into(),
            dns_provider: "cloudflare".into(),
            credentials_secret: "CANARY-SECRET-MUST-NOT-EXPORT".into(),
            provider_config: std::collections::HashMap::from([(
                "zone".into(),
                "example.test".into(),
            )]),
            acme_email: "ops@example.test".into(),
            ..Default::default()
        })
        .unwrap();
    st.datasets_registry
        .add(naslos_api::datasets::ManagedDataset {
            name: "media".into(),
            host_path: "/var/mnt/tank/media".into(),
            shared: false,
            warning_acknowledged: false,
            created_at: None,
            attached_to: Vec::new(),
        })
        .unwrap();

    // A catalog app with a secret-marked value, so the route exercises the
    // values redaction (not only the empty-apps case).
    let root = std::env::temp_dir().join(format!(
        "naslos-config-export-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join("apps/demo")).unwrap();
    std::fs::write(
        root.join("apps/demo/naslos-app.yaml"),
        "name: demo\ndescription: Demo\nschema:\n  type: object\n  properties:\n    adminPassword:\n      type: string\n      format: password\n",
    )
    .unwrap();
    let catalog = Arc::new(naslos_api::catalog::Catalog::load(&[
        naslos_api::catalog::SourceRef {
            name: "test".into(),
            display_name: "Test".into(),
            channel: "stable".into(),
            dir: root.to_string_lossy().into_owned(),
            official: true,
        },
    ]));
    st.catalog.store(catalog.clone());

    let apps_path = root.join("apps.json");
    let record = naslos_api::apps::Record {
        name: "demo".into(),
        values: serde_json::json!({
            "adminPassword": "CANARY-SECRET-MUST-NOT-EXPORT",
            "replicas": 2
        })
        .as_object()
        .unwrap()
        .clone(),
        ..Default::default()
    };
    std::fs::write(&apps_path, serde_json::to_vec(&vec![record]).unwrap()).unwrap();
    let catalog_provider = catalog.clone();
    let app_manager = naslos_api::apps::Manager::new(naslos_api::apps::Config {
        store_path: apps_path.to_string_lossy().into_owned(),
        helm: Arc::new(naslos_api::helm::Client::new("naslos-apps")),
        helm_privileged: None,
        privileged_namespace: "naslos-apps-priv".into(),
        charts: Arc::new(naslos_api::chartsrepo::Manager::new(
            Arc::new(naslos_api::chartsrepo::Store::new("")),
            "",
            std::time::Duration::ZERO,
            None,
            Arc::new(NoopGit),
        )),
        catalog: Arc::new(move || Some(catalog_provider.clone())),
        router: None,
        discoverer: None,
        base_domain: String::new(),
        sso_domains: None,
        namespace_pool: Vec::new(),
        privileged_namespace_pool: Vec::new(),
        namespace_pool_limit: None,
        pool_settings_path: String::new(),
        kube: None,
        datasets: None,
        pool_provisioner_image: String::new(),
        app_dns: None,
    })
    .unwrap();
    st.app_manager = Some(Arc::new(app_manager));

    // A DDNS entry whose legacy config still holds an OVH key that the schema
    // now marks secret, plus a safe field that must survive.
    let ddns = naslos_api::ddns::Manager::new(naslos_api::ddns::Options {
        registry: st.providers.clone(),
        store_path: String::new(),
        apps_namespace: "naslos-apps".into(),
        kube: None,
        ip_sources: Vec::new(),
        ipv6_sources: Vec::new(),
        interval: std::time::Duration::ZERO,
        cooldown: std::time::Duration::ZERO,
    });
    ddns.store()
        .put(naslos_api::ddns::Entry {
            id: "ddns-1".into(),
            provider: "ovh".into(),
            zone: "example.test".into(),
            record: "home".into(),
            record_type: "A".into(),
            provider_config: std::collections::HashMap::from([
                ("endpoint".into(), "ovh-eu".into()),
                ("mode".into(), "api".into()),
                (
                    "applicationKey".into(),
                    "CANARY-SECRET-MUST-NOT-EXPORT".into(),
                ),
            ]),
            ..Default::default()
        })
        .unwrap();
    st.ddns = Some(Arc::new(ddns));
    let st = Arc::new(st);

    let req = with_peer(
        get(
            "/api/system/config/export",
            true,
            Some("admin"),
            Some("naslos_admins"),
        ),
        [127, 0, 0, 1],
    );
    let resp = build_router(st).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/json")
    );
    assert_eq!(
        resp.headers()
            .get(axum::http::header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
    let disposition = resp
        .headers()
        .get(axum::http::header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        disposition.starts_with("attachment; filename=\"naslos-config-export-"),
        "{disposition}"
    );

    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["kind"], "naslos-config-export");
    assert_eq!(body["schemaVersion"], 1);
    assert!(body["generatedAt"]
        .as_str()
        .unwrap_or_default()
        .ends_with('Z'));

    assert_eq!(body["shares"][0]["name"], "media");
    assert_eq!(body["shares"][0]["protocol"], "smb");
    assert_eq!(body["storageSchedules"][0]["kind"], "snapshot");
    assert_eq!(body["storageSchedules"][0]["target"], "tank/media");
    assert_eq!(
        body["buddySchedules"][0]["receivers"],
        serde_json::json!(["peer-a"])
    );
    assert_eq!(body["domains"][0]["baseDomain"], "example.test");
    assert!(body["domains"][0].get("credentialsSecret").is_none());
    assert_eq!(
        body["ddnsEntries"][0]["providerConfig"],
        serde_json::json!({"endpoint": "ovh-eu", "mode": "api"})
    );
    assert_eq!(body["apps"][0]["name"], "demo");
    assert_eq!(body["apps"][0]["values"]["replicas"], 2);
    assert!(
        body["apps"][0]["values"].get("adminPassword").is_none(),
        "the catalog-marked secret must be redacted: {}",
        body["apps"][0]
    );
    assert_eq!(
        body["apps"][0]["redactedKeys"],
        serde_json::json!(["adminPassword"])
    );
    assert_eq!(body["datasetRegistry"][0]["name"], "media");

    let raw = String::from_utf8_lossy(&bytes);
    assert!(!raw.contains("CANARY-SECRET-MUST-NOT-EXPORT"), "{raw}");
    assert!(!raw.contains("credentialsSecret"), "{raw}");
    assert!(!raw.contains("receiverResults"), "{raw}");
}
