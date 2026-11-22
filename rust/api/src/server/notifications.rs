//! Notification handlers (port of `api/internal/server/notifications.go`).

use crate::json;
use crate::notifications::Settings;
use crate::state::AppState;
use crate::webhook;
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

/// `GET /api/notifications/webhook`
pub async fn webhook_get(State(state): State<Arc<AppState>>) -> Response {
    let s = state.webhook.get_settings();
    json(
        StatusCode::OK,
        serde_json::json!({
            "enabled": s.enabled,
            "url": s.url,
            "hasAuthToken": !s.auth_token.is_empty(),
            "enabledEvents": s.enabled_events,
            "minSeverity": s.min_severity,
        }),
    )
}

#[derive(Default, serde::Deserialize)]
struct WebhookBody {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    url: String,
    #[serde(default, rename = "authToken")]
    auth_token: Option<String>,
    #[serde(default, rename = "enabledEvents")]
    enabled_events: Vec<String>,
    #[serde(default, rename = "minSeverity")]
    min_severity: String,
}

/// `PUT /api/notifications/webhook`
pub async fn webhook_put(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let req: WebhookBody = match super::zfs::read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let settings = webhook::Settings {
        enabled: req.enabled,
        url: req.url,
        auth_token: String::new(),
        enabled_events: req.enabled_events,
        min_severity: req.min_severity,
    };
    // Invalid URL/severity is a client error; a failed write is not.
    if let Err(e) = webhook::validate_settings(&settings) {
        return json(StatusCode::BAD_REQUEST, serde_json::json!({ "error": e }));
    }
    match state
        .webhook
        .update_settings(settings, req.auth_token.as_deref())
    {
        Ok(()) => json(
            StatusCode::OK,
            serde_json::json!({ "status": "webhook settings updated" }),
        ),
        Err(e) => json(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "error": e }),
        ),
    }
}

/// `DELETE /api/notifications/webhook`
pub async fn webhook_delete(State(state): State<Arc<AppState>>) -> Response {
    match state.webhook.clear() {
        Ok(()) => json(
            StatusCode::OK,
            serde_json::json!({ "status": "webhook channel removed" }),
        ),
        Err(e) => json(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "error": e }),
        ),
    }
}

/// `POST /api/notifications/webhook/test`
pub async fn webhook_test_post(State(state): State<Arc<AppState>>) -> Response {
    if !state.webhook.get_settings().enabled {
        return json(
            StatusCode::BAD_REQUEST,
            serde_json::json!({ "error": "webhook channel is disabled" }),
        );
    }
    match state.webhook.send_test().await {
        Ok(()) => json(
            StatusCode::OK,
            serde_json::json!({ "status": "test webhook sent" }),
        ),
        Err(e) => json(StatusCode::BAD_GATEWAY, serde_json::json!({ "error": e })),
    }
}
