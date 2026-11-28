//! Cloud send: snapshot, stream `zfs send`, seal/upload, publish last, retain.
//!
//! A send creates one *link* (Buddy chain: fresh DEK and prefix) as a full or a
//! GUID-based incremental inside a *lineage*. The resume state records the
//! snapshot pair and GUIDs, so an interrupted attempt continues the same link
//! only when the stream is still reproducible; otherwise a fresh link (full if
//! the base is gone) starts and retention sweeps the orphans later.

use std::sync::Arc;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::jobs::{self, CloudJob, JobKind, JobResult, JobState};
use super::s3::S3Storage;
use super::store::{self, AnchorPaths, CloudChunkStore, LinkInfo};
use super::{
    identity_path, load_credentials, snapshot_name, snapshot_prefix, source_for_dataset,
    Destination, MAX_MANIFEST_BYTES, MAX_STREAM_BODY_BYTES,
};
use crate::buddy::client::ChainState;
use crate::buddy::client::{PushProgress, PushResult};
use crate::buddy::keys::load_identity;
use crate::buddy::transfer::{push_with, PushPolicy};
use crate::state::AppState;

const DEFAULT_KEEP_LINEAGES: usize = 2;
const DEFAULT_FULL_EVERY: usize = 30;

/// The persisted resume state of an interrupted send. It holds the chain's DEK
/// through [`ChainState`], so it is transient and never a DR artifact.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SendResume {
    pub destination: String,
    pub source: String,
    pub dataset: String,
    #[serde(default)]
    pub raw: bool,
    #[serde(default, rename = "fromSnapshot")]
    pub from_snapshot: String,
    #[serde(default, rename = "fromGUID")]
    pub from_guid: String,
    #[serde(default, rename = "toSnapshot")]
    pub to_snapshot: String,
    #[serde(default, rename = "toGUID")]
    pub to_guid: String,
    #[serde(default)]
    pub lineage: String,
    #[serde(default)]
    pub chain: ChainState,
}

/// The parameters of one send, shared by the manual endpoint and schedules.
#[derive(Debug, Clone)]
pub struct SendParams {
    pub dataset: String,
    pub destination: String,
    pub keep_lineages: usize,
    pub force_full: bool,
    pub schedule_id: String,
}

/// A start failure the HTTP layer maps to a status.
#[derive(Debug, Clone)]
pub enum StartError {
    NotFound(String),
    BadRequest(String),
    ServiceUnavailable(String),
    Conflict(String),
}

/// Validate a `keepLineages` value (1..=100).
pub fn valid_keep_lineages(value: usize) -> Result<usize, String> {
    if !(1..=100).contains(&value) {
        return Err("keepLineages must be between 1 and 100".to_string());
    }
    Ok(value)
}

fn full_every() -> usize {
    std::env::var("CLOUD_FULL_EVERY")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_FULL_EVERY)
}

/// A conservative manifest-size preflight: 200 B per chunk plus framing.
/// Returns the estimated chunks and manifest bytes for a stream of `bytes`.
pub fn manifest_estimate(bytes: i64) -> (i64, i64) {
    let chunks = (bytes.max(0) / crate::buddy::envelope::CHUNK_PLAIN_SIZE as i64) + 1;
    (chunks, chunks * 200 + 4096)
}

/// Refuse a stream the restore path could never receive or whose manifest
/// would exceed the destination cap.
pub fn preflight_estimate(dataset: &str, bytes: i64) -> Result<(), String> {
    if bytes > MAX_STREAM_BODY_BYTES {
        return Err(format!(
            "refusing to send {dataset}: the stream is estimated at {bytes} bytes, above the agent's {MAX_STREAM_BODY_BYTES}-byte receive cap, so it could not be restored"
        ));
    }
    let (_, manifest) = manifest_estimate(bytes);
    if manifest > MAX_MANIFEST_BYTES as i64 {
        return Err(format!(
            "refusing to send {dataset}: the stream's manifest is estimated at {manifest} bytes, above the {MAX_MANIFEST_BYTES}-byte cap"
        ));
    }
    Ok(())
}

/// Whether a new link must be a full. A base is reused only inside a lineage
/// with fewer than `full_every` links and smaller incrementals than the full.
pub fn must_send_full(links: &[LinkInfo], base: Option<&LinkInfo>, full_every: usize) -> bool {
    let Some(base) = base else {
        return true;
    };
    let lineage: Vec<&LinkInfo> = links.iter().filter(|l| l.lineage == base.lineage).collect();
    let links_in_lineage = lineage.len();
    let full_bytes = lineage
        .iter()
        .find(|l| !l.incremental())
        .map(|l| l.plain_bytes())
        .unwrap_or(0);
    let incremental_bytes: i64 = lineage
        .iter()
        .filter(|l| l.incremental())
        .map(|l| l.plain_bytes())
        .sum();
    links_in_lineage >= full_every || (full_bytes > 0 && incremental_bytes >= full_bytes)
}

/// The newest published link whose `to_guid` is still present locally.
pub fn find_base<'a>(
    links: &'a [LinkInfo],
    local_guids: &std::collections::HashSet<String>,
) -> Option<&'a LinkInfo> {
    links.iter().find(|link| {
        !link.manifest.to_guid.is_empty() && local_guids.contains(&link.manifest.to_guid)
    })
}

/// Start a send job. Validation that needs an HTTP status (identity, creds,
/// destination) is done by the handler; the job re-checks what it needs.
pub fn enqueue_send(state: &Arc<AppState>, params: SendParams) -> Result<String, StartError> {
    let cloud = state
        .cloud
        .as_ref()
        .ok_or_else(|| StartError::ServiceUnavailable("cloud backup is not enabled".to_string()))?;
    cloud
        .destinations
        .require_loaded()
        .map_err(StartError::ServiceUnavailable)?;
    let destination = cloud.destinations.get(&params.destination).ok_or_else(|| {
        StartError::NotFound(format!("destination {:?} not found", params.destination))
    })?;
    if !destination.enabled {
        return Err(StartError::BadRequest(format!(
            "destination {:?} is disabled",
            destination.name
        )));
    }
    let source = source_for_dataset(&params.dataset).map_err(StartError::BadRequest)?;
    let id = jobs::random_job_id();
    let (job, token) = jobs::new_job(
        id.clone(),
        JobKind::Send,
        destination.id.clone(),
        source.clone(),
        params.dataset.clone(),
    );
    if let Err(conflict) = cloud.jobs.add_if_free(
        &id,
        job.clone(),
        &destination.id,
        &source,
        &params.dataset,
        JobKind::Send,
    ) {
        return Err(StartError::Conflict(match conflict {
            jobs::EnqueueConflict::Source => format!(
                "a cloud backup of {} to {} is already running",
                params.dataset, destination.name
            ),
            jobs::EnqueueConflict::Dataset => "another restore is running".to_string(),
        }));
    }
    let state_for_task = state.clone();
    tokio::spawn(async move {
        run_send_job(state_for_task, job, token, params, destination).await;
    });
    Ok(id)
}

async fn run_send_job(
    state: Arc<AppState>,
    job: Arc<CloudJob>,
    token: CancellationToken,
    params: SendParams,
    destination: Destination,
) {
    macro_rules! finish_failed {
        ($msg:expr) => {{
            let msg: String = ($msg).to_string();
            let cancelled = token.is_cancelled();
            job.finish(
                if cancelled {
                    JobState::Cancelled
                } else {
                    JobState::Failed
                },
                msg.clone(),
                None,
            );
            if !params.schedule_id.is_empty() {
                if let Some(cloud) = state.cloud.as_ref() {
                    cloud.schedules.record_result(
                        &params.schedule_id,
                        &destination.id,
                        false,
                        &msg,
                    );
                }
            }
            notify(&state, &job.snapshot(), &destination.name).await;
            record_scheduled_audit(&state, &params, &destination, !cancelled, &msg).await;
            return;
        }};
    }

    let identity = match load_identity(&identity_path()) {
        Ok(identity) => Arc::new(identity),
        Err(e) => finish_failed!(e),
    };
    let credentials = match load_credentials(&state, &destination).await {
        Ok(credentials) => credentials,
        Err(e) => finish_failed!(e),
    };
    let Some(agent) = state.agent.clone() else {
        finish_failed!("the API has no agent client, so it cannot stream ZFS data");
    };
    let s3 = match S3Storage::build(
        &destination,
        &credentials,
        &super::net::NetConfig::from_env(),
    ) {
        Ok(s3) => Arc::new(s3),
        Err(e) => finish_failed!(e.to_string()),
    };

    let source = match source_for_dataset(&params.dataset) {
        Ok(source) => source,
        Err(e) => finish_failed!(e),
    };
    let Some(cloud) = state.cloud.as_ref() else {
        finish_failed!("cloud backup is not enabled");
    };
    let (resume_path, _anchor) = cloud.source_state_paths(&destination.id, &source);
    let ledger_path = ledger_path_for(&cloud.source_state_dir(&destination.id), &source);
    let (mut resume_state, discarded) = resume_disposition(
        load_resume(&resume_path, &destination.id, &source, &params.dataset),
        params.force_full,
    );
    if let Some(resume) = discarded {
        // forceFull is an explicit "start over": the old link cannot be
        // continued, so remove its snapshot and transient state now.
        abandon_link(
            &agent,
            &params.dataset,
            &resume_path,
            &ledger_path,
            &resume.to_snapshot,
        )
        .await;
    }

    // An encrypted source is always sent raw (`-w`): the bucket then holds the
    // ZFS ciphertext and no plaintext, and no dataset key is needed to read it
    // back. An unencrypted source is a plain stream. The manifest kind records
    // which, so restore knows whether a dataset key is required.
    let datasets = match agent.list_datasets().await {
        Ok(datasets) => datasets,
        Err(e) => finish_failed!(format!("listing datasets: {e}")),
    };
    let Some(entry) = datasets
        .iter()
        .find(|dataset| dataset.name == params.dataset)
    else {
        finish_failed!(format!(
            "dataset {:?} is not known to the agent",
            params.dataset
        ));
    };
    let raw = !entry.encryption.is_empty() && entry.encryption != "off" && entry.encryption != "-";
    let kind = if raw { "zfs-send-raw" } else { "zfs-send" };

    // Mount-namespace trap: a dataset mounted only inside a pod captures an
    // empty filesystem. Refuse before snapshotting, like Buddy does.
    let mount_state =
        super::super::server::buddy_send::dataset_mount_state(&agent, &params.dataset).await;
    if let Some((mounted, mountpoint, _)) = &mount_state {
        if !mounted && mountpoint.starts_with('/') {
            finish_failed!(format!(
                "refusing to back up: dataset {} is not mounted on the node (mountpoint {}), so zfs send would capture an empty filesystem; mount it in the host namespace and retry",
                params.dataset, mountpoint
            ));
        }
    }

    // Resume only when both snapshots still exist with the recorded GUIDs,
    // the raw flag matches, and the chain state fits.
    let snapshots = match agent.snapshots_with_guid_shallow(&params.dataset).await {
        Ok(snapshots) => snapshots,
        Err(e) => finish_failed!(format!("listing snapshots of {}: {e}", params.dataset)),
    };
    let snapshot_guid = |name: &str| snapshot_guid_of(&snapshots, name);
    if let Some(resume) = &resume_state {
        let to_guid = snapshot_guid(&resume.to_snapshot);
        let from_name = if resume.from_snapshot.is_empty() {
            None
        } else {
            Some(resume.from_snapshot.as_str())
        };
        let from_guid = if resume.from_snapshot.is_empty() {
            None
        } else {
            snapshot_guid(&resume.from_snapshot)
        };
        let compatible = store::resume_compatible(
            resume,
            Some(resume.to_snapshot.as_str()),
            to_guid.as_deref(),
            from_name,
            from_guid.as_deref(),
            raw,
        ) && store::chain_state_matches(&resume.chain, &resume.source, kind);
        if !compatible {
            let _ = std::fs::remove_file(&resume_path);
            resume_state = None;
        }
    }

    let links = match store::list_links(&s3, &source, &identity).await {
        Ok(links) => links,
        Err(e) => finish_failed!(e),
    };
    let local_guids: std::collections::HashSet<String> =
        snapshots.iter().map(|s| s.guid.clone()).collect();

    // The destination id is not part of the signed material, so re-adding a
    // destination mints a new local anchor path. Seed it from the highest
    // signed sequence in the bucket before choosing the next sequence; never
    // reuse a sequence (that would overwrite a link path on create-only PUT).
    let anchor_paths = AnchorPaths::new(&cloud.source_state_dir(&destination.id), &source);
    let bucket_max = links.iter().map(|link| link.sequence).max().unwrap_or(0);
    if let Err(e) = store::seed_sequence(&anchor_paths.anchor, bucket_max) {
        finish_failed!(e);
    }

    // A crash between publishing the manifest and deleting the resume state
    // must not publish the chain twice. The signed listing is authoritative.
    if let Some(resume) = &resume_state {
        if let Some(link) = links.iter().find(|l| l.chain == resume.chain.chain) {
            let _ = std::fs::remove_file(&resume_path);
            let _ = std::fs::remove_file(&ledger_path);
            prune_local_snapshots(
                &agent,
                &params.dataset,
                &source,
                &destination.id,
                keep_of(&params),
            )
            .await;
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
                    plain_bytes: link.plain_bytes(),
                    pruned: Some(0),
                    chunks: Some(link.manifest.chunks.len() as i64),
                    ..Default::default()
                }),
            );
            if !params.schedule_id.is_empty() {
                if let Some(cloud) = state.cloud.as_ref() {
                    cloud
                        .schedules
                        .record_result(&params.schedule_id, &destination.id, true, "");
                }
            }
            notify(&state, &job.snapshot(), &destination.name).await;
            record_scheduled_audit(&state, &params, &destination, true, "").await;
            return;
        }
    }

    let (lineage, from_snapshot, from_guid) = if let Some(resume) = &resume_state {
        (
            resume.lineage.clone(),
            resume.from_snapshot.clone(),
            resume.from_guid.clone(),
        )
    } else {
        let base = if params.force_full {
            None
        } else {
            find_base(&links, &local_guids)
        };
        let force_full = params.force_full || must_send_full(&links, base, full_every());
        match (base, force_full) {
            (Some(base), false) => (
                base.lineage.clone(),
                base.manifest.to_snapshot.clone(),
                base.manifest.to_guid.clone(),
            ),
            _ => (String::new(), String::new(), String::new()),
        }
    };

    let chain = if let Some(resume) = &resume_state {
        resume.chain.clone()
    } else {
        match ChainState::new(&source, kind) {
            Ok(chain) => chain,
            Err(e) => finish_failed!(format!("creating the chain state: {e}")),
        }
    };
    let lineage = if lineage.is_empty() {
        chain.chain.clone()
    } else {
        lineage
    };

    let snapshot = if let Some(resume) = &resume_state {
        resume.to_snapshot.clone()
    } else {
        snapshot_name(&source, &destination.id, Utc::now())
    };
    let target_guid = if let Some(resume) = &resume_state {
        resume.to_guid.clone()
    } else {
        if let Err(e) = agent.create_snapshot(&params.dataset, &snapshot).await {
            finish_failed!(format!("snapshotting {}: {e}", params.dataset));
        }
        // The pre-create listing cannot contain this snapshot. Re-list after
        // the create, like Buddy does, and fail (rather than treat it as
        // missing) when the listing itself fails.
        let after = match agent.snapshots_with_guid_shallow(&params.dataset).await {
            Ok(after) => after,
            Err(e) => {
                abandon_link(
                    &agent,
                    &params.dataset,
                    &resume_path,
                    &ledger_path,
                    &snapshot,
                )
                .await;
                finish_failed!(format!(
                    "listing snapshots after creating {}@{snapshot}: {e}",
                    params.dataset
                ));
            }
        };
        match snapshot_guid_of(&after, &snapshot) {
            Some(guid) if !guid.is_empty() => guid,
            _ => {
                abandon_link(
                    &agent,
                    &params.dataset,
                    &resume_path,
                    &ledger_path,
                    &snapshot,
                )
                .await;
                finish_failed!(format!(
                    "the agent did not report a GUID for {}@{snapshot}; refusing to publish a chain that cannot be walked",
                    params.dataset
                ));
            }
        }
    };

    // Persist the link before asking the agent to estimate it. A transport
    // error while estimating (timeout, agent restart) is retryable with the
    // same snapshot, so the resume state must already exist.
    let mut resume = resume_state.clone().unwrap_or(SendResume {
        destination: destination.id.clone(),
        source: source.clone(),
        dataset: params.dataset.clone(),
        raw,
        from_snapshot: from_snapshot.clone(),
        from_guid: from_guid.clone(),
        to_snapshot: snapshot.clone(),
        to_guid: target_guid.clone(),
        lineage: lineage.clone(),
        chain: chain.clone(),
    });
    resume.raw = raw;
    resume.from_snapshot = from_snapshot.clone();
    resume.from_guid = from_guid.clone();
    resume.to_snapshot = snapshot.clone();
    resume.to_guid = target_guid.clone();
    resume.lineage = lineage.clone();
    resume.chain = chain.clone();
    if let Err(e) = save_resume(&resume_path, &resume) {
        // Without the state a retry would orphan this snapshot; discard it.
        abandon_link(
            &agent,
            &params.dataset,
            &resume_path,
            &ledger_path,
            &snapshot,
        )
        .await;
        finish_failed!(format!("saving the resume state: {e}"));
    }

    let expected = match agent
        .estimate_send(&crate::agent::SendStreamOptions {
            dataset: params.dataset.clone(),
            to: snapshot.clone(),
            from: from_snapshot.clone(),
            raw,
            recursive: false,
        })
        .await
    {
        Ok(bytes) => bytes,
        Err(e) => {
            // RPC/transport failure: retryable. Keep the resume state (and
            // the chunk ledger) so the next attempt continues this link.
            if preflight_abandons(false) {
                abandon_link(
                    &agent,
                    &params.dataset,
                    &resume_path,
                    &ledger_path,
                    &snapshot,
                )
                .await;
            }
            finish_failed!(format!("estimating the stream: {e}"));
        }
    };
    if let Err(e) = preflight_estimate(&params.dataset, expected) {
        // Deterministic cap rejection: retrying the same link can never work.
        if preflight_abandons(true) {
            abandon_link(
                &agent,
                &params.dataset,
                &resume_path,
                &ledger_path,
                &snapshot,
            )
            .await;
        }
        finish_failed!(e);
    }
    job.update_progress(|p| {
        p.total_chunks = Some(manifest_estimate(expected).0);
    });

    let stream = match agent
        .send_stream(&crate::agent::SendStreamOptions {
            dataset: params.dataset.clone(),
            to: snapshot.clone(),
            from: from_snapshot.clone(),
            raw,
            recursive: false,
        })
        .await
    {
        Ok(stream) => stream,
        Err(e) => finish_failed!(format!("starting zfs send: {e}")),
    };
    let mut reader = tokio_util::io::StreamReader::new(stream);

    let keep = if params.keep_lineages == 0 {
        DEFAULT_KEEP_LINEAGES
    } else {
        params.keep_lineages
    };
    let sequence = match store::next_sequence(&anchor_paths.anchor) {
        Ok(sequence) => sequence,
        Err(e) => finish_failed!(e),
    };
    let chunk_store = CloudChunkStore::new(
        s3.clone(),
        identity.clone(),
        anchor_paths,
        lineage.clone(),
        sequence,
        chain.chain.clone(),
    );

    let progress = {
        let job = job.clone();
        move |p: PushProgress| {
            let mut view = job.progress();
            view.chunk = p.chunk;
            view.plain_bytes = p.plain_bytes;
            view.uploaded = p.uploaded;
            view.skipped = p.skipped;
            job.set_progress(view);
        }
    };
    let before_publish = |pushed: &PushResult| -> Result<(), String> {
        super::super::server::buddy_send::require_publish_evidence(
            &params.dataset,
            &snapshot,
            &pushed.chain,
            mount_state.as_ref().map(|(_, _, used)| *used),
            pushed.plain_bytes,
            !from_snapshot.is_empty(),
            expected,
            params.force_full,
        )
    };
    let persist_template = resume.clone();
    let persist_path = resume_path.clone();
    let save_state = move |chain: &ChainState| {
        let mut next = persist_template.clone();
        next.chain = chain.clone();
        save_resume(&persist_path, &next)
    };
    let cancel = jobs::cancel_check(token.clone());
    let sign = |manifest: &mut crate::buddy::envelope::Manifest,
                identity: &crate::buddy::keys::Identity| {
        super::sign_manifest(manifest, identity)
    };
    let policy = PushPolicy {
        sign_manifest: &sign,
        max_manifest_bytes: MAX_MANIFEST_BYTES,
        check_cancelled: &cancel,
    };
    let (result, error) = push_with(
        &chunk_store,
        &identity,
        crate::buddy::client::PushOptions {
            source: &source,
            kind,
            reader: &mut reader,
            state: Some(chain.clone()),
            from_snapshot: &from_snapshot,
            to_snapshot: &snapshot,
            from_guid: &from_guid,
            to_guid: &target_guid,
            prune_keep: keep,
            progress: Some(&progress),
            before_publish: Some(&before_publish),
            save_state: &save_state,
        },
        &policy,
    )
    .await;

    match error {
        Some(error) => {
            // Keep the resume state: the next attempt continues this link.
            finish_failed!(error);
        }
        None => {
            let _ = std::fs::remove_file(&resume_path);
            let _ = std::fs::remove_file(&ledger_path);
            prune_local_snapshots(&agent, &params.dataset, &source, &destination.id, keep).await;
            let result_view = JobResult {
                chain: result.chain.clone(),
                snapshot: snapshot.clone(),
                kind: if from_snapshot.is_empty() {
                    "full"
                } else {
                    "incremental"
                }
                .to_string(),
                plain_bytes: result.plain_bytes,
                pruned: Some(result.pruned_chains),
                chunks: Some(result.chunks as i64),
                ..Default::default()
            };
            job.finish(JobState::Succeeded, String::new(), Some(result_view));
            if !params.schedule_id.is_empty() {
                if let Some(cloud) = state.cloud.as_ref() {
                    cloud
                        .schedules
                        .record_result(&params.schedule_id, &destination.id, true, "");
                }
            }
            notify(&state, &job.snapshot(), &destination.name).await;
            record_scheduled_audit(&state, &params, &destination, true, "").await;
        }
    }
}

/// Whether a preflight failure must abandon the link. Only the deterministic
/// cap check does; an estimate RPC/transport error keeps the resume state so a
/// later attempt can continue the same snapshot.
fn preflight_abandons(deterministic: bool) -> bool {
    deterministic
}

/// forceFull must discard an interrupted link rather than continue it.
fn resume_disposition(
    resume: Option<SendResume>,
    force_full: bool,
) -> (Option<SendResume>, Option<SendResume>) {
    if force_full {
        (None, resume)
    } else {
        (resume, None)
    }
}

fn snapshot_guid_of(snaps: &[crate::agent::SnapshotInfo], name: &str) -> Option<String> {
    snaps
        .iter()
        .find(|snapshot| snapshot.name == name)
        .map(|snapshot| snapshot.guid.clone())
}

/// Destroy a snapshot this link created and remove its transient state. Used
/// for forceFull, deterministic preflight failures, and a missing GUID.
async fn abandon_link(
    agent: &crate::agent::Client,
    dataset: &str,
    resume_path: &std::path::Path,
    ledger_path: &std::path::Path,
    snapshot: &str,
) {
    if !snapshot.is_empty() {
        if let Err(e) = agent.destroy_snapshot(dataset, snapshot).await {
            tracing::warn!(
                "cloud send: cannot destroy abandoned snapshot {dataset}@{snapshot}: {e}"
            );
        }
    }
    for path in [resume_path, ledger_path] {
        let _ = tokio::fs::remove_file(path).await;
    }
}

fn load_resume(
    path: &std::path::Path,
    destination: &str,
    source: &str,
    dataset: &str,
) -> Option<SendResume> {
    let data = std::fs::read(path).ok()?;
    let resume: SendResume = serde_json::from_slice(&data).ok()?;
    if resume.destination == destination && resume.source == source && resume.dataset == dataset {
        Some(resume)
    } else {
        None
    }
}

fn save_resume(path: &std::path::Path, resume: &SendResume) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }
    let data = serde_json::to_vec_pretty(resume).map_err(|e| e.to_string())?;
    crate::chartsrepo::write_atomic(&path.display().to_string(), &data, 0o600, 0o700)
}

/// Keep the newest per-destination snapshots. The published `to_snapshot` is
/// the newest, so it is always inside the kept window; a different destination
/// has its own prefix and is never touched.
async fn prune_local_snapshots(
    agent: &crate::agent::Client,
    dataset: &str,
    source: &str,
    destination_id: &str,
    keep: usize,
) {
    let keep = keep.max(1);
    let prefix = snapshot_prefix(source, destination_id);
    let paths = match agent.snapshot_paths(dataset).await {
        Ok(paths) => paths,
        Err(e) => {
            tracing::warn!("cloud send: cannot list snapshots of {dataset} for pruning: {e}");
            return;
        }
    };
    for (target, snapshot) in crate::server::replication::prune_plan(&paths, dataset, &prefix, keep)
    {
        if target != dataset {
            continue;
        }
        if let Err(e) = agent.destroy_snapshot(dataset, &snapshot).await {
            tracing::warn!("cloud send: cannot destroy old snapshot {dataset}@{snapshot}: {e}");
        }
    }
}

async fn notify(state: &AppState, job: &jobs::JobPublic, destination_name: &str) {
    let (event, notif) = match job.state {
        JobState::Succeeded => (
            crate::notifications::EVENT_BACKUP_SUCCESS,
            crate::notifications::Notification {
                title: format!(
                    "Cloud backup of {} to {} succeeded",
                    job.dataset, destination_name
                ),
                message: format!(
                    "source {} chain {} ({} chunks)",
                    job.source,
                    job.result
                        .as_ref()
                        .map(|r| r.chain.clone())
                        .unwrap_or_default(),
                    job.result
                        .as_ref()
                        .map(|r| r.kind.clone())
                        .unwrap_or_default()
                ),
                severity: crate::notifications::SEVERITY_INFO.to_string(),
                tags: vec![
                    "white_check_mark".into(),
                    "naslos".into(),
                    "backup".into(),
                    "cloud".into(),
                ],
            },
        ),
        JobState::Failed | JobState::Cancelled => (
            crate::notifications::EVENT_BACKUP_FAILURE,
            crate::notifications::Notification {
                title: format!(
                    "Cloud backup of {} to {} failed",
                    job.dataset, destination_name
                ),
                message: format!("source {}: {}", job.source, job.error),
                severity: crate::notifications::SEVERITY_ERROR.to_string(),
                tags: vec!["x".into(), "naslos".into(), "backup".into(), "cloud".into()],
            },
        ),
        JobState::Running => return,
    };
    crate::webhook::deliver_logged(
        &state.notifications,
        &state.webhook,
        &state.smtp,
        &state.syslog,
        event,
        &notif,
    )
    .await;
}

async fn record_scheduled_audit(
    state: &AppState,
    params: &SendParams,
    destination: &Destination,
    ok: bool,
    error: &str,
) {
    if params.schedule_id.is_empty() {
        return;
    }
    let pending = crate::audit::Pending::system(
        "cloud",
        "scheduled_send",
        Some(params.schedule_id.clone()),
        serde_json::json!({
            "dataset": params.dataset,
            "destination": destination.id,
            "ok": ok,
            "error": error,
        }),
    );
    let status = if ok { 200 } else { 500 };
    if let Err(e) = state.audit.record_once_async(&pending, status).await {
        tracing::warn!("could not audit the scheduled cloud send: {e}");
    }
}

fn keep_of(params: &SendParams) -> usize {
    if params.keep_lineages == 0 {
        DEFAULT_KEEP_LINEAGES
    } else {
        params.keep_lineages
    }
}

fn ledger_path_for(dir: &std::path::Path, source: &str) -> std::path::PathBuf {
    AnchorPaths::new(dir, source).ledger
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::store::test_link;

    #[test]
    fn the_manifest_preflight_refuses_a_stream_above_the_cap() {
        assert!(preflight_estimate("tank/data", 10 << 20).is_ok());
        let huge = MAX_MANIFEST_BYTES as i64 / 200 * 1024 * 1024;
        assert!(preflight_estimate("tank/data", huge).is_err());
        assert!(preflight_estimate("tank/data", MAX_STREAM_BODY_BYTES + 1).is_err());
    }

    #[test]
    fn manifest_estimate_is_conservative() {
        let (chunks, bytes) = manifest_estimate(3 << 20);
        assert_eq!(chunks, 4);
        assert!(bytes > 4 * 150);
    }

    #[test]
    fn keep_lineages_is_bounded() {
        assert!(valid_keep_lineages(1).is_ok());
        assert!(valid_keep_lineages(100).is_ok());
        assert!(valid_keep_lineages(0).is_err());
        assert!(valid_keep_lineages(101).is_err());
    }

    #[test]
    fn only_a_deterministic_preflight_failure_abandons_the_link() {
        // estimate_send RPC/transport error: keep the resume state + ledger.
        assert!(!preflight_abandons(false));
        // manifest/stream cap rejection: nothing can make this link work.
        assert!(preflight_abandons(true));
    }

    #[test]
    fn force_full_discards_an_interrupted_resume() {
        let resume = SendResume {
            destination: "d".into(),
            source: "tank/data".into(),
            dataset: "tank/data".into(),
            to_snapshot: "naslos-cloud-x-1".into(),
            ..Default::default()
        };
        let (kept, discarded) = resume_disposition(Some(resume.clone()), true);
        assert!(kept.is_none());
        assert_eq!(
            discarded.unwrap().to_snapshot,
            "naslos-cloud-x-1".to_string()
        );
        let (kept, discarded) = resume_disposition(Some(resume), false);
        assert!(kept.is_some());
        assert!(discarded.is_none());
    }

    #[test]
    fn the_new_snapshot_guid_comes_from_the_post_create_listing() {
        let before = vec![crate::agent::SnapshotInfo {
            dataset: "tank/data".into(),
            name: "old".into(),
            guid: "g0".into(),
            created: String::new(),
        }];
        let after = vec![crate::agent::SnapshotInfo {
            dataset: "tank/data".into(),
            name: "new".into(),
            guid: "g1".into(),
            created: String::new(),
        }];
        assert_eq!(snapshot_guid_of(&before, "new"), None);
        assert_eq!(snapshot_guid_of(&after, "new").as_deref(), Some("g1"));
    }

    #[test]
    fn a_base_inside_the_lineage_is_used_until_full_every_or_size() {
        let links = vec![
            test_link("lin", 3, "c", "snap-b"),
            test_link("lin", 2, "b", "snap-a"),
            test_link("lin", 1, "a", ""),
        ];
        // A lone incremental has no full to compare against and is under the
        // link cap, so it may continue. The full listing exceeds the full's
        // bytes (the size trigger), and 3 links trips fullEvery.
        assert!(!must_send_full(&links[..1], Some(&links[0]), 30));
        assert!(must_send_full(&links, Some(&links[0]), 30));
        assert!(must_send_full(&links, Some(&links[0]), 3));
        assert!(must_send_full(&links, None, 30));
    }

    #[test]
    fn the_newest_local_guid_that_is_published_is_the_base() {
        let links = vec![
            test_link("lin", 2, "b", "snap-a"),
            test_link("lin", 1, "a", ""),
        ];
        let mut local = std::collections::HashSet::new();
        local.insert("guid-snap-b".to_string());
        assert_eq!(
            find_base(&links, &local).map(|l| l.chain.as_str()),
            Some("b")
        );
        assert!(find_base(&links, &std::collections::HashSet::new()).is_none());
    }
}
