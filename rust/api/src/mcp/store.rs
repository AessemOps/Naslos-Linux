//! The MCP token store (FR-MCP): `/var/lib/naslos/mcp.json`, bearer-token
//! records hashed with SHA-256, and the in-memory recent-call ring buffers.
//!
//! Plaintext tokens are only ever shown once, in the `POST /api/mcp/tokens`
//! response; the store keeps only `sha256(plaintext)`. Verification compares
//! hashes in constant time over the whole token list so lookup timing does
//! not reveal whether a prefix of the list matched (SEC-13).

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

use base64::Engine;
use subtle::ConstantTimeEq;

/// Bearer tokens are `nmcp_` + 32 random bytes, base64url without padding.
const TOKEN_PREFIX: &str = "nmcp_";
const TOKEN_BYTES: usize = 32;
/// How long a token's last-usage flush may lag behind the in-memory value.
const LAST_USED_FLUSH_EVERY: Duration = Duration::from_secs(60);
/// Per-token recent-call ring buffer length.
const RECENT_CALLS_PER_TOKEN: usize = 50;

/// A token's scope. Ordered: `Read` < `Write` < `Full`. Every tool declares
/// the minimum scope it needs; `tools/list` hides the rest and `tools/call`
/// re-checks.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Read,
    Write,
    Full,
}

/// `now + days` with the full `u64 -> i64` and `DateTime + Duration`
/// range checked. An out-of-range request is rejected instead of wrapping.
fn expiry_in_days(
    now: chrono::DateTime<chrono::Utc>,
    days: Option<u64>,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, String> {
    match days {
        Some(days) if days > 0 => {
            let days =
                i64::try_from(days).map_err(|_| "expiresInDays is out of range".to_string())?;
            let span = chrono::Duration::try_days(days)
                .ok_or_else(|| "expiresInDays is out of range".to_string())?;
            now.checked_add_signed(span)
                .map(Some)
                .ok_or_else(|| "expiresInDays is out of range".to_string())
        }
        _ => Ok(None),
    }
}

/// Fail closed: a token with an unparsable or past `expiresAt` is expired.
fn token_not_expired(token: &TokenRecord, now: &chrono::DateTime<chrono::Utc>) -> bool {
    match token.expires_at.as_deref() {
        None => true,
        Some(raw) => chrono::DateTime::parse_from_rfc3339(raw)
            .map(|expires| expires.with_timezone(&chrono::Utc) > *now)
            .unwrap_or(false),
    }
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Read => "read",
            Scope::Write => "write",
            Scope::Full => "full",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "read" => Some(Scope::Read),
            "write" => Some(Scope::Write),
            "full" => Some(Scope::Full),
            _ => None,
        }
    }

    /// Whether a token with this scope may use a tool requiring `need`.
    pub fn allows(self, need: Scope) -> bool {
        self >= need
    }

    /// The synthetic RBAC role a token of this scope acts as. A `write`
    /// token is an operator, never an administrator: identity, credential and
    /// egress tools are `full` for exactly this reason (R-009).
    pub fn synthetic_role(self) -> crate::rbac::Role {
        match self {
            Scope::Read => crate::rbac::Role::ReadOnly,
            Scope::Write => crate::rbac::Role::Operator,
            Scope::Full => crate::rbac::Role::Admin,
        }
    }

    /// The trusted group list for a call under this scope. Used as the
    /// synthetic identity of the internal dispatch.
    pub fn synthetic_groups(self) -> Vec<String> {
        vec![match self {
            Scope::Read => "naslos_readonly",
            Scope::Write => "naslos_operators",
            Scope::Full => "naslos_admins",
        }
        .to_string()]
    }
}

/// A stored token. `hash` is lowercase hex `sha256(plaintext)`; the plaintext
/// is never persisted (mirrors how the notification token store keeps its
/// secret out of `GET` responses).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TokenRecord {
    pub id: String,
    pub name: String,
    pub hash: String,
    pub scope: Scope,
    #[serde(rename = "createdBy", default)]
    pub created_by: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "expiresAt", default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(
        rename = "lastUsedAt",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub last_used_at: Option<String>,
}

/// One audited `tools/call`, kept in memory only (never persisted).
#[derive(Debug, Clone, serde::Serialize)]
pub struct CallRecord {
    pub tool: String,
    pub scope: String,
    pub status: u16,
    #[serde(rename = "durationMs")]
    pub duration_ms: u64,
    pub at: String,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Persisted {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    tokens: Vec<TokenRecord>,
}

struct Inner {
    enabled: bool,
    tokens: Vec<TokenRecord>,
}

pub struct Store {
    path: String,
    inner: Mutex<Inner>,
    /// Token id -> most recent calls, newest last (capped per token).
    recent: Mutex<HashMap<String, VecDeque<CallRecord>>>,
    last_used_flush: Mutex<std::time::Instant>,
    /// Set when the persisted config could not be read or parsed. Mutations
    /// are refused and the original file is preserved (plus a `.corrupt-<ts>`
    /// copy), so a corrupt file can never be replaced by an empty token list
    /// that silently deletes every hash (A-2).
    load_error: Mutex<Option<String>>,
}

/// The token resolved from a verified `Authorization: Bearer` header. Stashed
/// in the request extensions by the guard so the MCP service can scope
/// `tools/list` and `tools/call`.
#[derive(Debug, Clone)]
pub struct TokenAuth {
    pub id: String,
    pub name: String,
    pub scope: Scope,
}

impl Store {
    pub fn new(path: &str) -> Self {
        let mut store = Self {
            path: path.to_string(),
            inner: Mutex::new(Inner {
                enabled: false,
                tokens: Vec::new(),
            }),
            recent: Mutex::new(HashMap::new()),
            last_used_flush: Mutex::new(std::time::Instant::now()),
            load_error: Mutex::new(None),
        };
        store.load();
        store
    }

    pub fn from_env() -> Self {
        Self::new(
            &std::env::var("MCP_CONFIG").unwrap_or_else(|_| "/var/lib/naslos/mcp.json".to_string()),
        )
    }

    fn load(&mut self) {
        *self.load_error.lock().unwrap() = None;
        if self.path.is_empty() {
            return;
        }
        let data = match std::fs::read(&self.path) {
            Ok(data) => data,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                *self.load_error.lock().unwrap() = Some(format!(
                    "the MCP config {} could not be read: {e}; refusing to change MCP settings",
                    self.path
                ));
                return;
            }
        };
        if data.iter().all(|b| b.is_ascii_whitespace()) {
            // A freshly provisioned empty file is a valid empty store.
            return;
        }
        match serde_json::from_slice::<Persisted>(&data) {
            Ok(p) => {
                let mut inner = self.inner.lock().unwrap();
                // Enabling without tokens would expose an empty tool surface;
                // a corrupt record must not silently drop tokens either, so
                // both fields load together or not at all.
                inner.enabled = p.enabled;
                inner.tokens = p.tokens;
            }
            Err(e) => {
                let backup = match crate::shares::backup_corrupt_store(&self.path, &data) {
                    Ok(path) => format!(" (backup saved to {})", path.display()),
                    Err(backup_err) => format!(" ({backup_err})"),
                };
                let message = format!(
                    "the MCP config {} is corrupt: {e}{backup}; repair it or replace it with a valid store before changing MCP settings",
                    self.path
                );
                tracing::error!("{message}");
                *self.load_error.lock().unwrap() = Some(message);
            }
        }
    }

    /// Fail closed unless the persisted config was read successfully. Every
    /// mutation calls this first: writing over an unreadable file would
    /// permanently delete every stored token hash.
    pub fn require_loaded(&self) -> Result<(), String> {
        match self.load_error.lock().unwrap().clone() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn save_with_path(path: &str, inner: &Inner) -> Result<(), String> {
        if path.is_empty() {
            return Ok(());
        }
        let persisted = Persisted {
            enabled: inner.enabled,
            tokens: inner.tokens.clone(),
        };
        let data = serde_json::to_vec_pretty(&persisted)
            .map_err(|e| format!("marshaling the MCP config: {e}"))?;
        // 0600: this file is the bearer-token hash table.
        crate::chartsrepo::write_atomic(path, &data, 0o600, 0o750)
    }

    pub fn enabled(&self) -> bool {
        self.inner.lock().unwrap().enabled
    }

    pub fn set_enabled(&self, enabled: bool) -> Result<(), String> {
        self.require_loaded()?;
        let mut inner = self.inner.lock().unwrap();
        let candidate = Inner {
            enabled,
            tokens: inner.tokens.clone(),
        };
        Self::save_with_path(&self.path, &candidate)?;
        *inner = candidate;
        Ok(())
    }
    /// Tokens without the hash, for the Settings UI.
    pub fn tokens_public(&self) -> Vec<TokenRecord> {
        self.inner
            .lock()
            .unwrap()
            .tokens
            .iter()
            .map(|t| {
                let mut t = t.clone();
                t.hash = String::new();
                t
            })
            .collect()
    }

    pub fn token_count(&self) -> usize {
        self.inner.lock().unwrap().tokens.len()
    }

    /// Create a token. Returns the stored record plus the plaintext, which is
    /// shown to the operator exactly once.
    pub fn create_token(
        &self,
        name: &str,
        scope: Scope,
        expires_in_days: Option<u64>,
        created_by: &str,
    ) -> Result<(TokenRecord, String), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("token name is required".into());
        }
        if name.len() > 64 {
            return Err("token name is too long (max 64)".into());
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ' '))
        {
            return Err("token name may only use letters, digits, - _ . and spaces".into());
        }
        self.require_loaded()?;
        let mut inner = self.inner.lock().unwrap();
        if inner
            .tokens
            .iter()
            .any(|t| t.name.eq_ignore_ascii_case(name))
        {
            return Err(format!("a token named {name:?} already exists"));
        }

        let mut bytes = [0u8; TOKEN_BYTES];
        getrandom::fill(&mut bytes).map_err(|e| format!("generating token bytes: {e}"))?;
        let plaintext = format!(
            "{TOKEN_PREFIX}{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
        );

        let mut id_bytes = [0u8; 8];
        getrandom::fill(&mut id_bytes).map_err(|e| format!("generating token id: {e}"))?;
        let now = chrono::Utc::now();
        let expires_at = expiry_in_days(now, expires_in_days)?;
        let record = TokenRecord {
            id: hex(&id_bytes),
            name: name.to_string(),
            hash: sha256_hex(plaintext.as_bytes()),
            scope,
            created_by: created_by.to_string(),
            created_at: now.to_rfc3339(),
            expires_at: expires_at.map(|at| at.to_rfc3339()),
            last_used_at: None,
        };
        let mut candidate = Inner {
            enabled: inner.enabled,
            tokens: inner.tokens.clone(),
        };
        candidate.tokens.push(record.clone());
        Self::save_with_path(&self.path, &candidate)?;
        *inner = candidate;
        Ok((record, plaintext))
    }

    /// Verify a bearer token. `Ok(Some(auth))` on success, `Ok(None)` when
    /// the token is unknown, expired or disabled — the caller answers 401
    /// without saying which. The hash comparison runs over every stored
    /// token in constant time so the number of tokens is the only signal.
    pub fn verify(&self, plaintext: &str) -> Option<TokenAuth> {
        let inner = self.inner.lock().unwrap();
        if !inner.enabled {
            return None;
        }
        let provided = sha256_hex(plaintext.as_bytes());
        let now = chrono::Utc::now();
        let mut found: Option<(String, String, Scope)> = None;
        for token in &inner.tokens {
            let hash_match = token.hash.as_bytes().ct_eq(provided.as_bytes()).unwrap_u8();
            // Constant-time select over the whole list: the loop body's work
            // is identical for every token whatever matches. A malformed
            // `expiresAt` fails closed (the token is treated as expired).
            if hash_match == 1 && token_not_expired(token, &now) {
                found = Some((token.id.clone(), token.name.clone(), token.scope));
            }
        }
        found.map(|(id, name, scope)| TokenAuth { id, name, scope })
    }

    pub fn revoke(&self, id: &str) -> Result<bool, String> {
        self.require_loaded()?;
        let mut inner = self.inner.lock().unwrap();
        if !inner.tokens.iter().any(|t| t.id == id) {
            return Ok(false);
        }
        // Persist-then-swap: a failed save must not revoke the token only
        // until the next restart (B-3).
        let candidate = Inner {
            enabled: inner.enabled,
            tokens: inner
                .tokens
                .iter()
                .filter(|t| t.id != id)
                .cloned()
                .collect(),
        };
        Self::save_with_path(&self.path, &candidate)?;
        *inner = candidate;
        drop(inner);
        self.recent.lock().unwrap().remove(id);
        Ok(true)
    }

    /// Record a token's use. The in-memory value updates immediately; the
    /// flush to disk is debounced so a chatty MCP client cannot turn every
    /// call into a write.
    pub fn record_last_used(&self, id: &str) {
        if self.load_error.lock().unwrap().is_some() {
            // Never write over a store that failed to load.
            return;
        }
        let mut flush = false;
        {
            let mut inner = self.inner.lock().unwrap();
            let now = chrono::Utc::now().to_rfc3339();
            for token in &mut inner.tokens {
                if token.id == id {
                    token.last_used_at = Some(now.clone());
                }
            }
            let mut last = self.last_used_flush.lock().unwrap();
            if last.elapsed() >= LAST_USED_FLUSH_EVERY {
                *last = std::time::Instant::now();
                flush = true;
            }
        }
        if flush {
            let inner = self.inner.lock().unwrap();
            if let Err(e) = Self::save_with_path(&self.path, &inner) {
                tracing::warn!("could not flush MCP token usage: {e}");
            }
        }
    }

    /// Append to a token's recent-call ring buffer.
    pub fn record_call(&self, id: &str, record: CallRecord) {
        let mut recent = self.recent.lock().unwrap();
        let buffer = recent.entry(id.to_string()).or_default();
        buffer.push_back(record);
        while buffer.len() > RECENT_CALLS_PER_TOKEN {
            buffer.pop_front();
        }
    }

    /// The most recent calls across every token, newest first (capped at the
    /// per-token buffer length). The Settings UI shows one flat feed with the
    /// token name attached.
    pub fn recent_calls(&self) -> Vec<serde_json::Value> {
        let inner = self.inner.lock().unwrap();
        let names: HashMap<String, String> = inner
            .tokens
            .iter()
            .map(|t| (t.id.clone(), t.name.clone()))
            .collect();
        drop(inner);

        let recent = self.recent.lock().unwrap();
        let mut all: Vec<(String, CallRecord)> = recent
            .iter()
            .flat_map(|(id, buffer)| {
                let token_name = names.get(id).cloned().unwrap_or_default();
                buffer
                    .iter()
                    .map(move |rec| (token_name.clone(), rec.clone()))
            })
            .collect();
        all.sort_by(|a, b| b.1.at.cmp(&a.1.at));
        all.into_iter()
            .take(RECENT_CALLS_PER_TOKEN)
            .map(|(token_name, rec)| {
                serde_json::json!({
                    "tokenName": token_name,
                    "tool": rec.tool,
                    "scope": rec.scope,
                    "status": rec.status,
                    "durationMs": rec.duration_ms,
                    "at": rec.at,
                })
            })
            .collect()
    }
}

pub(crate) fn sha256_hex_for_tracking(data: &[u8]) -> String {
    sha256_hex(data)
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(data);
    hex(&digest)
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(tag: &str) -> String {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("mcp-{tag}.json"));
        // Keep the dir alive for the test's lifetime by leaking it: the store
        // only needs the path string.
        std::mem::forget(dir);
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn create_verify_roundtrip() {
        let store = Store::new("");
        store.set_enabled(true).unwrap();
        let (record, plaintext) = store
            .create_token("drill", Scope::Read, None, "admin")
            .unwrap();
        assert!(plaintext.starts_with(TOKEN_PREFIX));
        assert_eq!(plaintext.len(), TOKEN_PREFIX.len() + 43); // 32 bytes -> 43 b64 chars
        assert_ne!(record.hash, plaintext);

        let auth = store.verify(&plaintext).expect("token must verify");
        assert_eq!(auth.id, record.id);
        assert_eq!(auth.scope, Scope::Read);
    }

    #[test]
    fn verify_fails_when_disabled_or_unknown_or_revoked() {
        let store = Store::new("");
        let (_, plaintext) = store.create_token("t", Scope::Full, None, "admin").unwrap();

        // Disabled: every request is refused even with a valid token.
        assert!(store.verify(&plaintext).is_none());
        store.set_enabled(true).unwrap();
        assert!(store.verify(&plaintext).is_some());

        // Unknown token.
        assert!(store.verify("nmcp_nope").is_none());

        // Revoked token.
        let (record, _) = store
            .create_token("t2", Scope::Write, None, "admin")
            .unwrap();
        assert!(store.revoke(&record.id).unwrap());
        assert!(!store.revoke(&record.id).unwrap());
    }

    #[test]
    fn expired_tokens_do_not_verify() {
        let store = Store::new("");
        store.set_enabled(true).unwrap();
        let (record, plaintext) = store
            .create_token("ephemeral", Scope::Read, Some(1), "admin")
            .unwrap();
        assert!(record.expires_at.is_some());
        assert!(store.verify(&plaintext).is_some());

        // A zero/negative expiry window never applies (filter keeps d>0),
        // so simulate expiry by rewriting the record on disk.
        let path = tmp_path("expiry");
        let store = Store::new(&path);
        store.set_enabled(true).unwrap();
        let (record, plaintext) = store
            .create_token("gone", Scope::Read, Some(1), "admin")
            .unwrap();
        // Rewrite the persisted file with an expires_at in the past.
        let data = std::fs::read(&path).unwrap();
        let mut doc: serde_json::Value = serde_json::from_slice(&data).unwrap();
        let past = (chrono::Utc::now() - chrono::Duration::hours(2)).to_rfc3339();
        for token in doc["tokens"].as_array_mut().unwrap() {
            if token["id"] == serde_json::json!(record.id) {
                token["expiresAt"] = serde_json::json!(past);
            }
        }
        std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
        let store = Store::new(&path);
        assert!(store.verify(&plaintext).is_none());
    }

    #[test]
    fn an_out_of_range_expiry_is_rejected_not_wrapped() {
        let store = Store::new("");
        store.set_enabled(true).unwrap();
        let err = store
            .create_token("overflow", Scope::Read, Some(u64::MAX), "admin")
            .unwrap_err();
        assert!(err.contains("out of range"), "{err}");
        assert_eq!(store.token_count(), 0);
    }

    #[test]
    fn a_malformed_expiry_fails_closed() {
        let path = tmp_path("malformed-expiry");
        let store = Store::new(&path);
        store.set_enabled(true).unwrap();
        let (record, plaintext) = store
            .create_token("broken", Scope::Read, Some(1), "admin")
            .unwrap();
        let data = std::fs::read(&path).unwrap();
        let mut doc: serde_json::Value = serde_json::from_slice(&data).unwrap();
        for token in doc["tokens"].as_array_mut().unwrap() {
            if token["id"] == serde_json::json!(record.id) {
                token["expiresAt"] = serde_json::json!("not-a-timestamp");
            }
        }
        std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
        // A reload sees the persisted (unparsable) value; it must not verify.
        let store = Store::new(&path);
        assert!(store.verify(&plaintext).is_none());
    }

    #[test]
    fn synthetic_roles_never_make_write_an_admin() {
        assert_eq!(Scope::Read.synthetic_role(), crate::rbac::Role::ReadOnly);
        assert_eq!(Scope::Write.synthetic_role(), crate::rbac::Role::Operator);
        assert_eq!(Scope::Full.synthetic_role(), crate::rbac::Role::Admin);
        assert!(!Scope::Write
            .synthetic_groups()
            .iter()
            .any(|g| g == "naslos_admins"));
        assert!(Scope::Full
            .synthetic_groups()
            .iter()
            .any(|g| g == "naslos_admins"));
    }

    #[test]
    fn names_must_be_unique_and_sane() {
        let store = Store::new("");
        store
            .create_token("same", Scope::Read, None, "admin")
            .unwrap();
        assert!(store
            .create_token("Same", Scope::Read, None, "admin")
            .is_err());
        assert!(store.create_token("", Scope::Read, None, "admin").is_err());
        assert!(store
            .create_token("bad/name", Scope::Read, None, "admin")
            .is_err());
    }

    #[test]
    fn persistence_roundtrip_and_default_off() {
        let path = tmp_path("persist");
        {
            let store = Store::new(&path);
            assert!(!store.enabled(), "MCP must default to disabled");
            store.set_enabled(true).unwrap();
            store
                .create_token("keeper", Scope::Write, Some(30), "admin")
                .unwrap();
        }
        let store = Store::new(&path);
        assert!(store.enabled());
        assert_eq!(store.token_count(), 1);
        let tokens = store.tokens_public();
        assert_eq!(tokens[0].name, "keeper");
        assert_eq!(tokens[0].hash, "", "the hash must never reach the UI");
    }

    #[test]
    fn scopes_order_read_write_full() {
        assert!(Scope::Read.allows(Scope::Read));
        assert!(!Scope::Read.allows(Scope::Write));
        assert!(Scope::Write.allows(Scope::Write));
        assert!(!Scope::Write.allows(Scope::Full));
        assert!(Scope::Full.allows(Scope::Full));
        assert_eq!(Scope::parse("READ"), Some(Scope::Read));
        assert_eq!(Scope::parse("nope"), None);
    }

    #[test]
    fn a_corrupt_config_fails_closed_and_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        let corrupt = b"{this is not json";
        std::fs::write(&path, corrupt).unwrap();

        let store = Store::new(path.to_str().unwrap());
        assert!(store.require_loaded().is_err());
        assert!(store.set_enabled(true).is_err());
        assert!(store
            .create_token("blocked", Scope::Read, None, "admin")
            .is_err());
        assert!(store.revoke("anything").is_err());
        assert_eq!(
            std::fs::read(&path).unwrap(),
            corrupt,
            "the file is untouched"
        );

        let backups: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".corrupt-"))
            .collect();
        assert_eq!(backups.len(), 1, "the corrupt file is backed up");
        assert_eq!(std::fs::read(backups[0].path()).unwrap(), corrupt);

        // An operator repair plus a restart loads again.
        std::fs::write(&path, br#"{"enabled":true,"tokens":[]}"#).unwrap();
        let repaired = Store::new(path.to_str().unwrap());
        assert!(repaired.require_loaded().is_ok());
        assert!(repaired.enabled());
    }

    #[test]
    fn a_failed_revoke_save_leaves_the_token_valid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        let store = Store::new(path.to_str().unwrap());
        store.set_enabled(true).unwrap();
        let (record, plaintext) = store
            .create_token("keeper", Scope::Read, None, "admin")
            .unwrap();
        // A directory at the store path makes the atomic rename fail even when
        // the tests run as root (permission bits would not).
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();

        assert!(store.revoke(&record.id).is_err());
        assert!(
            store.verify(&plaintext).is_some(),
            "a failed revoke must not revoke the token in memory"
        );
        assert_eq!(store.token_count(), 1);
    }

    #[test]
    fn recent_calls_ring_is_capped_and_flattened() {
        let store = Store::new("");
        let (a, _) = store.create_token("a", Scope::Read, None, "admin").unwrap();
        let (b, _) = store.create_token("b", Scope::Read, None, "admin").unwrap();
        for i in 0..(RECENT_CALLS_PER_TOKEN as u64 + 10) {
            store.record_call(
                &a.id,
                CallRecord {
                    tool: format!("tool-{i}"),
                    scope: "read".into(),
                    status: 200,
                    duration_ms: i,
                    at: chrono::Utc::now().to_rfc3339(),
                },
            );
        }
        store.record_call(
            &b.id,
            CallRecord {
                tool: "other".into(),
                scope: "read".into(),
                status: 200,
                duration_ms: 1,
                at: chrono::Utc::now().to_rfc3339(),
            },
        );
        let calls = store.recent_calls();
        assert_eq!(calls.len(), RECENT_CALLS_PER_TOKEN);
        // Flattened feed is capped globally and the other token's call is
        // present (it sorts after only RECENT_CALLS_PER_TOKEN-1 of a's).
        assert!(calls
            .iter()
            .any(|c| c["tokenName"] == serde_json::json!("b")));
    }
}
