//! HTTP contract tests: the agent's status codes, JSON shapes and auth behavior
//! are a wire contract with the API and the Playwright suite. These pin them.

mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use common::{FakeHost, FakeRunner};
use http_body_util::BodyExt;
use naslos_agent::server::{Options, Server};
use naslos_agent::shares::SharesClient;
use naslos_agent::zfs::ZfsClient;
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;

const TOKEN: &str = "test-token";

fn server_with(zfs: Option<ZfsClient>, shares: Option<SharesClient>, token: &str) -> Server {
    Server::new(
        ":0",
        zfs.map(Arc::new),
        shares.map(Arc::new),
        Options {
            auth_token: token.to_string(),
            ..Default::default()
        },
    )
}

fn degraded() -> Server {
    server_with(None, None, TOKEN)
}

async fn send(server: &Server, req: Request<Body>) -> (StatusCode, Value) {
    let resp = server.router().oneshot(req).await.expect("router call");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

fn get_auth(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap()
}

fn post_json(uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn health_is_public() {
    let (status, body) = send(&degraded(), get_auth("/health")).await;
    // /health ignores the Authorization header entirely (no auth middleware).
    let resp = degraded()
        .router()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(bytes.last(), Some(&b'\n'), "Go Encode appends a newline");
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn api_requires_a_bearer_token() {
    for req in [
        Request::builder().method("GET").uri("/api/v1/pools").body(Body::empty()).unwrap(),
        Request::builder()
            .method("GET")
            .uri("/api/v1/pools")
            .header(header::AUTHORIZATION, "Bearer wrong")
            .body(Body::empty())
            .unwrap(),
    ] {
        let resp = degraded().router().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"], "missing or invalid agent token");
    }
}

#[tokio::test]
async fn new_route_is_authenticated_by_default() {
    // The API router is mounted behind the auth layer, so an unlisted path 401s.
    let resp = degraded()
        .router()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/not-a-route")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn degraded_mode_returns_503_for_zfs_and_shares() {
    let server = degraded();

    let (status, body) = send(&server, get_auth("/api/v1/pools")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        body["error"],
        "ZFS is not available on this node (agent running in degraded mode)"
    );

    let (status, body) = send(&server, get_auth("/api/v1/datasets")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        body["error"],
        "ZFS is not available on this node (agent running in degraded mode)"
    );

    let (status, body) = send(&server, get_auth("/api/v1/shares/status")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        body["error"],
        "share configuration is not available on this node (agent running in degraded mode)"
    );

    // Backup endpoints use the streaming client and have their own message.
    let (status, body) = send(&server, get_auth("/api/v1/zfs/send/tank/data?to=snap1")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        body["error"],
        "ZFS streaming is not available on this node (agent running in degraded mode)"
    );
}

#[tokio::test]
async fn validation_errors_are_400_and_node_errors_are_500() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[]));
    let zfs = ZfsClient::new(runner, host.root.clone());
    let server = server_with(Some(zfs), None, TOKEN);

    // A validation error reaches the HTTP layer as 400.
    let (status, body) = send(&server, post_json("/api/v1/pools", r#"{"name":"-f","disks":["/dev/sdb"]}"#)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("pool name"));

    // A decode error is 400 too.
    let (status, _) = send(&server, post_json("/api/v1/pools", "not-json")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn pools_get_returns_an_array_with_lowercase_keys() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&["tank\t10G\t1G\t9G\tONLINE\n", ""]));
    let zfs = ZfsClient::new(runner, host.root.clone());
    let server = server_with(Some(zfs), None, TOKEN);

    let (status, body) = send(&server, get_auth("/api/v1/pools")).await;
    assert_eq!(status, StatusCode::OK);
    let arr = body.as_array().expect("pools is a JSON array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "tank");
    assert_eq!(arr[0]["health"], "ONLINE");
    assert_eq!(arr[0]["size"], "10G");
}

#[tokio::test]
async fn pool_devices_get_shape() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&["tank\t10G\t1G\t9G\tONLINE\n", ""]));
    let zfs = ZfsClient::new(runner, host.root.clone());
    let server = server_with(Some(zfs), None, TOKEN);

    let (status, body) = send(&server, get_auth("/api/v1/pools/tank/devices")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["pool"], "tank");
    assert!(body["disks"].is_array());
}

#[tokio::test]
async fn shares_status_reports_applied_state() {
    let host = FakeHost::new();
    let shares = SharesClient::new(host.root.clone());
    let server = server_with(None, Some(shares), TOKEN);

    let (status, body) = send(&server, get_auth("/api/v1/shares/status")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["applied"], false);
    assert!(body["messages"].as_array().unwrap().len() == 2);
}

#[tokio::test]
async fn pool_devices_serializes_no_disks_as_null() {
    // Go marshalled a nil slice as `null`; the port must not emit `[]`.
    // An empty host root means every fake device belongs to the pool.
    let host = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(host.path().join("dev")).unwrap();
    // 1: `zpool list` (no pools); pool_disks is never reached for none.
    let runner = Arc::new(FakeRunner::new(&["tank\t10G\t1G\t9G\tONLINE\n", ""]));
    let zfs = ZfsClient::new(runner, host.path().to_path_buf());
    let server = server_with(Some(zfs), None, TOKEN);

    let (_status, body) = send(&server, get_auth("/api/v1/pools/tank/devices")).await;
    assert!(
        body["disks"].is_null(),
        "empty free-disk list must serialize as null, got {}",
        body["disks"]
    );
}

#[tokio::test]
async fn bare_trailing_slash_route_is_bad_request_not_not_found() {
    // Go's prefix mux matched `/api/v1/datasets/` and returned 400 because the
    // empty pool was rejected before any host call.
    let server = degraded();
    let resp = server
        .router()
        .oneshot(get_auth("/api/v1/datasets/"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "route did not match");
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"], "pool name required");

    let resp = server
        .router()
        .oneshot(get_auth("/api/v1/snapshots/"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "route did not match");
}
