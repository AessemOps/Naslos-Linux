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
/// Unexpired nonces held per peer key. A nonce lives NONCE_TTL (10 min), so
/// this is a sliding rate budget: a chunked send issues one signed request
/// per ~1 MiB chunk, and the old 4096 cap throttled a legitimate LAN-speed
/// sender to ~6.8 MiB/s and hard-failed anything bigger once the window
/// filled (found by the live drill: a multi-GiB send died at ~4 GiB). 65536
/// absorbs ~109 chunk requests per second sustained — LAN-rate backups —
/// while still bounding a flood to a few MB of nonce memory per key.
const MAX_NONCES_PER_KEY: usize = 65536;
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

/// The on-disk replay cache state, serialized with every append and
/// compaction so no nonce can be lost between the snapshot and the rename.
#[derive(Default)]
struct NonceFile {
    path: Option<String>,
    /// Lines in the file since the last compaction.
    lines: usize,
    /// Unexpired nonces written by the last compaction (the growth baseline).
    compacted_live: usize,
    /// Set when the file exists but cannot be parsed; verification is refused
    /// rather than silently dropping replay protection.
    error: Option<String>,
}

/// Verifies incoming peer requests and remembers nonces.
pub struct Authenticator {
    peers: Arc<PeerStore>,
    seen: Mutex<HashMap<String, HashMap<String, i64>>>,
    nonce_file: Mutex<NonceFile>,
    warned: Mutex<bool>,
    /// Unexpired nonces kept per key before requests are refused. Kept as a
    /// field so the refusal can be tested at a small value.
    max_nonces_per_key: usize,
    /// Floor for the growth factor that triggers compaction.
    nonce_compact_threshold: usize,
}

impl Authenticator {
    pub fn new(peers: Arc<PeerStore>) -> Self {
        Self {
            peers,
            seen: Mutex::new(HashMap::new()),
            nonce_file: Mutex::new(NonceFile::default()),
            warned: Mutex::new(false),
            max_nonces_per_key: MAX_NONCES_PER_KEY,
            nonce_compact_threshold: NONCE_FILE_COMPACT_THRESHOLD,
        }
    }

    /// Test-only: shrink the per-key nonce cap.
    #[cfg(test)]
    pub(crate) fn with_nonce_cap(peers: Arc<PeerStore>, cap: usize) -> Self {
        let mut a = Self::new(peers);
        a.max_nonces_per_key = cap;
        a
    }

    /// Test-only: compact at a small file size.
    #[cfg(test)]
    pub(crate) fn with_compact_threshold(mut self, threshold: usize) -> Self {
        self.nonce_compact_threshold = threshold;
        self
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
        {
            let mut state = self.nonce_file.lock().unwrap();
            state.path = Some(path.clone());
            state.lines = 0;
            state.compacted_live = 0;
            state.error = None;
        }
        match self.load_nonces(&path, Utc::now()) {
            Ok(restored) => {
                let mut state = self.nonce_file.lock().unwrap();
                state.lines = restored;
                state.compacted_live = restored;
                if restored > 0 {
                    tracing::info!("buddy auth: restored {restored} unexpired nonces from {path}");
                }
            }
            Err(e) => {
                tracing::error!("buddy auth: {e}");
                self.nonce_file.lock().unwrap().error = Some(e);
            }
        }
    }

    /// Restore the cache. A malformed line fails the whole load: skipping it
    /// would silently un-arm replay protection for that request (B-11).
    fn load_nonces(&self, path: &str, now: DateTime<Utc>) -> Result<usize, String> {
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => {
                return Err(format!(
                    "the replay cache at {path} could not be read ({e}); refusing requests until it is repaired"
                ))
            }
        };
        let mut restored = 0;
        let mut guard = self.seen.lock().unwrap();
        for (number, line) in raw.split('\n').enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let fields: Vec<&str> = line.split(' ').collect();
            if fields.len() != 3 || fields.iter().any(|field| field.is_empty()) {
                return Err(format!(
                    "the replay cache at {path} is malformed on line {}; remove or repair it before accepting peers",
                    number + 1
                ));
            }
            let expiry_nanos = fields[0].parse::<i64>().map_err(|_| {
                format!(
                    "the replay cache at {path} has an invalid expiry on line {}; remove or repair it before accepting peers",
                    number + 1
                )
            })?;
            if DateTime::from_timestamp_nanos(expiry_nanos) < now {
                continue;
            }
            guard
                .entry(fields[1].to_string())
                .or_default()
                .insert(fields[2].to_string(), expiry_nanos);
            restored += 1;
        }
        Ok(restored)
    }

    /// Append one nonce durably, then compact when the file has doubled since
    /// the last compaction. The append and the compaction run under the one
    /// nonce-file lock, so an append can never land between the compaction's
    /// snapshot and its rename and be lost.
    fn persist_nonce(&self, key_id: &str, nonce: &str, expiry_nanos: i64) -> Result<(), String> {
        let mut state = self.nonce_file.lock().unwrap();
        if let Some(error) = &state.error {
            return Err(format!(
                "the replay cache is unusable ({error}); refusing the request"
            ));
        }
        let Some(path) = state.path.clone() else {
            return Ok(());
        };
        let line = format!("{expiry_nanos} {key_id} {nonce}\n");
        let created = !std::path::Path::new(&path).exists();
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let append = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(&path)
            .and_then(|mut f| {
                f.write_all(line.as_bytes())?;
                f.sync_data()
            });
        append.map_err(|e| {
            format!("the replay cache at {path} is not writable ({e}); refusing the request")
        })?;
        if created {
            // The first append also creates the name: sync the directory so a
            // crash cannot lose the whole cache after its data was durable.
            if let Some(parent) = std::path::Path::new(&path).parent() {
                std::fs::File::open(parent)
                    .and_then(|handle| handle.sync_all())
                    .map_err(|e| {
                        format!(
                            "the replay cache directory for {path} cannot be synced ({e}); refusing the request"
                        )
                    })?;
            }
        }
        state.lines += 1;

        // Compact on a growth factor: the file must at least double since the
        // last rewrite (never on every request once past a fixed threshold).
        let baseline = state.compacted_live.max(self.nonce_compact_threshold);
        if state.lines < baseline.saturating_mul(2) {
            return Ok(());
        }
        let mut builder = String::new();
        let mut live = 0;
        {
            // `seen` is always taken after the nonce-file lock (never before),
            // so the two locks have one order.
            let guard = self.seen.lock().unwrap();
            for (k, nonces) in guard.iter() {
                for (n, exp) in nonces {
                    builder.push_str(&format!("{exp} {k} {n}\n"));
                    live += 1;
                }
            }
        }
        match crate::chartsrepo::write_atomic(&path, builder.as_bytes(), 0o600, 0o750) {
            Ok(()) => {
                state.lines = live;
                state.compacted_live = live;
            }
            Err(_) => self.warn_once("cannot compact the nonce cache"),
        }
        Ok(())
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
        if !audience.is_empty() && path == format!("{}/status", crate::buddy::PATH_PREFIX) {
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
        match self.replay(&auth.key_id, &auth.nonce, now) {
            Ok(true) => return Err("this request was already used (nonce replay)".to_string()),
            Ok(false) => {}
            Err(e) => return Err(e),
        }
        Ok(peer)
    }

    /// Record a nonce; returns true if it was already seen.
    fn replay(&self, key_id: &str, nonce: &str, now: DateTime<Utc>) -> Result<bool, String> {
        {
            let state = self.nonce_file.lock().unwrap();
            if let Some(error) = &state.error {
                return Err(format!(
                    "the replay cache is unusable ({error}); refusing the request until it is repaired"
                ));
            }
        }
        let now_nanos = now.timestamp_nanos_opt().unwrap_or(0);
        {
            let mut guard = self.seen.lock().unwrap();
            let nonces = guard.entry(key_id.to_string()).or_default();
            nonces.retain(|_, exp| *exp >= now_nanos);
            if nonces.contains_key(nonce) {
                return Ok(true);
            }
            // Never evict an unexpired nonce: doing so makes a captured
            // request replayable again. Refuse instead and let the peer retry
            // after the window (NONCE_TTL) drains.
            if nonces.len() >= self.max_nonces_per_key {
                return Err(
                    "too many unexpired signed requests for this key; retry after the replay window"
                        .to_string(),
                );
            }
            nonces.insert(nonce.to_string(), now_nanos + NONCE_TTL.as_nanos() as i64);
        }
        self.persist_nonce(key_id, nonce, now_nanos + NONCE_TTL.as_nanos() as i64)?;
        Ok(false)
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
    getrandom::fill(&mut buf).map_err(|e| format!("generating nonce: {e}"))?;
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
        let headers = sign_request_for(
            &id,
            "GET",
            &format!("{}/status", crate::buddy::PATH_PREFIX),
            &digest,
            "receiver",
            now,
        )
        .unwrap();
        let peer = auth
            .verify(
                &headers,
                &digest,
                "GET",
                &format!("{}/status", crate::buddy::PATH_PREFIX),
                "receiver",
                now,
            )
            .unwrap();
        assert_eq!(peer.name, "peer-a");
    }

    #[test]
    fn replay_refusal_at_the_cap_carries_the_retry_wording() {
        // The cap refusal is the one error the sender is allowed to retry:
        // it must fire exactly at the cap and keep the "retry after the
        // replay window" wording the client keys its backoff on.
        let id = new_identity("naslos-a").unwrap();
        let store = Arc::new(PeerStore::new(""));
        store
            .add(Peer {
                name: "peer-a".to_string(),
                public_key: id.public_key.clone(),
                enabled: true,
                ..Default::default()
            })
            .unwrap();
        let auth = Authenticator::with_nonce_cap(store, 2);
        let path = format!("{}/status", crate::buddy::PATH_PREFIX);
        let now = Utc::now();
        for i in 0..2 {
            let digest = body_digest(b"");
            let headers = sign_request_for(&id, "GET", &path, &digest, "receiver", now).unwrap();
            assert!(
                auth.verify(&headers, &digest, "GET", &path, "receiver", now)
                    .is_ok(),
                "request {i} is below the cap and must pass"
            );
        }
        let digest = body_digest(b"");
        let headers = sign_request_for(&id, "GET", &path, &digest, "receiver", now).unwrap();
        let err = auth
            .verify(&headers, &digest, "GET", &path, "receiver", now)
            .unwrap_err();
        assert!(
            err.contains("too many unexpired") && err.contains("retry after the replay window"),
            "the refusal must stay retryable-worded, got: {err}"
        );
    }

    #[test]
    fn a_non_writable_replay_cache_refuses_the_request() {
        let dir = tempfile::tempdir().unwrap();
        let id = new_identity("a").unwrap();
        let auth = authenticator_with(&id, "p");
        auth.persist_nonces(dir.path().to_str().unwrap());
        let path = dir.path().join(".nonces");
        let _ = std::fs::remove_file(&path);
        // A directory at the cache path makes append fail even when the tests
        // run as root (permission bits would not).
        std::fs::create_dir(&path).unwrap();
        let err = auth.replay("p", "nonce-123", Utc::now()).unwrap_err();
        assert!(err.contains("not writable"), "{err}");
    }

    #[test]
    fn replay_is_rejected() {
        let id = new_identity("a").unwrap();
        let auth = authenticator_with(&id, "p");
        let now = Utc::now();
        let digest = body_digest(b"");
        let h = sign_request_for(
            &id,
            "GET",
            &format!("{}/status", crate::buddy::PATH_PREFIX),
            &digest,
            "r",
            now,
        )
        .unwrap();
        auth.verify(
            &h,
            &digest,
            "GET",
            &format!("{}/status", crate::buddy::PATH_PREFIX),
            "r",
            now,
        )
        .unwrap();
        let err = auth
            .verify(
                &h,
                &digest,
                "GET",
                &format!("{}/status", crate::buddy::PATH_PREFIX),
                "r",
                now,
            )
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
        let h = sign_request_for(
            &id,
            "GET",
            &format!("{}/status", crate::buddy::PATH_PREFIX),
            &digest,
            "",
            now,
        )
        .unwrap();
        // On /status, the empty-audience signature is accepted alongside the real one.
        assert!(auth
            .verify(
                &h,
                &digest,
                "GET",
                &format!("{}/status", crate::buddy::PATH_PREFIX),
                "receiver",
                now
            )
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

    #[test]
    fn a_malformed_nonce_file_refuses_verification() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".nonces"), "not-a-cache-line\n").unwrap();
        let id = new_identity("a").unwrap();
        let auth = authenticator_with(&id, "p");
        auth.persist_nonces(dir.path().to_str().unwrap());
        let now = Utc::now();
        let digest = body_digest(b"");
        let path = format!("{}/status", crate::buddy::PATH_PREFIX);
        let headers = sign_request_for(&id, "GET", &path, &digest, "r", now).unwrap();
        let err = auth
            .verify(&headers, &digest, "GET", &path, "r", now)
            .unwrap_err();
        assert!(err.contains("replay cache"), "{err}");
    }

    #[test]
    fn concurrent_appends_and_compactions_keep_every_nonce() {
        const THREADS: usize = 6;
        const PER_THREAD: usize = 25;
        let dir = tempfile::tempdir().unwrap();
        let auth =
            Arc::new(Authenticator::new(Arc::new(PeerStore::new(""))).with_compact_threshold(8));
        auth.persist_nonces(dir.path().to_str().unwrap());
        let now = Utc::now();
        let mut handles = Vec::new();
        for thread in 0..THREADS {
            let auth = auth.clone();
            handles.push(std::thread::spawn(move || {
                for index in 0..PER_THREAD {
                    auth.replay("key", &format!("nonce-{thread}-{index}"), now)
                        .expect("append");
                }
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }

        // Every line is a complete record after interleaved compactions.
        let raw = std::fs::read_to_string(dir.path().join(".nonces")).unwrap();
        for line in raw.split('\n').filter(|line| !line.trim().is_empty()) {
            let fields: Vec<&str> = line.split(' ').collect();
            assert_eq!(fields.len(), 3, "malformed line {line:?}");
            fields[0].parse::<i64>().expect("expiry is a number");
        }

        // A restart restores every nonce: a compaction never dropped an
        // append that raced with it.
        let reloaded = Authenticator::new(Arc::new(PeerStore::new("")));
        reloaded.persist_nonces(dir.path().to_str().unwrap());
        for thread in 0..THREADS {
            for index in 0..PER_THREAD {
                assert!(
                    reloaded
                        .replay("key", &format!("nonce-{thread}-{index}"), now)
                        .expect("replay check"),
                    "nonce {thread}/{index} was lost by an interleaved compaction"
                );
            }
        }
    }
}
