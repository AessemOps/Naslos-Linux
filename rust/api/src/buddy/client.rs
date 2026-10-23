//! Buddy sender client (port of `api/internal/buddy/client.go`).
//!
//! A sender pushes backups the receiver cannot read: it seals each chunk, uploads
//! only what is missing, then publishes a signed manifest. The same client drives
//! restores, verifying the signature, the chain's contiguity and every chunk's
//! digest against the signed manifest.

use base64::Engine;
use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt};

use super::auth::{body_digest, sign_request_for};
use super::envelope::{
    chunk_plain_len, digest_of, new_stream_prefix, open_chunk, seal_chunk, unwrap_dek, wrap_dek,
    Manifest, ManifestChunk, CHUNK_PLAIN_SIZE, ENVELOPE_VERSION, MAX_SEALED_CHUNK_SIZE,
};
use super::keys::{public_key_of, Identity};
use super::peers::validate_source;

const MAX_MANIFEST_BYTES: usize = 64 << 20;
const SEALED_OVERHEAD: usize = 4 + 4 + 12 + 16;
pub const CHAIN_STATE_VERSION: i32 = 1;

/// The receiver's identity and stored state, as returned by `/status`.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct Status {
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub receiver: String,
    #[serde(default, rename = "keyId")]
    pub key_id: String,
    #[serde(default, rename = "freeBytes")]
    pub free_bytes: i64,
    #[serde(default, rename = "usedBytes")]
    pub used_bytes: i64,
    #[serde(default, rename = "quotaBytes")]
    pub quota_bytes: i64,
    #[serde(default, rename = "lastBackup")]
    pub last_backup: Option<DateTime<Utc>>,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub chains: HashMap<String, String>,
    #[serde(default, rename = "enrollOpen")]
    pub enroll_open: bool,
    #[serde(default)]
    pub peers: Vec<String>,
    #[serde(default, rename = "serverTime")]
    pub server_time: Option<DateTime<Utc>>,
    #[serde(default, rename = "protocolVersion")]
    pub protocol_version: i32,
}

/// One stored chain of a source.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ChainSummary {
    pub chain: String,
    pub kind: String,
    #[serde(rename = "createdAt", default)]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(rename = "fromSnapshot", default)]
    pub from_snapshot: String,
    #[serde(rename = "toSnapshot", default)]
    pub to_snapshot: String,
    #[serde(rename = "fromGUID", default)]
    pub from_guid: String,
    #[serde(rename = "toGUID", default)]
    pub to_guid: String,
    #[serde(default)]
    pub chunks: usize,
}

/// A stored chunk's digest and nonce generation.
#[derive(Debug, Clone, Default)]
pub struct StoredChunk {
    pub digest: String,
    pub generation: u16,
}

/// One progress tick of a push.
#[derive(Debug, Clone, Default)]
pub struct PushProgress {
    pub chunk: i64,
    pub plain_bytes: i64,
    pub sealed_bytes: i64,
    pub uploaded: usize,
    pub skipped: usize,
}

/// The outcome of a push.
#[derive(Debug, Clone, Default)]
pub struct PushResult {
    pub source: String,
    pub chain: String,
    pub chunks: usize,
    pub uploaded: usize,
    pub skipped: usize,
    pub plain_bytes: i64,
    pub sealed_bytes: i64,
    pub pruned_chains: usize,
    pub short_tail: bool,
    pub state: Option<ChainState>,
    pub manifest: Option<Manifest>,
}

/// Options for a push.
#[allow(clippy::type_complexity)]
pub struct PushOptions<'a> {
    pub source: &'a str,
    pub kind: &'a str,
    pub reader: &'a mut (dyn AsyncRead + Unpin + Send),
    pub state: Option<ChainState>,
    pub from_snapshot: &'a str,
    pub to_snapshot: &'a str,
    pub from_guid: &'a str,
    pub to_guid: &'a str,
    pub prune_keep: usize,
    pub progress: Option<&'a (dyn Fn(PushProgress) + Send + Sync)>,
    pub before_publish: Option<&'a (dyn Fn(&PushResult) -> Result<(), String> + Send + Sync)>,
}

/// The outcome of a restore.
#[derive(Debug, Clone, Default)]
pub struct RestoreResult {
    pub source: String,
    pub chain: String,
    pub chunks: usize,
    pub plain_bytes: i64,
    pub manifest: Option<Manifest>,
}

/// Options for a restore.
pub struct RestoreOptions<'a> {
    pub source: &'a str,
    pub chain: &'a str,
    pub out: &'a mut (dyn tokio::io::AsyncWrite + Unpin + Send),
    pub progress: Option<&'a (dyn Fn(i64, usize, i64) + Send + Sync)>,
    pub skip_signature_check: bool,
}

/// Resume state for an interrupted push. Holds the chain's DEK, so it is as
/// sensitive as the identity and must be stored 0600.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ChainState {
    pub version: i32,
    pub source: String,
    pub chain: String,
    pub kind: String,
    pub dek: String,
    pub prefix: String,
    #[serde(default)]
    pub generation: u16,
    #[serde(rename = "startedAt", default)]
    pub started_at: Option<DateTime<Utc>>,
}

impl ChainState {
    /// Start a fresh chain segment for a source.
    pub fn new(source: &str, kind: &str) -> Result<Self, String> {
        validate_source(source)?;
        let kind = if kind.is_empty() { "tar" } else { kind };
        let mut id = [0u8; 8];
        getrandom::getrandom(&mut id).map_err(|e| format!("generating chain id: {e}"))?;
        let dek = super::envelope::generate_dek()?;
        let prefix = new_stream_prefix()?;
        Ok(Self {
            version: CHAIN_STATE_VERSION,
            source: source.to_string(),
            chain: id.iter().map(|b| format!("{b:02x}")).collect(),
            kind: kind.to_string(),
            dek: base64::engine::general_purpose::STANDARD.encode(dek),
            prefix: base64::engine::general_purpose::STANDARD.encode(prefix),
            generation: 0,
            started_at: Some(Utc::now()),
        })
    }

    pub fn load(path: &str) -> Result<Self, String> {
        let data = std::fs::read(path).map_err(|e| e.to_string())?;
        serde_json::from_slice(&data).map_err(|e| format!("parsing chain state {path}: {e}"))
    }

    pub fn save(&self, path: &str) -> Result<(), String> {
        let data = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        let dir = std::path::Path::new(path)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        if dir != std::path::Path::new(".") {
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        }
        let tmp = dir.join(".chain.tmp");
        crate::chartsrepo::write_atomic(&tmp.display().to_string(), &data, 0o600, 0o700)?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    }

    fn dek(&self) -> Result<Vec<u8>, String> {
        let dek = base64::engine::general_purpose::STANDARD
            .decode(&self.dek)
            .map_err(|e| format!("decoding chain key: {e}"))?;
        if dek.len() != 32 {
            return Err(format!("chain key must be 32 bytes, got {}", dek.len()));
        }
        Ok(dek)
    }

    fn prefix(&self) -> Result<Vec<u8>, String> {
        let prefix = base64::engine::general_purpose::STANDARD
            .decode(&self.prefix)
            .map_err(|e| format!("decoding stream prefix: {e}"))?;
        if prefix.len() != 6 {
            return Err("stream prefix must be 6 bytes".to_string());
        }
        Ok(prefix)
    }
}

/// Guard the chain id that goes into URLs.
pub fn valid_chain_id(chain: &str) -> Result<(), String> {
    if chain.is_empty() {
        return Err("chain id is required".to_string());
    }
    if chain.len() > 64 {
        return Err("chain id must be 64 characters or fewer".to_string());
    }
    if !chain
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    {
        return Err(format!(
            "invalid chain id {chain:?}: use lowercase letters and digits"
        ));
    }
    Ok(())
}

#[derive(Default)]
struct AudienceCache {
    audience: String,
    known: bool,
}

/// A buddy sender.
pub struct Client {
    pub identity: Arc<Identity>,
    pub base_url: String,
    http: reqwest::Client,
    pub sequence_dir: String,
    audience: tokio::sync::Mutex<AudienceCache>,
}

impl Client {
    pub fn new(base_url: &str, identity: Arc<Identity>) -> Self {
        Self {
            identity,
            base_url: base_url.trim_end_matches('/').to_string(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(300))
                .build()
                .unwrap_or_default(),
            sequence_dir: String::new(),
            audience: tokio::sync::Mutex::new(AudienceCache::default()),
        }
    }

    fn sequence_path(&self, source: &str) -> Option<std::path::PathBuf> {
        if self.sequence_dir.is_empty() {
            return None;
        }
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(format!("{}|{source}", self.base_url).as_bytes());
        let digest = h.finalize();
        let name: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
        Some(std::path::Path::new(&self.sequence_dir).join(format!("{name}.seq")))
    }

    fn last_published_sequence(&self, source: &str) -> Result<u64, String> {
        let Some(path) = self.sequence_path(source) else {
            return Ok(0);
        };
        match std::fs::read_to_string(&path) {
            Ok(raw) => raw
                .trim()
                .parse()
                .map_err(|e| format!("parsing the stored publish sequence for {source}: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(e.to_string()),
        }
    }

    fn record_published_sequence(&self, source: &str, seq: u64) -> Result<(), String> {
        let Some(path) = self.sequence_path(source) else {
            return Ok(());
        };
        let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        crate::chartsrepo::write_atomic(
            &path.display().to_string(),
            format!("{seq}\n").as_bytes(),
            0o600,
            0o700,
        )
    }

    async fn do_request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: &[u8],
    ) -> Result<(Vec<u8>, u16), String> {
        let audience = self.audience_for(path).await?;
        self.send(method, path, body, &audience).await
    }

    async fn audience_for(&self, path: &str) -> Result<String, String> {
        let mut cache = self.audience.lock().await;
        if !cache.audience.is_empty() {
            return Ok(cache.audience.clone());
        }
        if path == "/status" {
            return Ok(String::new());
        }
        if cache.known {
            return Ok(String::new());
        }
        // Discover the receiver identity from the read-only /status.
        let result = self.send(reqwest::Method::GET, "/status", b"", "").await;
        cache.known = true;
        match result {
            Ok((data, _)) => {
                if let Ok(status) = serde_json::from_slice::<Status>(&data) {
                    cache.audience = status.receiver;
                }
                Ok(cache.audience.clone())
            }
            Err(_) => Ok(String::new()),
        }
    }

    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: &[u8],
        audience: &str,
    ) -> Result<(Vec<u8>, u16), String> {
        // Sign the full URI (prefix included): the receiver rebuilds the path it
        // signed, so a prefix-stripping proxy cannot change what it means.
        let signed_path = format!("{}{}", super::PATH_PREFIX, path);
        let uri = format!("{}{}", self.base_url, signed_path);
        let digest = body_digest(body);
        let auth = sign_request_for(
            &self.identity,
            method.as_str(),
            &signed_path,
            &digest,
            audience,
            Utc::now(),
        )?;

        let mut rb = self
            .http
            .request(method.clone(), &uri)
            .header(super::auth::HEADER_KEY_ID, auth.key_id)
            .header(super::auth::HEADER_TIMESTAMP, auth.timestamp)
            .header(super::auth::HEADER_NONCE, auth.nonce)
            .header(super::auth::HEADER_SIGNATURE, auth.signature);
        if !body.is_empty() {
            rb = rb
                .header("Content-Type", "application/json")
                .body(body.to_vec());
        }
        let resp = rb.send().await.map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(format!(
                "{} {path}: response exceeded the {MAX_MANIFEST_BYTES}-byte cap",
                method.as_str()
            ));
        }
        if status >= 400 {
            return Err(format!(
                "{} {path}: {}",
                method.as_str(),
                describe_error(status, &bytes)
            ));
        }
        Ok((bytes.to_vec(), status))
    }

    pub async fn status(&self) -> Result<Status, String> {
        let (data, _) = self
            .do_request(reqwest::Method::GET, "/status", b"")
            .await?;
        let status: Status =
            serde_json::from_slice(&data).map_err(|e| format!("parsing status: {e}"))?;
        let mut cache = self.audience.lock().await;
        cache.audience = status.receiver.clone();
        cache.known = true;
        Ok(status)
    }

    pub async fn enroll(
        &self,
        token: &str,
        name: &str,
        sources: &[String],
    ) -> Result<String, String> {
        let payload = serde_json::json!({
            "token": token, "name": name, "publicKey": self.identity.public_key, "sources": sources
        });
        let body = serde_json::to_vec(&payload).map_err(|e| e.to_string())?;
        let (data, _) = self
            .do_request(reqwest::Method::POST, "/enroll", &body)
            .await?;
        let resp: serde_json::Value = serde_json::from_slice(&data)
            .map_err(|e| format!("parsing enrollment response: {e}"))?;
        Ok(resp
            .get("fingerprint")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    }

    pub async fn backups(&self) -> Result<Vec<super::store::Backups>, String> {
        let (data, _) = self
            .do_request(reqwest::Method::GET, "/backups", b"")
            .await?;
        #[derive(serde::Deserialize)]
        struct Resp {
            #[serde(default)]
            backups: Vec<super::store::Backups>,
        }
        let resp: Resp =
            serde_json::from_slice(&data).map_err(|e| format!("parsing backups: {e}"))?;
        Ok(resp.backups)
    }

    pub async fn manifest(&self, source: &str, chain: &str) -> Result<Manifest, String> {
        validate_source(source)?;
        let mut path = format!("/manifest/{source}");
        if !chain.is_empty() {
            path.push_str(&format!("?chain={chain}"));
        }
        let (data, _) = self.do_request(reqwest::Method::GET, &path, b"").await?;
        serde_json::from_slice(&data).map_err(|e| format!("parsing manifest: {e}"))
    }

    async fn stored_chunks(
        &self,
        source: &str,
        chain: &str,
    ) -> Result<(HashMap<i64, StoredChunk>, bool), String> {
        validate_source(source)?;
        let path = format!("/chunks/{source}?chain={chain}");
        let (data, _) = self.do_request(reqwest::Method::GET, &path, b"").await?;
        #[derive(serde::Deserialize)]
        struct Entry {
            index: i64,
            #[serde(default)]
            digest: String,
            #[serde(default)]
            generation: u16,
        }
        #[derive(serde::Deserialize)]
        struct Resp {
            #[serde(default)]
            published: bool,
            #[serde(default)]
            chunks: Vec<Entry>,
        }
        let resp: Resp =
            serde_json::from_slice(&data).map_err(|e| format!("parsing chunk list: {e}"))?;
        let mut stored = HashMap::new();
        for e in resp.chunks {
            stored.insert(
                e.index,
                StoredChunk {
                    digest: e.digest,
                    generation: e.generation,
                },
            );
        }
        Ok((stored, resp.published))
    }

    pub async fn chains(&self, source: &str) -> Result<Vec<ChainSummary>, String> {
        validate_source(source)?;
        let (data, _) = self
            .do_request(reqwest::Method::GET, &format!("/chains/{source}"), b"")
            .await?;
        #[derive(serde::Deserialize)]
        struct Resp {
            #[serde(default)]
            chains: Vec<ChainSummary>,
        }
        let resp: Resp =
            serde_json::from_slice(&data).map_err(|e| format!("parsing chain list: {e}"))?;
        Ok(resp.chains)
    }

    async fn sequence(&self, source: &str, chain: &str) -> Result<Vec<ChainSummary>, String> {
        validate_source(source)?;
        let mut path = format!("/sequence/{source}");
        if !chain.is_empty() {
            path.push_str(&format!("?chain={chain}"));
        }
        let (data, _) = self.do_request(reqwest::Method::GET, &path, b"").await?;
        #[derive(serde::Deserialize)]
        struct Resp {
            #[serde(default)]
            chains: Vec<ChainSummary>,
        }
        let resp: Resp = serde_json::from_slice(&data)
            .map_err(|e| format!("parsing the restore sequence: {e}"))?;
        if resp.chains.is_empty() {
            return Err(format!("no backup of {source} is stored here"));
        }
        Ok(resp.chains)
    }

    /// The chains needed to rebuild a source from scratch, oldest first.
    pub async fn restore_sequence(
        &self,
        source: &str,
        chain: &str,
    ) -> Result<Vec<ChainSummary>, String> {
        if let Ok(seq) = self.sequence(source, chain).await {
            return Ok(seq);
        }
        let chains = self.chains(source).await?;
        if chains.is_empty() {
            return Err(format!("no backup of {source} is stored here"));
        }
        let target = if chain.is_empty() {
            chains[0].clone()
        } else {
            chains
                .iter()
                .find(|c| c.chain == chain)
                .cloned()
                .ok_or_else(|| format!("chain {chain} is not stored for {source}"))?
        };
        let by_guid: HashMap<&str, &ChainSummary> = chains
            .iter()
            .filter(|c| !c.to_guid.is_empty())
            .map(|c| (c.to_guid.as_str(), c))
            .collect();
        let mut sequence = vec![target.clone()];
        let mut seen = HashSet::new();
        seen.insert(target.chain.clone());
        let mut current = target;
        while !current.from_guid.is_empty() {
            let Some(base) = by_guid.get(current.from_guid.as_str()) else {
                return Err(format!(
                    "chain {} needs the chain that produced GUID {}, which is not stored here: the backup cannot be rebuilt from this receiver",
                    current.chain, current.from_guid
                ));
            };
            if !seen.insert(base.chain.clone()) {
                return Err(format!(
                    "the chain history of {source} contains a loop at {}",
                    base.chain
                ));
            }
            sequence.push((*base).clone());
            current = (*base).clone();
        }
        sequence.reverse();
        Ok(sequence)
    }

    pub async fn prune(&self, source: &str, keep: usize) -> Result<usize, String> {
        validate_source(source)?;
        let body =
            serde_json::to_vec(&serde_json::json!({ "keep": keep })).map_err(|e| e.to_string())?;
        let (data, _) = self
            .do_request(reqwest::Method::POST, &format!("/prune/{source}"), &body)
            .await?;
        let resp: serde_json::Value =
            serde_json::from_slice(&data).map_err(|e| format!("parsing prune response: {e}"))?;
        Ok(resp
            .get("removedChains")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize)
    }

    /// Stream the plaintext, seal chunk by chunk, upload what is missing and
    /// publish a signed manifest.
    pub async fn push(&self, opts: PushOptions<'_>) -> (PushResult, Option<String>) {
        let mut result = PushResult {
            source: opts.source.to_string(),
            ..Default::default()
        };
        if let Err(e) = validate_source(opts.source) {
            return (result, Some(e));
        }
        let kind = if opts.kind.is_empty() {
            "tar"
        } else {
            opts.kind
        };
        let mut state = match opts.state.clone() {
            Some(s) => s,
            None => match ChainState::new(opts.source, kind) {
                Ok(s) => s,
                Err(e) => return (result, Some(e)),
            },
        };
        if state.source != opts.source {
            return (
                result,
                Some(format!(
                    "resume state belongs to {}, not {}",
                    state.source, opts.source
                )),
            );
        }
        if let Err(e) = valid_chain_id(&state.chain) {
            return (result, Some(e));
        }
        result.chain = state.chain.clone();
        let dek = match state.dek() {
            Ok(d) => d,
            Err(e) => return (result, Some(e)),
        };
        let prefix = match state.prefix() {
            Ok(p) => p,
            Err(e) => return (result, Some(e)),
        };
        let (stored, published) = match self.stored_chunks(opts.source, &state.chain).await {
            Ok(v) => v,
            Err(e) => return (result, Some(e)),
        };
        let tail_index = stored.keys().copied().max().unwrap_or(-1);
        let last_sequence = match self.last_published_sequence(opts.source) {
            Ok(s) => s,
            Err(e) => return (result, Some(e)),
        };
        let sequence = last_sequence + 1;

        let mut manifest = Manifest {
            version: ENVELOPE_VERSION,
            source: opts.source.to_string(),
            chain: state.chain.clone(),
            kind: state.kind.clone(),
            from_snapshot: opts.from_snapshot.to_string(),
            to_snapshot: opts.to_snapshot.to_string(),
            from_guid: opts.from_guid.to_string(),
            to_guid: opts.to_guid.to_string(),
            created_at: Utc::now(),
            key_id: String::new(),
            sequence,
            generation: state.generation,
            stream_prefix: state.prefix.clone(),
            chunk_plain_size: CHUNK_PLAIN_SIZE as i64,
            chunks: Vec::new(),
            dek_wrapped: String::new(),
            signature: String::new(),
        };
        result.state = Some(state.clone());

        let mut generation = state.generation;
        let mut max_generation = generation;
        let mut buf = vec![0u8; CHUNK_PLAIN_SIZE];
        let mut index: i64 = 0;
        loop {
            let (n, short_tail) = match read_full(opts.reader, &mut buf).await {
                Ok(v) => v,
                Err(e) => return (result, Some(format!("reading input: {e}"))),
            };
            if n == 0 && !short_tail {
                break;
            }
            if short_tail {
                result.short_tail = true;
            }
            let plain = &buf[..n];
            let mut chunk_generation = generation;
            let stored_info = stored.get(&index).cloned();
            if let Some(info) = &stored_info {
                chunk_generation = info.generation;
            }
            let (mut sealed, mut sha) = match seal_chunk(
                &dek,
                &prefix,
                chunk_generation,
                opts.source,
                &state.chain,
                index,
                plain,
            ) {
                Ok(v) => v,
                Err(e) => return (result, Some(e)),
            };
            let mut entry = ManifestChunk {
                index,
                plain_bytes: n as i64,
                sealed_bytes: sealed.len() as i64,
                generation: chunk_generation,
                sha256_plain: sha.clone(),
            };

            let mut upload = true;
            if let Some(info) = &stored_info {
                if info.digest != digest_of(&sealed) {
                    if published || index != tail_index {
                        return (
                            result,
                            Some(format!(
                                "the receiver already holds different bytes for chunk {index} of chain {}: the source changed since the push was interrupted, start a new chain",
                                state.chain
                            )),
                        );
                    }
                    if info.generation == u16::MAX {
                        return (result, Some(format!("chain {} cannot be resumed: its nonce generation counter is exhausted", state.chain)));
                    }
                    chunk_generation = info.generation + 1;
                    match seal_chunk(
                        &dek,
                        &prefix,
                        chunk_generation,
                        opts.source,
                        &state.chain,
                        index,
                        plain,
                    ) {
                        Ok((s, h)) => {
                            sealed = s;
                            sha = h;
                            entry = ManifestChunk {
                                index,
                                plain_bytes: n as i64,
                                sealed_bytes: sealed.len() as i64,
                                generation: chunk_generation,
                                sha256_plain: sha.clone(),
                            };
                            generation = chunk_generation;
                        }
                        Err(e) => return (result, Some(e)),
                    }
                } else {
                    entry.sealed_bytes = n as i64 + SEALED_OVERHEAD as i64;
                    upload = false;
                }
            }
            if entry.generation > max_generation {
                max_generation = entry.generation;
            }

            if upload {
                let path = format!(
                    "/chunks/{}?chain={}&index={index}",
                    opts.source, state.chain
                );
                if let Err(e) = self.do_request(reqwest::Method::PUT, &path, &sealed).await {
                    return (result, Some(e));
                }
                result.uploaded += 1;
            } else {
                result.skipped += 1;
            }

            result.chunks += 1;
            result.plain_bytes += n as i64;
            result.sealed_bytes += entry.sealed_bytes;
            manifest.chunks.push(entry);
            if let Some(progress) = opts.progress {
                progress(PushProgress {
                    chunk: index,
                    plain_bytes: result.plain_bytes,
                    sealed_bytes: result.sealed_bytes,
                    uploaded: result.uploaded,
                    skipped: result.skipped,
                });
            }
            index += 1;
            if short_tail {
                break;
            }
        }

        if manifest.chunks.is_empty() {
            return (
                result,
                Some("nothing to back up: the input stream produced no data".to_string()),
            );
        }
        if max_generation > generation {
            generation = max_generation;
        }
        state.generation = generation;
        manifest.generation = generation;

        if let Some(before) = opts.before_publish {
            if let Err(e) = before(&result) {
                return (result, Some(e));
            }
        }

        let kek = match self.identity.kek_bytes() {
            Ok(k) => k,
            Err(e) => return (result, Some(e)),
        };
        let wrapped = match wrap_dek(&kek, &dek, opts.source, &state.chain) {
            Ok(w) => w,
            Err(e) => return (result, Some(e)),
        };
        manifest.dek_wrapped = wrapped;
        if let Err(e) = manifest.sign(&self.identity) {
            return (result, Some(e));
        }
        let payload = match serde_json::to_vec(&manifest) {
            Ok(p) => p,
            Err(e) => return (result, Some(e.to_string())),
        };
        if payload.len() > MAX_MANIFEST_BYTES {
            return (
                result,
                Some(format!(
                    "chain {} has {} chunks and its manifest is {} bytes, over the {}-byte limit",
                    state.chain,
                    manifest.chunks.len(),
                    payload.len(),
                    MAX_MANIFEST_BYTES
                )),
            );
        }
        if let Err(e) = self
            .do_request(
                reqwest::Method::PUT,
                &format!("/manifest/{}", opts.source),
                &payload,
            )
            .await
        {
            return (result, Some(e));
        }
        if let Err(e) = self.record_published_sequence(opts.source, sequence) {
            return (result, Some(e));
        }
        if opts.prune_keep > 0 {
            match self.prune(opts.source, opts.prune_keep).await {
                Ok(removed) => result.pruned_chains = removed,
                Err(e) => return (result, Some(e)),
            }
        }
        result.state = Some(state);
        result.manifest = Some(manifest);
        (result, None)
    }

    /// Pull the stored chunks back, decrypt them and write the original stream.
    pub async fn restore(&self, opts: RestoreOptions<'_>) -> (RestoreResult, Option<String>) {
        let mut result = RestoreResult {
            source: opts.source.to_string(),
            ..Default::default()
        };
        if let Err(e) = validate_source(opts.source) {
            return (result, Some(e));
        }
        let manifest = match self.manifest(opts.source, opts.chain).await {
            Ok(m) => m,
            Err(e) => return (result, Some(e)),
        };
        if manifest.version != ENVELOPE_VERSION {
            return (result, Some(format!("refusing to restore: the receiver returned a v{} manifest, this sender speaks v{ENVELOPE_VERSION}", manifest.version)));
        }
        if manifest.source != opts.source {
            return (
                result,
                Some(format!(
                    "refusing to restore: the receiver returned a manifest for {:?}, not {:?}",
                    manifest.source, opts.source
                )),
            );
        }
        if !opts.chain.is_empty() && manifest.chain != opts.chain {
            return (
                result,
                Some(format!(
                    "refusing to restore: the receiver returned chain {:?}, not the requested {:?}",
                    manifest.chain, opts.chain
                )),
            );
        }
        if let Err(e) = manifest.verify_signature(&self.identity.public_key) {
            if !opts.skip_signature_check {
                return (result, Some(format!("refusing to restore: {e}")));
            }
        }
        if opts.chain.is_empty() {
            match self.last_published_sequence(opts.source) {
                Ok(last) if manifest.sequence < last => {
                    return (result, Some(format!("refusing to restore: the receiver returned a stale manifest (sequence {}, latest published {last})", manifest.sequence)));
                }
                Err(e) => return (result, Some(e)),
                _ => {}
            }
        }
        let prefix = match manifest.stream_prefix_bytes() {
            Ok(p) => p,
            Err(e) => return (result, Some(format!("refusing to restore: {e}"))),
        };
        let kek = match self.identity.kek_bytes() {
            Ok(k) => k,
            Err(e) => return (result, Some(e)),
        };
        let dek = match unwrap_dek(
            &kek,
            &manifest.dek_wrapped,
            &manifest.source,
            &manifest.chain,
        ) {
            Ok(d) => d,
            Err(e) => return (result, Some(e)),
        };

        result.chain = manifest.chain.clone();
        result.manifest = Some(manifest.clone());
        let mut chunks = manifest.chunks.clone();
        chunks.sort_by_key(|c| c.index);
        for (expected, chunk) in chunks.iter().enumerate() {
            if chunk.index != expected as i64 {
                return (
                    result,
                    Some(format!(
                        "chain {} is not contiguous: expected chunk {expected}, found {}",
                        manifest.chain, chunk.index
                    )),
                );
            }
            let path = format!(
                "/chunks/{}?chain={}&index={}",
                manifest.source, manifest.chain, chunk.index
            );
            let sealed = match self.do_request(reqwest::Method::GET, &path, b"").await {
                Ok((data, _)) => data,
                Err(e) => return (result, Some(e)),
            };
            let plain = match open_chunk(
                &dek,
                &prefix,
                chunk.generation,
                &manifest.source,
                &manifest.chain,
                chunk.index,
                &sealed,
            ) {
                Ok(p) => p,
                Err(e) => return (result, Some(e)),
            };
            if digest_of(&plain) != chunk.sha256_plain {
                return (
                    result,
                    Some(format!(
                        "chunk {} does not match the signed manifest",
                        chunk.index
                    )),
                );
            }
            use tokio::io::AsyncWriteExt;
            if let Err(e) = opts.out.write_all(&plain).await {
                return (result, Some(format!("writing output: {e}")));
            }
            result.chunks += 1;
            result.plain_bytes += plain.len() as i64;
            if let Some(progress) = opts.progress {
                progress(chunk.index, chunks.len(), result.plain_bytes);
            }
        }
        (result, None)
    }
}

/// Read up to `buf.len()` bytes, returning (n, short_tail). `short_tail` is true
/// when the stream ended part-way through the buffer (Go's ErrUnexpectedEOF).
async fn read_full(
    reader: &mut (dyn AsyncRead + Unpin + Send),
    buf: &mut [u8],
) -> Result<(usize, bool), String> {
    let mut filled = 0usize;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]).await {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) => return Err(e.to_string()),
        }
    }
    let short_tail = filled > 0 && filled < buf.len();
    Ok((filled, short_tail))
}

fn describe_error(status: u16, data: &[u8]) -> String {
    if let Ok(payload) = serde_json::from_slice::<serde_json::Value>(data) {
        if let Some(err) = payload.get("error").and_then(|v| v.as_str()) {
            if !err.is_empty() {
                return format!("{err} ({status})");
            }
        }
    }
    let text = String::from_utf8_lossy(data).trim().to_string();
    let text = if text.is_empty() {
        "<no body>".to_string()
    } else {
        text
    };
    let text = if text.len() > 300 {
        format!("{}...", &text[..300])
    } else {
        text
    };
    format!("{text} ({status})")
}

// keep imports used by the module's optional helpers
#[allow(unused_imports)]
use chunk_plain_len as _chunk_plain_len;
#[allow(unused_imports)]
use public_key_of as _public_key_of;
#[allow(unused_imports)]
use MAX_SEALED_CHUNK_SIZE as _MAX_SEALED_CHUNK_SIZE;
