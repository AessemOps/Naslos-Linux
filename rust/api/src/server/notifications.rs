//! Notification handlers (port of `api/internal/server/notifications.go`).

use crate::json;
use crate::notifications::Settings;
use crate::smtp;
use crate::state::AppState;
use crate::syslog;
use crate::webhook;
use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use std::sync::Arc;

/// `GET|PUT /api/notifications`
pub async fn notifications_get(State(state): State<Arc<AppState>>) -> Response {
    let s = state.notifications.get_settings();
    json(StatusCode::OK, crate::notifications::public_view(&s))
}

#[derive(Default, serde::Deserialize)]
struct SettingsBody {
    #[serde(default)]
    enabled: bool,
    #[serde(default, rename = "serverUrl")]
    server_url: Option<String>,
    #[serde(default)]
    topic: Option<String>,
    #[serde(default, rename = "authToken")]
    auth_token: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default, rename = "enabledEvents")]
    enabled_events: Option<Vec<String>>,
    #[serde(default, rename = "minSeverity")]
    min_severity: String,
    #[serde(default, rename = "allowInsecureHttp")]
    allow_insecure_http: Option<bool>,
}

pub async fn notifications_put(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let req: SettingsBody = match super::zfs::read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let existing = state.notifications.get_settings();
    let settings = Settings {
        enabled: req.enabled,
        server_url: req.server_url.unwrap_or(existing.server_url),
        topic: req.topic.unwrap_or(existing.topic),
        auth_token: String::new(),
        email: req.email.unwrap_or(existing.email),
        enabled_events: req.enabled_events.unwrap_or(existing.enabled_events),
        min_severity: req.min_severity,
        allow_insecure_http: req
            .allow_insecure_http
            .unwrap_or(existing.allow_insecure_http),
    };
    // Validate against the effective settings (including a kept token), so a
    // plaintext URL without an acknowledgement is a client error.
    let mut effective = settings.clone();
    effective.auth_token = req.auth_token.clone().unwrap_or(existing.auth_token);
    if let Err(e) = crate::notifications::validate_settings(&effective) {
        return json(StatusCode::BAD_REQUEST, serde_json::json!({ "error": e }));
    }
    match state
        .notifications
        .update_settings(settings, req.auth_token.as_deref())
    {
        Ok(()) => {
            let saved = state.notifications.get_settings();
            audit_insecure_ack(&state, "ntfy", &saved.server_url, saved.allow_insecure_http).await;
            json(
                StatusCode::OK,
                serde_json::json!({ "status": "notification settings updated" }),
            )
        }
        Err(e) => json(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "error": e }),
        ),
    }
}

/// Record an operator's explicit plaintext-HTTP acknowledgement in the audit
/// log. Settings values (URL, token) are never included. The fsync-ing append
/// runs on the blocking pool so it cannot stall a Tokio worker (FR-AUD-08).
async fn audit_insecure_ack(state: &AppState, channel: &str, url: &str, acknowledged: bool) {
    let plaintext = url.trim().to_ascii_lowercase().starts_with("http://");
    if !plaintext || !acknowledged {
        return;
    }
    let pending = crate::audit::Pending::system(
        "notifications",
        "insecure_http_acknowledged",
        Some(channel.to_string()),
        serde_json::json!({ "channel": channel, "transport": "http" }),
    );
    if let Err(e) = state.audit.record_once_async(&pending, 200).await {
        tracing::warn!("could not audit the {channel} plaintext-HTTP acknowledgement: {e}");
    }
}

/// `POST /api/notifications/test`
pub async fn notification_test_post(State(state): State<Arc<AppState>>) -> Response {
    if !state.notifications.get_settings().enabled {
        return json(
            StatusCode::BAD_REQUEST,
            serde_json::json!({ "error": "ntfy channel is disabled" }),
        );
    }
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
    json(StatusCode::OK, webhook::public_view(&s))
}

#[derive(Default, serde::Deserialize)]
struct WebhookBody {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    url: Option<String>,
    #[serde(default, rename = "authToken")]
    auth_token: Option<String>,
    #[serde(default, rename = "enabledEvents")]
    enabled_events: Option<Vec<String>>,
    #[serde(default, rename = "minSeverity")]
    min_severity: String,
    #[serde(default, rename = "allowInsecureHttp")]
    allow_insecure_http: Option<bool>,
}

/// `PUT /api/notifications/webhook`
pub async fn webhook_put(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let req: WebhookBody = match super::zfs::read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let existing = state.webhook.get_settings();
    let settings = webhook::Settings {
        enabled: req.enabled,
        url: req.url.unwrap_or(existing.url),
        auth_token: String::new(),
        enabled_events: req.enabled_events.unwrap_or(existing.enabled_events),
        min_severity: req.min_severity,
        allow_insecure_http: req
            .allow_insecure_http
            .unwrap_or(existing.allow_insecure_http),
    };
    // Validate against the effective settings (including a kept token).
    let mut effective = settings.clone();
    effective.auth_token = req.auth_token.clone().unwrap_or(existing.auth_token);
    if let Err(e) = webhook::validate_settings(&effective) {
        return json(StatusCode::BAD_REQUEST, serde_json::json!({ "error": e }));
    }
    match state
        .webhook
        .update_settings(settings, req.auth_token.as_deref())
    {
        Ok(()) => {
            let saved = state.webhook.get_settings();
            audit_insecure_ack(&state, "webhook", &saved.url, saved.allow_insecure_http).await;
            json(
                StatusCode::OK,
                serde_json::json!({ "status": "webhook settings updated" }),
            )
        }
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

/// `GET /api/notifications/smtp`
pub async fn smtp_get(State(state): State<Arc<AppState>>) -> Response {
    let s = state.smtp.get_settings();
    json(
        StatusCode::OK,
        serde_json::json!({
            "enabled": s.enabled,
            "host": s.host,
            "port": s.port,
            "tls": s.tls,
            "username": s.username,
            "hasPassword": !s.password.is_empty(),
            "from": s.from,
            "to": s.to,
            "enabledEvents": s.enabled_events,
            "minSeverity": s.min_severity,
        }),
    )
}

#[derive(Default, serde::Deserialize)]
struct SmtpBody {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    host: String,
    #[serde(default)]
    port: u16,
    #[serde(default)]
    tls: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    from: String,
    #[serde(default)]
    to: Vec<String>,
    #[serde(default, rename = "enabledEvents")]
    enabled_events: Vec<String>,
    #[serde(default, rename = "minSeverity")]
    min_severity: String,
}

/// `PUT /api/notifications/smtp`
pub async fn smtp_put(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let req: SmtpBody = match super::zfs::read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let settings = smtp::Settings {
        enabled: req.enabled,
        host: req.host,
        port: req.port,
        tls: req.tls,
        username: req.username,
        password: String::new(),
        from: req.from,
        to: req.to,
        enabled_events: req.enabled_events,
        min_severity: req.min_severity,
    };
    // A bad target/TLS/credential combination or severity is a client error;
    // a failed write is not.
    if let Err(e) = state
        .smtp
        .validate_update(&settings, req.password.as_deref())
    {
        return json(StatusCode::BAD_REQUEST, serde_json::json!({ "error": e }));
    }
    match state
        .smtp
        .update_settings(settings, req.password.as_deref())
    {
        Ok(()) => json(
            StatusCode::OK,
            serde_json::json!({ "status": "smtp settings updated" }),
        ),
        Err(e) => json(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "error": e }),
        ),
    }
}

/// `DELETE /api/notifications/smtp`
pub async fn smtp_delete(State(state): State<Arc<AppState>>) -> Response {
    match state.smtp.clear() {
        Ok(()) => json(
            StatusCode::OK,
            serde_json::json!({ "status": "smtp channel removed" }),
        ),
        Err(e) => json(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "error": e }),
        ),
    }
}

/// `POST /api/notifications/smtp/test`
pub async fn smtp_test_post(State(state): State<Arc<AppState>>) -> Response {
    if !state.smtp.get_settings().enabled {
        return json(
            StatusCode::BAD_REQUEST,
            serde_json::json!({ "error": "smtp channel is disabled" }),
        );
    }
    match state.smtp.send_test().await {
        Ok(()) => json(
            StatusCode::OK,
            serde_json::json!({ "status": "test email sent" }),
        ),
        Err(e) => json(StatusCode::BAD_GATEWAY, serde_json::json!({ "error": e })),
    }
}

/// `GET /api/notifications/syslog`
pub async fn syslog_get(State(state): State<Arc<AppState>>) -> Response {
    let s = state.syslog.get_settings();
    json(
        StatusCode::OK,
        serde_json::json!({
            "enabled": s.enabled,
            "protocol": s.protocol,
            "host": s.host,
            "port": s.port,
            "facility": s.facility,
            "enabledEvents": s.enabled_events,
            "minSeverity": s.min_severity,
        }),
    )
}

#[derive(Default, serde::Deserialize)]
struct SyslogBody {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    protocol: String,
    #[serde(default)]
    host: String,
    #[serde(default)]
    port: u16,
    #[serde(default)]
    facility: String,
    #[serde(default, rename = "enabledEvents")]
    enabled_events: Vec<String>,
    #[serde(default, rename = "minSeverity")]
    min_severity: String,
}

/// `PUT /api/notifications/syslog`
pub async fn syslog_put(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let req: SyslogBody = match super::zfs::read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let mut settings = syslog::Settings {
        enabled: req.enabled,
        protocol: req.protocol,
        host: req.host,
        port: req.port,
        facility: req.facility,
        enabled_events: req.enabled_events,
        min_severity: req.min_severity,
    };
    let validated = syslog::normalize_settings(&mut settings)
        .and_then(|()| syslog::validate_settings(&settings));
    if let Err(e) = validated {
        return json(StatusCode::BAD_REQUEST, serde_json::json!({ "error": e }));
    }
    match state.syslog.update_settings(settings) {
        Ok(()) => json(
            StatusCode::OK,
            serde_json::json!({ "status": "syslog settings updated" }),
        ),
        Err(e) => json(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "error": e }),
        ),
    }
}

/// `DELETE /api/notifications/syslog`
pub async fn syslog_delete(State(state): State<Arc<AppState>>) -> Response {
    match state.syslog.clear() {
        Ok(()) => json(
            StatusCode::OK,
            serde_json::json!({ "status": "syslog channel removed" }),
        ),
        Err(e) => json(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "error": e }),
        ),
    }
}

/// `POST /api/notifications/syslog/test`
pub async fn syslog_test_post(State(state): State<Arc<AppState>>) -> Response {
    if !state.syslog.get_settings().enabled {
        return json(
            StatusCode::BAD_REQUEST,
            serde_json::json!({ "error": "syslog channel is disabled" }),
        );
    }
    match state.syslog.send_test().await {
        Ok(()) => json(
            StatusCode::OK,
            serde_json::json!({ "status": "test syslog message sent" }),
        ),
        Err(e) => json(StatusCode::BAD_GATEWAY, serde_json::json!({ "error": e })),
    }
}
