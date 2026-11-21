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
                |State(m): State<MockAgent>, Path(dataset): Path<String>| async move {
                    let list = m
                        .snaps
                        .lock()
                        .unwrap()
                        .get(&dataset)
                        .cloned()
                        .unwrap_or_default();
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
            "/api/v1/zfs/send/{*dataset}",
            get(
                |State(m): State<MockAgent>,
                 Path(_dataset): Path<String>,
                 Query(q): Query<HashMap<String, String>>| async move {
                    if q.get("estimate").map(String::as_str) == Some("true") {
                        return (
                            StatusCode::OK,
                            Body::from(json!({ "bytes": m.send_body.len() as i64 }).to_string()),
                        );
                    }
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
                |State(_m): State<MockAgent>, Path(_dataset): Path<String>, body: Body| async move {
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
    let agent = AgentClient::new(base_url, "token", "").unwrap();
    Arc::new(AppState {
        proxy_secret: SECRET.to_string(),
        trusted_cidrs: vec![Cidr::parse("0.0.0.0/0").unwrap()],
        metrics: Manager::new(),
        talos: None,
        agent: Some(Arc::new(agent)),
        vmd: None,
        vms: Arc::new(naslos_api::virt::Store::new("")),
        shares: Mutex::new(SharesManager::new("", "/var/mnt")),
        samba_users: Mutex::new(SambaUserStore::new("")),
        identity: None,
        charts: None,
        catalog: Arc::new(naslos_api::state::CatalogHolder::new()),
        app_manager: None,
        app_jobs: Arc::new(naslos_api::server::app_jobs::AppJobManager::new()),
        base_domain: String::new(),
        providers: Arc::new(naslos_api::providers::Registry::load("")),
        domains: Arc::new(naslos_api::certs::Store::new("")),
        certs: None,
        sso: None,
        kube: None,
        sso_domains: Vec::new(),
        apps_namespace: "naslos-apps".to_string(),
        ddns: None,
        notifications: Arc::new(naslos_api::notifications::Manager::new("")),
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
            name: "a".into(),
            guid: "g1".into(),
            created: String::new(),
        },
        SnapshotInfo {
            name: "b".into(),
            guid: "g2".into(),
            created: String::new(),
        },
    ];
    let dst = vec![SnapshotInfo {
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
