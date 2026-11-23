//! Buddy receiver storage (port of `api/internal/buddy/store.go`).
//!
//! Opaque sealed chunks plus signed manifests rooted at one directory. Nothing
//! here can decrypt: the store never sees a key.

use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use super::envelope::{
    chunk_plain_len, validate_chunk_index, CHUNK_PLAIN_SIZE, MAX_SEALED_CHUNK_SIZE,
};
use super::Manifest;

const ENROLLMENT_MARKER: &str = ".enroll-used";
const MAX_LISTED_CHAINS: usize = 500;
const LINKS_FILE: &str = "links.json";
const STREAM_PREFIX_LEN: usize = 6;
const NONCE_SIZE: usize = 12;
const CHUNK_MAGIC: &[u8; 4] = b"NBC2";
/// Magic + plain length + nonce + one AES-GCM tag. Anything shorter cannot be
/// a sealed chunk, whatever its claims.
const MIN_SEALED_CHUNK_LEN: usize = CHUNK_MAGIC.len() + 4 + NONCE_SIZE + 16;
/// Quota and free-space accounting floor: every stored file is billed as at
/// least one filesystem block (and rounded up to whole blocks), so a sender
/// cannot create unbounded files/inodes with zero- or one-byte writes while
/// `delta == 0`.
const BILLING_BLOCK: i64 = 4096;
/// Per-chain chunk cap (1 TiB at 1 MiB chunks). The wire format allows more
/// indices, but no legitimate chain needs them and an unbounded count is a
/// cheaper denial-of-service than bytes.
const MAX_CHAIN_CHUNKS: i64 = 1 << 20;

/// A storage error, with quota/no-space distinguishable for the HTTP layer.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("quota exceeded: {used} of {quota} bytes are already stored for this key")]
    Quota { used: i64, quota: i64 },
    #[error("not enough free space on the receiver")]
    NoSpace,
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Other(String),
}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        StoreError::Other(e.to_string())
    }
}

impl From<String> for StoreError {
    fn from(s: String) -> Self {
        StoreError::Other(s)
    }
}

type SResult<T> = Result<T, StoreError>;

fn other(msg: impl Into<String>) -> StoreError {
    StoreError::Other(msg.into())
}

/// The per-key usage cache. Usage and the loaded set live under one lock so a
/// disk scan can never interleave with a reserve and double- or under-count
/// (B-2).
#[derive(Default)]
struct UsageCache {
    usage: HashMap<String, i64>,
    loaded: HashSet<String>,
}

/// The storage root plus the per-key usage cache.
pub struct Store {
    root: String,
    usage: Mutex<UsageCache>,
    /// One lock per chunk path, so a stat + quota reserve + rename of the same
    /// index cannot interleave with a concurrent replacement (R-058).
    chunk_locks: Mutex<HashMap<String, Weak<Mutex<()>>>>,
}

impl Store {
    pub fn new(dir: &str) -> Self {
        Self {
            root: dir.to_string(),
            usage: Mutex::new(UsageCache::default()),
            chunk_locks: Mutex::new(HashMap::new()),
        }
    }

    fn chunk_lock(&self, key: &str) -> Arc<Mutex<()>> {
        let mut locks = self.chunk_locks.lock().unwrap();
        if let Some(existing) = locks.get(key).and_then(Weak::upgrade) {
            return existing;
        }
        if locks.len() > 1024 {
            locks.retain(|_, weak| weak.strong_count() > 0);
        }
        let lock = Arc::new(Mutex::new(()));
        locks.insert(key.to_string(), Arc::downgrade(&lock));
        lock
    }

    pub fn root(&self) -> &str {
        &self.root
    }

    fn root_path(&self) -> PathBuf {
        PathBuf::from(&self.root)
    }

    /// Record that the one-time enrollment token has been spent.
    pub fn mark_enrollment_used(&self) -> SResult<()> {
        std::fs::create_dir_all(&self.root)?;
        write_file_atomic(
            &self.root_path().join(ENROLLMENT_MARKER),
            format!("{}\n", Utc::now().to_rfc3339()).as_bytes(),
        )
    }

    pub fn enrollment_used(&self) -> bool {
        self.root_path().join(ENROLLMENT_MARKER).exists()
    }

    fn sources_in(&self, key_dir_name: &str) -> SResult<Vec<String>> {
        let root = self.root_path().join(key_dir_name);
        if !root.exists() {
            return Ok(Vec::new());
        }
        let mut sources = Vec::new();
        walk_sources(&root, &root, &mut sources)?;
        sources.sort();
        Ok(sources)
    }

    fn source_dir_in(&self, key_dir_name: &str, source: &str) -> SResult<PathBuf> {
        super::peers::validate_source(source).map_err(StoreError::Other)?;
        if key_dir_name.trim().is_empty() {
            return Err(other("key id is required"));
        }
        Ok(self.root_path().join(key_dir_name).join(source))
    }

    fn source_dir(&self, key_id: &str, source: &str) -> SResult<PathBuf> {
        if key_id.trim().is_empty() {
            return Err(other("key id is required"));
        }
        self.source_dir_in(&key_dir(key_id), source)
    }

    fn chain_dir_in(&self, key_dir_name: &str, source: &str, chain: &str) -> SResult<PathBuf> {
        validate_chain_name(chain)?;
        Ok(self
            .source_dir_in(key_dir_name, source)?
            .join("chains")
            .join(chain))
    }

    fn chain_dir(&self, key_id: &str, source: &str, chain: &str) -> SResult<PathBuf> {
        validate_chain_name(chain)?;
        Ok(self.source_dir(key_id, source)?.join("chains").join(chain))
    }

    /// Store one sealed chunk (no quota).
    pub fn put_chunk(
        &self,
        key_id: &str,
        source: &str,
        chain: &str,
        index: i64,
        sealed: &[u8],
    ) -> SResult<()> {
        self.put_chunk_within(key_id, source, chain, index, sealed, 0)
    }

    /// Store one sealed chunk with a per-key quota (0 = unlimited).
    pub fn put_chunk_within(
        &self,
        key_id: &str,
        source: &str,
        chain: &str,
        index: i64,
        sealed: &[u8],
        quota: i64,
    ) -> SResult<()> {
        validate_chunk_index(index).map_err(StoreError::Invalid)?;
        if index >= MAX_CHAIN_CHUNKS {
            return Err(StoreError::Invalid(format!(
                "chunk index {index} is outside 0..{MAX_CHAIN_CHUNKS} for one chain"
            )));
        }
        validate_sealed_chunk(sealed)?;
        let dir = self.chain_dir(key_id, source, chain)?;
        std::fs::create_dir_all(&dir)?;
        let target = dir.join(chunk_name(index));

        // Serialize the read-reserve-rename sequence for this chunk path.
        let lock = self.chunk_lock(&target.to_string_lossy());
        let _guard = lock.lock().unwrap();

        let mut replaced: i64 = 0;
        if let Ok(existing) = std::fs::read(&target) {
            if digest_of(&existing) == digest_of(sealed) {
                return Ok(());
            }
            if !self.may_replace(key_id, source, chain, index, &existing)? {
                return Err(other(format!(
                    "chunk {index} already exists with different content"
                )));
            }
            replaced = billed_len(existing.len());
        }

        let delta = billed_len(sealed.len()).saturating_sub(replaced);
        self.reserve_usage(key_id, delta, quota)?;

        let write = (|| -> SResult<()> {
            let tmp = tempfile::Builder::new()
                .prefix(".chunk-")
                .suffix(".tmp")
                .tempfile_in(&dir)?;
            use std::io::Write;
            tmp.as_file().try_clone()?.write_all(sealed)?;
            tmp.as_file().sync_all()?;
            let tmp_path = tmp.into_temp_path();
            std::fs::rename(&tmp_path, &target)?;
            Ok(())
        })();
        if write.is_err() {
            self.release_usage(key_id, delta);
            return write;
        }
        Ok(())
    }

    /// Reserve quota/free space for a write and add it to the usage cache.
    ///
    /// The usage scan, the quota check, the free-space check and the increment
    /// all happen under the one usage mutex, so two concurrent writes cannot
    /// both pass the checks (SEC-2026-C8). A write with `delta == 0` (a
    /// same-size replacement) still checks free space: the reserve must hold
    /// on every write, not only on growth.
    fn reserve_usage(&self, key_id: &str, delta: i64, quota: i64) -> SResult<()> {
        let mut cache = self.usage.lock().unwrap();
        let used = self.load_usage_in(&mut cache, key_id)?;
        if quota > 0 && delta > 0 && used.saturating_add(delta) > quota {
            return Err(StoreError::Quota { used, quota });
        }
        // Fail closed when the filesystem cannot be queried.
        let free = free_space(&self.root)?;
        if free.saturating_sub(delta.max(0)) < free_space_reserve() {
            return Err(StoreError::NoSpace);
        }
        cache
            .usage
            .insert(key_id.to_string(), used.saturating_add(delta));
        Ok(())
    }

    fn release_usage(&self, key_id: &str, delta: i64) {
        if delta == 0 {
            return;
        }
        let mut cache = self.usage.lock().unwrap();
        if cache.loaded.contains(key_id) {
            if let Some(used) = cache.usage.get_mut(key_id) {
                *used = used.saturating_sub(delta);
            }
        }
    }

    fn may_replace(
        &self,
        key_id: &str,
        source: &str,
        chain: &str,
        index: i64,
        existing: &[u8],
    ) -> SResult<bool> {
        if self.chain_published(key_id, source, chain)? {
            return Ok(false);
        }
        let indices = self.list_chunks(key_id, source, chain)?;
        if indices.len() as i64 != index + 1 || indices.last() != Some(&index) {
            return Ok(false);
        }
        match chunk_plain_len(existing) {
            Some(plain) if plain < CHUNK_PLAIN_SIZE => Ok(true),
            _ => Ok(false),
        }
    }

    /// Whether a chain has a manifest (a finished backup rather than an upload).
    pub fn is_chain_published(&self, key_id: &str, source: &str, chain: &str) -> SResult<bool> {
        self.chain_published(key_id, source, chain)
    }

    fn chain_published(&self, key_id: &str, source: &str, chain: &str) -> SResult<bool> {
        let dir = self.chain_dir(key_id, source, chain)?;
        Ok(dir.join("manifest.json").exists())
    }

    /// Read a sealed chunk back.
    pub fn chunk(&self, key_id: &str, source: &str, chain: &str, index: i64) -> SResult<Vec<u8>> {
        let dir = self.chain_dir(key_id, source, chain)?;
        std::fs::read(dir.join(chunk_name(index)))
            .map_err(|e| other(format!("chunk {index} is not stored: {e}")))
    }

    /// The chunk indices present for a chain.
    pub fn list_chunks(&self, key_id: &str, source: &str, chain: &str) -> SResult<Vec<i64>> {
        let dir = self.chain_dir(key_id, source, chain)?;
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut indices = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(rest) = name.strip_prefix("chunk-") {
                if let Some(num) = rest.strip_suffix(".enc") {
                    if let Ok(index) = num.parse::<i64>() {
                        indices.push(index);
                    }
                }
            }
        }
        indices.sort();
        Ok(indices)
    }

    /// The SHA-256 (and nonce generation) of every sealed chunk of a chain.
    pub fn chunk_digests(
        &self,
        key_id: &str,
        source: &str,
        chain: &str,
    ) -> SResult<HashMap<i64, StoredChunk>> {
        let mut out = HashMap::new();
        for index in self.list_chunks(key_id, source, chain)? {
            let sealed = self.chunk(key_id, source, chain, index)?;
            out.insert(
                index,
                StoredChunk {
                    digest: digest_of(&sealed),
                    generation: sealed_chunk_generation(&sealed),
                },
            );
        }
        Ok(out)
    }

    /// Store a chain's manifest and point "current" at it, refusing a rollback.
    pub fn put_manifest(&self, key_id: &str, source: &str, m: &Manifest) -> SResult<()> {
        self.put_manifest_within(key_id, source, m, 0)
    }

    /// Store a manifest under a per-key quota (0 = unlimited). The manifest,
    /// the GUID index and `current.json` are charged too: they are real bytes
    /// a peer can otherwise use to fill the receive dataset unquoted.
    pub fn put_manifest_within(
        &self,
        key_id: &str,
        source: &str,
        m: &Manifest,
        quota: i64,
    ) -> SResult<()> {
        let dir = self.chain_dir(key_id, source, &m.chain)?;
        let data = serde_json::to_vec_pretty(m).map_err(|e| other(e.to_string()))?;

        let current = self.manifest(key_id, source).ok();
        let previous = current
            .as_ref()
            .map(|c| c.chain.clone())
            .unwrap_or_default();
        // Same chain: a strictly older sequence is a rollback. An equal
        // sequence is the idempotent re-PUT of a retried publish, accepted
        // only when it is byte-identical to what is stored.
        if let Some(cur) = &current {
            if cur.chain == m.chain {
                if m.sequence < cur.sequence {
                    return Err(other(format!(
                        "refusing stale manifest for {source}: sequence {} is older than the stored {}",
                        m.sequence, cur.sequence
                    )));
                }
                if m.sequence == cur.sequence {
                    // A retried publish regenerates `createdAt`, the nonce
                    // generation and therefore the signature, so compare the
                    // content-bearing fields instead of the raw bytes.
                    let stored = std::fs::read(dir.join("manifest.json")).map_err(|e| {
                        other(format!(
                            "cannot compare the retried manifest for {source} with the stored one: {e}"
                        ))
                    })?;
                    let stored_manifest: Manifest =
                        serde_json::from_slice(&stored).map_err(|e| {
                            other(format!("parsing the stored manifest for {source}: {e}"))
                        })?;
                    if manifest_content_fingerprint(&stored_manifest)?
                        != manifest_content_fingerprint(m)?
                    {
                        return Err(other(format!(
                            "refusing a different manifest for {source} at sequence {}: an equal sequence is only accepted for the same content",
                            m.sequence
                        )));
                    }
                    // Same publish, retried. Keep the stored manifest: it pins
                    // the nonce generation the stored chunks were sealed with,
                    // and `current.json` already points at this chain.
                    return Ok(());
                }
            }
        }
        // A different chain may not move the source backwards either: sequence
        // is monotonic across the source's chain history, not just within one
        // chain (B-6).
        if let Some(cur) = &current {
            if cur.chain != m.chain && m.sequence < cur.sequence {
                return Err(other(format!(
                    "refusing manifest for {source} on chain {}: sequence {} is older than the current chain {} at {}",
                    m.chain, m.sequence, cur.chain, cur.sequence
                )));
            }
        }
        if !previous.is_empty() && previous != m.chain {
            let candidate = dir
                .parent()
                .map(|p| p.join(&m.chain))
                .unwrap_or_else(|| dir.clone());
            if candidate.join("manifest.json").exists() {
                return Err(other(format!(
                    "refusing to move {source} back to the already-stored chain {} (rollback)",
                    m.chain
                )));
            }
        }
        // The manifest's claimed chunk sizes must match what is actually
        // stored: an unverified `sealedBytes` claim lets a peer reserve
        // nothing (or report free bytes as used) while writing arbitrary
        // data.
        self.verify_manifest_chunks(key_id, source, m)?;

        std::fs::create_dir_all(&dir)?;
        // The three writes below are manifest.json + current.json + links.
        // Charge the net delta: an idempotent re-PUT replaces the same bytes
        // and must not be refused because the quota is already tight.
        let source_dir = self.source_dir(key_id, source)?;
        let file_len = |path: PathBuf| -> i64 {
            std::fs::metadata(path)
                .map(|meta| billed_len(meta.len() as usize))
                .unwrap_or(0)
        };
        let mut estimated = billed_len(data.len()) - file_len(dir.join("manifest.json"))
            + billed_len(data.len())
            - file_len(source_dir.join("current.json"));
        if !m.to_guid.is_empty() {
            let link_change = self
                .read_links(key_id, source)
                .map(|links| links.get(&m.to_guid) != Some(&m.chain))
                .unwrap_or(true);
            if link_change {
                estimated = estimated.saturating_add(BILLING_BLOCK);
            }
        }
        self.reserve_usage(key_id, estimated, quota)?;

        let write = (|| -> SResult<()> {
            write_file_atomic(&dir.join("manifest.json"), &data)?;
            let source_dir = self.source_dir(key_id, source)?;
            std::fs::create_dir_all(&source_dir)?;
            if !m.to_guid.is_empty() {
                let mut links = self.read_links(key_id, source).unwrap_or_default();
                if links.get(&m.to_guid) != Some(&m.chain) {
                    links.insert(m.to_guid.clone(), m.chain.clone());
                    self.write_links(key_id, source, &links);
                }
            }
            write_file_atomic(&source_dir.join("current.json"), &data)
        })();
        match write {
            Ok(()) => {
                // current.json/links.json now affect the on-disk total.
                self.invalidate_usage(key_id);
                Ok(())
            }
            Err(e) => {
                // manifest.json may already have landed before a later write
                // failed. Releasing the whole reservation would leave the
                // cached total below disk; recompute from disk instead.
                self.invalidate_usage(key_id);
                Err(e)
            }
        }
    }

    /// Every chunk a manifest lists must exist and be exactly the size the
    /// manifest claims.
    fn verify_manifest_chunks(&self, key_id: &str, source: &str, m: &Manifest) -> SResult<()> {
        if m.chunks.len() as i64 > MAX_CHAIN_CHUNKS {
            return Err(StoreError::Invalid(format!(
                "manifest lists {} chunks, above the {MAX_CHAIN_CHUNKS} per-chain limit",
                m.chunks.len()
            )));
        }
        let dir = self.chain_dir(key_id, source, &m.chain)?;
        for chunk in &m.chunks {
            validate_chunk_index(chunk.index).map_err(StoreError::Invalid)?;
            if chunk.sealed_bytes <= 0 {
                return Err(StoreError::Invalid(format!(
                    "manifest chunk {} claims {} sealed bytes",
                    chunk.index, chunk.sealed_bytes
                )));
            }
            let path = dir.join(chunk_name(chunk.index));
            let meta = std::fs::metadata(&path).map_err(|e| {
                StoreError::Invalid(format!(
                    "manifest lists chunk {} as {} bytes but it is not stored: {e}",
                    chunk.index, chunk.sealed_bytes
                ))
            })?;
            if meta.len() as i64 != chunk.sealed_bytes {
                return Err(StoreError::Invalid(format!(
                    "manifest claims chunk {} is {} bytes but the stored chunk is {} bytes",
                    chunk.index,
                    chunk.sealed_bytes,
                    meta.len()
                )));
            }
        }
        Ok(())
    }

    /// The current manifest of a source.
    pub fn manifest(&self, key_id: &str, source: &str) -> SResult<Manifest> {
        let dir = self.source_dir(key_id, source)?;
        let data = std::fs::read(dir.join("current.json"))
            .map_err(|e| other(format!("no backup stored for {source}: {e}")))?;
        serde_json::from_slice(&data)
            .map_err(|e| other(format!("parsing manifest for {source}: {e}")))
    }

    /// The logical sources stored for a key.
    pub fn sources(&self, key_id: &str) -> SResult<Vec<String>> {
        self.sources_in(&key_dir(key_id))
    }

    /// One chain's manifest.
    pub fn manifest_for_chain(&self, key_id: &str, source: &str, chain: &str) -> SResult<Manifest> {
        let dir = self.chain_dir(key_id, source, chain)?;
        let raw = std::fs::read(dir.join("manifest.json"))
            .map_err(|e| other(format!("chain {chain} is not stored for {source}: {e}")))?;
        serde_json::from_slice(&raw)
            .map_err(|e| other(format!("parsing manifest of chain {chain}: {e}")))
    }

    fn read_links(&self, key_id: &str, source: &str) -> Option<HashMap<String, String>> {
        let dir = self.source_dir(key_id, source).ok()?;
        let raw = std::fs::read(dir.join("chains").join(LINKS_FILE)).ok()?;
        serde_json::from_slice(&raw).ok()
    }

    fn write_links(&self, key_id: &str, source: &str, links: &HashMap<String, String>) {
        let Ok(dir) = self.source_dir(key_id, source) else {
            return;
        };
        let chains = dir.join("chains");
        if std::fs::create_dir_all(&chains).is_err() {
            return;
        }
        if let Ok(data) = serde_json::to_vec_pretty(links) {
            let _ = write_file_atomic(&chains.join(LINKS_FILE), &data);
        }
    }

    /// Count a source's stored chains without reading their manifests.
    pub fn chain_count(&self, key_id: &str, source: &str) -> SResult<usize> {
        let dir = self.source_dir(key_id, source)?;
        let entries = match std::fs::read_dir(dir.join("chains")) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e.into()),
        };
        Ok(entries.flatten().filter(|e| e.path().is_dir()).count())
    }

    /// The chains needed to rebuild a source, oldest first.
    pub fn sequence(&self, key_id: &str, source: &str, chain: &str) -> SResult<Vec<Manifest>> {
        let mut current = self
            .manifest_for(key_id, source, chain)?
            .ok_or_else(|| other(format!("no backup of {source} is stored here")))?;
        let mut links = self.read_links(key_id, source);
        let mut needs_scan = links.is_none();
        let mut by_to_guid: HashMap<String, Manifest> = HashMap::new();
        let mut sequence = Vec::new();
        let mut seen = std::collections::HashSet::new();

        loop {
            if !seen.insert(current.chain.clone()) {
                return Err(other(format!(
                    "the chain history of {source} contains a loop at {}",
                    current.chain
                )));
            }
            sequence.push(current.clone());
            if current.from_guid.is_empty() {
                break;
            }
            let mut next = if !needs_scan {
                links
                    .as_ref()
                    .and_then(|l| l.get(&current.from_guid))
                    .cloned()
                    .unwrap_or_default()
            } else {
                String::new()
            };
            if next.is_empty() {
                let all = self.chains(key_id, source)?;
                let mut rebuilt = HashMap::new();
                for candidate in &all {
                    if !candidate.to_guid.is_empty() {
                        rebuilt.insert(candidate.to_guid.clone(), candidate.chain.clone());
                        by_to_guid.insert(candidate.chain.clone(), candidate.clone());
                    }
                }
                links = Some(rebuilt.clone());
                needs_scan = false;
                self.write_links(key_id, source, &rebuilt);
                next = rebuilt.get(&current.from_guid).cloned().unwrap_or_default();
            }
            if next.is_empty() {
                return Err(other(format!(
                    "the chain {} needs the chain that produced GUID {}, which is not stored here",
                    current.chain, current.from_guid
                )));
            }
            let base = match by_to_guid.get(&next) {
                Some(m) => m.clone(),
                None => self
                    .manifest_for(key_id, source, &next)?
                    .ok_or_else(|| other(format!(
                        "the chain {} needs the chain that produced GUID {}, which is not stored here",
                        current.chain, current.from_guid
                    )))?,
            };
            current = base;
        }
        sequence.reverse();
        Ok(sequence)
    }

    fn manifest_for(&self, key_id: &str, source: &str, chain: &str) -> SResult<Option<Manifest>> {
        if chain.is_empty() {
            match self.manifest(key_id, source) {
                Ok(m) => Ok(Some(m)),
                Err(_) => Ok(None),
            }
        } else {
            let dir = self.chain_dir(key_id, source, chain)?;
            match std::fs::read(dir.join("manifest.json")) {
                Ok(raw) => serde_json::from_slice(&raw)
                    .map(Some)
                    .map_err(|e| other(e.to_string())),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e.into()),
            }
        }
    }

    /// Every chain manifest stored for a source, newest first.
    pub fn chains(&self, key_id: &str, source: &str) -> SResult<Vec<Manifest>> {
        self.chains_limited(key_id, source, MAX_LISTED_CHAINS)
    }

    pub fn chains_limited(
        &self,
        key_id: &str,
        source: &str,
        limit: usize,
    ) -> SResult<Vec<Manifest>> {
        let dir = self.source_dir(key_id, source)?;
        let entries = match std::fs::read_dir(dir.join("chains")) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut manifests = Vec::new();
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let Ok(raw) = std::fs::read(entry.path().join("manifest.json")) else {
                continue;
            };
            if let Ok(m) = serde_json::from_slice::<Manifest>(&raw) {
                manifests.push(m);
            }
        }
        manifests.sort_by_key(|m| std::cmp::Reverse(m.created_at));
        if limit > 0 && manifests.len() > limit {
            manifests.truncate(limit);
        }
        Ok(manifests)
    }

    fn manifest_in(&self, key_dir_name: &str, source: &str) -> SResult<Manifest> {
        let dir = self.source_dir_in(key_dir_name, source)?;
        let raw = std::fs::read(dir.join("current.json"))?;
        serde_json::from_slice(&raw).map_err(|e| other(e.to_string()))
    }

    /// Summarise what is stored for one key (or every key when empty).
    pub fn summary(&self, key_id: &str) -> SResult<Vec<Backups>> {
        let key_dirs: Vec<String> = if key_id.is_empty() {
            let entries = match std::fs::read_dir(&self.root) {
                Ok(e) => e,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
                Err(e) => return Err(e.into()),
            };
            entries
                .flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        } else {
            vec![key_dir(key_id)]
        };
        let mut out = Vec::new();
        for dir in key_dirs {
            for source in self.sources_in(&dir)? {
                let Ok(manifest) = self.manifest_in(&dir, &source) else {
                    continue;
                };
                let mut row = Backups {
                    source: source.clone(),
                    chain: manifest.chain.clone(),
                    kind: manifest.kind.clone(),
                    created_at: manifest.created_at,
                    chunks: manifest.chunks.len(),
                    stored_bytes: 0,
                };
                if let Ok(chain_dir) = self.chain_dir_in(&dir, &source, &manifest.chain) {
                    row.stored_bytes = dir_size(&chain_dir)?;
                }
                out.push(row);
            }
        }
        out.sort_by(|a, b| a.source.cmp(&b.source));
        Ok(out)
    }

    /// The bytes stored for a key (or every key when empty). The charge is
    /// block-rounded and matches what a restart recomputes from disk.
    pub fn usage(&self, key_id: &str) -> SResult<i64> {
        if key_id.is_empty() {
            return self.uncached_usage();
        }
        self.usage_locked(key_id)
    }

    fn usage_locked(&self, key_id: &str) -> SResult<i64> {
        let mut cache = self.usage.lock().unwrap();
        self.load_usage_in(&mut cache, key_id)
    }

    /// Load a key's usage while holding the usage mutex. The scan runs inside
    /// the critical section so it cannot race a concurrent reserve (B-2), and
    /// an I/O error is fatal rather than reported as zero (fail closed).
    fn load_usage_in(&self, cache: &mut UsageCache, key_id: &str) -> SResult<i64> {
        if let Some(used) = cache.usage.get(key_id) {
            if cache.loaded.contains(key_id) {
                return Ok(*used);
            }
        }
        let size = charged_dir_size(&self.root_path().join(key_dir(key_id)))?;
        cache.usage.insert(key_id.to_string(), size);
        cache.loaded.insert(key_id.to_string());
        Ok(size)
    }

    fn uncached_usage(&self) -> SResult<i64> {
        // Fail closed: an unreadable store must not look like an empty one.
        charged_dir_size(&self.root_path())
    }

    /// Forget a key's cached usage.
    pub fn invalidate_usage(&self, key_id: &str) {
        let mut cache = self.usage.lock().unwrap();
        cache.usage.remove(key_id);
        cache.loaded.remove(key_id);
    }

    /// The bytes available on the store's filesystem.
    pub fn free_space(&self) -> SResult<i64> {
        std::fs::create_dir_all(&self.root)?;
        free_space(&self.root)
    }

    /// The newest stored backup time for a key.
    pub fn last_backup(&self, key_id: &str) -> SResult<Option<DateTime<Utc>>> {
        let backups = self.summary(key_id)?;
        Ok(backups.into_iter().map(|b| b.created_at).max())
    }

    /// Keep the newest `keep` chains, never deleting one the current backup
    /// descends from. Returns the number removed.
    pub fn prune(&self, key_id: &str, source: &str, keep: usize) -> SResult<usize> {
        if keep < 1 {
            return Err(other("keep must be at least 1"));
        }
        let dir = self.source_dir(key_id, source)?;
        let chains_dir = dir.join("chains");
        let entries = match std::fs::read_dir(&chains_dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e.into()),
        };
        struct ChainInfo {
            name: String,
            when: DateTime<Utc>,
            from_guid: String,
            to_guid: String,
        }
        let mut chains = Vec::new();
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let mut info = ChainInfo {
                name,
                when: DateTime::<Utc>::MIN_UTC,
                from_guid: String::new(),
                to_guid: String::new(),
            };
            if let Ok(raw) = std::fs::read(entry.path().join("manifest.json")) {
                if let Ok(m) = serde_json::from_slice::<Manifest>(&raw) {
                    info.when = m.created_at;
                    info.from_guid = m.from_guid;
                    info.to_guid = m.to_guid;
                }
            }
            chains.push(info);
        }
        chains.sort_by_key(|c| std::cmp::Reverse(c.when));

        let current_chain = std::fs::read(dir.join("current.json"))
            .ok()
            .and_then(|raw| serde_json::from_slice::<Manifest>(&raw).ok())
            .map(|m| m.chain)
            .unwrap_or_default();

        let mut required = std::collections::HashSet::new();
        let newest = if !current_chain.is_empty() {
            current_chain.clone()
        } else {
            chains.first().map(|c| c.name.clone()).unwrap_or_default()
        };
        if !newest.is_empty() {
            required.insert(newest.clone());
            let mut from_guid = chains
                .iter()
                .find(|c| c.name == newest)
                .map(|c| c.from_guid.clone())
                .unwrap_or_default();
            while !from_guid.is_empty() {
                let mut next = String::new();
                for candidate in &chains {
                    if candidate.to_guid == from_guid && !required.contains(&candidate.name) {
                        next = candidate.name.clone();
                        from_guid = candidate.from_guid.clone();
                        break;
                    }
                }
                if next.is_empty() {
                    break;
                }
                required.insert(next);
            }
        }

        let mut remaining = chains.len();
        let mut removed = 0;
        for chain in chains.iter().rev() {
            if remaining <= keep {
                break;
            }
            if required.contains(&chain.name) {
                continue;
            }
            if std::fs::remove_dir_all(chains_dir.join(&chain.name)).is_ok() {
                removed += 1;
                remaining -= 1;
            }
        }
        if removed > 0 {
            self.invalidate_usage(key_id);
            let mut links = HashMap::new();
            for chain in &chains {
                if chain.to_guid.is_empty() {
                    continue;
                }
                if chains_dir.join(&chain.name).exists() {
                    links.insert(chain.to_guid.clone(), chain.name.clone());
                }
            }
            self.write_links(key_id, source, &links);
        }
        Ok(removed)
    }
}

/// What a resuming sender needs to know about a stored chunk.
#[derive(Debug, Clone)]
pub struct StoredChunk {
    pub digest: String,
    pub generation: u16,
}

/// A summary row for one stored source.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Backups {
    pub source: String,
    pub chain: String,
    pub kind: String,
    #[serde(rename = "createdAt")]
    pub created_at: DateTime<Utc>,
    pub chunks: usize,
    #[serde(rename = "storedBytes")]
    pub stored_bytes: i64,
}

/// `<root>/<key hash>/...`: a hash of the fingerprint, never parsed back.
pub fn key_dir(fingerprint: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(fingerprint.as_bytes());
    let digest = h.finalize();
    format!(
        "k{}",
        digest[..16]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

/// Bound a chain id before joining it into a path.
///
/// The client generates lowercase hex (`[a-z0-9]{1,64}`); anything else is
/// refused. In particular a leading `.` used to make a chain invisible to the
/// usage scan, so a peer could zero its reported usage while the data stayed.
pub fn validate_chain_name(chain: &str) -> SResult<()> {
    if chain.is_empty()
        || chain.len() > 64
        || !chain
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    {
        return Err(other(format!(
            "invalid chain id {chain:?}: use 1-64 lowercase letters or digits"
        )));
    }
    Ok(())
}

/// Reject bytes that cannot be a sealed chunk. The receiver never decrypts,
/// but it can refuse the shapes a real sender never produces: an envelope
/// with the wrong magic, too short to carry a nonce and a tag, or claiming
/// more plaintext than a chunk can hold.
fn validate_sealed_chunk(sealed: &[u8]) -> SResult<()> {
    if sealed.len() > MAX_SEALED_CHUNK_SIZE {
        return Err(StoreError::Invalid(format!(
            "chunk is {} bytes, the maximum is {MAX_SEALED_CHUNK_SIZE}",
            sealed.len()
        )));
    }
    if sealed.len() < MIN_SEALED_CHUNK_LEN {
        return Err(StoreError::Invalid(format!(
            "chunk is {} bytes, below the {MIN_SEALED_CHUNK_LEN}-byte minimum sealed envelope",
            sealed.len()
        )));
    }
    if &sealed[..CHUNK_MAGIC.len()] != CHUNK_MAGIC {
        return Err(StoreError::Invalid(
            "chunk is not a NBC2 envelope".to_string(),
        ));
    }
    match chunk_plain_len(sealed) {
        Some(plain) if plain <= CHUNK_PLAIN_SIZE => Ok(()),
        Some(plain) => Err(StoreError::Invalid(format!(
            "chunk declares {plain} plain bytes, above the {CHUNK_PLAIN_SIZE} maximum"
        ))),
        None => Err(StoreError::Invalid(
            "chunk carries no readable plaintext length".to_string(),
        )),
    }
}

/// The manifest fields that identify what it covers, excluding values the
/// sender regenerates on every attempt:
///
/// - `createdAt` is the publish time;
/// - `generation` (manifest and per chunk) is bumped per attempt;
/// - `dekWrapped` re-wraps the same chain DEK under a fresh random wrapping
///   nonce, so the bytes differ every attempt while the content does not;
/// - `signature` covers all of the above.
///
/// The chain metadata and the chunk list (indices, plain/sealed sizes,
/// plaintext digests) stay in the fingerprint, so a genuinely different
/// payload at the same sequence is still refused. Used to recognise a retried
/// publish; the stored manifest is kept, so a retried wrap can never replace
/// the one the stored chunks were published with.
fn manifest_content_fingerprint(m: &Manifest) -> SResult<Vec<u8>> {
    let mut copy = m.clone();
    copy.created_at = DateTime::<Utc>::MIN_UTC;
    copy.generation = 0;
    copy.dek_wrapped.clear();
    for chunk in &mut copy.chunks {
        chunk.generation = 0;
    }
    copy.signature.clear();
    serde_json::to_vec(&copy).map_err(|e| other(format!("encoding a manifest for comparison: {e}")))
}

/// Block-round a size for quota accounting.
fn billed_len(len: usize) -> i64 {
    let len = i64::try_from(len).unwrap_or(i64::MAX);
    if len <= BILLING_BLOCK {
        BILLING_BLOCK
    } else {
        len.saturating_add(BILLING_BLOCK - 1)
            .saturating_div(BILLING_BLOCK)
            .saturating_mul(BILLING_BLOCK)
    }
}

fn chunk_name(index: i64) -> String {
    format!("chunk-{index:06}.enc")
}

fn sealed_chunk_generation(sealed: &[u8]) -> u16 {
    if sealed.len() < CHUNK_MAGIC.len() + 4 + NONCE_SIZE || &sealed[..4] != CHUNK_MAGIC {
        return 0;
    }
    let offset = 4 + 4 + STREAM_PREFIX_LEN;
    u16::from_be_bytes([sealed[offset], sealed[offset + 1]])
}

fn digest_of(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn free_space_reserve() -> i64 {
    if let Ok(v) = std::env::var("BUDDY_RESERVE_BYTES") {
        if let Ok(n) = v.parse::<i64>() {
            if n >= 0 {
                return n;
            }
        }
    }
    1 << 30
}

fn free_space(path: &str) -> SResult<i64> {
    use std::ffi::CString;
    let c = CString::new(path).map_err(|e| other(e.to_string()))?;
    // SAFETY: statvfs writes into a caller-owned struct; the path is a valid C string.
    unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c.as_ptr(), &mut stat) != 0 {
            return Err(other(std::io::Error::last_os_error().to_string()));
        }
        let bsize = stat.f_bsize as u128;
        let bavail = stat.f_bavail as u128;
        let total = bavail.saturating_mul(bsize);
        if total > i64::MAX as u128 {
            return Ok(i64::MAX);
        }
        Ok(total as i64)
    }
}

fn write_file_atomic(path: &Path, data: &[u8]) -> SResult<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = tempfile::Builder::new()
        .prefix(".write-")
        .suffix(".tmp")
        .tempfile_in(dir)?;
    {
        use std::io::Write;
        tmp.as_file().try_clone()?.write_all(data)?;
        tmp.as_file().sync_all()?;
    }
    let tmp_path = tmp.into_temp_path();
    std::fs::set_permissions(
        &tmp_path,
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )?;
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

/// Sum a tree. Dot-named entries are included: skipping them was the chain-id
/// quota bypass (a `.hidden` directory counted as zero bytes).
fn dir_size(dir: &Path) -> SResult<i64> {
    dir_bytes(dir, false)
}

/// Sum a tree with each file billed as at least one block, so the in-memory
/// charge and a restart's recomputation agree and tiny files cannot be free.
fn charged_dir_size(dir: &Path) -> SResult<i64> {
    dir_bytes(dir, true)
}

fn dir_bytes(dir: &Path, charge_blocks: bool) -> SResult<i64> {
    let mut total = 0i64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = match std::fs::read_dir(&current) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        for entry in entries {
            let entry = entry.map_err(|e| StoreError::Other(e.to_string()))?;
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                // A concurrent chunk/manifest replacement can rename an entry
                // away between `read_dir` and this call; that is transient,
                // not a store failure.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(StoreError::Other(format!("stat {}: {e}", path.display()))),
            };
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            let meta = match std::fs::symlink_metadata(&path) {
                Ok(meta) => meta,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(StoreError::Other(format!("stat {}: {e}", path.display()))),
            };
            let len = meta.len() as i64;
            total = total.saturating_add(if charge_blocks {
                billed_len(meta.len() as usize)
            } else {
                len
            });
        }
    }
    Ok(total)
}

fn walk_sources(root: &Path, current: &Path, out: &mut Vec<String>) -> SResult<()> {
    let entries = match std::fs::read_dir(current) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() || path == root {
            continue;
        }
        if path.join("current.json").exists() {
            if let Ok(rel) = path.strip_prefix(root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
            continue;
        }
        walk_sources(root, &path, out)?;
    }
    Ok(())
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            version: 0,
            source: String::new(),
            chain: String::new(),
            kind: String::new(),
            from_snapshot: String::new(),
            to_snapshot: String::new(),
            from_guid: String::new(),
            to_guid: String::new(),
            created_at: DateTime::<Utc>::MIN_UTC,
            key_id: String::new(),
            sequence: 0,
            generation: 0,
            stream_prefix: String::new(),
            chunk_plain_size: 0,
            chunks: Vec::new(),
            dek_wrapped: String::new(),
            signature: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::envelope::{new_stream_prefix, seal_chunk};
    use super::super::keys::new_identity;
    use super::*;

    fn key_id() -> String {
        new_identity("t").unwrap().fingerprint().unwrap()
    }

    #[test]
    fn chunk_round_trip_and_digests() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(&dir.path().display().to_string());
        let kid = key_id();
        let dek = [1u8; 32];
        let prefix = new_stream_prefix().unwrap();
        let (sealed, _) = seal_chunk(&dek, &prefix, 0, "src", "chain", 0, b"data").unwrap();
        store.put_chunk(&kid, "src", "chain", 0, &sealed).unwrap();
        assert_eq!(store.chunk(&kid, "src", "chain", 0).unwrap(), sealed);
        assert_eq!(store.list_chunks(&kid, "src", "chain").unwrap(), vec![0]);
        let digests = store.chunk_digests(&kid, "src", "chain").unwrap();
        assert_eq!(digests[&0].generation, 0);
    }

    #[test]
    fn chunk_path_cannot_traverse() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(&dir.path().display().to_string());
        let kid = key_id();
        assert!(store
            .put_chunk(&kid, "../escape", "chain", 0, b"x")
            .is_err());
        assert!(store.put_chunk(&kid, "src", "../chain", 0, b"x").is_err());
    }

    #[test]
    fn quota_is_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(&dir.path().display().to_string());
        let kid = key_id();
        let prefix = new_stream_prefix().unwrap();
        let (sealed, _) =
            seal_chunk(&[1u8; 32], &prefix, 0, "src", "chain", 0, &[0u8; 64]).unwrap();
        assert_eq!(sealed.len(), 100);
        let err = store
            .put_chunk_within(&kid, "src", "chain", 0, &sealed, 50)
            .unwrap_err();
        assert!(matches!(err, StoreError::Quota { .. }));
    }

    fn sealed_for(plain_len: usize, fill: u8) -> Vec<u8> {
        let prefix = new_stream_prefix().unwrap();
        let (sealed, _) = seal_chunk(
            &[3u8; 32],
            &prefix,
            0,
            "src",
            "chain",
            0,
            &vec![fill; plain_len],
        )
        .unwrap();
        sealed
    }

    #[test]
    fn structurally_invalid_chunks_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(&dir.path().display().to_string());
        let kid = key_id();

        // Empty and short bodies, even with the right magic.
        for bad in [Vec::new(), b"NBC2".to_vec(), vec![0u8; 20]] {
            let err = store
                .put_chunk_within(&kid, "src", "chain", 0, &bad, 0)
                .unwrap_err();
            assert!(matches!(err, StoreError::Invalid(_)), "{err:?}");
        }

        // Right length, wrong magic.
        let mut wrong_magic = sealed_for(8, 1);
        wrong_magic[..4].copy_from_slice(b"NBC1");
        assert!(matches!(
            store
                .put_chunk_within(&kid, "src", "chain", 0, &wrong_magic, 0)
                .unwrap_err(),
            StoreError::Invalid(_)
        ));

        // A chunk cannot declare more plaintext than the fixed chunk size.
        let mut oversized = sealed_for(8, 1);
        oversized[4..8].copy_from_slice(&((CHUNK_PLAIN_SIZE + 1) as u32).to_be_bytes());
        assert!(matches!(
            store
                .put_chunk_within(&kid, "src", "chain", 0, &oversized, 0)
                .unwrap_err(),
            StoreError::Invalid(_)
        ));
        assert!(store.list_chunks(&kid, "src", "chain").unwrap().is_empty());
        assert_eq!(store.usage(&kid).unwrap(), 0);
    }

    #[test]
    fn tiny_chunks_still_consume_a_block_of_quota() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(&dir.path().display().to_string());
        let kid = key_id();
        let prefix = new_stream_prefix().unwrap();
        // 36 bytes sealed: the smallest valid envelope (empty plaintext).
        let (sealed, _) = seal_chunk(&[1u8; 32], &prefix, 0, "src", "tiny", 0, b"").unwrap();
        assert_eq!(sealed.len(), 36);
        assert_eq!(billed_len(sealed.len()), BILLING_BLOCK);

        store
            .put_chunk_within(&kid, "src", "tiny", 0, &sealed, 2 * BILLING_BLOCK - 1)
            .unwrap();
        assert_eq!(store.usage(&kid).unwrap(), BILLING_BLOCK);
        let err = store
            .put_chunk_within(&kid, "src", "tiny", 1, &sealed, 2 * BILLING_BLOCK - 1)
            .unwrap_err();
        assert!(
            matches!(err, StoreError::Quota { used, quota } if used == BILLING_BLOCK && quota == 2 * BILLING_BLOCK - 1),
            "{err:?}"
        );
    }

    #[test]
    fn chain_ids_must_match_the_client_format() {
        let longest_ok = "a".repeat(64);
        let too_long = "a".repeat(65);
        for ok in ["chain", "c1", "0f3c", longest_ok.as_str()] {
            assert!(validate_chain_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            ".",
            "..",
            ".hidden",
            "Chain",
            "chain-1",
            "chain_1",
            "chain/1",
            too_long.as_str(),
        ] {
            assert!(validate_chain_name(bad).is_err(), "{bad}");
        }
        let store = Store::new("");
        let kid = key_id();
        assert!(store
            .put_chunk(&kid, "src", ".hidden", 0, &sealed_for(8, 1))
            .is_err());
    }

    #[test]
    fn the_chunk_index_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(&dir.path().display().to_string());
        let kid = key_id();
        let sealed = sealed_for(8, 1);
        for index in [MAX_CHAIN_CHUNKS, MAX_CHAIN_CHUNKS + 1, i64::MAX] {
            assert!(matches!(
                store
                    .put_chunk_within(&kid, "src", "chain", index, &sealed, 0)
                    .unwrap_err(),
                StoreError::Invalid(_)
            ));
        }
        assert!(matches!(
            store
                .put_chunk_within(&kid, "src", "chain", -1, &sealed, 0)
                .unwrap_err(),
            StoreError::Invalid(_)
        ));
    }

    fn manifest_titled(chain: &str, sequence: u64) -> Manifest {
        Manifest {
            version: 2,
            source: "src".into(),
            chain: chain.into(),
            kind: "zfs-send".into(),
            created_at: DateTime::from_timestamp(1_700_000_000, 123_000_000).unwrap(),
            chunk_plain_size: CHUNK_PLAIN_SIZE as i64,
            sequence,
            ..Default::default()
        }
    }

    #[test]
    fn a_manifest_may_not_claim_more_bytes_than_it_covers() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(&dir.path().display().to_string());
        let kid = key_id();
        let sealed = sealed_for(64, 7); // 100 bytes
        store.put_chunk(&kid, "src", "chain", 0, &sealed).unwrap();

        let mut manifest = manifest_titled("chain", 1);
        manifest.chunks = vec![super::super::envelope::ManifestChunk {
            index: 0,
            plain_bytes: 64,
            sealed_bytes: sealed.len() as i64 + 1,
            generation: 0,
            sha256_plain: "a".repeat(64),
        }];
        let err = store.put_manifest(&kid, "src", &manifest).unwrap_err();
        assert!(
            matches!(&err, StoreError::Invalid(m) if m.contains("stored chunk")),
            "{err:?}"
        );

        // Missing chunks are refused too (a manifest bomb with no payload).
        let mut bomb = manifest_titled("chain", 1);
        bomb.chunks = (0..100)
            .map(|index| super::super::envelope::ManifestChunk {
                index,
                plain_bytes: 1,
                sealed_bytes: MAX_SEALED_CHUNK_SIZE as i64,
                generation: 0,
                sha256_plain: "a".repeat(64),
            })
            .collect();
        assert!(store.put_manifest(&kid, "src", &bomb).is_err());

        // The honest manifest is accepted.
        manifest.chunks[0].sealed_bytes = sealed.len() as i64;
        store.put_manifest(&kid, "src", &manifest).unwrap();
    }

    #[test]
    fn manifest_bytes_are_quota_charged() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(&dir.path().display().to_string());
        let kid = key_id();
        let sealed = sealed_for(64, 7);
        store.put_chunk(&kid, "src", "chain", 0, &sealed).unwrap();
        let mut manifest = manifest_titled("chain", 1);
        manifest.chunks = vec![super::super::envelope::ManifestChunk {
            index: 0,
            plain_bytes: 64,
            sealed_bytes: sealed.len() as i64,
            generation: 0,
            sha256_plain: "a".repeat(64),
        }];
        let err = store
            .put_manifest_within(&kid, "src", &manifest, BILLING_BLOCK)
            .unwrap_err();
        assert!(matches!(err, StoreError::Quota { .. }), "{err:?}");
        store
            .put_manifest_within(&kid, "src", &manifest, 0)
            .unwrap();

        // A retried publish of the same manifest replaces the same bytes and
        // must not be refused at the quota boundary.
        let used = store.usage(&kid).unwrap();
        store
            .put_manifest_within(&kid, "src", &manifest, used)
            .unwrap();
    }

    #[test]
    fn a_new_chain_may_not_move_the_sequence_backwards() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(&dir.path().display().to_string());
        let kid = key_id();
        store
            .put_manifest(&kid, "src", &manifest_titled("aaa", 7))
            .unwrap();

        let earlier = manifest_titled("bbb", 6);
        let err = store.put_manifest(&kid, "src", &earlier).unwrap_err();
        assert!(
            format!("{err:?}").contains("older than the current chain"),
            "{err:?}"
        );

        store
            .put_manifest(&kid, "src", &manifest_titled("bbb", 8))
            .unwrap();

        // Same chain, same sequence: only the same content may be re-PUT.
        let mut changed = manifest_titled("bbb", 8);
        changed.to_snapshot = "different".into();
        let err = store.put_manifest(&kid, "src", &changed).unwrap_err();
        assert!(format!("{err:?}").contains("same content"), "{err:?}");
        store
            .put_manifest(&kid, "src", &manifest_titled("bbb", 8))
            .unwrap();
    }

    #[test]
    fn a_retried_publish_may_regenerate_volatile_fields() {
        use base64::Engine as _;
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(&dir.path().display().to_string());
        let kid = key_id();
        let mut original = manifest_titled("chain", 3);
        original.dek_wrapped = base64::engine::general_purpose::STANDARD.encode([1u8; 60]);
        store.put_manifest(&kid, "src", &original).unwrap();

        // A retry after a lost response rebuilds the manifest with a new
        // timestamp, a new nonce generation, a fresh DEK-wrapping nonce and a
        // new signature.
        let mut retried = original.clone();
        retried.created_at = DateTime::from_timestamp(1_700_100_000, 0).unwrap();
        retried.generation = original.generation.saturating_add(9);
        retried.dek_wrapped = base64::engine::general_purpose::STANDARD.encode([2u8; 60]);
        retried.signature = "cmVzaWduZWQ=".into();
        assert_ne!(retried.dek_wrapped, original.dek_wrapped);
        store.put_manifest(&kid, "src", &retried).unwrap();

        // The stored manifest is the original one: it pins the generation and
        // the wrapped DEK the stored chunks were published with.
        let stored = store.manifest(&kid, "src").unwrap();
        assert_eq!(stored.created_at, original.created_at);
        assert_eq!(stored.generation, original.generation);
        assert_eq!(stored.dek_wrapped, original.dek_wrapped);
        assert_eq!(stored.signature, original.signature);

        // A changed chunk list at the same sequence is different content.
        let mut changed = manifest_titled("chain", 3);
        changed.dek_wrapped = original.dek_wrapped.clone();
        changed.chunks = vec![super::super::envelope::ManifestChunk {
            index: 0,
            plain_bytes: 1,
            sealed_bytes: 36,
            generation: 0,
            sha256_plain: "b".repeat(64),
        }];
        let err = store.put_manifest(&kid, "src", &changed).unwrap_err();
        assert!(format!("{err:?}").contains("same content"), "{err:?}");
    }

    #[test]
    fn a_partial_manifest_write_does_not_leave_usage_below_disk() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().display().to_string();
        let store = Store::new(&root);
        let kid = key_id();
        let sealed = sealed_for(64, 7);
        store.put_chunk(&kid, "src", "chain", 0, &sealed).unwrap();

        // manifest.json lands first; the final current.json write then fails
        // because its target path is a directory.
        let source_dir = store.source_dir(&kid, "src").unwrap();
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::create_dir(source_dir.join("current.json")).unwrap();

        let mut manifest = manifest_titled("chain", 1);
        manifest.chunks = vec![super::super::envelope::ManifestChunk {
            index: 0,
            plain_bytes: 64,
            sealed_bytes: sealed.len() as i64,
            generation: 0,
            sha256_plain: "a".repeat(64),
        }];
        store.put_manifest(&kid, "src", &manifest).unwrap_err();
        assert!(store
            .chain_dir(&kid, "src", "chain")
            .unwrap()
            .join("manifest.json")
            .is_file());

        // The cached total must match a fresh scan (manifest bytes included),
        // not drop to the pre-reservation value.
        let cached = store.usage(&kid).unwrap();
        let fresh = Store::new(&root).usage(&kid).unwrap();
        assert_eq!(cached, fresh);
        assert!(cached >= billed_len(sealed.len()));
    }

    #[test]
    fn usage_is_block_rounded_and_survives_invalidation_and_restart() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().display().to_string();
        let store = Store::new(&root);
        let kid = key_id();
        let prefix = new_stream_prefix().unwrap();
        for index in 0..3 {
            let (sealed, _) =
                seal_chunk(&[9u8; 32], &prefix, 0, "src", "chain", index, b"x").unwrap();
            store
                .put_chunk_within(&kid, "src", "chain", index, &sealed, 0)
                .unwrap();
        }
        let expected = 3 * BILLING_BLOCK;
        assert_eq!(store.usage(&kid).unwrap(), expected);
        store.invalidate_usage(&kid);
        assert_eq!(store.usage(&kid).unwrap(), expected);
        let restarted = Store::new(&root);
        assert_eq!(restarted.usage(&kid).unwrap(), expected);
    }

    #[test]
    fn manifest_rollback_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(&dir.path().display().to_string());
        let kid = key_id();
        let base = Manifest {
            version: 2,
            source: "src".into(),
            chain: "new".into(),
            kind: "zfs-send".into(),
            created_at: Utc::now(),
            chunk_plain_size: CHUNK_PLAIN_SIZE as i64,
            ..Default::default()
        };
        store.put_manifest(&kid, "src", &base).unwrap();
        let mut old = base.clone();
        old.chain = "old".into();
        // Store the old chain's manifest directly, then try to make it current.
        let chain_dir = store.chain_dir(&kid, "src", "old").unwrap();
        std::fs::create_dir_all(&chain_dir).unwrap();
        std::fs::write(
            chain_dir.join("manifest.json"),
            serde_json::to_vec(&old).unwrap(),
        )
        .unwrap();
        assert!(store.put_manifest(&kid, "src", &old).is_err());
    }

    #[test]
    fn an_older_sequence_for_the_same_chain_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(&dir.path().display().to_string());
        let kid = key_id();
        let base = Manifest {
            version: 2,
            source: "src".into(),
            chain: "same".into(),
            kind: "zfs-send".into(),
            created_at: Utc::now(),
            chunk_plain_size: CHUNK_PLAIN_SIZE as i64,
            sequence: 3,
            ..Default::default()
        };
        store.put_manifest(&kid, "src", &base).unwrap();

        let mut newer = base.clone();
        newer.sequence = 4;
        assert!(store.put_manifest(&kid, "src", &newer).is_ok());

        // Equal sequence is the idempotent re-PUT of a retried publish.
        let mut same = base.clone();
        same.sequence = 4;
        assert!(store.put_manifest(&kid, "src", &same).is_ok());

        // Strictly older is a rollback.
        let mut stale = base.clone();
        stale.sequence = 2;
        let err = store.put_manifest(&kid, "src", &stale).unwrap_err();
        assert!(format!("{err:?}").contains("stale manifest"), "{err:?}");
        assert_eq!(store.manifest(&kid, "src").unwrap().sequence, 4);
    }

    #[test]
    fn concurrent_same_chunk_uploads_reserve_quota_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::new(dir.path().to_str().unwrap()));
        let kid = key_id();
        let prefix = new_stream_prefix().unwrap();
        let (sealed, _) = seal_chunk(
            &[3u8; 32],
            &prefix,
            0,
            "src",
            "c1",
            0,
            &vec![0xABu8; 32 * 1024],
        )
        .unwrap();
        for index in 0..25 {
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let mut handles = Vec::new();
            for _ in 0..2 {
                let store = store.clone();
                let sealed = sealed.clone();
                let barrier = barrier.clone();
                let kid = kid.clone();
                handles.push(std::thread::spawn(move || {
                    barrier.wait();
                    store.put_chunk_within(&kid, "src", "c1", index, &sealed, 0)
                }));
            }
            for handle in handles {
                handle.join().unwrap().unwrap();
            }
        }
        assert_eq!(store.usage(&kid).unwrap(), billed_len(sealed.len()) * 25);
    }

    #[test]
    fn concurrent_different_chunk_content_keeps_one_winner() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::new(dir.path().to_str().unwrap()));
        let kid = key_id();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let mut handles = Vec::new();
        // Full chunks: a partial chunk is replaceable, a full one is not.
        let mut chunks = Vec::new();
        for byte in [0x11u8, 0x22] {
            let prefix = new_stream_prefix().unwrap();
            let (sealed, _) = seal_chunk(
                &[3u8; 32],
                &prefix,
                0,
                "src",
                "c1",
                0,
                &vec![byte; CHUNK_PLAIN_SIZE],
            )
            .unwrap();
            chunks.push(sealed);
        }
        let billed = billed_len(chunks[0].len());
        for sealed in chunks {
            let store = store.clone();
            let barrier = barrier.clone();
            let kid = kid.clone();
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                store.put_chunk_within(&kid, "src", "c1", 0, &sealed, 0)
            }));
        }
        let results: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(store.usage(&kid).unwrap(), billed);
    }

    #[test]
    fn key_dir_is_hashed() {
        let d = key_dir("SHA256:abc/def");
        assert!(d.starts_with('k'));
        assert!(!d.contains('/'));
    }
}
