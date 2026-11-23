//! vmd route handlers. Every route except `/health` is behind the token
//! check; a new route added to the API router is authenticated by default.

use super::error::{json_response, read_json, write_error, MAX_JSON_BODY_BYTES};
use super::AppState;
use crate::capabilities;
use crate::images;
use crate::model::{DesiredConfig, ImageEntry};
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use futures::{SinkExt, StreamExt};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
    // apply installs the config synchronously (validation errors are 409s)
    // and runs the reconcile detached: the reconciler must survive this
    // request's lifetime (a client timeout cancels the handler future).
    match Arc::clone(&state.supervisor).apply(config).await {
        Ok(()) => json_response(StatusCode::OK, &serde_json::json!({ "status": "applied" })),
        Err(e) => write_error(StatusCode::CONFLICT, e),
    }
}

/// Authenticated console tunnel: the API bridges browser RFB bytes here
/// instead of dialing QEMU's port directly (QEMU VNC binds loopback only).
/// The target is a port inside the configured VNC display range; the bearer
/// token is enforced by the router-wide auth middleware.
pub async fn vm_vnc_tunnel(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    ws: WebSocketUpgrade,
) -> Response {
    let Some(raw) = query.get("port") else {
        return write_error(
            StatusCode::BAD_REQUEST,
            "a VNC port is required".to_string(),
        );
    };
    let Ok(port) = raw.parse::<u16>() else {
        return write_error(
            StatusCode::BAD_REQUEST,
            format!("VNC port {raw:?} is not a valid TCP port"),
        );
    };
    if !state.supervisor.vnc_port_in_range(port) {
        return write_error(
            StatusCode::FORBIDDEN,
            format!("port {port} is outside the configured VNC display range"),
        );
    }
    // The name must own the port. Without this a valid API token could proxy
    // any other VM's console (or any loopback service in the display range)
    // by asking for a different VM name.
    if state.supervisor.vnc_port_for(&name).await != Some(port) {
        return write_error(
            StatusCode::NOT_FOUND,
            format!("VM {name:?} is not listening on port {port}"),
        );
    }
    ws.on_upgrade(move |socket| tunnel_vnc(socket, name, port))
}

async fn tunnel_vnc(socket: WebSocket, name: String, port: u16) {
    let stream = match tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::net::TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    {
        Ok(Ok(stream)) => stream,
        _ => {
            tracing::warn!("console tunnel for {name}: cannot reach 127.0.0.1:{port}");
            let mut socket = socket;
            let _ = socket.close().await;
            return;
        }
    };
    let (mut tcp_read, mut tcp_write) = stream.into_split();
    let (mut ws_tx, mut ws_rx) = socket.split();

    let browser_to_vnc = async {
        while let Some(Ok(message)) = ws_rx.next().await {
            match message {
                Message::Binary(data) => {
                    if tcp_write.write_all(&data).await.is_err() {
                        break;
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
        let _ = tcp_write.shutdown().await;
    };
    let vnc_to_browser = async {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            match tcp_read.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    if ws_tx
                        .send(Message::Binary(buf[..n].to_vec().into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    };
    tokio::select! {
        _ = browser_to_vnc => {}
        _ = vnc_to_browser => {}
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
    if !crate::model::valid_pool_component(&pool) {
        return write_error(
            StatusCode::BAD_REQUEST,
            format!("pool {pool:?} is not a valid dataset component"),
        );
    }
    let list = images::list(&state.datasets_root, &pool).await;
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
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
    body: Body,
) -> Response {
    let pool = query.get("pool").cloned().unwrap_or_default();
    let name = query.get("name").cloned().unwrap_or_default();
    if !crate::model::valid_pool_component(&pool) {
        return write_error(
            StatusCode::BAD_REQUEST,
            format!("pool {pool:?} is not a valid dataset component"),
        );
    }
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
    // An explicit `confirm=<name>` is required to replace an existing image;
    // the same explicit confirmation is what the UI sends for a re-upload.
    let confirm_overwrite = query.get("confirm").map(String::as_str) == Some(name.as_str());
    // The overwrite decision (first chunk) and the rename (final chunk) both
    // check attachment; a VM could start using the existing image while a
    // multi-chunk replacement is in flight.
    if offset == 0 || final_chunk {
        if let Some(owner) = state.supervisor.image_in_use(&pool, &name).await {
            return write_error(
                StatusCode::CONFLICT,
                format!("cannot replace {name:?}: {owner} attaches it"),
            );
        }
    }
    if offset == 0 {
        let registry = state.imports.lock().await;
        if registry.active.contains(&name) {
            return write_error(
                StatusCode::CONFLICT,
                format!("{name:?} is being imported; wait for the import to finish"),
            );
        }
    }
    match images::write_chunk(
        &state.datasets_root,
        &pool,
        &name,
        offset,
        final_chunk,
        confirm_overwrite,
        body,
    )
    .await
    {
        Ok(size) => json_response(
            StatusCode::OK,
            &serde_json::json!({ "name": name, "sizeBytes": size }),
        ),
        Err(e) => image_error(e),
    }
}

/// Map an image-library failure to the status the caller can act on.
fn image_error(message: String) -> Response {
    let status = if message.contains("library limit") {
        StatusCode::PAYLOAD_TOO_LARGE
    } else if message.contains("already exists")
        || message.contains("already in progress")
        || message.contains("no upload of")
        || message.contains("symlink")
        || message.contains("in progress")
    {
        StatusCode::CONFLICT
    } else {
        StatusCode::BAD_REQUEST
    };
    write_error(status, message)
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
    /// Must equal `name` to replace an existing library image.
    #[serde(default)]
    pub confirm: Option<String>,
}

pub async fn image_import_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let req: ImportRequest = match read_json(body, MAX_JSON_BODY_BYTES).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    if !crate::model::valid_pool_component(&req.pool) {
        return write_error(StatusCode::BAD_REQUEST, "a valid pool is required");
    }
    if let Some(owner) = state.supervisor.image_in_use(&req.pool, &req.name).await {
        return write_error(
            StatusCode::CONFLICT,
            format!("cannot replace {:?}: {owner} attaches it", req.name),
        );
    }
    if images::upload_in_progress(&state.datasets_root, &req.pool, &req.name) {
        return write_error(
            StatusCode::CONFLICT,
            format!(
                "{:?} is being uploaded; wait for the upload to finish",
                req.name
            ),
        );
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
    let confirm_overwrite = req.confirm.as_deref() == Some(req.name.as_str());
    let datasets_root = state.datasets_root.clone();
    let supervisor = state.supervisor.clone();
    let check_pool = pool.clone();
    let check_name = name.clone();
    tokio::spawn(async move {
        // A long download can outlive the initial attachment check: re-check
        // right before the staged file replaces the library entry.
        let before_publish = move || {
            let supervisor = supervisor.clone();
            let pool = check_pool.clone();
            let name = check_name.clone();
            async move {
                match supervisor.image_in_use(&pool, &name).await {
                    Some(owner) => Err(format!("cannot replace {name:?}: {owner} attaches it")),
                    None => Ok(()),
                }
            }
        };
        let outcome = images::import_url(
            &datasets_root,
            &pool,
            &name,
            &url,
            sha256.as_deref(),
            confirm_overwrite,
            before_publish,
        )
        .await;
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
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
    Path(name): Path<String>,
) -> Response {
    let pool = query.get("pool").cloned().unwrap_or_default();
    if !crate::model::valid_pool_component(&pool) {
        return write_error(
            StatusCode::BAD_REQUEST,
            format!("pool {pool:?} is not a valid dataset component"),
        );
    }
    if let Some(owner) = state.supervisor.image_in_use(&pool, &name).await {
        return write_error(
            StatusCode::CONFLICT,
            format!("cannot delete {name:?}: {owner} attaches it"),
        );
    }
    match images::delete(&state.datasets_root, &pool, &name).await {
        Ok(()) => json_response(
            StatusCode::OK,
            &serde_json::json!({ "status": "deleted", "name": name }),
        ),
        Err(e) => image_error(e),
    }
}
