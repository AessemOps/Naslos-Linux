//! Installed-app handlers and the async job endpoints (port of
//! `server/apps.go` app part and `server/app_jobs.go`).

use crate::apps::{Exposure, InstallRequest};
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
    exposure: Option<Exposure>,
    #[serde(default, rename = "baseDomain")]
    base_domain: String,
    #[serde(default)]
    confirmed: bool,
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
    if !req.base_domain.is_empty() && !base_domain_selectable(&state, &req.base_domain) {
        return bad_request(&format!(
            "base domain {:?} is not configured",
            req.base_domain
        ));
    }
    if let Some(conflict) = state.app_jobs.conflicting(&req.name) {
        return conflict_response(&conflict);
    }
    let install = InstallRequest {
        name: req.name.clone(),
        values: req.values.clone(),
        exposure: req.exposure.clone(),
        base_domain: req.base_domain.clone(),
    };
    let job = state.app_jobs.enqueue(
        JobKind::Install,
        &req.name,
        &req.base_domain,
        Some(install),
        None,
        state.app_manager.clone(),
    );
    accepted(&job)
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
            // A live status view (the manager's view() is private); report the
            // record with a coarse status.
            let mut view = serde_json::to_value(&rec).unwrap();
            view["status"] = serde_json::json!("unknown");
            json(StatusCode::OK, view)
        }
        Err(e) => json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e })),
    }
}

#[derive(Default, serde::Deserialize)]
struct UpgradeBody {
    #[serde(default)]
    values: Option<serde_json::Map<String, serde_json::Value>>,
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
    if let Some(conflict) = state.app_jobs.conflicting(&name) {
        return conflict_response(&conflict);
    }
    let job = state
        .app_jobs
        .enqueue(JobKind::Upgrade, &name, "", None, req.values, Some(manager));
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
    if let Some(conflict) = state.app_jobs.conflicting(&name) {
        return conflict_response(&conflict);
    }
    let job = state
        .app_jobs
        .enqueue(JobKind::Uninstall, &name, "", None, None, Some(manager));
    accepted(&job)
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
    exposure: Exposure,
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
