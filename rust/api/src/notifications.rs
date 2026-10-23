//! ntfy push notifications (port of `api/internal/notifications`).

use std::sync::Mutex;
use std::time::Duration;

/// The type of event that triggers a notification.
pub type EventType = String;
pub const EVENT_ZFS_HEALTH: &str = "zfs_health";
pub const EVENT_DISK_FAILURE: &str = "disk_failure";
pub const EVENT_BACKUP_FAILURE: &str = "backup_failure";
pub const EVENT_BACKUP_SUCCESS: &str = "backup_success";

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
        let mut guard = self.settings.lock().unwrap();
        settings.auth_token = match auth_token {
            Some(v) => v.to_string(),
            None => guard.auth_token.clone(),
        };
        *guard = settings.clone();
        self.save(&settings)
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
        let target = notification_url(&settings.server_url, &settings.topic)?;

        let mut rb = self
            .client
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
        let resp = rb
            .send()
            .await
            .map_err(|e| format!("sending notification: {e}"))?;
        if resp.status().as_u16() >= 400 {
            return Err(format!("ntfy returned status {}", resp.status().as_u16()));
        }
        Ok(())
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

/// Validate the operator-configured ntfy target and render the publish URL.
pub fn notification_url(server_url: &str, topic: &str) -> Result<String, String> {
    let server_url = if server_url.trim().is_empty() {
        "https://ntfy.sh"
    } else {
        server_url.trim()
    };
    let mut parsed =
        reqwest::Url::parse(server_url).map_err(|e| format!("invalid ntfy server URL: {e}"))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(format!(
            "ntfy server URL must use http or https, got {:?}",
            parsed.scheme()
        ));
    }
    let host = parsed
        .host_str()
        .ok_or("ntfy server URL has no host")?
        .to_string();
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        let link_local = match ip {
            std::net::IpAddr::V4(v4) => v4.is_link_local(),
            std::net::IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) == 0xfe80,
        };
        if link_local {
            return Err(format!(
                "refusing to send notifications to a link-local address ({host})"
            ));
        }
    }
    if topic.is_empty() {
        return Err("ntfy topic is empty".to_string());
    }
    if !valid_topic(topic) {
        return Err(format!(
            "ntfy topic {topic:?} may only contain letters, digits, '-' and '_'"
        ));
    }
    let base_path = parsed.path().trim_end_matches('/').to_string();
    parsed.set_path(&format!("{base_path}/{topic}"));
    Ok(parsed.to_string())
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
            notification_url("https://ntfy.example/", "topic").unwrap(),
            "https://ntfy.example/topic"
        );
        assert!(notification_url("ftp://ntfy.sh", "t").is_err());
        assert!(notification_url("https://ntfy.sh", "bad/topic").is_err());
        assert!(notification_url("https://169.254.169.254", "t").is_err());
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
