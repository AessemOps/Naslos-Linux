//! Transport-independent push/restore core shared by every chunk backend.
//!
//! A backend only moves sealed bytes and signed manifests; everything that
//! decides *what* is sealed — the per-attempt nonce generation, the
//! persist-before-upload rule, the re-seal compare that lets a resumed push
//! skip a stored chunk, the tail-only mismatch tolerance, the manifest
//! cross-checks and per-chunk digest checks on restore — lives here exactly
//! once. Duplicating it per transport would risk AES-GCM nonce reuse.
//!
//! The Buddy HTTP receiver ([`super::client::Client`]) is one implementation of
//! [`ChunkStore`]/[`ChunkSource`]; its wire format is unchanged.

use std::collections::{HashMap, HashSet};
use std::future::Future;

use chrono::Utc;
use tokio::io::{AsyncRead, AsyncReadExt};

use super::client::{
    valid_chain_id, ChainState, ChainSummary, PushOptions, PushProgress, PushResult,
    RestoreOptions, RestoreResult, StoredChunk,
};
use super::envelope::{
    digest_of, open_chunk, seal_chunk, unwrap_dek, wrap_dek, Manifest, ManifestChunk,
    CHUNK_PLAIN_SIZE, ENVELOPE_VERSION,
};
use super::keys::Identity;
use super::peers::validate_source;

/// Bytes a sealed chunk adds to its plaintext (version, generation, nonce, tag).
pub const SEALED_OVERHEAD: usize = 4 + 4 + 12 + 16;

/// The local freshness anchor: the newest sequence this instance published for
/// a source at one destination. Restores refuse anything older (PF-H3).
pub trait SequenceAnchor: Sync {
    /// `0` when nothing was published (or no anchor is configured).
    fn last_published_sequence(&self, source: &str) -> Result<u64, String>;
    /// Record a successful publish.
    fn record_published_sequence(&self, source: &str, sequence: u64) -> Result<(), String>;
}

/// Where a push writes sealed chunks and the signed manifest.
pub trait ChunkStore: SequenceAnchor {
    /// The chunks already stored for `chain` (index → digest/generation) and
    /// whether the chain's manifest is already published.
    fn stored_chunks(
        &self,
        source: &str,
        chain: &str,
    ) -> impl Future<Output = Result<(HashMap<i64, StoredChunk>, bool), String>> + Send;

    /// Store one sealed chunk.
    fn put_chunk(
        &self,
        source: &str,
        chain: &str,
        index: i64,
        sealed: &[u8],
    ) -> impl Future<Output = Result<(), String>> + Send;

    /// Publish the signed manifest (`payload` is its serialized form, already
    /// checked against the size cap). This is the chain's completion marker.
    fn publish_manifest(
        &self,
        source: &str,
        manifest: &Manifest,
        payload: &[u8],
    ) -> impl Future<Output = Result<(), String>> + Send;

    /// Apply retention after a publish; returns the number of removed chains.
    fn prune(
        &self,
        source: &str,
        keep: usize,
    ) -> impl Future<Output = Result<usize, String>> + Send;
}

/// Where a restore reads manifests and sealed chunks from.
pub trait ChunkSource: SequenceAnchor {
    /// The manifest of `chain` (the newest when `chain` is empty).
    fn fetch_manifest(
        &self,
        source: &str,
        chain: &str,
    ) -> impl Future<Output = Result<Manifest, String>> + Send;

    /// One sealed chunk.
    fn fetch_chunk(
        &self,
        source: &str,
        chain: &str,
        index: i64,
    ) -> impl Future<Output = Result<Vec<u8>, String>> + Send;
}

/// Signs a manifest in place (sets `key_id` and `signature`).
pub type SignManifestFn = dyn Fn(&mut Manifest, &Identity) -> Result<(), String> + Send + Sync;
/// Verifies a manifest's signature against the identity.
pub type VerifyManifestFn = dyn Fn(&Manifest, &Identity) -> Result<(), String> + Send + Sync;
/// Called between chunks; an `Err` aborts the transfer with that message.
pub type CancelCheckFn = dyn Fn() -> Result<(), String> + Send + Sync;

/// Transport-specific policy for [`push_with`].
pub struct PushPolicy<'a> {
    pub sign_manifest: &'a SignManifestFn,
    /// The largest serialized manifest the destination accepts.
    pub max_manifest_bytes: usize,
    pub check_cancelled: &'a CancelCheckFn,
}

/// Transport-specific policy for [`restore_chain`].
pub struct RestorePolicy<'a> {
    pub verify_signature: &'a VerifyManifestFn,
    pub check_cancelled: &'a CancelCheckFn,
}

fn buddy_sign(manifest: &mut Manifest, identity: &Identity) -> Result<(), String> {
    manifest.sign(identity)
}

fn buddy_verify(manifest: &Manifest, identity: &Identity) -> Result<(), String> {
    manifest.verify_signature(&identity.public_key)
}

/// A cancellation check that never cancels.
pub fn never_cancelled() -> Result<(), String> {
    Ok(())
}

impl PushPolicy<'static> {
    /// The Buddy receiver policy: `Manifest::sign`, the 16 MiB manifest cap,
    /// no cancellation check (a Buddy send is cancelled by aborting its task).
    pub fn buddy() -> Self {
        Self {
            sign_manifest: &buddy_sign,
            max_manifest_bytes: super::client::MAX_MANIFEST_BYTES,
            check_cancelled: &never_cancelled,
        }
    }
}

impl RestorePolicy<'static> {
    /// The Buddy receiver policy: `Manifest::verify_signature` against the
    /// identity's public key, no cancellation check.
    pub fn buddy() -> Self {
        Self {
            verify_signature: &buddy_verify,
            check_cancelled: &never_cancelled,
        }
    }
}

/// The generation a push attempt uses. It is strictly greater than both the
/// persisted generation and every stored chunk's generation, so a retry can
/// never re-use a (prefix, generation, index) nonce for different plaintext.
pub fn next_attempt_generation(persisted: u16, stored_max: u16) -> Result<u16, String> {
    persisted.max(stored_max).checked_add(1).ok_or_else(|| {
        "the chain's nonce generation counter is exhausted; start a new chain".to_string()
    })
}

/// Build the chains needed to rebuild a source up to `target` (the first,
/// i.e. newest-listed, chain when `None`), oldest first, by walking each
/// chain's `from_guid` back to the chain that produced it. Refuses a missing
/// base and a loop. The result still has to pass
/// [`super::client::validate_restore_sequence`].
pub fn build_restore_sequence(
    chains: &[ChainSummary],
    target: Option<&str>,
) -> Result<Vec<ChainSummary>, String> {
    if chains.is_empty() {
        return Err("no backup is stored here".to_string());
    }
    let target = match target {
        None => chains[0].clone(),
        Some(chain) => chains
            .iter()
            .find(|c| c.chain == chain)
            .cloned()
            .ok_or_else(|| format!("chain {chain} is not stored here"))?,
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
                "the chain history contains a loop at {}",
                base.chain
            ));
        }
        sequence.push((*base).clone());
        current = (*base).clone();
    }
    sequence.reverse();
    Ok(sequence)
}

/// Stream the plaintext, seal chunk by chunk, store what is missing and
/// publish a signed manifest.
pub async fn push_with<S: ChunkStore>(
    store: &S,
    identity: &Identity,
    opts: PushOptions<'_>,
    policy: &PushPolicy<'_>,
) -> (PushResult, Option<String>) {
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
    let (stored, published) = match store.stored_chunks(opts.source, &state.chain).await {
        Ok(v) => v,
        Err(e) => return (result, Some(e)),
    };
    let tail_index = stored.keys().copied().max().unwrap_or(-1);
    let stored_max_generation = stored.values().map(|c| c.generation).max().unwrap_or(0);
    let attempt_generation = match next_attempt_generation(state.generation, stored_max_generation)
    {
        Ok(g) => g,
        Err(e) => return (result, Some(e)),
    };
    // Persist before transmitting: if this attempt dies mid-stream the next
    // one must start from a fresh generation, not the old one.
    state.generation = attempt_generation;
    if let Err(e) = (opts.save_state)(&state) {
        return (result, Some(format!("persisting the resume state: {e}")));
    }
    let last_sequence = match store.last_published_sequence(opts.source) {
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

    let generation = attempt_generation;
    let mut buf = vec![0u8; CHUNK_PLAIN_SIZE];
    let mut index: i64 = 0;
    loop {
        if let Err(e) = (policy.check_cancelled)() {
            return (result, Some(e));
        }
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
                chunk_generation = generation;
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
                    }
                    Err(e) => return (result, Some(e)),
                }
            } else {
                entry.sealed_bytes = n as i64 + SEALED_OVERHEAD as i64;
                upload = false;
            }
        }
        if upload {
            if let Err(e) = store
                .put_chunk(opts.source, &state.chain, index, &sealed)
                .await
            {
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
        state.resume_index = index;
        if let Err(e) = (opts.save_state)(&state) {
            return (result, Some(format!("persisting the resume state: {e}")));
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
    manifest.generation = generation;

    if let Some(before) = opts.before_publish {
        if let Err(e) = before(&result) {
            return (result, Some(e));
        }
    }

    let kek = match identity.kek_bytes() {
        Ok(k) => k,
        Err(e) => return (result, Some(e)),
    };
    let wrapped = match wrap_dek(&kek, &dek, opts.source, &state.chain) {
        Ok(w) => w,
        Err(e) => return (result, Some(e)),
    };
    manifest.dek_wrapped = wrapped;
    if let Err(e) = (policy.sign_manifest)(&mut manifest, identity) {
        return (result, Some(e));
    }
    let payload = match serde_json::to_vec(&manifest) {
        Ok(p) => p,
        Err(e) => return (result, Some(e.to_string())),
    };
    if payload.len() > policy.max_manifest_bytes {
        return (
            result,
            Some(format!(
                "chain {} has {} chunks and its manifest is {} bytes, over the {}-byte limit",
                state.chain,
                manifest.chunks.len(),
                payload.len(),
                policy.max_manifest_bytes
            )),
        );
    }
    if let Err(e) = store
        .publish_manifest(opts.source, &manifest, &payload)
        .await
    {
        return (result, Some(e));
    }
    if let Err(e) = store.record_published_sequence(opts.source, sequence) {
        return (result, Some(e));
    }
    if opts.prune_keep > 0 {
        match store.prune(opts.source, opts.prune_keep).await {
            Ok(removed) => result.pruned_chains = removed,
            Err(e) => return (result, Some(e)),
        }
    }
    result.state = Some(state);
    result.manifest = Some(manifest);
    (result, None)
}

/// Pull one chain's chunks back, decrypt them and write the original stream.
/// The manifest is verified (version, source, chain, signature, sequence and
/// every field of the caller's unsigned summary) before a chunk is fetched.
pub async fn restore_chain<S: ChunkSource>(
    src: &S,
    identity: &Identity,
    opts: RestoreOptions<'_>,
    policy: &RestorePolicy<'_>,
) -> (RestoreResult, Option<String>) {
    let mut result = RestoreResult {
        source: opts.source.to_string(),
        ..Default::default()
    };
    if let Err(e) = validate_source(opts.source) {
        return (result, Some(e));
    }
    let manifest = match src.fetch_manifest(opts.source, opts.chain).await {
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
    if let Err(e) = (policy.verify_signature)(&manifest, identity) {
        if !opts.skip_signature_check {
            return (result, Some(format!("refusing to restore: {e}")));
        }
    }
    if let Some(expected) = opts.expected_sequence {
        if manifest.sequence != expected {
            return (
                result,
                Some(format!(
                    "refusing to restore chain {}: the receiver returned sequence {}, but the verified sequence was {expected}",
                    manifest.chain, manifest.sequence
                )),
            );
        }
    } else if opts.chain.is_empty() {
        match src.last_published_sequence(opts.source) {
            Ok(last) if manifest.sequence < last => {
                return (result, Some(format!("refusing to restore: the receiver returned a stale manifest (sequence {}, latest published {last})", manifest.sequence)));
            }
            Err(e) => return (result, Some(e)),
            _ => {}
        }
    }
    // The chain listing the caller followed is receiver-supplied and
    // unsigned. Cross-check every identity-bearing field against the
    // signed manifest before reading a chunk or writing plaintext, so a
    // receiver cannot answer the signed request for chain A while the
    // restore sequence was built from a different chain's summary.
    let mut mismatch = None;
    for (field, expected, actual) in [
        (
            "fromGUID",
            opts.expected_from_guid,
            manifest.from_guid.as_str(),
        ),
        ("toGUID", opts.expected_to_guid, manifest.to_guid.as_str()),
        ("kind", opts.expected_kind, manifest.kind.as_str()),
        (
            "fromSnapshot",
            opts.expected_from_snapshot,
            manifest.from_snapshot.as_str(),
        ),
        (
            "toSnapshot",
            opts.expected_to_snapshot,
            manifest.to_snapshot.as_str(),
        ),
    ] {
        if expected != actual {
            mismatch = Some(format!(
                "refusing to restore chain {}: the signed manifest's {field} is {actual:?}, but the receiver's chain summary said {expected:?}",
                manifest.chain
            ));
            break;
        }
    }
    if let Some(e) = mismatch {
        return (result, Some(e));
    }
    let prefix = match manifest.stream_prefix_bytes() {
        Ok(p) => p,
        Err(e) => return (result, Some(format!("refusing to restore: {e}"))),
    };
    let kek = match identity.kek_bytes() {
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
        if let Err(e) = (policy.check_cancelled)() {
            return (result, Some(e));
        }
        let sealed = match src
            .fetch_chunk(&manifest.source, &manifest.chain, chunk.index)
            .await
        {
            Ok(data) => data,
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

/// Read up to `buf.len()` bytes, returning (n, short_tail). `short_tail` is true
/// when the stream ended part-way through the buffer (Go's ErrUnexpectedEOF).
pub(crate) async fn read_full(
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn chain(name: &str, from: &str, to: &str, sequence: u64) -> ChainSummary {
        ChainSummary {
            chain: name.to_string(),
            from_guid: from.to_string(),
            to_guid: to.to_string(),
            sequence,
            ..Default::default()
        }
    }

    fn names(seq: &[ChainSummary]) -> Vec<&str> {
        seq.iter().map(|c| c.chain.as_str()).collect()
    }

    #[test]
    fn build_restore_sequence_walks_a_contiguous_lineage_oldest_first() {
        // Listed newest first, as the receiver does.
        let chains = vec![
            chain("c", "g2", "g3", 3),
            chain("b", "g1", "g2", 2),
            chain("a", "", "g1", 1),
        ];
        let seq = build_restore_sequence(&chains, None).unwrap();
        assert_eq!(names(&seq), ["a", "b", "c"]);
        assert!(super::super::client::validate_restore_sequence("s", &seq, 3).is_ok());
    }

    #[test]
    fn build_restore_sequence_selects_the_requested_target() {
        let chains = vec![
            chain("c", "g2", "g3", 3),
            chain("b", "g1", "g2", 2),
            chain("a", "", "g1", 1),
        ];
        let seq = build_restore_sequence(&chains, Some("b")).unwrap();
        assert_eq!(names(&seq), ["a", "b"]);
        let seq = build_restore_sequence(&chains, Some("a")).unwrap();
        assert_eq!(names(&seq), ["a"]);
        let err = build_restore_sequence(&chains, Some("zz")).unwrap_err();
        assert_eq!(err, "chain zz is not stored here");
    }

    #[test]
    fn build_restore_sequence_refuses_a_missing_base() {
        let chains = vec![chain("c", "g2", "g3", 3), chain("a", "", "g1", 1)];
        let err = build_restore_sequence(&chains, None).unwrap_err();
        assert!(
            err.contains("needs the chain that produced GUID g2"),
            "{err}"
        );
    }

    #[test]
    fn build_restore_sequence_refuses_a_loop() {
        let chains = vec![chain("x", "g2", "g1", 2), chain("y", "g1", "g2", 1)];
        let err = build_restore_sequence(&chains, None).unwrap_err();
        assert!(err.contains("contains a loop"), "{err}");
    }

    #[test]
    fn build_restore_sequence_refuses_an_empty_listing() {
        let err = build_restore_sequence(&[], None).unwrap_err();
        assert_eq!(err, "no backup is stored here");
    }

    #[test]
    fn the_attempt_generation_is_strictly_greater_than_both_inputs() {
        assert_eq!(next_attempt_generation(0, 0).unwrap(), 1);
        assert_eq!(next_attempt_generation(3, 7).unwrap(), 8);
        assert_eq!(next_attempt_generation(9, 2).unwrap(), 10);
        assert!(next_attempt_generation(u16::MAX, 0).is_err());
    }

    /// An in-memory backend: proves the core is transport independent.
    #[derive(Default)]
    struct MemStore {
        chunks: Mutex<HashMap<(String, i64), Vec<u8>>>,
        manifests: Mutex<HashMap<String, Manifest>>,
        sequence: Mutex<u64>,
        puts: Mutex<usize>,
    }

    impl SequenceAnchor for MemStore {
        fn last_published_sequence(&self, _source: &str) -> Result<u64, String> {
            Ok(*self.sequence.lock().unwrap())
        }
        fn record_published_sequence(&self, _source: &str, sequence: u64) -> Result<(), String> {
            *self.sequence.lock().unwrap() = sequence;
            Ok(())
        }
    }

    impl ChunkStore for MemStore {
        async fn stored_chunks(
            &self,
            _source: &str,
            chain: &str,
        ) -> Result<(HashMap<i64, StoredChunk>, bool), String> {
            let chunks = self.chunks.lock().unwrap();
            let mut out = HashMap::new();
            for ((c, index), sealed) in chunks.iter() {
                if c == chain {
                    let generation = u16::from_be_bytes([sealed[14], sealed[15]]);
                    out.insert(
                        *index,
                        StoredChunk {
                            digest: digest_of(sealed),
                            generation,
                        },
                    );
                }
            }
            let published = self.manifests.lock().unwrap().contains_key(chain);
            Ok((out, published))
        }
        async fn put_chunk(
            &self,
            _source: &str,
            chain: &str,
            index: i64,
            sealed: &[u8],
        ) -> Result<(), String> {
            *self.puts.lock().unwrap() += 1;
            self.chunks
                .lock()
                .unwrap()
                .insert((chain.to_string(), index), sealed.to_vec());
            Ok(())
        }
        async fn publish_manifest(
            &self,
            _source: &str,
            manifest: &Manifest,
            _payload: &[u8],
        ) -> Result<(), String> {
            self.manifests
                .lock()
                .unwrap()
                .insert(manifest.chain.clone(), manifest.clone());
            Ok(())
        }
        async fn prune(&self, _source: &str, _keep: usize) -> Result<usize, String> {
            Ok(0)
        }
    }

    impl ChunkSource for MemStore {
        async fn fetch_manifest(&self, _source: &str, chain: &str) -> Result<Manifest, String> {
            self.manifests
                .lock()
                .unwrap()
                .get(chain)
                .cloned()
                .ok_or_else(|| "no manifest".to_string())
        }
        async fn fetch_chunk(
            &self,
            _source: &str,
            chain: &str,
            index: i64,
        ) -> Result<Vec<u8>, String> {
            self.chunks
                .lock()
                .unwrap()
                .get(&(chain.to_string(), index))
                .cloned()
                .ok_or_else(|| format!("chunk {index} missing"))
        }
    }

    fn identity() -> Identity {
        super::super::keys::new_identity("sender").unwrap()
    }

    async fn push(
        store: &MemStore,
        id: &Identity,
        data: &[u8],
        state: ChainState,
        policy: &PushPolicy<'_>,
    ) -> (PushResult, Option<String>) {
        let mut reader: &[u8] = data;
        push_with(
            store,
            id,
            PushOptions {
                source: "naslos-test/data",
                kind: "zfs-send",
                reader: &mut reader,
                state: Some(state),
                from_snapshot: "",
                to_snapshot: "snap",
                from_guid: "",
                to_guid: "g1",
                prune_keep: 0,
                progress: None,
                before_publish: None,
                save_state: &|_| Ok(()),
            },
            policy,
        )
        .await
    }

    #[tokio::test]
    async fn push_with_and_restore_chain_round_trip_through_any_store() {
        let store = MemStore::default();
        let id = identity();
        let data: Vec<u8> = (0..(CHUNK_PLAIN_SIZE * 2 + 17))
            .map(|i| (i % 251) as u8)
            .collect();
        let state = ChainState::new("naslos-test/data", "zfs-send").unwrap();
        let (result, err) = push(&store, &id, &data, state.clone(), &PushPolicy::buddy()).await;
        assert!(err.is_none(), "{err:?}");
        assert_eq!(result.chunks, 3);
        assert_eq!(result.uploaded, 3);
        assert_eq!(store.last_published_sequence("").unwrap(), 1);

        // A second push of the same bytes re-seals at the stored generation,
        // skips every chunk and publishes the next sequence.
        let (again, err) = push(&store, &id, &data, state.clone(), &PushPolicy::buddy()).await;
        assert!(err.is_none(), "{err:?}");
        assert_eq!(again.uploaded, 0);
        assert_eq!(again.skipped, 3);
        assert_eq!(store.last_published_sequence("").unwrap(), 2);

        let mut out = Vec::new();
        let (restored, err) = restore_chain(
            &store,
            &id,
            RestoreOptions {
                source: "naslos-test/data",
                chain: &state.chain,
                out: &mut out,
                progress: None,
                skip_signature_check: false,
                expected_sequence: Some(2),
                expected_from_guid: "",
                expected_to_guid: "g1",
                expected_kind: "zfs-send",
                expected_from_snapshot: "",
                expected_to_snapshot: "snap",
            },
            &RestorePolicy::buddy(),
        )
        .await;
        assert!(err.is_none(), "{err:?}");
        assert_eq!(restored.chunks, 3);
        assert_eq!(out, data);
    }

    #[tokio::test]
    async fn an_unpublished_resume_skips_identical_chunks_and_reseals_only_the_tail() {
        let store = MemStore::default();
        let id = identity();
        let full: Vec<u8> = (0..(CHUNK_PLAIN_SIZE * 2)).map(|i| (i % 7) as u8).collect();
        let state = ChainState::new("naslos-test/data", "zfs-send").unwrap();
        // An interrupted first attempt: two chunks stored, the second only
        // half of what the full stream will produce, and no manifest because
        // signing is refused before publication.
        let refuse = |_: &mut Manifest, _: &Identity| Err("interrupted".to_string());
        let policy = PushPolicy {
            sign_manifest: &refuse,
            ..PushPolicy::buddy()
        };
        let truncated = &full[..CHUNK_PLAIN_SIZE + CHUNK_PLAIN_SIZE / 2];
        let (first, err) = push(&store, &id, truncated, state.clone(), &policy).await;
        assert_eq!(err.as_deref(), Some("interrupted"));
        assert_eq!(first.uploaded, 2);
        assert!(store.manifests.lock().unwrap().is_empty());
        let first_generation = first.state.unwrap().generation;

        let (second, err) = push(
            &store,
            &id,
            &full,
            ChainState {
                generation: first_generation,
                ..state.clone()
            },
            &PushPolicy::buddy(),
        )
        .await;
        assert!(err.is_none(), "{err:?}");
        assert_eq!(second.skipped, 1, "the identical head chunk is skipped");
        assert_eq!(second.uploaded, 1, "only the changed tail is re-sealed");
        let manifest = second.manifest.unwrap();
        assert!(manifest.chunks[1].generation > first_generation);
        assert_eq!(manifest.chunks[0].generation, first_generation);
    }

    #[tokio::test]
    async fn a_non_tail_mismatch_is_refused() {
        let store = MemStore::default();
        let id = identity();
        let state = ChainState::new("naslos-test/data", "zfs-send").unwrap();
        let a: Vec<u8> = vec![1; CHUNK_PLAIN_SIZE * 2];
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let check = move || {
            if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= 2 {
                Err("cancelled".to_string())
            } else {
                Ok(())
            }
        };
        let policy = PushPolicy {
            check_cancelled: &check,
            ..PushPolicy::buddy()
        };
        let mut a_more = a.clone();
        a_more.push(9);
        let (first, _) = push(&store, &id, &a_more, state.clone(), &policy).await;
        let b: Vec<u8> = vec![2; CHUNK_PLAIN_SIZE * 2];
        let (_, err) = push(
            &store,
            &id,
            &b,
            ChainState {
                generation: first.state.unwrap().generation,
                ..state
            },
            &PushPolicy::buddy(),
        )
        .await;
        let err = err.expect("a changed non-tail chunk must be refused");
        assert!(
            err.contains("already holds different bytes for chunk 0"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn the_manifest_cap_and_signer_are_caller_supplied() {
        let store = MemStore::default();
        let id = identity();
        let state = ChainState::new("naslos-test/data", "zfs-send").unwrap();
        let policy = PushPolicy {
            max_manifest_bytes: 16,
            ..PushPolicy::buddy()
        };
        let (_, err) = push(&store, &id, b"hello", state.clone(), &policy).await;
        assert!(err.unwrap().contains("over the 16-byte limit"));
        assert!(store.manifests.lock().unwrap().is_empty());

        let refuse = |_: &mut Manifest, _: &Identity| Err("no signer".to_string());
        let policy = PushPolicy {
            sign_manifest: &refuse,
            ..PushPolicy::buddy()
        };
        let (_, err) = push(&store, &id, b"hello", state, &policy).await;
        assert_eq!(err.as_deref(), Some("no signer"));
    }
}
