//! LDAP-based user and group management (port of `api/internal/identity`).
//!
//! All password changes flow through `set_password` to keep LDAP and SMB in
//! sync. The connection is established lazily and re-established after failure,
//! so the API tolerates LDAP being unavailable at startup and LDAP restarts at
//! runtime.

use ldap3::exop::PasswordModify;
use ldap3::{Ldap, LdapConnAsync, LdapConnSettings, Mod, Scope, SearchEntry};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Bounds a single dial+bind attempt.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Throttles reconnect attempts so a burst of requests does not each block on a
/// full dial timeout while LDAP is down.
const CONNECT_COOLDOWN: Duration = Duration::from_secs(2);

/// LDAP connection configuration.
///
/// `Debug` is manual: the struct holds the service-account password, and a
/// derived `Debug` turns any `{:?}` in a log line or assertion into a leak.
#[derive(Clone, Default)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub base_dn: String,
    pub bind_dn: String,
    pub bind_pass: String,
    pub ca_cert_path: String,
    pub use_tls: bool,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("base_dn", &self.base_dn)
            .field("bind_dn", &self.bind_dn)
            .field("bind_pass", &"<redacted>")
            .field("ca_cert_path", &self.ca_cert_path)
            .field("use_tls", &self.use_tls)
            .finish()
    }
}

/// A user account.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Person {
    pub dn: String,
    pub uid: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub email: String,
    #[serde(rename = "firstName")]
    pub first_name: String,
    #[serde(rename = "lastName")]
    pub last_name: String,
    pub enabled: bool,
    pub groups: Vec<String>,
}

/// A group.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Group {
    pub dn: String,
    pub cn: String,
    pub description: String,
    pub members: Vec<String>,
}

/// `^[a-z0-9][a-z0-9._-]{0,63}$`.
pub fn is_valid_identity_name(value: &str) -> bool {
    let b = value.as_bytes();
    if b.is_empty() || b.len() > 64 {
        return false;
    }
    if !b[0].is_ascii_lowercase() && !b[0].is_ascii_digit() {
        return false;
    }
    b.iter()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-'))
}

/// Reject a uid/cn outside the allowlist (NAS-006).
pub fn validate_identity_name(kind: &str, value: &str) -> Result<(), String> {
    if !is_valid_identity_name(value) {
        return Err(format!(
            "invalid {kind} {value:?}: use lowercase letters, digits, '.', '_' or '-', starting with a letter or digit, at most 64 characters"
        ));
    }
    Ok(())
}

pub fn normalize_uid(uid: &str) -> String {
    uid.trim().to_lowercase()
}

/// Canonical group name: lowercase, trimmed, allowlisted.
pub fn normalize_group_name(cn: &str) -> Result<String, String> {
    let normalized = cn.trim().to_lowercase();
    validate_identity_name("group name", &normalized)?;
    Ok(normalized)
}

/// RFC 4514 DN-value escaping. The allowlist already excludes the
/// metacharacters, so this is defense in depth.
pub fn escape_dn_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for (i, ch) in value.chars().enumerate() {
        let escape = matches!(ch, ',' | '+' | '"' | '\\' | '<' | '>' | ';' | '=')
            || (i == 0 && (ch == ' ' || ch == '#'))
            || (i == value.chars().count() - 1 && ch == ' ');
        if escape {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// RFC 4515 filter-value escaping.
pub fn escape_filter(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '*' => out.push_str("\\2a"),
            '(' => out.push_str("\\28"),
            ')' => out.push_str("\\29"),
            '\\' => out.push_str("\\5c"),
            '\0' => out.push_str("\\00"),
            c => out.push(c),
        }
    }
    out
}

/// Compute the NT hash (MD4 of UTF-16LE password) for SMB. The MD4
/// implementation is required by the protocol and is not used for security.
pub fn compute_nt_hash(password: &str) -> String {
    let utf16: Vec<u16> = password.encode_utf16().collect();
    let mut bytes = Vec::with_capacity(utf16.len() * 2);
    for unit in &utf16 {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    let digest = md4(&bytes);
    digest.iter().map(|b| format!("{b:02X}")).collect()
}

/// Non-empty common name for a person entry.
pub fn person_cn(display_name: &str, first_name: &str, last_name: &str, uid: &str) -> String {
    let cn = display_name.trim();
    if !cn.is_empty() {
        return cn.to_string();
    }
    let combined = format!("{} {}", first_name.trim(), last_name.trim())
        .trim()
        .to_string();
    if !combined.is_empty() {
        return combined;
    }
    uid.to_string()
}

/// Reduce a list of DNs/CNs to short names.
pub fn short_names(values: &[String]) -> Vec<String> {
    let mut result = Vec::with_capacity(values.len());
    for v in values {
        if v.is_empty() {
            continue;
        }
        let lower = v.to_lowercase();
        if let Some(rest) = lower.strip_prefix("cn=") {
            result.push(rest.split(',').next().unwrap_or("").to_string());
        } else if let Some(rest) = lower.strip_prefix("uid=") {
            result.push(rest.split(',').next().unwrap_or("").to_string());
        } else {
            result.push(lower);
        }
    }
    result
}

/// Whether the account is not expired (`shadowExpire`).
///
/// `shadowExpire` is days since the Unix epoch; `-1`/empty means "never", and
/// `0`/a past date (the disable path writes `1`) means disabled.
pub fn is_person_enabled(shadow_expire: &str) -> bool {
    let value = shadow_expire.trim();
    if value.is_empty() || value == "-1" {
        return true;
    }
    let Ok(expire) = value.parse::<i64>() else {
        // An unparsable value is safer treated as expired.
        return false;
    };
    let today = chrono::Utc::now().timestamp() / 86_400;
    expire > today
}

/// An LDAP operation, so the retry-once logic lives in one place.
enum Op {
    Search {
        base: String,
        filter: String,
        attrs: Vec<String>,
    },
    Add {
        dn: String,
        attrs: Vec<(String, HashSet<String>)>,
    },
    Modify {
        dn: String,
        mods: Vec<Mod<String>>,
    },
    Delete {
        dn: String,
    },
    PasswordModify {
        dn: String,
        password: String,
    },
}

/// LDAP client for identity operations.
pub struct Client {
    conn: Mutex<Option<Ldap>>,
    addr: String,
    base_dn: String,
    bind_dn: String,
    bind_pass: String,
    tls: Option<Arc<rustls::ClientConfig>>,
    last_err: Mutex<Option<(Instant, String)>>,
    /// Cached readiness probe: `/api/ready` is public, and an LDAP search per
    /// request would serialize on the single connection under the mutex.
    ready_cache: tokio::sync::Mutex<Option<(Instant, Result<(), String>)>>,
    /// Serializes uidNumber allocation (search max + add) so two concurrent
    /// creates cannot pick the same POSIX uid.
    user_alloc: tokio::sync::Mutex<()>,
}

/// Resolve the service-account password **from a file only**.
///
/// The secret must not live in the environment: the API forks
/// `helm`/`kubectl`/`talosctl`/`git` and any child would inherit it
/// (SEC-2026-F16). There is deliberately no inline fallback — an earlier one
/// silently defeated that control.
fn resolve_bind_pass(file: Option<&str>) -> Result<String, String> {
    let Some(path) = file.map(str::trim).filter(|p| !p.is_empty()) else {
        if std::env::var("LDAP_BIND_PASS")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            tracing::warn!(
                "LDAP_BIND_PASS is set but ignored; mount the password and set LDAP_BIND_PASS_FILE"
            );
        }
        return Err(
            "LDAP_BIND_PASS_FILE is required (mount the service-account password; an inline LDAP_BIND_PASS is not accepted)"
                .to_string(),
        );
    };
    std::fs::read_to_string(path)
        .map(|s| s.trim_end_matches(['\r', '\n']).to_string())
        .map_err(|e| format!("reading LDAP_BIND_PASS_FILE {path}: {e}"))
}

impl Client {
    /// Build a client; does not dial (lazy connection, NAS restart-race safe).
    pub fn new(cfg: &Config) -> Result<Self, String> {
        let tls = if cfg.use_tls {
            if cfg.ca_cert_path.trim().is_empty() {
                return Err(
                    "LDAP_CA_CERT is required when LDAP_USE_TLS=true; refusing to build a client that cannot verify the server"
                        .to_string(),
                );
            }
            Some(build_tls_config(&cfg.host, &cfg.ca_cert_path)?)
        } else {
            let loopback =
                cfg.host == "localhost" || cfg.host == "::1" || cfg.host.starts_with("127.");
            if !loopback {
                tracing::warn!(
                    "LDAP_USE_TLS=false for {}: the service-account password crosses the network in cleartext",
                    cfg.host
                );
            }
            None
        };
        Ok(Self {
            conn: Mutex::new(None),
            addr: format!("{}:{}", cfg.host, cfg.port),
            base_dn: cfg.base_dn.clone(),
            bind_dn: cfg.bind_dn.clone(),
            bind_pass: cfg.bind_pass.clone(),
            tls,
            last_err: Mutex::new(None),
            ready_cache: tokio::sync::Mutex::new(None),
            user_alloc: tokio::sync::Mutex::new(()),
        })
    }

    /// Build from the process env, fail-closed on a bad config.
    pub fn from_env() -> Result<Self, String> {
        let cfg = Config {
            host: std::env::var("LDAP_HOST").unwrap_or_else(|_| "naslos-openldap".into()),
            port: std::env::var("LDAP_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(636),
            base_dn: std::env::var("LDAP_BASE_DN").unwrap_or_else(|_| "dc=naslos,dc=local".into()),
            bind_dn: std::env::var("LDAP_BIND_DN")
                .unwrap_or_else(|_| "cn=naslos-service,ou=services,dc=naslos,dc=local".into()),
            bind_pass: resolve_bind_pass(std::env::var("LDAP_BIND_PASS_FILE").ok().as_deref())?,
            ca_cert_path: std::env::var("LDAP_CA_CERT").unwrap_or_default(),
            use_tls: std::env::var("LDAP_USE_TLS")
                .map(|v| v == "true")
                .unwrap_or(true),
        };
        Self::new(&cfg)
    }

    /// Cheap readiness probe: at most one LDAP round-trip every 15 s.
    pub async fn ready(&self) -> Result<(), String> {
        const TTL: std::time::Duration = std::time::Duration::from_secs(15);
        let mut cache = self.ready_cache.lock().await;
        if let Some((at, result)) = cache.as_ref() {
            if at.elapsed() < TTL {
                return result.clone();
            }
        }
        let result = self.list_groups().await.map(|_| ());
        *cache = Some((Instant::now(), result.clone()));
        result
    }

    fn people_base(&self) -> String {
        format!("ou=people,{}", self.base_dn)
    }

    fn groups_base(&self) -> String {
        format!("ou=groups,{}", self.base_dn)
    }

    fn person_dn(&self, uid: &str) -> String {
        format!("uid={},{}", escape_dn_component(uid), self.people_base())
    }

    fn group_dn(&self, cn: &str) -> String {
        format!("cn={},{}", escape_dn_component(cn), self.groups_base())
    }

    fn placeholder_member_dn(&self) -> String {
        format!("cn=empty-members,{}", self.groups_base())
    }

    /// Ensure a live, bound connection, throttled by the cooldown.
    async fn ensure(&self) -> Result<(), String> {
        let mut guard = self.conn.lock().await;
        if guard.is_some() {
            return Ok(());
        }
        {
            let last = self.last_err.lock().await;
            if let Some((at, err)) = last.as_ref() {
                if at.elapsed() < CONNECT_COOLDOWN {
                    return Err(err.clone());
                }
            }
        }
        match self.connect().await {
            Ok(ldap) => {
                *guard = Some(ldap);
                *self.last_err.lock().await = None;
                Ok(())
            }
            Err(e) => {
                *guard = None;
                *self.last_err.lock().await = Some((Instant::now(), e.clone()));
                Err(e)
            }
        }
    }

    async fn connect(&self) -> Result<Ldap, String> {
        let mut settings = LdapConnSettings::new().set_conn_timeout(CONNECT_TIMEOUT);
        let url = if let Some(cfg) = &self.tls {
            settings = settings.set_config(cfg.clone());
            format!("ldaps://{}", self.addr)
        } else {
            format!("ldap://{}", self.addr)
        };
        let (conn, mut ldap) = LdapConnAsync::with_settings(settings, &url)
            .await
            .map_err(|e| format!("connecting to LDAP {}: {e}", self.addr))?;
        tokio::spawn(async move {
            let _ = conn.drive().await;
        });
        ldap.simple_bind(&self.bind_dn, &self.bind_pass)
            .await
            .map_err(|e| format!("binding to LDAP {} as {}: {e}", self.addr, self.bind_dn))?
            .success()
            .map_err(|e| format!("binding to LDAP {} as {}: {e}", self.addr, self.bind_dn))?;
        Ok(ldap)
    }

    /// Run an operation, reconnecting and retrying once on a connection error.
    async fn run(&self, op: Op) -> Result<OpResult, String> {
        self.ensure().await?;
        match self.exec(&op).await {
            Ok(v) => Ok(v),
            Err(e) if is_connection_error(&e) => {
                *self.conn.lock().await = None;
                self.ensure().await?;
                self.exec(&op).await
            }
            Err(e) => Err(e),
        }
    }

    async fn exec(&self, op: &Op) -> Result<OpResult, String> {
        let mut guard = self.conn.lock().await;
        let ldap = guard.as_mut().ok_or("LDAP connection is unavailable")?;
        match op {
            Op::Search {
                base,
                filter,
                attrs,
            } => {
                let attr_refs: Vec<&str> = attrs.iter().map(|s| s.as_str()).collect();
                let (entries, _res) = ldap
                    .search(base, Scope::Subtree, filter, attr_refs)
                    .await
                    .map_err(|e| e.to_string())?
                    .success()
                    .map_err(|e| e.to_string())?;
                Ok(OpResult::Search(
                    entries.into_iter().map(SearchEntry::construct).collect(),
                ))
            }
            Op::Add { dn, attrs } => {
                ldap.add(dn, attrs.clone())
                    .await
                    .map_err(|e| e.to_string())?
                    .success()
                    .map_err(|e| e.to_string())?;
                Ok(OpResult::Done)
            }
            Op::Modify { dn, mods } => {
                ldap.modify(dn, mods.clone())
                    .await
                    .map_err(|e| e.to_string())?
                    .success()
                    .map_err(|e| e.to_string())?;
                Ok(OpResult::Done)
            }
            Op::Delete { dn } => {
                ldap.delete(dn)
                    .await
                    .map_err(|e| e.to_string())?
                    .success()
                    .map_err(|e| e.to_string())?;
                Ok(OpResult::Done)
            }
            Op::PasswordModify { dn, password } => {
                ldap.extended(PasswordModify {
                    user_id: Some(dn),
                    old_pass: None,
                    new_pass: Some(password),
                })
                .await
                .map_err(|e| e.to_string())?
                .success()
                .map_err(|e| e.to_string())?;
                Ok(OpResult::Done)
            }
        }
    }
}

enum OpResult {
    Search(Vec<SearchEntry>),
    Done,
}

impl OpResult {
    fn entries(self) -> Result<Vec<SearchEntry>, String> {
        match self {
            OpResult::Search(e) => Ok(e),
            OpResult::Done => Err("unexpected non-search result".to_string()),
        }
    }
}

fn attr(entry: &SearchEntry, name: &str) -> String {
    entry
        .attrs
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .and_then(|(_, v)| v.first())
        .cloned()
        .unwrap_or_default()
}

fn attrs(entry: &SearchEntry, name: &str) -> Vec<String> {
    entry
        .attrs
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.clone())
        .unwrap_or_default()
}

fn set1(value: &str) -> HashSet<String> {
    let mut s = HashSet::new();
    s.insert(value.to_string());
    s
}

/// Build a rustls client config that trusts only `ca_cert_path` (fail closed).
fn build_tls_config(
    server_name: &str,
    ca_cert_path: &str,
) -> Result<Arc<rustls::ClientConfig>, String> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut roots = rustls::RootCertStore::empty();
    if !ca_cert_path.is_empty() {
        let pem = std::fs::read(ca_cert_path)
            .map_err(|e| format!("reading CA cert {ca_cert_path}: {e}"))?;
        let certs = rustls_pemfile::certs(&mut std::io::BufReader::new(pem.as_slice()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("parsing CA certificate: {e}"))?;
        if certs.is_empty() {
            return Err("failed to parse CA certificate".to_string());
        }
        for cert in certs {
            roots
                .add(cert)
                .map_err(|e| format!("adding CA certificate: {e}"))?;
        }
    }
    let _ = server_name;
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// Whether an error means the connection itself failed (retryable).
pub fn is_connection_error(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    for fragment in [
        "connection reset",
        "broken pipe",
        "use of closed network connection",
        "response channel closed",
        "connection refused",
        "no such host",
        "ldap: connection closed",
        "connection closed",
        "unexpected eof",
        "eof",
    ] {
        if lower.contains(fragment) {
            return true;
        }
    }
    false
}

/// Minimal MD4 (RFC 1320), required for the NT hash (AUDIT-L7).
fn md4(data: &[u8]) -> [u8; 16] {
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    let (mut a, mut b, mut c, mut d): (u32, u32, u32, u32) =
        (0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476);

    for chunk in msg.chunks(64) {
        let mut x = [0u32; 16];
        for (i, word) in x.iter_mut().enumerate() {
            *word = u32::from_le_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }

        let (aa, bb, cc, dd) = (a, b, c, d);

        // Round 1
        let f = |x: u32, y: u32, z: u32| (x & y) | (!x & z);
        let r1 = |a: u32, b: u32, c: u32, d: u32, k: usize, s: u32| {
            a.wrapping_add(f(b, c, d)).wrapping_add(x[k]).rotate_left(s)
        };
        a = r1(a, b, c, d, 0, 3);
        d = r1(d, a, b, c, 1, 7);
        c = r1(c, d, a, b, 2, 11);
        b = r1(b, c, d, a, 3, 19);
        a = r1(a, b, c, d, 4, 3);
        d = r1(d, a, b, c, 5, 7);
        c = r1(c, d, a, b, 6, 11);
        b = r1(b, c, d, a, 7, 19);
        a = r1(a, b, c, d, 8, 3);
        d = r1(d, a, b, c, 9, 7);
        c = r1(c, d, a, b, 10, 11);
        b = r1(b, c, d, a, 11, 19);
        a = r1(a, b, c, d, 12, 3);
        d = r1(d, a, b, c, 13, 7);
        c = r1(c, d, a, b, 14, 11);
        b = r1(b, c, d, a, 15, 19);

        // Round 2
        let g = |x: u32, y: u32, z: u32| (x & y) | (x & z) | (y & z);
        let r2 = |a: u32, b: u32, c: u32, d: u32, k: usize, s: u32| {
            a.wrapping_add(g(b, c, d))
                .wrapping_add(x[k])
                .wrapping_add(0x5a827999)
                .rotate_left(s)
        };
        a = r2(a, b, c, d, 0, 3);
        d = r2(d, a, b, c, 4, 5);
        c = r2(c, d, a, b, 8, 9);
        b = r2(b, c, d, a, 12, 13);
        a = r2(a, b, c, d, 1, 3);
        d = r2(d, a, b, c, 5, 5);
        c = r2(c, d, a, b, 9, 9);
        b = r2(b, c, d, a, 13, 13);
        a = r2(a, b, c, d, 2, 3);
        d = r2(d, a, b, c, 6, 5);
        c = r2(c, d, a, b, 10, 9);
        b = r2(b, c, d, a, 14, 13);
        a = r2(a, b, c, d, 3, 3);
        d = r2(d, a, b, c, 7, 5);
        c = r2(c, d, a, b, 11, 9);
        b = r2(b, c, d, a, 15, 13);

        // Round 3
        let h = |x: u32, y: u32, z: u32| x ^ y ^ z;
        let r3 = |a: u32, b: u32, c: u32, d: u32, k: usize, s: u32| {
            a.wrapping_add(h(b, c, d))
                .wrapping_add(x[k])
                .wrapping_add(0x6ed9eba1)
                .rotate_left(s)
        };
        a = r3(a, b, c, d, 0, 3);
        d = r3(d, a, b, c, 8, 9);
        c = r3(c, d, a, b, 4, 11);
        b = r3(b, c, d, a, 12, 15);
        a = r3(a, b, c, d, 2, 3);
        d = r3(d, a, b, c, 10, 9);
        c = r3(c, d, a, b, 6, 11);
        b = r3(b, c, d, a, 14, 15);
        a = r3(a, b, c, d, 1, 3);
        d = r3(d, a, b, c, 9, 9);
        c = r3(c, d, a, b, 5, 11);
        b = r3(b, c, d, a, 13, 15);
        a = r3(a, b, c, d, 3, 3);
        d = r3(d, a, b, c, 11, 9);
        c = r3(c, d, a, b, 7, 11);
        b = r3(b, c, d, a, 15, 15);

        a = a.wrapping_add(aa);
        b = b.wrapping_add(bb);
        c = c.wrapping_add(cc);
        d = d.wrapping_add(dd);
    }

    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a.to_le_bytes());
    out[4..8].copy_from_slice(&b.to_le_bytes());
    out[8..12].copy_from_slice(&c.to_le_bytes());
    out[12..16].copy_from_slice(&d.to_le_bytes());
    out
}

// Person/group operations that share the Op machinery are in `identity_ops.rs`
// to keep this file readable.
mod ops;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nt_hash_matches_the_known_vector() {
        // The classic "password" NT hash (uppercase hex).
        assert_eq!(
            compute_nt_hash("password"),
            "8846F7EAEE8FB117AD06BDD830B7586C"
        );
    }

    #[test]
    fn the_bind_password_comes_from_a_file_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pw");
        std::fs::write(&path, "from-file\n").unwrap();
        assert_eq!(
            resolve_bind_pass(Some(path.to_str().unwrap())).unwrap(),
            "from-file"
        );
        // No inline fallback: an environment-only password must be refused.
        assert!(resolve_bind_pass(None).is_err());
        assert!(resolve_bind_pass(Some("  ")).is_err());
        assert!(resolve_bind_pass(Some("/nonexistent/naslos")).is_err());
    }

    #[test]
    fn config_debug_does_not_leak_the_password() {
        let cfg = Config {
            host: "ldap".into(),
            bind_pass: "hunter2".into(),
            ..Default::default()
        };
        let rendered = format!("{cfg:?}");
        assert!(!rendered.contains("hunter2"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
    }

    #[test]
    fn tls_without_a_ca_is_refused() {
        let cfg = Config {
            host: "naslos-openldap".into(),
            use_tls: true,
            ca_cert_path: String::new(),
            ..Default::default()
        };
        let err = match Client::new(&cfg) {
            Ok(_) => panic!("TLS without a CA must fail"),
            Err(e) => e,
        };
        assert!(err.contains("LDAP_CA_CERT"), "{err}");
    }

    #[test]
    fn shadow_expire_semantics() {
        assert!(is_person_enabled(""));
        assert!(is_person_enabled("-1"));
        // The disable path writes 1 (Jan 2 1970), and 0 is the classic lock.
        assert!(!is_person_enabled("0"));
        assert!(!is_person_enabled("1"));
        assert!(!is_person_enabled("not-a-number"));
        // A far-future expiry is enabled.
        assert!(is_person_enabled("99999"));
    }

    #[test]
    fn name_allowlist() {
        for ok in ["alice", "naslos_admins", "smbuser1", "a.b-c"] {
            assert!(is_valid_identity_name(ok), "{ok}");
        }
        for bad in ["Alice", "-x", "a b", "", "a/b", "a@b", &"x".repeat(65)] {
            assert!(!is_valid_identity_name(bad), "{bad}");
        }
    }

    #[test]
    fn dn_and_filter_escaping() {
        assert_eq!(escape_filter("a*b(c)"), "a\\2ab\\28c\\29");
        assert_eq!(escape_dn_component("a,b"), "a\\,b");
    }

    #[test]
    fn short_names_reduce_dns() {
        let out = short_names(&[
            "cn=naslos_admins,ou=groups,dc=naslos,dc=local".into(),
            "uid=alice,ou=people,dc=naslos,dc=local".into(),
        ]);
        assert_eq!(out, vec!["naslos_admins", "alice"]);
    }

    #[test]
    fn person_cn_falls_back_to_uid() {
        assert_eq!(person_cn("", "", "", "alice"), "alice");
        assert_eq!(person_cn("", "Al", "Ice", "alice"), "Al Ice");
        assert_eq!(person_cn("Alice", "Al", "Ice", "alice"), "Alice");
    }
}
