//! ZFS pool/dataset handlers (port of `api/internal/server/zfs.go`).
//!
//! Pool operations are executed by the privileged naslos-agent DaemonSet; this
//! layer validates first (fail fast with 400) and maps agent errors to the right
//! status (`writeAgentError`).

use crate::agent::{AgentError, CreatePoolRequest};
use crate::dataset_encryption::{self, KeyFormat, RawKey};
use crate::json;
use crate::logsafe;
use crate::state::AppState;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

// ---- Dataset mutation serialization --------------------------------------

/// One async mutex per dataset path. The agent serializes too, but the API's
/// check-then-act sequence (ownership checks, GUID capture, key deletion) must
/// also see one mutation at a time for a name.
static DATASET_LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    OnceLock::new();

/// Serialize create/destroy/zvol-create/zvol-delete/receive for one name.
pub(crate) async fn lock_dataset(name: &str) -> tokio::sync::OwnedMutexGuard<()> {
    let lock = {
        let locks = DATASET_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
        let mut locks = locks.lock().unwrap();
        locks
            .entry(name.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    };
    lock.lock_owned().await
}

/// Component-boundary overlap: equal, or one path is inside the other. A
/// prefix check alone (`starts_with("/var/mnt/tank")`) would call
/// `/var/mnt/tank2` a child of `/var/mnt/tank`.
pub(crate) fn paths_overlap(a: &str, b: &str) -> bool {
    let a = a.trim_end_matches('/');
    let b = b.trim_end_matches('/');
    if a.is_empty() || b.is_empty() {
        return false;
    }
    a == b || a.starts_with(&format!("{b}/")) || b.starts_with(&format!("{a}/"))
}

/// Mountpoints of a dataset and every descendant, plus the server-chosen
/// mountpoint for the target itself (covers `canmount=off`/`none` subtrees).
fn subtree_mountpoints(datasets: &[crate::agent::Dataset], name: &str) -> Vec<String> {
    let prefix = format!("{name}/");
    let mut points: Vec<String> = datasets
        .iter()
        .filter(|d| d.name == name || d.name.starts_with(&prefix))
        .filter(|d| !d.mountpoint.is_empty() && d.mountpoint != "none")
        .map(|d| d.mountpoint.trim_end_matches('/').to_string())
        .collect();
    let expected = format!("/var/mnt/{name}");
    if !points.contains(&expected) {
        points.push(expected);
    }
    points
}

/// The VM that owns a dataset path, if any. `<pool>/vms` is reserved for VM
/// storage; `<pool>/vms/<name>` is one VM's disk dataset.
fn vm_dataset_owner(state: &AppState, name: &str) -> Option<String> {
    let (pool, rest) = name.split_once('/')?;
    if rest == "vms" {
        return Some("the VM dataset tree (<pool>/vms)".to_string());
    }
    let vm_part = rest.strip_prefix("vms/")?;
    let vm_name = vm_part.split('/').next().unwrap_or(vm_part);
    if vm_name == "isos" {
        return Some("the VM image library (<pool>/vms/isos)".to_string());
    }
    if let Some(vm) = state.vms.get(vm_name) {
        if vm.pool == pool {
            return Some(format!("VM {vm_name:?}"));
        }
    }
    if vm_part.is_empty() {
        Some("the VM dataset tree (<pool>/vms)".to_string())
    } else {
        Some(format!("the VM dataset tree (<pool>/vms/{vm_name})"))
    }
}

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

/// Parse a validated ZFS size (`10G`, `1.5T`, `4096`) into bytes.
fn parse_size_bytes(raw: &str) -> Option<u64> {
    let s = raw.trim();
    if !matches_size(s) {
        return None;
    }
    let (number, unit) = match s.as_bytes().last() {
        Some(b'K') => (&s[..s.len() - 1], 1024u64),
        Some(b'M') => (&s[..s.len() - 1], 1024u64.pow(2)),
        Some(b'G') => (&s[..s.len() - 1], 1024u64.pow(3)),
        Some(b'T') => (&s[..s.len() - 1], 1024u64.pow(4)),
        Some(b'P') => (&s[..s.len() - 1], 1024u64.pow(5)),
        Some(b'E') => (&s[..s.len() - 1], 1024u64.pow(6)),
        _ => (s, 1),
    };
    let value: f64 = number.parse().ok()?;
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    let bytes = value * unit as f64;
    if bytes >= u64::MAX as f64 {
        return None;
    }
    // Do not round: `0.1` is not a byte and `1023.5` is not a power of two.
    // Fractional byte counts are refused rather than silently rounded.
    if bytes < 1.0 || bytes.fract() != 0.0 {
        return None;
    }
    Some(bytes as u64)
}

/// Validate a zvol size (creation and resize).
pub fn validate_zvol_size(raw: &str) -> Result<u64, String> {
    let value = raw.trim();
    if value.is_empty() {
        return Err("zvol size is required (for example 10G)".into());
    }
    parse_size_bytes(value)
        .ok_or_else(|| format!("invalid zvol size {value:?}: use a size such as 10G"))
}

/// Validate `volblocksize`: a power of two from 512 bytes to 16 MiB.
pub fn validate_volblocksize(raw: &str) -> Result<(), String> {
    let value = raw.trim();
    let bytes = parse_size_bytes(value).ok_or_else(|| {
        format!("invalid volblocksize {value:?}: use a power of two from 512 to 16M")
    })?;
    if !(512..=16 * 1024 * 1024).contains(&bytes) || !bytes.is_power_of_two() {
        return Err(format!(
            "invalid volblocksize {value:?}: use a power of two from 512 to 16M"
        ));
    }
    Ok(())
}

/// Validate the properties a zvol is created with.
pub fn validate_zvol_options(options: &HashMap<String, String>) -> Result<(), String> {
    for (key, raw) in options {
        let value = raw.trim();
        match key.as_str() {
            "volblocksize" => validate_volblocksize(value)?,
            "compression" => {
                if !valid_compression(value) {
                    return Err(format!("unsupported compression {value:?}"));
                }
            }
            "volsize" => {
                return Err("set the size with the size field, not a volsize option".into())
            }
            "quota" | "recordsize" | "atime" | "copies" | "readonly" => {
                return Err(format!(
                    "{key} is a dataset option; it cannot be set on a zvol"
                ))
            }
            _ => return Err(format!("unsupported zvol option {key:?}")),
        }
    }
    Ok(())
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
    if trimmed.len() > 200 {
        return Err("dataset name must be 200 characters or fewer".into());
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
///
/// This is deliberately more permissive than [`validate_dataset_name`]: it is
/// used for datasets that already exist and must support names ZFS allows but
/// the UI no longer creates, notably TrueNAS's `.system` tree. It still rejects
/// absolute paths, snapshots, and empty/`.`/`..` components.
pub fn validate_dataset_path(name: &str) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("dataset name is required".into());
    }
    if trimmed != name {
        return Err("dataset name must not start or end with whitespace".into());
    }
    if trimmed.starts_with('/') {
        return Err("dataset name must be relative to its pool, not an absolute path".into());
    }
    if trimmed.contains('@') {
        return Err("dataset name must not contain '@' (that is a snapshot)".into());
    }
    let parts: Vec<&str> = trimmed.split('/').collect();
    if parts.len() < 2 {
        return Err(format!("dataset {trimmed:?} must be <pool>/<name>"));
    }
    if !is_valid_pool_name(parts[0]) {
        return Err(format!("invalid pool name {:?}", parts[0]));
    }
    for part in &parts[1..] {
        if part.is_empty() || *part == "." || *part == ".." {
            return Err(format!("invalid dataset path {trimmed:?}"));
        }
        if part.chars().any(char::is_control) {
            return Err(format!("invalid dataset path {trimmed:?}"));
        }
    }
    Ok(())
}

/// Validate a snapshot name (the part after `@`). Mirrors the agent's
/// validator so a bad name is refused before it reaches a command line.
pub fn validate_snapshot_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("snapshot name is required".into());
    }
    if name != name.trim() {
        return Err("snapshot name must not start or end with whitespace".into());
    }
    if name.starts_with('-') {
        return Err(format!("snapshot name {name:?} must not start with '-'"));
    }
    if name.contains('@') || name.contains(' ') || name.contains('/') || name.contains('\\') {
        return Err(format!(
            "snapshot name {name:?} must not contain '@', '/', '\\' or spaces"
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '+' | '-'))
    {
        return Err(format!(
            "snapshot name {name:?} must be letters, digits, '.', '_', ':', '+' or '-'"
        ));
    }
    Ok(())
}

/// Validate a dataset create request's options.
pub fn validate_dataset_create_options(options: &HashMap<String, String>) -> Result<(), String> {
    // Encryption shape is checked first: a malformed request gets a precise
    // error, and a well-formed one is refused until key custody, unlock/lock
    // and encrypted send ship (FR-ENC; docs/dataset-encryption.md).
    crate::dataset_encryption::validate_create_options(options)?;
    for (key, raw) in options {
        let value = raw.trim();
        match key.as_str() {
            "volblocksize" | "volsize" | "volmode" => {
                return Err(format!("{key} is a zvol property; create a zvol instead"))
            }
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
            // Shape validated by `dataset_encryption::validate_create_options`
            // above; the agent owns `keylocation` and the key itself.
            "encryption" | "keyformat" => {}
            _ => return Err(format!("unsupported dataset option {key:?}")),
        }
    }
    Ok(())
}

/// Pool-create options. Encryption is create-only in ZFS: `zpool create`
/// cannot apply it and a follow-up `zfs set encryption=…` always fails, so the
/// request is refused before any disk is wiped (R-043).
pub fn validate_pool_create_options(options: &HashMap<String, String>) -> Result<(), String> {
    for key in options.keys() {
        if matches!(
            key.as_str(),
            "encryption" | "keyformat" | "keylocation" | "encryptionroot"
        ) {
            return Err(format!(
                "{key} is a create-only encryption property; a pool cannot be created with it (create the pool, then create encrypted child datasets)"
            ));
        }
        if matches!(key.as_str(), "volsize" | "volblocksize" | "volmode") {
            return Err(format!(
                "{key} is a zvol property and cannot be set on a pool"
            ));
        }
    }
    validate_dataset_create_options(options)
}

/// Map a partition device (`/dev/sdc2`, `/dev/nvme0n1p1`) to its whole disk.
/// Returns `None` for a whole disk or a non-partition device.
pub(crate) fn whole_disk_path(dev: &str) -> Option<String> {
    let name = dev.strip_prefix("/dev/")?;
    if name.starts_with("nvme") || name.starts_with("mmcblk") {
        let pos = name.rfind('p')?;
        let base = &name[..pos];
        if base.chars().last().is_some_and(|c| c.is_ascii_digit()) {
            return Some(format!("/dev/{base}"));
        }
        return None;
    }
    if name.starts_with("sd") || name.starts_with("vd") || name.starts_with("hd") {
        let base = name.trim_end_matches(|c: char| c.is_ascii_digit());
        if base.len() < name.len() && base.len() > 2 {
            return Some(format!("/dev/{base}"));
        }
    }
    None
}

/// The pure part of the disk check.
///
/// `signatures` maps a whole disk to the filesystem types found on it;
/// `wipe_confirmed` lists the disks the operator explicitly accepted erasing.
/// A ZFS signature (`zfs` in Talos's discovered volume names, `zfs_member` on
/// other tools) is never overridable: `wipefs --all` would destroy the label of
/// a pool that `zpool import` could still recover.
pub fn validate_disk_selection(
    disks: &[String],
    usable: &std::collections::HashSet<String>,
    in_pool: &HashMap<String, String>,
    signatures: &HashMap<String, Vec<String>>,
    wipe_confirmed: &[String],
) -> Result<(), String> {
    let confirmed: std::collections::HashSet<&str> =
        wipe_confirmed.iter().map(|d| d.trim()).collect();
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
        if let Some(sigs) = signatures.get(dev) {
            if sigs.iter().any(|s| s == "zfs" || s == "zfs_member") {
                return Err(format!(
                    "disk {dev} holds a ZFS pool label; import or destroy that pool first"
                ));
            }
            if !sigs.is_empty() && !confirmed.contains(dev) {
                return Err(format!(
                    "disk {dev} already contains data ({}) and will be erased; confirm the wipe for this disk",
                    sigs.join(", ")
                ));
            }
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
    /// Disks the operator explicitly accepted erasing (see
    /// `validate_disk_selection`).
    #[serde(default, rename = "wipeConfirmed")]
    wipe_confirmed: Vec<String>,
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
    if let Err(e) = validate_pool_create_options(&req.options) {
        return bad_request(&e);
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
    if let Err(e) = validate_add_disks(&state, &topology, &req.disks, &req.wipe_confirmed).await {
        return bad_request(&e);
    }
    if !req.cache.is_empty() {
        if let Err(e) = validate_add_disks(
            &state,
            "single",
            std::slice::from_ref(&req.cache),
            &req.wipe_confirmed,
        )
        .await
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
        Ok(report) => json(
            StatusCode::CREATED,
            serde_json::json!({
                "status": if report.partial { "pool created with warnings" } else { "pool created" },
                "pool": req.name,
                "topology": req.topology,
                "partial": report.partial,
                "warnings": report.warnings,
                "repair": report.repair,
            }),
        ),
        Err(e) => write_agent_error(&e),
    }
}

#[derive(Deserialize, Default)]
struct ImportBody {
    #[serde(default)]
    name: String,
    /// Explicit confirmation for `zpool import -f`.
    #[serde(default)]
    force: bool,
    /// Must repeat the pool name when `force` is set.
    #[serde(default)]
    confirm: String,
}

/// The pool name to import. An empty (or malformed) body is an error: the
/// agent refuses an empty name (import-all needs an explicit `all`), and the
/// API must never synthesize one by accident (AUD-2611-T4-1).
fn import_pool_name(req: ImportBody) -> Result<String, String> {
    if req.name.trim().is_empty() {
        return Err("name is required".to_string());
    }
    if !is_valid_pool_name(&req.name) {
        return Err(format!("invalid pool name {:?}", req.name));
    }
    Ok(req.name)
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
    // A malformed body must not degrade to an empty name: the agent refuses
    // that (it requires an explicit `all`), but the API should say so itself
    // instead of hiding the decode error (AUD-2611-T4-1).
    let req: ImportBody = match read_json(body).await {
        Ok(req) => req,
        Err(resp) => return resp,
    };
    let force = req.force;
    let confirm = req.confirm.trim().to_string();
    if force && confirm != req.name.trim() {
        return bad_request(
            "a forced import requires confirm=<pool name>; it can take over a pool another node still serves",
        );
    }
    let name = match import_pool_name(req) {
        Ok(name) => name,
        Err(e) => return bad_request(&e),
    };
    match client.import_pool(&name, force, &confirm).await {
        Ok(()) => json(
            StatusCode::OK,
            serde_json::json!({ "status": "pool imported", "pool": name }),
        ),
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
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if !is_valid_pool_name(&pool) {
        return bad_request(&format!("invalid pool name {pool:?}"));
    }
    if query.get("confirm").map(String::as_str) != Some(pool.as_str()) {
        return bad_request("destroying a pool requires confirm=<pool name>");
    }
    let force = query.get("force").map(|v| v == "true").unwrap_or(false);
    let client = match agent(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match client.delete_pool(&pool, force).await {
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
    #[serde(default, rename = "wipeConfirmed")]
    wipe_confirmed: Vec<String>,
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
    if let Err(e) = validate_add_disks(&state, &req.topology, &req.disks, &req.wipe_confirmed).await
    {
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
    wipe_confirmed: &[String],
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

    // Fail closed: if ownership or signatures cannot be read we cannot prove
    // the disks are free, and these operations are irreversible.
    let signatures = talos
        .get_discovered_signatures()
        .await
        .map_err(|e| format!("cannot verify disk contents: {e}"))?;

    let mut in_pool = HashMap::new();
    if let Some(client) = state.agent.clone() {
        let pools = client
            .list_pools()
            .await
            .map_err(|e| format!("cannot verify disk ownership: {e}"))?;
        for p in pools {
            for d in p.disks {
                if let Some(whole) = whole_disk_path(&d) {
                    in_pool.entry(whole).or_insert_with(|| p.name.clone());
                }
                in_pool.insert(d, p.name.clone());
            }
        }
    }

    validate_disk_selection(disks, &usable, &in_pool, &signatures, wipe_confirmed)
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

/// Serializes encrypted creates so the "dataset absent / Secret absent" checks
/// and the two writes cannot interleave. The API runs one replica, but a
/// second create for the same name must still see the first one's Secret.
static DATASET_CREATE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
    let plan = match dataset_encryption::parse_plan(&req.options) {
        Ok(plan) => plan,
        Err(e) => return bad_request(&e),
    };
    let full = format!("{}/{}", req.pool, req.name);
    // Hold this for the whole check → Secret → create sequence. The agent
    // locks the same name too, so a destroy cannot race the create.
    let _guard = lock_dataset(&full).await;
    if let Err(e) = require_pool(&state, &req.pool).await {
        return bad_request(&e);
    }
    let client = match agent(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };

    let Some(plan) = plan else {
        return match client
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
        };
    };

    if plan.key_format != KeyFormat::Raw {
        return bad_request(dataset_encryption::PASSPHRASE_NOT_IMPLEMENTED);
    }

    // The API runs one replica; this lock still prevents an encrypted create
    // from interleaving with a key check in the same process.
    let _key_guard = DATASET_CREATE_LOCK.lock().await;
    let datasets = match client.list_datasets().await {
        Ok(datasets) => datasets,
        Err(e) => return write_agent_error(&e),
    };
    if datasets.iter().any(|d| d.name == full) {
        return json(
            StatusCode::CONFLICT,
            serde_json::json!({ "error": format!("dataset {full} already exists") }),
        );
    }

    let Some(kube) = state.kube.as_ref() else {
        return json(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({ "error": "dataset encryption needs the Kubernetes client; none is configured" }),
        );
    };
    let namespace = dataset_encryption::keys_namespace();
    let secret_name = match dataset_encryption::secret_name_for_dataset(&full) {
        Ok(name) => name,
        Err(e) => return bad_request(&e),
    };
    match kube.get_json("secret", &secret_name, &namespace).await {
        Ok(None) => {}
        Ok(Some(_)) => {
            return json(
                StatusCode::CONFLICT,
                serde_json::json!({ "error": format!("the dataset key Secret {secret_name} already exists; refusing to overwrite it") }),
            )
        }
        Err(e) => {
            return json(
                StatusCode::BAD_GATEWAY,
                serde_json::json!({ "error": format!("checking the dataset key Secret: {e}") }),
            )
        }
    }

    let key = match RawKey::generate() {
        Ok(key) => key,
        Err(e) => {
            return json(
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({ "error": e }),
            )
        }
    };
    let object = match dataset_encryption::key_secret_object(&full, &key, &namespace) {
        Ok(object) => object,
        Err(e) => return bad_request(&e),
    };
    if let Err(e) = kube.apply(&object).await {
        return json(
            StatusCode::BAD_GATEWAY,
            serde_json::json!({ "error": format!("writing the dataset key Secret: {e}") }),
        );
    }

    match client
        .create_dataset_with_key(&req.pool, &req.name, &req.options, key.as_bytes())
        .await
    {
        Ok(()) => {
            tracing::info!(
                "created encrypted dataset {} ({})",
                logsafe::field(&full),
                plan.algorithm.as_zfs_value()
            );
            json(
                StatusCode::CREATED,
                serde_json::json!({
                    "status": "dataset created",
                    "name": full,
                    "encryption": plan.algorithm.as_zfs_value(),
                    "keystatus": "available"
                }),
            )
        }
        Err(e) => {
            // The agent rolls back its own partial dataset. If the dataset is
            // still absent, the key has no owner and can be removed; if it
            // exists (or existence cannot be checked), keep the Secret: deleting
            // the only key copy would make the dataset unrecoverable.
            let exists = match client.list_datasets().await {
                Ok(datasets) => datasets.iter().any(|d| d.name == full),
                Err(_) => true,
            };
            if !exists {
                if let Err(err) = kube.delete("secret", &secret_name, &namespace).await {
                    tracing::warn!(
                        "could not remove the unused dataset key Secret {}: {}",
                        logsafe::field(&secret_name),
                        err
                    );
                }
            } else {
                tracing::error!(
                    "encrypted dataset create reported an error but {} exists; the key Secret was kept",
                    logsafe::field(&full)
                );
            }
            write_agent_error(&e)
        }
    }
}

/// Whether a dataset reports an encrypted state from `zfs list`.
fn is_encrypted_dataset(dataset: &crate::agent::Dataset) -> bool {
    !dataset.encryption.is_empty() && dataset.encryption != "off" && dataset.encryption != "-"
}

fn encryption_root_of(dataset: &crate::agent::Dataset) -> String {
    if dataset.encryption_root.is_empty() {
        dataset.name.clone()
    } else {
        dataset.encryption_root.clone()
    }
}

/// The encryption roots whose keys belong to the destroyed subtree.
///
/// A child that merely inherits encryption reports an ancestor as its
/// `encryptionRoot`; that ancestor survives the destroy and keeps the key its
/// other children need, so only roots inside `<name>/` (or the dataset itself)
/// are eligible for Secret deletion.
fn encryption_roots_in_subtree(
    datasets: &[crate::agent::Dataset],
    name: &str,
) -> (bool, std::collections::BTreeSet<String>) {
    let prefix = format!("{name}/");
    let encrypted_subtree = datasets
        .iter()
        .filter(|d| d.name == name || d.name.starts_with(&prefix))
        .any(is_encrypted_dataset);
    let roots = datasets
        .iter()
        .filter(|d| d.name == name || d.name.starts_with(&prefix))
        .filter(|d| is_encrypted_dataset(d))
        .map(encryption_root_of)
        .filter(|root| root == name || root.starts_with(&prefix))
        .collect();
    (encrypted_subtree, roots)
}

/// Read and validate the key Secret for one encryption root.
#[allow(clippy::result_large_err)]
pub(crate) async fn read_dataset_key(state: &AppState, root: &str) -> Result<RawKey, Response> {
    let kube = state.kube.as_ref().ok_or_else(|| {
        json(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({ "error": "dataset encryption needs the Kubernetes client; none is configured" }),
        )
    })?;
    let namespace = dataset_encryption::keys_namespace();
    let secret_name =
        dataset_encryption::secret_name_for_dataset(root).map_err(|e| bad_request(&e))?;
    match kube.get_json("secret", &secret_name, &namespace).await {
        Ok(Some(secret)) => dataset_encryption::parse_key_secret(&secret, root)
            .map_err(|e| json(StatusCode::CONFLICT, serde_json::json!({ "error": e }))),
        Ok(None) => Err(json(
            StatusCode::CONFLICT,
            serde_json::json!({
                "error": format!("the key Secret for {root} is missing; restore it from your Secret backup before unlocking")
            }),
        )),
        Err(e) => Err(json(
            StatusCode::BAD_GATEWAY,
            serde_json::json!({ "error": format!("reading the dataset key Secret: {e}") }),
        )),
    }
}

/// `GET /api/datasets/keystatus?name=<pool>/<dataset>` — authoritative
/// `zfs get` state for one dataset.
pub async fn datasets_keystatus_get(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let name = query
        .get("name")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if let Err(e) = validate_dataset_path(&name) {
        return bad_request(&e);
    }
    let client = match agent(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match client.encryption_status(&name).await {
        Ok(status) => json(StatusCode::OK, serde_json::to_value(status).unwrap()),
        Err(e) => write_agent_error(&e),
    }
}

#[derive(Default, Deserialize)]
struct DatasetNameBody {
    #[serde(default)]
    name: String,
}

/// `POST /api/datasets/unlock` — explicit administrator unlock after boot.
/// There is no automatic unlock; the key comes from the dataset key Secret.
pub async fn datasets_unlock_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let req: DatasetNameBody = match read_json(body).await {
        Ok(req) => req,
        Err(resp) => return resp,
    };
    if let Err(e) = validate_dataset_path(&req.name) {
        return bad_request(&e);
    }
    let client = match agent(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let status = match client.encryption_status(&req.name).await {
        Ok(status) => status,
        Err(e) => return write_agent_error(&e),
    };
    if !status.is_encrypted() {
        return bad_request("dataset is not encrypted");
    }
    if status.keystatus == "available" {
        return json(
            StatusCode::OK,
            serde_json::json!({
                "status": "unlocked", "name": status.name,
                "keystatus": status.keystatus, "alreadyUnlocked": true
            }),
        );
    }
    let root = if status.encryption_root.is_empty() {
        status.name.clone()
    } else {
        status.encryption_root.clone()
    };
    let key = match read_dataset_key(&state, &root).await {
        Ok(key) => key,
        Err(resp) => return resp,
    };
    if let Err(e) = client.unlock_dataset(&root, key.as_bytes()).await {
        return write_agent_error(&e);
    }
    let after = match client.encryption_status(&req.name).await {
        Ok(status) => status,
        Err(e) => return write_agent_error(&e),
    };
    tracing::info!(
        "unlocked encrypted dataset {} (keystatus={})",
        logsafe::field(&root),
        after.keystatus
    );
    json(
        StatusCode::OK,
        serde_json::json!({
            "status": "unlocked", "name": after.name, "keystatus": after.keystatus
        }),
    )
}

/// `POST /api/datasets/lock` — unload the key; refused while mounted.
pub async fn datasets_lock_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let req: DatasetNameBody = match read_json(body).await {
        Ok(req) => req,
        Err(resp) => return resp,
    };
    if let Err(e) = validate_dataset_path(&req.name) {
        return bad_request(&e);
    }
    let client = match agent(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let status = match client.encryption_status(&req.name).await {
        Ok(status) => status,
        Err(e) => return write_agent_error(&e),
    };
    if !status.is_encrypted() {
        return bad_request("dataset is not encrypted");
    }
    let root = if status.encryption_root.is_empty() {
        status.name.clone()
    } else {
        status.encryption_root.clone()
    };
    if let Err(e) = client.lock_dataset(&root).await {
        return write_agent_error(&e);
    }
    let after = match client.encryption_status(&req.name).await {
        Ok(status) => status,
        Err(e) => return write_agent_error(&e),
    };
    tracing::info!(
        "locked encrypted dataset {} (keystatus={})",
        logsafe::field(&root),
        after.keystatus
    );
    json(
        StatusCode::OK,
        serde_json::json!({
            "status": "locked", "name": after.name, "keystatus": after.keystatus
        }),
    )
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
    // Every destroy is irreversible: no dataset type gets a confirmation-free
    // path (R-004).
    if query.get("confirm").map(String::as_str) != Some(name.as_str()) {
        return bad_request(
            "destroying a dataset requires confirm=<dataset name>; the data and its snapshots are irrecoverable",
        );
    }
    let client = match agent(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };

    // Serialize the whole check → GUID verification → destroy → key deletion
    // sequence for this name. The agent locks the name as well.
    let _guard = lock_dataset(&name).await;

    // A zvol and a filesystem can share a name; only /api/zvols may destroy a
    // block volume.
    let zvols = match client.list_zvols().await {
        Ok(zvols) => zvols,
        Err(e) => return write_agent_error(&e),
    };
    if zvols.iter().any(|z| z.name == name) {
        return json(
            StatusCode::CONFLICT,
            serde_json::json!({
                "error": format!("{name} is a zvol; block volumes are only deletable through /api/zvols")
            }),
        );
    }

    let datasets = match client.list_datasets().await {
        Ok(datasets) => datasets,
        Err(e) => return write_agent_error(&e),
    };
    let Some(target) = datasets.iter().find(|d| d.name == name).cloned() else {
        return json(
            StatusCode::NOT_FOUND,
            serde_json::json!({ "error": format!("dataset {name} does not exist") }),
        );
    };
    if let Err((status, message)) = require_destroy_unblocked(&state, &name, &datasets) {
        return json(status, serde_json::json!({ "error": message }));
    }

    let (encrypted_subtree, encrypted_roots) = encryption_roots_in_subtree(&datasets, &name);
    let root_guids: Vec<(String, String)> = datasets
        .iter()
        .filter(|d| encrypted_roots.contains(&d.name))
        .map(|d| (d.name.clone(), d.guid.clone()))
        .collect();

    // Re-validate the path and re-verify the GUIDs immediately before the
    // destructive agent call: ownership checks above are asynchronous guesses
    // about live state, and the key Secret may only be deleted for the exact
    // dataset generation that was verified here.
    if let Err(e) = validate_dataset_path(&name) {
        return bad_request(&e);
    }
    if query.get("confirm").map(String::as_str) != Some(name.as_str()) {
        return bad_request("destroying a dataset requires confirm=<dataset name>");
    }
    let current = match client.list_datasets().await {
        Ok(datasets) => datasets,
        Err(e) => return write_agent_error(&e),
    };
    let Some(current_target) = current.iter().find(|d| d.name == name) else {
        return json(
            StatusCode::CONFLICT,
            serde_json::json!({ "error": format!("dataset {name} disappeared; retry the request") }),
        );
    };
    if !target.guid.is_empty() && current_target.guid != target.guid {
        return json(
            StatusCode::CONFLICT,
            serde_json::json!({
                "error": format!("dataset {name} changed or was recreated; retry the request")
            }),
        );
    }
    for (root, guid) in &root_guids {
        let unchanged = current
            .iter()
            .find(|d| d.name == *root)
            .is_some_and(|d| d.guid == *guid || guid.is_empty());
        if !unchanged {
            return json(
                StatusCode::CONFLICT,
                serde_json::json!({
                    "error": format!("encryption root {root} changed while the destroy was being prepared; retry the request")
                }),
            );
        }
    }
    let expected_guid = (!target.guid.is_empty()).then_some(target.guid.as_str());

    match client
        .destroy_dataset_with_guid(&name, recursive, expected_guid)
        .await
    {
        Ok(()) => {
            tracing::info!(
                "destroyed dataset {} (recursive={}, encrypted={})",
                logsafe::field(&name),
                recursive,
                encrypted_subtree
            );
            // `true` only when there was nothing to delete or every delete
            // succeeded; a missing Kubernetes client with roots to delete is
            // not a successful crypto-shred. Only roots whose GUIDs were
            // verified above are removed.
            let mut keys_deleted = encrypted_roots.is_empty();
            if let Some(kube) = state.kube.as_ref() {
                keys_deleted = true;
                let namespace = dataset_encryption::keys_namespace();
                for root in &encrypted_roots {
                    let Ok(secret_name) = dataset_encryption::secret_name_for_dataset(root) else {
                        keys_deleted = false;
                        continue;
                    };
                    if let Err(e) = kube.delete("secret", &secret_name, &namespace).await {
                        keys_deleted = false;
                        tracing::warn!(
                            "dataset {} was destroyed but its key Secret {} could not be deleted: {}",
                            logsafe::field(root),
                            logsafe::field(&secret_name),
                            e
                        );
                    }
                }
            }
            json(
                StatusCode::OK,
                serde_json::json!({
                    "status": "dataset destroyed",
                    "name": name,
                    "guid": target.guid,
                    "keysDeleted": keys_deleted
                }),
            )
        }
        Err(e) => write_agent_error(&e),
    }
}

/// Why a live consumer owns this dataset, if any. Runs after a fresh dataset
/// list: the share store and the app dataset registry must both be loaded
/// (fail closed — an unreadable store proves nothing), and VM, share and
/// registered-app ownership are matched by path in both directions.
///
/// Shared with local replication: a forced full receive destroys the
/// destination's contents exactly like a destroy, so it must pass the same
/// ownership guards.
pub(crate) fn dataset_consumer_conflict(
    state: &AppState,
    name: &str,
    datasets: &[crate::agent::Dataset],
) -> Result<(), String> {
    if let Some(owner) = vm_dataset_owner(state, name) {
        return Err(format!(
            "dataset {name} is owned by {owner}; delete the VM through /api/virt/vms instead"
        ));
    }

    if !state.datasets_registry.loaded() {
        return Err(
            "the dataset registry is not loaded; refusing to treat the dataset as unattached without proving no app mounts it".to_string(),
        );
    }

    let mountpoints = subtree_mountpoints(datasets, name);

    for ds in state.datasets_registry.list() {
        if ds.attached_to.is_empty() {
            continue;
        }
        let host = ds.host_path.trim_end_matches('/');
        if mountpoints.iter().any(|mp| paths_overlap(mp, host)) {
            return Err(format!(
                "dataset {name} backs {host}, which is attached to app(s) {}; detach the dataset first",
                ds.attached_to.join(", ")
            ));
        }
    }

    {
        let shares = state.shares.lock().unwrap();
        if !shares.loaded() {
            return Err(
                "the share configuration is not loaded; refusing to destroy a dataset without proving no share serves it".to_string(),
            );
        }
        for share in shares.list() {
            if mountpoints
                .iter()
                .any(|mp| paths_overlap(mp, share.path.trim_end_matches('/')))
            {
                return Err(format!(
                    "dataset {name} overlaps path {}, which share {:?} is serving; delete or repoint the share first",
                    share.path, share.name
                ));
            }
        }
    }

    Ok(())
}

/// Refuse a destroy while any live consumer owns the dataset. Only a job that
/// is still writing blocks: finished jobs remain in the bounded history for
/// the UI, but a completed replication must not freeze both datasets forever.
fn require_destroy_unblocked(
    state: &AppState,
    name: &str,
    datasets: &[crate::agent::Dataset],
) -> Result<(), (StatusCode, String)> {
    if let Err(message) = dataset_consumer_conflict(state, name, datasets) {
        return Err((StatusCode::CONFLICT, message));
    }

    for job in state.replication_jobs.list() {
        if !job.is_running() {
            continue;
        }
        if paths_overlap(job.destination(), name) || paths_overlap(job.source(), name) {
            return Err((
                StatusCode::CONFLICT,
                format!(
                    "dataset {name} is being written by replication job {} ({} -> {}); wait for it to finish or cancel the job",
                    job.id(),
                    job.source(),
                    job.destination()
                ),
            ));
        }
    }

    Ok(())
}

// ---- zvols ---------------------------------------------------------------

#[derive(Default, Deserialize)]
struct CreateZvolBody {
    #[serde(default)]
    pool: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    size: String,
    #[serde(default)]
    options: HashMap<String, String>,
}

pub async fn zvols_get(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let client = match agent(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let pool = query.get("pool").map(|p| p.trim()).unwrap_or_default();
    if !pool.is_empty() && !is_valid_pool_name(pool) {
        return bad_request(&format!("invalid pool name {pool:?}"));
    }
    let result = if pool.is_empty() {
        client.list_zvols().await
    } else {
        client.list_pool_zvols(pool).await
    };
    match result {
        Ok(zvols) => json(StatusCode::OK, serde_json::to_value(zvols).unwrap()),
        Err(e) => write_agent_error(&e),
    }
}

pub async fn zvols_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let req: CreateZvolBody = match read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    if !is_valid_pool_name(&req.pool) {
        return bad_request(&format!("invalid pool name {:?}", req.pool));
    }
    if let Err(e) = validate_dataset_name(&req.name) {
        return bad_request(&e);
    }
    if let Err(e) = validate_zvol_size(&req.size) {
        return bad_request(&e);
    }
    if let Err(e) = validate_zvol_options(&req.options) {
        return bad_request(&e);
    }
    if let Err(e) = require_pool(&state, &req.pool).await {
        return bad_request(&e);
    }
    let client = match agent(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let name = format!("{}/{}", req.pool, req.name);
    let _guard = lock_dataset(&name).await;
    match client.create_zvol(&name, &req.size, &req.options).await {
        Ok(()) => {
            tracing::info!("created zvol {}", logsafe::field(&name));
            json(
                StatusCode::CREATED,
                serde_json::json!({ "status": "zvol created", "name": name }),
            )
        }
        Err(e) => write_agent_error(&e),
    }
}

#[derive(Default, Deserialize)]
struct ResizeZvolBody {
    #[serde(default)]
    size: String,
}

pub async fn zvols_resize(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
    body: Body,
) -> Response {
    let name = query
        .get("name")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if let Err(e) = validate_dataset_path(&name) {
        return bad_request(&e);
    }
    let req: ResizeZvolBody = match read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    if let Err(e) = validate_zvol_size(&req.size) {
        return bad_request(&e);
    }
    let client = match agent(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let _guard = lock_dataset(&name).await;
    match client.resize_zvol(&name, &req.size).await {
        Ok(()) => {
            tracing::info!(
                "resized zvol {} to {}",
                logsafe::field(&name),
                logsafe::field(req.size.trim())
            );
            json(
                StatusCode::OK,
                serde_json::json!({
                    "status": "zvol resized", "name": name, "size": req.size.trim()
                }),
            )
        }
        Err(e) => write_agent_error(&e),
    }
}

pub async fn zvols_delete(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let name = query
        .get("name")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if let Err(e) = validate_dataset_path(&name) {
        return bad_request(&e);
    }
    if query.get("confirm").map(String::as_str) != Some(name.as_str()) {
        return bad_request("destroying a zvol requires confirm=<zvol name>");
    }
    let client = match agent(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let _guard = lock_dataset(&name).await;
    match client.delete_zvol(&name, &name).await {
        Ok(()) => {
            tracing::info!("destroyed zvol {}", logsafe::field(&name));
            json(
                StatusCode::OK,
                serde_json::json!({ "status": "zvol destroyed", "name": name }),
            )
        }
        Err(e) => write_agent_error(&e),
    }
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
        // Existing ZFS/TrueNAS system datasets are dot-prefixed; manage them
        // (delete/snapshot/backup) even though new ones cannot be created.
        assert!(validate_dataset_path("Data/.system").is_ok());
        assert!(validate_dataset_path("Data/.system/configs-ae32c386").is_ok());
        assert!(validate_dataset_name(".system").is_err());
        assert!(validate_dataset_path("Data/../etc").is_err());
        assert!(validate_dataset_path("Data/..").is_err());
        assert_eq!(whole_disk_path("/dev/sdc2").as_deref(), Some("/dev/sdc"));
        assert_eq!(
            whole_disk_path("/dev/nvme0n1p1").as_deref(),
            Some("/dev/nvme0n1")
        );
        assert_eq!(whole_disk_path("/dev/sdc"), None);
        assert_eq!(whole_disk_path("/dev/zd32"), None);
    }

    #[test]
    fn zvol_sizes_are_not_rounded_into_validity() {
        assert_eq!(validate_zvol_size("10G").unwrap(), 10 * 1024 * 1024 * 1024);
        assert_eq!(validate_zvol_size("512").unwrap(), 512);
        for bad in ["0.1", "1.5", "0", "-1G", "lots"] {
            assert!(validate_zvol_size(bad).is_err(), "{bad}");
        }
        for ok in ["512", "4K", "16M"] {
            assert!(validate_volblocksize(ok).is_ok(), "{ok}");
        }
        // 1023.5 rounds to 1024 but is not a power-of-two size the user typed.
        for bad in ["1023.5", "1023", "3M"] {
            assert!(validate_volblocksize(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn snapshot_names_cannot_become_flags_or_paths() {
        assert!(validate_snapshot_name("naslos-rep-abcd-20260101T000000Z").is_ok());
        assert!(validate_snapshot_name("manual.2026-01-01_10:00+00").is_ok());
        assert!(validate_snapshot_name("-F").is_err());
        assert!(validate_snapshot_name("snap@other").is_err());
        assert!(validate_snapshot_name("snap name").is_err());
        assert!(validate_snapshot_name("snap/child").is_err());
        assert!(validate_snapshot_name("").is_err());
        assert!(validate_snapshot_name(" snap").is_err());
    }

    #[test]
    fn disk_selection_rules() {
        let usable: std::collections::HashSet<String> =
            ["/dev/sdb".to_string()].into_iter().collect();
        let mut in_pool = HashMap::new();
        in_pool.insert("/dev/sdc".to_string(), "other".to_string());

        let no_sigs = HashMap::new();
        let none: Vec<String> = Vec::new();

        assert!(
            validate_disk_selection(&["/dev/sdb".into()], &usable, &in_pool, &no_sigs, &none)
                .is_ok()
        );
        assert!(
            validate_disk_selection(&["sdb".into()], &usable, &in_pool, &no_sigs, &none).is_err()
        );
        assert!(validate_disk_selection(
            &["/dev/sdb".into(), "/dev/sdb".into()],
            &usable,
            &in_pool,
            &no_sigs,
            &none
        )
        .is_err());
        assert!(
            validate_disk_selection(&["/dev/sdc".into()], &usable, &in_pool, &no_sigs, &none)
                .is_err()
        );
        assert!(
            validate_disk_selection(&["/dev/sdz".into()], &usable, &in_pool, &no_sigs, &none)
                .is_err()
        );

        // A formatted disk needs an explicit per-disk confirmation.
        let mut sigs = HashMap::new();
        sigs.insert("/dev/sdb".to_string(), vec!["ext4".to_string()]);
        assert!(
            validate_disk_selection(&["/dev/sdb".into()], &usable, &in_pool, &sigs, &none).is_err()
        );
        assert!(validate_disk_selection(
            &["/dev/sdb".into()],
            &usable,
            &in_pool,
            &sigs,
            &["/dev/sdb".to_string()]
        )
        .is_ok());

        // A ZFS label is never overridable, whether Talos reports it as
        // `zfs` (discoveredvolumes) or another tool as `zfs_member`.
        for sig in ["zfs", "zfs_member"] {
            let mut zfs = HashMap::new();
            zfs.insert("/dev/sdb".to_string(), vec![sig.to_string()]);
            assert!(validate_disk_selection(
                &["/dev/sdb".into()],
                &usable,
                &in_pool,
                &zfs,
                &["/dev/sdb".to_string()]
            )
            .is_err());
        }
    }
    #[test]
    fn whole_disk_paths_match_shared_fixture() {
        for line in include_str!("../../../testdata/zfs_device_paths.txt").lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (input, expected) = line
                .split_once('|')
                .expect("fixture line is input|expected");
            let want = match expected.trim() {
                "" => None,
                value => Some(value.to_string()),
            };
            assert_eq!(whole_disk_path(input.trim()), want, "for {input}");
        }
    }

    #[test]
    fn dataset_validation_matches_shared_fixture() {
        for line in include_str!("../../../testdata/dataset_names.txt").lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split('|');
            let kind = parts.next().expect("kind");
            let name = parts.next().expect("name");
            let expected = parts.next().expect("ok/err") == "ok";
            let actual = match kind {
                "strict" => validate_dataset_name(name).is_ok(),
                "path" => validate_dataset_path(name).is_ok(),
                other => panic!("unknown fixture kind {other}"),
            };
            assert_eq!(actual, expected, "for {kind} {name}");
        }
        assert!(validate_dataset_name(&"a".repeat(201)).is_err());
    }

    #[test]
    fn import_requires_an_explicit_pool_name() {
        assert!(import_pool_name(ImportBody {
            name: String::new(),
            ..Default::default()
        })
        .is_err());
        assert!(import_pool_name(ImportBody {
            name: "  ".into(),
            ..Default::default()
        })
        .is_err());
        assert!(import_pool_name(ImportBody {
            name: "../tank".into(),
            ..Default::default()
        })
        .is_err());
        assert_eq!(
            import_pool_name(ImportBody {
                name: "tank".into(),
                ..Default::default()
            })
            .unwrap(),
            "tank"
        );
    }

    #[test]
    fn encryption_options_accept_raw_keys_and_refuse_passphrases() {
        let mut valid = HashMap::new();
        valid.insert("encryption".to_string(), "aes-256-gcm".to_string());
        valid.insert("keyformat".to_string(), "raw".to_string());
        validate_dataset_create_options(&valid).expect("raw keys are implemented");

        let mut passphrase = HashMap::new();
        passphrase.insert("encryption".to_string(), "on".to_string());
        passphrase.insert("keyformat".to_string(), "passphrase".to_string());
        let err = validate_dataset_create_options(&passphrase).unwrap_err();
        assert!(err.contains("passphrase"), "{err}");
        assert!(err.contains("[OPEN]"), "{err}");

        let mut malformed = HashMap::new();
        malformed.insert("encryption".to_string(), "rot13".to_string());
        malformed.insert("keyformat".to_string(), "raw".to_string());
        let err = validate_dataset_create_options(&malformed).unwrap_err();
        assert!(err.contains("unsupported encryption"), "{err}");

        let mut caller_location = HashMap::new();
        caller_location.insert("encryption".to_string(), "aes-256-gcm".to_string());
        caller_location.insert("keyformat".to_string(), "raw".to_string());
        caller_location.insert("keylocation".to_string(), "file:///tmp/key".to_string());
        let err = validate_dataset_create_options(&caller_location).unwrap_err();
        assert!(err.contains("keylocation"), "{err}");
    }

    #[test]
    fn key_material_is_never_a_dataset_option() {
        for key in ["key", "passphrase", "encryption_key"] {
            let options = HashMap::from([(key.to_string(), "secret".to_string())]);
            let err = validate_dataset_create_options(&options).unwrap_err();
            assert!(err.contains("unsupported dataset option"), "{key}: {err}");
            assert!(!err.contains("secret"), "the error echoed the value: {err}");
        }
    }

    fn encrypted_dataset(name: &str, root: &str) -> crate::agent::Dataset {
        crate::agent::Dataset {
            name: name.to_string(),
            encryption: "aes-256-gcm".to_string(),
            keystatus: "available".to_string(),
            encryption_root: root.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn inherited_child_destroy_keeps_the_ancestor_key() {
        let datasets = vec![
            encrypted_dataset("tank/secure", "tank/secure"),
            encrypted_dataset("tank/secure/docs", "tank/secure"),
            encrypted_dataset("tank/secure/docs/child", "tank/secure"),
        ];

        let (encrypted, roots) = encryption_roots_in_subtree(&datasets, "tank/secure/docs");
        assert!(encrypted, "the subtree is encrypted");
        assert!(
            roots.is_empty(),
            "the surviving ancestor key must not be deleted: {roots:?}"
        );
    }

    #[test]
    fn root_destroy_selects_only_roots_inside_the_subtree() {
        let datasets = vec![
            encrypted_dataset("tank/secure", "tank/secure"),
            encrypted_dataset("tank/secure/docs", "tank/secure"),
            encrypted_dataset("tank/secure/docs/own", "tank/secure/docs/own"),
        ];

        let (encrypted, roots) = encryption_roots_in_subtree(&datasets, "tank/secure");
        assert!(encrypted);
        assert_eq!(
            roots,
            std::collections::BTreeSet::from([
                "tank/secure".to_string(),
                "tank/secure/docs/own".to_string(),
            ])
        );
    }
    fn plain_dataset(name: &str, mountpoint: &str, guid: &str) -> crate::agent::Dataset {
        crate::agent::Dataset {
            name: name.to_string(),
            mountpoint: mountpoint.to_string(),
            guid: guid.to_string(),
            encryption: "off".to_string(),
            ..Default::default()
        }
    }

    fn vm_spec(name: &str, pool: &str) -> naslos_vmd::model::VmSpec {
        naslos_vmd::model::VmSpec {
            name: name.into(),
            pool: pool.into(),
            cpu: 1,
            mem_mib: 256,
            autostart: false,
            firmware: naslos_vmd::model::Firmware::default(),
            secure_boot: false,
            desired: naslos_vmd::model::DesiredState::default(),
            disk: naslos_vmd::model::DiskSpec {
                size_gb: 2,
                ..Default::default()
            },
            net: naslos_vmd::model::NetSpec::default(),
            cloud_init: None,
            iso: None,
        }
    }

    #[test]
    fn overlap_uses_component_boundaries_in_both_directions() {
        assert!(paths_overlap("/var/mnt/tank/media", "/var/mnt/tank/media"));
        assert!(paths_overlap("/var/mnt/tank", "/var/mnt/tank/media"));
        assert!(paths_overlap("/var/mnt/tank/media", "/var/mnt/tank"));
        assert!(paths_overlap(
            "/var/mnt/tank/media",
            "/var/mnt/tank/media/sub"
        ));
        assert!(!paths_overlap("/var/mnt/tank2", "/var/mnt/tank"));
        assert!(!paths_overlap("/var/mnt/other", "/var/mnt/tank/media"));
    }

    #[test]
    fn destroy_guard_rejects_a_vm_dataset() {
        let state = AppState::for_test("s");
        state.vms.create(vm_spec("alpha", "tank")).expect("vm");
        let datasets = vec![plain_dataset(
            "tank/vms/alpha",
            "/var/mnt/tank/vms/alpha",
            "1",
        )];
        let err = require_destroy_unblocked(&state, "tank/vms/alpha", &datasets)
            .expect_err("vm dataset must be refused");
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert!(err.1.contains("VM"), "{}", err.1);
    }

    #[test]
    fn destroy_guard_rejects_an_attached_app_dataset() {
        let state = AppState::for_test("s");
        state
            .datasets_registry
            .add(crate::datasets::ManagedDataset {
                name: "media".into(),
                host_path: "/var/mnt/tank/media".into(),
                shared: false,
                warning_acknowledged: false,
                created_at: None,
                attached_to: Vec::new(),
            })
            .expect("register");
        state
            .datasets_registry
            .attach("media", "sonarr")
            .expect("attach");
        let datasets = vec![plain_dataset("tank", "/var/mnt/tank", "1")];
        let err = require_destroy_unblocked(&state, "tank", &datasets)
            .expect_err("an attached app dataset blocks its parent");
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert!(err.1.contains("sonarr"), "{}", err.1);
    }

    #[test]
    fn destroy_guard_rejects_a_share_inside_or_above_the_dataset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shares.json");
        std::fs::write(
            &path,
            r#"[{"name":"media","path":"/var/mnt/tank/media/sub","protocol":"smb","enabled":true}]"#,
        )
        .unwrap();
        let state = AppState::for_test("s");
        *state.shares.lock().unwrap() =
            crate::shares::Manager::new(&path.display().to_string(), "/var/mnt");
        assert!(state.shares.lock().unwrap().loaded());

        let datasets = vec![
            plain_dataset("tank", "/var/mnt/tank", "1"),
            plain_dataset("tank/media", "/var/mnt/tank/media", "2"),
        ];
        // A child share blocks the parent dataset.
        let err = require_destroy_unblocked(&state, "tank", &datasets)
            .expect_err("ancestor share must block");
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert!(err.1.contains("media"), "{}", err.1);
        // And a share path inside the dataset blocks it directly.
        let err = require_destroy_unblocked(&state, "tank/media", &datasets)
            .expect_err("share path inside the dataset must block");
        assert_eq!(err.0, StatusCode::CONFLICT);
    }

    #[test]
    fn destroy_guard_fails_closed_when_shares_are_not_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shares.json");
        std::fs::write(&path, "{not json").unwrap();
        let state = AppState::for_test("s");
        *state.shares.lock().unwrap() =
            crate::shares::Manager::new(&path.display().to_string(), "/var/mnt");
        assert!(!state.shares.lock().unwrap().loaded());
        let datasets = vec![plain_dataset("tank/media", "/var/mnt/tank/media", "1")];
        let err = require_destroy_unblocked(&state, "tank/media", &datasets)
            .expect_err("an unloaded share store must fail closed");
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert!(err.1.contains("not loaded"), "{}", err.1);
    }

    #[test]
    fn destroy_guard_rejects_a_running_replication_source_or_destination() {
        let state = AppState::for_test("s");
        state
            .replication_jobs
            .remember_running_for_test("tank/src", "tank/replica");
        let datasets = vec![plain_dataset("tank/replica", "/var/mnt/tank/replica", "1")];
        let err = require_destroy_unblocked(&state, "tank/replica", &datasets)
            .expect_err("replication destination");
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert!(err.1.contains("replication job"), "{}", err.1);
        let err = require_destroy_unblocked(&state, "tank/src", &datasets)
            .expect_err("replication source");
        assert_eq!(err.0, StatusCode::CONFLICT);
    }

    #[test]
    fn destroy_guard_ignores_a_finished_replication_job() {
        let state = AppState::for_test("s");
        state
            .replication_jobs
            .remember_for_test("tank/src", "tank/replica");
        let datasets = vec![plain_dataset("tank/replica", "/var/mnt/tank/replica", "1")];
        require_destroy_unblocked(&state, "tank/replica", &datasets)
            .expect("a finished job must not block its destination");
        require_destroy_unblocked(&state, "tank/src", &datasets)
            .expect("a finished job must not block its source");
    }

    #[test]
    fn destroy_guard_fails_closed_when_the_dataset_registry_is_not_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("datasets.json");
        std::fs::write(&path, "{not json").unwrap();
        let registry = crate::datasets::Registry::new(&path.display().to_string(), "");
        assert!(!registry.loaded());
        let mut state = AppState::for_test("s");
        state.datasets_registry = std::sync::Arc::new(registry);
        let datasets = vec![plain_dataset("tank/media", "/var/mnt/tank/media", "1")];
        let err = require_destroy_unblocked(&state, "tank/media", &datasets)
            .expect_err("an unloaded registry must fail closed");
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert!(err.1.contains("dataset registry"), "{}", err.1);
    }

    #[test]
    fn pool_create_refuses_create_only_encryption_options() {
        for key in ["encryption", "keyformat", "keylocation"] {
            let options = HashMap::from([(key.to_string(), "aes-256-gcm".to_string())]);
            let err = validate_pool_create_options(&options).unwrap_err();
            assert!(err.contains("create-only encryption"), "{key}: {err}");
        }
        assert!(validate_pool_create_options(&HashMap::from([(
            "compression".to_string(),
            "zstd".to_string()
        )]))
        .is_ok());
    }

    #[tokio::test]
    async fn api_dataset_locks_serialize_the_same_name_only() {
        let guard = lock_dataset("tank/lock-test").await;
        let waiter = tokio::spawn(async {
            let _guard = lock_dataset("tank/lock-test").await;
            true
        });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(!waiter.is_finished(), "the same name must wait");
        drop(guard);
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
                .await
                .expect("released")
                .expect("task")
        );

        let guard = lock_dataset("tank/lock-test").await;
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(200),
                lock_dataset("tank/lock-other")
            )
            .await
            .is_ok(),
            "different names must not block each other"
        );
        drop(guard);
    }
}
