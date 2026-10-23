//! Buddy peers (port of `api/internal/buddy/peers.go`): the senders authorized
//! to push to this instance, and the source/quota validation around them.

use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Mutex;

use super::envelope::MAX_SEALED_CHUNK_SIZE;
use super::keys::public_key_of;

/// The smallest useful quota: one sealed chunk.
pub const MIN_PEER_QUOTA: i64 = MAX_SEALED_CHUNK_SIZE as i64;
/// The largest quota (so a fat-fingered value cannot look like "unlimited").
pub const MAX_PEER_QUOTA: i64 = 1i64 << 60;

/// The shape of a logical backup source (`naslos-a/tank/data`).
pub fn validate_source(source: &str) -> Result<(), String> {
    if source.trim().is_empty() {
        return Err("source is required".to_string());
    }
    if source.len() > 200 {
        return Err("source must be 200 characters or fewer".to_string());
    }
    if source.contains("..") || source.starts_with('/') {
        return Err(format!(
            "invalid source {source:?}: it must be a relative logical name"
        ));
    }
    let mut chars = source.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return Err(invalid_source(source)),
    }
    if !chars
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '/' | '-'))
    {
        return Err(invalid_source(source));
    }
    Ok(())
}

fn invalid_source(source: &str) -> String {
    format!("invalid source {source:?}: use lowercase letters, digits, '.', '_', '-' or '/'")
}

/// Validate a peer's quota: 0 means unlimited.
pub fn validate_quota(quota: i64) -> Result<(), String> {
    if quota < 0 {
        return Err("quota must not be negative (0 means unlimited)".to_string());
    }
    if quota == 0 {
        return Ok(());
    }
    if quota < MIN_PEER_QUOTA {
        return Err(format!(
            "quota must be at least {MIN_PEER_QUOTA} bytes (one chunk), or 0 for unlimited"
        ));
    }
    if quota > MAX_PEER_QUOTA {
        return Err(format!(
            "quota {quota} is implausibly large (maximum {MAX_PEER_QUOTA})"
        ));
    }
    Ok(())
}

/// A sender authorized to push to this instance.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Peer {
    pub name: String,
    #[serde(rename = "publicKey")]
    pub public_key: String,
    #[serde(default)]
    pub fingerprint: String,
    #[serde(
        rename = "allowedSources",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub allowed_sources: Vec<String>,
    #[serde(rename = "quotaBytes", default, skip_serializing_if = "is_zero")]
    pub quota_bytes: i64,
    pub enabled: bool,
    #[serde(rename = "createdAt", default)]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(
        rename = "lastSeenAt",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub last_seen_at: Option<DateTime<Utc>>,
}

fn is_zero(n: &i64) -> bool {
    *n == 0
}

impl Peer {
    /// Whether the peer may write the given source.
    pub fn allows(&self, source: &str) -> bool {
        if self.allowed_sources.is_empty() {
            return true;
        }
        for allowed in &self.allowed_sources {
            let allowed = allowed.trim();
            if allowed.is_empty() {
                continue;
            }
            let prefix = format!("{}/", allowed.trim_end_matches('/'));
            if source == allowed || source.starts_with(&prefix) {
                return true;
            }
        }
        false
    }
}

/// The peer registry, persisted as JSON.
pub struct PeerStore {
    path: String,
    peers: Mutex<HashMap<String, Peer>>,
}

impl PeerStore {
    pub fn new(path: &str) -> Self {
        Self {
            path: path.to_string(),
            peers: Mutex::new(HashMap::new()),
        }
    }

    pub fn store_path(&self) -> &str {
        &self.path
    }

    /// Read the registry; a missing file is an empty registry.
    pub fn load(&self) -> Result<(), String> {
        let mut guard = self.peers.lock().unwrap();
        guard.clear();
        if self.path.is_empty() {
            return Ok(());
        }
        let data = match std::fs::read(&self.path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.to_string()),
        };
        let list: Vec<Peer> = serde_json::from_slice(&data)
            .map_err(|e| format!("parsing peer registry {}: {e}", self.path))?;
        for mut p in list {
            if p.name.is_empty() || p.public_key.is_empty() {
                continue;
            }
            if p.fingerprint.is_empty() {
                match public_key_of(&p.public_key) {
                    Ok((_, fp)) => p.fingerprint = fp,
                    Err(_) => continue,
                }
            }
            guard.insert(p.name.clone(), p);
        }
        Ok(())
    }

    fn save(&self, guard: &HashMap<String, Peer>) -> Result<(), String> {
        if self.path.is_empty() {
            return Ok(());
        }
        let mut list: Vec<&Peer> = guard.values().collect();
        list.sort_by(|a, b| a.name.cmp(&b.name));
        let mut data =
            serde_json::to_vec_pretty(&list).map_err(|e| format!("encoding peers: {e}"))?;
        data.push(b'\n');
        crate::chartsrepo::write_atomic(&self.path, &data, 0o600, 0o750)
    }

    /// Authorize a peer (enrollment).
    pub fn add(&self, mut p: Peer) -> Result<(), String> {
        if p.name.trim().is_empty() {
            return Err("peer name is required".to_string());
        }
        let (_, fingerprint) = public_key_of(&p.public_key)?;
        validate_quota(p.quota_bytes)?;
        p.fingerprint = fingerprint;
        if p.created_at.is_none() {
            p.created_at = Some(Utc::now());
        }

        let mut guard = self.peers.lock().unwrap();
        for existing in guard.values() {
            if existing.fingerprint == p.fingerprint && existing.name != p.name {
                return Err(format!(
                    "this key is already authorized as {:?}",
                    existing.name
                ));
            }
            if existing.name == p.name && existing.fingerprint != p.fingerprint {
                return Err(format!(
                    "peer {:?} already exists with a different key; revoke it before authorizing a new key under that name",
                    p.name
                ));
            }
        }
        guard.insert(p.name.clone(), p);
        self.save(&guard)
    }

    /// Revoke a peer's access.
    pub fn remove(&self, name: &str) -> Result<(), String> {
        let mut guard = self.peers.lock().unwrap();
        if guard.remove(name).is_none() {
            return Err(format!("peer {name:?} not found"));
        }
        self.save(&guard)
    }

    pub fn list(&self) -> Vec<Peer> {
        let guard = self.peers.lock().unwrap();
        let mut out: Vec<Peer> = guard.values().cloned().collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    pub fn get(&self, name: &str) -> Option<Peer> {
        self.peers.lock().unwrap().get(name).cloned()
    }

    pub fn by_fingerprint(&self, fingerprint: &str) -> Option<Peer> {
        self.peers
            .lock()
            .unwrap()
            .values()
            .find(|p| p.fingerprint == fingerprint)
            .cloned()
    }

    pub fn count(&self) -> usize {
        self.peers.lock().unwrap().len()
    }

    /// Record that a peer made a request (best effort).
    pub fn touch_seen(&self, name: &str, t: DateTime<Utc>) {
        let mut guard = self.peers.lock().unwrap();
        if let Some(p) = guard.get_mut(name) {
            p.last_seen_at = Some(t);
            let _ = self.save(&guard);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_validation() {
        assert!(validate_source("naslos-a/tank/data").is_ok());
        assert!(validate_source("").is_err());
        assert!(validate_source("/abs").is_err());
        assert!(validate_source("a/../b").is_err());
        assert!(validate_source("Upper/Case").is_err());
    }

    #[test]
    fn quota_validation() {
        assert!(validate_quota(0).is_ok());
        assert!(validate_quota(-1).is_err());
        assert!(validate_quota(MIN_PEER_QUOTA).is_ok());
        assert!(validate_quota(MIN_PEER_QUOTA - 1).is_err());
        assert!(validate_quota(MAX_PEER_QUOTA + 1).is_err());
    }

    #[test]
    fn allows_prefix_and_exact() {
        let p = Peer {
            allowed_sources: vec!["naslos-a/".into(), "naslos-b/tank".into()],
            ..Default::default()
        };
        assert!(p.allows("naslos-a/media"));
        assert!(p.allows("naslos-b/tank"));
        assert!(!p.allows("naslos-c/x"));
        assert!(Peer::default().allows("anything"));
    }
}
