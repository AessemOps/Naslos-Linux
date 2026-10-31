use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;

use crate::datasets::{normalize_host_path, ManagedDataset};
use crate::json;
use crate::state::AppState;

fn unavailable() -> Response {
    json(
        StatusCode::SERVICE_UNAVAILABLE,
        serde_json::json!({ "error": "the dataset registry requires the agent" }),
    )
}

pub async fn list(State(state): State<Arc<AppState>>) -> Response {
    json(
        StatusCode::OK,
        serde_json::json!(state.datasets_registry.list()),
    )
}

pub async fn create(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let mut record: ManagedDataset = match super::zfs::read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    record.created_at = None;
    let normalized = match normalize_host_path(&record.host_path) {
        Ok(p) => p,
        Err(e) => return json(StatusCode::BAD_REQUEST, serde_json::json!({ "error": e })),
    };
    let Some(agent) = &state.agent else {
        return unavailable();
    };
    let datasets = match agent.list_datasets().await {
        Ok(d) => d,
        Err(e) => {
            return json(
                StatusCode::BAD_GATEWAY,
                serde_json::json!({ "error": format!("listing datasets from the agent: {e}") }),
            )
        }
    };
    if !datasets
        .iter()
        .any(|d| normalize_host_path(&d.mountpoint).ok().as_deref() == Some(normalized.as_str()))
    {
        return json(
            StatusCode::BAD_REQUEST,
            serde_json::json!({
                "error": format!("{normalized:?} is not a mounted ZFS dataset")
            }),
        );
    }
    match state.datasets_registry.add(record) {
        Ok(record) => json(StatusCode::CREATED, serde_json::json!(record)),
        Err(e) => json(StatusCode::BAD_REQUEST, serde_json::json!({ "error": e })),
    }
}

pub async fn delete(State(state): State<Arc<AppState>>, Path(name): Path<String>) -> Response {
    if let Some(dataset) = state.datasets_registry.get(&name) {
        if !dataset.attached_to.is_empty() {
            return json(
                StatusCode::CONFLICT,
                serde_json::json!({
                    "error": format!(
                        "dataset {name:?} is attached to {}",
                        dataset.attached_to.join(", ")
                    )
                }),
            );
        }
    }
    if let Some(manager) = &state.app_manager {
        if let Some(app) = manager
            .records()
            .into_iter()
            .find(|r| r.datasets.iter().any(|d| d == &name))
        {
            return json(
                StatusCode::CONFLICT,
                serde_json::json!({
                    "error": format!("dataset {name:?} is attached to app {}", app.name)
                }),
            );
        }
    }
    match state.datasets_registry.remove(&name) {
        Ok(()) => json(
            StatusCode::OK,
            serde_json::json!({ "status": "dataset unregistered", "name": name }),
        ),
        Err(e) => json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e })),
    }
}
