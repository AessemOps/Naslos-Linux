//! HTTP handlers for `/api/cloud/*` (frozen contract in
//! `/tmp/kilo/wp1-challenge/AMENDED.md` §E; docs/api.md).

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::Deserialize;

use crate::cloud::restore::{self, RestoreError, RestoreRequest};
use crate::cloud::send::{self, SendParams, StartError};
use crate::cloud::verify::{self, VerifyError, VerifyRequest};
use crate::cloud::{
    self, credential_flags, delete_managed_credentials, identity_path, load_credentials,
    managed_secret_name, normalize_destination, DestinationInput, DestinationView,
};
use crate::json;
use crate::state::AppState;

fn error_response(status: StatusCode, message: impl Into<String>) -> Response {
    json(status, serde_json::json!({ "error": message.into() }))
}

fn cloud_unavailable() -> Response {
    error_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "cloud backup is not enabled",
    )
}

fn store_unavailable(message: &str) -> Response {
    error_response(StatusCode::SERVICE_UNAVAILABLE, message)
}

fn intern(error: impl std::fmt::Display) -> Response {
    error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("cloud backup failed: {error}"),
    )
}

#[allow(clippy::result_large_err)]
async fn read_body<T: serde::de::DeserializeOwned>(body: Body) -> Result<T, Response> {
    crate::server::zfs::read_json(body).await
}

pub async fn destinations_get(State(state): State<Arc<AppState>>) -> Response {
    let Some(cloud) = state.cloud.as_ref() else {
        return cloud_unavailable();
    };
    if let Some(e) = cloud.destinations.load_error() {
        return store_unavailable(&e);
    }
    let destinations: Vec<DestinationView> = cloud
        .destinations
        .list()
        .iter()
        .map(DestinationView::from)
        .collect();
    json(
        StatusCode::OK,
        serde_json::json!({ "destinations": destinations }),
    )
}

pub async fn destination_get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let Some(cloud) = state.cloud.as_ref() else {
        return cloud_unavailable();
    };
    match cloud.destinations.get(&id) {
        Some(destination) => json(
            StatusCode::OK,
            serde_json::json!({ "destination": DestinationView::from(&destination) }),
        ),
        None => error_response(
            StatusCode::NOT_FOUND,
            format!("destination {id:?} not found"),
        ),
    }
}

pub async fn destination_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let input: DestinationInput = match read_body(body).await {
        Ok(input) => input,
        Err(response) => return response,
    };
    upsert_destination(&state, None, input).await
}

pub async fn destination_put(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Body,
) -> Response {
    let input: DestinationInput = match read_body(body).await {
        Ok(input) => input,
        Err(response) => return response,
    };
    upsert_destination(&state, Some(id), input).await
}

async fn upsert_destination(
    state: &Arc<AppState>,
    id: Option<String>,
    input: DestinationInput,
) -> Response {
    let Some(cloud) = state.cloud.as_ref() else {
        return cloud_unavailable();
    };
    if let Some(e) = cloud.destinations.load_error() {
        return store_unavailable(&e);
    }
    let existing = match &id {
        Some(id) => match cloud.destinations.get(id) {
            Some(destination) => Some(destination),
            None => {
                return error_response(
                    StatusCode::NOT_FOUND,
                    format!("destination {id:?} not found"),
                )
            }
        },
        None => None,
    };
    let creating = existing.is_none();
    let mut destination = existing.clone().unwrap_or_default();
    if creating {
        let mut buf = [0u8; 16];
        let _ = getrandom::fill(&mut buf);
        destination.id = buf.iter().map(|b| format!("{b:02x}")).collect();
        destination.enabled = true;
        destination.created_at = Some(chrono::Utc::now());
    }
    destination.name = input.name.clone();
    destination.endpoint = input.endpoint.clone();
    destination.region = input.region.clone();
    destination.bucket = input.bucket.clone();
    destination.prefix = input.prefix.clone();
    destination.path_style = input.path_style;
    destination.allow_insecure_http = input.allow_insecure_http;
    if let Some(enabled) = input.enabled {
        destination.enabled = enabled;
    }
    destination = match normalize_destination(destination) {
        Ok(normalized) => normalized,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, e),
    };

    // Credential precedence: an operator-managed Secret name, then inline
    // credentials, then keep what is stored. Inline and named are exclusive.
    let named = input.credentials_secret_name();
    let access = input.access_key();
    let secret = input.secret_key();
    let has_inline = access.is_some() || secret.is_some() || input.session_token_value().is_some();
    if named.is_some() && has_inline {
        return error_response(
            StatusCode::BAD_REQUEST,
            "supply either credentialsSecret or inline credentials, not both",
        );
    }
    if let Some(name) = named {
        if let Err(e) = crate::kube::validate_credentials_secret(&name) {
            return error_response(StatusCode::BAD_REQUEST, e);
        }
        if !name.starts_with("naslos-cloud-") {
            return error_response(
                StatusCode::BAD_REQUEST,
                "credentialsSecret must start with \"naslos-cloud-\"",
            );
        }
        match cloud::load_named_credentials(state, &name).await {
            Ok((_, (has_access, has_secret, has_token))) => {
                destination.credentials_secret = name;
                destination.credentials_managed = false;
                destination.has_access_key = has_access;
                destination.has_secret_key = has_secret;
                destination.has_session_token = has_token;
            }
            Err(e) => return error_response(StatusCode::BAD_REQUEST, e),
        }
    } else if has_inline {
        let (Some(access), Some(secret)) = (access, secret) else {
            return error_response(
                StatusCode::BAD_REQUEST,
                "both accessKeyId and secretAccessKey are required",
            );
        };
        if state.kube.is_none() {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "the Kubernetes client is unavailable, so credentials cannot be stored",
            );
        }
        let name = managed_secret_name(&destination.id);
        if let Err(e) = cloud::write_managed_credentials(
            state,
            &destination.id,
            &access,
            &secret,
            input.session_token_value().as_deref(),
        )
        .await
        {
            return intern(e);
        }
        destination.credentials_secret = name;
        destination.credentials_managed = true;
        destination.has_access_key = true;
        destination.has_secret_key = true;
        destination.has_session_token =
            input.session_token_value().is_some() || destination.has_session_token;
    } else if creating {
        return error_response(
            StatusCode::BAD_REQUEST,
            "credentials are required: set accessKeyId and secretAccessKey, or credentialsSecret",
        );
    } else {
        // Keep the stored Secret; refresh the flags from it when possible.
        let flags = credential_flags(state, &destination).await;
        destination.has_access_key = flags.0;
        destination.has_secret_key = flags.1;
        destination.has_session_token = flags.2;
    }

    // A changed endpoint/bucket or new credentials invalidate the capability
    // probe; the next /test re-learns it.
    if existing.as_ref().is_some_and(|old| {
        old.endpoint != destination.endpoint
            || old.bucket != destination.bucket
            || old.credentials_secret != destination.credentials_secret
    }) {
        destination.conditional_create = false;
    }
    destination.last_error.clear();

    // The plaintext acknowledgement is an explicit, audited decision.
    if let Ok(endpoint) =
        cloud::net::parse_endpoint(&destination.endpoint, destination.allow_insecure_http)
    {
        if endpoint.plaintext && destination.allow_insecure_http {
            let pending = crate::audit::Pending::system(
                "cloud",
                "insecure_http_acknowledged",
                Some(destination.id.clone()),
                serde_json::json!({"transport": "http"}),
            );
            if let Err(e) = state.audit.record_once_async(&pending, 200).await {
                tracing::warn!("could not audit the cloud insecure-http acknowledgement: {e}");
            }
        }
    }

    if let Err(e) = cloud.destinations.put(destination.clone()) {
        if creating && destination.credentials_managed {
            cloud::delete_managed_credentials(state, &destination).await;
        }
        return intern(e);
    }
    let status = if creating {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    json(
        status,
        serde_json::json!({ "destination": DestinationView::from(&destination) }),
    )
}

pub async fn destination_delete(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let Some(cloud) = state.cloud.as_ref() else {
        return cloud_unavailable();
    };
    let Some(destination) = cloud.destinations.get(&id) else {
        return error_response(
            StatusCode::NOT_FOUND,
            format!("destination {id:?} not found"),
        );
    };
    if cloud.jobs.has_running_for_destination(&id) {
        return error_response(
            StatusCode::CONFLICT,
            format!(
                "destination {:?} is used by a running job",
                destination.name
            ),
        );
    }
    let used_by_schedule = cloud
        .schedules
        .list()
        .into_iter()
        .find(|schedule| schedule.destinations.iter().any(|d| d == &id));
    if let Some(schedule) = used_by_schedule {
        return error_response(
            StatusCode::CONFLICT,
            format!(
                "destination {:?} is used by cloud schedule {:?}; remove it from the schedule first",
                destination.name, schedule.id
            ),
        );
    }
    if let Err(e) = cloud.destinations.remove(&id) {
        return intern(e);
    }
    delete_managed_credentials(&state, &destination).await;
    json(
        StatusCode::OK,
        serde_json::json!({ "status": "deleted", "id": id }),
    )
}

pub async fn destination_test(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let Some(cloud) = state.cloud.as_ref() else {
        return cloud_unavailable();
    };
    let Some(destination) = cloud.destinations.get(&id) else {
        return error_response(
            StatusCode::NOT_FOUND,
            format!("destination {id:?} not found"),
        );
    };
    let credentials = match load_credentials(&state, &destination).await {
        Ok(credentials) => credentials,
        Err(e) => {
            tracing::warn!("cloud destination {id} test: credentials unavailable: {e}");
            return json(
                StatusCode::OK,
                serde_json::json!({
                    "ok": false,
                    "category": "auth",
                    "conditionalCreate": false,
                }),
            );
        }
    };
    let s3 = match cloud::s3::S3Storage::build(
        &destination,
        &credentials,
        &cloud::net::NetConfig::from_env(),
    ) {
        Ok(s3) => s3,
        Err(e) => {
            return record_test_result(&state, &id, false, Some(e.category), false).await;
        }
    };
    let (ok, category, conditional) = s3.test().await;
    record_test_result(&state, &id, ok, category, conditional).await
}

async fn record_test_result(
    state: &Arc<AppState>,
    id: &str,
    ok: bool,
    category: Option<cloud::s3::Category>,
    conditional: bool,
) -> Response {
    let Some(cloud) = state.cloud.as_ref() else {
        return cloud_unavailable();
    };
    let updated = cloud.destinations.update(id, |destination| {
        destination.last_test_at = Some(chrono::Utc::now());
        destination.conditional_create = conditional;
        destination.last_error = if ok {
            String::new()
        } else {
            category.map(|c| c.as_str().to_string()).unwrap_or_default()
        };
        Ok(())
    });
    if let Err(e) = updated {
        tracing::warn!("could not record the cloud destination test result: {e}");
    }
    json(
        StatusCode::OK,
        serde_json::json!({
            "ok": ok,
            "category": category.map(|c| c.as_str()),
            "conditionalCreate": conditional,
        }),
    )
}

#[derive(Debug, Deserialize)]
struct SendRequest {
    #[serde(default)]
    dataset: String,
    #[serde(default)]
    destination: String,
    #[serde(default, rename = "keepLineages")]
    keep_lineages: Option<usize>,
    #[serde(default, rename = "forceFull")]
    force_full: bool,
}

pub async fn send_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let Some(_cloud) = state.cloud.as_ref() else {
        return cloud_unavailable();
    };
    let request: SendRequest = match read_body(body).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    if let Err(e) = crate::server::zfs::validate_dataset_path(&request.dataset) {
        return error_response(StatusCode::BAD_REQUEST, e);
    }
    if request.destination.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "destination is required");
    }
    if crate::buddy::keys::load_identity(&identity_path()).is_err() {
        return error_response(
            StatusCode::PRECONDITION_FAILED,
            "a Buddy identity is required for cloud backup: create one on the Backups page first",
        );
    }
    let Some(destination) = state
        .cloud
        .as_ref()
        .and_then(|c| c.destinations.get(request.destination.trim()))
    else {
        return error_response(
            StatusCode::NOT_FOUND,
            format!("destination {:?} not found", request.destination),
        );
    };
    if let Err(e) = load_credentials(&state, &destination).await {
        return error_response(StatusCode::PRECONDITION_FAILED, e);
    }
    if state.agent.is_none() {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "the API has no agent client, so it cannot stream ZFS data",
        );
    }
    let keep = match request.keep_lineages {
        Some(value) => match send::valid_keep_lineages(value) {
            Ok(value) => value,
            Err(e) => return error_response(StatusCode::BAD_REQUEST, e),
        },
        None => 2,
    };
    let params = SendParams {
        dataset: request.dataset,
        destination: destination.id,
        keep_lineages: keep,
        force_full: request.force_full,
        schedule_id: String::new(),
    };
    match send::enqueue_send(&state, params) {
        Ok(job_id) => json(
            StatusCode::ACCEPTED,
            serde_json::json!({ "jobId": job_id, "status": "started" }),
        ),
        Err(error) => start_error(error),
    }
}

fn start_error(error: StartError) -> Response {
    match error {
        StartError::NotFound(e) => error_response(StatusCode::NOT_FOUND, e),
        StartError::BadRequest(e) => error_response(StatusCode::BAD_REQUEST, e),
        StartError::ServiceUnavailable(e) => error_response(StatusCode::SERVICE_UNAVAILABLE, e),
        StartError::Conflict(e) => error_response(StatusCode::CONFLICT, e),
    }
}

fn restore_error(error: RestoreError) -> Response {
    match error {
        RestoreError::NotFound(e) => error_response(StatusCode::NOT_FOUND, e),
        RestoreError::BadRequest(e) => error_response(StatusCode::BAD_REQUEST, e),
        RestoreError::Precondition(e) => error_response(StatusCode::PRECONDITION_FAILED, e),
        RestoreError::Conflict(e) => error_response(StatusCode::CONFLICT, e),
        RestoreError::ServiceUnavailable(e) => error_response(StatusCode::SERVICE_UNAVAILABLE, e),
        RestoreError::Upstream(e) => error_response(StatusCode::BAD_GATEWAY, e),
    }
}

fn verify_error(error: VerifyError) -> Response {
    match error {
        VerifyError::NotFound(e) => error_response(StatusCode::NOT_FOUND, e),
        VerifyError::BadRequest(e) => error_response(StatusCode::BAD_REQUEST, e),
        VerifyError::Precondition(e) => error_response(StatusCode::PRECONDITION_FAILED, e),
        VerifyError::Conflict(e) => error_response(StatusCode::CONFLICT, e),
        VerifyError::ServiceUnavailable(e) => error_response(StatusCode::SERVICE_UNAVAILABLE, e),
        VerifyError::Upstream(e) => error_response(StatusCode::BAD_GATEWAY, e),
    }
}

pub async fn restore_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let Some(_cloud) = state.cloud.as_ref() else {
        return cloud_unavailable();
    };
    let request: RestoreRequest = match read_body(body).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    if let Err(e) = crate::server::zfs::validate_dataset_path(&request.dataset) {
        return error_response(StatusCode::BAD_REQUEST, e);
    }
    if request.source.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "source is required");
    }
    let Some(agent) = state.agent.clone() else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "the API has no agent client, so it cannot receive ZFS data",
        );
    };
    match agent.list_datasets().await {
        Ok(datasets) => {
            if datasets.iter().any(|d| d.name == request.dataset) {
                return error_response(
                    StatusCode::CONFLICT,
                    format!(
                        "dataset {} already exists; a cloud restore only creates a new dataset",
                        request.dataset
                    ),
                );
            }
        }
        Err(e) => {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                format!("listing datasets: {e}"),
            )
        }
    }
    let key = match restore::resolve_key(&state, &agent, &request.source, &request.key_hex).await {
        Ok(key) => key,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, e),
    };
    let plan = match restore::prepare(&state, &request, key.is_some()).await {
        Ok(plan) => plan,
        Err(e) => return restore_error(e),
    };
    match restore::enqueue(&state, plan, request) {
        Ok(job_id) => json(
            StatusCode::ACCEPTED,
            serde_json::json!({ "jobId": job_id, "status": "started" }),
        ),
        Err(e) => restore_error(e),
    }
}

pub async fn verify_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let request: VerifyRequest = match read_body(body).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    let mode = match verify::VerifyMode::parse(&request.mode) {
        Ok(mode) => mode,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, e),
    };
    match verify::enqueue(&state, request, mode).await {
        Ok(job_id) => json(
            StatusCode::ACCEPTED,
            serde_json::json!({ "jobId": job_id, "status": "started" }),
        ),
        Err(e) => verify_error(e),
    }
}

pub async fn jobs_get(State(state): State<Arc<AppState>>) -> Response {
    let Some(cloud) = state.cloud.as_ref() else {
        return cloud_unavailable();
    };
    json(
        StatusCode::OK,
        serde_json::json!({ "jobs": cloud.jobs.list() }),
    )
}

pub async fn job_get(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let Some(cloud) = state.cloud.as_ref() else {
        return cloud_unavailable();
    };
    match cloud.jobs.get(&id) {
        Some(job) => json(
            StatusCode::OK,
            serde_json::to_value(job.snapshot()).unwrap_or_default(),
        ),
        None => error_response(StatusCode::NOT_FOUND, format!("job {id:?} not found")),
    }
}

pub async fn job_delete(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let Some(cloud) = state.cloud.as_ref() else {
        return cloud_unavailable();
    };
    match cloud.jobs.cancel(&id) {
        Ok(()) => json(
            StatusCode::OK,
            serde_json::json!({ "status": "cancelling", "id": id }),
        ),
        Err(None) => error_response(StatusCode::NOT_FOUND, format!("job {id:?} not found")),
        Err(Some(state)) => error_response(
            StatusCode::CONFLICT,
            format!(
                "job {id:?} is already {}",
                match state {
                    cloud::jobs::JobState::Succeeded => "succeeded",
                    cloud::jobs::JobState::Failed => "failed",
                    cloud::jobs::JobState::Cancelled => "cancelled",
                    cloud::jobs::JobState::Running => "running",
                }
            ),
        ),
    }
}

#[derive(Debug, Deserialize)]
pub struct ChainsQuery {
    #[serde(default)]
    destination: String,
    #[serde(default)]
    source: String,
}

pub async fn chains_get(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ChainsQuery>,
) -> Response {
    let Some(cloud) = state.cloud.as_ref() else {
        return cloud_unavailable();
    };
    if query.destination.trim().is_empty() || query.source.trim().is_empty() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "destination and source are required",
        );
    }
    let Some(destination) = cloud.destinations.get(query.destination.trim()) else {
        return error_response(
            StatusCode::NOT_FOUND,
            format!("destination {:?} not found", query.destination),
        );
    };
    let source = match cloud::source_for_dataset(query.source.trim()) {
        Ok(source) => source,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, e),
    };
    let credentials = match load_credentials(&state, &destination).await {
        Ok(credentials) => credentials,
        Err(e) => return error_response(StatusCode::PRECONDITION_FAILED, e),
    };
    let identity = match crate::buddy::keys::load_identity(&identity_path()) {
        Ok(identity) => identity,
        Err(e) => return error_response(StatusCode::PRECONDITION_FAILED, e),
    };
    let s3 = match cloud::s3::S3Storage::build(
        &destination,
        &credentials,
        &cloud::net::NetConfig::from_env(),
    ) {
        Ok(s3) => s3,
        Err(e) => return error_response(StatusCode::BAD_GATEWAY, e.to_string()),
    };
    match cloud::store::list_links(&s3, &source, &identity).await {
        Ok(links) => {
            let chains: Vec<cloud::ChainView> = links.iter().map(|l| l.view()).collect();
            json(StatusCode::OK, serde_json::json!({ "chains": chains }))
        }
        Err(e) => error_response(StatusCode::BAD_GATEWAY, e),
    }
}
