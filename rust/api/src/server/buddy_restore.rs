//! Buddy restore (port of the restore half of `server/buddy_send.go`).
//!
//! Restoring verifies the stream before ZFS sees it: the client checks the
//! manifest signature, the chain's contiguity and every chunk's digest, so a
//! restore either lands whole or fails loudly.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};

use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use sha2::Digest;
use tokio::io::AsyncWrite;

use crate::buddy::client::{Client, RestoreOptions};
use crate::buddy::keys::load_identity;
use crate::buddy::peers::validate_source;
use crate::json;
use crate::state::AppState;

fn bad_request(msg: &str) -> Response {
    json(StatusCode::BAD_REQUEST, serde_json::json!({ "error": msg }))
}

/// The datasets with a restore currently running. Whole restores are
/// serialized per destination: two concurrent restores of the same dataset
/// would interleave their per-chain receives and share one recovery point, so
/// the second could destroy the first's crash marker (B-7).
fn active_restores() -> &'static Mutex<HashSet<String>> {
    static ACTIVE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Tests in any module that assert on the process-wide flight set must hold
/// this lock, so one test cannot observe another test's flight.
#[cfg(test)]
pub(crate) async fn test_lock() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    LOCK.lock().await
}

/// Releases the per-dataset restore slot on every exit path.
pub(crate) struct RestoreFlight {
    dataset: String,
}

impl RestoreFlight {
    /// True while any synchronous restore is in flight.
    pub(crate) fn any_in_flight() -> bool {
        match active_restores().lock() {
            Ok(guard) => !guard.is_empty(),
            Err(poisoned) => !poisoned.into_inner().is_empty(),
        }
    }

    /// `None` when a restore of this dataset is already in progress.
    pub(crate) fn acquire(dataset: &str) -> Option<Self> {
        let mut active = match active_restores().lock() {
            Ok(guard) => guard,
            // A panic in another test/request cannot make the slot silently
            // disappear: recover the set and keep enforcing single-flight.
            Err(poisoned) => poisoned.into_inner(),
        };
        if !active.insert(dataset.to_string()) {
            return None;
        }
        Some(Self {
            dataset: dataset.to_string(),
        })
    }
}

impl Drop for RestoreFlight {
    fn drop(&mut self) {
        let mut active = match active_restores().lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        active.remove(&self.dataset);
    }
}

/// Validate a buddy URL (http/https only, with a host).
pub fn normalize_receiver_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("receiver is required: it is the buddy's base URL".to_string());
    }
    let parsed = reqwest::Url::parse(trimmed).map_err(|e| format!("invalid receiver URL: {e}"))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(format!(
            "receiver URL {trimmed:?} must start with http:// or https://"
        ));
    }
    if parsed.host_str().is_none() {
        return Err(format!("receiver URL {trimmed:?} has no host"));
    }
    Ok(trimmed.trim_end_matches('/').to_string())
}

fn identity_path() -> String {
    std::env::var("BUDDY_IDENTITY")
        .unwrap_or_else(|_| "/var/lib/naslos/buddy-identity.json".to_string())
}

fn sequence_dir() -> String {
    let identity = identity_path();
    std::path::Path::new(&identity)
        .parent()
        .map(|p| p.join("buddy-sequences").display().to_string())
        .unwrap_or_else(|| "buddy-sequences".to_string())
}

/// A sink that hashes everything written to it (the verify path).
pub(crate) struct ShaWriter {
    pub(crate) hasher: sha2::Sha256,
}

impl AsyncWrite for ShaWriter {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.hasher.update(buf);
        std::task::Poll::Ready(Ok(buf.len()))
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

#[derive(Default, serde::Deserialize)]
struct RestoreRequest {
    #[serde(default)]
    source: String,
    #[serde(default)]
    receiver: String,
    #[serde(default)]
    dataset: String,
    #[serde(default)]
    chain: String,
    #[serde(default)]
    force: bool,
    #[serde(default)]
    verify: bool,
}

/// `POST /api/buddy/restore`
pub async fn restore(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let req: RestoreRequest = match super::zfs::read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let source = req.source.trim().to_string();
    let dataset = req.dataset.trim().to_string();
    if source.is_empty() {
        return bad_request("source is required");
    }
    if dataset.is_empty() && !req.verify {
        return bad_request("dataset is required (it is where the backup is restored to)");
    }
    if !dataset.is_empty() {
        if let Err(e) = super::zfs::validate_dataset_path(&dataset) {
            return bad_request(&format!("invalid restore dataset: {e}"));
        }
    }
    if let Err(e) = validate_source(&source) {
        return bad_request(&e);
    }
    let receiver = match normalize_receiver_url(&req.receiver) {
        Ok(r) => r,
        Err(e) => return bad_request(&e),
    };
    let identity = match load_identity(&identity_path()) {
        Ok(i) => std::sync::Arc::new(i),
        Err(e) => {
            return json(
                StatusCode::PRECONDITION_FAILED,
                serde_json::json!({ "error": e }),
            )
        }
    };
    let mut client = Client::new(&receiver, identity);
    client.sequence_dir = sequence_dir();

    let sequence = match client.restore_sequence(&source, &req.chain).await {
        Ok(s) => s,
        Err(e) => return json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e })),
    };
    let started = std::time::Instant::now();

    if req.verify {
        let mut digests = Vec::new();
        let mut total = 0i64;
        for chain in &sequence {
            let mut sink = ShaWriter {
                hasher: sha2::Sha256::new(),
            };
            let (result, err) = client
                .restore(RestoreOptions {
                    source: &source,
                    chain: &chain.chain,
                    out: &mut sink,
                    progress: None,
                    skip_signature_check: false,
                    expected_sequence: Some(chain.sequence),
                    expected_from_guid: &chain.from_guid,
                    expected_to_guid: &chain.to_guid,
                    expected_kind: &chain.kind,
                    expected_from_snapshot: &chain.from_snapshot,
                    expected_to_snapshot: &chain.to_snapshot,
                })
                .await;
            if let Some(e) = err {
                return json(StatusCode::BAD_GATEWAY, serde_json::json!({ "error": e }));
            }
            digests.push(serde_json::json!({
                "chain": chain.chain,
                "sha256": hex(&sink.hasher.finalize()),
                "bytes": result.plain_bytes,
            }));
            total += result.plain_bytes;
        }
        return json(
            StatusCode::OK,
            serde_json::json!({
                "status": "verified",
                "source": source,
                "receiver": receiver,
                "chains": digests,
                "plainBytes": total,
                "durationSeconds": started.elapsed().as_secs_f64(),
            }),
        );
    }

    let Some(agent) = state.agent.clone() else {
        return json(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({ "error": "the API has no agent client, so it cannot stream ZFS data" }),
        );
    };

    // One whole restore per destination dataset: concurrent restores would
    // interleave per-chain receives and share one recovery point (B-7).
    let Some(_flight) = RestoreFlight::acquire(&dataset) else {
        return json(
            StatusCode::CONFLICT,
            serde_json::json!({
                "error": format!("a restore of {dataset} is already in progress on this instance; wait for it to finish or fail before retrying")
            }),
        );
    };

    // A previous attempt may have died mid-restore. Roll the destination back
    // to the snapshot taken before that attempt, then start over: a partially
    // applied receive cannot be resumed safely (R-059). The marker is keyed
    // by source and dataset, and is removed only once the dataset it names has
    // actually been recovered.
    let progress_path = restore_progress_path(&source, &dataset);
    if let Some(prev) = RestoreProgress::load(&progress_path) {
        if prev.dataset != dataset || prev.source != source {
            return json(
                StatusCode::CONFLICT,
                serde_json::json!({
                    "error": format!(
                        "the restore marker {progress_path} names {} of {:?}, not {dataset} of {source:?}; inspect and remove it before retrying",
                        prev.dataset, prev.source
                    )
                }),
            );
        }
        if let Err(e) = recover_interrupted_restore(&agent, &dataset, &prev, &progress_path).await {
            return json(StatusCode::CONFLICT, serde_json::json!({ "error": e }));
        }
    }

    // Verify every chain end-to-end before the first `zfs receive`, so a
    // corrupted later chain cannot leave the destination half-restored.
    for chain in &sequence {
        if let Err(e) = verify_chain(&client, &source, chain).await {
            return json(StatusCode::BAD_GATEWAY, serde_json::json!({ "error": e }));
        }
    }

    // Snapshot the destination after verification and before applying. The
    // snapshot is destroyed on success and used to roll back on failure.
    let mut progress = RestoreProgress {
        dataset: dataset.clone(),
        source: source.clone(),
        receiver: receiver.clone(),
        snapshot: format!(
            "buddy-restore-{}-{}",
            chrono::Utc::now().format("%Y%m%dT%H%M%SZ"),
            random_suffix()
        ),
        applied_chains: Vec::new(),
        chunks: 0,
        plain_bytes: 0,
        rollback_lost: false,
    };
    match agent.create_snapshot(&dataset, &progress.snapshot).await {
        Ok(()) => {}
        Err(e) => {
            // A new destination dataset has nothing to snapshot. An existing
            // one must have a rollback point, or a failed receive would leave
            // it half-restored.
            let exists = agent
                .list_datasets()
                .await
                .map(|datasets| datasets.iter().any(|d| d.name == dataset))
                .unwrap_or(true);
            if exists {
                return json(
                    StatusCode::CONFLICT,
                    serde_json::json!({ "error": format!("cannot snapshot the restore destination {dataset} before applying: {e}") }),
                );
            }
            tracing::warn!(
                "buddy restore: {dataset} does not exist yet, so there is no rollback snapshot"
            );
            progress.snapshot.clear();
        }
    }
    if let Err(e) = progress.save(&progress_path) {
        tracing::warn!("buddy restore: cannot record the restore index: {e}");
    }

    let outcome: Result<(Vec<String>, usize, i64), (StatusCode, String)> = async {
        let mut applied = Vec::new();
        let mut total_chunks = 0usize;
        let mut total_bytes = 0i64;
        for (i, chain) in sequence.iter().enumerate() {
            // -F only on the first stream: later ones are incrementals.
            let force = req.force && i == 0;
            let (mut writer, reader) = tokio::io::duplex(256 * 1024);
            let agent_for_task = agent.clone();
            let dataset_for_task = dataset.clone();
            let handle = tokio::spawn(async move {
                agent_for_task
                    .receive_stream_body(&dataset_for_task, force, reader)
                    .await
            });

            let (result, err) = client
                .restore(RestoreOptions {
                    source: &source,
                    chain: &chain.chain,
                    out: &mut writer,
                    progress: None,
                    skip_signature_check: false,
                    expected_sequence: Some(chain.sequence),
                    expected_from_guid: &chain.from_guid,
                    expected_to_guid: &chain.to_guid,
                    expected_kind: &chain.kind,
                    expected_from_snapshot: &chain.from_snapshot,
                    expected_to_snapshot: &chain.to_snapshot,
                })
                .await;
            drop(writer);
            let wait = handle.await;
            if let Some(e) = err {
                return Err((StatusCode::BAD_GATEWAY, e));
            }
            match wait {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    return Err((
                        StatusCode::UNPROCESSABLE_ENTITY,
                        format!("zfs receive refused chain {}: {e}", chain.chain),
                    ))
                }
                Err(e) => {
                    return Err((
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("zfs receive task failed: {e}"),
                    ))
                }
            }
            applied.push(chain.chain.clone());
            total_chunks += result.chunks;
            total_bytes += result.plain_bytes;
            progress.applied_chains.push(chain.chain.clone());
            progress.chunks = total_chunks;
            progress.plain_bytes = total_bytes;
            if let Err(e) = progress.save(&progress_path) {
                tracing::warn!("buddy restore: cannot record the restore index: {e}");
            }
        }
        Ok((applied, total_chunks, total_bytes))
    }
    .await;

    let (applied, total_chunks, total_bytes) = match outcome {
        Ok(v) => v,
        Err((status, error)) => {
            // Undo the partial receive. The recovery point (snapshot or a
            // freshly created dataset) is destroyed only after the rollback
            // succeeded; when it cannot be finished, both it and the marker
            // stay and the error names the recovery point (B-4).
            let error =
                match cleanup_failed_restore(&agent, &dataset, &mut progress, &progress_path).await
                {
                    Ok(()) => error,
                    Err(cleanup) => format!("{error}; {cleanup}"),
                };
            return json(status, serde_json::json!({ "error": error }));
        }
    };
    // The restore succeeded: drop the marker first, then the pre-restore
    // snapshot. If this crashes (or the unlink fails) after the marker is
    // gone, only an orphaned snapshot is left — never a marker naming a
    // snapshot that no longer exists. The recovery path also treats a missing
    // snapshot as already recovered, so either ordering is safe.
    if let Err(e) = std::fs::remove_file(&progress_path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!("buddy restore: cannot remove the restore marker {progress_path}: {e}");
        }
    }
    if !progress.snapshot.is_empty() {
        match agent.destroy_snapshot(&dataset, &progress.snapshot).await {
            Ok(()) => {}
            Err(e) if snapshot_already_gone(&e) => {
                tracing::warn!(
                    "buddy restore: the pre-restore snapshot {} of {dataset} is already gone",
                    progress.snapshot
                );
            }
            Err(e) => {
                tracing::warn!(
                    "buddy restore: cannot destroy the pre-restore snapshot {} of {dataset}: {e}; \
                     the restore succeeded, so delete that snapshot manually after verifying the data",
                    progress.snapshot
                );
            }
        }
    }

    let last = sequence.last().cloned().unwrap_or_default();
    json(
        StatusCode::OK,
        serde_json::json!({
            "status": "restored",
            "source": source,
            "dataset": dataset,
            "chains": applied,
            "chain": last.chain,
            "incrementalFrom": last.from_snapshot,
            "snapshot": last.to_snapshot,
            "chunks": total_chunks,
            "plainBytes": total_bytes,
            "kind": last.kind,
            "takenAt": last.created_at,
            "durationSeconds": started.elapsed().as_secs_f64(),
        }),
    )
}

/// Whether a rollback/destroy failure means the snapshot no longer exists.
///
/// This is only a secondary signal: the agent redacts a missing-snapshot
/// `ZfsError` to a 500 (`internal error (ref …)`), not a 404, so recovery
/// probes the live snapshot listing before relying on it.
fn snapshot_already_gone(error: &crate::agent::AgentError) -> bool {
    if error.status == 404 {
        return true;
    }
    let message = error.message.to_ascii_lowercase();
    ["not found", "does not exist", "no such", "could not find"]
        .iter()
        .any(|needle| message.contains(needle))
}

/// Whether the live agent still has `dataset@snapshot`. `Ok(false)` means a
/// successful listing does not contain it (it is gone); `Err` means the
/// listing itself failed, so the snapshot's state is unknown.
async fn live_snapshot_exists(
    agent: &crate::agent::Client,
    dataset: &str,
    snapshot: &str,
) -> Result<bool, String> {
    let paths = agent
        .snapshot_paths(dataset)
        .await
        .map_err(|e| format!("the agent could not list the snapshots of {dataset}: {e}"))?;
    let full = format!("{dataset}@{snapshot}");
    Ok(paths.iter().any(|path| path == &full))
}

/// Whether the dataset still exists according to the live dataset listing.
pub(crate) async fn live_dataset_exists(
    agent: &crate::agent::Client,
    dataset: &str,
) -> Result<bool, String> {
    let datasets = agent
        .list_datasets()
        .await
        .map_err(|e| format!("the agent could not list datasets: {e}"))?;
    Ok(datasets.iter().any(|entry| entry.name == dataset))
}

fn remove_marker(progress_path: &str) {
    if let Err(e) = std::fs::remove_file(progress_path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!("buddy restore: cannot remove the restore marker {progress_path}: {e}");
        }
    }
}

/// Record on the marker that the failed receive has no rollback point left.
/// A save failure is returned: the caller must not claim the marker was
/// updated when it wasn't.
fn mark_rollback_lost(progress: &mut RestoreProgress, progress_path: &str) -> Result<(), String> {
    progress.rollback_lost = true;
    progress
        .save(progress_path)
        .map_err(|e| format!("the restore marker could not be updated: {e}"))
}

/// A failed receive whose rollback point is gone cannot be undone. Keep the
/// marker (marked `rollbackLost`) while the destination still exists (it may be
/// half-applied); drop it only when the dataset itself is gone (nothing left
/// to recover).
async fn keep_marker_without_rollback(
    agent: &crate::agent::Client,
    dataset: &str,
    progress: &mut RestoreProgress,
    progress_path: &str,
    reason: &str,
) -> Result<(), String> {
    match live_dataset_exists(agent, dataset).await {
        Ok(false) => {
            tracing::warn!(
                "buddy restore: {dataset} no longer exists, so its failed restore has nothing left to recover; \
                 dropping the restore marker"
            );
            remove_marker(progress_path);
            Ok(())
        }
        Ok(true) => {
            let marked = mark_rollback_lost(progress, progress_path);
            let suffix = match &marked {
                Ok(()) => String::new(),
                Err(e) => format!(" ({e})"),
            };
            Err(format!(
                "the restore of {dataset} failed and {reason}, so the destination may be half-applied with no rollback point; \
                 the restore marker {progress_path} is kept for inspection{suffix}. Destroy {dataset} or verify and repair it \
                 manually before retrying"
            ))
        }
        Err(e) => {
            let marked = mark_rollback_lost(progress, progress_path);
            let suffix = match &marked {
                Ok(()) => String::new(),
                Err(e) => format!(" ({e})"),
            };
            Err(format!(
                "the restore of {dataset} failed and {reason}; {e}, so the destination could not be checked; \
                 the restore marker {progress_path} is kept because it may be half-applied{suffix}"
            ))
        }
    }
}

/// Undo an interrupted restore recorded in `prev`. The snapshot (or the
/// freshly created destination) is destroyed only after the rollback
/// succeeded, and the marker is removed only after that cleanup. On any
/// failure both stay put and the returned error names the recovery point. A
/// snapshot absent from a successful live listing is a stale marker from the
/// success-path crash window and is treated as already recovered — except when
/// the marker is flagged `rollbackLost`: then the previous receive failed with
/// no rollback point and recovery must refuse until an operator acts. A
/// listing failure keeps the marker unless the destination dataset itself is
/// gone (nothing left to recover).
async fn recover_interrupted_restore(
    agent: &crate::agent::Client,
    dataset: &str,
    prev: &RestoreProgress,
    progress_path: &str,
) -> Result<(), String> {
    if prev.rollback_lost {
        // The previous receive failed and its rollback point is unusable: the
        // destination may be half-applied. The marker looks identical to a
        // stale success marker (snapshot gone, dataset present), so the flag
        // is the only thing preventing a silent overwrite.
        return match live_dataset_exists(agent, dataset).await {
            Ok(false) => {
                tracing::warn!(
                    "buddy restore: the previous restore of {dataset} lost its rollback point but {dataset} no longer exists; \
                     dropping the marker and starting fresh"
                );
                remove_marker(progress_path);
                Ok(())
            }
            Ok(true) => Err(format!(
                "the previous restore of {dataset} failed and lost its rollback point, so the destination may be half-applied with no \
                 safe recovery; destroy {dataset} or verify and repair it manually, then remove the restore marker {progress_path}. \
                 Refusing to restore over it"
            )),
            Err(e) => Err(format!(
                "the previous restore of {dataset} lost its rollback point, and {e}; the restore marker {progress_path} is kept \
                 because the destination may be half-applied"
            )),
        };
    }
    if !prev.snapshot.is_empty() {
        match live_snapshot_exists(agent, dataset, &prev.snapshot).await {
            Ok(false) => {
                tracing::warn!(
                    "buddy restore: the interrupted restore of {dataset} names snapshot {:?}, which is absent from the live snapshot \
                     listing; treating the stale marker as recovered",
                    prev.snapshot
                );
            }
            Err(e) => {
                // A gone destination dataset makes the snapshot listing fail
                // (and the agent redacts that to a 500). There is nothing left
                // to recover then, so drop the stale marker and let the
                // restore recreate the dataset.
                match live_dataset_exists(agent, dataset).await {
                    Ok(false) => {
                        tracing::warn!(
                            "buddy restore: cannot list the snapshots of {dataset} ({e}), and the dataset itself no longer exists; \
                             dropping the stale restore marker"
                        );
                    }
                    Ok(true) => {
                        return Err(format!(
                            "a previous restore of {dataset} is recorded, but {e}; the snapshot and the restore marker {progress_path} are kept, \
                             so the dataset can still be recovered"
                        ))
                    }
                    Err(ds_error) => {
                        return Err(format!(
                            "a previous restore of {dataset} is recorded, but {e}; {ds_error}, so the destination could not be checked; \
                             the snapshot and the restore marker {progress_path} are kept"
                        ))
                    }
                }
            }
            Ok(true) => match agent.rollback_snapshot(dataset, &prev.snapshot).await {
                Ok(()) => match agent.destroy_snapshot(dataset, &prev.snapshot).await {
                    Ok(()) => {}
                    Err(e) if snapshot_already_gone(&e) => {
                        tracing::warn!(
                            "buddy restore: the interrupted restore's snapshot {} of {dataset} is already gone",
                            prev.snapshot
                        );
                    }
                    Err(e) => {
                        return Err(format!(
                            "a previous restore of {dataset} was rolled back to snapshot {:?}, but deleting that snapshot failed: {e}; \
                             the snapshot and the restore marker {progress_path} are kept for cleanup",
                            prev.snapshot
                        ))
                    }
                },
                Err(e) if snapshot_already_gone(&e) => {
                    // The snapshot disappeared between the probe and the
                    // rollback. The listing is authoritative for "%s gone".
                    tracing::warn!(
                        "buddy restore: the interrupted restore of {dataset} names snapshot {:?}, which no longer exists; \
                         treating the stale marker as recovered",
                        prev.snapshot
                    );
                }
                Err(e) => {
                    return Err(format!(
                        "a previous restore of {dataset} was interrupted and rolling it back to snapshot {:?} failed: {e}; \
                         the snapshot and the restore marker {progress_path} are kept, so the dataset can still be recovered",
                        prev.snapshot
                    ))
                }
            },
        }
    } else {
        // The interrupted attempt created the destination from nothing and
        // recorded no rollback point, so removing the partial dataset is the
        // only recovery. A destination that is already gone counts as done.
        match agent.destroy_dataset(dataset, true).await {
            Ok(()) => {}
            Err(e) if e.status == 404 => {}
            Err(e) => {
                return Err(format!(
                    "a previous restore created {dataset} and failed part-way; destroying the partial dataset failed: {e}; \
                     the restore marker {progress_path} is kept for cleanup"
                ))
            }
        }
    }
    remove_marker(progress_path);
    Ok(())
}

/// Undo a failed in-flight restore. Returns an error describing the leftover
/// recovery point; the caller appends it to the original restore error.
async fn cleanup_failed_restore(
    agent: &crate::agent::Client,
    dataset: &str,
    progress: &mut RestoreProgress,
    progress_path: &str,
) -> Result<(), String> {
    if !progress.snapshot.is_empty() {
        let snapshot_exists = match live_snapshot_exists(agent, dataset, &progress.snapshot).await {
            Ok(exists) => exists,
            Err(e) => {
                // A gone destination dataset makes the snapshot listing fail
                // (redacted 500). If the dataset is gone there is nothing to
                // recover; otherwise the marker stays and is flagged as
                // having no confirmed rollback point.
                return keep_marker_without_rollback(
                    agent,
                    dataset,
                    progress,
                    progress_path,
                    &format!("its snapshot state could not be checked ({e})"),
                )
                .await;
            }
        };
        if !snapshot_exists {
            return keep_marker_without_rollback(
                agent,
                dataset,
                progress,
                progress_path,
                "its rollback snapshot is gone",
            )
            .await;
        }
        match agent.rollback_snapshot(dataset, &progress.snapshot).await {
            Ok(()) => match agent.destroy_snapshot(dataset, &progress.snapshot).await {
                Ok(()) => {}
                Err(e) if snapshot_already_gone(&e) => {
                    tracing::warn!(
                        "buddy restore: the pre-restore snapshot {} of {dataset} is already gone",
                        progress.snapshot
                    );
                }
                Err(e) => {
                    return Err(format!(
                        "{dataset} was rolled back to snapshot {:?}, but deleting that snapshot failed: {e}; \
                         the snapshot and the restore marker {progress_path} are kept for cleanup",
                        progress.snapshot
                    ))
                }
            },
            Err(e) if snapshot_already_gone(&e) => {
                // The snapshot vanished between the probe and the rollback;
                // the receive failed, so keep the marker if the destination
                // may still be half-applied.
                return keep_marker_without_rollback(
                    agent,
                    dataset,
                    progress,
                    progress_path,
                    "its rollback snapshot is gone",
                )
                .await;
            }
            Err(e) => {
                return Err(format!(
                    "rolling {dataset} back to snapshot {:?} failed: {e}; the snapshot and the restore marker {progress_path} are kept, \
                     so restore the dataset from that snapshot before deleting it",
                    progress.snapshot
                ))
            }
        }
    } else {
        // There was no pre-restore snapshot because the destination did not
        // exist: the failed receive created it, so destroy it rather than
        // leaving a half-applied dataset with no recovery point.
        match agent.destroy_dataset(dataset, true).await {
            Ok(()) => {}
            Err(e) if e.status == 404 => {}
            Err(e) => {
                return Err(format!(
                    "the partially created destination {dataset} could not be destroyed: {e}; \
                     the restore marker {progress_path} is kept for cleanup"
                ))
            }
        }
    }
    remove_marker(progress_path);
    Ok(())
}

async fn verify_chain(
    client: &Client,
    source: &str,
    chain: &crate::buddy::client::ChainSummary,
) -> Result<i64, String> {
    let mut sink = ShaWriter {
        hasher: sha2::Sha256::new(),
    };
    let (result, err) = client
        .restore(RestoreOptions {
            source,
            chain: &chain.chain,
            out: &mut sink,
            progress: None,
            skip_signature_check: false,
            expected_sequence: Some(chain.sequence),
            expected_from_guid: &chain.from_guid,
            expected_to_guid: &chain.to_guid,
            expected_kind: &chain.kind,
            expected_from_snapshot: &chain.from_snapshot,
            expected_to_snapshot: &chain.to_snapshot,
        })
        .await;
    match err {
        Some(e) => Err(e),
        None => Ok(result.plain_bytes),
    }
}

/// Where an in-flight restore records its snapshot and applied chain index.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RestoreProgress {
    pub(crate) dataset: String,
    pub(crate) source: String,
    pub(crate) receiver: String,
    pub(crate) snapshot: String,
    pub(crate) applied_chains: Vec<String>,
    pub(crate) chunks: usize,
    pub(crate) plain_bytes: i64,
    /// The marker was kept after a failed receive with no usable rollback
    /// point, so the destination may be half-applied. Recovery must refuse
    /// until an operator verifies/destroys the dataset or removes the marker.
    /// Missing from older markers, hence the serde default.
    #[serde(default)]
    pub(crate) rollback_lost: bool,
}

impl RestoreProgress {
    pub(crate) fn load(path: &str) -> Option<Self> {
        let data = std::fs::read(path).ok()?;
        serde_json::from_slice(&data).ok()
    }

    pub(crate) fn save(&self, path: &str) -> Result<(), String> {
        let data = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        crate::chartsrepo::write_atomic(path, &data, 0o600, 0o700)
    }
}

fn random_suffix() -> String {
    let mut buf = [0u8; 4];
    if getrandom::fill(&mut buf).is_err() {
        return std::process::id().to_string();
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn restore_progress_path(source: &str, dataset: &str) -> String {
    let digest = sha2::Sha256::digest(format!("{source}|{dataset}").as_bytes());
    format!("{}/restore-{}.json", sequence_dir(), hex(&digest[..8]))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::{
        cleanup_failed_restore, recover_interrupted_restore, restore_progress_path, RestoreFlight,
        RestoreProgress,
    };

    /// A stub agent whose responses are routed by method and path.
    async fn agent_stub<F>(handler: F) -> (String, tokio::task::JoinHandle<()>)
    where
        F: Fn(&str, &str) -> (u16, &'static str, &'static str) + Send + Sync + 'static,
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the test agent");
        let addr = listener.local_addr().expect("local address");
        let handler = std::sync::Arc::new(handler);
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let handler = handler.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 8192];
                    let n = socket.read(&mut buf).await.unwrap_or(0);
                    let head = String::from_utf8_lossy(&buf[..n]);
                    let mut parts = head.lines().next().unwrap_or("").split_whitespace();
                    let method = parts.next().unwrap_or("");
                    let path = parts.next().unwrap_or("");
                    let (status, reason, body) = handler(method, path);
                    let head = format!(
                        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = socket.write_all(head.as_bytes()).await;
                    let _ = socket.write_all(body.as_bytes()).await;
                });
            }
        });
        (format!("http://{addr}"), handle)
    }

    fn write_marker(path: &std::path::Path, snapshot: &str) -> RestoreProgress {
        let progress = RestoreProgress {
            dataset: "test/restored".to_string(),
            source: "naslos-a/data".to_string(),
            receiver: "https://naslos-b".to_string(),
            snapshot: snapshot.to_string(),
            ..Default::default()
        };
        progress
            .save(&path.display().to_string())
            .expect("save marker");
        progress
    }

    #[test]
    fn restore_progress_round_trips_and_is_keyed_per_source_and_dataset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("restore.json");
        let path = path.to_str().unwrap();
        assert_ne!(
            restore_progress_path("naslos-a/data", "test/restored"),
            restore_progress_path("naslos-a/other", "test/restored")
        );
        assert_ne!(
            restore_progress_path("naslos-a/data", "test/restored"),
            restore_progress_path("naslos-a/data", "test/other")
        );
        assert_eq!(
            restore_progress_path("naslos-a/data", "test/restored"),
            restore_progress_path("naslos-a/data", "test/restored")
        );
        let progress = RestoreProgress {
            dataset: "test/restored".to_string(),
            source: "naslos-a/data".to_string(),
            receiver: "https://naslos-b".to_string(),
            snapshot: "buddy-restore-1-abcd".to_string(),
            applied_chains: vec!["aaaa".to_string(), "bbbb".to_string()],
            chunks: 7,
            plain_bytes: 4096,
            rollback_lost: false,
        };
        progress.save(path).unwrap();
        let loaded = RestoreProgress::load(path).unwrap();
        assert_eq!(loaded.dataset, "test/restored");
        assert_eq!(loaded.applied_chains.len(), 2);
        assert_eq!(loaded.chunks, 7);
        let raw = std::fs::read_to_string(path).unwrap();
        assert!(raw.contains("appliedChains"), "{raw}");
    }

    #[tokio::test]
    async fn a_second_restore_of_the_same_dataset_is_refused_until_the_first_finishes() {
        let _guard = super::test_lock().await;
        let first = RestoreFlight::acquire("test/single-flight").expect("first restore acquires");
        assert!(
            RestoreFlight::acquire("test/single-flight").is_none(),
            "a concurrent restore of the same dataset must be refused"
        );
        // A different dataset is independent.
        let other = RestoreFlight::acquire("test/other").expect("different dataset acquires");
        drop(other);
        drop(first);
        let again = RestoreFlight::acquire("test/single-flight")
            .expect("the slot is released when the restore ends");
        drop(again);
    }

    /// The success path removes the marker before the snapshot; a crash in
    /// that window leaves a marker naming a gone snapshot. Recovery probes the
    /// live snapshot listing and drops the stale marker instead of returning a
    /// permanent 409 (the agent reports a missing snapshot to rollback as a
    /// redacted 500, so the probe is what makes this work in production).
    #[tokio::test]
    async fn a_marker_whose_snapshot_is_gone_is_recovered_not_a_permanent_conflict() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("restore.json");
        let progress = write_marker(&marker, "buddy-restore-gone");
        let (url, stub) = agent_stub(|method, path| {
            if method == "GET" && path.starts_with("/api/v1/snapshots/test/restored") {
                (200, "OK", "[]")
            } else {
                // A rollback here would be a bug: the probe must short-circuit.
                (
                    500,
                    "Internal Server Error",
                    r#"{"error":"unexpected request"}"#,
                )
            }
        })
        .await;
        let agent = crate::agent::Client::new(&url, "test", "").expect("agent client");

        let result = recover_interrupted_restore(
            &agent,
            "test/restored",
            &progress,
            &marker.display().to_string(),
        )
        .await;
        stub.abort();
        assert!(result.is_ok(), "{result:?}");
        assert!(
            !marker.exists(),
            "the stale marker must be dropped once its snapshot is gone"
        );
    }

    /// A listing failure is a hard failure: the snapshot's state is unknown,
    /// so the marker must stay.
    #[tokio::test]
    async fn a_snapshot_listing_error_keeps_the_marker() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("restore.json");
        let progress = write_marker(&marker, "buddy-restore-kept");
        let (url, stub) =
            agent_stub(|_method, _path| (500, "Internal Server Error", r#"{"error":"zfs busy"}"#))
                .await;
        let agent = crate::agent::Client::new(&url, "test", "").expect("agent client");

        let result = recover_interrupted_restore(
            &agent,
            "test/restored",
            &progress,
            &marker.display().to_string(),
        )
        .await;
        stub.abort();
        assert!(
            result.is_err(),
            "a listing error must not be assumed to mean gone"
        );
        assert!(
            marker.exists(),
            "the marker must be kept when the state is unknown"
        );
    }

    /// A rollback that fails for a real reason still keeps the recovery point
    /// and names the snapshot in the error.
    #[tokio::test]
    async fn a_real_rollback_failure_keeps_the_marker_and_names_the_snapshot() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("restore.json");
        let progress = write_marker(&marker, "buddy-restore-kept");
        let (url, stub) = agent_stub(|method, path| {
            if method == "GET" && path.starts_with("/api/v1/snapshots/test/restored") {
                (200, "OK", r#"["test/restored@buddy-restore-kept"]"#)
            } else if method == "POST" && path.starts_with("/api/v1/rollback/") {
                (
                    500,
                    "Internal Server Error",
                    r#"{"error":"rollback failed"}"#,
                )
            } else {
                (404, "Not Found", r#"{"error":"unexpected request"}"#)
            }
        })
        .await;
        let agent = crate::agent::Client::new(&url, "test", "").expect("agent client");

        let error = recover_interrupted_restore(
            &agent,
            "test/restored",
            &progress,
            &marker.display().to_string(),
        )
        .await
        .expect_err("a real rollback failure must not be treated as recovered");
        stub.abort();
        assert!(error.contains("buddy-restore-kept"), "{error}");
        assert!(marker.exists(), "the marker must be kept for recovery");
    }

    /// A failed receive whose snapshot is gone but whose destination still
    /// exists may be half-applied: keep the marker and say so.
    #[tokio::test]
    async fn a_failed_restore_with_a_gone_snapshot_keeps_the_marker_while_the_dataset_exists() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("restore.json");
        let mut progress = write_marker(&marker, "buddy-restore-gone");
        let (url, stub) = agent_stub(|method, path| {
            if method == "GET" && path.starts_with("/api/v1/snapshots/test/restored") {
                (200, "OK", "[]")
            } else if method == "GET" && path == "/api/v1/datasets" {
                (200, "OK", r#"[{"name":"test/restored"}]"#)
            } else {
                (404, "Not Found", r#"{"error":"unexpected request"}"#)
            }
        })
        .await;
        let agent = crate::agent::Client::new(&url, "test", "").expect("agent client");

        let error = cleanup_failed_restore(
            &agent,
            "test/restored",
            &mut progress,
            &marker.display().to_string(),
        )
        .await
        .expect_err("a half-applied destination with no rollback point must be reported");
        stub.abort();
        assert!(error.contains("test/restored"), "{error}");
        assert!(error.contains("half-applied"), "{error}");
        assert!(marker.exists(), "the marker must be kept for inspection");
        let reloaded =
            RestoreProgress::load(&marker.display().to_string()).expect("reload the marker");
        assert!(
            reloaded.rollback_lost,
            "the marker must record that the rollback point is lost"
        );
    }

    /// A failed receive whose snapshot and dataset are both gone has nothing
    /// to recover, so the marker is dropped.
    #[tokio::test]
    async fn a_failed_restore_with_a_gone_dataset_drops_the_marker() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("restore.json");
        let mut progress = write_marker(&marker, "buddy-restore-gone");
        let (url, stub) = agent_stub(|method, path| {
            if method == "GET"
                && (path.starts_with("/api/v1/snapshots/test/restored")
                    || path == "/api/v1/datasets")
            {
                (200, "OK", "[]")
            } else {
                (404, "Not Found", r#"{"error":"unexpected request"}"#)
            }
        })
        .await;
        let agent = crate::agent::Client::new(&url, "test", "").expect("agent client");

        let result = cleanup_failed_restore(
            &agent,
            "test/restored",
            &mut progress,
            &marker.display().to_string(),
        )
        .await;
        stub.abort();
        assert!(result.is_ok(), "{result:?}");
        assert!(
            !marker.exists(),
            "a marker for a dataset that no longer exists has nothing to recover"
        );
    }

    /// A rollback that genuinely succeeds is the other case that drops the
    /// marker after a failed receive.
    #[tokio::test]
    async fn a_failed_restore_that_rolls_back_cleanly_drops_the_marker() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("restore.json");
        let mut progress = write_marker(&marker, "buddy-restore-kept");
        let (url, stub) = agent_stub(|method, path| {
            if method == "GET" && path.starts_with("/api/v1/snapshots/test/restored") {
                (200, "OK", r#"["test/restored@buddy-restore-kept"]"#)
            } else if (method == "POST" && path.starts_with("/api/v1/rollback/"))
                || (method == "DELETE" && path.starts_with("/api/v1/snapshots/test/restored"))
            {
                (200, "OK", "null")
            } else {
                (404, "Not Found", r#"{"error":"unexpected request"}"#)
            }
        })
        .await;
        let agent = crate::agent::Client::new(&url, "test", "").expect("agent client");

        let result = cleanup_failed_restore(
            &agent,
            "test/restored",
            &mut progress,
            &marker.display().to_string(),
        )
        .await;
        stub.abort();
        assert!(result.is_ok(), "{result:?}");
        assert!(!marker.exists(), "a clean rollback finishes the recovery");
    }

    /// A marker that lost its rollback point must not be mistaken for a stale
    /// success marker: the destination may be half-applied, so recovery
    /// refuses until an operator destroys or verifies it.
    #[tokio::test]
    async fn a_rollback_lost_marker_refuses_recovery_until_an_operator_acts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("restore.json");
        let mut progress = write_marker(&marker, "buddy-restore-gone");
        progress.rollback_lost = true;
        progress
            .save(&marker.display().to_string())
            .expect("save the marker");
        let (url, stub) = agent_stub(|method, path| {
            if method == "GET" && path == "/api/v1/datasets" {
                (200, "OK", r#"[{"name":"test/restored"}]"#)
            } else {
                (404, "Not Found", r#"{"error":"unexpected request"}"#)
            }
        })
        .await;
        let agent = crate::agent::Client::new(&url, "test", "").expect("agent client");

        let error = recover_interrupted_restore(
            &agent,
            "test/restored",
            &progress,
            &marker.display().to_string(),
        )
        .await
        .expect_err("a lost rollback point must not be restored over");
        stub.abort();
        assert!(error.contains("lost its rollback point"), "{error}");
        assert!(error.contains("test/restored"), "{error}");
        assert!(error.contains("destroy"), "{error}");
        assert!(marker.exists(), "only an operator clears this marker");
    }

    /// Destroying the dataset is the operator action that removes the wedge:
    /// recovery drops the rollback-lost marker and lets a fresh restore run.
    #[tokio::test]
    async fn a_rollback_lost_marker_is_dropped_when_the_dataset_is_gone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("restore.json");
        let mut progress = write_marker(&marker, "buddy-restore-gone");
        progress.rollback_lost = true;
        progress
            .save(&marker.display().to_string())
            .expect("save the marker");
        let (url, stub) = agent_stub(|method, path| {
            if method == "GET" && path == "/api/v1/datasets" {
                (200, "OK", "[]")
            } else {
                (404, "Not Found", r#"{"error":"unexpected request"}"#)
            }
        })
        .await;
        let agent = crate::agent::Client::new(&url, "test", "").expect("agent client");

        let result = recover_interrupted_restore(
            &agent,
            "test/restored",
            &progress,
            &marker.display().to_string(),
        )
        .await;
        stub.abort();
        assert!(result.is_ok(), "{result:?}");
        assert!(!marker.exists(), "a gone destination removes the wedge");
    }

    /// A snapshot-listing error against a gone dataset means there is nothing
    /// left to recover: drop the marker and let a fresh restore recreate it.
    #[tokio::test]
    async fn a_listing_error_with_a_gone_dataset_drops_the_marker() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("restore.json");
        let progress = write_marker(&marker, "buddy-restore-gone");
        let (url, stub) = agent_stub(|method, path| {
            if method == "GET" && path.starts_with("/api/v1/snapshots/test/restored") {
                (
                    500,
                    "Internal Server Error",
                    r#"{"error":"internal error (ref x)"}"#,
                )
            } else if method == "GET" && path == "/api/v1/datasets" {
                (200, "OK", "[]")
            } else {
                (404, "Not Found", r#"{"error":"unexpected request"}"#)
            }
        })
        .await;
        let agent = crate::agent::Client::new(&url, "test", "").expect("agent client");

        let result = recover_interrupted_restore(
            &agent,
            "test/restored",
            &progress,
            &marker.display().to_string(),
        )
        .await;
        stub.abort();
        assert!(result.is_ok(), "{result:?}");
        assert!(
            !marker.exists(),
            "nothing is left to recover without the dataset"
        );
    }

    /// A snapshot-listing error with the dataset still present keeps the
    /// marker: its state is unknown, so recovery must not proceed.
    #[tokio::test]
    async fn a_listing_error_with_an_existing_dataset_keeps_the_marker() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("restore.json");
        let progress = write_marker(&marker, "buddy-restore-gone");
        let (url, stub) = agent_stub(|method, path| {
            if method == "GET" && path.starts_with("/api/v1/snapshots/test/restored") {
                (
                    500,
                    "Internal Server Error",
                    r#"{"error":"internal error (ref x)"}"#,
                )
            } else if method == "GET" && path == "/api/v1/datasets" {
                (200, "OK", r#"[{"name":"test/restored"}]"#)
            } else {
                (404, "Not Found", r#"{"error":"unexpected request"}"#)
            }
        })
        .await;
        let agent = crate::agent::Client::new(&url, "test", "").expect("agent client");

        let error = recover_interrupted_restore(
            &agent,
            "test/restored",
            &progress,
            &marker.display().to_string(),
        )
        .await
        .expect_err("an unknown snapshot state must not be assumed gone");
        stub.abort();
        assert!(error.contains("kept"), "{error}");
        assert!(
            marker.exists(),
            "the marker must be kept while the state is unknown"
        );
    }

    /// The same dataset-gone fallback applies to the failed-receive cleanup.
    #[tokio::test]
    async fn a_cleanup_listing_error_with_a_gone_dataset_drops_the_marker() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("restore.json");
        let mut progress = write_marker(&marker, "buddy-restore-gone");
        let (url, stub) = agent_stub(|method, path| {
            if method == "GET" && path.starts_with("/api/v1/snapshots/test/restored") {
                (
                    500,
                    "Internal Server Error",
                    r#"{"error":"internal error (ref x)"}"#,
                )
            } else if method == "GET" && path == "/api/v1/datasets" {
                (200, "OK", "[]")
            } else {
                (404, "Not Found", r#"{"error":"unexpected request"}"#)
            }
        })
        .await;
        let agent = crate::agent::Client::new(&url, "test", "").expect("agent client");

        let result = cleanup_failed_restore(
            &agent,
            "test/restored",
            &mut progress,
            &marker.display().to_string(),
        )
        .await;
        stub.abort();
        assert!(result.is_ok(), "{result:?}");
        assert!(!marker.exists(), "a gone dataset leaves nothing to recover");
    }

    /// A cleanup listing error with the dataset still present keeps the
    /// marker and flags it as having no confirmed rollback point.
    #[tokio::test]
    async fn a_cleanup_listing_error_with_an_existing_dataset_keeps_the_marker() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = dir.path().join("restore.json");
        let mut progress = write_marker(&marker, "buddy-restore-gone");
        let (url, stub) = agent_stub(|method, path| {
            if method == "GET" && path.starts_with("/api/v1/snapshots/test/restored") {
                (
                    500,
                    "Internal Server Error",
                    r#"{"error":"internal error (ref x)"}"#,
                )
            } else if method == "GET" && path == "/api/v1/datasets" {
                (200, "OK", r#"[{"name":"test/restored"}]"#)
            } else {
                (404, "Not Found", r#"{"error":"unexpected request"}"#)
            }
        })
        .await;
        let agent = crate::agent::Client::new(&url, "test", "").expect("agent client");

        let error = cleanup_failed_restore(
            &agent,
            "test/restored",
            &mut progress,
            &marker.display().to_string(),
        )
        .await
        .expect_err("an unknown rollback state with a live destination must be reported");
        stub.abort();
        assert!(error.contains("half-applied"), "{error}");
        assert!(marker.exists(), "the marker must be kept for inspection");
        let reloaded =
            RestoreProgress::load(&marker.display().to_string()).expect("reload the marker");
        assert!(
            reloaded.rollback_lost,
            "the marker must record the missing rollback point"
        );
    }
}
