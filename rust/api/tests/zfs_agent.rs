//! S2 contract tests: the ZFS/datasets handlers pass through to the agent and
//! map agent errors to the right HTTP status. A mock agent (a small axum
//! server) stands in for the DaemonSet.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use http_body_util::BodyExt;
use naslos_api::agent::Client as AgentClient;
use naslos_api::metrics::Manager;
use naslos_api::server::build_router;
use naslos_api::shares::{Manager as SharesManager, SambaUserStore};
use naslos_api::state::{AppState, Cidr};
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

const SECRET: &str = "test-proxy-secret";

/// A mock agent. `default` answers every path; `pools` overrides
/// `GET /api/v1/pools` when set.
struct MockAgent {
    default: (u16, Value),
    pools: Option<(u16, Value)>,
}

async fn spawn_mock(mock: MockAgent) -> String {
    let default = Arc::new(mock.default);
    let pools = mock.pools.map(Arc::new);
    let app = Router::new()
        .route(
            "/api/v1/pools",
            get({
                let default = default.clone();
                let pools = pools.clone();
                move || {
                    let (status, body) = pools
                        .as_ref()
                        .map(|p| (**p).clone())
                        .unwrap_or_else(|| (*default).clone());
                    async move { (StatusCode::from_u16(status).unwrap(), Json(body)) }
                }
            }),
        )
        .route(
            "/api/v1/pools/import",
            get({
                let default = default.clone();
                move || {
                    let (status, body) = (*default).clone();
                    async move { (StatusCode::from_u16(status).unwrap(), Json(body)) }
                }
            }),
        )
        .route(
            "/api/v1/datasets",
            get({
                let default = default.clone();
                move || {
                    let (status, body) = (*default).clone();
                    async move { (StatusCode::from_u16(status).unwrap(), Json(body)) }
                }
            }),
        )
        .route(
            "/api/v1/pools/{pool}",
            get({
                let default = default.clone();
                move || {
                    let (status, body) = (*default).clone();
                    async move { (StatusCode::from_u16(status).unwrap(), Json(body)) }
                }
            })
            .delete({
                let default = default.clone();
                move || {
                    let (status, body) = (*default).clone();
                    async move { (StatusCode::from_u16(status).unwrap(), Json(body)) }
                }
            }),
        )
        .route(
            "/api/v1/pools/{pool}/devices",
            post({
                let default = default.clone();
                move || {
                    let (status, body) = (*default).clone();
                    async move { (StatusCode::from_u16(status).unwrap(), Json(body)) }
                }
            }),
        )
        .route(
            "/api/v1/datasets/{*rest}",
            post({
                let default = default.clone();
                move || {
                    let (status, body) = (*default).clone();
                    async move { (StatusCode::from_u16(status).unwrap(), Json(body)) }
                }
            })
            .delete({
                let default = default.clone();
                move || {
                    let (status, body) = (*default).clone();
                    async move { (StatusCode::from_u16(status).unwrap(), Json(body)) }
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

fn state_with_agent(base_url: &str) -> Arc<AppState> {
    let agent = AgentClient::new(base_url, "token", "").unwrap();
    Arc::new(AppState {
        proxy_secret: SECRET.to_string(),
        trusted_cidrs: vec![Cidr::parse("0.0.0.0/0").unwrap()],
        metrics: Manager::new(),
        talos: None,
        agent: Some(Arc::new(agent)),
        shares: std::sync::Mutex::new(SharesManager::new("", "/var/mnt")),
        samba_users: std::sync::Mutex::new(SambaUserStore::new("")),
        charts: None,
        catalog: std::sync::Arc::new(naslos_api::state::CatalogHolder::new()),
        identity: None,
    })
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
    let req = match body {
        Some(v) => {
            b = b.header("content-type", "application/json");
            b.body(Body::from(v.to_string())).unwrap()
        }
        None => b.body(Body::empty()).unwrap(),
    };
    let resp = build_router(state).oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn list_pools_passes_through() {
    let base = spawn_mock(MockAgent {
        default: (
            200,
            json!([{ "name": "tank", "size": "10G", "alloc": "1G", "free": "9G", "health": "ONLINE", "disks": ["/dev/vdb"] }]),
        ),
        pools: None,
    })
    .await;
    let (status, body) = call(state_with_agent(&base), "GET", "/api/volumes/zfs", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body[0]["name"], "tank");
    assert_eq!(body[0]["health"], "ONLINE");
}

#[tokio::test]
async fn degraded_agent_503_is_forwarded() {
    let base = spawn_mock(MockAgent {
        default: (503, json!({ "error": "ZFS is not available on this node" })),
        pools: None,
    })
    .await;
    let (status, body) = call(state_with_agent(&base), "GET", "/api/volumes/zfs", None).await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "503 must be forwarded, not 500"
    );
    assert_eq!(body["error"], "ZFS is not available on this node");
}

#[tokio::test]
async fn agent_validation_400_is_forwarded() {
    // The pool must exist for create-dataset to reach the agent call.
    let base = spawn_mock(MockAgent {
        default: (400, json!({ "error": "invalid disk path" })),
        pools: Some((
            200,
            json!([{ "name": "tank", "size": "10G", "alloc": "1G", "free": "9G", "health": "ONLINE", "topology": "", "disks": [], "mountpoint": "" }]),
        )),
    })
    .await;
    let (status, body) = call(
        state_with_agent(&base),
        "POST",
        "/api/datasets",
        Some(json!({ "pool": "tank", "name": "media" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "invalid disk path");
}

#[tokio::test]
async fn bad_pool_name_fails_fast_without_contacting_the_agent() {
    let base = spawn_mock(MockAgent {
        default: (200, json!([])),
        pools: None,
    })
    .await;
    let (status, body) = call(
        state_with_agent(&base),
        "POST",
        "/api/volumes/zfs",
        Some(json!({ "name": "-f", "disks": ["/dev/vdb"] })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("invalid pool name"));
}

#[tokio::test]
async fn datasets_delete_validates_path() {
    let base = spawn_mock(MockAgent {
        default: (200, json!([])),
        pools: None,
    })
    .await;
    let (status, _b) = call(
        state_with_agent(&base),
        "DELETE",
        "/api/datasets?name=media",
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a bare name must be <pool>/<name>"
    );
}

#[tokio::test]
async fn no_agent_configured_is_502() {
    let state = Arc::new(AppState {
        proxy_secret: SECRET.to_string(),
        trusted_cidrs: vec![Cidr::parse("0.0.0.0/0").unwrap()],
        metrics: Manager::new(),
        talos: None,
        agent: None,
        shares: std::sync::Mutex::new(SharesManager::new("", "/var/mnt")),
        samba_users: std::sync::Mutex::new(SambaUserStore::new("")),
        charts: None,
        catalog: std::sync::Arc::new(naslos_api::state::CatalogHolder::new()),
        identity: None,
    });
    let (status, _b) = call(state, "GET", "/api/volumes/zfs", None).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
}
