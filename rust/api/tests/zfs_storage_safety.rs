//! R-004/R-039/R-044/R-045 API regressions: dataset-destroy confirmation,
//! zvol/ownership refusals, GUID-bound destroy, confirmation forwarding for a
//! forced receive, and the no-deadline mutation client.

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use http_body_util::BodyExt;
use naslos_api::agent::Client as AgentClient;
use naslos_api::server::build_router;
use naslos_api::state::AppState;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tower::ServiceExt;

const SECRET: &str = "test-proxy-secret";

struct MockAgent {
    datasets: Value,
    zvols: Value,
    requests: Arc<Mutex<Vec<String>>>,
}

async fn spawn_agent(mock: MockAgent) -> String {
    let datasets = Arc::new(mock.datasets);
    let zvols = Arc::new(mock.zvols);
    let requests = mock.requests.clone();
    let app = Router::new()
        .route(
            "/api/v1/datasets",
            get({
                let datasets = datasets.clone();
                move || {
                    let datasets = datasets.clone();
                    async move { Json((*datasets).clone()) }
                }
            }),
        )
        .route(
            "/api/v1/zvols",
            get({
                let zvols = zvols.clone();
                move || {
                    let zvols = zvols.clone();
                    async move { Json((*zvols).clone()) }
                }
            }),
        )
        .route("/api/v1/pools", get(|| async { Json(json!([])) }))
        .route(
            "/api/v1/pools/import",
            post({
                let requests = requests.clone();
                move |req: Request<Body>| {
                    let requests = requests.clone();
                    async move {
                        let (parts, body) = req.into_parts();
                        let bytes = body.collect().await.unwrap().to_bytes();
                        requests.lock().unwrap().push(format!(
                            "{} {} | {}",
                            parts.method,
                            parts.uri,
                            String::from_utf8_lossy(&bytes)
                        ));
                        (
                            StatusCode::OK,
                            Json(json!({ "status": "pool imported", "name": "tank" })),
                        )
                    }
                }
            }),
        )
        .route(
            "/api/v1/datasets/{*rest}",
            delete({
                let requests = requests.clone();
                move |req: Request<Body>| {
                    let requests = requests.clone();
                    async move {
                        requests
                            .lock()
                            .unwrap()
                            .push(format!("{} {}", req.method(), req.uri()));
                        (
                            StatusCode::OK,
                            Json(json!({ "status": "dataset destroyed" })),
                        )
                    }
                }
            })
            .post({
                let requests = requests.clone();
                move |req: Request<Body>| {
                    let requests = requests.clone();
                    async move {
                        requests
                            .lock()
                            .unwrap()
                            .push(format!("{} {}", req.method(), req.uri()));
                        (StatusCode::CREATED, Json(json!({ "status": "created" })))
                    }
                }
            }),
        )
        .route(
            "/api/v1/zfs/receive/{*dataset}",
            post({
                let requests = requests.clone();
                move |req: Request<Body>| {
                    let requests = requests.clone();
                    async move {
                        requests
                            .lock()
                            .unwrap()
                            .push(format!("{} {}", req.method(), req.uri()));
                        (StatusCode::OK, Json(json!({ "status": "received" })))
                    }
                }
            }),
        )
        .route(
            "/api/v1/zfs/receive/",
            post(|| async { (StatusCode::BAD_REQUEST, Json(json!({ "error": "dataset" }))) }),
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
        AgentClient::new(base, "token", "").expect("agent client"),
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

fn dataset(name: &str, mountpoint: &str, guid: &str, encryption: &str) -> Value {
    json!({
        "name": name,
        "mountpoint": mountpoint,
        "guid": guid,
        "encryption": encryption,
        "encryptionRoot": if encryption == "off" { "" } else { name },
        "keystatus": if encryption == "off" { "none" } else { "available" },
    })
}

#[tokio::test]
async fn a_plain_dataset_destroy_still_requires_confirmation() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let base = spawn_agent(MockAgent {
        datasets: json!([dataset("tank/plain", "/var/mnt/tank/plain", "10", "off")]),
        zvols: json!([]),
        requests: requests.clone(),
    })
    .await;
    let (status, body) = call(
        state_with_agent(&base),
        "DELETE",
        "/api/datasets?name=tank/plain",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"].as_str().unwrap().contains("confirm"),
        "{body}"
    );
    assert!(
        requests.lock().unwrap().is_empty(),
        "no agent call before confirmation"
    );
}

#[tokio::test]
async fn a_zvol_is_only_deletable_through_the_zvol_route() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let base = spawn_agent(MockAgent {
        datasets: json!([]),
        zvols: json!([{ "name": "tank/vol" }]),
        requests: requests.clone(),
    })
    .await;
    let (status, body) = call(
        state_with_agent(&base),
        "DELETE",
        "/api/datasets?name=tank/vol&confirm=tank/vol",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        body["error"].as_str().unwrap().contains("/api/zvols"),
        "{body}"
    );
    assert!(
        !requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.starts_with("DELETE")),
        "a zvol must not reach the dataset destroy"
    );
}

#[tokio::test]
async fn a_confirmed_dataset_destroy_is_bound_to_the_listed_guid() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let base = spawn_agent(MockAgent {
        datasets: json!([dataset("tank/plain", "/var/mnt/tank/plain", "42", "off")]),
        zvols: json!([]),
        requests: requests.clone(),
    })
    .await;
    let (status, body) = call(
        state_with_agent(&base),
        "DELETE",
        "/api/datasets?name=tank/plain&confirm=tank/plain",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["guid"], "42");
    let requests = requests.lock().unwrap();
    assert!(
        requests
            .iter()
            .any(|r| r == "DELETE /api/v1/datasets/tank/plain?guid=42"),
        "{requests:?}"
    );
}

#[tokio::test]
async fn an_encrypted_subtree_destroy_reports_key_cleanup() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let base = spawn_agent(MockAgent {
        datasets: json!([dataset(
            "tank/secure",
            "/var/mnt/tank/secure",
            "77",
            "aes-256-gcm"
        )]),
        zvols: json!([]),
        requests: requests.clone(),
    })
    .await;
    let (status, body) = call(
        state_with_agent(&base),
        "DELETE",
        "/api/datasets?name=tank/secure&confirm=tank/secure",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // No Kubernetes client in the test state: the key could not be removed,
    // so cleanup must be reported as incomplete.
    assert_eq!(body["keysDeleted"], false);
    assert!(requests
        .lock()
        .unwrap()
        .iter()
        .any(|r| r == "DELETE /api/v1/datasets/tank/secure?guid=77"));
}

#[tokio::test]
async fn a_forced_import_needs_a_matching_confirmation() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let base = spawn_agent(MockAgent {
        datasets: json!([]),
        zvols: json!([]),
        requests: requests.clone(),
    })
    .await;
    let state = state_with_agent(&base);
    let (status, _body) = call(
        state.clone(),
        "POST",
        "/api/volumes/zfs/import",
        Some(json!({ "name": "tank", "force": true })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(requests.lock().unwrap().is_empty());

    let (status, _body) = call(
        state,
        "POST",
        "/api/volumes/zfs/import",
        Some(json!({ "name": "tank", "force": true, "confirm": "tank" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert!(requests[0].contains(r#""force":true"#), "{requests:?}");
    assert!(requests[0].contains(r#""confirm":"tank""#), "{requests:?}");
}

#[tokio::test]
async fn pool_create_refuses_encryption_before_the_agent() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let base = spawn_agent(MockAgent {
        datasets: json!([]),
        zvols: json!([]),
        requests: requests.clone(),
    })
    .await;
    let (status, body) = call(
        state_with_agent(&base),
        "POST",
        "/api/volumes/zfs",
        Some(json!({
            "name": "tank",
            "disks": ["/dev/sdb"],
            "options": { "encryption": "aes-256-gcm", "keyformat": "raw" }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"].as_str().unwrap().contains("create-only"),
        "{body}"
    );
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_forced_receive_forwards_the_confirmation_query() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let base = spawn_agent(MockAgent {
        datasets: json!([]),
        zvols: json!([]),
        requests: requests.clone(),
    })
    .await;
    let client = AgentClient::new(&base, "token", "").unwrap();
    client
        .receive_stream_body("tank/data", true, tokio::io::empty())
        .await
        .expect("receive");
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert!(
        requests[0].contains("force=true") && requests[0].contains("confirm=tank%2Fdata"),
        "{requests:?}"
    );
}

#[tokio::test]
async fn destructive_calls_ignore_the_generic_total_deadline() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new().route(
        "/api/v1/datasets/{*rest}",
        delete({
            let requests = requests.clone();
            move |req: Request<Body>| {
                let requests = requests.clone();
                async move {
                    requests
                        .lock()
                        .unwrap()
                        .push(format!("{} {}", req.method(), req.uri()));
                    // Longer than the 100 ms total deadline the client is
                    // configured with; a destructive call must not be killed.
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    (StatusCode::OK, Json(json!({ "status": "destroyed" })))
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = AgentClient::with_timeouts(
        &format!("http://{addr}"),
        "token",
        "",
        Duration::from_millis(100),
        Duration::from_secs(2),
    )
    .unwrap();
    client
        .destroy_dataset_with_guid("tank/data", true, Some("9"))
        .await
        .expect("the mutation client has no total deadline");
}
