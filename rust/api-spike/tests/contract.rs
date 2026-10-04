//! Contract tests for the spike endpoints: status codes, auth, and JSON shape
//! (the same checks the Go `routes_test.go` / dashboard contract rely on).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use naslos_api_spike::build_router;
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;

const SECRET: &str = "test-proxy-secret";

fn test_state() -> Arc<naslos_api_spike::AppState> {
    Arc::new(naslos_api_spike::AppState::for_test(SECRET))
}

async fn send(req: Request<Body>) -> (StatusCode, Value, String) {
    let resp = build_router(test_state()).oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let raw = String::from_utf8_lossy(&bytes).to_string();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value, raw)
}

fn owner_get(path: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(path)
        .header("X-Naslos-Proxy-Secret", SECRET)
        .header("Remote-User", "someone")
        .header("Remote-Groups", "naslos_users")
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn health_and_ready_are_public() {
    for path in ["/api/health", "/api/ready"] {
        let (status, body, raw) = send(
            Request::builder()
                .method("GET")
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(body["status"], "ok");
        assert!(raw.ends_with('\n'), "Go Encode appends a newline");
    }
    let (_s, body, _r) = send(
        Request::builder()
            .method("GET")
            .uri("/api/ready")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(body["ldap"], "down");
}

#[tokio::test]
async fn owner_routes_require_secret_and_identity() {
    // No proxy secret.
    let resp = build_router(test_state())
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/metrics")
                .header("Remote-User", "someone")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // Secret but no identity.
    let resp = build_router(test_state())
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/metrics")
                .header("X-Naslos-Proxy-Secret", SECRET)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // A plain user (not admin) is authorized on dashboard/metrics.
    let (status, _b, _r) = send(owner_get("/api/metrics")).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn dashboard_shape_matches_go() {
    let (status, body, _r) = send(owner_get("/api/dashboard")).await;
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
    assert!(body["cpu"]["usage"].is_number());
    assert!(body["zfs"]["poolCount"].is_number());
    // Go serializes an empty interface/pool list as null in the projection.
    assert!(body["network"]["interfaces"].is_null());
    assert!(body["zfs"]["pools"].is_null());
    // updatedAt is RFC3339, not a numeric epoch.
    assert!(body["updatedAt"].as_str().is_some());
}

#[tokio::test]
async fn metrics_shape_matches_go() {
    let (status, body, _r) = send(owner_get("/api/metrics")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["cpu"]["usagePercent"], 0.0);
    assert!(body["cpu"]["loadAvg1"].is_number());
    assert!(body["system"]["talosVersion"].is_string());
    assert!(body["updatedAt"].as_str().is_some());
}

#[tokio::test]
async fn unknown_api_route_is_authenticated_then_not_found() {
    // The owner router is behind auth, so an unknown /api path 401s without the
    // secret (default-deny), never leaking route existence.
    let resp = build_router(test_state())
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/not-a-route")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
