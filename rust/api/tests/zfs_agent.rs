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
use naslos_api::schedule::Cadence;
use naslos_api::server::build_router;
use naslos_api::server::storage_schedules::{run_due_schedules, ScheduleKind, StorageSchedule};
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
            "/api/v1/pools/{pool}/scrub",
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
        webhook: std::sync::Arc::new(naslos_api::webhook::Manager::new("")),
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
        storage_schedules: std::sync::Arc::new(
            naslos_api::server::storage_schedules::StorageScheduleStore::new(""),
        ),
        identity: None,
        external_directory: naslos_api::directory::Store::disabled(),
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
        webhook: std::sync::Arc::new(naslos_api::webhook::Manager::new("")),
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
        storage_schedules: std::sync::Arc::new(
            naslos_api::server::storage_schedules::StorageScheduleStore::new(""),
        ),
        identity: None,
        external_directory: naslos_api::directory::Store::disabled(),
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

#[tokio::test]
async fn scrub_schedule_reads_pool_health_before_starting_the_scrub() {
    // The agent's `GET /api/v1/pools/{pool}` returns a PoolHealth object, not a
    // `{"status": ...}` map. Decoding it the wrong way fails every tick, so the
    // schedule never starts a scrub and never reports a result.
    let base = spawn_mock(MockAgent {
        default: (
            200,
            json!({
                "name": "tank",
                "state": "ONLINE",
                "scan": "none requested",
                "errors": "No known data errors",
                "config": [],
                "ioStats": {}
            }),
        ),
        pools: None,
    })
    .await;
    let state = state_with_agent(&base);
    let now = chrono::Utc::now();
    state
        .storage_schedules
        .put(StorageSchedule {
            id: "scrubtest".to_string(),
            kind: ScheduleKind::Scrub,
            target: "tank".to_string(),
            cadence: Cadence::Daily,
            run_at: "02:30".to_string(),
            enabled: true,
            next_run: Some(now - chrono::Duration::seconds(1)),
            ..Default::default()
        })
        .unwrap();

    run_due_schedules(&state, now).await;

    let saved = state.storage_schedules.get("scrubtest").unwrap();
    assert_eq!(
        saved.last_result, "started",
        "the schedule must read pool health and start the scrub"
    );
}

fn scrub_schedule(id: &str, now: chrono::DateTime<chrono::Utc>) -> StorageSchedule {
    StorageSchedule {
        id: id.to_string(),
        kind: ScheduleKind::Scrub,
        target: "tank".to_string(),
        cadence: Cadence::Daily,
        run_at: "02:30".to_string(),
        enabled: true,
        next_run: Some(now),
        ..Default::default()
    }
}

#[tokio::test]
async fn scrub_schedule_resolves_a_finished_scrub_from_the_scan_field() {
    // `PoolHealth.scan` is already the parsed `scan:` value (e.g. "scrub
    // repaired ..."). Running it through the raw-status parser a second time
    // yields "" and leaves the schedule stuck on `started` forever.
    let base = spawn_mock(MockAgent {
        default: (
            200,
            json!({
                "name": "tank",
                "state": "ONLINE",
                "scan": "scrub repaired 0B in 00:01:23 with 0 errors on Sun Jan  4 02:00:00 2026",
                "errors": "No known data errors",
                "config": [],
                "ioStats": {}
            }),
        ),
        pools: None,
    })
    .await;
    let state = state_with_agent(&base);
    let now = chrono::Utc::now();
    let mut entry = scrub_schedule("scrubdone", now + chrono::Duration::hours(1));
    entry.last_result = "started".to_string();
    state.storage_schedules.put(entry).unwrap();

    run_due_schedules(&state, now).await;

    let saved = state.storage_schedules.get("scrubdone").unwrap();
    assert_eq!(saved.last_result, "ok");
}

#[tokio::test]
async fn scrub_schedule_does_not_restart_a_running_scrub() {
    let posts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = posts.clone();
    let app = Router::new()
        .route(
            "/api/v1/pools/{pool}",
            get(|| async {
                Json(json!({
                    "name": "tank",
                    "state": "ONLINE",
                    "scan": "scrub in progress since Sun Jan  4 01:00:00 2026",
                    "errors": "No known data errors",
                    "config": [],
                    "ioStats": {}
                }))
            }),
        )
        .route(
            "/api/v1/pools/{pool}/scrub",
            post(move || {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    (StatusCode::OK, Json(json!({ "status": "scrub started" })))
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let state = state_with_agent(&format!("http://{addr}"));
    let now = chrono::Utc::now();
    let mut entry = scrub_schedule("scrubrunning", now - chrono::Duration::seconds(1));
    entry.last_result = "started".to_string();
    state.storage_schedules.put(entry).unwrap();

    run_due_schedules(&state, now).await;

    assert_eq!(
        posts.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a running scrub must not be restarted"
    );
    let saved = state.storage_schedules.get("scrubrunning").unwrap();
    assert_eq!(saved.last_result, "started");
    assert!(saved.next_run.unwrap() > now);
}
