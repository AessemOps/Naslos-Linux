//! Installed-app handlers and the async job endpoints (port of
//! `server/apps.go` app part and `server/app_jobs.go`).

use crate::apps::{ExposurePatch, InstallRequest};
use crate::json;
use crate::state::AppState;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use std::sync::Arc;

use super::app_jobs::{JobKind, JobState};

fn app_unavailable() -> Response {
    json(
        StatusCode::SERVICE_UNAVAILABLE,
        serde_json::json!({ "error": "app management is not available" }),
    )
}

fn bad_request(msg: &str) -> Response {
    json(StatusCode::BAD_REQUEST, serde_json::json!({ "error": msg }))
}

fn base_domain_selectable(state: &AppState, domain: &str) -> bool {
    state.base_domain.is_empty() || super::domains::base_domain_selectable(state, domain)
}

fn selectable_domains(state: &AppState) -> Vec<String> {
    super::domains::selectable_domains(state)
}

/// `GET|POST /api/apps`
pub async fn apps_get(State(state): State<Arc<AppState>>) -> Response {
    let Some(manager) = state.app_manager.clone() else {
        return app_unavailable();
    };
    match manager.list().await {
        Ok(views) => json(StatusCode::OK, serde_json::to_value(views).unwrap()),
        Err(e) => json(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "error": e }),
        ),
    }
}

#[derive(Default, serde::Deserialize)]
struct InstallBody {
    #[serde(default)]
    name: String,
    #[serde(default)]
    values: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(default)]
    exposure: Option<ExposurePatch>,
    #[serde(default, rename = "baseDomain")]
    base_domain: String,
    #[serde(default)]
    confirmed: bool,
    #[serde(default, rename = "privilegedConfirmed")]
    privileged_confirmed: bool,
    #[serde(default)]
    datasets: Vec<String>,
    #[serde(default)]
    pool: String,
    #[serde(default)]
    env: Vec<crate::app_env::EnvVar>,
}

/// A privileged catalog entry moves the app into the PSA-privileged namespace,
/// so a generic "I reviewed the source" click is not enough (SEC-2026-D8).
fn require_privileged_confirmation(privileged: bool, confirmed: bool) -> Result<(), String> {
    if privileged && !confirmed {
        return Err(
            "this app runs privileged (it is installed into the privileged namespace); set privilegedConfirmed=true after reviewing that fact"
                .to_string(),
        );
    }
    Ok(())
}

pub async fn apps_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    if state.app_manager.is_none() {
        return app_unavailable();
    }
    let req: InstallBody = match super::zfs::read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    if !req.confirmed {
        return bad_request("install must be explicitly confirmed");
    }
    let privileged = match state.catalog.load() {
        Some(catalog) => match catalog.get(&req.name) {
            Ok(app) => {
                if !req.env.is_empty()
                    && !app
                        .capabilities
                        .iter()
                        .any(|capability| capability == crate::app_env::CAPABILITY)
                {
                    return bad_request("this chart does not support custom environment variables");
                }
                app.privileged
            }
            Err(e) => return json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e })),
        },
        None => return app_unavailable(),
    };
    if let Err(e) = require_privileged_confirmation(privileged, req.privileged_confirmed) {
        return bad_request(&e);
    }
    if !req.base_domain.is_empty() && !base_domain_selectable(&state, &req.base_domain) {
        return bad_request(&format!(
            "base domain {:?} is not configured",
            req.base_domain
        ));
    }
    if !req.pool.is_empty() {
        let Some(agent) = state.agent.as_ref() else {
            return app_unavailable();
        };
        match agent.list_pools().await {
            Ok(pools) if !pools.iter().any(|pool| pool.name == req.pool) => {
                return bad_request(&format!("storage pool {:?} does not exist", req.pool));
            }
            Ok(_) => {}
            Err(e) => {
                return json(
                    StatusCode::BAD_GATEWAY,
                    serde_json::json!({ "error": format!("listing storage pools: {e}") }),
                );
            }
        }
    }
    let install = InstallRequest {
        name: req.name.clone(),
        values: req.values.clone(),
        exposure: req.exposure.clone(),
        base_domain: req.base_domain.clone(),
        datasets: req.datasets.clone(),
        pool: req.pool.clone(),
        privileged_confirmed: req.privileged_confirmed,
        env: req.env.clone(),
    };
    match state.app_jobs.enqueue(
        JobKind::Install,
        &req.name,
        &req.base_domain,
        Some(install),
        None,
        None,
        None,
        state.app_manager.clone(),
    ) {
        Ok(job) => accepted(&job),
        Err(conflict) => conflict_response(&conflict),
    }
}

/// `GET|PUT|DELETE /api/apps/{name}`
pub async fn app_detail_get(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    let Some(manager) = state.app_manager.clone() else {
        return app_unavailable();
    };
    match manager.get(&name) {
        Ok(rec) => {
            let isolated = manager.record_is_isolated(&rec);
            match manager.view(&name).await {
                Ok(view) => {
                    let mut value = serde_json::to_value(&view).unwrap_or_default();
                    // Whether the record runs in a per-app isolation namespace.
                    // The UI uses this to block dataset edits that require a
                    // reinstall.
                    value["isolated"] = serde_json::json!(isolated);
                    // Values come from the env Secret; secret variables are
                    // masked. Only the detail endpoint reads the Secret.
                    value["env"] =
                        serde_json::json!(manager.env_detail(&name).await.unwrap_or_default());
                    json(StatusCode::OK, value)
                }
                Err(e) => json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e })),
            }
        }
        Err(e) => json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e })),
    }
}

/// `GET /api/apps/namespaces`
pub async fn app_namespaces_get(State(state): State<Arc<AppState>>) -> Response {
    let Some(manager) = state.app_manager.clone() else {
        return app_unavailable();
    };
    json(StatusCode::OK, manager.pool_status())
}

#[derive(Default, serde::Deserialize)]
struct NamespaceLimitBody {
    #[serde(default)]
    limit: usize,
}

/// `PUT /api/apps/namespaces` — size the usable per-app namespace pool.
///
/// The chart provisions the full pool; raising the limit up to the provisioned
/// count takes effect immediately (no platform upgrade), and it can never drop
/// below the apps already installed.
pub async fn app_namespaces_put(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let Some(manager) = state.app_manager.clone() else {
        return app_unavailable();
    };
    let req: NamespaceLimitBody = match super::zfs::read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    match manager.set_namespace_limit(req.limit) {
        Ok(status) => json(StatusCode::OK, status),
        Err(e) => bad_request(&e),
    }
}

/// `GET /api/apps/{name}/pods`
///
/// The pods (and containers) the drawer's live log stream can attach to. An
/// isolated app owns its whole namespace; a legacy app in the shared namespace
/// is filtered by the chart's `app.kubernetes.io/instance` label.
pub async fn app_pods_get(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    let Some(manager) = state.app_manager.clone() else {
        return app_unavailable();
    };
    let rec = match manager.get(&name) {
        Ok(rec) => rec,
        Err(e) => return json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e })),
    };
    let Some(kube) = state.kube.clone() else {
        return json(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({ "error": "cannot reach the Kubernetes API: no client configured" }),
        );
    };
    let namespace = manager.record_namespace(&rec);
    let isolated = manager.record_is_isolated(&rec);
    let list = match kube.list_json("pods", &namespace, None).await {
        Ok(list) => list,
        Err(e) => {
            return json(
                StatusCode::BAD_GATEWAY,
                serde_json::json!({ "error": format!("listing pods: {e}") }),
            )
        }
    };
    let mut pods: Vec<serde_json::Value> = list
        .get("items")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|pod| pod_summary(pod, &rec.name, isolated))
                .collect()
        })
        .unwrap_or_default();
    // Running pods first, then by name: the drawer defaults to the first entry.
    pods.sort_by(|a, b| {
        let running = |p: &serde_json::Value| p["phase"].as_str() != Some("Running");
        running(a).cmp(&running(b)).then_with(|| {
            a["name"]
                .as_str()
                .unwrap_or("")
                .cmp(b["name"].as_str().unwrap_or(""))
        })
    });
    json(
        StatusCode::OK,
        serde_json::json!({ "namespace": namespace, "pods": pods }),
    )
}

fn pod_summary(pod: &serde_json::Value, app: &str, isolated: bool) -> Option<serde_json::Value> {
    let instance = pod
        .pointer("/metadata/labels/app.kubernetes.io~1instance")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !isolated && instance != app {
        return None;
    }
    let name = pod.pointer("/metadata/name")?.as_str()?.to_string();
    let phase = pod
        .pointer("/status/phase")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown");
    let started_at = pod.pointer("/status/startTime").and_then(|v| v.as_str());
    // Status carries readiness/restarts; a pending pod has none yet, so fall
    // back to the spec so the container picker still works.
    let mut containers: Vec<serde_json::Value> = pod
        .pointer("/status/containerStatuses")
        .and_then(|v| v.as_array())
        .map(|statuses| {
            statuses
                .iter()
                .filter_map(|c| {
                    let name = c.get("name")?.as_str()?;
                    Some(serde_json::json!({
                        "name": name,
                        "ready": c.get("ready").and_then(|v| v.as_bool()).unwrap_or(false),
                        "restartCount": c.get("restartCount").and_then(|v| v.as_u64()).unwrap_or(0),
                    }))
                })
                .collect()
        })
        .unwrap_or_default();
    if containers.is_empty() {
        containers = pod
            .pointer("/spec/containers")
            .and_then(|v| v.as_array())
            .map(|specs| {
                specs
                    .iter()
                    .filter_map(|c| {
                        let name = c.get("name")?.as_str()?;
                        Some(serde_json::json!({
                            "name": name,
                            "ready": false,
                            "restartCount": 0,
                        }))
                    })
                    .collect()
            })
            .unwrap_or_default();
    }
    Some(serde_json::json!({
        "name": name,
        "phase": phase,
        "startedAt": started_at,
        "containers": containers,
    }))
}

#[derive(Default, serde::Deserialize)]
struct UpgradeBody {
    #[serde(default)]
    values: Option<serde_json::Map<String, serde_json::Value>>,
    /// Omitted keeps the stored dataset list; `[]` detaches every dataset.
    #[serde(default)]
    datasets: Option<Vec<String>>,
    /// Omitted keeps the stored environment; `[]` removes every variable.
    #[serde(default)]
    env: Option<Vec<crate::app_env::EnvVar>>,
}

pub async fn app_detail_put(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    body: Body,
) -> Response {
    let Some(manager) = state.app_manager.clone() else {
        return app_unavailable();
    };
    let req: UpgradeBody = match super::zfs::read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    if let Err(e) = manager.get(&name) {
        return json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e }));
    }
    let job = match state.app_jobs.enqueue(
        JobKind::Upgrade,
        &name,
        "",
        None,
        req.values,
        req.datasets,
        req.env,
        Some(manager),
    ) {
        Ok(job) => job,
        Err(conflict) => return conflict_response(&conflict),
    };
    accepted(&job)
}

pub async fn app_detail_delete(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    let Some(manager) = state.app_manager.clone() else {
        return app_unavailable();
    };
    if let Err(e) = manager.get(&name) {
        return json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e }));
    }
    let job = match state.app_jobs.enqueue(
        JobKind::Uninstall,
        &name,
        "",
        None,
        None,
        None,
        None,
        Some(manager),
    ) {
        Ok(job) => job,
        Err(conflict) => return conflict_response(&conflict),
    };
    accepted(&job)
}

/// `GET /api/apps/{name}/quota`
///
/// The per-app namespace quota the resource editor must respect. `{available:
/// false}` when the app has no dedicated quota (legacy shared namespace).
pub async fn app_quota_get(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    let Some(manager) = state.app_manager.clone() else {
        return app_unavailable();
    };
    if let Err(e) = manager.get(&name) {
        return json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e }));
    }
    match manager.resource_quota(&name).await {
        Ok(Some(quota)) => json(StatusCode::OK, quota),
        Ok(None) => json(StatusCode::OK, serde_json::json!({ "available": false })),
        Err(e) => json(StatusCode::BAD_GATEWAY, serde_json::json!({ "error": e })),
    }
}

/// `GET /api/apps/{name}/services`
pub async fn app_services_get(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    let Some(manager) = state.app_manager.clone() else {
        return app_unavailable();
    };
    match manager.discover_services(&name).await {
        Ok(services) => json(StatusCode::OK, serde_json::to_value(services).unwrap()),
        Err(e) => json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e })),
    }
}

/// `GET|PUT /api/apps/{name}/exposure`
pub async fn app_exposure_get(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    let Some(manager) = state.app_manager.clone() else {
        return app_unavailable();
    };
    let rec = match manager.get(&name) {
        Ok(r) => r,
        Err(e) => return json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e })),
    };
    let base_domain = if rec.base_domain.is_empty() {
        state.base_domain.clone()
    } else {
        rec.base_domain.clone()
    };
    json(
        StatusCode::OK,
        serde_json::json!({
            "exposure": rec.exposure,
            "baseDomain": base_domain,
            "primaryDomain": state.base_domain,
            "selectableDomains": selectable_domains(&state),
            "ssoDomains": super::domains::effective_sso_domains(&state),
            "authAllowed": manager.auth_allowed(&rec.base_domain),
            "lastError": rec.last_error,
        }),
    )
}

#[derive(Default, serde::Deserialize)]
struct ExposureBody {
    #[serde(default)]
    exposure: crate::apps::ExposurePatch,
    #[serde(default, rename = "baseDomain")]
    base_domain: String,
}

pub async fn app_exposure_put(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    body: Body,
) -> Response {
    let Some(manager) = state.app_manager.clone() else {
        return app_unavailable();
    };
    let req: ExposureBody = match super::zfs::read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    if !req.base_domain.is_empty() && !base_domain_selectable(&state, &req.base_domain) {
        return bad_request(&format!(
            "base domain {:?} is not configured",
            req.base_domain
        ));
    }
    match manager
        .set_exposure(&name, &req.exposure, &req.base_domain)
        .await
    {
        Ok(view) => json(StatusCode::OK, serde_json::to_value(view).unwrap()),
        Err(e) => bad_request(&e),
    }
}

/// `GET /api/apps/updates`
///
/// Record-only summary (no Helm calls) for the Installed tab badge and the
/// sidebar dot. Always 200: a missing app manager reports zero updates.
pub async fn app_updates_get(State(state): State<Arc<AppState>>) -> Response {
    let Some(manager) = state.app_manager.clone() else {
        return json(
            StatusCode::OK,
            serde_json::json!({ "count": 0, "apps": [] }),
        );
    };
    let (count, apps) = manager.update_summaries();
    json(
        StatusCode::OK,
        serde_json::json!({ "count": count, "apps": apps }),
    )
}

/// `GET /api/apps/usage`
///
/// Bulk CPU/RAM/network for every installed app, cached server-side (~4s).
/// Always 200: `available:false` carries the reason so the UI hides the
/// metrics column instead of treating a missing kube client as an error.
pub async fn app_usage_get(State(state): State<Arc<AppState>>) -> Response {
    let Some(manager) = state.app_manager.clone() else {
        return json(
            StatusCode::OK,
            serde_json::to_value(crate::app_usage::UsageReport::unavailable(
                "app management is not available",
            ))
            .unwrap_or_default(),
        );
    };
    json(
        StatusCode::OK,
        serde_json::to_value(manager.usage().await).unwrap_or_default(),
    )
}

/// `GET /api/apps/jobs`
pub async fn app_jobs_get(State(state): State<Arc<AppState>>) -> Response {
    json(
        StatusCode::OK,
        serde_json::json!({ "jobs": state.app_jobs.list() }),
    )
}

/// `GET /api/apps/jobs/{id}`
pub async fn app_job_detail_get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match state.app_jobs.get(&id) {
        Some(job) => json(
            StatusCode::OK,
            serde_json::to_value(job.snapshot()).unwrap(),
        ),
        None => json(
            StatusCode::NOT_FOUND,
            serde_json::json!({ "error": format!("job {id:?} not found") }),
        ),
    }
}

fn accepted(job: &super::app_jobs::JobPublic) -> Response {
    json(
        StatusCode::ACCEPTED,
        serde_json::json!({ "jobId": job.id, "state": JobState::Running }),
    )
}

fn conflict_response(conflict: &super::app_jobs::JobPublic) -> Response {
    json(
        StatusCode::CONFLICT,
        serde_json::json!({
            "error": format!(
                "an app job for {:?} is already running (job {}, {:?}, started {})",
                conflict.app,
                conflict.id,
                conflict.kind,
                conflict.started_at.to_rfc3339()
            )
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::{pod_summary, require_privileged_confirmation};
    use serde_json::json;

    #[test]
    fn privileged_installs_need_a_second_confirmation() {
        assert!(require_privileged_confirmation(false, false).is_ok());
        assert!(require_privileged_confirmation(true, false).is_err());
        assert!(require_privileged_confirmation(true, true).is_ok());
    }

    fn pod(instance: &str, phase: &str, with_status: bool) -> serde_json::Value {
        let status = if with_status {
            json!({
                "phase": phase,
                "startTime": "2026-11-10T00:00:00Z",
                "containerStatuses": [
                    { "name": "demo", "ready": true, "restartCount": 2 }
                ]
            })
        } else {
            json!({ "phase": phase })
        };
        json!({
            "metadata": {
                "name": "demo-abc",
                "labels": { "app.kubernetes.io/instance": instance }
            },
            "spec": { "containers": [{ "name": "demo" }] },
            "status": status
        })
    }

    #[test]
    fn isolated_namespace_lists_every_pod_with_status() {
        let summary = pod_summary(&pod("other", "Running", true), "demo", true).unwrap();
        assert_eq!(summary["name"], "demo-abc");
        assert_eq!(summary["phase"], "Running");
        assert_eq!(summary["containers"][0]["name"], "demo");
        assert_eq!(summary["containers"][0]["restartCount"], 2);
    }

    #[test]
    fn shared_namespace_filters_by_instance_label() {
        assert!(pod_summary(&pod("other", "Running", true), "demo", false).is_none());
        assert!(pod_summary(&pod("demo", "Running", true), "demo", false).is_some());
    }

    #[test]
    fn pending_pod_falls_back_to_spec_containers() {
        let summary = pod_summary(&pod("demo", "Pending", false), "demo", true).unwrap();
        assert_eq!(summary["phase"], "Pending");
        assert_eq!(summary["containers"][0]["name"], "demo");
        assert_eq!(summary["containers"][0]["ready"], false);
    }
}
