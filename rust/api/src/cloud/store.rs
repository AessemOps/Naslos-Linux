//! The S3 [`ChunkStore`]/[`ChunkSource`] implementation and the lineage model.
//!
//! Object layout (also documented in `docs/spec.md` FR-REP-12):
//!
//! ```text
//! <prefix>/<source>/lineages/<lineage>/links/<sequence>-<chain>/
//!     chunks/<index>-g<generation>.nbc
//!     manifest.json
//! ```
//!
//! A *link* is one `zfs send` (= a Buddy chain: own DEK, prefix and manifest).
//! A *lineage* is a full link plus its GUID-contiguous incrementals; retention
//! deletes whole lineages only, so a kept incremental can never lose its base.
//! The generation is part of the chunk key, so a re-sealed tail always creates a
//! new key and `If-None-Match: *` stays valid.

use std::collections::{HashMap, HashSet};
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use super::s3::{Category, S3Error, S3Storage};
use super::{
    chunk_key, link_prefix, manifest_key, parse_link_key, ParsedKeyKind, SIGNATURE_DOMAIN,
};
use crate::buddy::client::{ChainState, ChainSummary, StoredChunk};
use crate::buddy::envelope::{digest_of, Manifest};
use crate::buddy::keys::Identity;
use crate::buddy::transfer::build_restore_sequence;
use crate::buddy::transfer::{ChunkSource, ChunkStore, SequenceAnchor};

/// One published link, verified against the owner's identity.
#[derive(Debug, Clone)]
pub struct LinkInfo {
    pub lineage: String,
    pub sequence: u64,
    pub chain: String,
    pub manifest: Manifest,
}

impl LinkInfo {
    pub fn raw(&self) -> bool {
        self.manifest.kind.ends_with("-raw")
    }

    pub fn plain_bytes(&self) -> i64 {
        self.manifest.chunks.iter().map(|c| c.plain_bytes).sum()
    }

    pub fn incremental(&self) -> bool {
        !self.manifest.from_snapshot.is_empty()
    }

    /// The API `Chain` shape (DM-11 companion, docs/api.md).
    pub fn view(&self) -> super::ChainView {
        super::ChainView {
            chain: self.chain.clone(),
            lineage: self.lineage.clone(),
            kind: if self.incremental() {
                "incremental"
            } else {
                "full"
            }
            .to_string(),
            from_snapshot: self.manifest.from_snapshot.clone(),
            to_snapshot: self.manifest.to_snapshot.clone(),
            from_guid: self.manifest.from_guid.clone(),
            to_guid: self.manifest.to_guid.clone(),
            sequence: self.sequence,
            created_at: Some(self.manifest.created_at),
            chunks: self.manifest.chunks.len(),
            plain_bytes: self.plain_bytes(),
            raw: self.raw(),
        }
    }

    pub fn summary(&self) -> ChainSummary {
        ChainSummary {
            chain: self.chain.clone(),
            kind: self.manifest.kind.clone(),
            created_at: Some(self.manifest.created_at),
            from_snapshot: self.manifest.from_snapshot.clone(),
            to_snapshot: self.manifest.to_snapshot.clone(),
            from_guid: self.manifest.from_guid.clone(),
            to_guid: self.manifest.to_guid.clone(),
            chunks: self.manifest.chunks.len(),
            sequence: self.sequence,
        }
    }
}

/// The per (destination, source) freshness anchor and resume files.
pub struct AnchorPaths {
    pub anchor: PathBuf,
    pub ledger: PathBuf,
}

impl AnchorPaths {
    pub fn new(dir: &FsPath, source: &str) -> Self {
        let digest = sha256_hex(source.as_bytes());
        Self {
            anchor: dir.join(format!("{digest}.seq")),
            ledger: dir.join(format!("{digest}.ledger.json")),
        }
    }
}

pub fn last_published(anchor: &FsPath) -> Result<u64, String> {
    match std::fs::read_to_string(anchor) {
        Ok(text) => text
            .trim()
            .parse::<u64>()
            .map_err(|e| format!("parsing the sequence anchor {}: {e}", anchor.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(format!(
            "reading the sequence anchor {}: {e}",
            anchor.display()
        )),
    }
}

pub fn record_published(anchor: &FsPath, sequence: u64) -> Result<(), String> {
    if let Some(dir) = anchor.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        }
    }
    crate::chartsrepo::write_atomic(
        &anchor.display().to_string(),
        format!("{sequence}\n").as_bytes(),
        0o600,
        0o700,
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LedgerEntry {
    generation: u16,
    sha256: String,
    size: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Ledger {
    #[serde(default)]
    entries: HashMap<String, LedgerEntry>,
}

impl Ledger {
    fn load(path: &FsPath) -> Self {
        match std::fs::read(path) {
            Ok(data) => serde_json::from_slice(&data).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    fn save(&self, path: &FsPath) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| format!("creating {}: {e}", dir.display()))?;
            }
        }
        let data = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        crate::chartsrepo::write_atomic(&path.display().to_string(), &data, 0o600, 0o700)
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// A push into one link directory. It owns the local ledger used to skip
/// already-uploaded chunks on resume: list-only metadata has no portable digest,
/// so only a chunk recorded here at the same generation may be skipped.
pub struct CloudChunkStore {
    pub s3: Arc<S3Storage>,
    pub identity: Arc<Identity>,
    pub anchor: PathBuf,
    pub ledger: PathBuf,
    pub lineage: String,
    pub sequence: u64,
    pub chain: String,
}

impl CloudChunkStore {
    pub fn new(
        s3: Arc<S3Storage>,
        identity: Arc<Identity>,
        paths: AnchorPaths,
        lineage: String,
        sequence: u64,
        chain: String,
    ) -> Self {
        Self {
            s3,
            identity,
            anchor: paths.anchor,
            ledger: paths.ledger,
            lineage,
            sequence,
            chain,
        }
    }

    pub fn link_prefix(&self, source: &str) -> String {
        let source_root = self.s3.source_root(source);
        let rest = format!(
            "lineages/{}/links/{}-{}",
            self.lineage, self.sequence, self.chain
        );
        format!("{source_root}/{rest}")
    }

    /// Delete the local resume ledger after a successful publish (or before a
    /// fresh link replaces an abandoned one).
    pub fn clear_ledger(&self) {
        let _ = std::fs::remove_file(&self.ledger);
    }
}

impl SequenceAnchor for CloudChunkStore {
    fn last_published_sequence(&self, _source: &str) -> Result<u64, String> {
        last_published(&self.anchor)
    }

    fn record_published_sequence(&self, _source: &str, sequence: u64) -> Result<(), String> {
        record_published(&self.anchor, sequence)
    }
}

impl ChunkStore for CloudChunkStore {
    async fn stored_chunks(
        &self,
        source: &str,
        chain: &str,
    ) -> Result<(HashMap<i64, StoredChunk>, bool), String> {
        let source_root = self.s3.source_root(source);
        let prefix = format!("{source_root}/lineages/{}/links/", self.lineage);
        let objects = self.s3.list(&prefix).await.map_err(|e| e.to_string())?;
        let ledger = Ledger::load(&self.ledger);
        let mut chunks = HashMap::new();
        let mut published = false;
        for object in objects {
            let key = object.location.as_ref();
            let Some(parsed) = parse_link_key(&source_root, key) else {
                continue;
            };
            if parsed.chain != chain {
                continue;
            }
            match parsed.kind {
                ParsedKeyKind::Manifest => published = true,
                ParsedKeyKind::Chunk { index, generation } => {
                    // Only a digest from this attempt's ledger may authorize a
                    // skip. An unledgered object is treated as unknown, so a
                    // non-tail mismatch aborts and the tail gets a new key.
                    let digest = ledger
                        .entries
                        .get(&index.to_string())
                        .filter(|entry| entry.generation == generation)
                        .map(|entry| entry.sha256.clone())
                        .unwrap_or_default();
                    chunks.insert(index, StoredChunk { digest, generation });
                }
                ParsedKeyKind::Other => {}
            }
        }
        Ok((chunks, published))
    }

    async fn put_chunk(
        &self,
        source: &str,
        chain: &str,
        index: i64,
        sealed: &[u8],
    ) -> Result<(), String> {
        if sealed.len() < 16 {
            return Err("sealed chunk is too short".to_string());
        }
        let generation = u16::from_be_bytes([sealed[14], sealed[15]]);
        let link = self.link_prefix(source);
        let key = chunk_key(&link, index, generation);
        self.s3
            .put_create_or_equal(&key, sealed)
            .await
            .map_err(|e| e.to_string())?;
        let mut ledger = Ledger::load(&self.ledger);
        ledger.entries.insert(
            index.to_string(),
            LedgerEntry {
                generation,
                sha256: digest_of(sealed),
                size: sealed.len(),
            },
        );
        ledger.save(&self.ledger).map_err(|e| {
            format!("the chunk was stored but the resume ledger could not be recorded: {e}")
        })?;
        let _ = chain;
        Ok(())
    }

    async fn publish_manifest(
        &self,
        source: &str,
        manifest: &Manifest,
        payload: &[u8],
    ) -> Result<(), String> {
        if manifest.chain != self.chain || manifest.sequence != self.sequence {
            return Err(format!(
                "refusing to publish manifest for {}-{} at {}-{}",
                manifest.sequence, manifest.chain, self.sequence, self.chain
            ));
        }
        let link = self.link_prefix(source);
        self.s3
            .put_create_or_equal(&manifest_key(&link), payload)
            .await
            .map_err(|e| e.to_string())?;
        self.clear_ledger();
        Ok(())
    }

    async fn prune(&self, source: &str, keep: usize) -> Result<usize, String> {
        prune_lineages(&self.s3, &self.identity, source, &self.lineage, keep).await
    }
}

/// A restore/verify reader. `locations` are the links listed for the source;
/// `fetch_manifest` fills the cache `fetch_chunk` reads the generation from.
pub struct CloudChunkSource {
    pub s3: Arc<S3Storage>,
    pub locations: HashMap<String, LinkInfo>,
    pub anchor: PathBuf,
    manifests: Mutex<HashMap<String, Manifest>>,
}

impl CloudChunkSource {
    pub fn new(s3: Arc<S3Storage>, locations: &[LinkInfo], anchor: PathBuf) -> Self {
        let mut map = HashMap::new();
        for link in locations {
            map.insert(link.chain.clone(), link.clone());
        }
        Self {
            s3,
            locations: map,
            anchor,
            manifests: Mutex::new(HashMap::new()),
        }
    }

    fn link(&self, chain: &str) -> Result<&LinkInfo, String> {
        self.locations
            .get(chain)
            .ok_or_else(|| format!("chain {chain} is not part of this restore sequence"))
    }

    pub fn manifest_of(&self, chain: &str) -> Option<Manifest> {
        self.manifests.lock().unwrap().get(chain).cloned()
    }
}

impl SequenceAnchor for CloudChunkSource {
    fn last_published_sequence(&self, _source: &str) -> Result<u64, String> {
        last_published(&self.anchor)
    }

    fn record_published_sequence(&self, _source: &str, _sequence: u64) -> Result<(), String> {
        // A restore never publishes.
        Ok(())
    }
}

impl ChunkSource for CloudChunkSource {
    async fn fetch_manifest(&self, _source: &str, chain: &str) -> Result<Manifest, String> {
        let link = self.link(chain)?.clone();
        let key = manifest_key(&link_prefix(
            &self.s3.root_prefix(),
            &link.manifest.source,
            &link.lineage,
            link.sequence,
            &link.chain,
        ));
        let bytes = self.s3.get(&key).await.map_err(|e| e.to_string())?;
        let manifest: Manifest =
            serde_json::from_slice(&bytes).map_err(|e| format!("parsing manifest: {e}"))?;
        if manifest.chain != chain {
            return Err("the manifest path and its chain disagree".to_string());
        }
        self.manifests
            .lock()
            .unwrap()
            .insert(chain.to_string(), manifest.clone());
        Ok(manifest)
    }

    async fn fetch_chunk(&self, _source: &str, chain: &str, index: i64) -> Result<Vec<u8>, String> {
        let link = self.link(chain)?;
        let manifest = match self.manifest_of(chain) {
            Some(m) => m,
            None => self.fetch_manifest("", chain).await?,
        };
        let chunk = manifest
            .chunks
            .iter()
            .find(|c| c.index == index)
            .ok_or_else(|| format!("chunk {index} is not in the manifest of chain {chain}"))?;
        let key = super::chunk_key(
            &super::link_prefix(
                &self.s3.root_prefix(),
                &manifest.source,
                &link.lineage,
                link.sequence,
                &link.chain,
            ),
            index,
            chunk.generation,
        );
        match self.s3.get(&key).await {
            Ok(bytes) => Ok(bytes),
            Err(e) if e.category == Category::NotFound => {
                Err(format!("chunk {index} is missing from the bucket"))
            }
            Err(e) => Err(e.to_string()),
        }
    }
}

/// List and verify every published link of a source, newest sequence first.
pub async fn list_links(
    s3: &S3Storage,
    source: &str,
    identity: &Identity,
) -> Result<Vec<LinkInfo>, String> {
    let source_root = s3.source_root(source);
    let objects = s3.list(&format!("{source_root}/lineages/")).await?;
    let mut links = Vec::new();
    for object in objects {
        let key = object.location.as_ref();
        if parse_link_key(&source_root, key).map(|p| p.kind) != Some(ParsedKeyKind::Manifest) {
            continue;
        }
        let bytes = s3.get(key).await?;
        let manifest: Manifest =
            serde_json::from_slice(&bytes).map_err(|e| format!("parsing manifest {key}: {e}"))?;
        super::verify_manifest(&manifest, identity)
            .map_err(|e| format!("refusing the chain at {key}: {e}"))?;
        let parsed = parse_link_key(&source_root, key)
            .ok_or_else(|| format!("unexpected manifest path {key}"))?;
        if manifest.source != source {
            return Err(format!(
                "the manifest at {key} belongs to {:?}, not {source:?}",
                manifest.source
            ));
        }
        if parsed.chain != manifest.chain || parsed.sequence != manifest.sequence {
            return Err(format!(
                "the manifest at {key} claims chain {} sequence {}",
                manifest.chain, manifest.sequence
            ));
        }
        links.push(LinkInfo {
            lineage: parsed.lineage,
            sequence: parsed.sequence,
            chain: parsed.chain,
            manifest,
        });
    }
    links.sort_by_key(|link| std::cmp::Reverse(link.sequence));
    Ok(links)
}

/// Pick the link sequence that rebuilds a source up to `target` (default the
/// newest), oldest first. Reuses the pure Buddy walk so the GUID/loop/missing
/// base rules stay in one place.
pub fn select_links(links: &[LinkInfo], target: Option<&str>) -> Result<Vec<LinkInfo>, String> {
    let summaries: Vec<ChainSummary> = links.iter().map(LinkInfo::summary).collect();
    let chosen = build_restore_sequence(&summaries, target)?;
    let by_chain: HashMap<&str, &LinkInfo> = links.iter().map(|l| (l.chain.as_str(), l)).collect();
    chosen
        .iter()
        .map(|summary| {
            by_chain
                .get(summary.chain.as_str())
                .map(|l| (*l).clone())
                .ok_or_else(|| {
                    format!(
                        "chain {} disappeared while building the sequence",
                        summary.chain
                    )
                })
        })
        .collect()
}

/// The lineages to delete for `keep` retained lineages. Rules:
/// - never delete the current lineage, and never delete a lineage without a
///   newer lineage that has a published full;
/// - delete whole lineages only, newest link first, and return the links in
///   that order so the caller can delete each manifest before its chunks.
pub fn lineage_retention_plan(
    links: &[LinkInfo],
    current_lineage: &str,
    keep: usize,
) -> Vec<LinkInfo> {
    let keep = keep.max(1);
    let mut by_lineage: HashMap<&str, Vec<&LinkInfo>> = HashMap::new();
    for link in links {
        by_lineage
            .entry(link.lineage.as_str())
            .or_default()
            .push(link);
    }
    // Newest lineage first, by its highest sequence.
    let mut lineages: Vec<(&str, u64, bool)> = by_lineage
        .iter()
        .map(|(lineage, links)| {
            let newest = links.iter().map(|l| l.sequence).max().unwrap_or(0);
            let has_full = links.iter().any(|l| !l.incremental());
            (*lineage, newest, has_full)
        })
        .collect();
    lineages.sort_by_key(|lineage| std::cmp::Reverse(lineage.1));
    let mut retained: HashSet<&str> = lineages
        .iter()
        .take(keep)
        .map(|(lineage, _, _)| *lineage)
        .collect();
    // The lineage a send just published is protected even if older lineages
    // were imported out of order.
    retained.insert(current_lineage);
    let mut doomed: Vec<&str> = Vec::new();
    let mut newer_has_full = false;
    for (lineage, _, has_full) in &lineages {
        if !retained.contains(lineage) && newer_has_full && *has_full {
            doomed.push(lineage);
        }
        newer_has_full |= *has_full;
    }
    let mut out = Vec::new();
    for lineage in doomed {
        let mut links: Vec<LinkInfo> = by_lineage[lineage].iter().map(|l| (*l).clone()).collect();
        links.sort_by_key(|link| std::cmp::Reverse(link.sequence));
        out.extend(links);
    }
    out
}

/// Delete old lineages and sweep unreferenced sealed chunks older than 24 h.
pub async fn prune_lineages(
    s3: &S3Storage,
    identity: &Identity,
    source: &str,
    current_lineage: &str,
    keep: usize,
) -> Result<usize, String> {
    let links = list_links(s3, source, identity).await?;
    let doomed = lineage_retention_plan(&links, current_lineage, keep);
    let deleted_lineages: HashSet<&str> = doomed.iter().map(|l| l.lineage.as_str()).collect();
    let source_root = s3.source_root(source);
    for link in &doomed {
        let link_dir = link_prefix(
            &s3.root_prefix(),
            source,
            &link.lineage,
            link.sequence,
            &link.chain,
        );
        // Manifest first: a crash leaves an unlisted prefix, never a manifest
        // pointing at deleted chunks.
        s3.delete(&manifest_key(&link_dir)).await?;
        let objects = s3.list(&format!("{link_dir}/chunks/")).await?;
        for object in objects {
            s3.delete(object.location.as_ref()).await?;
        }
    }
    // Sweep stale unledgered chunk objects (abandoned links and old
    // generations). Only objects no surviving manifest references and older
    // than 24 h are eligible.
    let remaining: Vec<LinkInfo> = links
        .into_iter()
        .filter(|l| !deleted_lineages.contains(l.lineage.as_str()))
        .collect();
    let mut referenced: HashSet<String> = HashSet::new();
    for link in &remaining {
        let link_dir = link_prefix(
            &s3.root_prefix(),
            source,
            &link.lineage,
            link.sequence,
            &link.chain,
        );
        for chunk in &link.manifest.chunks {
            referenced.insert(chunk_key(&link_dir, chunk.index, chunk.generation));
        }
    }
    let cutoff = Utc::now() - chrono::Duration::hours(24);
    let all = s3.list(&format!("{source_root}/lineages/")).await?;
    for object in all {
        let key = object.location.as_ref();
        if referenced.contains(key) {
            continue;
        }
        if parse_link_key(&source_root, key).map(|p| p.kind) == Some(ParsedKeyKind::Manifest) {
            continue;
        }
        if object.last_modified < cutoff {
            s3.delete(key).await?;
        }
    }
    Ok(deleted_lineages.len())
}

/// A failed S3 access is a soft error for callers that only need "not found".
pub fn is_not_found(error: &S3Error) -> bool {
    error.category == Category::NotFound
}

/// `NCLOUD1|` is exported for the spec test that proves the domain separation.
pub fn signature_domain() -> &'static [u8] {
    SIGNATURE_DOMAIN
}

/// The next link sequence for a push (the anchor records published sequences).
pub fn next_sequence(anchor: &FsPath) -> Result<u64, String> {
    Ok(last_published(anchor)? + 1)
}

/// Whether an interrupted send may resume the same link. Both snapshots must
/// still exist with their recorded GUIDs, and the raw flag and dataset must
/// match; otherwise the caller starts a fresh link (and the orphaned objects are
/// swept by retention).
pub fn resume_compatible(
    state: &super::send::SendResume,
    to_snapshot: Option<&str>,
    to_guid: Option<&str>,
    from_snapshot: Option<&str>,
    from_guid: Option<&str>,
    raw: bool,
) -> bool {
    if state.raw != raw {
        return false;
    }
    if to_snapshot != Some(state.to_snapshot.as_str()) || to_guid != Some(state.to_guid.as_str()) {
        return false;
    }
    if state.from_snapshot.is_empty() {
        return from_snapshot.is_none() || from_snapshot == Some("");
    }
    from_snapshot == Some(state.from_snapshot.as_str())
        && from_guid == Some(state.from_guid.as_str())
}

/// A `ChainState` from a resume file is reused only when it still matches.
pub fn chain_state_matches(chain: &ChainState, source: &str, kind: &str) -> bool {
    chain.source == source && chain.kind == kind
}

/// Test helper: build a link for retention tests.
#[cfg(test)]
pub(crate) fn test_link(
    lineage: &str,
    sequence: u64,
    chain: &str,
    from_snapshot: &str,
) -> LinkInfo {
    LinkInfo {
        lineage: lineage.into(),
        sequence,
        chain: chain.into(),
        manifest: Manifest {
            version: crate::buddy::envelope::ENVELOPE_VERSION,
            source: "tank/data".into(),
            chain: chain.into(),
            kind: "zfs-send".into(),
            from_snapshot: from_snapshot.into(),
            to_snapshot: format!("snap-{chain}"),
            from_guid: if from_snapshot.is_empty() {
                String::new()
            } else {
                format!("guid-{from_snapshot}")
            },
            to_guid: format!("guid-snap-{chain}"),
            created_at: chrono::DateTime::parse_from_rfc3339("2026-01-05T10:15:00Z")
                .unwrap()
                .with_timezone(&Utc),
            key_id: String::new(),
            sequence,
            generation: 1,
            stream_prefix: "AAAAAA==".into(),
            chunk_plain_size: crate::buddy::envelope::CHUNK_PLAIN_SIZE as i64,
            chunks: vec![crate::buddy::envelope::ManifestChunk {
                index: 0,
                plain_bytes: 1024,
                sealed_bytes: 1024 + 36,
                generation: 1,
                sha256_plain: "00".into(),
            }],
            dek_wrapped: "AAAA".into(),
            signature: String::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn links() -> Vec<LinkInfo> {
        // Newest first, as list_links returns them.
        vec![
            test_link("lin2", 6, "f", "snap-e"),
            test_link("lin2", 5, "e", "snap-d"),
            test_link("lin2", 4, "d", ""),
            test_link("lin1", 3, "c", "snap-b"),
            test_link("lin1", 2, "b", "snap-a"),
            test_link("lin1", 1, "a", ""),
        ]
    }

    #[test]
    fn retention_keeps_whole_newest_lineages_and_deletes_newest_link_first() {
        let plan = lineage_retention_plan(&links(), "lin2", 2);
        assert!(plan.is_empty(), "{plan:?}");
        let plan = lineage_retention_plan(&links(), "lin2", 1);
        assert_eq!(
            plan.iter().map(|l| l.chain.as_str()).collect::<Vec<_>>(),
            ["c", "b", "a"]
        );
    }

    #[test]
    fn retention_never_deletes_the_current_lineage() {
        let plan = lineage_retention_plan(&links(), "lin1", 0);
        assert!(plan.is_empty(), "the current lineage is always protected");
    }

    #[test]
    fn retention_only_deletes_a_lineage_with_a_newer_full() {
        let mut broken = links();
        // The newer lineage has no full link: it must not cause a deletion.
        for link in broken.iter_mut().filter(|l| l.lineage == "lin2") {
            link.manifest.from_snapshot = "snap".into();
        }
        let plan = lineage_retention_plan(&broken, "lin2", 1);
        assert!(plan.is_empty(), "{plan:?}");
    }

    #[test]
    fn select_links_returns_the_guid_walk_oldest_first() {
        let chosen = select_links(&links(), Some("e")).unwrap();
        assert_eq!(
            chosen.iter().map(|l| l.chain.as_str()).collect::<Vec<_>>(),
            ["d", "e"]
        );
    }

    #[test]
    fn the_ledger_only_authorizes_the_same_generation() {
        let entry = LedgerEntry {
            generation: 7,
            sha256: "abc".into(),
            size: 10,
        };
        let mut ledger = Ledger::default();
        ledger.entries.insert("3".into(), entry);
        assert_eq!(
            ledger
                .entries
                .get("3")
                .filter(|e| e.generation == 7)
                .map(|e| e.sha256.clone()),
            Some("abc".to_string())
        );
        assert!(ledger
            .entries
            .get("3")
            .filter(|e| e.generation == 8)
            .is_none());
    }

    #[test]
    fn resume_requires_the_same_snapshots_guids_and_raw_flag() {
        let state = crate::cloud::send::SendResume {
            destination: "d".into(),
            source: "tank/data".into(),
            dataset: "tank/data".into(),
            raw: true,
            from_snapshot: "base".into(),
            from_guid: "g0".into(),
            to_snapshot: "top".into(),
            to_guid: "g1".into(),
            lineage: "lin".into(),
            chain: ChainState::default(),
        };
        assert!(resume_compatible(
            &state,
            Some("top"),
            Some("g1"),
            Some("base"),
            Some("g0"),
            true
        ));
        assert!(!resume_compatible(
            &state,
            Some("top"),
            Some("other"),
            Some("base"),
            Some("g0"),
            true
        ));
        assert!(!resume_compatible(
            &state,
            Some("top"),
            Some("g1"),
            Some("base"),
            Some("g0"),
            false
        ));
        assert!(!resume_compatible(
            &state,
            Some("top"),
            Some("g1"),
            None,
            None,
            true
        ));
    }

    #[test]
    fn the_next_sequence_starts_at_one() {
        let dir = tempfile::tempdir().unwrap();
        let anchor = dir.path().join("x.seq");
        assert_eq!(next_sequence(&anchor).unwrap(), 1);
        record_published(&anchor, 7).unwrap();
        assert_eq!(next_sequence(&anchor).unwrap(), 8);
        assert_eq!(last_published(&anchor).unwrap(), 7);
    }

    #[test]
    fn is_not_found_matches_only_the_missing_object_category() {
        assert!(is_not_found(&S3Error {
            category: Category::NotFound,
            message: "x".into()
        }));
        assert!(!is_not_found(&S3Error {
            category: Category::Auth,
            message: "x".into()
        }));
    }
}
