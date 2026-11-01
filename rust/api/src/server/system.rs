//! System version/update/operation endpoints (spec FR-UPD).
//!
//! Admin-only (the owner router). The API reads versions, verifies the cached
//! release, and creates one upgrader Job; it never performs the upgrade itself.

use crate::json;
use crate::semver::Version;
use crate::state::AppState;
use crate::system::{
    active_phase, job_spec, new_operation_id, operation_job_name, stepping, Candidate, JobOptions,
    ReleaseFacts,
};
use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use serde::Deserialize;
use std::sync::Arc;

fn unavailable(message: &str) -> Response {
    json(
        StatusCode::SERVICE_UNAVAILABLE,
        serde_json::json!({ "error": message }),
    )
}

fn precondition(message: &str) -> Response {
    json(
        StatusCode::PRECONDITION_FAILED,
        serde_json::json!({ "error": message }),
    )
}

/// `GET /api/system/version`
pub async fn version(State(state): State<Arc<AppState>>) -> Response {
    let release = state.system.release.clone();
    let talos = running_talos(&state).await;
    let kubernetes = running_kubernetes(&state).await;
    json(
        StatusCode::OK,
        serde_json::json!({
            "naslosVersion": release.naslos_version,
            "chartVersion": release.chart_version,
            "talosVersion": talos,
            "kubernetesVersion": kubernetes,
            "releaseTalosVersion": release.talos_version,
            "releaseKubernetesVersion": release.kubernetes_version,
            "schematicId": release.schematic_id,
            "profile": release.profile,
            "upgradable": release.upgradable() && state.updates.is_some(),
            "updatesEnabled": state.updates.is_some(),
        }),
    )
}

/// `GET /api/system/updates`
pub async fn updates_get(State(state): State<Arc<AppState>>) -> Response {
    let Some(checker) = state.updates.clone() else {
        return json(
            StatusCode::OK,
            serde_json::json!({
                "enabled": false,
                "upgradable": false,
                "available": null,
                "blocked": "update checks are disabled",
            }),
        );
    };
    let check = checker.load_cached();
    let talos = running_talos(&state).await;
    let kubernetes = running_kubernetes(&state).await;
    render_updates(&state, check.as_ref(), &talos, &kubernetes)
}

/// `POST /api/system/updates/check`
pub async fn updates_check(State(state): State<Arc<AppState>>) -> Response {
    let Some(checker) = state.updates.clone() else {
        return json(
            StatusCode::OK,
            serde_json::json!({
                "enabled": false,
                "upgradable": false,
                "available": null,
                "blocked": "update checks are disabled",
            }),
        );
    };
    let check = checker.check().await;
    let talos = running_talos(&state).await;
    let kubernetes = running_kubernetes(&state).await;
    render_updates(&state, Some(&check), &talos, &kubernetes)
}

fn render_updates(
    state: &AppState,
    check: Option<&crate::system::UpdateCheck>,
    talos: &str,
    kubernetes: &str,
) -> Response {
    let release = &state.system.release;
    let Some(check) = check else {
        return json(
            StatusCode::OK,
            serde_json::json!({
                "enabled": true,
                "upgradable": release.upgradable(),
                "current": release.naslos_version,
                "checkedAt": "",
                "available": null,
                "stepping": {"allowed": false, "reason": "no update check has run yet"},
                "blocked": "no update check has run yet",
                "error": "",
            }),
        );
    };
    let decision = check
        .available
        .as_ref()
        .map(|candidate| stepping(release, candidate, talos, kubernetes));
    json(
        StatusCode::OK,
        serde_json::json!({
            "enabled": true,
            "upgradable": release.upgradable(),
            "current": check.current,
            "checkedAt": check.checked_at,
            "source": check.source,
            "available": check.available.as_ref().map(Candidate::as_json),
            "stepping": {
                "allowed": decision.as_ref().map(|d| d.allowed).unwrap_or(false),
                "reason": decision.as_ref().map(|d| d.reason.clone()).unwrap_or_default(),
            },
            "blocked": check.blocked,
            "error": check.error,
        }),
    )
}

#[derive(Default, Deserialize)]
struct OperationBody {
    #[serde(default)]
    op: String,
    #[serde(default)]
    version: String,
}

/// Serialise operation creation inside the API process. The chart runs one
/// replica; the ConfigMap is the durable cross-restart lock, this closes the
/// window before the Job has written it.
static OPERATION_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// `POST /api/system/operations`
pub async fn operation_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let body: OperationBody = match super::zfs::read_json(body).await {
        Ok(body) => body,
        Err(resp) => return resp,
    };
    let _guard = OPERATION_LOCK.lock().await;
    if !crate::system::valid_op(&body.op) {
        return json(
            StatusCode::BAD_REQUEST,
            serde_json::json!({ "error": "op must be naslos, talos or kubernetes" }),
        );
    }
    let release = state.system.release.clone();
    if !release.upgradable() {
        return precondition(
            "this node was not provisioned from an installer pack; upgrade it with make install-vm",
        );
    }
    let Some(checker) = state.updates.clone() else {
        return precondition("update checks are disabled");
    };
    let requested = if body.version.is_empty() {
        checker
            .load_cached()
            .and_then(|c| c.available)
            .map(|c| c.version)
            .unwrap_or_default()
    } else {
        body.version.clone()
    };
    let Some(candidate) = checker.cached_candidate(&requested) else {
        return precondition("run an update check first and choose the offered release");
    };
    if state.app_jobs.has_running() {
        return json(
            StatusCode::CONFLICT,
            serde_json::json!({ "error": "an app install/upgrade is running" }),
        );
    }
    if state.buddy_jobs.has_running() {
        return json(
            StatusCode::CONFLICT,
            serde_json::json!({ "error": "a Buddy backup job is running" }),
        );
    }
    if let Some(current) = read_status(&state).await {
        if active_phase(&current.phase) {
            return json(
                StatusCode::CONFLICT,
                serde_json::json!({
                    "error": "another system operation is running",
                    "operationId": current.id,
                }),
            );
        }
    }
    // The Job exists before it writes the ConfigMap; it is a lock too.
    if let Some(job) = read_job_status(&state).await {
        if job.get("phase").and_then(serde_json::Value::as_str) == Some("pending") {
            return json(
                StatusCode::CONFLICT,
                serde_json::json!({
                    "error": "another system operation is starting",
                    "operationId": job.get("id").and_then(serde_json::Value::as_str).unwrap_or(""),
                }),
            );
        }
    }

    let talos = running_talos(&state).await;
    let kubernetes = running_kubernetes(&state).await;
    if let Err(reason) = check_order(&body.op, &release, &candidate, &talos, &kubernetes) {
        return precondition(&reason);
    }
    if let Err(reason) = check_pools(&state).await {
        return precondition(&reason);
    }

    let image = if body.op == "naslos" {
        if candidate.api_image.is_empty() {
            return precondition("the release pack does not pin the API image by digest");
        }
        candidate.api_image.clone()
    } else {
        if state.system.api_image.is_empty() {
            return precondition("the API image is not configured for the upgrader Job");
        }
        state.system.api_image.clone()
    };
    if !image.contains("@sha256:") {
        return precondition(
            "the upgrader image is not digest-pinned; release a pack whose values-installer pins images",
        );
    }
    if state.system.talos_secret.is_empty() {
        return precondition("api.talosConfigSecret is not configured");
    }
    let Some(kube) = state.kube.clone() else {
        return unavailable("the Kubernetes client is not available");
    };

    let id = new_operation_id();
    let namespace = system_namespace();
    let spec = job_spec(&JobOptions {
        name: &operation_job_name(&id),
        namespace: &namespace,
        image: &image,
        op: &body.op,
        operation_id: &id,
        from: &release.naslos_version,
        version: &candidate.version,
        pack_url: &candidate.pack_url,
        pack_sha256: &candidate.pack_sha256,
        release_name: &state.system.release_name,
        service_account: state.system.service_account(),
        talos_secret: &state.system.talos_secret,
        state_pvc: &state.system.state_pvc,
        workload_namespace: &state.system.workload_namespace,
        backoff_limit: state.system.backoff_limit,
        ttl_seconds: state.system.ttl_seconds,
    });
    if let Err(e) = kube.apply(&spec).await {
        return json(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "error": format!("creating the upgrader Job: {e}") }),
        );
    }
    json(
        StatusCode::ACCEPTED,
        serde_json::json!({ "operationId": id, "job": operation_job_name(&id) }),
    )
}

/// `GET /api/system/operations/current`
pub async fn operation_current(State(state): State<Arc<AppState>>) -> Response {
    let job = read_job_status(&state).await;
    let Some(status) = read_status(&state).await else {
        // The Job writes the ConfigMap on its first start; until then its
        // existence is the only state. Returning idle here would make the UI
        // stop polling before the operation appears.
        return match job {
            Some(mut value) => {
                let log = job_log(&state, value["id"].as_str().unwrap_or(""))
                    .await
                    .unwrap_or_default();
                value["log"] = serde_json::json!(log);
                json(StatusCode::OK, value)
            }
            None => json(
                StatusCode::OK,
                serde_json::json!({"phase": "idle", "steps": [], "log": ""}),
            ),
        };
    };
    // A new operation's Job exists before it writes the ConfigMap. Prefer the
    // newer Job so a page reload during that window does not show the previous
    // operation's terminal state and stop polling.
    if let Some(job) = &job {
        let job_id = job
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let job_started = job
            .get("startedAt")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if job_id != status.id
            && job_started.get(..19).unwrap_or(job_started)
                >= status.started_at.get(..19).unwrap_or(&status.started_at)
        {
            let mut value = job.clone();
            let log = job_log(&state, job_id).await.unwrap_or_default();
            value["log"] = serde_json::json!(log);
            return json(StatusCode::OK, value);
        }
    }
    let log = job_log(&state, &status.id).await.unwrap_or_default();
    let mut value = serde_json::to_value(&status).unwrap_or_else(|_| serde_json::json!({}));
    value["log"] = serde_json::json!(log);
    json(StatusCode::OK, value)
}

async fn read_status(state: &AppState) -> Option<crate::upgrade::status::OperationStatus> {
    let kube = state.kube.as_ref()?;
    let namespace = system_namespace();
    let value = kube
        .get_json("configmap", state.system.configmap(), &namespace)
        .await
        .ok()??;
    let raw = value.get("data")?.get("state")?.as_str()?;
    serde_json::from_str(raw).ok()
}

async fn read_job_status(state: &AppState) -> Option<serde_json::Value> {
    let kube = state.kube.as_ref()?;
    let namespace = system_namespace();
    let jobs = kube
        .list_json(
            "jobs",
            &namespace,
            Some("app.kubernetes.io/name=naslos-system-op"),
        )
        .await
        .ok()?;
    let items = jobs.get("items")?.as_array()?;
    let job = items.iter().max_by_key(|job| {
        job.pointer("/metadata/creationTimestamp")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
    })?;
    let labels = job.get("metadata")?.get("labels")?;
    let id = labels.get("naslos.io/operation-id")?.as_str()?;
    let status = job.get("status").cloned().unwrap_or_default();
    let count = |key: &str| {
        status
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    };
    let phase = if count("succeeded") > 0 {
        "succeeded"
    } else if count("failed") > 0 {
        "failed"
    } else {
        "pending"
    };
    Some(serde_json::json!({
        "id": id,
        "op": labels.get("naslos.io/op").and_then(serde_json::Value::as_str).unwrap_or(""),
        "from": labels.get("naslos.io/from").and_then(serde_json::Value::as_str).unwrap_or(""),
        "to": labels.get("naslos.io/to").and_then(serde_json::Value::as_str).unwrap_or(""),
        "phase": phase,
        "steps": [],
        "message": "waiting for the upgrader Job to report",
        "startedAt": job.pointer("/metadata/creationTimestamp").and_then(serde_json::Value::as_str).unwrap_or(""),
        "finishedAt": "",
        "result": "",
        "attempts": 0,
        "log": "",
    }))
}

async fn job_log(state: &AppState, id: &str) -> Option<String> {
    let kube = state.kube.as_ref()?;
    let namespace = system_namespace();
    let selector = format!("job-name={}", operation_job_name(id));
    let pods = kube
        .list_json("pods", &namespace, Some(&selector))
        .await
        .ok()?;
    let pod = pods
        .get("items")?
        .as_array()?
        .first()?
        .pointer("/metadata/name")?
        .as_str()?;
    kube.pod_logs(&namespace, pod, 100).await.ok()
}

async fn running_talos(state: &AppState) -> String {
    match &state.talos {
        Some(talos) => talos.get_version().await.unwrap_or_default(),
        None => String::new(),
    }
}

async fn running_kubernetes(state: &AppState) -> String {
    match &state.kube {
        Some(kube) => kube.server_version().await.unwrap_or_default(),
        None => String::new(),
    }
}

fn check_order(
    op: &str,
    release: &ReleaseFacts,
    candidate: &Candidate,
    talos: &str,
    kubernetes: &str,
) -> Result<(), String> {
    let target = Version::parse(&candidate.version)
        .ok_or_else(|| "the release version is not strict semver".to_string())?;
    let current = Version::parse(&release.naslos_version);
    match op {
        "naslos" => {
            if current.map(|c| target <= c).unwrap_or(false) {
                return Err(format!(
                    "{} is not newer than {}",
                    candidate.version, release.naslos_version
                ));
            }
            if Version::parse(talos) != Version::parse(&release.talos_version) {
                return Err(format!(
                    "Talos is {talos}, this release pins {}; finish the pending Talos upgrade first",
                    release.talos_version
                ));
            }
            if Version::parse(kubernetes) != Version::parse(&release.kubernetes_version) {
                return Err(format!(
                    "Kubernetes is {kubernetes}, this release pins {}; finish the pending Kubernetes upgrade first",
                    release.kubernetes_version
                ));
            }
            let decision = stepping(release, candidate, talos, kubernetes);
            if !decision.allowed {
                return Err(decision.reason);
            }
        }
        "talos" => {
            if current != Some(target) {
                return Err("upgrade Naslos to this release before upgrading Talos".into());
            }
            let (Some(running), Some(target_talos)) = (
                Version::parse(talos),
                Version::parse(&candidate.talos_version),
            ) else {
                return Err("cannot read the running Talos version".into());
            };
            if target_talos <= running {
                return Err(format!(
                    "Talos {running} is already at or ahead of {target_talos}"
                ));
            }
            if target_talos.minor_distance(&running) > 1 {
                return Err(format!(
                    "Talos {target_talos} is more than one minor ahead of {running}"
                ));
            }
        }
        "kubernetes" => {
            if current != Some(target) {
                return Err("upgrade Naslos to this release before upgrading Kubernetes".into());
            }
            if Version::parse(talos) != Version::parse(&candidate.talos_version) {
                return Err(format!(
                    "Talos is {talos}, the release pins {}; upgrade Talos first",
                    candidate.talos_version
                ));
            }
            let (Some(running), Some(target_k8s)) = (
                Version::parse(kubernetes),
                Version::parse(&candidate.kubernetes_version),
            ) else {
                return Err("cannot read the running Kubernetes version".into());
            };
            if target_k8s <= running {
                return Err(format!(
                    "Kubernetes {running} is already at or ahead of {target_k8s}"
                ));
            }
            if target_k8s.minor_distance(&running) > 1 {
                return Err(format!(
                    "Kubernetes {target_k8s} is more than one minor ahead of {running}"
                ));
            }
        }
        _ => return Err("unknown operation".into()),
    }
    Ok(())
}

async fn check_pools(state: &AppState) -> Result<(), String> {
    let Some(agent) = state.agent.as_ref() else {
        return Err("the agent is unavailable; cannot verify ZFS pool health".into());
    };
    let pools = agent
        .list_pools()
        .await
        .map_err(|e| format!("cannot verify ZFS pool health: {e}"))?;
    for pool in pools {
        if !pool.health.eq_ignore_ascii_case("ONLINE") {
            return Err(format!(
                "pool {} is {}; fix it before upgrading",
                pool.name, pool.health
            ));
        }
    }
    Ok(())
}

fn system_namespace() -> String {
    std::env::var("NASLOS_NAMESPACE").unwrap_or_else(|_| "naslos".to_string())
}
