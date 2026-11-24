//! Contract tests for local replication (FR-REP): route validation, the async
//! job lifecycle and the retention rule that only replication-owned snapshots
//! may be destroyed. A small axum server stands in for the agent streams.

use axum::body::Body;
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use http_body_util::BodyExt;
use naslos_api::agent::{Client as AgentClient, SnapshotInfo};
use naslos_api::metrics::Manager;
use naslos_api::server::build_router;
use naslos_api::shares::{Manager as SharesManager, SambaUserStore};
use naslos_api::state::{AppState, Cidr};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tower::ServiceExt;

const SECRET: &str = "test-proxy-secret";

#[derive(Clone)]
struct MockAgent {
    datasets: Arc<Mutex<Vec<Value>>>,
    snaps: Arc<Mutex<HashMap<String, Vec<Value>>>>,
    paths: Arc<Mutex<HashMap<String, Vec<String>>>>,
    deleted: Arc<Mutex<Vec<String>>>,
    created: Arc<Mutex<Vec<String>>>,
    sends: Arc<Mutex<Vec<Value>>>,
    receives: Arc<Mutex<Vec<Value>>>,
    zvols: Arc<Mutex<Vec<Value>>>,
    send_body: Arc<Vec<u8>>,
    send_delay: Duration,
}

impl Default for MockAgent {
    fn default() -> Self {
        Self {
            datasets: Arc::new(Mutex::new(Vec::new())),
            snaps: Arc::new(Mutex::new(HashMap::new())),
            paths: Arc::new(Mutex::new(HashMap::new())),
            deleted: Arc::new(Mutex::new(Vec::new())),
            created: Arc::new(Mutex::new(Vec::new())),
            sends: Arc::new(Mutex::new(Vec::new())),
            receives: Arc::new(Mutex::new(Vec::new())),
            zvols: Arc::new(Mutex::new(Vec::new())),
            send_body: Arc::new(b"zfs-stream".to_vec()),
            send_delay: Duration::ZERO,
        }
    }
}

impl MockAgent {
    fn with_pair() -> Self {
        let mock = Self::default();
        mock.datasets.lock().unwrap().extend([
            json!({ "name": "tank/src" }),
            json!({ "name": "backup/dst" }),
        ]);
        mock
    }

    fn set_snapshots(&self, dataset: &str, snapshots: &[(&str, &str)]) {
        self.snaps.lock().unwrap().insert(
            dataset.to_string(),
            snapshots
                .iter()
                .map(|(name, guid)| json!({ "name": name, "guid": guid, "created": "" }))
                .collect(),
        );
    }

    fn set_paths(&self, dataset: &str, paths: &[&str]) {
        self.paths.lock().unwrap().insert(
            dataset.to_string(),
            paths.iter().map(|p| p.to_string()).collect(),
        );
    }
}

async fn spawn_mock(mock: MockAgent) -> String {
    let m = mock.clone();
    let app = Router::new()
        .route(
            "/api/v1/datasets",
            get(move |State(m): State<MockAgent>| async move {
                Json(m.datasets.lock().unwrap().clone())
            }),
        )
        .route(
            "/api/v1/zfs/snapshots/{*dataset}",
            get(
                |State(m): State<MockAgent>,
                 Path(dataset): Path<String>,
                 Query(q): Query<HashMap<String, String>>| async move {
                    let recursive = q.get("recursive").map(|v| v != "false").unwrap_or(true);
                    let snaps = m.snaps.lock().unwrap();
                    let mut list = Vec::new();
                    for (name, items) in snaps.iter() {
                        if name != &dataset
                            && !(recursive && name.starts_with(&format!("{dataset}/")))
                        {
                            continue;
                        }
                        for item in items {
                            let mut item = item.clone();
                            item["dataset"] = json!(name);
                            list.push(item);
                        }
                    }
                    Json(list)
                },
            ),
        )
        .route(
            "/api/v1/snapshots/{*dataset}",
            get(
                |State(m): State<MockAgent>,
                 Path(dataset): Path<String>,
                 Query(q): Query<HashMap<String, String>>| async move {
                    if q.get("paths").map(String::as_str) == Some("true") {
                        let list = m
                            .paths
                            .lock()
                            .unwrap()
                            .get(&dataset)
                            .cloned()
                            .unwrap_or_default();
                        return Json(json!(list));
                    }
                    let list = m
                        .snaps
                        .lock()
                        .unwrap()
                        .get(&dataset)
                        .cloned()
                        .unwrap_or_default();
                    Json(json!(list))
                },
            )
            .post(
                |State(m): State<MockAgent>, Path(dataset): Path<String>, body: Body| async move {
                    let bytes = axum::body::to_bytes(body, 64 * 1024).await.unwrap();
                    let req: Value = serde_json::from_slice(&bytes).unwrap_or_default();
                    let name = req["name"].as_str().unwrap_or("").to_string();
                    m.created.lock().unwrap().push(format!("{dataset}@{name}"));
                    m.snaps
                        .lock()
                        .unwrap()
                        .entry(dataset.clone())
                        .or_default()
                        .push(json!({
                            "name": name,
                            "guid": format!("guid-{name}"),
                            "created": ""
                        }));
                    (
                        StatusCode::CREATED,
                        Json(json!({ "status": "snapshot created" })),
                    )
                },
            )
            .delete(
                |State(m): State<MockAgent>,
                 Path(dataset): Path<String>,
                 Query(q): Query<HashMap<String, String>>| async move {
                    let name = q.get("name").cloned().unwrap_or_default();
                    m.deleted.lock().unwrap().push(format!("{dataset}@{name}"));
                    if let Some(paths) = m.paths.lock().unwrap().get_mut(&dataset) {
                        paths.retain(|p| p != &format!("{dataset}@{name}"));
                    }
                    Json(json!({ "status": "snapshot destroyed" }))
                },
            ),
        )
        .route(
            "/api/v1/zvols",
            get(
                |State(m): State<MockAgent>, Query(_q): Query<HashMap<String, String>>| async move {
                    Json(m.zvols.lock().unwrap().clone())
                },
            ),
        )
        .route(
            "/api/v1/zfs/send/{*dataset}",
            get(
                |State(m): State<MockAgent>,
                 Path(dataset): Path<String>,
                 Query(q): Query<HashMap<String, String>>| async move {
                    if q.get("estimate").map(String::as_str) == Some("true") {
                        return (
                            StatusCode::OK,
                            Body::from(json!({ "bytes": m.send_body.len() as i64 }).to_string()),
                        );
                    }
                    m.sends.lock().unwrap().push(json!({
                        "dataset": dataset,
                        "to": q.get("to"),
                        "from": q.get("from"),
                        "recursive": q.get("recursive"),
                    }));
                    if !m.send_delay.is_zero() {
                        tokio::time::sleep(m.send_delay).await;
                    }
                    (StatusCode::OK, Body::from(m.send_body.as_ref().clone()))
                },
            ),
        )
        .route(
            "/api/v1/zfs/receive/{*dataset}",
            post(
                |State(m): State<MockAgent>,
                 Path(dataset): Path<String>,
                 Query(q): Query<HashMap<String, String>>,
                 headers: axum::http::HeaderMap,
                 body: Body| async move {
                    let unlock = headers
                        .get("x-naslos-unlock-key")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    m.receives.lock().unwrap().push(json!({
                        "dataset": dataset,
                        "force": q.get("force"),
                        "raw": q.get("raw"),
                        "unlock": unlock,
                    }));
                    let data = axum::body::to_bytes(body, 8 << 20).await.unwrap();
                    Json(json!({ "status": "received", "bytes": data.len() }))
                },
            ),
        )
        .with_state(m);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

fn state_with_agent(base_url: &str) -> Arc<AppState> {
    state_with_agent_and_kube(base_url, None)
}

fn state_with_agent_and_kube(
    base_url: &str,
    kube: Option<Arc<naslos_api::kube::Client>>,
) -> Arc<AppState> {
    let agent = AgentClient::new(base_url, "token", "").unwrap();
    Arc::new(AppState {
        proxy_secret: SECRET.to_string(),
        trusted_cidrs: vec![Cidr::parse("0.0.0.0/0").unwrap()],
        audit: naslos_api::audit::AuditLog::for_test(),
        metrics: Manager::new(),
        talos: None,
        agent: Some(Arc::new(agent)),
        vmd: None,
        vms: Arc::new(naslos_api::virt::Store::new("")),
        shares: Mutex::new(SharesManager::new("", "/var/mnt")),
        samba_users: Mutex::new(SambaUserStore::new("")),
        identity: None,
        external_directory: naslos_api::directory::Store::disabled(),
        charts: None,
        catalog: Arc::new(naslos_api::state::CatalogHolder::new()),
        app_manager: None,
        app_jobs: Arc::new(naslos_api::server::app_jobs::AppJobManager::new()),
        base_domain: String::new(),
        providers: Arc::new(naslos_api::providers::Registry::load("")),
        domains: Arc::new(naslos_api::certs::Store::new("")),
        certs: None,
        sso: None,
        kube,
        sso_domains: Vec::new(),
        apps_namespace: "naslos-apps".to_string(),
        ddns: None,
        notifications: Arc::new(naslos_api::notifications::Manager::new("")),
        webhook: std::sync::Arc::new(naslos_api::webhook::Manager::new("")),
        smtp: std::sync::Arc::new(naslos_api::smtp::Manager::new("")),
        syslog: std::sync::Arc::new(naslos_api::syslog::Manager::new("")),
        network: naslos_api::network::Manager::new(
            "",
            Arc::new(naslos_api::network::UnavailableNode),
        ),
        datasets_registry: Arc::new(naslos_api::datasets::Registry::new("", "")),
        buddy: None,
        buddy_jobs: Arc::new(naslos_api::server::buddy_send::BuddyJobManager::new()),
        buddy_schedules: Arc::new(naslos_api::server::buddy_schedules::ScheduleStore::new("")),
        storage_schedules: Arc::new(
            naslos_api::server::storage_schedules::StorageScheduleStore::new(""),
        ),
        replication_jobs: Arc::new(naslos_api::server::replication::ReplicationJobManager::new()),
        system: naslos_api::system::Config::default(),
        updates: None,
        mcp: naslos_api::mcp::McpState::for_test(),
        reporting: Arc::new(naslos_api::reporting::Reporter::disabled()),
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

async fn wait_job(state: Arc<AppState>, id: &str) -> Value {
    let uri = format!("/api/replication/jobs/{id}");
    for _ in 0..300 {
        let (status, body) = call(state.clone(), "GET", &uri, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        if body["state"] != "running" {
            return body;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("job {id} did not finish");
}

#[tokio::test]
async fn invalid_pairs_and_missing_datasets_are_refused() {
    let mock = MockAgent::with_pair();
    let state = state_with_agent(&spawn_mock(mock).await);

    for (source, destination, expected) in [
        ("tank/src", "tank/src", "different datasets"),
        ("tank/src", "tank/src/child", "inside the source"),
        ("tank/src/child", "tank/src", "inside the destination"),
    ] {
        let (status, body) = call(
            state.clone(),
            "POST",
            "/api/replication/send",
            Some(json!({ "source": source, "destination": destination })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body["error"].as_str().unwrap().contains(expected), "{body}");
    }

    let (status, body) = call(
        state.clone(),
        "POST",
        "/api/replication/send",
        Some(json!({ "source": "tank/missing", "destination": "backup/dst" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("source dataset"));

    let (status, body) = call(
        state.clone(),
        "POST",
        "/api/replication/send",
        Some(json!({ "source": "tank/src", "destination": "backup/missing" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("destination dataset"));
}

#[tokio::test]
async fn send_needs_the_agent_and_valid_input() {
    let state = Arc::new(AppState::for_test(SECRET));
    let (status, body) = call(
        state.clone(),
        "POST",
        "/api/replication/send",
        Some(json!({ "source": "tank/src", "destination": "backup/dst" })),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body["error"].as_str().unwrap().contains("no agent client"));

    let mock = MockAgent::with_pair();
    let state = state_with_agent(&spawn_mock(mock).await);
    let (status, _body) = call(
        state.clone(),
        "POST",
        "/api/replication/send",
        Some(json!({ "source": "media", "destination": "backup/dst" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, body) = call(
        state.clone(),
        "POST",
        "/api/replication/send",
        Some(json!({
            "source": "tank/src",
            "destination": "backup/dst",
            "snapshot": "-F"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (status, body) = call(
        state.clone(),
        "POST",
        "/api/replication/send",
        Some(json!({
            "source": "tank/src",
            "destination": "backup/dst",
            "snapshot": "does-not-exist"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("does not exist"));

    let (status, body) = call(
        state,
        "POST",
        "/api/replication/send",
        Some(json!({
            "source": "tank/src",
            "destination": "backup/dst",
            "pruneKeep": 100000
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("pruneKeep"));
}

#[tokio::test]
async fn a_full_replication_job_snapshots_streams_and_finishes() {
    let mock = MockAgent::with_pair();
    let base = spawn_mock(mock.clone()).await;
    let state = state_with_agent(&base);

    let (status, body) = call(
        state.clone(),
        "POST",
        "/api/replication/send",
        Some(json!({ "source": "tank/src", "destination": "backup/dst" })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let id = body["jobId"].as_str().unwrap().to_string();

    let job = wait_job(state.clone(), &id).await;
    assert_eq!(job["state"], "succeeded", "{job}");
    assert_eq!(job["result"]["status"], "replicated");
    assert_eq!(job["result"]["incremental"], false);
    assert_eq!(job["result"]["bytes"], 10);
    let target = job["result"]["snapshot"].as_str().unwrap();
    assert!(target.starts_with("naslos-rep-"), "{target}");
    assert!(job["incremental"] == false);
    assert_eq!(mock.created.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_common_guid_makes_the_send_incremental() {
    let mock = MockAgent::with_pair();
    mock.set_snapshots("tank/src", &[("base", "g1"), ("next", "g2")]);
    mock.set_snapshots("backup/dst", &[("dest-base", "g1")]);
    let base = spawn_mock(mock.clone()).await;
    let state = state_with_agent(&base);

    let (status, body) = call(
        state.clone(),
        "POST",
        "/api/replication/send",
        Some(json!({
            "source": "tank/src",
            "destination": "backup/dst",
            "snapshot": "next"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let job = wait_job(state, body["jobId"].as_str().unwrap()).await;
    assert_eq!(job["state"], "succeeded", "{job}");
    assert_eq!(job["result"]["incremental"], true);
    assert_eq!(job["result"]["baseSnapshot"], "base");
    assert_eq!(job["result"]["baseGUID"], "g1");
    // The destination's differently named copy of the same GUID is enough.
    assert!(mock.created.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_destination_that_already_has_the_target_is_up_to_date() {
    let mock = MockAgent::with_pair();
    mock.set_snapshots("tank/src", &[("snap", "g1")]);
    mock.set_snapshots("backup/dst", &[("renamed-on-destination", "g1")]);
    let base = spawn_mock(mock).await;
    let state = state_with_agent(&base);

    let (status, body) = call(
        state.clone(),
        "POST",
        "/api/replication/send",
        Some(json!({
            "source": "tank/src",
            "destination": "backup/dst",
            "snapshot": "snap"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let job = wait_job(state, body["jobId"].as_str().unwrap()).await;
    assert_eq!(job["state"], "succeeded", "{job}");
    assert_eq!(job["result"]["status"], "up-to-date");
    assert_eq!(job["result"]["bytes"], 0);
}

#[tokio::test]
async fn a_recursive_job_repairs_a_missing_descendant_replica() {
    let mock = MockAgent::with_pair();
    mock.datasets
        .lock()
        .unwrap()
        .push(json!({ "name": "tank/src/child" }));
    mock.set_snapshots("tank/src", &[("base", "g0"), ("snap", "g1")]);
    mock.set_snapshots("backup/dst", &[("base", "g0"), ("snap", "g1")]);
    mock.set_snapshots(
        "tank/src/child",
        &[("base", "g0-child"), ("snap", "g1-child")],
    );
    // The root replica is current, but `tank/src/child` never reached the
    // destination. The root GUID alone must not mark the recursive job done.
    mock.set_paths(
        "tank/src",
        &[
            "tank/src@base",
            "tank/src@snap",
            "tank/src/child@base",
            "tank/src/child@snap",
        ],
    );
    mock.set_paths("backup/dst", &["backup/dst@base", "backup/dst@snap"]);
    let base = spawn_mock(mock.clone()).await;
    let state = state_with_agent(&base);

    let (status, body) = call(
        state.clone(),
        "POST",
        "/api/replication/send",
        Some(json!({
            "source": "tank/src",
            "destination": "backup/dst",
            "snapshot": "snap",
            "recursive": true
        })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let job = wait_job(state, body["jobId"].as_str().unwrap()).await;
    assert_eq!(job["state"], "succeeded", "{job}");
    assert_eq!(job["result"]["status"], "replicated", "{job}");
    // The root is already current; only the missing child subtree is sent and
    // it must be a full send, never an incremental from an older base.
    assert_eq!(job["result"]["incremental"], false, "{job}");
    assert!(job["result"].get("baseSnapshot").is_none(), "{job}");
    let sends = mock.sends.lock().unwrap().clone();
    assert_eq!(sends.len(), 1, "{sends:?}");
    assert_eq!(sends[0]["dataset"], "tank/src/child", "{sends:?}");
    assert_eq!(sends[0]["from"], Value::Null, "{sends:?}");
    assert_eq!(sends[0]["recursive"], "true", "{sends:?}");
}

#[tokio::test]
async fn a_complete_recursive_replica_is_up_to_date() {
    let mock = MockAgent::with_pair();
    mock.set_snapshots("tank/src", &[("base", "g0"), ("snap", "g1")]);
    mock.set_snapshots("backup/dst", &[("base", "g0"), ("snap", "g1")]);
    mock.set_snapshots("tank/src/child", &[("snap", "g1-child")]);
    mock.set_snapshots("backup/dst/child", &[("snap", "g1-child")]);
    mock.set_paths("tank/src", &["tank/src@snap", "tank/src/child@snap"]);
    mock.set_paths("backup/dst", &["backup/dst@snap", "backup/dst/child@snap"]);
    let base = spawn_mock(mock).await;
    let state = state_with_agent(&base);

    let (status, body) = call(
        state.clone(),
        "POST",
        "/api/replication/send",
        Some(json!({
            "source": "tank/src",
            "destination": "backup/dst",
            "snapshot": "snap",
            "recursive": true
        })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let job = wait_job(state, body["jobId"].as_str().unwrap()).await;
    assert_eq!(job["state"], "succeeded", "{job}");
    assert_eq!(job["result"]["status"], "up-to-date", "{job}");
}

#[tokio::test]
async fn a_child_snapshot_with_a_different_guid_is_not_up_to_date() {
    let mock = MockAgent::with_pair();
    mock.set_snapshots("tank/src", &[("snap", "g1")]);
    mock.set_snapshots("backup/dst", &[("snap", "g1")]);
    mock.set_snapshots("tank/src/child", &[("snap", "g1-child")]);
    mock.set_snapshots("backup/dst/child", &[("snap", "g-DIFFERENT")]);
    mock.datasets.lock().unwrap().extend([
        json!({ "name": "tank/src/child" }),
        json!({ "name": "backup/dst/child" }),
    ]);
    mock.set_paths("tank/src", &["tank/src@snap", "tank/src/child@snap"]);
    mock.set_paths("backup/dst", &["backup/dst@snap", "backup/dst/child@snap"]);
    mock.set_paths("backup/dst/child", &["backup/dst/child@snap"]);
    let base = spawn_mock(mock.clone()).await;
    let state = state_with_agent(&base);

    let (status, body) = call(
        state.clone(),
        "POST",
        "/api/replication/send",
        Some(json!({
            "source": "tank/src",
            "destination": "backup/dst",
            "snapshot": "snap",
            "recursive": true
        })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let job = wait_job(state, body["jobId"].as_str().unwrap()).await;
    assert_eq!(job["state"], "failed", "{job}");
    let error = job["error"].as_str().unwrap_or_default();
    // The mismatched child snapshot does not carry this pair's prefix, so the
    // full-send gate refuses to replace it (FR-REP-04).
    assert!(error.contains("replication did not create"), "{error}");
    assert!(mock.sends.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_stale_child_replica_is_replaced_only_after_the_full_send_gate() {
    let mock = MockAgent::with_pair();
    let prefix = naslos_api::server::replication::snapshot_prefix("tank/src", "backup/dst");
    mock.set_snapshots("tank/src", &[("snap", "g1")]);
    mock.set_snapshots("backup/dst", &[("snap", "g1")]);
    mock.set_snapshots("tank/src/child", &[("snap", "g1-child")]);
    // The child exists but holds only this pair's own old snapshot, so it is
    // safe to replace with -F.
    mock.set_snapshots("backup/dst/child", &[(&format!("{prefix}old"), "g-old")]);
    mock.datasets.lock().unwrap().extend([
        json!({ "name": "tank/src/child" }),
        json!({ "name": "backup/dst/child" }),
    ]);
    mock.set_paths("tank/src", &["tank/src@snap", "tank/src/child@snap"]);
    mock.set_paths(
        "backup/dst",
        &["backup/dst@snap", &format!("backup/dst/child@{prefix}old")],
    );
    mock.set_paths(
        "backup/dst/child",
        &[&format!("backup/dst/child@{prefix}old")],
    );
    let base = spawn_mock(mock.clone()).await;
    let state = state_with_agent(&base);

    let (status, body) = call(
        state.clone(),
        "POST",
        "/api/replication/send",
        Some(json!({
            "source": "tank/src",
            "destination": "backup/dst",
            "snapshot": "snap",
            "recursive": true
        })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let job = wait_job(state, body["jobId"].as_str().unwrap()).await;
    assert_eq!(job["state"], "succeeded", "{job}");
    let sends = mock.sends.lock().unwrap().clone();
    assert_eq!(sends.len(), 1, "{sends:?}");
    assert_eq!(sends[0]["dataset"], "tank/src/child", "{sends:?}");
    let receives = mock.receives.lock().unwrap().clone();
    assert_eq!(receives.len(), 1, "{receives:?}");
    assert_eq!(receives[0]["dataset"], "backup/dst/child", "{receives:?}");
    assert_eq!(receives[0]["force"], "true", "{receives:?}");
}

#[tokio::test]
async fn retention_destroys_only_replication_owned_snapshots() {
    let mock = MockAgent::with_pair();
    let prefix = naslos_api::server::replication::snapshot_prefix("tank/src", "backup/dst");
    let old = format!("{prefix}20260101T000000Z");
    let newer = format!("{prefix}20260102T000000Z");
    mock.set_snapshots("tank/src", &[("base", "g1"), ("next", "g2")]);
    mock.set_snapshots("backup/dst", &[("dest-base", "g1")]);
    mock.set_paths(
        "backup/dst",
        &[
            &format!("backup/dst@{old}"),
            "backup/dst@user-snapshot",
            &format!("backup/dst@{newer}"),
        ],
    );
    let base = spawn_mock(mock.clone()).await;
    let state = state_with_agent(&base);

    let (status, body) = call(
        state.clone(),
        "POST",
        "/api/replication/send",
        Some(json!({
            "source": "tank/src",
            "destination": "backup/dst",
            "snapshot": "next",
            "pruneKeep": 1
        })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let job = wait_job(state, body["jobId"].as_str().unwrap()).await;
    assert_eq!(job["state"], "succeeded", "{job}");
    assert_eq!(job["result"]["prunedSnapshots"], 1);
    assert_eq!(
        mock.deleted.lock().unwrap().clone(),
        vec![format!("backup/dst@{old}")]
    );
}

#[tokio::test]
async fn a_second_job_for_the_destination_conflicts_and_cancels() {
    let mut mock = MockAgent::with_pair();
    mock.send_delay = Duration::from_secs(5);
    let base = spawn_mock(mock).await;
    let state = state_with_agent(&base);

    let (status, body) = call(
        state.clone(),
        "POST",
        "/api/replication/send",
        Some(json!({ "source": "tank/src", "destination": "backup/dst" })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let id = body["jobId"].as_str().unwrap().to_string();

    let (status, body) = call(
        state.clone(),
        "POST",
        "/api/replication/send",
        Some(json!({ "source": "tank/src", "destination": "backup/dst/child" })),
    )
    .await;
    // The nested destination fails validation first; an exact duplicate hits
    // the running-job conflict.
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (status, body) = call(
        state.clone(),
        "POST",
        "/api/replication/send",
        Some(json!({ "source": "tank/src", "destination": "backup/dst" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    let (status, body) = call(
        state.clone(),
        "DELETE",
        &format!("/api/replication/jobs/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let job = wait_job(state.clone(), &id).await;
    assert_eq!(job["state"], "cancelled");

    let (status, _body) = call(
        state,
        "DELETE",
        &format!("/api/replication/jobs/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn jobs_are_listed_and_unknown_ids_are_404() {
    let mock = MockAgent::with_pair();
    let state = state_with_agent(&spawn_mock(mock).await);

    let (status, body) = call(state.clone(), "GET", "/api/replication/jobs", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["jobs"], json!([]));

    let (status, _body) = call(state, "GET", "/api/replication/jobs/does-not-exist", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// The public planner is pure; this keeps the contract test honest about the
/// GUID match living in the production module.
#[test]
fn planner_matches_by_guid_not_name() {
    let src = vec![
        SnapshotInfo {
            dataset: String::new(),
            name: "a".into(),
            guid: "g1".into(),
            created: String::new(),
        },
        SnapshotInfo {
            dataset: String::new(),
            name: "b".into(),
            guid: "g2".into(),
            created: String::new(),
        },
    ];
    let dst = vec![SnapshotInfo {
        dataset: String::new(),
        name: "renamed".into(),
        guid: "g1".into(),
        created: String::new(),
    }];
    assert_eq!(
        naslos_api::server::replication::plan_send(&src, &dst, "b").unwrap(),
        naslos_api::server::replication::SendPlan::Incremental {
            base: src[0].clone()
        }
    );
}

fn encrypted(name: &str, root: &str) -> Value {
    json!({
        "name": name,
        "encryption": "aes-256-gcm",
        "encryptionRoot": root,
        "keystatus": "unavailable"
    })
}

#[tokio::test]
async fn an_encrypted_source_is_refused_before_streaming_without_a_key_client() {
    let mock = MockAgent::default();
    mock.datasets.lock().unwrap().extend([
        encrypted("tank/src", "tank/src"),
        json!({ "name": "backup" }),
    ]);
    let base = spawn_mock(mock.clone()).await;
    let state = state_with_agent(&base);

    let (status, body) = call(
        state,
        "POST",
        "/api/replication/send",
        Some(json!({ "source": "tank/src", "destination": "backup/replica" })),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("Kubernetes client"),
        "{body}"
    );
    assert!(mock.sends.lock().unwrap().is_empty(), "no send may start");
    assert!(
        mock.receives.lock().unwrap().is_empty(),
        "no receive may start"
    );
}

#[tokio::test]
async fn a_plain_source_never_replicates_into_an_encrypted_destination() {
    let mock = MockAgent::default();
    mock.datasets.lock().unwrap().extend([
        json!({ "name": "tank/src" }),
        encrypted("backup/dst", "backup/dst"),
    ]);
    let base = spawn_mock(mock.clone()).await;
    let state = state_with_agent(&base);

    let (status, body) = call(
        state,
        "POST",
        "/api/replication/send",
        Some(json!({ "source": "tank/src", "destination": "backup/dst" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body["error"].as_str().unwrap().contains("never mixed"));
    assert!(mock.sends.lock().unwrap().is_empty());
    assert!(mock.receives.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_recursive_encrypted_source_with_two_roots_is_refused() {
    let mock = MockAgent::default();
    mock.datasets.lock().unwrap().extend([
        encrypted("tank/src", "tank/src"),
        encrypted("tank/src/child", "tank/src/child"),
        json!({ "name": "backup" }),
    ]);
    let base = spawn_mock(mock.clone()).await;
    let state = state_with_agent(&base);

    let (status, body) = call(
        state,
        "POST",
        "/api/replication/send",
        Some(json!({
            "source": "tank/src",
            "destination": "backup/replica",
            "recursive": true
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("one encryption root"));
    assert!(mock.sends.lock().unwrap().is_empty());
}

fn kubectl_stub(dir: &std::path::Path, secrets: &[(&str, Value)]) -> std::path::PathBuf {
    let mut cases = String::new();
    for (name, value) in secrets {
        cases.push_str(&format!(
            "    {name})\n      cat <<'JSON'\n{value}\nJSON\n      exit 0\n      ;;\n"
        ));
    }
    let script = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = get ]; then\n\
           case \"$3\" in\n\
         {cases}    *) exit 0 ;;\n\
           esac\n\
         fi\n\
         cat >/dev/null 2>&1\n\
         echo '{{}}'\n\
         exit 0\n"
    );
    let path = dir.join("kubectl");
    std::fs::write(&path, script).unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    path
}

static KUBECTL_LOCK: Mutex<()> = Mutex::new(());

/// End-to-end raw-new path with a stub `kubectl`: the API reads the source key
/// from a Secret, writes the replica Secret, and hands the same key to the
/// agent's receive header (which the agent loads before mounting).
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn a_raw_new_job_writes_the_replica_key_and_loads_it_before_mounting() {
    let _guard = KUBECTL_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let key = [7u8; 32];
    let key_hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
    let key_b64 = naslos_api::dataset_encryption::RawKey::from_slice(&key)
        .unwrap()
        .to_base64();
    let secret = json!({
        "metadata": { "annotations": { "naslos.local/dataset": "tank/src" } },
        "data": { "key": key_b64 }
    });

    let source_secret =
        naslos_api::dataset_encryption::secret_name_for_dataset("tank/src").unwrap();
    let dir = std::env::temp_dir().join(format!("naslos-kubectl-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let stub = kubectl_stub(&dir, &[(&source_secret, secret)]);

    let previous = std::env::var_os("KUBECTL_BIN");
    std::env::set_var("KUBECTL_BIN", &stub);

    let mock = MockAgent::default();
    mock.datasets.lock().unwrap().extend([
        encrypted("tank/src", "tank/src"),
        json!({ "name": "backup" }),
    ]);
    let base = spawn_mock(mock.clone()).await;
    let kube = Arc::new(naslos_api::kube::Client::new("naslos"));
    let state = state_with_agent_and_kube(&base, Some(kube));

    let (status, body) = call(
        state.clone(),
        "POST",
        "/api/replication/send",
        Some(json!({ "source": "tank/src", "destination": "backup/replica" })),
    )
    .await;

    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let id = body["jobId"].as_str().unwrap().to_string();
    // The job creates the Secret after the response, so the stub must outlive it.
    let job = wait_job(state, &id).await;

    match previous {
        Some(value) => std::env::set_var("KUBECTL_BIN", value),
        None => std::env::remove_var("KUBECTL_BIN"),
    }
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(job["state"], "succeeded", "{job}");
    assert_eq!(job["raw"], json!(true), "{job}");

    let receives = mock.receives.lock().unwrap();
    assert_eq!(receives.len(), 1, "{receives:?}");
    assert_eq!(receives[0]["raw"], json!("true"), "{receives:?}");
    assert_eq!(receives[0]["unlock"], json!(key_hex), "{receives:?}");
}

/// A leftover replica-key Secret is refused, never overwritten: it could belong
/// to a dataset another actor created at the same name.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn a_raw_new_destination_with_an_existing_key_secret_is_refused() {
    let _guard = KUBECTL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let key = [3u8; 32];
    let key_b64 = naslos_api::dataset_encryption::RawKey::from_slice(&key)
        .unwrap()
        .to_base64();
    let source_secret =
        naslos_api::dataset_encryption::secret_name_for_dataset("tank/src").unwrap();
    let replica_secret =
        naslos_api::dataset_encryption::secret_name_for_dataset("backup/replica").unwrap();
    let source_value = json!({
        "metadata": { "annotations": { "naslos.local/dataset": "tank/src" } },
        "data": { "key": key_b64 }
    });
    let other_key = naslos_api::dataset_encryption::RawKey::from_slice(&[9u8; 32])
        .unwrap()
        .to_base64();
    let replica_value = json!({
        "metadata": { "annotations": { "naslos.local/dataset": "backup/replica" } },
        "data": { "key": other_key }
    });

    let dir = std::env::temp_dir().join(format!("naslos-kubectl-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let stub = kubectl_stub(
        &dir,
        &[
            (&source_secret, source_value),
            (&replica_secret, replica_value),
        ],
    );
    let previous = std::env::var_os("KUBECTL_BIN");
    std::env::set_var("KUBECTL_BIN", &stub);

    let mock = MockAgent::default();
    mock.datasets.lock().unwrap().extend([
        encrypted("tank/src", "tank/src"),
        json!({ "name": "backup" }),
    ]);
    let base = spawn_mock(mock.clone()).await;
    let kube = Arc::new(naslos_api::kube::Client::new("naslos"));
    let state = state_with_agent_and_kube(&base, Some(kube));

    let (status, body) = call(
        state,
        "POST",
        "/api/replication/send",
        Some(json!({ "source": "tank/src", "destination": "backup/replica" })),
    )
    .await;

    match previous {
        Some(value) => std::env::set_var("KUBECTL_BIN", value),
        None => std::env::remove_var("KUBECTL_BIN"),
    }
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("different key"),
        "{body}"
    );
    assert!(mock.sends.lock().unwrap().is_empty(), "no send may start");
    assert!(
        mock.receives.lock().unwrap().is_empty(),
        "no receive may start"
    );
}

/// A leftover replica Secret holding the source key is reused, so a cancel or
/// API restart cannot deadlock the pair on its own key.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn a_matching_leftover_replica_secret_is_reused() {
    let _guard = KUBECTL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let key = [5u8; 32];
    let key_hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
    let key_b64 = naslos_api::dataset_encryption::RawKey::from_slice(&key)
        .unwrap()
        .to_base64();
    let source_secret =
        naslos_api::dataset_encryption::secret_name_for_dataset("tank/src").unwrap();
    let replica_secret =
        naslos_api::dataset_encryption::secret_name_for_dataset("backup/replica").unwrap();
    let source_value = json!({
        "metadata": { "annotations": { "naslos.local/dataset": "tank/src" } },
        "data": { "key": key_b64 }
    });
    let replica_value = json!({
        "metadata": { "annotations": { "naslos.local/dataset": "backup/replica" } },
        "data": { "key": key_b64 }
    });

    let dir = std::env::temp_dir().join(format!("naslos-kubectl-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let stub = kubectl_stub(
        &dir,
        &[
            (&source_secret, source_value),
            (&replica_secret, replica_value),
        ],
    );
    let previous = std::env::var_os("KUBECTL_BIN");
    std::env::set_var("KUBECTL_BIN", &stub);

    let mock = MockAgent::default();
    mock.datasets.lock().unwrap().extend([
        encrypted("tank/src", "tank/src"),
        json!({ "name": "backup" }),
    ]);
    let base = spawn_mock(mock.clone()).await;
    let kube = Arc::new(naslos_api::kube::Client::new("naslos"));
    let state = state_with_agent_and_kube(&base, Some(kube));

    let (status, body) = call(
        state.clone(),
        "POST",
        "/api/replication/send",
        Some(json!({ "source": "tank/src", "destination": "backup/replica" })),
    )
    .await;

    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let id = body["jobId"].as_str().unwrap().to_string();
    let job = wait_job(state, &id).await;

    match previous {
        Some(value) => std::env::set_var("KUBECTL_BIN", value),
        None => std::env::remove_var("KUBECTL_BIN"),
    }
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(job["state"], "succeeded", "{job}");
    let receives = mock.receives.lock().unwrap();
    assert_eq!(receives.len(), 1, "{receives:?}");
    assert_eq!(receives[0]["unlock"], json!(key_hex), "{receives:?}");
}

#[tokio::test]
async fn a_recursive_plain_source_never_streams_an_encrypted_volume() {
    let mock = MockAgent::default();
    mock.datasets.lock().unwrap().extend([
        json!({ "name": "tank/src" }),
        json!({ "name": "backup/dst" }),
    ]);
    mock.zvols.lock().unwrap().push(json!({
        "name": "tank/src/disk",
        "volsize": "1G",
        "encryption": "aes-256-gcm",
        "encryptionRoot": "tank/src/disk"
    }));
    let base = spawn_mock(mock.clone()).await;
    let state = state_with_agent(&base);

    let (status, body) = call(
        state,
        "POST",
        "/api/replication/send",
        Some(json!({
            "source": "tank/src",
            "destination": "backup/dst",
            "recursive": true
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("plaintext"),
        "{body}"
    );
    assert!(mock.sends.lock().unwrap().is_empty(), "no send may start");
    assert!(
        mock.receives.lock().unwrap().is_empty(),
        "no receive may start"
    );
}

#[tokio::test]
async fn a_raw_tree_with_a_foreign_volume_root_is_refused() {
    let mock = MockAgent::default();
    mock.datasets.lock().unwrap().extend([
        encrypted("tank/src", "tank/src"),
        json!({ "name": "backup" }),
    ]);
    mock.zvols.lock().unwrap().push(json!({
        "name": "tank/src/disk",
        "volsize": "1G",
        "encryption": "aes-256-gcm",
        "encryptionRoot": "tank/src/disk"
    }));
    let base = spawn_mock(mock.clone()).await;
    let state = state_with_agent(&base);

    let (status, body) = call(
        state,
        "POST",
        "/api/replication/send",
        Some(json!({
            "source": "tank/src",
            "destination": "backup/replica",
            "recursive": true
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("one encryption root"),
        "{body}"
    );
    assert!(mock.sends.lock().unwrap().is_empty());
}
