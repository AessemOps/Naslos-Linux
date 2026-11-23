//! VM owner routes (FR-VMT): CRUD, lifecycle actions, snapshots and the
//! image library, all behind the role gate + the proxy secret like every
//! owner route, plus the console WebSocket with the `/api/ws/exec` posture
//! (same-origin check, session caps, plain-GET preflight).

use crate::json;
use crate::state::AppState;
use crate::virt;
use crate::vmd;
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use futures::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::sync::Arc;

use naslos_vmd::model::{validate_snapshot_name, DesiredState, VmSpec};

/// Largest single upload chunk the API relays (the UI sends 8 MiB chunks).
const MAX_CHUNK_BYTES: usize = 64 * 1024 * 1024;

/// Map a supervisor error to the closest API status so a refused overwrite
/// (confirm required, image in use) reaches the caller as a conflict instead
/// of a misleading bad gateway.
fn vmd_error_status(e: &vmd::VmdError) -> StatusCode {
    match e.status {
        409 => StatusCode::CONFLICT,
        400 => StatusCode::BAD_REQUEST,
        404 => StatusCode::NOT_FOUND,
        _ => StatusCode::BAD_GATEWAY,
    }
}

#[allow(clippy::result_large_err)]
fn vmd_client(state: &AppState) -> Result<Arc<vmd::Client>, Response> {
    state.vmd.clone().ok_or_else(|| {
        json(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({
                "error": "virtual machines are not configured on this install (virt.enabled=false or the vmd client is unavailable)"
            }),
        )
    })
}

/// The per-VM lifecycle lock: every operation that can change a VM's runtime
/// state or destroy its dataset (start/stop/restart/delete/restore) holds it,
/// so a delete cannot interleave with a start that was queued behind it.
fn lifecycle_lock(name: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: std::sync::OnceLock<
        std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    > = std::sync::OnceLock::new();
    let locks = LOCKS.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut locks = locks.lock().unwrap();
    locks.entry(name.to_string()).or_default().clone()
}

/// Fail closed: a VM's dataset may only be deleted or rolled back when the
/// supervisor positively reports the VM as stopped. A missing vmd, an
/// unreadable live report or any state other than `stopped` is refused — the
/// old code silently treated all three as "safe". Holding the caller's
/// lifecycle lock keeps the answer from going stale before the destructive
/// call.
#[allow(clippy::result_large_err)]
async fn ensure_stopped_for(
    state: &AppState,
    name: &str,
    action: &str,
) -> Result<Arc<vmd::Client>, Response> {
    let client = vmd_client(state)?;
    let live = client.vms_live().await.map_err(|e| {
        gateway(&format!(
            "cannot verify that VM {name:?} is stopped before you {action} it: {e}"
        ))
    })?;
    if let Some(entry) = live.iter().find(|l| l.name == name) {
        if entry.state != "stopped" {
            return Err(json(
                StatusCode::CONFLICT,
                serde_json::json!({
                    "error": format!(
                        "VM {name:?} is {}; stop it before you {action} it",
                        entry.state
                    )
                }),
            ));
        }
    }
    Ok(client)
}

#[allow(clippy::result_large_err)]
fn agent_client(state: &AppState) -> Result<Arc<crate::agent::Client>, Response> {
    state.agent.clone().ok_or_else(|| {
        json(
            StatusCode::BAD_GATEWAY,
            serde_json::json!({ "error": "agent client not configured" }),
        )
    })
}

/// Render the whole desired config and push it to vmd.
async fn push_config(state: &AppState, boot: bool) -> Result<(), String> {
    let client = state
        .vmd
        .clone()
        .ok_or_else(|| "the VM supervisor is not configured".to_string())?;
    let config = state.vms.render_config(boot);
    client
        .push_config(&config)
        .await
        .map_err(|e| format!("applying the VM configuration: {e}"))
}

// ---- Capabilities --------------------------------------------------------

pub async fn capabilities_get(State(state): State<Arc<AppState>>) -> Response {
    let client = match vmd_client(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match client.capabilities().await {
        Ok(caps) => json(StatusCode::OK, serde_json::to_value(caps).unwrap()),
        Err(e) => json(
            StatusCode::BAD_GATEWAY,
            serde_json::json!({ "error": e.to_string() }),
        ),
    }
}

// ---- CRUD ----------------------------------------------------------------

pub async fn vms_get(State(state): State<Arc<AppState>>) -> Response {
    let records = state.vms.list();
    let live = match state.vmd.clone() {
        Some(client) => client.vms_live().await.ok(),
        None => None,
    };
    let views = virt::merge_views(&records, live.as_deref());
    json(StatusCode::OK, serde_json::to_value(views).unwrap())
}

pub async fn vms_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let spec: VmSpec = match read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    // Validation before any host IO: a bad record never creates a dataset.
    if let Err(e) = naslos_vmd::model::validate_vm(&spec) {
        return bad_request(&e);
    }
    if let Err(e) = state.vms.validate_cross_vm_ports(&spec.name, &spec) {
        return bad_request(&e);
    }
    let agent = match agent_client(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };

    // The dataset must live under a pool that exists.
    let pools = match agent.list_pools().await {
        Ok(pools) => pools,
        Err(e) => {
            return json(
                StatusCode::BAD_GATEWAY,
                serde_json::json!({ "error": format!("cannot list pools: {e}") }),
            )
        }
    };
    if !pools.iter().any(|p| p.name == spec.pool) {
        return json(
            StatusCode::BAD_REQUEST,
            serde_json::json!({ "error": format!("pool {:?} does not exist", spec.pool) }),
        );
    }

    // Bootstrap the pool's VM tree: <pool>/vms and the <pool>/vms/isos
    // library (idempotent).
    if let Err(e) = ensure_dataset(&agent, &format!("{}/vms", spec.pool), None).await {
        return gateway(&e);
    }
    if let Err(e) = ensure_dataset(&agent, &virt::isos_dataset(&spec.pool), None).await {
        return gateway(&e);
    }
    // The per-VM dataset: quota equals the declared disk size (DM-7), so a
    // sparse image can never outgrow its declaration.
    let options = HashMap::from([
        ("quota".to_string(), format!("{}G", spec.disk.size_gb)),
        ("compression".to_string(), "off".to_string()),
    ]);
    if let Err(e) = ensure_dataset(
        &agent,
        &virt::vm_dataset(&spec.pool, &spec.name),
        Some(&options),
    )
    .await
    {
        return gateway(&e);
    }

    let created = match state.vms.create(spec) {
        Ok(vm) => vm,
        Err(e) => return bad_request(&e),
    };
    if let Err(e) = push_config(&state, false).await {
        // The supervisor never saw the VM: roll the record back so the next
        // create does not see a phantom.
        let _ = state.vms.remove(&created.name);
        return json(
            StatusCode::BAD_GATEWAY,
            serde_json::json!({ "error": e.to_string() }),
        );
    }
    json(StatusCode::CREATED, serde_json::to_value(created).unwrap())
}

pub async fn vm_detail_get(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    let Some(spec) = state.vms.get(&name) else {
        return not_found(&name);
    };
    let live = match state.vmd.clone() {
        Some(client) => client.vms_live().await.ok(),
        None => None,
    };
    let mut views = virt::merge_views(std::slice::from_ref(&spec), live.as_deref());
    let Some(view) = views.pop() else {
        return not_found(&name);
    };
    json(StatusCode::OK, serde_json::to_value(view).unwrap())
}

pub async fn vm_detail_put(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    body: Body,
) -> Response {
    let spec: VmSpec = match read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    if let Err(e) = naslos_vmd::model::validate_vm(&spec) {
        return bad_request(&e);
    }
    if let Err(e) = state.vms.validate_cross_vm_ports(&name, &spec) {
        return bad_request(&e);
    }
    // Fields that shape the on-disk image and its dataset are immutable: the
    // supervisor creates the disk once and never migrates it, so accepting a
    // change here would make vms.json lie about what QEMU is actually using.
    let Some(previous) = state.vms.get(&name) else {
        return not_found(&name);
    };
    if spec.pool != previous.pool {
        return conflict(&format!(
            "the pool is immutable after creation (this VM lives in {:?}); create a new VM in the target pool instead",
            previous.pool
        ));
    }
    if spec.disk.format != previous.disk.format || spec.disk.bus != previous.disk.bus {
        return conflict(
            "disk.format and disk.bus are immutable after creation; the existing disk image cannot be migrated in place",
        );
    }
    if spec.disk.source_image != previous.disk.source_image {
        return conflict(
            "disk.sourceImage is immutable after creation; it only seeds the disk on first start",
        );
    }
    if spec.disk.size_gb != previous.disk.size_gb {
        // No resize path exists yet: qemu-img resize and a matching
        // `zfs set quota` would both be needed, and neither is exposed by the
        // vmd/agent APIs. Refuse rather than record a size the guest does not
        // have.
        return conflict(
            "disk.sizeGB is immutable after creation: growing the image needs qemu-img resize plus a dataset quota change, which the supervisor API does not expose yet; recreate the VM with the new size instead",
        );
    }
    // The record only sticks if the supervisor accepts the whole config;
    // otherwise restore the previous spec so vms.json is never left with a
    // config that would fail every later push.
    let updated = match state.vms.update(&name, spec) {
        Ok(vm) => vm,
        Err(e) => return bad_request(&e),
    };
    if let Err(e) = push_config(&state, false).await {
        let _ = state.vms.update(&name, previous);
        return json(
            StatusCode::BAD_GATEWAY,
            serde_json::json!({ "error": e.to_string() }),
        );
    }
    json(StatusCode::OK, serde_json::to_value(updated).unwrap())
}

pub async fn vm_detail_delete(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if !confirmed(&query, &name) {
        return bad_request(&format!(
            "deleting a VM destroys its dataset: retry with ?confirm={name}"
        ));
    }
    // Serialize with every other lifecycle action for this VM, then prove it
    // is stopped. If vmd is absent or its live report cannot be read, the
    // delete is refused (fail closed): an unreadable supervisor says nothing
    // about whether QEMU is writing the image right now.
    let lock = lifecycle_lock(&name);
    let _guard = lock.lock().await;
    if state.vms.get(&name).is_none() {
        return not_found(&name);
    }
    if let Err(resp) = ensure_stopped_for(&state, &name, "delete").await {
        return resp;
    }
    let agent = match agent_client(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };

    // Remove the record first, then the dataset; a failed destroy restores
    // the record so nothing is orphaned. Recursive: the dataset owns the VM's
    // snapshots, and a non-recursive destroy fails with "filesystem has
    // children" the moment the VM ever had one — found by deleting a VM
    // after the live drill's snapshot step.
    let removed = match state.vms.remove(&name) {
        Ok(vm) => vm,
        Err(e) => return bad_request(&e),
    };
    if let Err(e) = agent
        .destroy_dataset(&virt::vm_dataset(&removed.pool, &removed.name), true)
        .await
    {
        let _ = state.vms.create(removed.clone());
        return json(
            StatusCode::BAD_GATEWAY,
            serde_json::json!({ "error": format!("destroying the VM dataset: {e}") }),
        );
    }
    if let Err(e) = push_config(&state, false).await {
        return json(
            StatusCode::BAD_GATEWAY,
            serde_json::json!({ "error": e.to_string() }),
        );
    }
    json(
        StatusCode::OK,
        serde_json::json!({ "status": "deleted", "name": name }),
    )
}

// ---- Lifecycle actions ---------------------------------------------------

pub async fn vm_start_post(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    let lock = lifecycle_lock(&name);
    let _guard = lock.lock().await;
    let Some(previous) = state.vms.get(&name) else {
        return not_found(&name);
    };
    // No supervisor, no start: fail closed before touching the record.
    if let Err(resp) = vmd_client(&state) {
        return resp;
    }
    if let Err(e) = state.vms.set_desired(&name, DesiredState::Running) {
        return bad_request(&e);
    }
    if let Err(e) = push_config(&state, false).await {
        let _ = state.vms.set_desired(&name, previous.desired);
        return json(
            StatusCode::BAD_GATEWAY,
            serde_json::json!({ "error": e.to_string() }),
        );
    }
    json(
        StatusCode::OK,
        serde_json::json!({ "status": "start requested", "name": name }),
    )
}

pub async fn vm_stop_post(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    let lock = lifecycle_lock(&name);
    let _guard = lock.lock().await;
    let Some(previous) = state.vms.get(&name) else {
        return not_found(&name);
    };
    if let Err(resp) = vmd_client(&state) {
        return resp;
    }
    if let Err(e) = state.vms.set_desired(&name, DesiredState::Stopped) {
        return bad_request(&e);
    }
    if let Err(e) = push_config(&state, false).await {
        let _ = state.vms.set_desired(&name, previous.desired);
        return json(
            StatusCode::BAD_GATEWAY,
            serde_json::json!({ "error": e.to_string() }),
        );
    }
    json(
        StatusCode::OK,
        serde_json::json!({ "status": "stop requested (graceful ACPI shutdown)", "name": name }),
    )
}

pub async fn vm_force_stop_post(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    let lock = lifecycle_lock(&name);
    let _guard = lock.lock().await;
    let Some(previous) = state.vms.get(&name) else {
        return not_found(&name);
    };
    let client = match vmd_client(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    if let Err(e) = state.vms.set_desired(&name, DesiredState::Stopped) {
        return bad_request(&e);
    }
    // The supervisor ignores force-stop for a VM that is not running.
    if let Err(e) = client.force_stop(&name).await {
        let _ = state.vms.set_desired(&name, previous.desired);
        return json(
            StatusCode::BAD_GATEWAY,
            serde_json::json!({ "error": e.to_string() }),
        );
    }
    if let Err(e) = push_config(&state, false).await {
        return json(
            StatusCode::BAD_GATEWAY,
            serde_json::json!({ "error": e.to_string() }),
        );
    }
    json(
        StatusCode::OK,
        serde_json::json!({ "status": "stopped", "name": name }),
    )
}

/// Restart is force-stop + start in v1: the guest gets no ACPI window (an
/// unresponsive OS would otherwise hang a restart for the whole graceful
/// timeout).
pub async fn vm_restart_post(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    let lock = lifecycle_lock(&name);
    let _guard = lock.lock().await;
    let Some(previous) = state.vms.get(&name) else {
        return not_found(&name);
    };
    let client = match vmd_client(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    if let Err(e) = state.vms.set_desired(&name, DesiredState::Running) {
        return bad_request(&e);
    }
    // Best effort: a not-running VM just starts.
    let _ = client.force_stop(&name).await;
    if let Err(e) = push_config(&state, false).await {
        let _ = state.vms.set_desired(&name, previous.desired);
        return json(
            StatusCode::BAD_GATEWAY,
            serde_json::json!({ "error": e.to_string() }),
        );
    }
    json(
        StatusCode::OK,
        serde_json::json!({ "status": "restart requested", "name": name }),
    )
}

// ---- Snapshots -------------------------------------------------------------

pub async fn vm_snapshots_get(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    let Some(spec) = state.vms.get(&name) else {
        return not_found(&name);
    };
    let agent = match agent_client(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match agent
        .snapshots_with_guid(&virt::vm_dataset(&spec.pool, &spec.name))
        .await
    {
        Ok(snaps) => json(StatusCode::OK, serde_json::to_value(snaps).unwrap()),
        Err(e) => json(
            StatusCode::BAD_GATEWAY,
            serde_json::json!({ "error": e.to_string() }),
        ),
    }
}

pub async fn vm_snapshot_post(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    body: Body,
) -> Response {
    #[derive(serde::Deserialize, Default)]
    struct Req {
        #[serde(default)]
        name: String,
    }
    let req: Req = match read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let Some(spec) = state.vms.get(&name) else {
        return not_found(&name);
    };
    if let Err(e) = validate_snapshot_name(&req.name) {
        return bad_request(&e);
    }
    let agent = match agent_client(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match agent
        .create_snapshot(&virt::vm_dataset(&spec.pool, &spec.name), &req.name)
        .await
    {
        Ok(()) => json(
            StatusCode::CREATED,
            serde_json::json!({ "status": "snapshot created", "name": req.name }),
        ),
        Err(e) => json(
            StatusCode::BAD_GATEWAY,
            serde_json::json!({ "error": e.to_string() }),
        ),
    }
}

/// Restore = `zfs rollback -r`, which destroys every snapshot newer than the
/// target (and the live state they describe). The API refuses a running VM
/// and requires `?confirm=<name>` because the loss is irreversible.
pub async fn vm_snapshot_restore_post(
    State(state): State<Arc<AppState>>,
    Path((name, snap)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if !confirmed(&query, &name) {
        return bad_request(&format!(
            "restoring a snapshot rolls the VM back and deletes newer snapshots: retry with ?confirm={name}"
        ));
    }
    let Some(spec) = state.vms.get(&name) else {
        return not_found(&name);
    };
    if let Err(e) = validate_snapshot_name(&snap) {
        return bad_request(&e);
    }
    let lock = lifecycle_lock(&name);
    let _guard = lock.lock().await;
    // Fail closed exactly like delete: no live report, no rollback.
    if let Err(resp) = ensure_stopped_for(&state, &name, "restore a snapshot onto").await {
        return resp;
    }
    let agent = match agent_client(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match agent
        .rollback_snapshot(&virt::vm_dataset(&spec.pool, &spec.name), &snap)
        .await
    {
        Ok(()) => json(
            StatusCode::OK,
            serde_json::json!({
                "status": "restored",
                "name": name,
                "snapshot": snap,
                "note": "zfs rollback -r: every snapshot newer than this one was destroyed, and the VM lost all state written after it"
            }),
        ),
        Err(e) => json(
            StatusCode::BAD_GATEWAY,
            serde_json::json!({ "error": e.to_string() }),
        ),
    }
}

// ---- Image library ---------------------------------------------------------

pub async fn images_get(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let pool = query.get("pool").cloned().unwrap_or_default();
    if !naslos_vmd::model::valid_pool_component(&pool) {
        return bad_request(&format!("pool {pool:?} is not a valid dataset component"));
    }
    let client = match vmd_client(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match client.images(&pool).await {
        Ok(list) => json(StatusCode::OK, serde_json::to_value(list).unwrap()),
        Err(e) => json(
            StatusCode::BAD_GATEWAY,
            serde_json::json!({ "error": e.to_string() }),
        ),
    }
}

/// One upload chunk, streamed through to vmd (the browser XHR sender gets
/// per-chunk progress). `application/octet-stream` bodies are allowed for
/// exactly this route (see auth.rs).
pub async fn image_upload_post(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let pool = query.get("pool").cloned().unwrap_or_default();
    let name = query.get("name").cloned().unwrap_or_default();
    let offset = match query
        .get("offset")
        .map(|v| v.trim())
        .unwrap_or("0")
        .parse::<u64>()
    {
        Ok(v) => v,
        Err(_) => {
            return bad_request("offset must be a number");
        }
    };
    let final_chunk = query
        .get("final")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);
    // Cheap early guard: the declared chunk size (the UI sends 8 MiB).
    if let Some(len) = headers
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<usize>().ok())
    {
        if len > MAX_CHUNK_BYTES {
            return json(
                StatusCode::PAYLOAD_TOO_LARGE,
                serde_json::json!({
                    "error": format!("upload chunks must be {} bytes or fewer", MAX_CHUNK_BYTES)
                }),
            );
        }
    }
    // Replacing an existing library image needs the same explicit
    // confirmation as every destructive route; vmd enforces confirm == name.
    let confirm = query.get("confirm").cloned();
    let client = match vmd_client(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match client
        .upload_chunk(&pool, &name, offset, final_chunk, confirm.as_deref(), body)
        .await
    {
        Ok(size) => json(
            StatusCode::OK,
            serde_json::json!({ "name": name, "sizeBytes": size }),
        ),
        Err(e) => json(
            vmd_error_status(&e),
            serde_json::json!({ "error": e.to_string() }),
        ),
    }
}

#[derive(serde::Deserialize, Default)]
pub struct ImportRequest {
    #[serde(default)]
    pool: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    sha256: Option<String>,
    #[serde(default)]
    confirm: Option<String>,
}

pub async fn image_import_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let req: ImportRequest = match read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    if req.url.trim().is_empty() {
        return bad_request("an import URL is required");
    }
    if !naslos_vmd::model::valid_pool_component(&req.pool) {
        return bad_request(&format!(
            "pool {:?} is not a valid dataset component",
            req.pool
        ));
    }
    if !naslos_vmd::model::valid_library_name(&req.name) {
        return bad_request(&format!("{:?} is not a valid library file name", req.name));
    }
    let client = match vmd_client(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    // vmd validates the pool and the target (public-only http(s), the
    // FR-DNS-07 SSRF guard) and runs the download; errors surface in the
    // images listing while it runs.
    match client
        .import_image(
            &req.pool,
            &req.name,
            req.url.trim(),
            req.sha256.as_deref(),
            req.confirm.as_deref(),
        )
        .await
    {
        Ok(()) => json(
            StatusCode::ACCEPTED,
            serde_json::json!({
                "status": "import started",
                "name": req.name,
                "note": "poll GET /api/virt/images for completion or the last error"
            }),
        ),
        Err(e) => json(
            vmd_error_status(&e),
            serde_json::json!({ "error": e.to_string() }),
        ),
    }
}

pub async fn image_delete(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
    Path(name): Path<String>,
) -> Response {
    let pool = query.get("pool").cloned().unwrap_or_default();
    if !naslos_vmd::model::valid_pool_component(&pool) {
        return bad_request(&format!("pool {pool:?} is not a valid dataset component"));
    }
    let client = match vmd_client(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match client.delete_image(&pool, &name).await {
        Ok(()) => json(
            StatusCode::OK,
            serde_json::json!({ "status": "deleted", "name": name }),
        ),
        Err(e) => json(
            vmd_error_status(&e),
            serde_json::json!({ "error": e.to_string() }),
        ),
    }
}

// ---- Console (VNC bridge) ---------------------------------------------------

/// Console session bounds, read once from the environment
/// (`VIRT_CONSOLE_MAX_SESSION_SECS` 4 h, `VIRT_CONSOLE_IDLE_TIMEOUT_SECS`
/// 10 min, `VIRT_CONSOLE_MAX_SESSIONS` 4 concurrent bridges).
struct ConsoleLimits {
    max: std::time::Duration,
    idle: std::time::Duration,
    max_sessions: usize,
}

fn env_secs(name: &str, default: u64) -> std::time::Duration {
    let Ok(raw) = std::env::var(name) else {
        return std::time::Duration::from_secs(default);
    };
    match raw.trim().parse::<u64>() {
        Ok(n) if n > 0 => std::time::Duration::from_secs(n),
        _ => {
            tracing::warn!("{name}={raw:?} is not a positive integer; using {default}s");
            std::time::Duration::from_secs(default)
        }
    }
}

fn console_limits() -> &'static ConsoleLimits {
    static LIMITS: std::sync::OnceLock<ConsoleLimits> = std::sync::OnceLock::new();
    LIMITS.get_or_init(|| ConsoleLimits {
        max: env_secs("VIRT_CONSOLE_MAX_SESSION_SECS", 4 * 60 * 60),
        idle: env_secs("VIRT_CONSOLE_IDLE_TIMEOUT_SECS", 10 * 60),
        max_sessions: env_secs("VIRT_CONSOLE_MAX_SESSIONS", 4).as_secs() as usize,
    })
}

fn console_slots() -> &'static Arc<tokio::sync::Semaphore> {
    static SLOTS: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
    SLOTS.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(console_limits().max_sessions)))
}

/// `GET /api/ws/vm/{name}/vnc`. The plain-GET preflight returns the VNC port
/// and the per-boot password the console client needs; the WebSocket upgrade
/// bridges RFB bytes between the browser and QEMU's VNC listener on the node.
pub async fn vm_vnc_ws(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    request: Request,
) -> Response {
    if !vnc_origin_allowed(request.headers()) {
        return json(
            StatusCode::FORBIDDEN,
            serde_json::json!({ "error": "websocket origin refused" }),
        );
    }
    // The console hands over a live VNC password and bridges a privileged
    // channel: a degraded audit log means the session could not be recorded,
    // so it is refused rather than started unaudited.
    if state.audit.status() != "ok" {
        return json(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({
                "error": "the audit log is degraded; refusing to start a console session"
            }),
        );
    }
    let Some(_spec) = state.vms.get(&name) else {
        return not_found(&name);
    };
    let client = match vmd_client(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let live = match client.vms_live().await {
        Ok(live) => live,
        Err(e) => {
            return json(
                StatusCode::BAD_GATEWAY,
                serde_json::json!({ "error": e.to_string() }),
            )
        }
    };
    let Some(entry) = live.iter().find(|l| l.name == name) else {
        return not_found(&name);
    };
    if entry.state != "running" {
        return json(
            StatusCode::CONFLICT,
            serde_json::json!({
                "error": format!("VM {name:?} is {} — start it before opening the console", entry.state)
            }),
        );
    }
    let (Some(vnc_port), vnc_password) = (entry.vnc_port, entry.vnc_password.clone()) else {
        return json(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({ "error": "the console port is not allocated for this VM" }),
        );
    };

    let is_ws = request
        .headers()
        .get(axum::http::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);
    if !is_ws {
        // Plain-GET preflight: a browser cannot read the status of a failed
        // websocket handshake, so this is what turns "VM not running" or "no
        // KVM" into a message the operator can act on. It also hands over
        // the per-boot VNC password (this is an owner-gated route).
        return json(
            StatusCode::OK,
            serde_json::json!({
                "name": name,
                "vncPort": vnc_port,
                "vncPassword": vnc_password,
                "status": "ready to attach"
            }),
        );
    }

    if !console_attach_allowed(&name) {
        return json(
            StatusCode::TOO_MANY_REQUESTS,
            serde_json::json!({
                "error": "console attach attempts are rate limited; retry in a second"
            }),
        );
    }
    let pending = console_session_pending(&request, &name);
    let audit = state.audit.clone();
    let (mut parts, _body) = request.into_parts();
    let ws = match WebSocketUpgrade::from_request_parts(&mut parts, &state).await {
        Ok(ws) => ws,
        Err(e) => return e.into_response(),
    };
    let permit = match console_slots().clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => {
            return json(
                StatusCode::TOO_MANY_REQUESTS,
                serde_json::json!({
                    "error": "too many console sessions; close one and retry"
                }),
            )
        }
    };
    let pending = match audit.begin(pending) {
        Ok(pending) => pending,
        Err(e) => {
            audit.mark_degraded(&format!("console session begin: {e}"));
            return json(
                StatusCode::SERVICE_UNAVAILABLE,
                serde_json::json!({ "error": format!("audit unavailable: {e}") }),
            );
        }
    };

    ws.max_message_size(1 << 20)
        .max_frame_size(256 << 10)
        .on_upgrade(move |socket| {
            vnc_bridge(socket, client, name, vnc_port, permit, audit, pending)
        })
}

/// The console origin policy: the browser `Origin` must be exactly the HTTPS
/// authority of the request's own `Host`, port included. HTTP, another host,
/// or any SSO/base-domain expansion is refused — this route hands over the
/// per-boot VNC password, so an `Origin` allowlist that accepts sibling hosts
/// is a cross-origin console takeover.
fn vnc_origin_allowed(headers: &HeaderMap) -> bool {
    let Some(origin) = headers
        .get(axum::http::header::ORIGIN)
        .and_then(|v| v.to_str().ok())
    else {
        // No Origin header: not a browser cross-origin request (the route is
        // still owner-gated); the console client always sends one.
        return true;
    };
    let Ok(uri) = origin.parse::<axum::http::Uri>() else {
        return false;
    };
    if uri.scheme_str() != Some("https") {
        return false;
    }
    let Some(authority) = uri.authority() else {
        return false;
    };
    let Some(host) = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    authority.as_str().eq_ignore_ascii_case(host.trim())
}

/// One attach attempt per VM per second. The console is expensive (a QEMU
/// TCP connection plus a WebSocket bridge) and the VNC password is
/// brute-forceable, so a tight loop of upgrade attempts is refused early.
fn console_attach_allowed(name: &str) -> bool {
    static LAST: std::sync::OnceLock<std::sync::Mutex<HashMap<String, std::time::Instant>>> =
        std::sync::OnceLock::new();
    let last = LAST.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut last = last.lock().unwrap();
    let now = std::time::Instant::now();
    if last
        .get(name)
        .is_some_and(|previous| now.duration_since(*previous) < std::time::Duration::from_secs(1))
    {
        return false;
    }
    last.insert(name.to_string(), now);
    true
}

/// The audit record for one console session. The generic audit middleware
/// only records mutations, and a WebSocket upgrade is a `GET`; the session is
/// recorded explicitly instead, attributed to the authenticated owner.
fn console_session_pending(request: &Request, name: &str) -> crate::audit::Pending {
    let request_id = request
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            format!("vnc-{name}-{nanos}")
        });
    match crate::auth::user_from_request(request) {
        Some(user) => crate::audit::Pending::user(
            &user,
            &axum::http::Method::GET,
            &format!("/api/ws/vm/{name}/vnc"),
            "/api/ws/vm/{name}/vnc",
            None,
            None,
            request_id,
        ),
        // Direct handler tests (and a future internal caller) have no user
        // extension; still audited, as a system session.
        None => crate::audit::Pending::system(
            "api",
            "virt.vms.console",
            Some(name.to_string()),
            serde_json::json!({ "requestId": request_id }),
        ),
    }
}

fn finish_console_session(
    audit: &crate::audit::AuditLog,
    pending: &crate::audit::Pending,
    status: u16,
) {
    if let Err(e) = audit.finish(pending, status) {
        if audit.mark_degraded(&format!("console session end: {e}")) {
            tracing::error!("console session audit end failed: {e}");
        }
    }
}

/// Bridge one console: browser WebSocket <-> the authenticated vmd console
/// tunnel. QEMU's VNC listener binds loopback only, so the API never dials it
/// directly: `Client::vnc_tunnel` opens a WebSocket on vmd's TLS listener and
/// vmd proxies its bytes to 127.0.0.1:<vnc_port>. RFB is a byte stream, so
/// the frames carry its bytes unchanged (the websockify pattern).
async fn vnc_bridge(
    socket: WebSocket,
    client: Arc<vmd::Client>,
    vm_name: String,
    vnc_port: u16,
    _slot: tokio::sync::OwnedSemaphorePermit,
    audit: crate::audit::AuditLog,
    pending: crate::audit::Pending,
) {
    use tokio_tungstenite::tungstenite::Message as TunnelMessage;

    let mut socket = socket;
    let tunnel = match tokio::time::timeout(
        std::time::Duration::from_secs(10),
        client.vnc_tunnel(&vm_name, vnc_port),
    )
    .await
    {
        Ok(Ok(tunnel)) => tunnel,
        Ok(Err(e)) => {
            tracing::warn!("console bridge: vmd tunnel refused for {vm_name}: {e}");
            let _ = socket.close().await;
            finish_console_session(&audit, &pending, 502);
            return;
        }
        Err(_) => {
            tracing::warn!("console bridge: opening the vmd tunnel timed out for {vm_name}");
            let _ = socket.close().await;
            finish_console_session(&audit, &pending, 504);
            return;
        }
    };
    let (mut tunnel_sender, mut tunnel_receiver) = tunnel.split();
    let (mut ws_sender, mut ws_receiver) = socket.split();

    // Tunnel -> WS: the console output.
    let pump = tokio::spawn(async move {
        while let Some(message) = tunnel_receiver.next().await {
            match message {
                Ok(TunnelMessage::Binary(data)) => {
                    if ws_sender.send(Message::Binary(data)).await.is_err() {
                        break;
                    }
                }
                Ok(TunnelMessage::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
        let _ = ws_sender.close().await;
    });

    let limits = console_limits();
    let started = tokio::time::Instant::now();
    let mut last_traffic = started;
    loop {
        let deadline = (started + limits.max).min(last_traffic + limits.idle);
        let next = tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {
                tracing::info!("console session ended: session limits");
                break;
            }
            msg = ws_receiver.next() => msg,
        };
        let Some(msg) = next else { break };
        match msg {
            Ok(Message::Binary(data)) => {
                if data.len() > (256 << 10) {
                    break;
                }
                last_traffic = tokio::time::Instant::now();
                if tunnel_sender
                    .send(TunnelMessage::Binary(data))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            Ok(Message::Close(_)) => break,
            Err(_) => break,
            _ => {
                // Text frames are unused (the console speaks RFB bytes);
                // pings are answered by axum.
                last_traffic = tokio::time::Instant::now();
            }
        }
    }
    let _ = tunnel_sender.close().await;
    pump.abort();
    let _ = pump.await;
    finish_console_session(&audit, &pending, 101);
}

// ---- helpers ----------------------------------------------------------------

/// Render the whole desired config and push it to vmd. Public for the
/// startup push in main.rs (boot semantics: autostart VMs start).
///
/// After a successful boot push, the autostart decision is persisted into
/// the records: the pushed copy alone would leave `vms.json` saying
/// `desired: stopped`, and the next plain push (any create/edit/action)
/// would stop the just-started VM again.
pub async fn apply_config(state: &AppState, boot: bool) -> Result<(), String> {
    push_config(state, boot).await?;
    if boot {
        for vm in state.vms.list() {
            if vm.autostart && vm.desired != DesiredState::Running {
                if let Err(e) = state.vms.set_desired(&vm.name, DesiredState::Running) {
                    tracing::warn!("persisting autostart for {}: {e}", vm.name);
                }
            }
        }
    }
    Ok(())
}

/// The VM events watcher (FR-VMT): polls vmd's live report and raises ntfy
/// notifications when a VM that should be running has failed to start or
/// exited unexpectedly. Transitions only — a persistent failure notifies
/// once, not on every poll.
///
/// A vmd-only restart (a deleted pod, a crash) leaves the API unaware: its
/// startup push already happened. When the watcher sees the supervisor go
/// down and come back, it re-pushes the persisted desired state so
/// desired-running VMs reconcile — an explicitly stopped VM stays stopped.
pub async fn run_vm_watcher(state: Arc<AppState>) {
    let Some(client) = state.vmd.clone() else {
        return;
    };
    let mut seen: HashMap<String, bool> = HashMap::new();
    let mut was_down = false;
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(30));
    ticker.tick().await;
    loop {
        ticker.tick().await;
        let live = match client.vms_live().await {
            Ok(live) => live,
            Err(_) => {
                was_down = true;
                continue;
            }
        };
        if was_down {
            was_down = false;
            tracing::info!("the VM supervisor is back; re-pushing the desired state");
            if let Err(e) = apply_config(&state, false).await {
                tracing::warn!("re-pushing the VM config after the supervisor restart: {e}");
            }
        }
        for entry in live {
            let failed = entry.unexpected_exit || !entry.last_error.is_empty();
            let was = seen.insert(entry.name.clone(), failed).unwrap_or(false);
            if failed && !was {
                let event = if entry.unexpected_exit {
                    virt::EVENT_VM_EXITED
                } else {
                    virt::EVENT_VM_FAILED
                };
                let severity = crate::notifications::SEVERITY_ERROR;
                let notif = crate::notifications::Notification {
                    title: format!("VM {} failed", entry.name),
                    message: if entry.last_error.is_empty() {
                        format!("VM {} exited unexpectedly", entry.name)
                    } else {
                        entry.last_error.clone()
                    },
                    severity: severity.to_string(),
                    tags: vec!["computer".to_string(), "x".to_string()],
                };
                crate::webhook::deliver_logged(
                    &state.notifications,
                    &state.webhook,
                    &state.smtp,
                    &state.syslog,
                    event,
                    &notif,
                )
                .await;
            }
        }
    }
}

#[allow(clippy::result_large_err)]
async fn read_json<T: serde::de::DeserializeOwned>(body: Body) -> Result<T, Response> {
    super::zfs::read_json(body).await
}

/// Create a dataset if it does not exist yet, applying `options` when it is
/// actually created (idempotent bootstrap for `<pool>/vms` and the library).
async fn ensure_dataset(
    agent: &Arc<crate::agent::Client>,
    dataset: &str,
    options: Option<&HashMap<String, String>>,
) -> Result<(), String> {
    let datasets = agent
        .list_datasets()
        .await
        .map_err(|e| format!("cannot list datasets: {e}"))?;
    if datasets.iter().any(|d| d.name == dataset) {
        return Ok(());
    }
    let (pool, rest) = dataset
        .split_once('/')
        .ok_or_else(|| format!("dataset {dataset:?} must be <pool>/<path>"))?;
    let empty = HashMap::new();
    let options = options.unwrap_or(&empty);
    // The agent rejects unknown option keys, and this call only ever sends
    // quota/compression (both allowlisted).
    agent
        .create_dataset(pool, rest, options)
        .await
        .map_err(|e| format!("creating dataset {dataset}: {e}"))
}

fn bad_request(msg: &str) -> Response {
    json(StatusCode::BAD_REQUEST, serde_json::json!({ "error": msg }))
}

fn conflict(msg: &str) -> Response {
    json(StatusCode::CONFLICT, serde_json::json!({ "error": msg }))
}

/// Destructive VM operations take `?confirm=<name>`: a stale UI request, a
/// mis-clicked row or a replayed call must not destroy a dataset by accident.
fn confirmed(query: &HashMap<String, String>, name: &str) -> bool {
    query.get("confirm").map(String::as_str) == Some(name)
}

fn gateway(msg: &str) -> Response {
    json(StatusCode::BAD_GATEWAY, serde_json::json!({ "error": msg }))
}

fn not_found(name: &str) -> Response {
    json(
        StatusCode::NOT_FOUND,
        serde_json::json!({ "error": format!("VM {name:?} does not exist") }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use naslos_vmd::model::DiskFormat;

    fn state() -> Arc<AppState> {
        Arc::new(AppState::for_test("test-secret"))
    }

    fn spec(name: &str) -> VmSpec {
        serde_json::from_value(serde_json::json!({
            "name": name,
            "cpu": 1,
            "memMiB": 128,
            "disk": { "sizeGB": 1 },
            "pool": "tank"
        }))
        .expect("a valid test VM spec")
    }

    fn query(pairs: &[(&str, &str)]) -> Query<HashMap<String, String>> {
        Query(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    #[test]
    fn lifecycle_locks_are_per_vm() {
        assert!(Arc::ptr_eq(
            &lifecycle_lock("alpha"),
            &lifecycle_lock("alpha")
        ));
        assert!(!Arc::ptr_eq(
            &lifecycle_lock("alpha"),
            &lifecycle_lock("beta")
        ));
    }

    #[tokio::test]
    async fn delete_and_restore_require_the_exact_confirm_name() {
        let state = state();
        state.vms.create(spec("vm1")).unwrap();

        let missing = vm_detail_delete(State(state.clone()), Path("vm1".into()), query(&[])).await;
        assert_eq!(missing.status(), StatusCode::BAD_REQUEST);

        let wrong = vm_detail_delete(
            State(state.clone()),
            Path("vm1".into()),
            query(&[("confirm", "other")]),
        )
        .await;
        assert_eq!(wrong.status(), StatusCode::BAD_REQUEST);

        let restore = vm_snapshot_restore_post(
            State(state.clone()),
            Path(("vm1".to_string(), "snap1".to_string())),
            query(&[]),
        )
        .await;
        assert_eq!(restore.status(), StatusCode::BAD_REQUEST);

        assert!(
            state.vms.get("vm1").is_some(),
            "a refused request must not remove the record"
        );
    }

    #[tokio::test]
    async fn destructive_actions_fail_closed_when_vmd_is_absent() {
        let state = state();
        state.vms.create(spec("vm1")).unwrap();

        let delete = vm_detail_delete(
            State(state.clone()),
            Path("vm1".into()),
            query(&[("confirm", "vm1")]),
        )
        .await;
        assert_eq!(delete.status(), StatusCode::SERVICE_UNAVAILABLE);

        let restore = vm_snapshot_restore_post(
            State(state.clone()),
            Path(("vm1".to_string(), "snap1".to_string())),
            query(&[("confirm", "vm1")]),
        )
        .await;
        assert_eq!(restore.status(), StatusCode::SERVICE_UNAVAILABLE);

        assert!(
            state.vms.get("vm1").is_some(),
            "no live report means no destructive action"
        );
    }

    #[tokio::test]
    async fn lifecycle_actions_do_not_mutate_desired_state_without_vmd() {
        let state = state();
        state.vms.create(spec("vm1")).unwrap();

        let start = vm_start_post(State(state.clone()), Path("vm1".into())).await;
        assert_eq!(start.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(state.vms.get("vm1").unwrap().desired, DesiredState::Stopped);

        state.vms.set_desired("vm1", DesiredState::Running).unwrap();
        let stop = vm_stop_post(State(state.clone()), Path("vm1".into())).await;
        assert_eq!(stop.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(state.vms.get("vm1").unwrap().desired, DesiredState::Running);
    }

    async fn put(state: &Arc<AppState>, vm: &VmSpec) -> StatusCode {
        let response = vm_detail_put(
            State(state.clone()),
            Path(vm.name.clone()),
            Body::from(serde_json::to_vec(vm).unwrap()),
        )
        .await;
        response.status()
    }

    #[tokio::test]
    async fn update_rejects_immutable_disk_pool_and_source_image() {
        let state = state();
        state.vms.create(spec("vm1")).unwrap();

        let mut format = spec("vm1");
        format.disk.format = DiskFormat::Raw;
        assert_eq!(put(&state, &format).await, StatusCode::CONFLICT);

        let mut pool = spec("vm1");
        pool.pool = "other".into();
        assert_eq!(put(&state, &pool).await, StatusCode::CONFLICT);

        let mut source = spec("vm1");
        source.disk.source_image = Some("isos/cloud.qcow2".into());
        assert_eq!(put(&state, &source).await, StatusCode::CONFLICT);

        let mut size = spec("vm1");
        size.disk.size_gb = 8;
        assert_eq!(put(&state, &size).await, StatusCode::CONFLICT);

        // The stored record still describes the disk that exists.
        let stored = state.vms.get("vm1").unwrap();
        assert_eq!(stored.disk.format, DiskFormat::Qcow2);
        assert_eq!(stored.disk.size_gb, 1);
    }

    #[test]
    fn vnc_origin_must_be_the_exact_https_request_authority() {
        use axum::http::header::{HOST, ORIGIN};
        let headers = |origin: Option<&str>, host: &str| {
            let mut h = HeaderMap::new();
            h.insert(HOST, host.parse().unwrap());
            if let Some(origin) = origin {
                h.insert(ORIGIN, origin.parse().unwrap());
            }
            h
        };

        assert!(vnc_origin_allowed(&headers(
            Some("https://naslos.example"),
            "naslos.example"
        )));
        assert!(vnc_origin_allowed(&headers(
            Some("https://naslos.example:8443"),
            "naslos.example:8443"
        )));
        assert!(!vnc_origin_allowed(&headers(
            Some("https://naslos.example:8443"),
            "naslos.example"
        )));
        assert!(!vnc_origin_allowed(&headers(
            Some("http://naslos.example"),
            "naslos.example"
        )));
        assert!(!vnc_origin_allowed(&headers(
            Some("https://other.example"),
            "naslos.example"
        )));
        assert!(vnc_origin_allowed(&headers(None, "naslos.example")));
    }

    fn vnc_request(origin: Option<&str>) -> Request {
        let mut builder = Request::builder()
            .uri("/api/ws/vm/vm1/vnc")
            .header("host", "naslos.example");
        if let Some(origin) = origin {
            builder = builder.header("origin", origin);
        }
        builder.body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn console_refuses_http_origin_and_a_degraded_audit_log() {
        let state = state();
        let refused = vm_vnc_ws(
            State(state.clone()),
            Path("vm1".into()),
            vnc_request(Some("http://naslos.example")),
        )
        .await;
        assert_eq!(refused.status(), StatusCode::FORBIDDEN);

        state.vms.create(spec("vm1")).unwrap();
        state.audit.mark_degraded("test failure");
        let unaudited =
            vm_vnc_ws(State(state.clone()), Path("vm1".into()), vnc_request(None)).await;
        assert_eq!(unaudited.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
