//! Buddy receiver storage (port of `api/internal/buddy/store.go`).
//!
//! Opaque sealed chunks plus signed manifests rooted at one directory. Nothing
//! here can decrypt: the store never sees a key.

use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::envelope::{chunk_plain_len, CHUNK_PLAIN_SIZE, MAX_SEALED_CHUNK_SIZE};
use super::Manifest;

const ENROLLMENT_MARKER: &str = ".enroll-used";
const MAX_LISTED_CHAINS: usize = 500;
const LINKS_FILE: &str = "links.json";
const STREAM_PREFIX_LEN: usize = 6;
const NONCE_SIZE: usize = 12;
const CHUNK_MAGIC: &[u8; 4] = b"NBC2";

/// A storage error, with quota/no-space distinguishable for the HTTP layer.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("quota exceeded: {used} of {quota} bytes are already stored for this key")]
    Quota { used: i64, quota: i64 },
    #[error("not enough free space on the receiver")]
    NoSpace,
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

/// The storage root plus the per-key usage cache.
pub struct Store {
    root: String,
    usage: Mutex<HashMap<String, i64>>,
    loaded: Mutex<HashMap<String, bool>>,
}

impl Store {
    pub fn new(dir: &str) -> Self {
        Self {
            root: dir.to_string(),
            usage: Mutex::new(HashMap::new()),
            loaded: Mutex::new(HashMap::new()),
        }
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
        if index < 0 {
            return Err(other(format!("invalid chunk index {index}")));
        }
        if sealed.len() > MAX_SEALED_CHUNK_SIZE {
            return Err(other(format!(
                "chunk is {} bytes, the maximum is {MAX_SEALED_CHUNK_SIZE}",
                sealed.len()
            )));
        }
        let dir = self.chain_dir(key_id, source, chain)?;
        std::fs::create_dir_all(&dir)?;
        let target = dir.join(chunk_name(index));

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
            replaced = existing.len() as i64;
        }

        let delta = sealed.len() as i64 - replaced;
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

    fn reserve_usage(&self, key_id: &str, delta: i64, quota: i64) -> SResult<()> {
        if delta == 0 {
            return Ok(());
        }
        let used = self.usage_locked(key_id)?;
        if quota > 0 && delta > 0 && used + delta > quota {
            return Err(StoreError::Quota { used, quota });
        }
        if delta > 0 {
            if let Ok(free) = free_space(&self.root) {
                if free - delta < free_space_reserve() {
                    return Err(StoreError::NoSpace);
                }
            }
        }
        let mut usage = self.usage.lock().unwrap();
        let loaded = self.loaded.lock().unwrap();
        if usage.contains_key(key_id) || loaded.get(key_id).copied().unwrap_or(false) {
            *usage.entry(key_id.to_string()).or_insert(0) += delta;
        }
        Ok(())
    }

    fn release_usage(&self, key_id: &str, delta: i64) {
        if delta == 0 {
            return;
        }
        let mut usage = self.usage.lock().unwrap();
        let loaded = self.loaded.lock().unwrap();
        if usage.contains_key(key_id) || loaded.get(key_id).copied().unwrap_or(false) {
            *usage.entry(key_id.to_string()).or_insert(0) -= delta;
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
        let dir = self.chain_dir(key_id, source, &m.chain)?;
        std::fs::create_dir_all(&dir)?;
        let data = serde_json::to_vec_pretty(m).map_err(|e| other(e.to_string()))?;

        let previous = match self.manifest(key_id, source) {
            Ok(cur) => cur.chain,
            Err(_) => String::new(),
        };
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
                    if let Ok(size) = dir_size(&chain_dir) {
                        row.stored_bytes = size;
                    }
                }
                out.push(row);
            }
        }
        out.sort_by(|a, b| a.source.cmp(&b.source));
        Ok(out)
    }

    /// The bytes stored for a key (or every key when empty).
    pub fn usage(&self, key_id: &str) -> SResult<i64> {
        if key_id.is_empty() {
            return self.uncached_usage();
        }
        self.usage_locked(key_id)
    }

    fn usage_locked(&self, key_id: &str) -> SResult<i64> {
        if self
            .loaded
            .lock()
            .unwrap()
            .get(key_id)
            .copied()
            .unwrap_or(false)
        {
            return Ok(self.usage.lock().unwrap().get(key_id).copied().unwrap_or(0));
        }
        let size = dir_size(&self.root_path().join(key_dir(key_id))).unwrap_or(0);
        self.usage.lock().unwrap().insert(key_id.to_string(), size);
        self.loaded.lock().unwrap().insert(key_id.to_string(), true);
        Ok(size)
    }

    fn uncached_usage(&self) -> SResult<i64> {
        match dir_size(&self.root_path()) {
            Ok(size) => Ok(size),
            Err(StoreError::Other(_)) => Ok(0),
            Err(e) => Err(e),
        }
    }

    /// Forget a key's cached usage.
    pub fn invalidate_usage(&self, key_id: &str) {
        self.usage.lock().unwrap().remove(key_id);
        self.loaded.lock().unwrap().remove(key_id);
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
#[derive(Debug, Clone, serde::Serialize)]
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
pub fn validate_chain_name(chain: &str) -> SResult<()> {
    if chain.is_empty()
        || chain.len() > 128
        || chain == "."
        || chain == ".."
        || chain.contains("..")
    {
        return Err(other(format!("invalid chain id {chain:?}")));
    }
    if !chain
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        return Err(other(format!("invalid chain id {chain:?}")));
    }
    Ok(())
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

fn dir_size(dir: &Path) -> SResult<i64> {
    let mut total = 0i64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = match std::fs::read_dir(&current) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(meta) = entry.metadata() {
                total += meta.len() as i64;
            }
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
        let sealed = vec![0u8; 100];
        let err = store
            .put_chunk_within(&kid, "src", "chain", 0, &sealed, 50)
            .unwrap_err();
        assert!(matches!(err, StoreError::Quota { .. }));
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
    fn key_dir_is_hashed() {
        let d = key_dir("SHA256:abc/def");
        assert!(d.starts_with('k'));
        assert!(!d.contains('/'));
    }
}
