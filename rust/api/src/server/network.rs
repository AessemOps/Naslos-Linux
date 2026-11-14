//! Host network settings handlers (FR-NET).
//!
//! Owner/admin routes only: inventory, start a guarded change, poll the change,
//! and revert it. The state machine lives in `crate::network::changes`.

use crate::json;
use crate::network::inventory::{self, ApplianceView};
use crate::network::validate::ChangeRequest;
use crate::network::StartError;
use crate::state::AppState;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use std::sync::Arc;

/// `GET /api/network`
pub async fn network_get(State(state): State<Arc<AppState>>) -> Response {
    let Some(talos) = state.talos.as_ref() else {
        return unavailable("Talos is not configured on this instance");
    };
    let appliance = ApplianceView {
        domain: crate::state::base_domain_from_env(),
        lan_cidr: std::env::var("EXPOSURE_LOCAL_ONLY_CIDR").unwrap_or_default(),
    };
    match inventory::collect(talos, appliance, state.network.pending_view()).await {
        Ok(inventory) => match serde_json::to_value(inventory) {
            Ok(value) => json(StatusCode::OK, value),
            Err(e) => backend(format!("encoding the network inventory: {e}")),
        },
        Err(e) => unavailable(e),
    }
}

/// `POST /api/network/changes`
pub async fn change_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let request: ChangeRequest = match super::zfs::read_json(body).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    if operation_in_flight(&state).await {
        return json(
            StatusCode::CONFLICT,
            serde_json::json!({ "error": "a system upgrade operation is in flight" }),
        );
    }
    match state.network.start(request).await {
        Ok(record) => json(
            StatusCode::ACCEPTED,
            serde_json::json!({ "id": record.id, "phase": record.phase.as_str() }),
        ),
        Err(e) => start_error(e),
    }
}

/// `GET /api/network/changes/{id}`
pub async fn change_detail_get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match state.network.detail(&id) {
        Some(view) => json(
            StatusCode::OK,
            serde_json::to_value(view).unwrap_or_else(|_| serde_json::json!({})),
        ),
        None => json(
            StatusCode::NOT_FOUND,
            serde_json::json!({ "error": "unknown network change" }),
        ),
    }
}

/// `POST /api/network/changes/{id}/revert`
pub async fn change_revert_post(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match state.network.revert(&id).await {
        Ok(record) => json(
            StatusCode::ACCEPTED,
            serde_json::json!({ "id": record.id, "phase": record.phase.as_str() }),
        ),
        Err(e) => start_error(e),
    }
}

fn start_error(error: StartError) -> Response {
    let status = StatusCode::from_u16(error.status).unwrap_or(StatusCode::BAD_REQUEST);
    json(status, serde_json::json!({ "error": error.message }))
}

fn unavailable(message: impl Into<String>) -> Response {
    json(
        StatusCode::SERVICE_UNAVAILABLE,
        serde_json::json!({ "error": message.into() }),
    )
}

fn backend(message: impl Into<String>) -> Response {
    json(
        StatusCode::BAD_GATEWAY,
        serde_json::json!({ "error": message.into() }),
    )
}

/// Refuse a network change while a Talos/Kubernetes upgrade Job is running.
async fn operation_in_flight(state: &AppState) -> bool {
    let Some(kube) = state.kube.as_ref() else {
        return false;
    };
    let namespace = std::env::var("NASLOS_NAMESPACE").unwrap_or_else(|_| "naslos".to_string());
    let Ok(Some(value)) = kube
        .get_json("configmap", state.system.configmap(), &namespace)
        .await
    else {
        return false;
    };
    let Some(raw) = value.get("data").and_then(|data| data.get("state")) else {
        return false;
    };
    let Some(raw) = raw.as_str() else {
        return false;
    };
    match serde_json::from_str::<crate::upgrade::status::OperationStatus>(raw) {
        Ok(status) => !matches!(status.phase.as_str(), "" | "idle" | "succeeded" | "failed"),
        Err(_) => false,
    }
}
