//! `/api/metrics` and `/api/dashboard` (port of `api/internal/server/metrics.go`).

use crate::json;
use crate::state::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use std::sync::Arc;

/// `GET /api/metrics`
pub async fn metrics(State(state): State<Arc<AppState>>) -> Response {
    let data = state.snapshot();
    let mut value = serde_json::to_value(data).unwrap();
    // Match Go's zero time.Time (`0001-01-01T00:00:00Z`) when un-collected.
    value["updatedAt"] = state.updated_at_json();
    json(StatusCode::OK, value)
}

/// `GET /api/dashboard`
pub async fn dashboard(State(state): State<Arc<AppState>>) -> Response {
    json(StatusCode::OK, state.dashboard_data())
}
