//! ntfy push notifications (port of `api/internal/notifications`).

use std::sync::Mutex;
use std::time::Duration;

/// The type of event that triggers a notification.
pub type EventType = String;
pub const EVENT_ZFS_HEALTH: &str = "zfs_health";
pub const EVENT_DISK_FAILURE: &str = "disk_failure";
pub const EVENT_BACKUP_FAILURE: &str = "backup_failure";
pub const EVENT_BACKUP_SUCCESS: &str = "backup_success";
pub const EVENT_AUDIT: &str = "audit";
/// The event name used by a channel's manual test action. It is not routable.
pub const EVENT_TEST: &str = "test";

/// The notification severity level.
pub type Severity = String;
pub const SEVERITY_INFO: &str = "info";
pub const SEVERITY_WARNING: &str = "warning";
pub const SEVERITY_ERROR: &str = "error";
pub const SEVERITY_CRITICAL: &str = "critical";

/// The ntfy notification configuration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Settings {
    pub enabled: bool,
    #[serde(rename = "serverUrl", default)]
    pub server_url: String,
    #[serde(default)]
    pub topic: String,
    #[serde(rename = "authToken", default)]
    pub auth_token: String,
    #[serde(default)]
    pub email: String,
    #[serde(rename = "enabledEvents", default)]
    pub enabled_events: Vec<EventType>,
    #[serde(rename = "minSeverity", default)]
    pub min_severity: Severity,
    /// Explicit acknowledgement that a plaintext `http` server URL is
    /// acceptable even though the bearer token would travel unencrypted.
    #[serde(rename = "allowInsecureHttp", default)]
    pub allow_insecure_http: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            server_url: "https://ntfy.sh".to_string(),
            topic: "naslos-alerts".to_string(),
            auth_token: String::new(),
            email: String::new(),
            enabled_events: vec![EVENT_ZFS_HEALTH.to_string(), EVENT_DISK_FAILURE.to_string()],
            min_severity: SEVERITY_WARNING.to_string(),
            allow_insecure_http: false,
        }
    }
}

/// A notification to be sent.
#[derive(Debug, Clone, Default)]
pub struct Notification {
    pub title: String,
    pub message: String,
    pub severity: Severity,
    pub tags: Vec<String>,
}

/// Manages ntfy notifications.
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
            &std::env::var("NOTIFICATIONS_CONFIG")
                .unwrap_or_else(|_| "/var/lib/naslos/notifications.json".to_string()),
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
        // 0600: this file holds the ntfy auth token.
        crate::chartsrepo::write_atomic(&self.config_path, &data, 0o600, 0o750)
    }

    pub fn get_settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    /// Update settings. `auth_token` is a pointer: `None` keeps the existing
    /// token, `Some("")` clears it, `Some(v)` replaces it.
    pub fn update_settings(
        &self,
        mut settings: Settings,
        auth_token: Option<&str>,
    ) -> Result<(), String> {
        let existing = self.settings.lock().unwrap().auth_token.clone();
        settings.auth_token = match auth_token {
            Some(v) => v.to_string(),
            None => existing,
        };
        settings.server_url = settings.server_url.trim().to_string();
        settings.topic = settings.topic.trim().to_string();
        if settings.min_severity.trim().is_empty() {
            settings.min_severity = SEVERITY_WARNING.to_string();
        }
        validate_settings(&settings)?;
        // Persist first: a failed write must not leave the in-memory settings
        // mutated (and apparently saved).
        self.save(&settings)?;
        *self.settings.lock().unwrap() = settings;
        Ok(())
    }

    /// Whether an event type is one the operator subscribes to.
    pub fn event_enabled(&self, event: &str) -> bool {
        self.settings
            .lock()
            .unwrap()
            .enabled_events
            .iter()
            .any(|e| e == event)
    }

    /// Send a notification via ntfy.
    pub async fn send(&self, notif: &Notification) -> Result<(), String> {
        let settings = self.get_settings();
        if !settings.enabled {
            return Ok(());
        }
        if !severity_meets_threshold(&notif.severity, &settings.min_severity) {
            return Ok(());
        }
        validate_settings(&settings)?;
        let target = notification_url(&settings.server_url, &settings.topic)?;
        // Resolve once more at send time and pin the connection to the vetted
        // address, so the host cannot be rebound between validation and the
        // request (AUD-2611-T7-1).
        let client = self.pinned_client(&target, &settings).await?;

        let mut rb = client
            .post(&target)
            .header("Title", &notif.title)
            .header("X-Priority", severity_to_priority(&notif.severity))
            .body(notif.message.clone());
        if !notif.tags.is_empty() {
            rb = rb.header("Tags", notif.tags.join(","));
        }
        if !settings.auth_token.is_empty() {
            rb = rb.bearer_auth(&settings.auth_token);
        }
        if !settings.email.is_empty() {
            rb = rb.header("X-Email", &settings.email);
        }
        let resp = rb.send().await.map_err(|e| transport_error(&e))?;
        check_status(resp.status())
    }

    /// A client pinned to the checked address of `target`; a literal address
    /// uses the shared client (it cannot be rebound).
    async fn pinned_client(
        &self,
        target: &str,
        settings: &Settings,
    ) -> Result<reqwest::Client, String> {
        let parsed = reqwest::Url::parse(target).map_err(|_| "invalid ntfy target".to_string())?;
        require_secure_transport(&parsed, settings)?;
        let host = parsed
            .host_str()
            .ok_or("ntfy target has no host".to_string())?;
        let port = parsed.port_or_known_default().unwrap_or(443);
        if host.parse::<std::net::IpAddr>().is_ok() {
            return Ok(self.client.clone());
        }
        let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host, port))
            .await
            .map_err(|_| "cannot resolve the ntfy host".to_string())?
            .collect();
        let Some(pinned) = addrs.first().copied() else {
            return Err("the ntfy host did not resolve".to_string());
        };
        if addrs
            .iter()
            .any(|addr| crate::ddns::drivers::blocked_ip(&addr.ip()))
        {
            return Err("refusing to send notifications to a non-public address".to_string());
        }
        reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .resolve(host, pinned)
            .build()
            .map_err(|e| format!("building the ntfy client: {e}"))
    }

    /// Send a test notification.
    pub async fn send_test(&self) -> Result<(), String> {
        self.send(&Notification {
            title: "Naslos Test".to_string(),
            message: "This is a test notification from Naslos.".to_string(),
            severity: SEVERITY_INFO.to_string(),
            tags: vec!["white_check_mark".to_string(), "naslos".to_string()],
        })
        .await
    }
}

/// Validate settings at save time. A disabled channel may keep an empty
/// destination; an enabled one needs a non-empty server URL and topic.
pub fn validate_settings(settings: &Settings) -> Result<(), String> {
    if !settings.min_severity.trim().is_empty() && !valid_severity(settings.min_severity.trim()) {
        return Err(format!(
            "unknown minimum severity {:?}",
            settings.min_severity
        ));
    }
    let server_url = settings.server_url.trim();
    if settings.enabled && server_url.is_empty() {
        return Err("ntfy server URL is required when the channel is enabled".to_string());
    }
    if !server_url.is_empty() {
        let parsed = parse_server_url(server_url)?;
        require_secure_transport(&parsed, settings)?;
    }
    let topic = settings.topic.trim();
    if settings.enabled && topic.is_empty() {
        return Err("ntfy topic is required when the channel is enabled".to_string());
    }
    if !topic.is_empty() && !valid_topic(topic) {
        return Err("ntfy topic may only contain letters, digits, '-' and '_'".to_string());
    }
    Ok(())
}

/// Parse and syntax-check an ntfy server URL (no network resolution here; the
/// send path resolves with `tokio::net::lookup_host`).
fn parse_server_url(server_url: &str) -> Result<reqwest::Url, String> {
    let parsed =
        reqwest::Url::parse(server_url).map_err(|_| "invalid ntfy server URL".to_string())?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(format!(
            "ntfy server URL must use http or https, got {:?}",
            parsed.scheme()
        ));
    }
    let host = parsed
        .host_str()
        .ok_or("ntfy server URL has no host")?
        .trim_start_matches('[')
        .trim_end_matches(']');
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        if crate::ddns::drivers::blocked_ip(&ip) {
            return Err("refusing to target a non-public address".to_string());
        }
    }
    Ok(parsed)
}

/// Reject plaintext `http` when a bearer token would travel over it, unless
/// the operator explicitly acknowledged `allowInsecureHttp`.
fn require_secure_transport(parsed: &reqwest::Url, settings: &Settings) -> Result<(), String> {
    if parsed.scheme() == "http" && !settings.auth_token.is_empty() {
        if !settings.allow_insecure_http {
            return Err(
                "refusing to send an ntfy auth token over plaintext http; use https or set allowInsecureHttp"
                    .to_string(),
            );
        }
        tracing::warn!(
            "ntfy auth token is being sent over plaintext http (allowInsecureHttp acknowledged)"
        );
    }
    Ok(())
}

/// The redacted GET projection. Destination values and the token are never
/// echoed; only their presence is reported.
pub fn public_view(settings: &Settings) -> serde_json::Value {
    serde_json::json!({
        "enabled": settings.enabled,
        "category": "ntfy",
        "hasServerUrl": !settings.server_url.trim().is_empty(),
        "hasTopic": !settings.topic.trim().is_empty(),
        "hasAuthToken": !settings.auth_token.is_empty(),
        "hasUrlToken": !settings.auth_token.is_empty()
            || !settings.server_url.trim().is_empty()
            || !settings.topic.trim().is_empty(),
        "hasEmail": !settings.email.trim().is_empty(),
        "allowInsecureHttp": settings.allow_insecure_http,
        "enabledEvents": settings.enabled_events,
        "minSeverity": settings.min_severity,
    })
}

/// Validate the operator-configured ntfy target and render the publish URL.
/// The URL is syntax-checked here; DNS/SSRF re-checks happen at send time.
pub fn notification_url(server_url: &str, topic: &str) -> Result<String, String> {
    let server_url = if server_url.trim().is_empty() {
        "https://ntfy.sh"
    } else {
        server_url.trim()
    };
    let mut parsed = parse_server_url(server_url)?;
    if topic.is_empty() {
        return Err("ntfy topic is empty".to_string());
    }
    if !valid_topic(topic) {
        return Err("ntfy topic may only contain letters, digits, '-' and '_'".to_string());
    }
    let base_path = parsed.path().trim_end_matches('/').to_string();
    parsed.set_path(&format!("{base_path}/{topic}"));
    Ok(parsed.to_string())
}

/// A transport failure without the publish URL or topic in it.
fn transport_error(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "the ntfy request timed out".to_string()
    } else if e.is_connect() {
        "could not connect to the ntfy server".to_string()
    } else if e.is_redirect() {
        "the ntfy server redirected (redirects are disabled)".to_string()
    } else {
        "the ntfy request failed".to_string()
    }
}

/// A 3xx is a failed delivery: redirects are disabled, so only a 2xx accepted
/// the notification.
fn check_status(status: reqwest::StatusCode) -> Result<(), String> {
    if status.is_success() {
        Ok(())
    } else if status.is_redirection() {
        Err("the ntfy server redirected (redirects are disabled)".to_string())
    } else {
        Err(format!(
            "the ntfy server rejected the notification (HTTP {})",
            status.as_u16()
        ))
    }
}

/// Enforce ntfy's topic charset.
pub fn valid_topic(topic: &str) -> bool {
    topic
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn severity_rank(severity: &str) -> i32 {
    match severity {
        SEVERITY_INFO => 0,
        SEVERITY_WARNING => 1,
        SEVERITY_ERROR => 2,
        SEVERITY_CRITICAL => 3,
        _ => 0,
    }
}

pub fn severity_meets_threshold(severity: &str, threshold: &str) -> bool {
    severity_rank(severity) >= severity_rank(threshold)
}

/// Whether a string is one of the four known severities.
pub fn valid_severity(severity: &str) -> bool {
    matches!(
        severity,
        SEVERITY_INFO | SEVERITY_WARNING | SEVERITY_ERROR | SEVERITY_CRITICAL
    )
}

pub fn severity_to_priority(severity: &str) -> &'static str {
    match severity {
        SEVERITY_INFO => "2",
        SEVERITY_WARNING => "3",
        SEVERITY_ERROR => "4",
        SEVERITY_CRITICAL => "5",
        _ => "3",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_validation_and_topic_charset() {
        assert_eq!(
            notification_url("https://ntfy.sh", "naslos-alerts").unwrap(),
            "https://ntfy.sh/naslos-alerts"
        );
        assert_eq!(
            notification_url("https://1.1.1.1/", "topic").unwrap(),
            "https://1.1.1.1/topic"
        );
        assert!(notification_url("ftp://ntfy.sh", "t").is_err());
        assert!(notification_url("https://ntfy.sh", "bad/topic").is_err());
        assert!(notification_url("https://169.254.169.254", "t").is_err());
        assert!(notification_url("http://127.0.0.1", "t").is_err());
        assert!(notification_url("http://10.0.0.5", "t").is_err());
        assert!(notification_url("http://192.168.1.10", "t").is_err());
        assert!(notification_url("http://100.64.0.1", "t").is_err());
    }

    #[test]
    fn enabled_channel_requires_destination_and_valid_topic() {
        let mut s = Settings {
            enabled: true,
            server_url: String::new(),
            topic: "alerts".into(),
            ..Default::default()
        };
        assert!(validate_settings(&s).is_err());
        s.server_url = "https://ntfy.sh".into();
        s.topic = "bad topic".into();
        assert!(validate_settings(&s).is_err());
        s.topic = "alerts".into();
        assert!(validate_settings(&s).is_ok());
        // A disabled channel may keep an empty destination.
        s.enabled = false;
        s.server_url.clear();
        s.topic.clear();
        assert!(validate_settings(&s).is_ok());
    }

    #[test]
    fn plaintext_http_needs_an_acknowledgement_for_a_token() {
        let mut s = Settings {
            enabled: true,
            server_url: "http://1.1.1.1:8080".into(),
            topic: "alerts".into(),
            auth_token: "secret".into(),
            ..Default::default()
        };
        assert!(validate_settings(&s).is_err());
        s.allow_insecure_http = true;
        assert!(validate_settings(&s).is_ok());
        // Without a token plaintext is allowed.
        s.allow_insecure_http = false;
        s.auth_token.clear();
        assert!(validate_settings(&s).is_ok());
    }

    #[test]
    fn redirect_status_is_not_success() {
        assert!(check_status(reqwest::StatusCode::OK).is_ok());
        assert!(check_status(reqwest::StatusCode::NO_CONTENT).is_ok());
        assert!(check_status(reqwest::StatusCode::FOUND).is_err());
        assert!(check_status(reqwest::StatusCode::MOVED_PERMANENTLY).is_err());
        assert!(check_status(reqwest::StatusCode::BAD_GATEWAY).is_err());
    }

    #[test]
    fn public_view_never_echoes_the_destination_or_token() {
        let s = Settings {
            enabled: true,
            server_url: "https://user:pw@ntfy.example/".into(),
            topic: "private-topic".into(),
            auth_token: "secret".into(),
            ..Default::default()
        };
        let value = public_view(&s).to_string();
        assert!(!value.contains("private-topic"), "{value}");
        assert!(!value.contains("ntfy.example"), "{value}");
        assert!(!value.contains("secret"), "{value}");
        assert!(value.contains("\"hasAuthToken\":true"), "{value}");
        assert!(value.contains("\"category\":\"ntfy\""), "{value}");
    }

    #[test]
    fn severity_threshold_and_priority() {
        assert!(severity_meets_threshold(SEVERITY_ERROR, SEVERITY_WARNING));
        assert!(!severity_meets_threshold(SEVERITY_INFO, SEVERITY_WARNING));
        assert_eq!(severity_to_priority(SEVERITY_CRITICAL), "5");
    }

    #[test]
    fn update_settings_preserves_token() {
        let m = Manager::new("");
        let mut s = m.get_settings();
        s.auth_token = "secret".to_string();
        m.update_settings(s.clone(), Some("secret")).unwrap();
        // None keeps the token.
        m.update_settings(s.clone(), None).unwrap();
        assert_eq!(m.get_settings().auth_token, "secret");
        // Some("") clears it.
        m.update_settings(s, Some("")).unwrap();
        assert_eq!(m.get_settings().auth_token, "");
    }
}
