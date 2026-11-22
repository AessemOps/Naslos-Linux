//! Generic webhook alert channel (FR-ALT).
//!
//! A second alert sink beside ntfy. It keeps its own persisted settings, its
//! own per-event/severity routing, and the same SSRF posture as the ntfy
//! channel (`crate::ddns::drivers::blocked_ip`): only `http`/`https`, no
//! embedded credentials, no loopback/private/link-local/metadata targets.
//!
//! Delivery is a single attempt with a 10 s timeout and redirects disabled.
//! Response bodies are drained with a fixed cap and are never surfaced in an
//! error or a log line, so a receiver cannot leak the request back to the
//! caller and a URL/query token cannot end up in an error string.

use crate::notifications::{
    severity_meets_threshold, EventType, Notification, Severity, SEVERITY_INFO, SEVERITY_WARNING,
};
use std::sync::Mutex;
use std::time::Duration;

/// Upper bound on bytes read (and discarded) from a webhook response.
pub const MAX_RESPONSE_BODY: usize = 8 * 1024;

/// The event name used by the manual test action. It is not routable.
pub const EVENT_TEST: &str = "test";

/// The webhook channel configuration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Settings {
    pub enabled: bool,
    #[serde(default)]
    pub url: String,
    #[serde(rename = "authToken", default)]
    pub auth_token: String,
    #[serde(rename = "enabledEvents", default)]
    pub enabled_events: Vec<EventType>,
    #[serde(rename = "minSeverity", default)]
    pub min_severity: Severity,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            url: String::new(),
            auth_token: String::new(),
            enabled_events: vec![
                crate::notifications::EVENT_ZFS_HEALTH.to_string(),
                crate::notifications::EVENT_DISK_FAILURE.to_string(),
            ],
            min_severity: SEVERITY_WARNING.to_string(),
        }
    }
}

impl Settings {
    /// Whether the channel should deliver this event at this severity.
    pub fn allows(&self, event: &str, severity: &str) -> bool {
        self.enabled
            && self.enabled_events.iter().any(|e| e == event)
            && severity_meets_threshold(severity, &self.min_severity)
    }
}

/// The JSON body POSTed to the configured webhook.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Payload {
    pub source: &'static str,
    pub event: String,
    pub title: String,
    pub message: String,
    pub severity: String,
    pub tags: Vec<String>,
}

/// Render the wire payload. It carries no credentials or channel settings.
pub fn render_payload(event: &str, notif: &Notification) -> Payload {
    Payload {
        source: "naslos",
        event: event.to_string(),
        title: notif.title.clone(),
        message: notif.message.clone(),
        severity: notif.severity.clone(),
        tags: notif.tags.clone(),
    }
}

/// Validate an operator-supplied webhook target.
///
/// The same guard as the ntfy channel: `http`/`https` only, a host that
/// resolves, and no loopback/private/link-local/metadata address. Credentials
/// in the URL are refused so they cannot leak through an error path.
pub fn validate_webhook_url(raw: &str) -> Result<reqwest::Url, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("webhook URL is empty".to_string());
    }
    let parsed = reqwest::Url::parse(trimmed).map_err(|_| "invalid webhook URL".to_string())?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(format!(
            "webhook URL must use http or https, got {:?}",
            parsed.scheme()
        ));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("webhook URL must not embed credentials".to_string());
    }
    let host = parsed
        .host_str()
        .ok_or("webhook URL has no host")?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    let port = parsed.port_or_known_default().unwrap_or(443);
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        if crate::ddns::drivers::blocked_ip(&ip) {
            return Err("refusing to send alerts to a non-public address".to_string());
        }
        return Ok(parsed);
    }
    use std::net::ToSocketAddrs;
    let addrs = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|_| "cannot resolve the webhook host".to_string())?;
    let mut any = false;
    for addr in addrs {
        any = true;
        if crate::ddns::drivers::blocked_ip(&addr.ip()) {
            return Err(
                "refusing to send alerts to a target that resolves to a non-public address"
                    .to_string(),
            );
        }
    }
    if !any {
        return Err("the webhook host did not resolve".to_string());
    }
    Ok(parsed)
}

/// Validate settings before either saving them or reporting a client error.
/// An empty minimum severity is accepted (it defaults to `warning` on save).
pub fn validate_settings(settings: &Settings) -> Result<(), String> {
    if !settings.min_severity.trim().is_empty()
        && !crate::notifications::valid_severity(settings.min_severity.trim())
    {
        return Err(format!(
            "unknown minimum severity {:?}",
            settings.min_severity
        ));
    }
    let url = settings.url.trim();
    if url.is_empty() {
        if settings.enabled {
            return Err("webhook URL is required when the channel is enabled".to_string());
        }
        return Ok(());
    }
    validate_webhook_url(url).map(|_| ())
}

/// Manages the persisted webhook channel.
pub struct Manager {
    settings: Mutex<Settings>,
    client: reqwest::Client,
    config_path: String,
}

impl Manager {
    pub fn new(config_path: &str) -> Self {
        let mut m = Self {
            settings: Mutex::new(Settings::default()),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap_or_default(),
            config_path: config_path.to_string(),
        };
        m.load();
        m
    }

    pub fn from_env() -> Self {
        Self::new(
            &std::env::var("ALERT_WEBHOOK_CONFIG")
                .unwrap_or_else(|_| "/var/lib/naslos/webhook.json".to_string()),
        )
    }

    fn load(&mut self) {
        if self.config_path.is_empty() {
            return;
        }
        let Ok(data) = std::fs::read(&self.config_path) else {
            return;
        };
        if let Ok(settings) = serde_json::from_slice::<Settings>(&data) {
            *self.settings.lock().unwrap() = settings;
        }
    }

    fn save(&self, settings: &Settings) -> Result<(), String> {
        if self.config_path.is_empty() {
            return Ok(());
        }
        let data =
            serde_json::to_vec_pretty(settings).map_err(|e| format!("marshaling settings: {e}"))?;
        // 0600: this file holds the optional bearer token.
        crate::chartsrepo::write_atomic(&self.config_path, &data, 0o600, 0o750)
    }

    pub fn get_settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    /// Update settings. `auth_token` follows the ntfy convention: `None` keeps
    /// the stored token, `Some("")` clears it, `Some(v)` replaces it.
    ///
    /// A non-empty URL is validated here, before it is persisted. A rejected
    /// URL leaves the previous settings untouched.
    pub fn update_settings(
        &self,
        mut settings: Settings,
        auth_token: Option<&str>,
    ) -> Result<(), String> {
        settings.url = settings.url.trim().to_string();
        if settings.min_severity.trim().is_empty() {
            settings.min_severity = SEVERITY_WARNING.to_string();
        }
        validate_settings(&settings)?;
        let mut guard = self.settings.lock().unwrap();
        settings.auth_token = match auth_token {
            Some(v) => v.to_string(),
            None => guard.auth_token.clone(),
        };
        self.save(&settings)?;
        *guard = settings;
        Ok(())
    }

    /// Reset the channel to its defaults (disabled, no URL, no token).
    pub fn clear(&self) -> Result<(), String> {
        let settings = Settings::default();
        self.save(&settings)?;
        *self.settings.lock().unwrap() = settings;
        Ok(())
    }

    /// Deliver one event to the webhook, respecting the channel's routing and
    /// severity settings. A dropped event is `Ok(())`, like the ntfy channel.
    pub async fn send(&self, event: &str, notif: &Notification) -> Result<(), String> {
        let settings = self.get_settings();
        if !settings.allows(event, &notif.severity) {
            return Ok(());
        }
        self.post(&settings, event, notif).await
    }

    /// Send a test payload. This requires the channel to be enabled but
    /// bypasses event routing and the severity threshold, so an operator can
    /// verify the target before subscribing events.
    pub async fn send_test(&self) -> Result<(), String> {
        let settings = self.get_settings();
        if !settings.enabled {
            return Err("webhook channel is disabled".to_string());
        }
        self.post(
            &settings,
            EVENT_TEST,
            &Notification {
                title: "Naslos Test".to_string(),
                message: "This is a test alert from Naslos.".to_string(),
                severity: SEVERITY_INFO.to_string(),
                tags: vec!["white_check_mark".to_string(), "naslos".to_string()],
            },
        )
        .await
    }

    async fn post(
        &self,
        settings: &Settings,
        event: &str,
        notif: &Notification,
    ) -> Result<(), String> {
        let target = validate_webhook_url(&settings.url)?;
        let client = self.pinned_client(&target)?;
        let body = serde_json::to_vec(&render_payload(event, notif))
            .map_err(|e| format!("encoding the webhook payload: {e}"))?;
        let mut rb = client
            .post(target)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::USER_AGENT, "naslos-alerts")
            .body(body);
        if !settings.auth_token.is_empty() {
            rb = rb.bearer_auth(&settings.auth_token);
        }
        let resp = rb.send().await.map_err(|e| transport_error(&e))?;
        let status = resp.status();
        drain_bounded(resp).await;
        // Redirects are disabled, so a 3xx is a failed delivery (usually a
        // misconfigured URL), not a success. Only a 2xx accepted the payload.
        if !status.is_success() {
            return Err(format!("webhook returned status {}", status.as_u16()));
        }
        Ok(())
    }

    /// A client pinned to the address that validation vetted, so DNS cannot be
    /// rebound between the check and the request.
    fn pinned_client(&self, target: &reqwest::Url) -> Result<reqwest::Client, String> {
        let host = target
            .host_str()
            .ok_or("webhook URL has no host")?
            .trim_start_matches('[')
            .trim_end_matches(']');
        let port = target.port_or_known_default().unwrap_or(443);
        if host.parse::<std::net::IpAddr>().is_ok() {
            return Ok(self.client.clone());
        }
        use std::net::ToSocketAddrs;
        let addrs: Vec<std::net::SocketAddr> = (host, port)
            .to_socket_addrs()
            .map_err(|_| "cannot resolve the webhook host".to_string())?
            .collect();
        let Some(pinned) = addrs.first().copied() else {
            return Err("the webhook host did not resolve".to_string());
        };
        if addrs
            .iter()
            .any(|addr| crate::ddns::drivers::blocked_ip(&addr.ip()))
        {
            return Err(
                "refusing to send alerts to a target that resolves to a non-public address"
                    .to_string(),
            );
        }
        reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .resolve(host, pinned)
            .build()
            .map_err(|_| "building the webhook client failed".to_string())
    }
}

/// A transport failure without the request URL in it (a URL can carry a
/// token in its path or query).
fn transport_error(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "the webhook request timed out".to_string()
    } else if e.is_connect() {
        "could not connect to the webhook target".to_string()
    } else if e.is_redirect() {
        "the webhook target redirected (redirects are disabled)".to_string()
    } else {
        "the webhook request failed".to_string()
    }
}

/// Read at most `MAX_RESPONSE_BODY` bytes and discard them. The body is never
/// logged or returned to a caller.
async fn drain_bounded(mut resp: reqwest::Response) {
    let mut total = 0usize;
    while let Ok(Some(chunk)) = resp.chunk().await {
        total = total.saturating_add(chunk.len());
        if total >= MAX_RESPONSE_BODY {
            break;
        }
    }
}

/// Per-channel delivery results, kept separate so one channel's failure is
/// observable without suppressing the others.
#[derive(Debug)]
pub struct DeliveryOutcome {
    pub ntfy: Result<(), String>,
    pub webhook: Result<(), String>,
    pub smtp: Result<(), String>,
    pub syslog: Result<(), String>,
}

/// Deliver one event to every alert channel. Each channel applies its own
/// routing and severity settings; a failure in one never stops the others.
pub async fn deliver(
    ntfy: &crate::notifications::Manager,
    webhook: &Manager,
    smtp: &crate::smtp::Manager,
    syslog: &crate::syslog::Manager,
    event: &str,
    notif: &Notification,
) -> DeliveryOutcome {
    // Run every sink concurrently: a slow or unreachable endpoint must not
    // delay another channel.
    let ntfy_fut = async {
        if ntfy.event_enabled(event) {
            ntfy.send(notif).await
        } else {
            Ok(())
        }
    };
    let (ntfy_result, webhook_result, smtp_result, syslog_result) = tokio::join!(
        ntfy_fut,
        webhook.send(event, notif),
        smtp.send(event, notif),
        syslog.send(event, notif),
    );
    DeliveryOutcome {
        ntfy: ntfy_result,
        webhook: webhook_result,
        smtp: smtp_result,
        syslog: syslog_result,
    }
}

/// `deliver` plus a warning per failed channel. Emitters call this so one
/// unreachable sink cannot break event handling.
pub async fn deliver_logged(
    ntfy: &crate::notifications::Manager,
    webhook: &Manager,
    smtp: &crate::smtp::Manager,
    syslog: &crate::syslog::Manager,
    event: &str,
    notif: &Notification,
) {
    let outcome = deliver(ntfy, webhook, smtp, syslog, event, notif).await;
    if let Err(e) = outcome.ntfy {
        tracing::warn!("ntfy notification failed: {e}");
    }
    if let Err(e) = outcome.webhook {
        tracing::warn!("webhook notification failed: {e}");
    }
    if let Err(e) = outcome.smtp {
        tracing::warn!("SMTP notification failed: {e}");
    }
    if let Err(e) = outcome.syslog {
        tracing::warn!("syslog notification failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notifications::{
        Notification, EVENT_DISK_FAILURE, EVENT_ZFS_HEALTH, SEVERITY_CRITICAL, SEVERITY_ERROR,
        SEVERITY_INFO, SEVERITY_WARNING,
    };

    fn notif() -> Notification {
        Notification {
            title: "Disk sda is at 60°C".to_string(),
            message: "Disk /dev/sda is above the 55°C warning threshold.".to_string(),
            severity: SEVERITY_ERROR.to_string(),
            tags: vec!["floppy_disk".to_string()],
        }
    }

    #[test]
    fn url_validation_accepts_public_http_targets() {
        assert!(validate_webhook_url("https://1.1.1.1/hook").is_ok());
        assert!(validate_webhook_url("http://1.1.1.1:8080/alerts/naslos").is_ok());
        // A literal address needs no DNS, so this stays offline.
        assert!(validate_webhook_url(" https://1.1.1.1/hook ").is_ok());
    }

    #[test]
    fn url_validation_refuses_bad_schemes_and_credentials() {
        assert!(validate_webhook_url("").is_err());
        assert!(validate_webhook_url("ftp://1.1.1.1/hook").is_err());
        assert!(validate_webhook_url("file:///etc/passwd").is_err());
        assert!(validate_webhook_url("https://user:pass@1.1.1.1/hook").is_err());
        assert!(validate_webhook_url("https://user@1.1.1.1/hook").is_err());
        assert!(validate_webhook_url("not a url").is_err());
    }

    #[test]
    fn url_validation_refuses_non_public_targets() {
        for target in [
            "http://127.0.0.1/hook",
            "http://127.0.0.2:8080/hook",
            "http://169.254.169.254/latest/meta-data",
            "http://10.0.0.5/hook",
            "http://172.16.4.2/hook",
            "http://192.168.1.10/hook",
            "http://100.64.0.1/hook",
            "http://0.0.0.0/hook",
            "http://198.18.0.1/hook",
            "http://[::1]/hook",
            "http://[fd00::1]/hook",
            "http://[fe80::1]/hook",
            "http://[::ffff:127.0.0.1]/hook",
        ] {
            assert!(
                validate_webhook_url(target).is_err(),
                "{target} must be refused"
            );
        }
    }

    #[test]
    fn routing_honours_events_and_severity() {
        let mut settings = Settings {
            enabled: true,
            url: "https://1.1.1.1/hook".to_string(),
            enabled_events: vec![EVENT_DISK_FAILURE.to_string()],
            min_severity: SEVERITY_ERROR.to_string(),
            ..Default::default()
        };
        assert!(settings.allows(EVENT_DISK_FAILURE, SEVERITY_ERROR));
        assert!(settings.allows(EVENT_DISK_FAILURE, SEVERITY_CRITICAL));
        assert!(!settings.allows(EVENT_DISK_FAILURE, SEVERITY_WARNING));
        assert!(!settings.allows(EVENT_ZFS_HEALTH, SEVERITY_ERROR));
        settings.enabled = false;
        assert!(!settings.allows(EVENT_DISK_FAILURE, SEVERITY_CRITICAL));
    }

    #[test]
    fn payload_shape_is_stable_and_carries_no_settings() {
        let payload = render_payload(EVENT_DISK_FAILURE, &notif());
        assert_eq!(payload.source, "naslos");
        assert_eq!(payload.event, EVENT_DISK_FAILURE);
        assert_eq!(payload.severity, SEVERITY_ERROR);
        assert_eq!(payload.title, "Disk sda is at 60°C");
        assert_eq!(payload.tags, vec!["floppy_disk".to_string()]);

        let value = serde_json::to_value(&payload).unwrap();
        for key in ["source", "event", "title", "message", "severity", "tags"] {
            assert!(value.get(key).is_some(), "payload missing {key}");
        }
        assert!(value.get("authToken").is_none());
        assert!(value.get("url").is_none());
    }

    fn temp_manager(settings: Option<&Settings>) -> (tempfile::TempDir, Manager) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("webhook.json");
        if let Some(settings) = settings {
            std::fs::write(&path, serde_json::to_vec(settings).unwrap()).unwrap();
        }
        let manager = Manager::new(path.to_str().unwrap());
        (dir, manager)
    }

    #[test]
    fn settings_survive_a_reload_and_the_token_stays_write_only() {
        let (dir, manager) = temp_manager(None);
        let settings = Settings {
            enabled: true,
            url: "https://1.1.1.1/hook".to_string(),
            ..Default::default()
        };
        manager
            .update_settings(settings, Some("s3cret-token"))
            .unwrap();

        let path = dir.path().join("webhook.json");
        let reloaded = Manager::new(path.to_str().unwrap());
        let stored = reloaded.get_settings();
        assert!(stored.enabled);
        assert_eq!(stored.url, "https://1.1.1.1/hook");
        assert_eq!(stored.auth_token, "s3cret-token");

        // `None` keeps the token, `Some("")` clears it.
        reloaded.update_settings(stored.clone(), None).unwrap();
        assert_eq!(reloaded.get_settings().auth_token, "s3cret-token");
        reloaded.update_settings(stored, Some("")).unwrap();
        assert_eq!(reloaded.get_settings().auth_token, "");
    }

    #[test]
    fn update_settings_rejects_invalid_urls_and_keeps_the_previous_ones() {
        let (_dir, manager) = temp_manager(None);
        let good = Settings {
            enabled: true,
            url: "https://1.1.1.1/hook".to_string(),
            ..Default::default()
        };
        manager
            .update_settings(good.clone(), Some("token"))
            .unwrap();

        let bad = Settings {
            url: "http://127.0.0.1/hook".to_string(),
            ..good.clone()
        };
        assert!(manager.update_settings(bad, None).is_err());
        assert_eq!(manager.get_settings().url, "https://1.1.1.1/hook");

        let missing = Settings {
            enabled: true,
            url: String::new(),
            ..good
        };
        assert!(manager.update_settings(missing, None).is_err());

        let unknown_severity = Settings {
            enabled: false,
            url: String::new(),
            min_severity: "loud".to_string(),
            ..Default::default()
        };
        assert!(manager.update_settings(unknown_severity, None).is_err());
    }

    #[test]
    fn clear_resets_the_channel() {
        let (_dir, manager) = temp_manager(None);
        manager
            .update_settings(
                Settings {
                    enabled: true,
                    url: "https://1.1.1.1/hook".to_string(),
                    ..Default::default()
                },
                Some("token"),
            )
            .unwrap();
        manager.clear().unwrap();
        let settings = manager.get_settings();
        assert!(!settings.enabled);
        assert!(settings.url.is_empty());
        assert!(settings.auth_token.is_empty());
    }

    #[tokio::test]
    async fn a_failing_channel_does_not_suppress_the_other() {
        // The webhook points at a blocked address, so delivery fails before any
        // socket is opened. The ntfy manager is disabled, so its result is Ok.
        let blocked = Settings {
            enabled: true,
            url: "http://127.0.0.1/hook".to_string(),
            auth_token: "do-not-leak-this".to_string(),
            enabled_events: vec![EVENT_DISK_FAILURE.to_string()],
            min_severity: SEVERITY_WARNING.to_string(),
        };
        let (_dir, webhook) = temp_manager(Some(&blocked));
        let ntfy = crate::notifications::Manager::new("");
        let smtp = crate::smtp::Manager::new("");
        let syslog = crate::syslog::Manager::new("");

        let outcome = deliver(
            &ntfy,
            &webhook,
            &smtp,
            &syslog,
            EVENT_DISK_FAILURE,
            &notif(),
        )
        .await;
        assert!(outcome.ntfy.is_ok());
        assert!(outcome.smtp.is_ok());
        assert!(outcome.syslog.is_ok());
        let error = outcome.webhook.unwrap_err();
        assert!(error.contains("non-public"));
        assert!(!error.contains("do-not-leak-this"));
        assert!(!error.contains("127.0.0.1"));
    }

    #[tokio::test]
    async fn disabled_or_unrouted_events_are_dropped_without_network() {
        let mut settings = Settings {
            enabled: true,
            url: "https://1.1.1.1/hook".to_string(),
            enabled_events: vec![EVENT_ZFS_HEALTH.to_string()],
            min_severity: SEVERITY_INFO.to_string(),
            ..Default::default()
        };
        let (_dir, webhook) = temp_manager(Some(&settings));
        // A routed-out event returns before the URL is even resolved.
        assert!(webhook.send(EVENT_DISK_FAILURE, &notif()).await.is_ok());

        settings.enabled = false;
        let (_dir, webhook) = temp_manager(Some(&settings));
        assert!(webhook.send(EVENT_ZFS_HEALTH, &notif()).await.is_ok());
    }

    #[tokio::test]
    async fn test_action_requires_an_enabled_channel() {
        let (_dir, webhook) = temp_manager(None);
        let error = webhook.send_test().await.unwrap_err();
        assert!(error.contains("disabled"));
    }

    #[tokio::test]
    async fn test_action_is_not_gated_by_event_routing() {
        // Enabled, with an event list that does not include "test", and a
        // severity threshold above info. The test still attempts delivery;
        // the blocked URL makes it fail immediately without a socket.
        let settings = Settings {
            enabled: true,
            url: "http://127.0.0.1/hook".to_string(),
            enabled_events: vec![EVENT_DISK_FAILURE.to_string()],
            min_severity: SEVERITY_CRITICAL.to_string(),
            ..Default::default()
        };
        let (_dir, webhook) = temp_manager(Some(&settings));
        let error = webhook.send_test().await.unwrap_err();
        assert!(error.contains("non-public"));
    }
}
