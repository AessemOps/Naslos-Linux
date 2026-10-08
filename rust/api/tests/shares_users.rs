//! Contract tests for the S3c shares/users handlers.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use naslos_api::server::build_router;
use naslos_api::state::AppState;
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;

const SECRET: &str = "test-proxy-secret";

fn state() -> Arc<AppState> {
    Arc::new(AppState::for_test(SECRET))
}

async fn call(method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value, String) {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("X-Naslos-Proxy-Secret", SECRET)
        .header("Remote-User", "admin")
        .header("Remote-Groups", "naslos_admins");
    let req = match body {
        Some(v) => {
            b = b.header("content-type", "application/json");
            b.body(Body::from(v.to_string())).unwrap()
        }
        None => b.body(Body::empty()).unwrap(),
    };
    let resp = build_router(state()).oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let raw = String::from_utf8_lossy(&bytes).to_string();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value, raw)
}

#[tokio::test]
async fn shares_list_is_empty_initially() {
    let (status, body, _r) = call("GET", "/api/shares", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn share_create_rejects_a_bad_name() {
    // The dataset check runs first (and fails without an agent), so this asserts
    // the 400 path; the specific name rule is unit-tested in the shares module.
    let (status, _body, _r) = call(
        "POST",
        "/api/shares",
        Some(serde_json::json!({ "name": "bad/name", "path": "/var/mnt/tank/x" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn share_create_requires_a_dataset_path() {
    // No agent configured, so the dataset check fails closed with 400.
    let (status, body, _r) = call(
        "POST",
        "/api/shares",
        Some(serde_json::json!({ "name": "media", "path": "/var/mnt/tank/media" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("ZFS dataset"));
}

#[tokio::test]
async fn samba_config_is_text() {
    let (status, _body, raw) = call("GET", "/api/shares/config/samba", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(raw.contains("[global]"));
}

#[tokio::test]
async fn shares_status_reports_a_revision() {
    let (status, body, _r) = call("GET", "/api/shares/status", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["revision"].is_string());
    assert_eq!(body["shareCount"], 0);
}

#[tokio::test]
async fn users_are_503_without_ldap() {
    let (status, body, _r) = call("GET", "/api/users", None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body["error"].as_str().unwrap().contains("Identity/LDAP"));

    let (status, _b, _r) = call("GET", "/api/groups", None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

// ---- S4d: catalog + sources ---------------------------------------------

#[tokio::test]
async fn catalog_is_empty_without_sources() {
    let (status, body, _r) = call("GET", "/api/catalog", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn sources_are_503_without_the_chart_manager() {
    let (status, body, _r) = call("GET", "/api/sources", None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("chart repositories"));

    let (status, _b, _r) = call("POST", "/api/sources/refresh", None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}
