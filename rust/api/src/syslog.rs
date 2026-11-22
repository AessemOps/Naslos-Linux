//! Syslog alert channel (FR-ALT-09).
//!
//! Forwards one RFC 5424 message per selected event over UDP, TCP or TLS. The
//! channel keeps its own persisted settings, event routing and severity floor.
//! It has no credentials, and the message carries only the event fields, never
//! the channel settings.
//!
//! The target policy is the LAN-aware one shared with SMTP
//! (`crate::alert_transport`): an explicit RFC1918 address is allowed, while
//! loopback, link-local (including cloud metadata), CGNAT, ULA and other
//! non-routable addresses are refused. TLS validates the collector certificate
//! against the compiled-in Mozilla trust anchors.
//!
//! Delivery is one datagram/stream write under a finite timeout. Errors name
//! the failure category only, never the host or the payload.

use crate::notifications::{
    severity_meets_threshold, valid_severity, EventType, Notification, Severity, EVENT_TEST,
    SEVERITY_INFO, SEVERITY_WARNING,
};
use std::sync::Mutex;
use std::time::Duration;
use tokio::io::{AsyncWrite, AsyncWriteExt};

/// Upper bound on one RFC 5424 message (header plus MSG).
pub const MAX_SYSLOG_BYTES: usize = 8 * 1024;
/// Whole-operation bound: resolve, connect and write.
const SYSLOG_TIMEOUT: Duration = Duration::from_secs(10);

fn default_protocol() -> String {
    "udp".to_string()
}

fn default_port() -> u16 {
    514
}

fn default_facility() -> String {
    "local0".to_string()
}

/// The syslog channel configuration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Settings {
    pub enabled: bool,
    /// `udp`, `tcp` or `tls`.
    #[serde(default = "default_protocol")]
    pub protocol: String,
    #[serde(default)]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// A facility name (`kern`, `user`, `local0`, …) or a number 0–23.
    #[serde(default = "default_facility")]
    pub facility: String,
    #[serde(rename = "enabledEvents", default)]
    pub enabled_events: Vec<EventType>,
    #[serde(rename = "minSeverity", default)]
    pub min_severity: Severity,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            protocol: default_protocol(),
            host: String::new(),
            port: default_port(),
            facility: default_facility(),
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

fn default_port_for_protocol(protocol: &str) -> u16 {
    match protocol {
        "tls" => 6514,
        _ => 514,
    }
}

/// Apply defaults for omitted fields. Public so the API can validate a PUT
/// body exactly as the manager will persist it.
pub fn normalize_settings(settings: &mut Settings) -> Result<(), String> {
    settings.host = crate::alert_transport::normalize_host(&settings.host)?;
    settings.protocol = settings.protocol.trim().to_ascii_lowercase();
    if settings.protocol.is_empty() {
        settings.protocol = default_protocol();
    }
    settings.facility = settings.facility.trim().to_ascii_lowercase();
    if settings.facility.is_empty() {
        settings.facility = default_facility();
    }
    if settings.port == 0 {
        settings.port = default_port_for_protocol(&settings.protocol);
    }
    settings.min_severity = settings.min_severity.trim().to_string();
    if settings.min_severity.is_empty() {
        settings.min_severity = SEVERITY_WARNING.to_string();
    }
    Ok(())
}

/// Validate everything except the network target. The send path resolves the
/// host asynchronously and applies the target guard there.
fn validate_fields(settings: &Settings) -> Result<(), String> {
    if !matches!(settings.protocol.as_str(), "udp" | "tcp" | "tls") {
        return Err(format!(
            "unknown syslog protocol {:?} (expected udp, tcp or tls)",
            settings.protocol
        ));
    }
    if settings.port == 0 {
        return Err("syslog port must be between 1 and 65535".to_string());
    }
    if facility_code(&settings.facility).is_none() {
        return Err(format!("unknown syslog facility {:?}", settings.facility));
    }
    if !settings.min_severity.is_empty() && !valid_severity(&settings.min_severity) {
        return Err(format!(
            "unknown minimum severity {:?}",
            settings.min_severity
        ));
    }
    if settings.enabled && settings.host.is_empty() {
        return Err("syslog host is required when the channel is enabled".to_string());
    }
    Ok(())
}

/// Validate settings before saving or reporting a client error. A non-empty
/// target is checked even while the channel is disabled.
pub fn validate_settings(settings: &Settings) -> Result<(), String> {
    validate_fields(settings)?;
    if !settings.host.is_empty() {
        crate::alert_transport::validate_target(&settings.host, settings.port)?;
    }
    Ok(())
}

/// Map a facility name (or a 0–23 number) to its RFC 5424 code.
pub fn facility_code(raw: &str) -> Option<u8> {
    let name = raw.trim().to_ascii_lowercase();
    if let Ok(number) = name.parse::<u8>() {
        return (number <= 23).then_some(number);
    }
    let code = match name.as_str() {
        "kern" => 0,
        "user" => 1,
        "mail" => 2,
        "daemon" => 3,
        "auth" => 4,
        "syslog" => 5,
        "lpr" => 6,
        "news" => 7,
        "uucp" => 8,
        "cron" => 9,
        "authpriv" => 10,
        "ftp" => 11,
        "ntp" => 12,
        "audit" | "security" => 13,
        "console" | "alert" => 14,
        "clock" => 15,
        "local0" => 16,
        "local1" => 17,
        "local2" => 18,
        "local3" => 19,
        "local4" => 20,
        "local5" => 21,
        "local6" => 22,
        "local7" => 23,
        _ => return None,
    };
    Some(code)
}

/// Map a Naslos severity to the RFC 5424 severity code.
pub fn severity_code(severity: &str) -> u8 {
    match severity {
        crate::notifications::SEVERITY_INFO => 6,
        crate::notifications::SEVERITY_WARNING => 4,
        crate::notifications::SEVERITY_ERROR => 3,
        crate::notifications::SEVERITY_CRITICAL => 2,
        _ => 4,
    }
}

fn sanitize_token(value: &str, max: usize) -> String {
    let token: String = value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        .take(max)
        .collect();
    if token.is_empty() {
        "-".to_string()
    } else {
        token
    }
}

fn sanitize_message(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            '\r' | '\n' | '\t' => ' ',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect()
}

fn truncate_utf8(value: &str, max: usize) -> &str {
    if value.len() <= max {
        return value;
    }
    let mut end = max;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

/// Render the RFC 5424 payload:
/// `<PRI>1 TIMESTAMP HOSTNAME APP-NAME PROCID MSGID STRUCTURED-DATA MSG`.
/// The output is bounded to [`MAX_SYSLOG_BYTES`].
pub fn render_rfc5424(facility: u8, event: &str, notif: &Notification) -> String {
    let pri = u16::from(facility) * 8 + u16::from(severity_code(&notif.severity));
    let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let msgid = sanitize_token(event, 32);
    let message = sanitize_message(&format!("{}: {}", notif.title, notif.message));
    // RFC 5424 reserved fields (nil values) are `-`.
    let header = format!("<{pri}>1 {timestamp} naslos naslos - {msgid} -");
    let room = MAX_SYSLOG_BYTES.saturating_sub(header.len() + 1);
    let message = truncate_utf8(&message, room);
    format!("{header} {message}")
}

/// Manages the persisted syslog channel.
pub struct Manager {
    settings: Mutex<Settings>,
    config_path: String,
}

impl Manager {
    pub fn new(config_path: &str) -> Self {
        let mut m = Self {
            settings: Mutex::new(Settings::default()),
            config_path: config_path.to_string(),
        };
        m.load();
        m
    }

    pub fn from_env() -> Self {
        Self::new(
            &std::env::var("ALERT_SYSLOG_CONFIG")
                .unwrap_or_else(|_| "/var/lib/naslos/syslog.json".to_string()),
        )
    }

    fn load(&mut self) {
        if self.config_path.is_empty() {
            return;
        }
        let Ok(data) = std::fs::read(&self.config_path) else {
            return;
        };
        if let Ok(mut settings) = serde_json::from_slice::<Settings>(&data) {
            // Only field normalization happens on load. The send path validates
            // the target again, so a hand-edited file cannot reach a
            // loopback/metadata address.
            if normalize_settings(&mut settings).is_ok() {
                *self.settings.lock().unwrap() = settings;
            } else {
                tracing::warn!("ignoring a malformed syslog alert configuration file");
            }
        }
    }

    fn save(&self, settings: &Settings) -> Result<(), String> {
        if self.config_path.is_empty() {
            return Ok(());
        }
        let data =
            serde_json::to_vec_pretty(settings).map_err(|e| format!("marshaling settings: {e}"))?;
        crate::chartsrepo::write_atomic(&self.config_path, &data, 0o600, 0o750)
    }

    pub fn get_settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    /// Update settings. A rejected target leaves the previous settings intact.
    pub fn update_settings(&self, mut settings: Settings) -> Result<(), String> {
        normalize_settings(&mut settings)?;
        validate_settings(&settings)?;
        self.save(&settings)?;
        *self.settings.lock().unwrap() = settings;
        Ok(())
    }

    /// Reset the channel to its defaults (disabled, no target).
    pub fn clear(&self) -> Result<(), String> {
        let settings = Settings::default();
        self.save(&settings)?;
        *self.settings.lock().unwrap() = settings;
        Ok(())
    }

    /// Deliver one event, respecting the channel's routing and severity.
    /// A dropped event is `Ok(())`, like the other channels.
    pub async fn send(&self, event: &str, notif: &Notification) -> Result<(), String> {
        let settings = self.get_settings();
        if !settings.allows(event, &notif.severity) {
            return Ok(());
        }
        self.deliver(&settings, event, notif).await
    }

    /// Send a test message. Requires the channel to be enabled but bypasses
    /// event routing and the severity floor.
    pub async fn send_test(&self) -> Result<(), String> {
        let settings = self.get_settings();
        if !settings.enabled {
            return Err("syslog channel is disabled".to_string());
        }
        self.deliver(
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

    async fn deliver(
        &self,
        settings: &Settings,
        event: &str,
        notif: &Notification,
    ) -> Result<(), String> {
        // A hand-edited settings file may have bypassed save-time validation.
        validate_fields(settings)?;
        let facility = facility_code(&settings.facility)
            .ok_or_else(|| "unknown syslog facility".to_string())?;
        let payload = render_rfc5424(facility, event, notif);
        let fut = async {
            let host = crate::alert_transport::normalize_host(&settings.host)?;
            if host.is_empty() {
                return Err("syslog host is empty".to_string());
            }
            let addrs = crate::alert_transport::resolve_target(&host, settings.port).await?;
            let target = *addrs
                .first()
                .ok_or_else(|| "the syslog host did not resolve".to_string())?;
            match settings.protocol.as_str() {
                "udp" => send_udp(target, payload.as_bytes()).await,
                "tcp" => send_stream(target, payload.as_bytes(), false, &host).await,
                "tls" => send_stream(target, payload.as_bytes(), true, &host).await,
                _ => Err("unknown syslog protocol".to_string()),
            }
        };
        tokio::time::timeout(SYSLOG_TIMEOUT, fut)
            .await
            .map_err(|_| "the syslog request timed out".to_string())?
    }
}

async fn send_udp(target: std::net::SocketAddr, payload: &[u8]) -> Result<(), String> {
    let bind = if target.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let socket = tokio::net::UdpSocket::bind(bind)
        .await
        .map_err(|_| "could not create the syslog socket".to_string())?;
    socket
        .connect(target)
        .await
        .map_err(|_| "could not connect to the syslog server".to_string())?;
    let sent = socket
        .send(payload)
        .await
        .map_err(|_| "could not send the syslog message".to_string())?;
    if sent != payload.len() {
        return Err("could not send the complete syslog message".to_string());
    }
    Ok(())
}

async fn send_stream(
    target: std::net::SocketAddr,
    payload: &[u8],
    tls: bool,
    host: &str,
) -> Result<(), String> {
    let tcp = tokio::net::TcpStream::connect(target)
        .await
        .map_err(|_| "could not connect to the syslog server".to_string())?;
    if tls {
        let mut stream = crate::alert_transport::tls_connect(tcp, host).await?;
        write_frame(&mut stream, payload).await?;
        let _ = stream.shutdown().await;
    } else {
        let mut stream = tcp;
        write_frame(&mut stream, payload).await?;
        let _ = stream.shutdown().await;
    }
    Ok(())
}

async fn write_frame<W: AsyncWrite + Unpin>(stream: &mut W, payload: &[u8]) -> Result<(), String> {
    // Non-transparent framing: one RFC 5424 message per line.
    stream
        .write_all(payload)
        .await
        .map_err(|_| "could not send the syslog message".to_string())?;
    stream
        .write_all(b"\n")
        .await
        .map_err(|_| "could not send the syslog message".to_string())?;
    stream
        .flush()
        .await
        .map_err(|_| "could not flush the syslog message".to_string())
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

    fn enabled(host: &str) -> Settings {
        Settings {
            enabled: true,
            host: host.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn facilities_and_severities_map_to_rfc5424_codes() {
        assert_eq!(facility_code("kern"), Some(0));
        assert_eq!(facility_code("local0"), Some(16));
        assert_eq!(facility_code("LOCAL7"), Some(23));
        assert_eq!(facility_code(" 7 "), Some(7));
        assert_eq!(facility_code("24"), None);
        assert_eq!(facility_code("bogus"), None);
        assert_eq!(severity_code(SEVERITY_INFO), 6);
        assert_eq!(severity_code(SEVERITY_WARNING), 4);
        assert_eq!(severity_code(SEVERITY_ERROR), 3);
        assert_eq!(severity_code(SEVERITY_CRITICAL), 2);
    }

    #[test]
    fn payload_is_rfc5424_with_pri_and_event_msgid() {
        let payload = render_rfc5424(16, EVENT_DISK_FAILURE, &notif());
        // local0 (16) * 8 + err (3) = 131.
        assert!(payload.starts_with("<131>1 "), "{payload}");
        assert!(payload.contains(" naslos naslos - disk_failure - "));
        assert!(payload
            .ends_with("Disk sda is at 60°C: Disk /dev/sda is above the 55°C warning threshold."));
        assert!(!payload.contains('\r'));
        assert!(!payload.contains('\n'));

        let info = render_rfc5424(
            1,
            EVENT_ZFS_HEALTH,
            &Notification {
                severity: SEVERITY_INFO.to_string(),
                ..notif()
            },
        );
        // user (1) * 8 + informational (6) = 14.
        assert!(info.starts_with("<14>1 "));
    }

    #[test]
    fn payload_sanitizes_control_characters_and_the_msgid() {
        let payload = render_rfc5424(
            16,
            "disk_failure\r\ninjected",
            &Notification {
                title: "line one\nline two".to_string(),
                message: "tab\there\r\nnext".to_string(),
                severity: SEVERITY_WARNING.to_string(),
                tags: Vec::new(),
            },
        );
        assert!(!payload.contains('\r'));
        assert!(!payload.contains('\n'));
        assert!(payload.contains("line one line two: tab here  next"));
        assert!(payload.contains(" - disk_failureinjected - "));
    }

    #[test]
    fn payload_is_bounded() {
        let payload = render_rfc5424(
            16,
            EVENT_DISK_FAILURE,
            &Notification {
                title: "t".to_string(),
                message: "x".repeat(MAX_SYSLOG_BYTES * 2),
                severity: SEVERITY_ERROR.to_string(),
                tags: Vec::new(),
            },
        );
        assert!(payload.len() <= MAX_SYSLOG_BYTES);
        assert!(payload.is_char_boundary(payload.len()));
    }

    #[test]
    fn target_policy_allows_lan_collectors_and_refuses_special_addresses() {
        for allowed in ["192.168.1.10", "10.0.0.5", "172.16.4.2", "1.1.1.1"] {
            assert!(validate_settings(&enabled(allowed)).is_ok(), "{allowed}");
        }
        for refused in [
            "127.0.0.1",
            "169.254.169.254",
            "::1",
            "fe80::1",
            "fd00::1",
            "100.64.0.1",
            "0.0.0.0",
        ] {
            assert!(
                validate_settings(&enabled(refused)).is_err(),
                "{refused} must be refused"
            );
        }
    }

    #[test]
    fn protocol_and_facility_are_validated() {
        let mut s = enabled("192.168.1.10");
        s.protocol = "carrier-pigeon".to_string();
        assert!(validate_settings(&s).is_err());

        let mut s = enabled("192.168.1.10");
        s.facility = "bogus".to_string();
        assert!(validate_settings(&s).is_err());

        let mut s = enabled("192.168.1.10");
        s.port = 0;
        assert!(validate_settings(&s).is_err());

        let mut s = enabled("");
        assert!(validate_settings(&s).is_err());
        s.enabled = false;
        assert!(validate_settings(&s).is_ok());
    }

    #[test]
    fn routing_honours_events_and_severity() {
        let mut settings = Settings {
            enabled: true,
            host: "192.168.1.10".to_string(),
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

    fn temp_manager(settings: Option<&Settings>) -> (tempfile::TempDir, Manager) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("syslog.json");
        if let Some(settings) = settings {
            std::fs::write(&path, serde_json::to_vec(settings).unwrap()).unwrap();
        }
        let manager = Manager::new(path.to_str().unwrap());
        (dir, manager)
    }

    #[test]
    fn settings_round_trip_and_reset() {
        let (dir, manager) = temp_manager(None);
        manager.update_settings(enabled("192.168.1.10")).unwrap();
        let path = dir.path().join("syslog.json");
        let reloaded = Manager::new(path.to_str().unwrap());
        assert!(reloaded.get_settings().enabled);
        assert_eq!(reloaded.get_settings().host, "192.168.1.10");
        reloaded.clear().unwrap();
        assert!(!reloaded.get_settings().enabled);
        assert!(reloaded.get_settings().host.is_empty());
    }

    #[test]
    fn update_settings_rejects_invalid_targets_and_keeps_the_previous_ones() {
        let (_dir, manager) = temp_manager(None);
        manager.update_settings(enabled("192.168.1.10")).unwrap();
        assert!(manager.update_settings(enabled("127.0.0.1")).is_err());
        assert_eq!(manager.get_settings().host, "192.168.1.10");
    }

    #[tokio::test]
    async fn disabled_or_unrouted_events_are_dropped_without_network() {
        let mut settings = enabled("192.168.1.10");
        settings.enabled_events = vec![EVENT_ZFS_HEALTH.to_string()];
        let (_dir, manager) = temp_manager(Some(&settings));
        assert!(manager.send(EVENT_DISK_FAILURE, &notif()).await.is_ok());

        settings.enabled = false;
        let (_dir, manager) = temp_manager(Some(&settings));
        assert!(manager.send(EVENT_ZFS_HEALTH, &notif()).await.is_ok());
    }

    #[tokio::test]
    async fn send_path_refuses_an_unknown_protocol_from_a_hand_edited_file() {
        let mut settings = enabled("192.168.1.10");
        settings.protocol = "sctp".to_string();
        let (_dir, manager) = temp_manager(Some(&settings));
        let error = manager.send_test().await.unwrap_err();
        assert!(error.contains("syslog protocol"), "{error}");
    }

    #[tokio::test]
    async fn test_action_requires_an_enabled_channel() {
        let (_dir, manager) = temp_manager(None);
        let error = manager.send_test().await.unwrap_err();
        assert!(error.contains("disabled"));
    }

    #[tokio::test]
    async fn test_action_is_not_gated_by_event_routing() {
        let mut settings = enabled("127.0.0.1");
        settings.enabled_events = vec![EVENT_DISK_FAILURE.to_string()];
        settings.min_severity = SEVERITY_CRITICAL.to_string();
        let (_dir, manager) = temp_manager(Some(&settings));
        let error = manager.send_test().await.unwrap_err();
        assert!(error.contains("non-routable"));
        assert!(!error.contains("127.0.0.1"));
    }
}
