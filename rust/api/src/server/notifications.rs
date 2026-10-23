//! Notification handlers (port of `api/internal/server/notifications.go`).

use crate::json;
use crate::notifications::Settings;
use crate::state::AppState;
use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use std::sync::Arc;

/// `GET|PUT /api/notifications`
pub async fn notifications_get(State(state): State<Arc<AppState>>) -> Response {
    let s = state.notifications.get_settings();
    json(
        StatusCode::OK,
        serde_json::json!({
            "enabled": s.enabled,
            "serverUrl": s.server_url,
            "topic": s.topic,
            "hasAuthToken": !s.auth_token.is_empty(),
            "email": s.email,
            "enabledEvents": s.enabled_events,
            "minSeverity": s.min_severity,
        }),
    )
}

#[derive(Default, serde::Deserialize)]
struct SettingsBody {
    #[serde(default)]
    enabled: bool,
    #[serde(default, rename = "serverUrl")]
    server_url: String,
    #[serde(default)]
    topic: String,
    #[serde(default, rename = "authToken")]
    auth_token: Option<String>,
    #[serde(default)]
    email: String,
    #[serde(default, rename = "enabledEvents")]
    enabled_events: Vec<String>,
    #[serde(default, rename = "minSeverity")]
    min_severity: String,
}

pub async fn notifications_put(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let req: SettingsBody = match super::zfs::read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let settings = Settings {
        enabled: req.enabled,
        server_url: req.server_url,
        topic: req.topic,
        auth_token: String::new(),
        email: req.email,
        enabled_events: req.enabled_events,
        min_severity: req.min_severity,
    };
    match state
        .notifications
        .update_settings(settings, req.auth_token.as_deref())
    {
        Ok(()) => json(
            StatusCode::OK,
            serde_json::json!({ "status": "notification settings updated" }),
        ),
        Err(e) => json(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "error": e }),
        ),
    }
}

/// `POST /api/notifications/test`
pub async fn notification_test_post(State(state): State<Arc<AppState>>) -> Response {
    match state.notifications.send_test().await {
        Ok(()) => json(
            StatusCode::OK,
            serde_json::json!({ "status": "test notification sent" }),
        ),
        Err(e) => json(StatusCode::BAD_GATEWAY, serde_json::json!({ "error": e })),
    }
}
