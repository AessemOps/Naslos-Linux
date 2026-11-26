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
        None,
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

fn put_json(uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("PUT")
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
        Request::builder()
            .method("GET")
            .uri("/api/v1/pools")
            .body(Body::empty())
            .unwrap(),
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

    let (status, body) = send(&server, get_auth("/api/v1/snapshot-properties/tank/data")).await;
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
    let (status, body) = send(
        &server,
        post_json("/api/v1/pools", r#"{"name":"-f","disks":["/dev/sdb"]}"#),
    )
    .await;
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
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "route did not match"
    );
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"], "pool name required");

    let resp = server
        .router()
        .oneshot(get_auth("/api/v1/snapshots/"))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "route did not match"
    );

    let resp = server
        .router()
        .oneshot(get_auth("/api/v1/snapshot-properties/"))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "route did not match"
    );
}

#[tokio::test]
async fn snapshot_property_route_matches_multi_segment_datasets() {
    // The API writes the property with a PUT to a multi-segment dataset path;
    // a 404 here would leave scheduled snapshots unowned.
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[
        "tank/data/child@naslos-2026-01-05_10-15-00\n",
        "",
    ]));
    let calls = runner.calls();
    let zfs = ZfsClient::new(runner, host.root.clone());
    let server = server_with(Some(zfs), None, TOKEN);

    let (status, body) = send(
        &server,
        put_json(
            "/api/v1/snapshot-properties/tank/data/child",
            r#"{"name":"naslos-2026-01-05_10-15-00","schedule":"abc123"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["schedule"], "abc123");
    assert!(common::ran_command(
        &calls,
        "/usr/local/sbin/zfs set user:naslos:schedule=abc123 tank/data/child@naslos-2026-01-05_10-15-00"
    ));
}

#[tokio::test]
async fn disks_health_is_200_without_smartctl() {
    // SMART unavailable is not an error: the VM's virtio disks take this path.
    let (status, body) = send(&degraded(), get_auth("/api/v1/disks/health")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["smartctlAvailable"], false);
    assert!(body["disks"].is_array());
    assert!(body["error"].as_str().unwrap().contains("SMART"));
}

#[tokio::test]
async fn disks_health_merges_smart_data() {
    use naslos_agent::zfs::SmartClient;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    let host = FakeHost::new();
    let script_dir = tempfile::tempdir().unwrap();
    let script = script_dir.path().join("smartctl");
    std::fs::write(
        &script,
        r#"#!/bin/sh
cat <<'JSON'
{"smartctl":{"exit_status":0},"device":{"name":"/dev/sdb"},
 "smart_support":{"available":true,"enabled":true},
 "smart_status":{"passed":true},"temperature":{"current":33},
 "power_on_time":{"hours":42}}
JSON
"#,
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let smart = SmartClient::new(host.root.clone(), script, Duration::from_secs(5), 10);
    let server = Server::new(
        ":0",
        None,
        None,
        Some(Arc::new(smart)),
        Options {
            auth_token: TOKEN.to_string(),
            ..Default::default()
        },
    );

    let (status, body) = send(&server, get_auth("/api/v1/disks/health")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["smartctlAvailable"], true);
    let disks = body["disks"].as_array().unwrap();
    assert_eq!(disks.len(), 4);
    assert_eq!(disks[0]["smart"]["health"], "PASSED");
    assert_eq!(disks[0]["smart"]["temperatureC"], 33);
    assert_eq!(disks[0]["smart"]["powerOnHours"], 42);
}

// ---- Rollback (FR-VMT support) -------------------------------------------

#[tokio::test]
async fn rollback_route_matches_multi_segment_dataset_paths() {
    // The VM restore path calls POST /api/v1/rollback/<pool>/vms/<name> with
    // real slashes; the catch-all route must match all segments, not 404
    // (found live: every restore answered 404 with an empty body, which the
    // API surfaced as "agent: ").
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[""]));
    let zfs = ZfsClient::new(runner.clone(), host.root.clone());
    let server = server_with(Some(zfs), None, TOKEN);
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/rollback/Data/vms/e2e-vmd-drill")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"name":"drill-snap"}"#))
        .unwrap();
    let (status, body) = send(&server, req).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let calls = runner.argv();
    let calls = calls.lock().unwrap();
    assert!(
        calls
            .iter()
            .any(|argv| argv.contains(&"rollback".to_string())
                && argv.last().map(String::as_str) == Some("Data/vms/e2e-vmd-drill@drill-snap")),
        "expected a zfs rollback call, got {calls:?}"
    );
}

// ---- zvols ---------------------------------------------------------------

#[tokio::test]
async fn zvol_routes_are_503_in_degraded_mode() {
    let (status, body) = send(&degraded(), get_auth("/api/v1/zvols")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        body["error"],
        "ZFS is not available on this node (agent running in degraded mode)"
    );
}

#[tokio::test]
async fn zvol_create_rejects_bad_input_with_400() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[]));
    let zfs = ZfsClient::new(runner, host.root.clone());
    let server = server_with(Some(zfs), None, TOKEN);

    for body in [
        r#"{"name":"tank/vol","size":"lots"}"#,
        r#"{"name":"tank/vol","size":"10G","options":{"recordsize":"128K"}}"#,
        r#"{"name":"../vol","size":"10G"}"#,
    ] {
        let (status, _) = send(&server, post_json("/api/v1/zvols", body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }
}

#[tokio::test]
async fn zvol_delete_requires_confirm_and_list_has_the_json_shape() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[
        "tank/vol\t10737418240\t16384\t1048576\t9663676416\t10737418240\toff\n",
    ]));
    let zfs = ZfsClient::new(runner, host.root.clone());
    let server = server_with(Some(zfs), None, TOKEN);

    let (status, body) = send(&server, get_auth("/api/v1/zvols")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body[0]["name"], "tank/vol");
    assert_eq!(body[0]["volsize"], "10G");
    assert_eq!(body[0]["volsizeBytes"], 10_737_418_240i64);
    assert_eq!(body[0]["volblocksize"], "16384");

    let req = Request::builder()
        .method("DELETE")
        .uri("/api/v1/zvols?name=tank/vol")
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(&server, req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("confirm"));
}

#[tokio::test]
async fn encryption_status_route_reports_the_raw_properties() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[
        "encryption\taes-256-gcm\nkeystatus\tunavailable\nencryptionroot\ttank/media\nmounted\tno\n",
    ]));
    let zfs = ZfsClient::new(runner, host.root.clone());
    let server = server_with(Some(zfs), None, TOKEN);

    let (status, body) = send(&server, get_auth("/api/v1/encryption/status/tank/media")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["encryption"], "aes-256-gcm");
    assert_eq!(body["keystatus"], "unavailable");
    assert_eq!(body["encryptionRoot"], "tank/media");
    assert_eq!(body["mounted"], false);
}

#[tokio::test]
async fn unlock_response_never_echoes_the_key() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[
        "encryption\taes-256-gcm\nkeystatus\tunavailable\nencryptionroot\ttank/media\nmounted\tno\n",
        "",
        "encryption\taes-256-gcm\nkeystatus\tavailable\nencryptionroot\ttank/media\nmounted\tyes\n",
    ]));
    let argv = runner.argv();
    let zfs = ZfsClient::new(runner, host.root.clone());
    let server = server_with(Some(zfs), None, TOKEN);

    let key = vec![7u8; 32];
    let payload = serde_json::json!({ "dataset": "tank/media", "key": key }).to_string();
    let (status, body) = send(&server, post_json("/api/v1/encryption/unlock", &payload)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "unlocked");
    assert_eq!(body["keystatus"], "available");

    let rendered = body.to_string();
    assert!(
        !rendered.contains("\"key\""),
        "the response has a key field"
    );
    assert!(
        !rendered.contains(&format!("{key:?}")),
        "the response echoed the key"
    );
    for args in argv.lock().unwrap().iter() {
        for arg in args {
            assert!(!arg.contains("7, 7, 7"), "the key reached argv: {arg}");
        }
    }
}

#[tokio::test]
async fn dataset_delete_forwards_the_expected_guid() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&["filesystem\t42\n", ""]));
    let calls = runner.calls();
    let zfs = ZfsClient::new(runner, host.root.clone());
    let server = server_with(Some(zfs), None, TOKEN);

    let resp = server
        .router()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/datasets/tank/data?guid=42")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let calls = calls.lock().unwrap();
    assert!(
        calls
            .iter()
            .any(|c| c == "/usr/local/sbin/zfs list -H -o type,guid tank/data"),
        "{calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|c| c == "/usr/local/sbin/zfs destroy tank/data"),
        "{calls:?}"
    );
}

#[tokio::test]
async fn a_busy_zvol_delete_is_409() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[
        "volume\n", // require_zvol
        "ERR:cannot destroy 'tank/vol': volume is busy\n",
    ]));
    let zfs = ZfsClient::new(runner, host.root.clone());
    let server = server_with(Some(zfs), None, TOKEN);

    let resp = server
        .router()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/zvols?name=tank/vol&confirm=tank/vol")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_forced_receive_requires_confirmation() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[]));
    let zfs = ZfsClient::new(runner, host.root.clone());
    let server = server_with(Some(zfs), None, TOKEN);

    let (status, body) = send(
        &server,
        Request::builder()
            .method("POST")
            .uri("/api/v1/zfs/receive/tank/data?force=true")
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"].as_str().unwrap().contains("confirm"),
        "{body}"
    );
}

#[tokio::test]
async fn a_forced_import_requires_confirmation() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[]));
    let zfs = ZfsClient::new(runner, host.root.clone());
    let server = server_with(Some(zfs), None, TOKEN);

    let (status, body) = send(
        &server,
        post_json("/api/v1/pools/import", r#"{"name":"tank","force":true}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"].as_str().unwrap().contains("confirm"),
        "{body}"
    );
}

#[tokio::test]
async fn a_raw_receive_unlock_key_is_validated_before_any_stream() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[]));
    let argv = runner.argv();
    let zfs = ZfsClient::new(runner, host.root.clone());
    let server = server_with(Some(zfs), None, TOKEN);

    let key = "11".repeat(32);
    let cases = [
        // A key without raw=true could only belong to a plaintext stream.
        ("/api/v1/zfs/receive/tank/data", key.clone()),
        // Wrong length and non-hex values are refused.
        ("/api/v1/zfs/receive/tank/data?raw=true", "ab".to_string()),
        ("/api/v1/zfs/receive/tank/data?raw=true", "zz".repeat(32)),
        // A forced receive is never combined with a raw key.
        (
            "/api/v1/zfs/receive/tank/data?raw=true&force=true&confirm=tank/data",
            key,
        ),
    ];
    for (uri, value) in cases {
        let (status, body) = send(
            &server,
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header("x-naslos-unlock-key", &value)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}: {body}");
    }
    assert!(argv.lock().unwrap().is_empty(), "no zfs command may run");
}
