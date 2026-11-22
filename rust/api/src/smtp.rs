//! SMTP/email alert channel (FR-ALT-07).
//!
//! The channel keeps its own persisted settings, event routing and severity
//! floor, and delivers one message per selected event. It speaks SMTP directly
//! (no mail library): `implicit` TLS (SMTPS), `starttls`, or plaintext `none`
//! for an isolated LAN relay. TLS always validates the server certificate
//! against the compiled-in Mozilla trust anchors; there is no insecure mode.
//!
//! Credentials are write-only: the API exposes `hasPassword`, never the stored
//! password, and the settings file is mode `0600`. A password is refused over
//! plaintext (`tls = none`) so it cannot cross the network in the clear.
//!
//! Delivery is a single attempt under a whole-operation timeout. A response
//! body/reply is never surfaced with its text: failures report a fixed category
//! and the SMTP numeric code only, so a server cannot reflect the username,
//! password or message back through the API or a log line.

use crate::notifications::{
    severity_meets_threshold, valid_severity, EventType, Notification, Severity, EVENT_TEST,
    SEVERITY_INFO, SEVERITY_WARNING,
};
use std::sync::Mutex;
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

/// Upper bound on one SMTP reply line, to keep a hostile/broken server bounded.
pub const MAX_REPLY_LINE: usize = 4096;
/// Upper bound on the number of lines in one multiline reply.
pub const MAX_REPLY_LINES: usize = 64;
/// Upper bound on the total bytes of one multiline reply.
pub const MAX_REPLY_BYTES: usize = 16 * 1024;
/// Upper bound on the DATA payload (headers plus body).
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;
/// Upper bound on recipients so one event cannot fan out unboundedly.
pub const MAX_RECIPIENTS: usize = 20;
/// Whole-operation bound: connect, TLS, auth, envelope and DATA.
const SMTP_TIMEOUT: Duration = Duration::from_secs(15);

fn default_port() -> u16 {
    587
}

fn default_tls() -> String {
    "starttls".to_string()
}

/// The SMTP channel configuration.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Settings {
    pub enabled: bool,
    #[serde(default)]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// `starttls`, `implicit` or `none`.
    #[serde(default = "default_tls")]
    pub tls: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub to: Vec<String>,
    #[serde(rename = "enabledEvents", default)]
    pub enabled_events: Vec<EventType>,
    #[serde(rename = "minSeverity", default)]
    pub min_severity: Severity,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            host: String::new(),
            port: default_port(),
            tls: default_tls(),
            username: String::new(),
            password: String::new(),
            from: String::new(),
            to: Vec::new(),
            enabled_events: vec![
                crate::notifications::EVENT_ZFS_HEALTH.to_string(),
                crate::notifications::EVENT_DISK_FAILURE.to_string(),
            ],
            min_severity: SEVERITY_WARNING.to_string(),
        }
    }
}

impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("smtp::Settings")
            .field("enabled", &self.enabled)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("tls", &self.tls)
            .field("username", &self.username)
            .field(
                "password",
                &if self.password.is_empty() {
                    "<unset>"
                } else {
                    "<redacted>"
                },
            )
            .field("from", &self.from)
            .field("to", &self.to)
            .field("enabled_events", &self.enabled_events)
            .field("min_severity", &self.min_severity)
            .finish()
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

fn default_port_for_tls(tls: &str) -> u16 {
    match tls {
        "implicit" => 465,
        "none" => 25,
        _ => 587,
    }
}

fn normalize(settings: &mut Settings) -> Result<(), String> {
    settings.host = crate::alert_transport::normalize_host(&settings.host)?;
    settings.tls = settings.tls.trim().to_ascii_lowercase();
    if settings.tls.is_empty() {
        settings.tls = default_tls();
    }
    if settings.port == 0 {
        settings.port = default_port_for_tls(&settings.tls);
    }
    settings.min_severity = settings.min_severity.trim().to_string();
    if settings.min_severity.is_empty() {
        settings.min_severity = SEVERITY_WARNING.to_string();
    }
    settings.from = settings.from.trim().to_string();
    settings.to = settings
        .to
        .iter()
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
        .collect();
    Ok(())
}

/// Validate everything except the network target. Used by the send path,
/// which resolves the host asynchronously and applies the target guard there.
fn validate_fields(settings: &Settings) -> Result<(), String> {
    if !matches!(settings.tls.as_str(), "starttls" | "implicit" | "none") {
        return Err(format!("unknown SMTP TLS mode {:?}", settings.tls));
    }
    if settings.port == 0 {
        return Err("SMTP port must be between 1 and 65535".to_string());
    }
    if !settings.min_severity.is_empty() && !valid_severity(&settings.min_severity) {
        return Err(format!(
            "unknown minimum severity {:?}",
            settings.min_severity
        ));
    }
    if !settings.password.is_empty() && settings.username.is_empty() {
        return Err("SMTP password requires a username".to_string());
    }
    if !settings.password.is_empty() && settings.tls == "none" {
        return Err("SMTP credentials require STARTTLS or implicit TLS".to_string());
    }
    if !settings.from.is_empty() && !valid_mailbox(&settings.from) {
        return Err("SMTP From address is not a valid mailbox".to_string());
    }
    for recipient in &settings.to {
        if !valid_mailbox(recipient) {
            return Err("SMTP recipient is not a valid mailbox".to_string());
        }
    }
    if settings.to.len() > MAX_RECIPIENTS {
        return Err(format!(
            "SMTP recipients are limited to {MAX_RECIPIENTS} addresses"
        ));
    }
    if settings.enabled {
        if settings.host.is_empty() {
            return Err("SMTP host is required when the channel is enabled".to_string());
        }
        if settings.from.is_empty() {
            return Err("SMTP From address is required when the channel is enabled".to_string());
        }
        if settings.to.is_empty() {
            return Err(
                "SMTP needs at least one recipient when the channel is enabled".to_string(),
            );
        }
    }
    Ok(())
}

/// Validate settings before either saving them or reporting a client error.
/// Invalid non-empty fields are rejected even while the channel is disabled so
/// a bad configuration cannot be stored and only fail later at send time.
pub fn validate_settings(settings: &Settings) -> Result<(), String> {
    validate_fields(settings)?;
    if !settings.host.is_empty() {
        crate::alert_transport::validate_target(&settings.host, settings.port)?;
    }
    Ok(())
}

/// Manages the persisted SMTP channel.
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
            &std::env::var("ALERT_SMTP_CONFIG")
                .unwrap_or_else(|_| "/var/lib/naslos/smtp.json".to_string()),
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
            // the target again, so a hand-edited file that bypassed save-time
            // checks still cannot reach a loopback/metadata address.
            if normalize(&mut settings).is_ok() {
                *self.settings.lock().unwrap() = settings;
            } else {
                tracing::warn!("ignoring a malformed SMTP alert configuration file");
            }
        }
    }

    fn save(&self, settings: &Settings) -> Result<(), String> {
        if self.config_path.is_empty() {
            return Ok(());
        }
        let data =
            serde_json::to_vec_pretty(settings).map_err(|e| format!("marshaling settings: {e}"))?;
        // 0600: this file may hold the SMTP password.
        crate::chartsrepo::write_atomic(&self.config_path, &data, 0o600, 0o750)
    }

    pub fn get_settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    /// Validate an update against the merged settings without persisting it.
    /// The API uses this to report a client error before touching the file.
    pub fn validate_update(
        &self,
        settings: &Settings,
        password: Option<&str>,
    ) -> Result<(), String> {
        let mut merged = settings.clone();
        merged.password = match password {
            Some(password) => password.to_string(),
            None => self.settings.lock().unwrap().password.clone(),
        };
        normalize(&mut merged)?;
        validate_settings(&merged)
    }

    /// Update settings. `password` follows the write-only convention: `None`
    /// keeps the stored value, `Some("")` clears it, `Some(v)` replaces it.
    ///
    /// Validation runs against the merged settings, so changing the username
    /// while omitting the password keeps a consistent credential pair.
    pub fn update_settings(
        &self,
        mut settings: Settings,
        password: Option<&str>,
    ) -> Result<(), String> {
        normalize(&mut settings)?;
        if let Some(password) = password {
            settings.password = password.to_string();
        } else {
            settings.password = self.settings.lock().unwrap().password.clone();
        }
        validate_settings(&settings)?;
        self.save(&settings)?;
        *self.settings.lock().unwrap() = settings;
        Ok(())
    }

    /// Reset the channel to its defaults (disabled, no host or credentials).
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

    /// Send a test email. Requires the channel to be enabled but bypasses event
    /// routing and the severity floor, so the transport can be verified before
    /// events are routed to it.
    pub async fn send_test(&self) -> Result<(), String> {
        let settings = self.get_settings();
        if !settings.enabled {
            return Err("smtp channel is disabled".to_string());
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
        // A hand-edited settings file may have bypassed save-time validation;
        // never send with an unknown TLS mode or an invalid credential pair.
        validate_fields(settings)?;
        // DNS resolution is inside the timeout too, so a stalled resolver
        // cannot delay the joined notification dispatch without bound.
        let fut = async {
            let host = crate::alert_transport::normalize_host(&settings.host)?;
            if host.is_empty() {
                return Err("SMTP host is empty".to_string());
            }
            let addrs = crate::alert_transport::resolve_target(&host, settings.port).await?;
            session(settings, &host, &addrs, event, notif).await
        };
        tokio::time::timeout(SMTP_TIMEOUT, fut)
            .await
            .map_err(|_| "the SMTP request timed out".to_string())?
    }
}

async fn session(
    settings: &Settings,
    host: &str,
    addrs: &[std::net::SocketAddr],
    event: &str,
    notif: &Notification,
) -> Result<(), String> {
    let connect_error = || "could not connect to the SMTP server".to_string();
    match settings.tls.as_str() {
        "implicit" => {
            let tcp = tokio::net::TcpStream::connect(addrs)
                .await
                .map_err(|_| connect_error())?;
            let tls = crate::alert_transport::tls_connect(tcp, host).await?;
            conversation(settings, tls, event, notif, true).await
        }
        "starttls" => {
            starttls_session(
                settings,
                host,
                addrs,
                event,
                notif,
                |tcp, host| async move { crate::alert_transport::tls_connect(tcp, &host).await },
            )
            .await
        }
        "none" => {
            let tcp = tokio::net::TcpStream::connect(addrs)
                .await
                .map_err(|_| connect_error())?;
            conversation(settings, tcp, event, notif, true).await
        }
        _ => Err("unknown SMTP TLS mode".to_string()),
    }
}

/// The plaintext STARTTLS phase plus the post-upgrade EHLO/AUTH/DATA flow.
///
/// `upgrade` is the TLS handshake. It is injectable so the protocol sequence
/// (notably: no second greeting after STARTTLS) can be tested without a real
/// certificate.
async fn starttls_session<S, F, Fut>(
    settings: &Settings,
    host: &str,
    addrs: &[std::net::SocketAddr],
    event: &str,
    notif: &Notification,
    upgrade: F,
) -> Result<(), String>
where
    F: FnOnce(tokio::net::TcpStream, String) -> Fut,
    Fut: std::future::Future<Output = Result<S, String>>,
    S: AsyncRead + AsyncWrite + Unpin,
{
    let tcp = tokio::net::TcpStream::connect(addrs)
        .await
        .map_err(|_| "could not connect to the SMTP server".to_string())?;
    let mut io = BufReader::new(tcp);
    read_greeting(&mut io).await?;
    let caps = ehlo(&mut io).await?;
    if !caps
        .iter()
        .any(|line| first_token(line).as_deref() == Some("STARTTLS"))
    {
        return Err("the SMTP server does not offer STARTTLS".to_string());
    }
    command_expect(
        &mut io,
        "STARTTLS",
        &[220],
        "the SMTP server refused STARTTLS",
    )
    .await?;
    // The 220 reply was read completely, so nothing is left buffered.
    let tcp = io.into_inner();
    let tls = upgrade(tcp, host.to_string()).await?;
    // STARTTLS replaces the greeting: the server waits for EHLO, it does not
    // send another 220. Reading one here would hang every default-mode send.
    conversation(settings, tls, event, notif, false).await
}

async fn conversation<S>(
    settings: &Settings,
    stream: S,
    event: &str,
    notif: &Notification,
    greeting: bool,
) -> Result<(), String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut io = BufReader::new(stream);
    if greeting {
        read_greeting(&mut io).await?;
    }
    let caps = ehlo(&mut io).await?;
    authenticate(&mut io, settings, &caps).await?;
    send_message(&mut io, settings, event, notif).await?;
    // Best-effort logout: the message has already been accepted.
    if command(&mut io, "QUIT").await.is_ok() {
        let _ = read_reply(&mut io).await;
    }
    Ok(())
}

async fn read_greeting<R: AsyncBufRead + Unpin>(io: &mut R) -> Result<(), String> {
    let reply = read_reply(io).await?;
    if reply.code != 220 {
        return Err(format!(
            "the SMTP server refused the connection (SMTP code {})",
            reply.code
        ));
    }
    Ok(())
}

/// The first whitespace-delimited token of an EHLO capability line, uppercased
/// and with an `AUTH=PLAIN`-style `=` normalized to a space.
fn first_token(line: &str) -> Option<String> {
    line.to_ascii_uppercase()
        .replace('=', " ")
        .split_whitespace()
        .next()
        .map(|s| s.to_string())
}

fn capability_tokens(caps: &[String], name: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in caps {
        let normalized = line.to_ascii_uppercase().replace('=', " ");
        let mut parts = normalized.split_whitespace();
        if parts.next() == Some(name) {
            out.extend(parts.map(|s| s.to_string()));
        }
    }
    out
}

async fn ehlo<S>(io: &mut S) -> Result<Vec<String>, String>
where
    S: AsyncBufRead + AsyncWrite + Unpin,
{
    let reply = command_expect(io, "EHLO naslos", &[250], "the SMTP server refused EHLO").await?;
    Ok(reply.lines)
}

async fn authenticate<S>(io: &mut S, settings: &Settings, caps: &[String]) -> Result<(), String>
where
    S: AsyncBufRead + AsyncWrite + Unpin,
{
    use base64::Engine;
    // AUTH is used only when both halves of the credential pair are set; a
    // stale username after clearing the password means "no authentication".
    if settings.username.is_empty() || settings.password.is_empty() {
        return Ok(());
    }
    let mechanisms = capability_tokens(caps, "AUTH");
    let engine = base64::engine::general_purpose::STANDARD;
    if mechanisms.iter().any(|m| m == "PLAIN") {
        let mut token = Vec::new();
        token.push(0);
        token.extend_from_slice(settings.username.as_bytes());
        token.push(0);
        token.extend_from_slice(settings.password.as_bytes());
        let line = format!("AUTH PLAIN {}", engine.encode(token));
        command_expect(io, &line, &[235], "SMTP authentication failed").await?;
        return Ok(());
    }
    if mechanisms.iter().any(|m| m == "LOGIN") {
        command_expect(io, "AUTH LOGIN", &[334], "SMTP authentication failed").await?;
        command_expect(
            io,
            &engine.encode(settings.username.as_bytes()),
            &[334],
            "SMTP authentication failed",
        )
        .await?;
        command_expect(
            io,
            &engine.encode(settings.password.as_bytes()),
            &[235],
            "SMTP authentication failed",
        )
        .await?;
        return Ok(());
    }
    Err("the SMTP server does not offer a supported AUTH mechanism".to_string())
}

async fn send_message<S>(
    io: &mut S,
    settings: &Settings,
    event: &str,
    notif: &Notification,
) -> Result<(), String>
where
    S: AsyncBufRead + AsyncWrite + Unpin,
{
    command_expect(
        io,
        &format!("MAIL FROM:<{}>", settings.from.trim()),
        &[250],
        "the SMTP server refused the sender",
    )
    .await?;
    for recipient in &settings.to {
        command_expect(
            io,
            &format!("RCPT TO:<{}>", recipient.trim()),
            &[250, 251],
            "the SMTP server refused a recipient",
        )
        .await?;
    }
    command_expect(io, "DATA", &[354], "the SMTP server refused the message").await?;
    let body = dot_stuff(&render_message(settings, event, notif));
    io.write_all(body.as_bytes())
        .await
        .map_err(|_| "could not send the message body to the SMTP server".to_string())?;
    io.write_all(b".\r\n")
        .await
        .map_err(|_| "could not send the message body to the SMTP server".to_string())?;
    io.flush()
        .await
        .map_err(|_| "could not send the message body to the SMTP server".to_string())?;
    let reply = read_reply(io).await?;
    if reply.code != 250 {
        return Err(format!(
            "the SMTP server rejected the message (SMTP code {})",
            reply.code
        ));
    }
    Ok(())
}

#[derive(Debug)]
struct Reply {
    code: u16,
    lines: Vec<String>,
}

/// Read one SMTP reply, including a multiline `250-…` block. The reply text is
/// kept for capability parsing only; it is never included in an error.
async fn read_reply<R: AsyncBufRead + Unpin>(io: &mut R) -> Result<Reply, String> {
    let mut lines = Vec::new();
    let mut expected_code = None;
    let mut total_bytes = 0usize;
    loop {
        let raw = read_line_bounded(io, MAX_REPLY_LINE)
            .await?
            .ok_or_else(|| "the SMTP server closed the connection".to_string())?;
        total_bytes = total_bytes.saturating_add(raw.len());
        if total_bytes > MAX_REPLY_BYTES {
            return Err("the SMTP reply was too long".to_string());
        }
        if lines.len() >= MAX_REPLY_LINES {
            return Err("the SMTP reply had too many lines".to_string());
        }
        let line = raw.trim_end_matches(['\r', '\n']);
        let bytes = line.as_bytes();
        if bytes.len() < 3 || !bytes[..3].iter().all(|b| b.is_ascii_digit()) {
            return Err("the SMTP server sent a malformed reply".to_string());
        }
        let code = line[..3]
            .parse::<u16>()
            .map_err(|_| "the SMTP server sent a malformed reply".to_string())?;
        match expected_code {
            Some(expected) if expected != code => {
                return Err("the SMTP server sent a malformed reply".to_string());
            }
            None => expected_code = Some(code),
            _ => {}
        }
        // `get` returns None when byte 4 is not a char boundary (for example a
        // multi-byte character in the separator position), so this can never
        // panic before the separator is validated.
        let text = line.get(4..).unwrap_or_default().to_string();
        match bytes.get(3).copied() {
            Some(b'-') => lines.push(text),
            Some(b' ') | None => {
                lines.push(text);
                return Ok(Reply { code, lines });
            }
            Some(_) => return Err("the SMTP server sent a malformed reply".to_string()),
        }
    }
}

async fn read_line_bounded<R: AsyncBufRead + Unpin>(
    io: &mut R,
    max: usize,
) -> Result<Option<String>, String> {
    let mut out = Vec::new();
    loop {
        let available = io
            .fill_buf()
            .await
            .map_err(|_| "the SMTP connection failed".to_string())?;
        if available.is_empty() {
            if out.is_empty() {
                return Ok(None);
            }
            return Err("the SMTP server closed the connection mid-reply".to_string());
        }
        let newline = available.iter().position(|b| *b == b'\n');
        let take = newline.map_or(available.len(), |p| p + 1);
        if out.len() + take > max {
            return Err("the SMTP reply was too long".to_string());
        }
        out.extend_from_slice(&available[..take]);
        io.consume(take);
        if newline.is_some() {
            let text = String::from_utf8(out)
                .map_err(|_| "the SMTP reply was not valid UTF-8".to_string())?;
            return Ok(Some(text));
        }
    }
}

async fn command<W: AsyncWrite + Unpin>(io: &mut W, line: &str) -> Result<(), String> {
    if line.contains(['\r', '\n']) {
        return Err("the SMTP command contains an invalid character".to_string());
    }
    io.write_all(line.as_bytes())
        .await
        .map_err(|_| "could not send to the SMTP server".to_string())?;
    io.write_all(b"\r\n")
        .await
        .map_err(|_| "could not send to the SMTP server".to_string())?;
    io.flush()
        .await
        .map_err(|_| "could not send to the SMTP server".to_string())?;
    Ok(())
}

async fn command_expect<S>(
    io: &mut S,
    line: &str,
    codes: &[u16],
    failure: &str,
) -> Result<Reply, String>
where
    S: AsyncBufRead + AsyncWrite + Unpin,
{
    command(io, line).await?;
    let reply = read_reply(io).await?;
    if !codes.contains(&reply.code) {
        return Err(format!("{failure} (SMTP code {})", reply.code));
    }
    Ok(reply)
}

/// Validate the envelope sender/recipient form accepted by this channel:
/// a bare mailbox (`user@example.com`), never a display name or header.
pub fn valid_mailbox(raw: &str) -> bool {
    let addr = raw.trim();
    if addr.is_empty() || addr.len() > 320 {
        return false;
    }
    if addr.chars().any(|c| {
        c.is_control() || c.is_whitespace() || matches!(c, '<' | '>' | ',' | ';' | '"' | '\\' | ':')
    }) {
        return false;
    }
    let mut parts = addr.split('@');
    let local = parts.next().unwrap_or_default();
    let domain = parts.next().unwrap_or_default();
    parts.next().is_none() && !local.is_empty() && !domain.is_empty()
}

/// Render the RFC 5322 message (CRLF line endings, no envelope data).
pub fn render_message(settings: &Settings, event: &str, notif: &Notification) -> String {
    let subject = sanitize_header(&notif.title, 180);
    let event = sanitize_token(event, 64);
    let body = normalize_body(&notif.message, MAX_MESSAGE_BYTES / 2);
    let recipients = settings
        .to
        .iter()
        .map(|r| r.trim())
        .collect::<Vec<_>>()
        .join(", ");
    let mut out = String::new();
    out.push_str(&format!("From: {}\r\n", settings.from.trim()));
    out.push_str(&format!("To: {recipients}\r\n"));
    out.push_str(&format!("Subject: [Naslos] {subject}\r\n"));
    out.push_str(&format!("X-Naslos-Event: {event}\r\n"));
    out.push_str("Date: ");
    out.push_str(&chrono::Utc::now().to_rfc2822());
    out.push_str(
        "\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n",
    );
    out.push_str(&body);
    if !out.ends_with("\r\n") {
        out.push_str("\r\n");
    }
    out
}

/// SMTP dot-stuffing: a body line that starts with `.` gets one more `.`, and
/// the DATA payload always ends with CRLF so the terminator is unambiguous.
pub fn dot_stuff(message: &str) -> String {
    let mut out = String::with_capacity(message.len() + 8);
    let mut line_start = true;
    for c in message.chars() {
        if line_start && c == '.' {
            out.push('.');
        }
        out.push(c);
        line_start = c == '\n';
    }
    if !out.ends_with("\r\n") {
        out.push_str("\r\n");
    }
    out
}

fn sanitize_header(value: &str, max: usize) -> String {
    let mut out = String::new();
    let mut count = 0usize;
    let mut last_space = false;
    for c in value.chars() {
        let c = match c {
            '\r' | '\n' | '\t' => ' ',
            _ => c,
        };
        if c.is_control() {
            continue;
        }
        if c == ' ' {
            if last_space {
                continue;
            }
            last_space = true;
        } else {
            last_space = false;
        }
        out.push(c);
        count += 1;
        if count >= max {
            break;
        }
    }
    out.trim().to_string()
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

fn normalize_body(value: &str, max_bytes: usize) -> String {
    let unified = value.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::new();
    for line in unified.split('\n') {
        let remaining = max_bytes.saturating_sub(out.len());
        if remaining < 2 {
            break;
        }
        let clean: String = line
            .chars()
            .filter(|c| !c.is_control() || *c == '\t')
            .collect();
        out.push_str(&truncate_utf8(&clean, remaining - 2));
        out.push_str("\r\n");
    }
    if out.is_empty() {
        out.push_str("\r\n");
    }
    out
}

fn truncate_utf8(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_string();
    }
    let mut end = max;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notifications::{
        Notification, EVENT_DISK_FAILURE, EVENT_ZFS_HEALTH, SEVERITY_CRITICAL, SEVERITY_ERROR,
        SEVERITY_WARNING,
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
            from: "naslos@example.com".to_string(),
            to: vec!["ops@example.com".to_string()],
            ..Default::default()
        }
    }

    #[test]
    fn target_policy_allows_lan_relays_and_public_hosts_but_not_special_addresses() {
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
    fn host_fields_cannot_embed_credentials_or_a_second_target() {
        for bad in [
            "user:pass@relay.lan",
            "relay.lan:587",
            "relay.lan/path",
            "relay lan",
        ] {
            assert!(validate_settings(&enabled(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn tls_modes_and_credentials_are_validated() {
        let mut s = enabled("192.168.1.10");
        s.username = "relay".to_string();
        s.password = "secret".to_string();
        assert!(validate_settings(&s).is_ok());

        s.tls = "none".to_string();
        assert!(validate_settings(&s).is_err());

        s.tls = "starttls".to_string();
        s.password.clear();
        assert!(validate_settings(&s).is_ok());
        s.username.clear();
        s.password = "secret".to_string();
        assert!(validate_settings(&s).is_err());

        let mut bad = enabled("192.168.1.10");
        bad.tls = "sometimes".to_string();
        assert!(validate_settings(&bad).is_err());
    }

    #[test]
    fn enabled_channel_needs_a_sender_and_recipients() {
        let mut s = enabled("192.168.1.10");
        s.from.clear();
        assert!(validate_settings(&s).is_err());
        let mut s = enabled("192.168.1.10");
        s.to.clear();
        assert!(validate_settings(&s).is_err());
        let mut s = enabled("192.168.1.10");
        s.to = vec!["not a mailbox".to_string()];
        assert!(validate_settings(&s).is_err());
    }

    #[test]
    fn mailbox_validation_rejects_header_injection() {
        assert!(valid_mailbox("ops@example.com"));
        assert!(valid_mailbox(" naslos@example.com "));
        for bad in [
            "",
            "ops",
            "ops@",
            "@example.com",
            "ops@example.com\r\nBcc: evil@example.com",
            "Ops <ops@example.com>",
            "ops@example.com, evil@example.com",
            "ops@exa mple.com",
        ] {
            assert!(!valid_mailbox(bad), "{bad:?}");
        }
    }

    #[test]
    fn message_shape_is_rfc5322_and_subject_is_sanitized() {
        let settings = enabled("192.168.1.10");
        let message = render_message(
            &settings,
            EVENT_DISK_FAILURE,
            &Notification {
                title: "Disk sda is at 60°C\r\nBcc: evil@example.com".to_string(),
                ..notif()
            },
        );
        assert!(message.starts_with("From: naslos@example.com\r\n"));
        assert!(message.contains("To: ops@example.com\r\n"));
        assert!(message.contains("Subject: [Naslos] Disk sda is at 60°C Bcc: evil@example.com\r\n"));
        assert!(message.contains("X-Naslos-Event: disk_failure\r\n"));
        assert!(message.contains("MIME-Version: 1.0\r\n"));
        assert!(message.contains("\r\n\r\nDisk /dev/sda"));
        assert!(message.ends_with("\r\n"));
        assert!(!message.contains("\r\nBcc: evil@example.com\r\n"));
        assert!(!message.replace("\r\n", "").contains('\n'));
    }

    #[test]
    fn dot_stuffing_protects_the_data_terminator() {
        let stuffed = dot_stuff("Subject: x\r\n\r\n.hidden\r\n.\r\n");
        assert!(stuffed.contains("\r\n..hidden\r\n"));
        assert!(stuffed.contains("\r\n..\r\n"));
        assert!(stuffed.ends_with("\r\n"));
    }

    #[test]
    fn message_is_bounded_even_for_a_huge_body() {
        let mut settings = enabled("192.168.1.10");
        settings.to = (0..MAX_RECIPIENTS)
            .map(|i| format!("ops{i}@example.com"))
            .collect();
        let message = render_message(
            &settings,
            EVENT_DISK_FAILURE,
            &Notification {
                title: "t".to_string(),
                message: "x".repeat(MAX_MESSAGE_BYTES * 2),
                severity: SEVERITY_ERROR.to_string(),
                tags: Vec::new(),
            },
        );
        assert!(message.len() <= MAX_MESSAGE_BYTES);
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

    #[test]
    fn multiline_replies_are_parsed_without_the_text_reaching_errors() {
        let raw = b"250-naslos.example\r\n250-STARTTLS\r\n250 AUTH PLAIN LOGIN\r\n";
        let mut io = BufReader::new(&raw[..]);
        let reply = futures::executor::block_on(read_reply(&mut io)).unwrap();
        assert_eq!(reply.code, 250);
        assert_eq!(reply.lines.len(), 3);
        assert_eq!(capability_tokens(&reply.lines, "AUTH"), ["PLAIN", "LOGIN"]);
        assert!(capability_tokens(&reply.lines, "STARTTLS").is_empty());
        assert_eq!(first_token(&reply.lines[1]).as_deref(), Some("STARTTLS"));
    }

    #[test]
    fn a_multibyte_separator_is_rejected_without_panicking() {
        // Byte 3 starts "é", so byte 4 is not a char boundary. The old
        // `line[4..]` slice panicked here before the separator was checked.
        let raw = "250é\r\n".as_bytes();
        let mut io = BufReader::new(raw);
        let error = futures::executor::block_on(read_reply(&mut io)).unwrap_err();
        assert!(error.contains("malformed"), "{error}");
    }

    #[test]
    fn malformed_replies_are_rejected() {
        for raw in [
            &b"garbage\r\n"[..],
            &b"25\r\n"[..],
            &b"250?\r\n"[..],
            &b"250-ok\r\n500 no\r\n"[..],
        ] {
            let mut io = BufReader::new(raw);
            assert!(
                futures::executor::block_on(read_reply(&mut io)).is_err(),
                "{:?}",
                String::from_utf8_lossy(raw)
            );
        }
    }

    #[test]
    fn a_reply_line_is_bounded() {
        let raw = vec![b'x'; MAX_REPLY_LINE + 2];
        let mut io = BufReader::new(&raw[..]);
        assert!(futures::executor::block_on(read_reply(&mut io)).is_err());
    }

    #[test]
    fn a_multiline_reply_is_bounded_by_lines_and_bytes() {
        let mut too_many = Vec::new();
        for _ in 0..=MAX_REPLY_LINES {
            too_many.extend_from_slice(b"250-x\r\n");
        }
        let mut io = BufReader::new(&too_many[..]);
        assert!(futures::executor::block_on(read_reply(&mut io)).is_err());

        let mut too_large = Vec::new();
        let payload = vec![b'x'; 1024];
        for _ in 0..(MAX_REPLY_BYTES / 1024 + 1) {
            too_large.extend_from_slice(b"250-");
            too_large.extend_from_slice(&payload);
            too_large.extend_from_slice(b"\r\n");
        }
        let mut io = BufReader::new(&too_large[..]);
        assert!(futures::executor::block_on(read_reply(&mut io)).is_err());
    }

    /// Read one CRLF-terminated command line in the fake SMTP server.
    async fn read_command<R: AsyncBufRead + Unpin>(reader: &mut R) -> String {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        line.trim_end_matches(['\r', '\n']).to_string()
    }

    /// A scripted plaintext peer for the STARTTLS regression test. It sends
    /// the greeting, advertises STARTTLS, then waits for EHLO after the
    /// (identity) upgrade - a server never sends a second greeting there.
    async fn spawn_starttls_server() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = socket.into_split();
            let mut reader = BufReader::new(read_half);
            write_half.write_all(b"220 test ESMTP\r\n").await.unwrap();
            assert_eq!(read_command(&mut reader).await, "EHLO naslos");
            write_half
                .write_all(b"250-test\r\n250-STARTTLS\r\n250 AUTH PLAIN\r\n")
                .await
                .unwrap();
            assert_eq!(read_command(&mut reader).await, "STARTTLS");
            write_half.write_all(b"220 go ahead\r\n").await.unwrap();
            // This is the regression assertion: the next client command must
            // be EHLO, not a blocked read for a second 220 greeting.
            assert_eq!(read_command(&mut reader).await, "EHLO naslos");
            write_half
                .write_all(b"250-test\r\n250 AUTH PLAIN\r\n")
                .await
                .unwrap();
            assert!(read_command(&mut reader).await.starts_with("MAIL FROM:<"));
            write_half.write_all(b"250 ok\r\n").await.unwrap();
            assert!(read_command(&mut reader).await.starts_with("RCPT TO:<"));
            write_half.write_all(b"250 ok\r\n").await.unwrap();
            assert_eq!(read_command(&mut reader).await, "DATA");
            write_half.write_all(b"354 go\r\n").await.unwrap();
            loop {
                if read_command(&mut reader).await == "." {
                    break;
                }
            }
            write_half.write_all(b"250 queued\r\n").await.unwrap();
            assert_eq!(read_command(&mut reader).await, "QUIT");
            write_half.write_all(b"221 bye\r\n").await.unwrap();
        });
        (addr, task)
    }

    #[tokio::test]
    async fn starttls_does_not_wait_for_a_second_greeting() {
        let (addr, server) = spawn_starttls_server().await;
        let settings = enabled("192.168.1.10");
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            starttls_session(
                &settings,
                "127.0.0.1",
                &[addr],
                EVENT_DISK_FAILURE,
                &notif(),
                |tcp, _host| async move { Ok::<_, String>(tcp) },
            ),
        )
        .await
        .expect("STARTTLS session timed out waiting for a second greeting");
        outcome.unwrap();
        server.await.unwrap();
    }

    fn temp_manager(settings: Option<&Settings>) -> (tempfile::TempDir, Manager) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("smtp.json");
        if let Some(settings) = settings {
            std::fs::write(&path, serde_json::to_vec(settings).unwrap()).unwrap();
        }
        let manager = Manager::new(path.to_str().unwrap());
        (dir, manager)
    }

    #[test]
    fn settings_survive_a_reload_and_the_password_stays_write_only() {
        let (dir, manager) = temp_manager(None);
        let mut settings = enabled("192.168.1.10");
        settings.username = "relay".to_string();
        manager
            .update_settings(settings, Some("s3cret-pass"))
            .unwrap();

        let path = dir.path().join("smtp.json");
        let reloaded = Manager::new(path.to_str().unwrap());
        let stored = reloaded.get_settings();
        assert!(stored.enabled);
        assert_eq!(stored.password, "s3cret-pass");
        assert!(!format!("{stored:?}").contains("s3cret-pass"));

        // `None` keeps the password, `Some("")` clears it.
        reloaded.update_settings(stored.clone(), None).unwrap();
        assert_eq!(reloaded.get_settings().password, "s3cret-pass");
        reloaded.update_settings(stored, Some("")).unwrap();
        assert_eq!(reloaded.get_settings().password, "");
    }

    #[test]
    fn update_settings_rejects_invalid_targets_and_keeps_the_previous_ones() {
        let (_dir, manager) = temp_manager(None);
        let mut good = enabled("192.168.1.10");
        good.username = "relay".to_string();
        manager.update_settings(good, Some("pass")).unwrap();
        let mut bad = enabled("127.0.0.1");
        bad.password = String::new();
        assert!(manager.update_settings(bad, None).is_err());
        assert_eq!(manager.get_settings().host, "192.168.1.10");

        let mut missing = enabled("192.168.1.10");
        missing.from.clear();
        missing.password = String::new();
        assert!(manager.update_settings(missing, None).is_err());
    }

    #[test]
    fn clear_resets_the_channel() {
        let (_dir, manager) = temp_manager(None);
        let mut settings = enabled("192.168.1.10");
        settings.username = "relay".to_string();
        manager.update_settings(settings, Some("pass")).unwrap();
        manager.clear().unwrap();
        let settings = manager.get_settings();
        assert!(!settings.enabled);
        assert!(settings.host.is_empty());
        assert!(settings.password.is_empty());
        assert_eq!(settings.port, 587);
        assert_eq!(settings.tls, "starttls");
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
    async fn send_path_refuses_an_unknown_tls_mode_from_a_hand_edited_file() {
        let settings = Settings {
            enabled: true,
            host: "192.168.1.10".to_string(),
            tls: "ssl".to_string(),
            from: "naslos@example.com".to_string(),
            to: vec!["ops@example.com".to_string()],
            ..Default::default()
        };
        let (_dir, manager) = temp_manager(Some(&settings));
        let error = manager.send_test().await.unwrap_err();
        assert!(error.contains("TLS mode"), "{error}");
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
