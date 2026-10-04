//! HTTP handlers (port of the Go `internal/server` handlers).

use super::error::{
    json_response, read_json, write_client_error, write_error, MAX_JSON_BODY_BYTES,
    MAX_STREAM_BODY_BYTES,
};
use super::AppState;
use crate::shares::Config;
use crate::zfs::validation::ZfsError;
use crate::zfs::{validate_pool_name, PoolConfig, SendStreamOptions};
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::Response;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

type QueryMap = HashMap<String, String>;

fn zfs_unavailable() -> Response {
    write_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "ZFS is not available on this node (agent running in degraded mode)",
    )
}

fn backup_unavailable() -> Response {
    write_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "ZFS streaming is not available on this node (agent running in degraded mode)",
    )
}

fn shares_unavailable(what: &str) -> Response {
    write_error(
        StatusCode::SERVICE_UNAVAILABLE,
        format!("{what} is not available on this node (agent running in degraded mode)"),
    )
}

pub async fn health() -> Response {
    json_response(StatusCode::OK, &json!({ "status": "ok" }))
}

// ---- Pools ---------------------------------------------------------------

pub async fn pools_get(State(state): State<Arc<AppState>>) -> Response {
    let Some(zfs) = state.zfs.as_ref() else {
        return zfs_unavailable();
    };
    match zfs.pools().await {
        Ok(pools) => json_response(StatusCode::OK, &pools),
        Err(e) => write_client_error(&e),
    }
}

pub async fn pools_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let Some(zfs) = state.zfs.as_ref() else {
        return zfs_unavailable();
    };
    let cfg: PoolConfig = match read_json(body, MAX_JSON_BODY_BYTES).await {
        Ok(cfg) => cfg,
        Err(resp) => return resp,
    };
    match zfs.create_pool(&cfg).await {
        Ok(()) => json_response(
            StatusCode::CREATED,
            &json!({ "status": "pool created", "name": cfg.name }),
        ),
        Err(e) => write_client_error(&e),
    }
}

pub async fn pool_import_get(State(state): State<Arc<AppState>>) -> Response {
    let Some(zfs) = state.zfs.as_ref() else {
        return zfs_unavailable();
    };
    match zfs.list_importable().await {
        Ok(pools) => json_response(StatusCode::OK, &pools),
        Err(e) => write_client_error(&e),
    }
}

#[derive(Default, Deserialize)]
struct ImportRequest {
    #[serde(default)]
    name: String,
}

pub async fn pool_import_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let Some(zfs) = state.zfs.as_ref() else {
        return zfs_unavailable();
    };
    // The Go handler ignores decode errors, so an empty/garbage body imports all.
    let req: ImportRequest = read_json(body, MAX_JSON_BODY_BYTES)
        .await
        .unwrap_or_default();
    match zfs.import_pool(&req.name).await {
        Ok(()) => {
            if req.name.is_empty() {
                json_response(StatusCode::OK, &json!({ "status": "all pools imported" }))
            } else {
                json_response(
                    StatusCode::OK,
                    &json!({ "status": "pool imported", "name": req.name }),
                )
            }
        }
        Err(e) => write_client_error(&e),
    }
}

pub async fn pool_detail_get(
    State(state): State<Arc<AppState>>,
    Path(pool): Path<String>,
) -> Response {
    let Some(zfs) = state.zfs.as_ref() else {
        return zfs_unavailable();
    };
    match zfs.pool_health(&pool).await {
        Ok(health) => json_response(StatusCode::OK, &health),
        Err(e) => write_client_error(&e),
    }
}

pub async fn pool_detail_delete(
    State(state): State<Arc<AppState>>,
    Path(pool): Path<String>,
) -> Response {
    let Some(zfs) = state.zfs.as_ref() else {
        return zfs_unavailable();
    };
    match zfs.destroy_pool(&pool).await {
        Ok(()) => json_response(
            StatusCode::OK,
            &json!({ "status": "pool destroyed", "name": pool }),
        ),
        Err(e) => write_client_error(&e),
    }
}

#[derive(Default, Deserialize)]
struct PoolDevicesRequest {
    #[serde(default)]
    disks: Vec<String>,
    #[serde(default)]
    topology: String,
    #[serde(default)]
    force: bool,
}

pub async fn pool_devices_get(
    State(state): State<Arc<AppState>>,
    Path(pool): Path<String>,
) -> Response {
    let Some(zfs) = state.zfs.as_ref() else {
        return zfs_unavailable();
    };
    if let Err(e) = validate_pool_name(&pool) {
        return write_error(StatusCode::BAD_REQUEST, e.to_string());
    }
    match zfs.free_disks().await {
        Ok(disks) => json_response(StatusCode::OK, &json!({ "pool": pool, "disks": disks })),
        Err(e) => write_client_error(&e),
    }
}

pub async fn pool_devices_post(
    State(state): State<Arc<AppState>>,
    Path(pool): Path<String>,
    body: Body,
) -> Response {
    let Some(zfs) = state.zfs.as_ref() else {
        return zfs_unavailable();
    };
    if let Err(e) = validate_pool_name(&pool) {
        return write_error(StatusCode::BAD_REQUEST, e.to_string());
    }
    let req: PoolDevicesRequest = match read_json(body, MAX_JSON_BODY_BYTES).await {
        Ok(req) => req,
        Err(resp) => return resp,
    };
    match zfs
        .add_vdev(&pool, &req.topology, &req.disks, req.force)
        .await
    {
        Ok(()) => {
            tracing::info!(
                "added {} disk(s) to pool {} (topology {:?}, force {})",
                req.disks.len(),
                pool,
                req.topology,
                req.force
            );
            json_response(
                StatusCode::CREATED,
                &json!({
                    "status": "vdev added",
                    "pool": pool,
                    "topology": req.topology,
                    "disks": req.disks,
                }),
            )
        }
        // The Go handler answers 400 for every AddVDev failure.
        Err(e) => write_error(StatusCode::BAD_REQUEST, e.to_string()),
    }
}

// ---- Datasets ------------------------------------------------------------

pub async fn datasets_all_get(State(state): State<Arc<AppState>>) -> Response {
    let Some(zfs) = state.zfs.as_ref() else {
        return zfs_unavailable();
    };
    match zfs.all_datasets().await {
        Ok(datasets) => json_response(StatusCode::OK, &datasets),
        Err(e) => write_client_error(&e),
    }
}

pub async fn datasets_get(
    State(state): State<Arc<AppState>>,
    Path(rest): Path<String>,
) -> Response {
    let Some(zfs) = state.zfs.as_ref() else {
        return zfs_unavailable();
    };
    if rest.is_empty() {
        return write_error(StatusCode::BAD_REQUEST, "pool name required");
    }
    match zfs.datasets(&rest).await {
        Ok(datasets) => json_response(StatusCode::OK, &datasets),
        Err(e) => write_client_error(&e),
    }
}

#[derive(Default, Deserialize)]
struct DatasetCreateRequest {
    #[serde(default)]
    name: String,
    #[serde(default)]
    options: HashMap<String, String>,
}

pub async fn datasets_post(
    State(state): State<Arc<AppState>>,
    Path(rest): Path<String>,
    body: Body,
) -> Response {
    let Some(zfs) = state.zfs.as_ref() else {
        return zfs_unavailable();
    };
    let req: DatasetCreateRequest = match read_json(body, MAX_JSON_BODY_BYTES).await {
        Ok(req) => req,
        Err(resp) => return resp,
    };
    let dataset_name = format!("{rest}/{}", req.name);
    match zfs.create_dataset(&dataset_name, &req.options).await {
        Ok(()) => {
            tracing::info!("created dataset {}", dataset_name);
            json_response(
                StatusCode::CREATED,
                &json!({ "status": "dataset created", "name": dataset_name }),
            )
        }
        Err(e) => write_error(StatusCode::BAD_REQUEST, e.to_string()),
    }
}

pub async fn datasets_delete(
    State(state): State<Arc<AppState>>,
    Path(rest): Path<String>,
    Query(query): Query<QueryMap>,
) -> Response {
    let Some(zfs) = state.zfs.as_ref() else {
        return zfs_unavailable();
    };
    let recursive = query.get("recursive").map(|v| v == "true").unwrap_or(false);
    match zfs.destroy_dataset(&rest, recursive).await {
        Ok(()) => {
            tracing::info!("destroyed dataset {} (recursive={})", rest, recursive);
            json_response(
                StatusCode::OK,
                &json!({ "status": "dataset destroyed", "name": rest }),
            )
        }
        Err(e) => write_error(StatusCode::BAD_REQUEST, e.to_string()),
    }
}

// ---- Snapshots -----------------------------------------------------------

pub async fn snapshots_get(
    State(state): State<Arc<AppState>>,
    Path(dataset): Path<String>,
) -> Response {
    let Some(zfs) = state.zfs.as_ref() else {
        return zfs_unavailable();
    };
    match zfs.snapshots(&dataset).await {
        Ok(snaps) => json_response(StatusCode::OK, &snaps),
        Err(e) => write_client_error(&e),
    }
}

#[derive(Default, Deserialize)]
struct SnapshotRequest {
    #[serde(default)]
    name: String,
}

pub async fn snapshots_post(
    State(state): State<Arc<AppState>>,
    Path(dataset): Path<String>,
    body: Body,
) -> Response {
    let Some(zfs) = state.zfs.as_ref() else {
        return zfs_unavailable();
    };
    let req: SnapshotRequest = match read_json(body, MAX_JSON_BODY_BYTES).await {
        Ok(req) => req,
        Err(resp) => return resp,
    };
    match zfs.snapshot(&dataset, &req.name).await {
        Ok(()) => json_response(StatusCode::CREATED, &json!({ "status": "snapshot created" })),
        Err(e) => write_client_error(&e),
    }
}

// ---- Shares --------------------------------------------------------------

#[derive(Default, Deserialize)]
struct SharesConfigRequest {
    #[serde(rename = "sambaConf", default)]
    samba_conf: String,
    #[serde(rename = "ganeshaConf", default)]
    ganesha_conf: String,
    #[serde(rename = "sambaUsers", default)]
    samba_users: String,
    #[serde(rename = "nssPasswd", default)]
    nss_passwd: String,
    #[serde(rename = "nssGroup", default)]
    nss_group: String,
    #[serde(rename = "nssShadow", default)]
    nss_shadow: String,
    #[serde(default)]
    revision: String,
    #[serde(rename = "shareCount", default)]
    share_count: i32,
}

pub async fn shares_config(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let Some(shares) = state.shares.as_ref() else {
        return shares_unavailable("share configuration");
    };
    let req: SharesConfigRequest = match read_json(body, MAX_JSON_BODY_BYTES).await {
        Ok(req) => req,
        Err(resp) => return resp,
    };
    let cfg = Config {
        samba_conf: req.samba_conf,
        ganesha_conf: req.ganesha_conf,
        samba_users: req.samba_users,
        nss_passwd: req.nss_passwd,
        nss_group: req.nss_group,
        nss_shadow: req.nss_shadow,
        revision: req.revision,
        share_count: req.share_count,
    };
    match shares.apply(&cfg) {
        Ok(status) => {
            tracing::info!(
                "applied shares config revision {} ({} enabled shares, {} smb sections, {} nfs exports)",
                cfg.revision,
                cfg.share_count,
                status.smb_share_count,
                status.nfs_export_count
            );
            json_response(StatusCode::OK, &status)
        }
        Err(msg) => write_client_error(&ZfsError::Other(msg)),
    }
}

pub async fn shares_status(State(state): State<Arc<AppState>>) -> Response {
    let Some(shares) = state.shares.as_ref() else {
        return shares_unavailable("share configuration");
    };
    match shares.status() {
        Ok(status) => json_response(StatusCode::OK, &status),
        Err(msg) => write_client_error(&ZfsError::Other(msg)),
    }
}

#[derive(Default, Deserialize)]
struct ShareFolderRequest {
    #[serde(default)]
    path: String,
    #[serde(default)]
    name: String,
}

pub async fn shares_folders_get(
    State(state): State<Arc<AppState>>,
    Query(query): Query<QueryMap>,
) -> Response {
    let Some(shares) = state.shares.as_ref() else {
        return shares_unavailable("share folders");
    };
    let path = query.get("path").cloned().unwrap_or_default();
    match shares.list_folders(&path) {
        Ok(folders) => {
            json_response(StatusCode::OK, &json!({ "path": path, "folders": folders }))
        }
        Err(msg) => write_error(StatusCode::NOT_FOUND, msg),
    }
}

pub async fn shares_folders_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let Some(shares) = state.shares.as_ref() else {
        return shares_unavailable("share folders");
    };
    let req: ShareFolderRequest = match read_json(body, MAX_JSON_BODY_BYTES).await {
        Ok(req) => req,
        Err(resp) => return resp,
    };
    match shares.create_folder(&req.path, &req.name) {
        Ok(created) => {
            tracing::info!("created share folder {}", created);
            json_response(StatusCode::CREATED, &json!({ "path": created }))
        }
        Err(msg) => write_error(StatusCode::BAD_REQUEST, msg),
    }
}

pub async fn shares_folders_delete(
    State(state): State<Arc<AppState>>,
    Query(query): Query<QueryMap>,
) -> Response {
    let Some(shares) = state.shares.as_ref() else {
        return shares_unavailable("share folders");
    };
    let path = query.get("path").cloned().unwrap_or_default();
    match shares.delete_folder(&path) {
        Ok(()) => {
            tracing::info!("removed share folder {}", path);
            json_response(
                StatusCode::OK,
                &json!({ "status": "folder removed", "path": path }),
            )
        }
        Err(msg) => write_error(StatusCode::BAD_REQUEST, msg),
    }
}

// ---- Backup streams ------------------------------------------------------

pub async fn zfs_send(
    State(state): State<Arc<AppState>>,
    Path(dataset): Path<String>,
    Query(query): Query<QueryMap>,
) -> Response {
    let Some(zfs) = state.zfs.as_ref() else {
        return backup_unavailable();
    };

    let opts = SendStreamOptions {
        dataset,
        to: query.get("to").cloned().unwrap_or_default(),
        from: query.get("from").cloned().unwrap_or_default(),
        raw: query.get("raw").map(|v| v != "false").unwrap_or(true),
    };
    if opts.to.is_empty() {
        return write_error(
            StatusCode::BAD_REQUEST,
            "to is required (the snapshot to send, without the dataset@ prefix)",
        );
    }

    if query.get("estimate").map(|v| v == "true").unwrap_or(false) {
        return match zfs.estimate_send(&opts).await {
            Ok(bytes) => json_response(
                StatusCode::OK,
                &json!({
                    "dataset": opts.dataset,
                    "to": opts.to,
                    "from": opts.from,
                    "raw": opts.raw,
                    "bytes": bytes,
                }),
            ),
            Err(e) => write_error(StatusCode::BAD_REQUEST, e.to_string()),
        };
    }

    let (read, wait) = match zfs.send_stream(&opts).await {
        Ok(pair) => pair,
        Err(e) => return write_error(StatusCode::BAD_REQUEST, e.to_string()),
    };

    let reader = tokio_util::io::ReaderStream::new(read);
    let mut wait = Some(wait);
    let stream = reader.chain(futures::stream::once(async move {
        if let Some(w) = wait.take() {
            if let Err(e) = w.await {
                tracing::error!("zfs send failed after streaming: {e}");
            }
        }
        Ok::<bytes::Bytes, std::io::Error>(bytes::Bytes::new())
    }));

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .body(Body::from_stream(stream))
        .expect("response builds")
}

pub async fn zfs_receive(
    State(state): State<Arc<AppState>>,
    Path(dataset): Path<String>,
    Query(query): Query<QueryMap>,
    body: Body,
) -> Response {
    let Some(zfs) = state.zfs.as_ref() else {
        return backup_unavailable();
    };
    let force = query.get("force").map(|v| v == "true").unwrap_or(false);

    let (mut stdin, wait) = match zfs.receive_stream(&dataset, force).await {
        Ok(pair) => pair,
        Err(e) => return write_error(StatusCode::BAD_REQUEST, e.to_string()),
    };

    let mut written: u64 = 0;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(e) => {
                let _ = wait.await;
                return write_error(
                    StatusCode::BAD_REQUEST,
                    format!("reading the restore stream: {e}"),
                );
            }
        };
        written += chunk.len() as u64;
        if written > MAX_STREAM_BODY_BYTES {
            let _ = wait.await;
            return write_error(
                StatusCode::BAD_REQUEST,
                "reading the restore stream: request body too large",
            );
        }
        if let Err(e) = stdin.write_all(&chunk).await {
            let _ = wait.await;
            return write_error(
                StatusCode::BAD_REQUEST,
                format!("reading the restore stream: {e}"),
            );
        }
    }

    if let Err(e) = stdin.shutdown().await {
        let _ = wait.await;
        return write_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("closing the restore stream: {e}"),
        );
    }

    match wait.await {
        Ok(()) => {
            tracing::info!("zfs receive {dataset} restored {written} bytes (force={force})");
            json_response(
                StatusCode::OK,
                &json!({
                    "status": "received",
                    "dataset": dataset,
                    "bytes": written,
                    "force": force,
                }),
            )
        }
        Err(e) => write_error(StatusCode::UNPROCESSABLE_ENTITY, e),
    }
}

pub async fn zfs_snapshots(
    State(state): State<Arc<AppState>>,
    Path(dataset): Path<String>,
) -> Response {
    let Some(zfs) = state.zfs.as_ref() else {
        return backup_unavailable();
    };
    match zfs.snapshots_with_guid(&dataset).await {
        Ok(snapshots) => json_response(StatusCode::OK, &snapshots),
        Err(e) => write_error(StatusCode::BAD_REQUEST, e.to_string()),
    }
}
