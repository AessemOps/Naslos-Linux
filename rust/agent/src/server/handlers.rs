//! HTTP handlers (port of the Go `internal/server` handlers).

use super::error::{
    json_response, read_json, write_client_error, write_error, MAX_JSON_BODY_BYTES,
    MAX_STREAM_BODY_BYTES,
};
use super::AppState;
use crate::shares::Config;
use crate::zfs::validation::ZfsError;
use crate::zfs::{
    validate_pool_name, DiskHealth, DiskHealthReport, PoolConfig, SendStreamOptions, SmartInfo,
};
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
    #[serde(default)]
    all: bool,
}

pub async fn pool_import_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let Some(zfs) = state.zfs.as_ref() else {
        return zfs_unavailable();
    };
    // A malformed body must not degrade to "import every pool": that runs
    // `zpool import -f` across all disks. Import-all is an explicit
    // {"all": true}; a named import is {"name": "<pool>"}.
    let req: ImportRequest = match read_json(body, MAX_JSON_BODY_BYTES).await {
        Ok(req) => req,
        Err(resp) => return resp,
    };
    if req.name.trim().is_empty() && !req.all {
        return write_error(
            StatusCode::BAD_REQUEST,
            "name is required (or set \"all\": true to import every importable pool)",
        );
    }
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
    Query(query): Query<QueryMap>,
) -> Response {
    let Some(zfs) = state.zfs.as_ref() else {
        return zfs_unavailable();
    };
    // A path parameter is easy to hit by mistake; require the operator to
    // repeat the pool name, and keep `-f` for an explicit force request only.
    if query.get("confirm").map(String::as_str) != Some(pool.as_str()) {
        return write_error(
            StatusCode::BAD_REQUEST,
            "destroying a pool requires confirm=<pool name>",
        );
    }
    let force = query.get("force").map(|v| v == "true").unwrap_or(false);
    match zfs.destroy_pool(&pool, force).await {
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
        // Go marshalled a nil slice as `null`; an empty list must serialize the
        // same way, so wrap it in an Option.
        Ok(disks) => {
            let disks = if disks.is_empty() { None } else { Some(disks) };
            json_response(StatusCode::OK, &json!({ "pool": pool, "disks": disks }))
        }
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

/// `POST /api/v1/pools/{pool}/scrub` — start a scrub. The API refuses when a
/// scrub/resilver is already running; the agent only validates the pool name
/// and runs `zpool scrub`.
pub async fn pool_scrub_post(
    State(state): State<Arc<AppState>>,
    Path(pool): Path<String>,
) -> Response {
    let Some(zfs) = state.zfs.as_ref() else {
        return zfs_unavailable();
    };
    if let Err(e) = validate_pool_name(&pool) {
        return write_error(StatusCode::BAD_REQUEST, e.to_string());
    }
    match zfs.scrub_pool(&pool).await {
        Ok(()) => {
            tracing::info!("started scrub on pool {}", pool);
            json_response(
                StatusCode::ACCEPTED,
                &json!({ "status": "scrub started", "pool": pool }),
            )
        }
        Err(e) => write_client_error(&e),
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

/// `/api/v1/datasets/` (empty pool segment): Go's prefix mux matched it and
/// returned 400 `pool name required` before any ZFS-availability check.
pub async fn datasets_get_empty() -> Response {
    write_error(StatusCode::BAD_REQUEST, "pool name required")
}

#[derive(Default, Deserialize)]
struct DatasetCreateRequest {
    #[serde(default)]
    name: String,
    #[serde(default)]
    options: HashMap<String, String>,
    /// Raw key bytes for an encrypted create. Never logged or echoed; the
    /// handler zeroes it after the ZFS call.
    #[serde(default)]
    key: Option<Vec<u8>>,
}

pub async fn datasets_post(
    State(state): State<Arc<AppState>>,
    Path(rest): Path<String>,
    body: Body,
) -> Response {
    let Some(zfs) = state.zfs.as_ref() else {
        return zfs_unavailable();
    };
    let mut req: DatasetCreateRequest = match read_json(body, MAX_JSON_BODY_BYTES).await {
        Ok(req) => req,
        Err(resp) => return resp,
    };
    let dataset_name = format!("{rest}/{}", req.name);
    let result = zfs
        .create_dataset_with_key(&dataset_name, &req.options, req.key.as_deref())
        .await;
    if let Some(key) = req.key.as_mut() {
        key.fill(0);
    }
    match result {
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

/// `/api/v1/datasets/` POST with an empty pool segment.
pub async fn datasets_post_empty() -> Response {
    write_error(StatusCode::BAD_REQUEST, "pool name required")
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

// ---- Dataset encryption (FR-ENC) -----------------------------------------

pub async fn encryption_status_get(
    State(state): State<Arc<AppState>>,
    Path(dataset): Path<String>,
) -> Response {
    let Some(zfs) = state.zfs.as_ref() else {
        return zfs_unavailable();
    };
    match zfs.encryption_status(&dataset).await {
        Ok(status) => json_response(StatusCode::OK, &status),
        Err(e) => write_client_error(&e),
    }
}

pub async fn encryption_status_get_empty() -> Response {
    write_error(StatusCode::BAD_REQUEST, "dataset is required")
}

#[derive(Default, Deserialize)]
struct EncryptionUnlockRequest {
    #[serde(default)]
    dataset: String,
    /// Raw key bytes. Never logged or echoed.
    #[serde(default)]
    key: Vec<u8>,
}

pub async fn encryption_unlock_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let Some(zfs) = state.zfs.as_ref() else {
        return zfs_unavailable();
    };
    let mut req: EncryptionUnlockRequest = match read_json(body, MAX_JSON_BODY_BYTES).await {
        Ok(req) => req,
        Err(resp) => return resp,
    };
    if req.dataset.trim().is_empty() {
        return write_error(StatusCode::BAD_REQUEST, "dataset is required");
    }
    let dataset = req.dataset.trim().to_string();
    let result = zfs.unlock_dataset(&dataset, &req.key).await;
    req.key.fill(0);
    match result {
        Ok(status) => {
            tracing::info!("unlocked dataset {}", dataset);
            json_response(
                StatusCode::OK,
                &json!({ "status": "unlocked", "name": status.name, "keystatus": status.keystatus }),
            )
        }
        Err(e) => write_client_error(&e),
    }
}

#[derive(Default, Deserialize)]
struct EncryptionLockRequest {
    #[serde(default)]
    dataset: String,
}

pub async fn encryption_lock_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let Some(zfs) = state.zfs.as_ref() else {
        return zfs_unavailable();
    };
    let req: EncryptionLockRequest = match read_json(body, MAX_JSON_BODY_BYTES).await {
        Ok(req) => req,
        Err(resp) => return resp,
    };
    if req.dataset.trim().is_empty() {
        return write_error(StatusCode::BAD_REQUEST, "dataset is required");
    }
    let dataset = req.dataset.trim().to_string();
    match zfs.lock_dataset(&dataset).await {
        Ok(status) => {
            tracing::info!("locked dataset {}", dataset);
            json_response(
                StatusCode::OK,
                &json!({ "status": "locked", "name": status.name, "keystatus": status.keystatus }),
            )
        }
        Err(e) => write_client_error(&e),
    }
}

// ---- zvols ---------------------------------------------------------------

pub async fn zvols_get(
    State(state): State<Arc<AppState>>,
    Query(query): Query<QueryMap>,
) -> Response {
    let Some(zfs) = state.zfs.as_ref() else {
        return zfs_unavailable();
    };
    let pool = query.get("pool").map(|p| p.trim()).unwrap_or_default();
    let result = if pool.is_empty() {
        zfs.all_zvols().await
    } else {
        zfs.zvols(pool).await
    };
    match result {
        Ok(zvols) => json_response(StatusCode::OK, &zvols),
        Err(e) => write_client_error(&e),
    }
}

#[derive(Default, Deserialize)]
struct ZvolCreateRequest {
    #[serde(default)]
    name: String,
    #[serde(default)]
    size: String,
    #[serde(default)]
    options: HashMap<String, String>,
}

pub async fn zvols_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let Some(zfs) = state.zfs.as_ref() else {
        return zfs_unavailable();
    };
    let req: ZvolCreateRequest = match read_json(body, MAX_JSON_BODY_BYTES).await {
        Ok(req) => req,
        Err(resp) => return resp,
    };
    match zfs.create_zvol(&req.name, &req.size, &req.options).await {
        Ok(()) => {
            tracing::info!("created zvol {}", req.name);
            json_response(
                StatusCode::CREATED,
                &json!({ "status": "zvol created", "name": req.name }),
            )
        }
        Err(e) => write_error(StatusCode::BAD_REQUEST, e.to_string()),
    }
}

#[derive(Default, Deserialize)]
struct ZvolResizeRequest {
    #[serde(default)]
    size: String,
}

pub async fn zvols_resize(
    State(state): State<Arc<AppState>>,
    Query(query): Query<QueryMap>,
    body: Body,
) -> Response {
    let Some(zfs) = state.zfs.as_ref() else {
        return zfs_unavailable();
    };
    let name = query.get("name").map(String::as_str).unwrap_or_default();
    if name.trim().is_empty() {
        return write_error(StatusCode::BAD_REQUEST, "zvol name is required");
    }
    let req: ZvolResizeRequest = match read_json(body, MAX_JSON_BODY_BYTES).await {
        Ok(req) => req,
        Err(resp) => return resp,
    };
    match zfs.resize_zvol(name, &req.size).await {
        Ok(()) => {
            tracing::info!("resized zvol {} to {}", name, req.size.trim());
            json_response(
                StatusCode::OK,
                &json!({ "status": "zvol resized", "name": name, "size": req.size.trim() }),
            )
        }
        Err(e) => write_error(StatusCode::BAD_REQUEST, e.to_string()),
    }
}

pub async fn zvols_delete(
    State(state): State<Arc<AppState>>,
    Query(query): Query<QueryMap>,
) -> Response {
    let Some(zfs) = state.zfs.as_ref() else {
        return zfs_unavailable();
    };
    let name = query.get("name").map(String::as_str).unwrap_or_default();
    if name.trim().is_empty() {
        return write_error(StatusCode::BAD_REQUEST, "zvol name is required");
    }
    if query.get("confirm").map(String::as_str) != Some(name) {
        return write_error(
            StatusCode::BAD_REQUEST,
            "destroying a zvol requires confirm=<zvol name>",
        );
    }
    match zfs.destroy_zvol(name).await {
        Ok(()) => {
            tracing::info!("destroyed zvol {}", name);
            json_response(
                StatusCode::OK,
                &json!({ "status": "zvol destroyed", "name": name }),
            )
        }
        Err(e) => write_error(StatusCode::BAD_REQUEST, e.to_string()),
    }
}

// ---- Snapshots -----------------------------------------------------------

pub async fn snapshots_get(
    State(state): State<Arc<AppState>>,
    Path(dataset): Path<String>,
    Query(query): Query<QueryMap>,
) -> Response {
    let Some(zfs) = state.zfs.as_ref() else {
        return zfs_unavailable();
    };
    if query.get("paths").map(String::as_str) == Some("true") {
        // Retention planning needs the full `pool/ds@snap` paths so a
        // recursive snapshot's child entries are visible.
        return match zfs.snapshot_paths(&dataset).await {
            Ok(paths) => json_response(StatusCode::OK, &paths),
            Err(e) => write_client_error(&e),
        };
    }
    match zfs.snapshots(&dataset).await {
        Ok(snaps) => json_response(StatusCode::OK, &snaps),
        Err(e) => write_client_error(&e),
    }
}

/// `/api/v1/snapshots/` (empty dataset segment): Go validated the empty path
/// and returned 400.
pub async fn snapshots_get_empty() -> Response {
    write_error(StatusCode::BAD_REQUEST, "dataset path is required")
}

#[derive(Default, Deserialize)]
struct SnapshotRequest {
    #[serde(default)]
    name: String,
    /// Snapshot child datasets too (`zfs snapshot -r`). Scheduled snapshot
    /// tasks use it; a manual snapshot keeps the default (non-recursive).
    #[serde(default)]
    recursive: bool,
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
    match zfs.snapshot_with(&dataset, &req.name, req.recursive).await {
        Ok(()) => json_response(
            StatusCode::CREATED,
            &json!({ "status": "snapshot created" }),
        ),
        Err(e) => write_client_error(&e),
    }
}

/// `/api/v1/snapshots/` POST with an empty dataset segment.
pub async fn snapshots_post_empty() -> Response {
    write_error(StatusCode::BAD_REQUEST, "dataset path is required")
}

/// `DELETE /api/v1/snapshots/{dataset}?name=<snap>` — destroy one snapshot.
/// The API's retention scheduler is the only caller, and it only passes names
/// it generated (`naslos-auto-*`).
pub async fn snapshots_delete(
    State(state): State<Arc<AppState>>,
    Path(dataset): Path<String>,
    Query(query): Query<QueryMap>,
) -> Response {
    let Some(zfs) = state.zfs.as_ref() else {
        return zfs_unavailable();
    };
    let name = query
        .get("name")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if name.is_empty() {
        return write_error(StatusCode::BAD_REQUEST, "name is required");
    }
    match zfs.destroy_snapshot(&dataset, &name).await {
        Ok(()) => json_response(
            StatusCode::OK,
            &json!({ "status": "snapshot destroyed", "dataset": dataset, "name": name }),
        ),
        Err(e) => write_client_error(&e),
    }
}

/// `POST /api/v1/rollback/{dataset}` — roll a dataset back to a snapshot
/// (`zfs rollback -r`). Used by the VM snapshot-restore route; the API
/// refuses the restore while the VM is running.
pub async fn rollback_post(
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
    match zfs.rollback(&dataset, &req.name).await {
        Ok(()) => {
            tracing::info!("rolled back {} to snapshot {}", dataset, req.name);
            json_response(
                StatusCode::OK,
                &json!({ "status": "rolled back", "dataset": dataset, "name": req.name }),
            )
        }
        Err(e) => write_client_error(&e),
    }
}

/// `/api/v1/rollback/` POST with an empty dataset segment.
pub async fn rollback_post_empty() -> Response {
    write_error(StatusCode::BAD_REQUEST, "dataset path is required")
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
    let revision = cfg.revision.clone();
    let share_count = cfg.share_count;
    // shares.apply does synchronous host-filesystem I/O (including fsync);
    // run it off the async worker threads.
    let shares = shares.clone();
    let result = tokio::task::spawn_blocking(move || shares.apply(&cfg)).await;
    match result {
        Ok(Ok(status)) => {
            tracing::info!(
                "applied shares config revision {} ({} enabled shares, {} smb sections, {} nfs exports)",
                revision,
                share_count,
                status.smb_share_count,
                status.nfs_export_count
            );
            json_response(StatusCode::OK, &status)
        }
        Ok(Err(msg)) => write_client_error(&ZfsError::Other(msg)),
        Err(e) => write_client_error(&ZfsError::Other(format!("apply task failed: {e}"))),
    }
}

pub async fn shares_status(State(state): State<Arc<AppState>>) -> Response {
    let Some(shares) = state.shares.as_ref() else {
        return shares_unavailable("share configuration");
    };
    let shares = shares.clone();
    match tokio::task::spawn_blocking(move || shares.status()).await {
        Ok(Ok(status)) => json_response(StatusCode::OK, &status),
        Ok(Err(msg)) => write_client_error(&ZfsError::Other(msg)),
        Err(e) => write_client_error(&ZfsError::Other(format!("status task failed: {e}"))),
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
    let shares = shares.clone();
    let path_for_task = path.clone();
    match tokio::task::spawn_blocking(move || shares.list_folders(&path_for_task)).await {
        Ok(Ok(folders)) => {
            json_response(StatusCode::OK, &json!({ "path": path, "folders": folders }))
        }
        Ok(Err(msg)) => write_error(StatusCode::NOT_FOUND, msg),
        Err(e) => write_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("task failed: {e}"),
        ),
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
    let shares = shares.clone();
    match tokio::task::spawn_blocking(move || shares.create_folder(&req.path, &req.name)).await {
        Ok(Ok(created)) => {
            tracing::info!("created share folder {}", created);
            json_response(StatusCode::CREATED, &json!({ "path": created }))
        }
        Ok(Err(msg)) => write_error(StatusCode::BAD_REQUEST, msg),
        Err(e) => write_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("task failed: {e}"),
        ),
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
    let shares = shares.clone();
    let path_for_task = path.clone();
    match tokio::task::spawn_blocking(move || shares.delete_folder(&path_for_task)).await {
        Ok(Ok(())) => {
            tracing::info!("removed share folder {}", path);
            json_response(
                StatusCode::OK,
                &json!({ "status": "folder removed", "path": path }),
            )
        }
        Ok(Err(msg)) => write_error(StatusCode::BAD_REQUEST, msg),
        Err(e) => write_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("task failed: {e}"),
        ),
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
        recursive: query.get("recursive").map(|v| v == "true").unwrap_or(false),
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

    let permit = match state.stream_slots.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return write_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "too many concurrent zfs streams; retry when one finishes",
            )
        }
    };

    let (read, wait) = match zfs.send_stream(&opts).await {
        Ok(pair) => pair,
        Err(e) => return write_error(StatusCode::BAD_REQUEST, e.to_string()),
    };

    // 128 KiB chunks instead of the 4 KiB default: the send path streams
    // potentially terabytes and Go used a 32 KiB copy buffer.
    let reader = tokio_util::io::ReaderStream::with_capacity(read, 128 * 1024);
    let mut wait = Some(wait);
    let stream = reader.chain(futures::stream::once(async move {
        if let Some(w) = wait.take() {
            if let Err(e) = w.await {
                tracing::error!("zfs send failed after streaming: {e}");
            }
        }
        Ok::<bytes::Bytes, std::io::Error>(bytes::Bytes::new())
    }));
    // Hold the stream permit until the body is dropped, not just until this
    // handler returns.
    let stream = stream.map(move |item| {
        let _held = &permit;
        item
    });

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
    let _permit = match state.stream_slots.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return write_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "too many concurrent zfs streams; retry when one finishes",
            )
        }
    };

    let (mut stdin, wait) = match zfs.receive_stream(&dataset, force).await {
        Ok(pair) => pair,
        Err(e) => return write_error(StatusCode::BAD_REQUEST, e.to_string()),
    };

    let mut written: u64 = 0;
    let mut stream = body.into_data_stream();
    let idle = super::stream_idle_timeout();
    loop {
        let next = match tokio::time::timeout(idle, stream.next()).await {
            Ok(next) => next,
            Err(_) => {
                let _ = stdin.shutdown().await;
                let _ = wait.await;
                return write_error(
                    StatusCode::GATEWAY_TIMEOUT,
                    "restore stream stalled (no data within the idle deadline)",
                );
            }
        };
        let Some(chunk) = next else { break };
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(e) => {
                let _ = stdin.shutdown().await;
                let _ = wait.await;
                return write_error(
                    StatusCode::BAD_REQUEST,
                    format!("reading the restore stream: {e}"),
                );
            }
        };
        written += chunk.len() as u64;
        if written > MAX_STREAM_BODY_BYTES {
            let _ = stdin.shutdown().await;
            let _ = wait.await;
            return write_error(
                StatusCode::BAD_REQUEST,
                "reading the restore stream: request body too large",
            );
        }
        if let Err(e) = stdin.write_all(&chunk).await {
            let _ = stdin.shutdown().await;
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
    Query(query): Query<QueryMap>,
) -> Response {
    let Some(zfs) = state.zfs.as_ref() else {
        return backup_unavailable();
    };
    // Default stays recursive (buddy send/restore rely on it); replication
    // asks for `recursive=false` to compare one dataset's own snapshots.
    let recursive = query.get("recursive").map(|v| v != "false").unwrap_or(true);
    match zfs.snapshots_with_guid(&dataset, recursive).await {
        Ok(snapshots) => json_response(StatusCode::OK, &snapshots),
        Err(e) => write_error(StatusCode::BAD_REQUEST, e.to_string()),
    }
}

/// `/api/v1/zfs/send/` (empty dataset segment).
pub async fn zfs_send_empty() -> Response {
    write_error(StatusCode::BAD_REQUEST, "dataset is required")
}

/// `/api/v1/zfs/receive/` (empty dataset segment).
pub async fn zfs_receive_empty() -> Response {
    write_error(StatusCode::BAD_REQUEST, "dataset is required")
}

/// `/api/v1/zfs/snapshots/` (empty dataset segment).
pub async fn zfs_snapshots_empty() -> Response {
    write_error(StatusCode::BAD_REQUEST, "dataset is required")
}

/// `GET /api/v1/system/disk` — the host EPHEMERAL partition's usage. The API
/// cannot statvfs the host filesystem from its own namespace, so the agent
/// reports it (the dashboard's disk widget used the Talos machinery Mounts API
/// in the Go implementation, which `talosctl` does not expose).
pub async fn disk_usage() -> Response {
    let path = std::ffi::CString::new("/host/var").expect("static path");
    // SAFETY: statvfs writes into a caller-owned struct; the path is valid.
    let stat = unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(path.as_ptr(), &mut stat) != 0 {
            return write_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("statvfs /host/var: {}", std::io::Error::last_os_error()),
            );
        }
        stat
    };
    let bsize = stat.f_bsize as u128;
    let total = (stat.f_blocks as u128).saturating_mul(bsize);
    let free = (stat.f_bavail as u128).saturating_mul(bsize);
    let used = total.saturating_sub(free);
    let usage = if total > 0 {
        used as f64 / total as f64 * 100.0
    } else {
        0.0
    };
    json_response(
        StatusCode::OK,
        &json!({
            "total": total.min(i64::MAX as u128) as i64,
            "free": free.min(i64::MAX as u128) as i64,
            "used": used.min(i64::MAX as u128) as i64,
            "usagePercent": usage,
        }),
    )
}

/// `GET /api/v1/disks/health` — SMART per whole disk merged with the ZFS vdev
/// state. SMART being unavailable is a normal 200: the VM's virtio disks have
/// no SMART, and the page must still show the ZFS half.
pub async fn disks_health(State(state): State<Arc<AppState>>) -> Response {
    let report = match state.smart.as_ref() {
        Some(smart) => smart.scan().await,
        None => DiskHealthReport {
            smartctl_available: false,
            error: "SMART health is not configured on this agent".to_string(),
            disks: Vec::new(),
        },
    };

    let mut zfs = match state.zfs.as_ref() {
        Some(zfs) => zfs.disk_health().await,
        None => std::collections::HashMap::new(),
    };

    let mut disks: std::collections::BTreeMap<String, DiskHealth> = report
        .disks
        .into_iter()
        .map(|disk| (disk.device.clone(), disk))
        .collect();

    for disk in disks.values_mut() {
        disk.zfs = zfs.remove(&disk.device);
    }
    for (device, health) in zfs {
        let message = if report.smartctl_available {
            "SMART data was not reported for this disk".to_string()
        } else if report.error.is_empty() {
            "SMART is unavailable on this node".to_string()
        } else {
            report.error.clone()
        };
        disks.insert(
            device.clone(),
            DiskHealth {
                device,
                smart: SmartInfo {
                    available: false,
                    message,
                    ..Default::default()
                },
                zfs: Some(health),
            },
        );
    }

    json_response(
        StatusCode::OK,
        &DiskHealthReport {
            smartctl_available: report.smartctl_available,
            error: report.error,
            disks: disks.into_values().collect(),
        },
    )
}
