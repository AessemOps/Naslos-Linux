//! A node-side apply failure must be reported and the share rolled back
//! (AUD-2611-T10-2).

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, put};
use axum::{Json, Router};
use http_body_util::BodyExt;
use naslos_api::server::build_router;
use naslos_api::state::AppState;
use serde_json::Value;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tower::ServiceExt;

const SECRET: &str = "test-proxy-secret";

/// A fake agent that always fails the first config push when `fail_first` is
/// set, and succeeds later pushes unless `fail_always` is set.
async fn fake_agent(fail_first: bool, fail_always: bool) -> String {
    let calls = Arc::new(AtomicUsize::new(0));
    let apply_calls = calls.clone();
    let app = Router::new()
        .route(
            "/api/v1/datasets",
            get(|| async {
                Json(serde_json::json!([
                    { "name": "tank", "mountpoint": "/var/mnt/tank" }
                ]))
            }),
        )
        .route(
            "/api/v1/shares/config",
            put(move || {
                let calls = apply_calls.clone();
                async move {
                    let n = calls.fetch_add(1, Ordering::SeqCst);
                    let fail = fail_always || (fail_first && n == 0) || (!fail_first && n > 0);
                    if fail {
                        (StatusCode::INTERNAL_SERVER_ERROR, "boom").into_response()
                    } else {
                        Json(serde_json::json!({
                            "applied": true,
                            "revision": "r1",
                            "sambaConfPath": "/var/lib/naslos/smb.conf",
                            "ganeshaConfPath": "/var/lib/naslos/ganesha.conf",
                            "smbShareCount": 1,
                            "nfsExportCount": 0
                        }))
                        .into_response()
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

fn state_with_agent(base: &str) -> Arc<AppState> {
    let mut state = AppState::for_test(SECRET);
    state.agent = Some(Arc::new(
        naslos_api::agent::Client::new(base, "token", "").unwrap(),
    ));
    Arc::new(state)
}

async fn call(
    state: Arc<AppState>,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("X-Naslos-Proxy-Secret", SECRET)
        .header("Remote-User", "admin")
        .header("Remote-Groups", "naslos_admins");
    let mut req = match body {
        Some(v) => {
            b = b.header("content-type", "application/json");
            b.body(Body::from(v.to_string())).unwrap()
        }
        None => b.body(Body::empty()).unwrap(),
    };
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40000))));
    let resp = build_router(state).oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

async fn create_media(state: Arc<AppState>) -> (StatusCode, Value) {
    call(
        state,
        "POST",
        "/api/shares",
        Some(serde_json::json!({ "name": "media", "path": "/var/mnt/tank/media" })),
    )
    .await
}

#[tokio::test]
async fn share_create_reports_an_apply_failure_and_rolls_back() {
    let base = fake_agent(true, true).await;
    let state = state_with_agent(&base);

    let (status, body) = create_media(state.clone()).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert!(body["error"].as_str().unwrap_or("").contains("not applied"));

    let (status, body) = call(state, "GET", "/api/shares", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().map(Vec::len), Some(0), "{body}");
}

#[tokio::test]
async fn share_delete_reports_an_apply_failure_and_keeps_the_share() {
    let base = fake_agent(false, false).await;
    let state = state_with_agent(&base);

    let (status, body) = create_media(state.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // The second push (delete) fails; the share must come back.
    let (status, body) = call(state.clone(), "DELETE", "/api/shares/media", None).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");

    let (status, body) = call(state, "GET", "/api/shares", None).await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<_> = body
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["name"].as_str())
        .collect();
    assert_eq!(names, vec!["media"], "{body}");
}
