//! ZFS pool/dataset handlers (port of `api/internal/server/zfs.go`).
//!
//! Pool operations are executed by the privileged naslos-agent DaemonSet; this
//! layer validates first (fail fast with 400) and maps agent errors to the right
//! status (`writeAgentError`).

use crate::agent::{AgentError, CreatePoolRequest};
use crate::json;
use crate::logsafe;
use crate::state::AppState;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;

// ---- Validation (ported from zfs.go) -------------------------------------

/// ZFS-safe pool charset: must start alphanumerically.
pub fn is_valid_pool_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
}

/// Dataset component charset: `^[A-Za-z0-9][A-Za-z0-9._-]*$`.
pub fn is_valid_dataset_component(part: &str) -> bool {
    let mut chars = part.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn valid_topologies() -> [&'static str; 7] {
    [
        "", "single", "mirror", "raidz", "raidz1", "raidz2", "raidz3",
    ]
}

/// ZFS's minimum disk count per topology (mirrors the agent).
pub fn vdev_minimum_disks(topology: &str) -> Option<usize> {
    match topology {
        "" | "single" | "stripe" => Some(1),
        "mirror" | "raidz" | "raidz1" => Some(2),
        "raidz2" => Some(3),
        "raidz3" => Some(4),
        _ => None,
    }
}

fn matches_size(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    if b.is_empty() || !b[0].is_ascii_digit() {
        return false;
    }
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        if i >= b.len() || !b[i].is_ascii_digit() {
            return false;
        }
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    if i < b.len() {
        if !matches!(b[i], b'K' | b'M' | b'G' | b'T' | b'P' | b'E') {
            return false;
        }
        i += 1;
    }
    i == b.len()
}

fn valid_compression(v: &str) -> bool {
    matches!(
        v,
        "on" | "off" | "lz4" | "zstd" | "zstd-fast" | "gzip" | "gzip-1" | "gzip-9" | "lzjb" | "zle"
    )
}

/// Validate a dataset name relative to its pool.
pub fn validate_dataset_name(name: &str) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("dataset name is required".into());
    }
    if trimmed != name {
        return Err("dataset name must not start or end with whitespace".into());
    }
    if trimmed.starts_with('/') {
        return Err("dataset name must be relative to its pool, not a path".into());
    }
    if trimmed.contains('@') {
        return Err("dataset name must not contain '@' (that is a snapshot)".into());
    }
    for part in trimmed.split('/') {
        if !is_valid_dataset_component(part) {
            return Err(format!(
                "invalid dataset name {trimmed:?}: each part must start with a letter or digit and contain only letters, digits, '.', '_' or '-'"
            ));
        }
    }
    Ok(())
}

/// Validate a full dataset path (`pool/name[/…]`).
pub fn validate_dataset_path(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("dataset name is required".into());
    }
    let parts: Vec<&str> = name.split('/').collect();
    if parts.len() < 2 {
        return Err(format!("dataset {name:?} must be <pool>/<name>"));
    }
    if !is_valid_pool_name(parts[0]) {
        return Err(format!("invalid pool name {:?}", parts[0]));
    }
    validate_dataset_name(&parts[1..].join("/"))
}

/// Validate a dataset create request's options.
pub fn validate_dataset_create_options(options: &HashMap<String, String>) -> Result<(), String> {
    for (key, raw) in options {
        let value = raw.trim();
        match key.as_str() {
            "compression" => {
                if !valid_compression(value) {
                    return Err(format!("unsupported compression {value:?}"));
                }
            }
            "quota" => {
                if !value.is_empty() && value != "none" && !matches_size(value) {
                    return Err(format!(
                        "invalid quota {value:?}: use a size such as 500G, or none"
                    ));
                }
            }
            "recordsize" => {
                if !matches_size(value) {
                    return Err(format!("invalid recordsize {value:?}"));
                }
            }
            "atime" | "readonly" => {
                if value != "on" && value != "off" {
                    return Err(format!("{key} must be on or off"));
                }
            }
            "copies" => {
                if value != "1" && value != "2" && value != "3" {
                    return Err("copies must be 1, 2 or 3".into());
                }
            }
            _ => return Err(format!("unsupported dataset option {key:?}")),
        }
    }
    Ok(())
}

/// The pure part of the disk check.
pub fn validate_disk_selection(
    disks: &[String],
    usable: &std::collections::HashSet<String>,
    in_pool: &HashMap<String, String>,
) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for disk in disks {
        let dev = disk.trim();
        if !dev.starts_with("/dev/") {
            return Err(format!("disk {disk:?} must be an absolute path under /dev"));
        }
        if !seen.insert(dev.to_string()) {
            return Err(format!("disk {dev} was selected twice"));
        }
        if let Some(owner) = in_pool.get(dev) {
            return Err(format!("disk {dev} already belongs to pool {owner:?}"));
        }
        if !usable.contains(dev) {
            return Err(format!(
                "disk {dev} is not a disk this node can use (unknown, or the system disk)"
            ));
        }
    }
    Ok(())
}

// ---- Handlers ------------------------------------------------------------

#[allow(clippy::result_large_err)]
fn agent(state: &AppState) -> Result<Arc<crate::agent::Client>, Response> {
    state.agent.clone().ok_or_else(|| {
        json(
            StatusCode::BAD_GATEWAY,
            serde_json::json!({ "error": "agent client not configured" }),
        )
    })
}

/// Map an agent client error to HTTP: agent-side failures carry their upstream
/// status (notably 503 degraded); transport failures (status 0) become 502.
pub fn write_agent_error(err: &AgentError) -> Response {
    if err.status != 0 {
        json(
            StatusCode::from_u16(err.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            serde_json::json!({ "error": err.message }),
        )
    } else {
        json(
            StatusCode::BAD_GATEWAY,
            serde_json::json!({ "error": err.to_string() }),
        )
    }
}

#[allow(clippy::result_large_err)]
pub async fn read_json<T: serde::de::DeserializeOwned>(body: Body) -> Result<T, Response> {
    let bytes = axum::body::to_bytes(body, 1 << 20).await.map_err(|e| {
        json(
            StatusCode::BAD_REQUEST,
            serde_json::json!({ "error": e.to_string() }),
        )
    })?;
    serde_json::from_slice(&bytes).map_err(|e| {
        json(
            StatusCode::BAD_REQUEST,
            serde_json::json!({ "error": e.to_string() }),
        )
    })
}

#[derive(Default, Deserialize)]
struct CreatePoolBody {
    #[serde(default)]
    name: String,
    #[serde(default)]
    topology: String,
    #[serde(default)]
    disks: Vec<String>,
    #[serde(default)]
    cache: String,
    #[serde(default)]
    options: HashMap<String, String>,
}

pub async fn pools_get(State(state): State<Arc<AppState>>) -> Response {
    let client = match agent(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match client.list_pools().await {
        Ok(pools) => json(StatusCode::OK, serde_json::to_value(pools).unwrap()),
        Err(e) => write_agent_error(&e),
    }
}

pub async fn pools_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let client = match agent(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let req: CreatePoolBody = match read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    if !is_valid_pool_name(&req.name) {
        return json(
            StatusCode::BAD_REQUEST,
            serde_json::json!({ "error": format!("invalid pool name {:?}: must start with a letter or digit and contain only letters, digits, '.', '_', ':' or '-'", req.name) }),
        );
    }
    if req.disks.is_empty() {
        return bad_request("at least one disk is required");
    }
    let topology = req.topology.trim().to_ascii_lowercase();
    if !valid_topologies().contains(&topology.as_str()) {
        return bad_request(&format!(
            "unsupported topology {:?} (supported: single, mirror, raidz1, raidz2, raidz3)",
            req.topology
        ));
    }
    if let Err(e) = validate_add_disks(&state, &topology, &req.disks).await {
        return bad_request(&e);
    }
    if !req.cache.is_empty() {
        if let Err(e) = validate_add_disks(&state, "single", std::slice::from_ref(&req.cache)).await
        {
            return bad_request(&format!("cache device: {e}"));
        }
        if req.disks.iter().any(|d| d.trim() == req.cache.trim()) {
            return bad_request("the cache device cannot also be a data disk");
        }
    }
    let create = CreatePoolRequest {
        name: req.name.clone(),
        topology: req.topology.clone(),
        disks: req.disks.clone(),
        cache: req.cache.clone(),
        options: req.options.clone(),
    };
    match client.create_pool(&create).await {
        Ok(()) => json(
            StatusCode::CREATED,
            serde_json::json!({ "status": "pool created", "pool": req.name, "topology": req.topology }),
        ),
        Err(e) => write_agent_error(&e),
    }
}

#[derive(Default, Deserialize)]
struct ImportBody {
    #[serde(default)]
    name: String,
}

pub async fn import_get(State(state): State<Arc<AppState>>) -> Response {
    let client = match agent(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match client.list_importable_pools().await {
        Ok(pools) => json(StatusCode::OK, serde_json::to_value(pools).unwrap()),
        Err(e) => write_agent_error(&e),
    }
}

pub async fn import_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let client = match agent(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    // Decode errors are ignored (Go), so an empty body imports everything.
    let req: ImportBody = read_json(body).await.unwrap_or_default();
    if !req.name.is_empty() && !is_valid_pool_name(&req.name) {
        return bad_request(&format!("invalid pool name {:?}", req.name));
    }
    match client.import_pool(&req.name).await {
        Ok(()) => {
            if req.name.is_empty() {
                json(
                    StatusCode::OK,
                    serde_json::json!({ "status": "all pools imported" }),
                )
            } else {
                json(
                    StatusCode::OK,
                    serde_json::json!({ "status": "pool imported", "pool": req.name }),
                )
            }
        }
        Err(e) => write_agent_error(&e),
    }
}

pub async fn pool_detail_get(
    State(state): State<Arc<AppState>>,
    Path(pool): Path<String>,
) -> Response {
    if !is_valid_pool_name(&pool) {
        return bad_request(&format!("invalid pool name {pool:?}"));
    }
    let client = match agent(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match client.pool_health(&pool).await {
        Ok(h) => json(StatusCode::OK, serde_json::to_value(h).unwrap()),
        Err(e) => write_agent_error(&e),
    }
}

pub async fn pool_detail_delete(
    State(state): State<Arc<AppState>>,
    Path(pool): Path<String>,
) -> Response {
    if !is_valid_pool_name(&pool) {
        return bad_request(&format!("invalid pool name {pool:?}"));
    }
    let client = match agent(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match client.delete_pool(&pool).await {
        Ok(()) => json(
            StatusCode::OK,
            serde_json::json!({ "status": "pool destroyed", "pool": pool }),
        ),
        Err(e) => write_agent_error(&e),
    }
}

pub async fn pool_health_get(
    State(state): State<Arc<AppState>>,
    Path(pool): Path<String>,
) -> Response {
    if !is_valid_pool_name(&pool) {
        return bad_request("invalid pool name");
    }
    let client = match agent(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match client.pool_health(&pool).await {
        Ok(h) => json(StatusCode::OK, serde_json::to_value(h).unwrap()),
        Err(e) => write_agent_error(&e),
    }
}

#[derive(Default, Deserialize)]
struct DevicesBody {
    #[serde(default)]
    disks: Vec<String>,
    #[serde(default)]
    topology: String,
    #[serde(default)]
    force: bool,
}

pub async fn pool_devices_post(
    State(state): State<Arc<AppState>>,
    Path(pool): Path<String>,
    body: Body,
) -> Response {
    if !is_valid_pool_name(&pool) {
        return bad_request("invalid pool name");
    }
    let req: DevicesBody = match read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    if let Err(e) = require_pool(&state, &pool).await {
        return bad_request(&e);
    }
    if let Err(e) = validate_add_disks(&state, &req.topology, &req.disks).await {
        return bad_request(&e);
    }
    let client = match agent(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match client
        .add_pool_vdev(&pool, &req.topology, &req.disks, req.force)
        .await
    {
        Ok(()) => {
            tracing::info!(
                "attached {} disk(s) to pool {} (topology {:?}, force {})",
                req.disks.len(),
                logsafe::field(&pool),
                logsafe::field(&req.topology),
                req.force
            );
            json(
                StatusCode::CREATED,
                serde_json::json!({
                    "status": "vdev added", "pool": pool,
                    "topology": req.topology, "disks": req.disks
                }),
            )
        }
        Err(e) => write_agent_error(&e),
    }
}

async fn require_pool(state: &AppState, name: &str) -> Result<(), String> {
    let client = state
        .agent
        .clone()
        .ok_or_else(|| "agent client not configured".to_string())?;
    let pools = client
        .list_pools()
        .await
        .map_err(|e| format!("cannot verify pool {name} (agent unavailable): {e}"))?;
    if pools.iter().any(|p| p.name == name) {
        Ok(())
    } else {
        Err(format!("pool {name:?} not found"))
    }
}

async fn validate_add_disks(
    state: &AppState,
    topology: &str,
    disks: &[String],
) -> Result<(), String> {
    let norm = topology.trim().to_ascii_lowercase();
    let Some(minimum) = vdev_minimum_disks(&norm) else {
        return Err(format!(
            "unsupported topology {topology:?} (supported: single, mirror, raidz1, raidz2, raidz3)"
        ));
    };
    if disks.is_empty() {
        return Err("select at least one disk".into());
    }
    if disks.len() < minimum {
        return Err(format!(
            "topology {norm:?} needs at least {minimum} disks, got {}",
            disks.len()
        ));
    }

    let talos = state
        .talos
        .clone()
        .ok_or_else(|| "cannot list the node's disks: Talos client not configured".to_string())?;
    let discovered = talos
        .get_discovered_volumes()
        .await
        .map_err(|e| format!("cannot list the node's disks: {e}"))?;
    let usable: std::collections::HashSet<String> = discovered
        .iter()
        .filter(|d| !d.system_disk)
        .map(|d| d.device_name.clone())
        .collect();

    let mut in_pool = HashMap::new();
    if let Some(client) = state.agent.clone() {
        if let Ok(pools) = client.list_pools().await {
            for p in pools {
                for d in p.disks {
                    in_pool.insert(d, p.name.clone());
                }
            }
        }
    }

    validate_disk_selection(disks, &usable, &in_pool)
}

// ---- Datasets ------------------------------------------------------------

#[derive(Default, Deserialize)]
struct CreateDatasetBody {
    #[serde(default)]
    pool: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    options: HashMap<String, String>,
}

pub async fn datasets_get(State(state): State<Arc<AppState>>) -> Response {
    let client = match agent(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match client.list_datasets().await {
        Ok(datasets) => json(StatusCode::OK, serde_json::to_value(datasets).unwrap()),
        Err(e) => write_agent_error(&e),
    }
}

pub async fn datasets_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let req: CreateDatasetBody = match read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    if !is_valid_pool_name(&req.pool) {
        return bad_request(&format!("invalid pool name {:?}", req.pool));
    }
    if let Err(e) = validate_dataset_name(&req.name) {
        return bad_request(&e);
    }
    if let Err(e) = validate_dataset_create_options(&req.options) {
        return bad_request(&e);
    }
    if let Err(e) = require_pool(&state, &req.pool).await {
        return bad_request(&e);
    }
    let client = match agent(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match client
        .create_dataset(&req.pool, &req.name, &req.options)
        .await
    {
        Ok(()) => {
            tracing::info!(
                "created dataset {}/{}",
                logsafe::field(&req.pool),
                logsafe::field(&req.name)
            );
            json(
                StatusCode::CREATED,
                serde_json::json!({ "status": "dataset created", "name": format!("{}/{}", req.pool, req.name) }),
            )
        }
        Err(e) => write_agent_error(&e),
    }
}

pub async fn datasets_delete(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let name = query
        .get("name")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let recursive = query.get("recursive").map(|v| v == "true").unwrap_or(false);
    if let Err(e) = validate_dataset_path(&name) {
        return bad_request(&e);
    }
    if let Err(e) = require_unused_by_shares(&state, &name).await {
        return json(StatusCode::CONFLICT, serde_json::json!({ "error": e }));
    }
    let client = match agent(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match client.destroy_dataset(&name, recursive).await {
        Ok(()) => {
            tracing::info!(
                "destroyed dataset {} (recursive={})",
                logsafe::field(&name),
                recursive
            );
            json(
                StatusCode::OK,
                serde_json::json!({ "status": "dataset destroyed", "name": name }),
            )
        }
        Err(e) => write_agent_error(&e),
    }
}

/// Refuse to destroy a dataset that a share serves (or a parent of one).
async fn require_unused_by_shares(state: &AppState, name: &str) -> Result<(), String> {
    let client = match state.agent.clone() {
        Some(c) => c,
        None => return Ok(()),
    };
    let datasets = client
        .list_datasets()
        .await
        .map_err(|e| format!("cannot verify which paths dataset {name} backs: {e}"))?;

    let mut mountpoints = Vec::new();
    for d in &datasets {
        if (d.name == name || d.name.starts_with(&format!("{name}/")))
            && !d.mountpoint.is_empty()
            && d.mountpoint != "none"
        {
            mountpoints.push(d.mountpoint.clone());
        }
    }

    let shares = state.shares.lock().unwrap();
    for share in shares.list() {
        for mp in &mountpoints {
            if share.path == *mp || share.path.starts_with(&format!("{mp}/")) {
                return Err(format!(
                    "dataset {name} backs path {mp}, which share {:?} is serving: delete or repoint the share first",
                    share.name
                ));
            }
        }
    }
    Ok(())
}

fn bad_request(msg: &str) -> Response {
    json(StatusCode::BAD_REQUEST, serde_json::json!({ "error": msg }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_and_dataset_validators() {
        assert!(is_valid_pool_name("tank"));
        assert!(!is_valid_pool_name("-f"));
        assert!(validate_dataset_name("media/2026").is_ok());
        assert!(validate_dataset_name("../x").is_err());
        assert!(validate_dataset_path("tank/media").is_ok());
        assert!(validate_dataset_path("media").is_err());
    }

    #[test]
    fn disk_selection_rules() {
        let usable: std::collections::HashSet<String> =
            ["/dev/sdb".to_string()].into_iter().collect();
        let mut in_pool = HashMap::new();
        in_pool.insert("/dev/sdc".to_string(), "other".to_string());

        assert!(validate_disk_selection(&["/dev/sdb".into()], &usable, &in_pool).is_ok());
        assert!(validate_disk_selection(&["sdb".into()], &usable, &in_pool).is_err());
        assert!(validate_disk_selection(
            &["/dev/sdb".into(), "/dev/sdb".into()],
            &usable,
            &in_pool
        )
        .is_err());
        assert!(validate_disk_selection(&["/dev/sdc".into()], &usable, &in_pool).is_err());
        assert!(validate_disk_selection(&["/dev/sdz".into()], &usable, &in_pool).is_err());
    }
}
