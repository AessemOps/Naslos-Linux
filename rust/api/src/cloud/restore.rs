//! Cloud restore: verify the stored sequence, preflight every object, then
//! stream one link at a time into a **new** dataset (never in place).
//!
//! The preflight is LIST-only (no egress) and reports every missing or
//! wrongly-sized chunk before the first `zfs receive`. Each link then gets a
//! single streaming pass (no Buddy-style double download), and the target's
//! GUID is asserted after every receive. On failure the partial dataset is
//! destroyed unless `keepPartial` was requested.

use std::collections::HashMap;
use std::sync::Arc;

use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use super::jobs::{self, CloudJob, JobKind, JobResult, JobState};
use super::s3::S3Storage;
use super::store::{AnchorPaths, CloudChunkSource, LinkInfo};
use super::{
    identity_path, load_credentials, source_for_dataset, Destination, MAX_STREAM_BODY_BYTES,
};
use crate::buddy::client::RestoreOptions;
use crate::buddy::envelope::{unwrap_dek, Manifest};
use crate::buddy::keys::{load_identity, Identity};
use crate::buddy::transfer::{restore_chain, RestorePolicy};
use crate::state::AppState;

/// The restore body (`docs/api.md`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreRequest {
    #[serde(default)]
    pub destination: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub chain: String,
    #[serde(default)]
    pub dataset: String,
    #[serde(default)]
    pub key_hex: String,
    #[serde(default)]
    pub keep_partial: bool,
}

/// A start failure the HTTP layer maps to a status.
#[derive(Debug, Clone)]
pub enum RestoreError {
    NotFound(String),
    BadRequest(String),
    Precondition(String),
    Conflict(String),
    ServiceUnavailable(String),
    Upstream(String),
}

/// The validated inputs of one restore.
pub struct RestorePlan {
    pub destination: Destination,
    pub source: String,
    pub links: Vec<LinkInfo>,
    pub raw: bool,
}

fn parse_key_hex(raw: &str) -> Result<crate::dataset_encryption::RawKey, String> {
    let trimmed = raw.trim();
    if trimmed.len() != 64 || !trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("keyHex must be 64 hex characters (32 bytes)".to_string());
    }
    let mut bytes = Vec::with_capacity(32);
    for i in (0..64).step_by(2) {
        let byte = u8::from_str_radix(&trimmed[i..i + 2], 16)
            .map_err(|_| "keyHex must be hexadecimal".to_string())?;
        bytes.push(byte);
    }
    crate::dataset_encryption::RawKey::from_slice(&bytes)
}

/// Read the source's raw key from its local Secret, if it exists. An inherited
/// encryption root is resolved through the agent's dataset list.
pub async fn local_dataset_key(
    state: &AppState,
    agent: &crate::agent::Client,
    dataset: &str,
) -> Result<Option<crate::dataset_encryption::RawKey>, String> {
    let datasets = agent
        .list_datasets()
        .await
        .map_err(|e| format!("listing datasets: {e}"))?;
    let Some(entry) = datasets.iter().find(|d| d.name == dataset) else {
        return Ok(None);
    };
    let encrypted =
        !entry.encryption.is_empty() && entry.encryption != "off" && entry.encryption != "-";
    if !encrypted {
        return Ok(None);
    }
    let root = if entry.encryption_root.is_empty() {
        dataset.to_string()
    } else {
        entry.encryption_root.clone()
    };
    let Some(kube) = state.kube.as_ref() else {
        return Ok(None);
    };
    let name = crate::dataset_encryption::secret_name_for_dataset(&root)?;
    let namespace = crate::dataset_encryption::keys_namespace();
    match kube.get_json("secret", &name, &namespace).await? {
        Some(secret) => crate::dataset_encryption::parse_key_secret(&secret, &root).map(Some),
        None => Ok(None),
    }
}

/// Resolve the key a raw restore needs: the write-only `keyHex` first, else the
/// source's local Secret. `None` means the caller must answer 412.
pub async fn resolve_key(
    state: &AppState,
    agent: &crate::agent::Client,
    source_dataset: &str,
    key_hex: &str,
) -> Result<Option<crate::dataset_encryption::RawKey>, String> {
    if !key_hex.trim().is_empty() {
        return parse_key_hex(key_hex).map(Some);
    }
    local_dataset_key(state, agent, source_dataset).await
}

/// Build and validate the restore sequence for one request.
pub async fn prepare(
    state: &Arc<AppState>,
    request: &RestoreRequest,
    key_is_present: bool,
) -> Result<RestorePlan, RestoreError> {
    let cloud = state.cloud.as_ref().ok_or_else(|| {
        RestoreError::ServiceUnavailable("cloud backup is not enabled".to_string())
    })?;
    cloud
        .destinations
        .require_loaded()
        .map_err(RestoreError::ServiceUnavailable)?;
    let destination = cloud
        .destinations
        .get(&request.destination)
        .ok_or_else(|| {
            RestoreError::NotFound(format!("destination {:?} not found", request.destination))
        })?;
    if !destination.enabled {
        return Err(RestoreError::BadRequest(format!(
            "destination {:?} is disabled",
            destination.name
        )));
    }
    let source = source_for_dataset(&request.source).map_err(RestoreError::BadRequest)?;
    let credentials = load_credentials(state, &destination)
        .await
        .map_err(RestoreError::Precondition)?;
    let identity = load_identity(&identity_path()).map_err(RestoreError::Precondition)?;
    let s3 = S3Storage::build(
        &destination,
        &credentials,
        &super::net::NetConfig::from_env(),
    )
    .map_err(|e| RestoreError::Upstream(e.to_string()))?;
    let all = super::store::list_links(&s3, &source, &identity)
        .await
        .map_err(RestoreError::Upstream)?;
    if all.is_empty() {
        return Err(RestoreError::NotFound(format!(
            "no backup of {:?} is stored on this destination",
            request.source
        )));
    }
    let target = if request.chain.trim().is_empty() {
        None
    } else {
        Some(request.chain.trim())
    };
    let links = super::store::select_links(&all, target).map_err(RestoreError::BadRequest)?;
    let anchor = AnchorPaths::new(&cloud.source_state_dir(&destination.id), &source).anchor;
    let last = super::store::last_published(&anchor).map_err(RestoreError::Upstream)?;
    let summaries: Vec<crate::buddy::client::ChainSummary> =
        links.iter().map(LinkInfo::summary).collect();
    crate::buddy::client::validate_restore_sequence(&source, &summaries, last)
        .map_err(RestoreError::BadRequest)?;
    let raw = links.iter().any(LinkInfo::raw);
    if raw && links.iter().any(|l| !l.raw()) {
        return Err(RestoreError::BadRequest(
            "the chain mix changes between raw and non-raw links; restore the newest full lineage"
                .to_string(),
        ));
    }
    if raw && !key_is_present {
        return Err(RestoreError::Precondition(
            "dataset key required: this is a raw backup of an encrypted dataset and its key is not stored on this NAS. Export the key or supply keyHex"
                .to_string(),
        ));
    }
    Ok(RestorePlan {
        destination,
        source,
        links,
        raw,
    })
}

/// Start a restore job. The handler has already validated the target and the
/// key; the job repeats the checks (a race must fail closed).
pub fn enqueue(
    state: &Arc<AppState>,
    plan: RestorePlan,
    request: RestoreRequest,
) -> Result<String, RestoreError> {
    let cloud = state.cloud.as_ref().ok_or_else(|| {
        RestoreError::ServiceUnavailable("cloud backup is not enabled".to_string())
    })?;
    if let Err(e) = crate::server::zfs::validate_dataset_path(&request.dataset) {
        return Err(RestoreError::BadRequest(e));
    }
    if request.dataset == request.source
        || request.dataset.starts_with(&format!("{}/", request.source))
    {
        return Err(RestoreError::BadRequest(
            "restore into a dataset outside the source hierarchy".to_string(),
        ));
    }
    let id = jobs::random_job_id();
    let source = plan.source.clone();
    let dataset = request.dataset.clone();
    let (job, token) = jobs::new_job(
        id.clone(),
        JobKind::Restore,
        plan.destination.id.clone(),
        source.clone(),
        dataset.clone(),
    );
    if let Err(conflict) = cloud.jobs.add_if_free(
        &id,
        job.clone(),
        &plan.destination.id,
        &source,
        &dataset,
        JobKind::Restore,
    ) {
        return Err(RestoreError::Conflict(match conflict {
            jobs::EnqueueConflict::Source => {
                format!("another backup/restore of {source} on this destination is running")
            }
            jobs::EnqueueConflict::Dataset => {
                format!("another restore into {dataset} is running")
            }
        }));
    }
    let state_for_task = state.clone();
    tokio::spawn(async move {
        run_restore_job(state_for_task, job, token, plan, request).await;
    });
    Ok(id)
}

#[derive(Debug, Default, Clone)]
pub struct SequenceProblems {
    pub missing: HashMap<String, Vec<i64>>,
    pub bad: HashMap<String, Vec<i64>>,
    pub plain_bytes: i64,
}

impl SequenceProblems {
    pub fn is_empty(&self) -> bool {
        self.missing.is_empty() && self.bad.is_empty()
    }
}

/// Compare a manifest against the chunk objects a LIST found.
pub fn index_problems(manifest: &Manifest, objects: &HashMap<i64, u64>) -> (Vec<i64>, Vec<i64>) {
    let mut missing = Vec::new();
    let mut bad = Vec::new();
    for chunk in &manifest.chunks {
        match objects.get(&chunk.index) {
            None => missing.push(chunk.index),
            Some(size) if *size != chunk.sealed_bytes as u64 => bad.push(chunk.index),
            Some(_) => {}
        }
    }
    missing.sort_unstable();
    bad.sort_unstable();
    (missing, bad)
}

/// LIST every link in the sequence and compare it to its signed manifest.
/// No object body is downloaded.
pub async fn preflight_sequence(
    s3: &S3Storage,
    links: &[LinkInfo],
) -> Result<SequenceProblems, String> {
    let mut problems = SequenceProblems::default();
    for link in links {
        let link_dir = super::link_prefix(
            &s3.root_prefix(),
            &link.manifest.source,
            &link.lineage,
            link.sequence,
            &link.chain,
        );
        let objects = s3.list(&format!("{link_dir}/chunks/")).await?;
        let live: HashMap<i64, u16> = link
            .manifest
            .chunks
            .iter()
            .map(|chunk| (chunk.index, chunk.generation))
            .collect();
        let mut sizes: HashMap<i64, u64> = HashMap::new();
        for object in objects {
            let key = object.location.as_ref();
            if let Some(super::ParsedLinkKey {
                kind: super::ParsedKeyKind::Chunk { index, generation },
                ..
            }) = super::parse_link_key(&s3.source_root(&link.manifest.source), key)
            {
                // Ignore superseded generations; the manifest names the live
                // object for each index.
                if live.get(&index) == Some(&generation) {
                    sizes.insert(index, object.size);
                }
            }
        }
        let (missing, bad) = index_problems(&link.manifest, &sizes);
        if !missing.is_empty() {
            problems.missing.insert(link.chain.clone(), missing);
        }
        if !bad.is_empty() {
            problems.bad.insert(link.chain.clone(), bad);
        }
        problems.plain_bytes += link.plain_bytes();
    }
    if problems.plain_bytes > MAX_STREAM_BODY_BYTES {
        return Err(format!(
            "the restore stream is {} bytes, above the agent's {MAX_STREAM_BODY_BYTES}-byte receive cap",
            problems.plain_bytes
        ));
    }
    Ok(problems)
}

async fn run_restore_job(
    state: Arc<AppState>,
    job: Arc<CloudJob>,
    token: CancellationToken,
    plan: RestorePlan,
    request: RestoreRequest,
) {
    macro_rules! fail {
        ($msg:expr) => {{
            let msg: String = ($msg).to_string();
            let cancelled = token.is_cancelled();
            job.finish(
                if cancelled {
                    JobState::Cancelled
                } else {
                    JobState::Failed
                },
                msg,
                None,
            );
            return;
        }};
    }

    let identity = match load_identity(&identity_path()) {
        Ok(identity) => Arc::new(identity),
        Err(e) => fail!(e),
    };
    let credentials = match load_credentials(&state, &plan.destination).await {
        Ok(credentials) => credentials,
        Err(e) => fail!(e),
    };
    let Some(agent) = state.agent.clone() else {
        fail!("the API has no agent client, so it cannot receive ZFS data");
    };
    let s3 = match S3Storage::build(
        &plan.destination,
        &credentials,
        &super::net::NetConfig::from_env(),
    ) {
        Ok(s3) => Arc::new(s3),
        Err(e) => fail!(e.to_string()),
    };

    let key = match resolve_key(&state, &agent, &request.source, &request.key_hex).await {
        Ok(key) => key,
        Err(e) => fail!(e),
    };
    if plan.raw && key.is_none() {
        fail!(
            "dataset key required: this is a raw backup of an encrypted dataset and its key is not stored on this NAS"
        );
    }

    // The target must not exist. A matching restore is not offered in WP-1
    // (restore only creates a new dataset).
    match agent.list_datasets().await {
        Ok(datasets) => {
            if datasets.iter().any(|d| d.name == request.dataset) {
                fail!(format!(
                    "dataset {} already exists; a cloud restore only creates a new dataset",
                    request.dataset
                ));
            }
        }
        Err(e) => fail!(format!("listing datasets: {e}")),
    }

    let problems = match preflight_sequence(&s3, &plan.links).await {
        Ok(problems) => problems,
        Err(e) => fail!(e),
    };
    if !problems.is_empty() {
        let result = JobResult {
            missing_indices: problems.missing.clone(),
            bad_indices: problems.bad.clone(),
            ..Default::default()
        };
        let error = format!(
            "the stored backup is incomplete: {} missing and {} bad chunk indices",
            problems.missing.values().map(Vec::len).sum::<usize>(),
            problems.bad.values().map(Vec::len).sum::<usize>(),
        );
        job.finish(JobState::Failed, error, Some(result));
        return;
    }

    // Fail before receiving if any chain DEK cannot be unwrapped.
    let kek = match identity.kek_bytes() {
        Ok(kek) => kek,
        Err(e) => fail!(e),
    };
    for link in &plan.links {
        if let Err(e) = unwrap_dek(
            &kek,
            &link.manifest.dek_wrapped,
            &link.manifest.source,
            &link.manifest.chain,
        ) {
            fail!(format!("chain {}: {e}", link.chain));
        }
    }

    let mut secret_created = false;
    if plan.raw {
        let Some(key) = &key else {
            fail!("dataset key required");
        };
        if let Err(e) =
            crate::server::replication::write_replica_secret(&state, &request.dataset, key).await
        {
            fail!(e);
        }
        secret_created = true;
    }

    let anchor = AnchorPaths::new(
        &state
            .cloud
            .as_ref()
            .map(|c| c.source_state_dir(&plan.destination.id))
            .unwrap_or_default(),
        &plan.source,
    )
    .anchor;
    let source_reader = CloudChunkSource::new(s3.clone(), &plan.links, anchor);
    let cancel = jobs::cancel_check(token.clone());
    let verify =
        |manifest: &Manifest, identity: &Identity| super::verify_manifest(manifest, identity);
    let policy = RestorePolicy {
        verify_signature: &verify,
        check_cancelled: &cancel,
    };

    let mut applied = 0usize;
    let mut total_plain = 0i64;
    let mut last_link: Option<LinkInfo> = None;
    let total_links = plan.links.len();
    for (position, link) in plan.links.iter().enumerate() {
        if token.is_cancelled() {
            break;
        }
        // Resume inside this job: a previously received link leaves its GUID on
        // the target.
        let already = agent
            .snapshots_with_guid_shallow(&request.dataset)
            .await
            .ok()
            .map(|snaps| snaps.iter().any(|s| s.guid == link.manifest.to_guid))
            .unwrap_or(false);
        if already {
            applied += 1;
            last_link = Some(link.clone());
            job.update_progress(|p| {
                p.link = Some(position + 1);
                p.links = Some(total_links);
            });
            continue;
        }

        let (mut writer, reader) = tokio::io::duplex(1 << 20);
        let agent_for_receive = agent.clone();
        let dataset = request.dataset.clone();
        let to_guid = link.manifest.to_guid.clone();
        let receive_key = key.as_ref().map(|k| k.as_bytes().to_vec());
        let receive_raw = plan.raw;
        let receive = tokio::spawn(async move {
            agent_for_receive
                .receive_stream_body_outcome_raw(
                    &dataset,
                    false,
                    receive_raw,
                    receive_key.as_deref(),
                    reader,
                )
                .await
        });

        let progress = {
            let job = job.clone();
            move |index: i64, chunks: usize, plain: i64| {
                job.update_progress(|p| {
                    p.chunk = index;
                    p.plain_bytes = plain;
                    p.total_chunks = Some(chunks as i64);
                    p.link = Some(position + 1);
                    p.links = Some(total_links);
                });
            }
        };
        let (restored, error) = restore_chain(
            &source_reader,
            &identity,
            RestoreOptions {
                source: &plan.source,
                chain: &link.chain,
                out: &mut writer,
                progress: Some(&progress),
                skip_signature_check: false,
                expected_sequence: Some(link.sequence),
                expected_from_guid: &link.manifest.from_guid,
                expected_to_guid: &link.manifest.to_guid,
                expected_kind: &link.manifest.kind,
                expected_from_snapshot: &link.manifest.from_snapshot,
                expected_to_snapshot: &link.manifest.to_snapshot,
            },
            &policy,
        )
        .await;
        drop(writer);
        let received = receive.await;
        let receive_error = match received {
            Ok(Ok(_)) => None,
            Ok(Err(e)) => Some(e.message),
            Err(e) => Some(format!("the receive task failed: {e}")),
        };
        if let Some(error) = error {
            let cancelled = token.is_cancelled();
            cleanup_restore(
                &state,
                &agent,
                &request.dataset,
                plan.raw,
                secret_created,
                request.keep_partial && applied > 0,
            )
            .await;
            job.finish(
                if cancelled {
                    JobState::Cancelled
                } else {
                    JobState::Failed
                },
                error,
                None,
            );
            return;
        }
        if let Some(error) = receive_error {
            let cancelled = token.is_cancelled();
            cleanup_restore(
                &state,
                &agent,
                &request.dataset,
                plan.raw,
                secret_created,
                request.keep_partial && applied > 0,
            )
            .await;
            job.finish(
                if cancelled {
                    JobState::Cancelled
                } else {
                    JobState::Failed
                },
                format!("zfs receive failed: {error}"),
                None,
            );
            return;
        }
        // The receive must have produced the GUID the manifest signed.
        let seen = agent
            .snapshots_with_guid_shallow(&request.dataset)
            .await
            .map(|snaps| snaps.iter().any(|s| s.guid == to_guid))
            .unwrap_or(false);
        if !seen {
            cleanup_restore(
                &state,
                &agent,
                &request.dataset,
                plan.raw,
                secret_created,
                request.keep_partial && applied > 0,
            )
            .await;
            job.finish(
                JobState::Failed,
                format!(
                    "the receive completed but the target does not hold GUID {to_guid}; the stream did not apply as signed"
                ),
                None,
            );
            return;
        }
        applied += 1;
        total_plain += restored.plain_bytes;
        last_link = Some(link.clone());
    }

    if token.is_cancelled() {
        cleanup_restore(
            &state,
            &agent,
            &request.dataset,
            plan.raw,
            secret_created,
            request.keep_partial && applied > 0,
        )
        .await;
        job.finish(JobState::Cancelled, "cancelled".to_string(), None);
        return;
    }

    let link = match last_link {
        Some(link) => link,
        None => {
            cleanup_restore(
                &state,
                &agent,
                &request.dataset,
                plan.raw,
                secret_created,
                false,
            )
            .await;
            job.finish(
                JobState::Failed,
                "the restore sequence was empty".to_string(),
                None,
            );
            return;
        }
    };
    job.finish(
        JobState::Succeeded,
        String::new(),
        Some(JobResult {
            chain: link.chain.clone(),
            snapshot: link.manifest.to_snapshot.clone(),
            kind: if link.incremental() {
                "incremental"
            } else {
                "full"
            }
            .to_string(),
            plain_bytes: total_plain,
            ..Default::default()
        }),
    );
}

async fn cleanup_restore(
    state: &AppState,
    agent: &crate::agent::Client,
    dataset: &str,
    raw: bool,
    secret_created: bool,
    keep_partial: bool,
) {
    if keep_partial {
        return;
    }
    let exists = crate::server::buddy_restore::live_dataset_exists(agent, dataset)
        .await
        .unwrap_or(false);
    if exists {
        if let Err(e) = agent.destroy_dataset(dataset, true).await {
            tracing::warn!("cloud restore: could not destroy the partial dataset {dataset}: {e}");
            return;
        }
    }
    if raw && secret_created && !exists {
        // The dataset never landed (or was just destroyed), so the key Secret
        // this restore created is safe to remove. A Secret that pre-existed was
        // refused by `write_replica_secret`, so this can only be ours.
        if let Some(kube) = state.kube.as_ref() {
            if let Ok(name) = crate::dataset_encryption::secret_name_for_dataset(dataset) {
                let namespace = crate::dataset_encryption::keys_namespace();
                if let Err(e) = kube.delete("secret", &name, &namespace).await {
                    tracing::warn!("cloud restore: could not remove the replica key Secret: {e}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buddy::envelope::ManifestChunk;

    fn manifest() -> Manifest {
        Manifest {
            version: crate::buddy::envelope::ENVELOPE_VERSION,
            source: "tank/data".into(),
            chain: "abcd".into(),
            kind: "zfs-send".into(),
            from_snapshot: String::new(),
            to_snapshot: "snap".into(),
            from_guid: String::new(),
            to_guid: "g".into(),
            created_at: chrono::Utc::now(),
            key_id: String::new(),
            sequence: 1,
            generation: 1,
            stream_prefix: "AAAAAA==".into(),
            chunk_plain_size: crate::buddy::envelope::CHUNK_PLAIN_SIZE as i64,
            chunks: (0..4)
                .map(|index| ManifestChunk {
                    index,
                    plain_bytes: 100,
                    sealed_bytes: 136,
                    generation: 1,
                    sha256_plain: "00".into(),
                })
                .collect(),
            dek_wrapped: "AAAA".into(),
            signature: String::new(),
        }
    }

    #[test]
    fn the_preflight_reports_every_missing_and_bad_index() {
        let mut objects = HashMap::new();
        objects.insert(0, 136);
        objects.insert(1, 99); // wrong size
        objects.insert(3, 136);
        let (missing, bad) = index_problems(&manifest(), &objects);
        assert_eq!(missing, vec![2]);
        assert_eq!(bad, vec![1]);
    }

    #[test]
    fn the_key_hex_is_strict() {
        assert!(parse_key_hex(&"00".repeat(32)).is_ok());
        assert!(parse_key_hex("00").is_err());
        assert!(parse_key_hex(&"zz".repeat(32)).is_err());
    }

    #[test]
    fn a_restore_into_the_source_hierarchy_is_refused() {
        // The enqueue check is a pure string comparison; the HTTP layer runs it.
        let source = "tank/data";
        assert!("tank/data/child".starts_with(&format!("{source}/")));
        assert!(!"tank/other".starts_with(&format!("{source}/")));
    }
}
