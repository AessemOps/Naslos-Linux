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
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use naslos_vmd::model::{validate_snapshot_name, DesiredState, VmSpec};

/// Largest single upload chunk the API relays (the UI sends 8 MiB chunks).
const MAX_CHUNK_BYTES: usize = 64 * 1024 * 1024;

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
    // The record only sticks if the supervisor accepts the whole config;
    // otherwise restore the previous spec so vms.json is never left with a
    // config that would fail every later push.
    let previous = state.vms.get(&name);
    let updated = match state.vms.update(&name, spec) {
        Ok(vm) => vm,
        Err(e) => return bad_request(&e),
    };
    if let Err(e) = push_config(&state, false).await {
        if let Some(previous) = previous {
            let _ = state.vms.update(&name, previous);
        }
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
) -> Response {
    // Existence check only; the removed record (with its pool) comes back
    // from store.remove below.
    if state.vms.get(&name).is_none() {
        return not_found(&name);
    }
    // A running VM's disk is being written right now: destroying the dataset
    // under it would corrupt the image. Stop the VM first.
    if let Some(client) = state.vmd.clone() {
        if let Ok(live) = client.vms_live().await {
            if live.iter().any(|l| l.name == name && l.state == "running") {
                return json(
                    StatusCode::CONFLICT,
                    serde_json::json!({
                        "error": format!("VM {name:?} is running; stop it before deleting it")
                    }),
                );
            }
        }
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
    if let Err(e) = state.vms.set_desired(&name, DesiredState::Running) {
        return bad_request(&e);
    }
    if let Err(e) = push_config(&state, false).await {
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
    if let Err(e) = state.vms.set_desired(&name, DesiredState::Stopped) {
        return bad_request(&e);
    }
    if let Err(e) = push_config(&state, false).await {
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
    let client = match vmd_client(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    if let Err(e) = state.vms.set_desired(&name, DesiredState::Stopped) {
        return bad_request(&e);
    }
    // The supervisor ignores force-stop for a VM that is not running.
    if let Err(e) = client.force_stop(&name).await {
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

/// Restore = `zfs rollback -r`, which refuses a dataset in use; the API
/// refuses a running VM first so the error is actionable.
pub async fn vm_snapshot_restore_post(
    State(state): State<Arc<AppState>>,
    Path((name, snap)): Path<(String, String)>,
) -> Response {
    let Some(spec) = state.vms.get(&name) else {
        return not_found(&name);
    };
    if let Err(e) = validate_snapshot_name(&snap) {
        return bad_request(&e);
    }
    if let Some(client) = state.vmd.clone() {
        if let Ok(live) = client.vms_live().await {
            if live.iter().any(|l| l.name == name && l.state == "running") {
                return json(
                    StatusCode::CONFLICT,
                    serde_json::json!({
                        "error": format!("VM {name:?} is running; stop it before restoring a snapshot")
                    }),
                );
            }
        }
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
                "note": "snapshots newer than the restored one were deleted (zfs rollback -r)"
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
    let client = match vmd_client(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match client
        .upload_chunk(&pool, &name, offset, final_chunk, body)
        .await
    {
        Ok(size) => json(
            StatusCode::OK,
            serde_json::json!({ "name": name, "sizeBytes": size }),
        ),
        Err(e) => json(
            StatusCode::BAD_GATEWAY,
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
        .import_image(&req.pool, &req.name, req.url.trim(), req.sha256.as_deref())
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
            StatusCode::BAD_GATEWAY,
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
            StatusCode::BAD_GATEWAY,
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
    if !super::ws::origin_allowed(request.headers(), &console_allowed_origins(&state)) {
        return json(
            StatusCode::FORBIDDEN,
            serde_json::json!({ "error": "websocket origin refused" }),
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

    let vmd_host = vmd_host_of(client.base_url());
    ws.max_message_size(1 << 20)
        .max_frame_size(256 << 10)
        .on_upgrade(move |socket| vnc_bridge(socket, vmd_host, vnc_port, permit))
}

fn console_allowed_origins(state: &AppState) -> Vec<String> {
    let mut out = Vec::new();
    if !state.base_domain.is_empty() {
        out.push(state.base_domain.clone());
    }
    for d in &state.sso_domains {
        if !d.is_empty() {
            out.push(d.clone());
        }
    }
    out
}

/// Extract the host to dial for the VNC listener: vmd's Service host (the
/// node's own address, since vmd is hostNetwork).
fn vmd_host_of(base_url: &str) -> String {
    let rest = base_url
        .strip_prefix("https://")
        .or_else(|| base_url.strip_prefix("http://"))
        .unwrap_or(base_url);
    rest.split(':').next().unwrap_or(rest).to_string()
}

/// Bridge one console: browser WebSocket <-> QEMU's VNC TCP listener. RFB is
/// a byte stream; the WS frames carry its bytes (the websockify pattern,
/// implemented here so no extra process exists). QEMU's VNC is
/// password-protected (vmd sets the per-boot password over QMP) and only the
/// API pod and the LAN can reach the node port at all.
async fn vnc_bridge(
    socket: WebSocket,
    vmd_host: String,
    vnc_port: u16,
    _slot: tokio::sync::OwnedSemaphorePermit,
) {
    let mut socket = socket;
    let stream = match tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::net::TcpStream::connect((vmd_host.as_str(), vnc_port)),
    )
    .await
    {
        Ok(Ok(stream)) => stream,
        _ => {
            tracing::warn!("console bridge: cannot reach {vmd_host}:{vnc_port}");
            let _ = socket.close().await;
            return;
        }
    };
    let (mut tcp_read, mut tcp_write) = stream.into_split();
    let (mut ws_sender, mut ws_receiver) = socket.split();

    // TCP -> WS: the console output.
    let pump = tokio::spawn(async move {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            match tcp_read.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if ws_sender
                        .send(Message::Binary(buf[..n].to_vec().into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
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
                if tcp_write.write_all(&data).await.is_err() {
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
    let _ = tcp_write.shutdown().await;
    pump.abort();
    let _ = pump.await;
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
                crate::webhook::deliver_logged(&state.notifications, &state.webhook, event, &notif)
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

fn gateway(msg: &str) -> Response {
    json(StatusCode::BAD_GATEWAY, serde_json::json!({ "error": msg }))
}

fn not_found(name: &str) -> Response {
    json(
        StatusCode::NOT_FOUND,
        serde_json::json!({ "error": format!("VM {name:?} does not exist") }),
    )
}
