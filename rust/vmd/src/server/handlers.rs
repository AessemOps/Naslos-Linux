//! vmd route handlers. Every route except `/health` is behind the token
//! check; a new route added to the API router is authenticated by default.

use super::error::{json_response, read_json, write_error, MAX_JSON_BODY_BYTES};
use super::AppState;
use crate::capabilities;
use crate::images;
use crate::model::{DesiredConfig, ImageEntry};
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;

pub async fn health() -> Response {
    json_response(StatusCode::OK, &serde_json::json!({ "status": "ok" }))
}

pub async fn capabilities_get() -> Response {
    // detect() touches /sys and runs a probe `ip` call: keep it off the
    // async workers with a blocking-pool hop.
    let caps = tokio::task::spawn_blocking(capabilities::detect)
        .await
        .unwrap_or_default();
    json_response(StatusCode::OK, &caps)
}

pub async fn vms_get(State(state): State<Arc<AppState>>) -> Response {
    let live = state.supervisor.live().await;
    json_response(StatusCode::OK, &live)
}

pub async fn vms_config_put(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let config: DesiredConfig = match read_json(body, MAX_JSON_BODY_BYTES).await {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match state.supervisor.apply(config).await {
        Ok(()) => json_response(StatusCode::OK, &serde_json::json!({ "status": "applied" })),
        Err(e) => write_error(StatusCode::CONFLICT, e),
    }
}

pub async fn vm_force_stop_post(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    match state.supervisor.force_stop(&name).await {
        Ok(()) => json_response(
            StatusCode::OK,
            &serde_json::json!({ "status": "stopped", "name": name }),
        ),
        Err(e) => write_error(StatusCode::BAD_REQUEST, e),
    }
}

/// One image library listing, with in-flight imports and their last errors.
#[derive(Serialize)]
pub struct ImagesList {
    pub images: Vec<ImageEntry>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub importing: Vec<String>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub errors: HashMap<String, String>,
}

pub async fn images_get(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let pool = query.get("pool").cloned().unwrap_or_default();
    let list = images::list(&pool).await;
    let registry = state.imports.lock().await;
    let importing: Vec<String> = registry.active.iter().cloned().collect();
    let errors = registry.errors.clone();
    json_response(
        StatusCode::OK,
        &ImagesList {
            images: list,
            importing,
            errors,
        },
    )
}

pub async fn image_upload_post(
    Query(query): Query<HashMap<String, String>>,
    body: Body,
) -> Response {
    let pool = query.get("pool").cloned().unwrap_or_default();
    let name = query.get("name").cloned().unwrap_or_default();
    let offset = match query.get("offset").map(|v| v.trim()) {
        None => 0u64,
        Some(raw) => match raw.parse::<u64>() {
            Ok(v) => v,
            Err(_) => {
                return write_error(
                    StatusCode::BAD_REQUEST,
                    format!("offset {raw:?} is not a number"),
                )
            }
        },
    };
    let final_chunk = query
        .get("final")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);
    match images::write_chunk(&pool, &name, offset, final_chunk, body).await {
        Ok(size) => json_response(
            StatusCode::OK,
            &serde_json::json!({ "name": name, "sizeBytes": size }),
        ),
        Err(e) => write_error(StatusCode::BAD_REQUEST, e),
    }
}

#[derive(serde::Deserialize, Default)]
pub struct ImportRequest {
    #[serde(default)]
    pub pool: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub sha256: Option<String>,
}

pub async fn image_import_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let req: ImportRequest = match read_json(body, MAX_JSON_BODY_BYTES).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    if !crate::model::valid_pool_component(&req.pool) {
        return write_error(StatusCode::BAD_REQUEST, "a valid pool is required");
    }
    // One import per target name; a second concurrent import of the same
    // file would interleave writes.
    {
        let mut registry = state.imports.lock().await;
        if registry.active.contains(&req.name) {
            return write_error(
                StatusCode::CONFLICT,
                format!("{:?} is already being imported", req.name),
            );
        }
        registry.active.insert(req.name.clone());
    }
    let imports = state.imports.clone();
    let pool = req.pool.clone();
    let name = req.name.clone();
    let url = req.url.clone();
    let sha256 = req.sha256.clone();
    tokio::spawn(async move {
        let outcome = images::import_url(&pool, &name, &url, sha256.as_deref()).await;
        let mut registry = imports.lock().await;
        registry.active.remove(&name);
        match outcome {
            Ok(size) => {
                tracing::info!("imported {name} ({size} bytes) into pool {pool}");
                registry.errors.remove(&name);
            }
            Err(e) => {
                tracing::warn!("importing {name} from {url}: {e}");
                registry.errors.insert(name.clone(), e);
            }
        }
    });
    json_response(
        StatusCode::ACCEPTED,
        &serde_json::json!({ "status": "import started", "name": req.name }),
    )
}

pub async fn image_delete(
    State(_state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
    Path(name): Path<String>,
) -> Response {
    let pool = query.get("pool").cloned().unwrap_or_default();
    match images::delete(&pool, &name).await {
        Ok(()) => json_response(
            StatusCode::OK,
            &serde_json::json!({ "status": "deleted", "name": name }),
        ),
        Err(e) => write_error(StatusCode::BAD_REQUEST, e),
    }
}
