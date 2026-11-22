//! S2 contract tests: the ZFS/datasets handlers pass through to the agent and
//! map agent errors to the right HTTP status. A mock agent (a small axum
//! server) stands in for the DaemonSet.

use axum::body::Body;
use axum::extract::ConnectInfo;
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
use std::net::SocketAddr;
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
            "/api/v1/disks/health",
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
        vmd: None,
        vms: std::sync::Arc::new(naslos_api::virt::Store::new("")),
        shares: std::sync::Mutex::new(SharesManager::new("", "/var/mnt")),
        samba_users: std::sync::Mutex::new(SambaUserStore::new("")),
        charts: None,
        catalog: std::sync::Arc::new(naslos_api::state::CatalogHolder::new()),
        app_manager: None,
        app_jobs: std::sync::Arc::new(naslos_api::server::app_jobs::AppJobManager::new()),
        base_domain: String::new(),
        providers: std::sync::Arc::new(naslos_api::providers::Registry::load("")),
        domains: std::sync::Arc::new(naslos_api::certs::Store::new("")),
        certs: None,
        sso: None,
        kube: None,
        sso_domains: Vec::new(),
        apps_namespace: "naslos-apps".to_string(),
        ddns: None,
        notifications: std::sync::Arc::new(naslos_api::notifications::Manager::new("")),
        network: naslos_api::network::Manager::new(
            "",
            std::sync::Arc::new(naslos_api::network::UnavailableNode),
        ),
        datasets_registry: std::sync::Arc::new(naslos_api::datasets::Registry::new("", "")),
        buddy: None,
        buddy_jobs: std::sync::Arc::new(naslos_api::server::buddy_send::BuddyJobManager::new()),
        buddy_schedules: std::sync::Arc::new(
            naslos_api::server::buddy_schedules::ScheduleStore::new(""),
        ),
        identity: None,
        system: naslos_api::system::Config::default(),
        updates: None,
        mcp: naslos_api::mcp::McpState::for_test(),
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
    let mut req = req;
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40000))));
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
        vmd: None,
        vms: std::sync::Arc::new(naslos_api::virt::Store::new("")),
        shares: std::sync::Mutex::new(SharesManager::new("", "/var/mnt")),
        samba_users: std::sync::Mutex::new(SambaUserStore::new("")),
        charts: None,
        catalog: std::sync::Arc::new(naslos_api::state::CatalogHolder::new()),
        app_manager: None,
        app_jobs: std::sync::Arc::new(naslos_api::server::app_jobs::AppJobManager::new()),
        base_domain: String::new(),
        providers: std::sync::Arc::new(naslos_api::providers::Registry::load("")),
        domains: std::sync::Arc::new(naslos_api::certs::Store::new("")),
        certs: None,
        sso: None,
        kube: None,
        sso_domains: Vec::new(),
        apps_namespace: "naslos-apps".to_string(),
        ddns: None,
        notifications: std::sync::Arc::new(naslos_api::notifications::Manager::new("")),
        network: naslos_api::network::Manager::new(
            "",
            std::sync::Arc::new(naslos_api::network::UnavailableNode),
        ),
        datasets_registry: std::sync::Arc::new(naslos_api::datasets::Registry::new("", "")),
        buddy: None,
        buddy_jobs: std::sync::Arc::new(naslos_api::server::buddy_send::BuddyJobManager::new()),
        buddy_schedules: std::sync::Arc::new(
            naslos_api::server::buddy_schedules::ScheduleStore::new(""),
        ),
        identity: None,
        system: naslos_api::system::Config::default(),
        updates: None,
        mcp: naslos_api::mcp::McpState::for_test(),
    });
    let (status, _b) = call(state, "GET", "/api/volumes/zfs", None).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn disk_health_merges_the_agent_report() {
    let base = spawn_mock(MockAgent {
        default: (
            200,
            json!({
                "smartctlAvailable": true,
                "error": "",
                "disks": [{
                    "device": "/dev/sda",
                    "smart": {
                        "available": true,
                        "enabled": true,
                        "health": "PASSED",
                        "temperatureC": 34,
                        "powerOnHours": 1234
                    },
                    "zfs": {
                        "pool": "tank",
                        "vdev": "mirror-0",
                        "state": "ONLINE",
                        "readErrors": 0,
                        "writeErrors": 0,
                        "cksumErrors": 0
                    }
                }]
            }),
        ),
        pools: None,
    })
    .await;
    let (status, body) = call(state_with_agent(&base), "GET", "/api/disks/health", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["smartctlAvailable"], true);
    let disks = body["disks"].as_array().unwrap();
    assert_eq!(disks.len(), 1);
    assert_eq!(disks[0]["device"], "/dev/sda");
    assert_eq!(disks[0]["smart"]["health"], "PASSED");
    assert_eq!(disks[0]["inPool"], "tank");
    assert_eq!(disks[0]["zfs"]["vdev"], "mirror-0");
}

#[tokio::test]
async fn disk_health_fills_the_pool_when_the_agent_omits_membership() {
    // An older agent reports SMART without the ZFS half. The partition member
    // (/dev/sdb1) must still surface the pool on its whole disk (/dev/sdb).
    let base = spawn_mock(MockAgent {
        default: (
            200,
            json!({
                "smartctlAvailable": true,
                "error": "",
                "disks": [{
                    "device": "/dev/sdb",
                    "smart": { "available": true, "enabled": true, "health": "PASSED" }
                }]
            }),
        ),
        pools: Some((
            200,
            json!([{
                "name": "Data", "size": "11T", "alloc": "1T", "free": "10T",
                "health": "ONLINE", "disks": ["/dev/sdb1"]
            }]),
        )),
    })
    .await;
    let (status, body) = call(state_with_agent(&base), "GET", "/api/disks/health", None).await;
    assert_eq!(status, StatusCode::OK);
    let disks = body["disks"].as_array().unwrap();
    assert_eq!(disks.len(), 1);
    assert_eq!(disks[0]["device"], "/dev/sdb");
    assert_eq!(disks[0]["inPool"], "Data");
    assert!(disks[0].get("zfs").is_none());
}

#[tokio::test]
async fn disk_health_is_non_fatal_when_the_agent_fails() {
    let base = spawn_mock(MockAgent {
        default: (503, json!({ "error": "ZFS is not available on this node" })),
        pools: None,
    })
    .await;
    let (status, body) = call(state_with_agent(&base), "GET", "/api/disks/health", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["smartctlAvailable"], false);
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("ZFS is not available"));
    assert!(body["disks"].is_array());
}

// ---- VM delete (FR-VMT support) -------------------------------------------

#[tokio::test]
async fn vm_delete_destroys_the_dataset_recursively() {
    // A VM dataset owns its snapshots, so the delete must pass
    // recursive=true: a non-recursive destroy fails with "filesystem has
    // children" the moment the VM ever had one (found by the live drill —
    // deleting a VM after its snapshot step always 502'd). The mock agent
    // records the request line; this harness has no vmd, so the post-destroy
    // config push 502s after the destroy already succeeded.
    let uris: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorder = uris.clone();
    let app = Router::new().route(
        "/api/v1/datasets/{*rest}",
        axum::routing::delete(move |req: Request<Body>| {
            let recorder = recorder.clone();
            async move {
                let line = format!("{} {}", req.method(), req.uri());
                recorder.lock().unwrap().push(line);
                (StatusCode::OK, Json(json!({ "status": "destroyed" })))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let state = state_with_agent(&format!("http://{addr}"));

    let spec = naslos_vmd::model::VmSpec {
        name: "drill-vm".into(),
        pool: "tank".into(),
        cpu: 1,
        mem_mib: 256,
        autostart: false,
        firmware: naslos_vmd::model::Firmware::default(),
        desired: naslos_vmd::model::DesiredState::default(),
        disk: naslos_vmd::model::DiskSpec {
            size_gb: 2,
            ..Default::default()
        },
        net: naslos_vmd::model::NetSpec::default(),
        cloud_init: None,
        iso: None,
    };
    state.vms.create(spec).expect("seed record");

    let (status, _body) = call(state.clone(), "DELETE", "/api/virt/vms/drill-vm", None).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "no vmd in this harness");
    let uris = uris.lock().unwrap();
    let destroy = uris
        .iter()
        .find(|u| u.starts_with("DELETE /api/v1/datasets/tank/vms/drill-vm"))
        .expect("the agent must receive the dataset destroy");
    assert!(
        destroy.contains("recursive=true"),
        "the VM dataset destroy must be recursive (snapshots belong to the VM), got: {destroy}"
    );
}

// ---- zvols ---------------------------------------------------------------

async fn record_zvol_request(
    requests: Arc<std::sync::Mutex<Vec<String>>>,
    req: Request<Body>,
) -> (StatusCode, Json<Value>) {
    let (parts, body) = req.into_parts();
    let bytes = body.collect().await.unwrap().to_bytes();
    let mut line = format!("{} {}", parts.method, parts.uri);
    if !bytes.is_empty() {
        line.push_str(&format!(" | {}", String::from_utf8_lossy(&bytes)));
    }
    requests.lock().unwrap().push(line);
    let body = if parts.method == axum::http::Method::GET {
        json!([])
    } else {
        json!({ "status": "ok" })
    };
    (StatusCode::OK, Json(body))
}

/// A mock agent that records every zvol request and always answers 200.
async fn spawn_zvol_recorder() -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let app = Router::new()
        .route(
            "/api/v1/pools",
            get(|| async {
                Json(json!([{
                    "name": "tank", "size": "10G", "alloc": "1G", "free": "9G",
                    "health": "ONLINE", "topology": "", "disks": [], "mountpoint": ""
                }]))
            }),
        )
        .route(
            "/api/v1/zvols",
            axum::routing::any({
                let requests = requests.clone();
                move |req: Request<Body>| record_zvol_request(requests.clone(), req)
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), requests)
}

fn zvol_requests(requests: &Arc<std::sync::Mutex<Vec<String>>>) -> Vec<String> {
    requests.lock().unwrap().clone()
}

#[tokio::test]
async fn zvol_create_sends_the_validated_full_name_and_options() {
    let (base, requests) = spawn_zvol_recorder().await;
    let (status, body) = call(
        state_with_agent(&base),
        "POST",
        "/api/zvols",
        Some(json!({
            "pool": "tank",
            "name": "targets/vol1",
            "size": "10G",
            "options": { "volblocksize": "16K", "compression": "zstd" }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["name"], "tank/targets/vol1");
    let requests = zvol_requests(&requests);
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert!(
        requests[0].starts_with("POST /api/v1/zvols | "),
        "{requests:?}"
    );
    assert!(
        requests[0].contains(r#""name":"tank/targets/vol1""#),
        "{requests:?}"
    );
    assert!(requests[0].contains(r#""size":"10G""#), "{requests:?}");
    assert!(
        requests[0].contains(r#""volblocksize":"16K""#),
        "{requests:?}"
    );
}

#[tokio::test]
async fn zvol_create_rejects_dataset_options_and_bad_sizes_before_the_agent() {
    let cases = [
        json!({ "pool": "tank", "name": "vol", "size": "10G", "options": { "recordsize": "128K" } }),
        json!({ "pool": "tank", "name": "vol", "size": "10G", "options": { "volsize": "10G" } }),
        json!({ "pool": "tank", "name": "vol", "size": "lots" }),
        json!({ "pool": "tank", "name": "../vol", "size": "10G" }),
        json!({ "pool": "-f", "name": "vol", "size": "10G" }),
        json!({ "pool": "tank", "name": "vol", "size": "10G", "options": { "volblocksize": "3M" } }),
    ];
    for body in cases {
        let (base, requests) = spawn_zvol_recorder().await;
        let (status, _b) = call(
            state_with_agent(&base),
            "POST",
            "/api/zvols",
            Some(body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(zvol_requests(&requests).is_empty(), "{body}");
    }
}

#[tokio::test]
async fn zvol_list_passes_the_pool_filter() {
    let (base, requests) = spawn_zvol_recorder().await;
    let state = state_with_agent(&base);

    let (status, _b) = call(state.clone(), "GET", "/api/zvols", None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _b) = call(state.clone(), "GET", "/api/zvols?pool=tank", None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _b) = call(state, "GET", "/api/zvols?pool=-f", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let requests = zvol_requests(&requests);
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert_eq!(requests[0], "GET /api/v1/zvols");
    assert_eq!(requests[1], "GET /api/v1/zvols?pool=tank");
}

#[tokio::test]
async fn zvol_resize_validates_and_forwards_the_name() {
    let (base, requests) = spawn_zvol_recorder().await;
    let state = state_with_agent(&base);

    let (status, body) = call(
        state.clone(),
        "PUT",
        "/api/zvols?name=tank/vol",
        Some(json!({ "size": "20G" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "tank/vol");

    let (status, _b) = call(
        state.clone(),
        "PUT",
        "/api/zvols?name=tank/vol",
        Some(json!({ "size": "lots" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _b) = call(state, "PUT", "/api/zvols", Some(json!({ "size": "20G" }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let requests = zvol_requests(&requests);
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert_eq!(
        requests[0],
        "PUT /api/v1/zvols?name=tank%2Fvol | {\"size\":\"20G\"}"
    );
}

#[tokio::test]
async fn zvol_delete_requires_confirmation_matching_the_name() {
    let (base, requests) = spawn_zvol_recorder().await;
    let state = state_with_agent(&base);

    let (status, _b) = call(state.clone(), "DELETE", "/api/zvols?name=tank/vol", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "no confirm");
    let (status, _b) = call(
        state.clone(),
        "DELETE",
        "/api/zvols?name=tank/vol&confirm=tank/other",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "wrong confirm");
    assert!(zvol_requests(&requests).is_empty());

    let (status, body) = call(
        state,
        "DELETE",
        "/api/zvols?name=tank/vol&confirm=tank/vol",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "tank/vol");
    let requests = zvol_requests(&requests);
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert_eq!(
        requests[0],
        "DELETE /api/v1/zvols?name=tank%2Fvol&confirm=tank%2Fvol"
    );
}

#[tokio::test]
async fn zvol_refusals_from_the_agent_are_forwarded() {
    let app = Router::new().route(
        "/api/v1/zvols",
        axum::routing::any(|| async {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "zvol tank/vol is in use (held by dm-0)" })),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let state = state_with_agent(&format!("http://{addr}"));

    let (status, body) = call(
        state,
        "DELETE",
        "/api/zvols?name=tank/vol&confirm=tank/vol",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("in use"), "{body}");
}

#[tokio::test]
async fn zvols_without_an_agent_are_502() {
    let state = Arc::new(AppState {
        proxy_secret: SECRET.to_string(),
        trusted_cidrs: vec![Cidr::parse("0.0.0.0/0").unwrap()],
        metrics: Manager::new(),
        talos: None,
        agent: None,
        vmd: None,
        vms: std::sync::Arc::new(naslos_api::virt::Store::new("")),
        shares: std::sync::Mutex::new(SharesManager::new("", "/var/mnt")),
        samba_users: std::sync::Mutex::new(SambaUserStore::new("")),
        charts: None,
        catalog: std::sync::Arc::new(naslos_api::state::CatalogHolder::new()),
        app_manager: None,
        app_jobs: std::sync::Arc::new(naslos_api::server::app_jobs::AppJobManager::new()),
        base_domain: String::new(),
        providers: std::sync::Arc::new(naslos_api::providers::Registry::load("")),
        domains: std::sync::Arc::new(naslos_api::certs::Store::new("")),
        certs: None,
        sso: None,
        kube: None,
        sso_domains: Vec::new(),
        apps_namespace: "naslos-apps".to_string(),
        ddns: None,
        notifications: std::sync::Arc::new(naslos_api::notifications::Manager::new("")),
        network: naslos_api::network::Manager::new(
            "",
            std::sync::Arc::new(naslos_api::network::UnavailableNode),
        ),
        datasets_registry: std::sync::Arc::new(naslos_api::datasets::Registry::new("", "")),
        buddy: None,
        buddy_jobs: std::sync::Arc::new(naslos_api::server::buddy_send::BuddyJobManager::new()),
        buddy_schedules: std::sync::Arc::new(
            naslos_api::server::buddy_schedules::ScheduleStore::new(""),
        ),
        identity: None,
        system: naslos_api::system::Config::default(),
        updates: None,
        mcp: naslos_api::mcp::McpState::for_test(),
    });
    let (status, _b) = call(state, "GET", "/api/zvols", None).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
}
