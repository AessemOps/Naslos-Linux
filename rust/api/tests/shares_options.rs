//! Contract tests for the SMB share options: shadow copies, audit logging and
//! the rendered multichannel setting (FR-SHR-13/14/15/16).

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
use std::sync::Arc;
use tower::ServiceExt;

const SECRET: &str = "test-proxy-secret";

/// A fake agent that has one dataset and always accepts the rendered config.
async fn fake_agent() -> String {
    fake_agent_for("/var/mnt/tank").await
}

async fn fake_agent_for(mountpoint: &str) -> String {
    let mountpoint = mountpoint.to_string();
    let app = Router::new()
        .route(
            "/api/v1/datasets",
            get(move || {
                let mountpoint = mountpoint.clone();
                async move {
                    Json(serde_json::json!([
                        { "name": "tank", "mountpoint": mountpoint }
                    ]))
                }
            }),
        )
        .route(
            "/api/v1/shares/config",
            put(|| async {
                Json(serde_json::json!({
                    "applied": true,
                    "revision": "test",
                    "sambaConfPath": "/var/lib/naslos/shares/smb.conf",
                    "ganeshaConfPath": "/var/lib/naslos/shares/ganesha.conf",
                    "smbShareCount": 1,
                    "nfsExportCount": 0
                }))
                .into_response()
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
    state_with_agent_for(base, "/var/mnt")
}

fn state_with_agent_for(base: &str, zfs_base: &str) -> Arc<AppState> {
    let mut state = AppState::for_test(SECRET);
    state.agent = Some(Arc::new(
        naslos_api::agent::Client::new(base, "token", "").unwrap(),
    ));
    state.shares = std::sync::Mutex::new(naslos_api::shares::Manager::new("", zfs_base));
    Arc::new(state)
}

async fn call(
    state: Arc<AppState>,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value, String) {
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
    let raw = String::from_utf8_lossy(&bytes).to_string();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value, raw)
}

async fn create(state: Arc<AppState>, body: Value) -> (StatusCode, Value) {
    let (status, value, _raw) = call(state, "POST", "/api/shares", Some(body)).await;
    (status, value)
}

#[tokio::test]
async fn smb_options_round_trip_and_render() {
    let base = fake_agent().await;
    let state = state_with_agent(&base);

    let (status, share) = create(
        state.clone(),
        serde_json::json!({
            "name": "media",
            "path": "/var/mnt/tank/media",
            "protocol": "smb",
            "shadowCopies": true,
            "audit": true
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{share}");
    assert_eq!(share["shadowCopies"], true);
    assert_eq!(share["audit"], true);

    let (status, _body, conf) = call(state.clone(), "GET", "/api/shares/config/samba", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        conf.contains("   vfs objects = shadow_copy2 fruit streams_xattr full_audit\n"),
        "{conf}"
    );
    assert!(
        conf.contains("   shadow:snapdir = .zfs/snapshot\n"),
        "{conf}"
    );
    assert!(
        conf.contains("   shadow:format = naslos-%Y-%m-%d_%H-%M-%S\n"),
        "{conf}"
    );
    assert!(conf.contains("   full_audit:syslog = no\n"), "{conf}");
    assert!(conf.contains("   logging = file\n"), "{conf}");
    assert!(conf.contains("   log level = 1\n"), "{conf}");

    // Updating one option must leave the other one alone.
    let (status, share, _raw) = call(
        state.clone(),
        "PUT",
        "/api/shares/media",
        Some(serde_json::json!({ "shadowCopies": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{share}");
    assert_eq!(share["shadowCopies"], false);
    assert_eq!(share["audit"], true);

    let (status, _body, conf) = call(state, "GET", "/api/shares/config/samba", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!conf.contains("shadow_copy2"), "{conf}");
    assert!(conf.contains("full_audit"), "{conf}");
}

#[tokio::test]
async fn smb_options_are_refused_on_nfs_shares() {
    let base = fake_agent().await;
    let state = state_with_agent(&base);

    let (status, body) = create(
        state.clone(),
        serde_json::json!({
            "name": "media",
            "path": "/var/mnt/tank/media",
            "protocol": "nfs",
            "shadowCopies": true
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or("")
            .contains("shadowCopies"),
        "{body}"
    );

    let (status, body) = create(
        state,
        serde_json::json!({
            "name": "records",
            "path": "/var/mnt/tank/records",
            "protocol": "nfs",
            "audit": true
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"].as_str().unwrap_or("").contains("audit"),
        "{body}"
    );
}

#[tokio::test]
async fn a_share_cannot_export_the_zfs_snapshot_directory() {
    // Use a real (temporary) base so the path-existence check passes and the
    // `.zfs` rule itself is what refuses the request.
    let dir = tempfile::tempdir().unwrap();
    let dataset = dir.path().join("tank");
    std::fs::create_dir_all(dataset.join(".zfs/snapshot/snap-1")).unwrap();

    let base = fake_agent_for(dataset.to_str().unwrap()).await;
    let state = state_with_agent_for(&base, dir.path().to_str().unwrap());

    let (status, body) = create(
        state,
        serde_json::json!({
            "name": "snap",
            "path": dataset.join(".zfs/snapshot/snap-1").to_str().unwrap(),
            "protocol": "smb"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"].as_str().unwrap_or("").contains(".zfs"),
        "{body}"
    );
}

#[tokio::test]
async fn a_plain_share_keeps_the_default_smb_options() {
    let base = fake_agent().await;
    let state = state_with_agent(&base);

    let (status, share) = create(
        state.clone(),
        serde_json::json!({
            "name": "plain",
            "path": "/var/mnt/tank/plain",
            "protocol": "smb"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{share}");
    assert_eq!(share["shadowCopies"], false);
    assert_eq!(share["audit"], false);

    let (status, _body, conf) = call(state, "GET", "/api/shares/config/samba", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        conf.contains("   server multi channel support = yes\n"),
        "{conf}"
    );
    assert!(!conf.contains("shadow_copy2"), "{conf}");
    assert!(!conf.contains("full_audit"), "{conf}");
    assert!(!conf.contains("logging = file"), "{conf}");
    assert!(!conf.contains("log level"), "{conf}");
}
