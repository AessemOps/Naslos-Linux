//! Buddy request authentication (port of `api/internal/buddy/auth.go`).
//!
//! A sender proves who it is by signing; the receiver never needs a shared
//! secret. Signatures are bound to an audience, the method/path, a body digest,
//! a timestamp and a nonce, so a captured request cannot be moved, replayed or
//! retargeted.

use base64::Engine;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::keys::{verify, Identity};
use super::peers::{Peer, PeerStore};

pub const HEADER_KEY_ID: &str = "X-Buddy-Key";
pub const HEADER_TIMESTAMP: &str = "X-Buddy-Timestamp";
pub const HEADER_NONCE: &str = "X-Buddy-Nonce";
pub const HEADER_SIGNATURE: &str = "X-Buddy-Signature";

/// How far a request's timestamp may be from ours.
pub const MAX_CLOCK_SKEW: Duration = Duration::from_secs(5 * 60);
/// How long a nonce stays unusable.
const NONCE_TTL: Duration = Duration::from_secs(2 * 5 * 60);
const MAX_NONCES_PER_KEY: usize = 4096;
const NONCE_FILE_COMPACT_THRESHOLD: usize = 1024;

/// The exact string a sender signs and a receiver rebuilds.
pub fn canonical_request(
    audience: &str,
    method: &str,
    path: &str,
    body_digest: &str,
    timestamp: &str,
    nonce: &str,
) -> String {
    [
        "BUDDY2",
        audience,
        &method.to_uppercase(),
        path,
        body_digest,
        timestamp,
        nonce,
    ]
    .join("\n")
}

/// The hex SHA-256 of a request body.
pub fn body_digest(body: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(body);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// The four authentication headers a signed request carries.
#[derive(Debug, Clone, Default)]
pub struct RequestAuth {
    pub key_id: String,
    pub timestamp: String,
    pub nonce: String,
    pub signature: String,
}

/// Authenticate an outgoing request, binding it to `audience`.
pub fn sign_request_for(
    id: &Identity,
    method: &str,
    uri: &str,
    body_digest: &str,
    audience: &str,
    now: DateTime<Utc>,
) -> Result<RequestAuth, String> {
    let key_id = id.fingerprint()?;
    let nonce = random_nonce()?;
    let timestamp = now.timestamp().to_string();
    let canonical = canonical_request(audience, method, uri, body_digest, &timestamp, &nonce);
    let sig = id.sign(canonical.as_bytes())?;
    Ok(RequestAuth {
        key_id,
        timestamp,
        nonce,
        signature: base64::engine::general_purpose::STANDARD.encode(sig),
    })
}

/// Verifies incoming peer requests and remembers nonces.
pub struct Authenticator {
    peers: Arc<PeerStore>,
    seen: Mutex<HashMap<String, HashMap<String, i64>>>,
    nonce_file: Mutex<Option<String>>,
    warned: Mutex<bool>,
}

impl Authenticator {
    pub fn new(peers: Arc<PeerStore>) -> Self {
        Self {
            peers,
            seen: Mutex::new(HashMap::new()),
            nonce_file: Mutex::new(None),
            warned: Mutex::new(false),
        }
    }

    /// Make the replay cache survive a restart by recording nonces under `dir`
    /// (`.nonces`).
    pub fn persist_nonces(&self, dir: &str) {
        if dir.is_empty() {
            return;
        }
        let path = std::path::Path::new(dir)
            .join(".nonces")
            .display()
            .to_string();
        *self.nonce_file.lock().unwrap() = Some(path.clone());
        self.load_nonces(&path, Utc::now());
    }

    fn load_nonces(&self, path: &str, now: DateTime<Utc>) {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return;
        };
        let mut guard = self.seen.lock().unwrap();
        let mut restored = 0;
        for line in raw.split('\n') {
            let mut fields = line.splitn(3, ' ');
            let (Some(exp), Some(key_id), Some(nonce)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let Ok(expiry_nanos) = exp.parse::<i64>() else {
                continue;
            };
            if DateTime::from_timestamp_nanos(expiry_nanos) < now {
                continue;
            }
            guard
                .entry(key_id.to_string())
                .or_default()
                .insert(nonce.to_string(), expiry_nanos);
            restored += 1;
        }
        if restored > 0 {
            tracing::info!("buddy auth: restored {restored} unexpired nonces from {path}");
        }
    }

    fn persist_nonce(&self, key_id: &str, nonce: &str, expiry_nanos: i64, live_entries: usize) {
        let file = self.nonce_file.lock().unwrap().clone();
        let Some(path) = file else { return };
        let line = format!("{expiry_nanos} {key_id} {nonce}\n");
        use std::io::Write;
        let append = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&path)
            .and_then(|mut f| f.write_all(line.as_bytes()));
        if append.is_err() {
            self.warn_once(
                "cannot persist nonces; a restart would re-arm them within the skew window",
            );
        }
        if live_entries < NONCE_FILE_COMPACT_THRESHOLD {
            return;
        }
        let mut builder = String::new();
        {
            let guard = self.seen.lock().unwrap();
            for (k, nonces) in guard.iter() {
                for (n, exp) in nonces {
                    builder.push_str(&format!("{exp} {k} {n}\n"));
                }
            }
        }
        if crate::chartsrepo::write_atomic(&path, builder.as_bytes(), 0o600, 0o750).is_err() {
            self.warn_once("cannot compact the nonce cache");
        }
    }

    fn warn_once(&self, msg: &str) {
        let mut warned = self.warned.lock().unwrap();
        if !*warned {
            *warned = true;
            tracing::warn!("buddy auth: {msg}");
        }
    }

    /// Authenticate a request. `path` is the request URI exactly as the sender
    /// signed it; `audience` is this receiver's own identity.
    pub fn verify(
        &self,
        auth: &RequestAuth,
        body_digest: &str,
        method: &str,
        path: &str,
        audience: &str,
        now: DateTime<Utc>,
    ) -> Result<Peer, String> {
        if auth.key_id.is_empty()
            || auth.timestamp.is_empty()
            || auth.nonce.is_empty()
            || auth.signature.is_empty()
        {
            return Err("missing authentication headers".to_string());
        }
        let ts: i64 = auth
            .timestamp
            .parse()
            .map_err(|_| "invalid timestamp".to_string())?;
        if (now.timestamp() - ts).abs() > MAX_CLOCK_SKEW.as_secs() as i64 {
            return Err(format!(
                "request timestamp is outside the {}s window (check the clocks)",
                MAX_CLOCK_SKEW.as_secs()
            ));
        }

        let peer = self
            .peers
            .by_fingerprint(&auth.key_id)
            .ok_or_else(|| format!("unknown key {}", auth.key_id))?;
        if !peer.enabled {
            return Err(format!("key {} is disabled", auth.key_id));
        }
        let sig = base64::engine::general_purpose::STANDARD
            .decode(&auth.signature)
            .map_err(|e| format!("decoding signature: {e}"))?;

        let mut audiences = vec![audience.to_string()];
        if !audience.is_empty() && path == format!("{}/status", super::PATH_PREFIX) {
            audiences.push(String::new());
        }
        let mut verified = false;
        for candidate in &audiences {
            let canonical = canonical_request(
                candidate,
                method,
                path,
                body_digest,
                &auth.timestamp,
                &auth.nonce,
            );
            if verify(&peer.public_key, canonical.as_bytes(), &sig).is_ok() {
                verified = true;
                break;
            }
        }
        if !verified {
            return Err(format!(
                "signature verification failed (signed request: {method} {path})"
            ));
        }

        validate_nonce(&auth.nonce)?;
        if self.replay(&auth.key_id, &auth.nonce, now) {
            return Err("this request was already used (nonce replay)".to_string());
        }
        Ok(peer)
    }

    /// Record a nonce; returns true if it was already seen.
    fn replay(&self, key_id: &str, nonce: &str, now: DateTime<Utc>) -> bool {
        let now_nanos = now.timestamp_nanos_opt().unwrap_or(0);
        let live;
        {
            let mut guard = self.seen.lock().unwrap();
            let nonces = guard.entry(key_id.to_string()).or_default();
            nonces.retain(|_, exp| *exp >= now_nanos);
            if nonces.contains_key(nonce) {
                return true;
            }
            if nonces.len() >= MAX_NONCES_PER_KEY {
                if let Some(soonest) = nonces
                    .iter()
                    .min_by_key(|(_, exp)| **exp)
                    .map(|(k, _)| k.clone())
                {
                    nonces.remove(&soonest);
                }
            }
            let expiry = now_nanos + NONCE_TTL.as_nanos() as i64;
            nonces.insert(nonce.to_string(), expiry);
            live = nonces.len();
        }
        self.persist_nonce(key_id, nonce, now_nanos + NONCE_TTL.as_nanos() as i64, live);
        false
    }
}

/// A nonce must decode to 16..64 bytes.
pub fn validate_nonce(nonce: &str) -> Result<(), String> {
    if nonce.len() > 128 {
        return Err("nonce is too long".to_string());
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(nonce)
        .map_err(|_| "nonce is not valid base64".to_string())?;
    if !(16..=64).contains(&decoded.len()) {
        return Err("nonce must decode to between 16 and 64 bytes".to_string());
    }
    Ok(())
}

/// A 128-bit random nonce, base64-encoded.
pub fn random_nonce() -> Result<String, String> {
    let mut buf = [0u8; 16];
    getrandom::getrandom(&mut buf).map_err(|e| format!("generating nonce: {e}"))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(buf))
}

#[cfg(test)]
mod tests {
    use super::super::keys::new_identity;
    use super::*;

    fn authenticator_with(id: &Identity, name: &str) -> Authenticator {
        let store = Arc::new(PeerStore::new(""));
        store
            .add(Peer {
                name: name.to_string(),
                public_key: id.public_key.clone(),
                enabled: true,
                ..Default::default()
            })
            .unwrap();
        Authenticator::new(store)
    }

    #[test]
    fn sign_then_verify() {
        let id = new_identity("naslos-a").unwrap();
        let auth = authenticator_with(&id, "peer-a");
        let now = Utc::now();
        let digest = body_digest(b"");
        let headers =
            sign_request_for(&id, "GET", "/api/buddy/status", &digest, "receiver", now).unwrap();
        let peer = auth
            .verify(
                &headers,
                &digest,
                "GET",
                "/api/buddy/status",
                "receiver",
                now,
            )
            .unwrap();
        assert_eq!(peer.name, "peer-a");
    }

    #[test]
    fn replay_is_rejected() {
        let id = new_identity("a").unwrap();
        let auth = authenticator_with(&id, "p");
        let now = Utc::now();
        let digest = body_digest(b"");
        let h = sign_request_for(&id, "GET", "/api/buddy/status", &digest, "r", now).unwrap();
        auth.verify(&h, &digest, "GET", "/api/buddy/status", "r", now)
            .unwrap();
        let err = auth
            .verify(&h, &digest, "GET", "/api/buddy/status", "r", now)
            .unwrap_err();
        assert!(err.contains("already used"), "{err}");
    }

    #[test]
    fn wrong_audience_fails() {
        let id = new_identity("a").unwrap();
        let auth = authenticator_with(&id, "p");
        let now = Utc::now();
        let digest = body_digest(b"");
        let h = sign_request_for(&id, "GET", "/api/buddy/x", &digest, "receiver-a", now).unwrap();
        assert!(auth
            .verify(&h, &digest, "GET", "/api/buddy/x", "receiver-b", now)
            .is_err());
    }

    #[test]
    fn empty_audience_allowed_only_on_status() {
        let id = new_identity("a").unwrap();
        let auth = authenticator_with(&id, "p");
        let now = Utc::now();
        let digest = body_digest(b"");
        let h = sign_request_for(&id, "GET", "/api/buddy/status", &digest, "", now).unwrap();
        // On /status, the empty-audience signature is accepted alongside the real one.
        assert!(auth
            .verify(&h, &digest, "GET", "/api/buddy/status", "receiver", now)
            .is_ok());
    }

    #[test]
    fn nonce_validation() {
        assert!(
            validate_nonce(&base64::engine::general_purpose::STANDARD.encode([0u8; 16])).is_ok()
        );
        assert!(validate_nonce("not-base64!!").is_err());
        assert!(
            validate_nonce(&base64::engine::general_purpose::STANDARD.encode([0u8; 8])).is_err()
        );
    }
}
