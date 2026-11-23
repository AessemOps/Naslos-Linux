//! Buddy restore (port of the restore half of `server/buddy_send.go`).
//!
//! Restoring verifies the stream before ZFS sees it: the client checks the
//! manifest signature, the chain's contiguity and every chunk's digest, so a
//! restore either lands whole or fails loudly.

use std::sync::Arc;

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
struct ShaWriter {
    hasher: sha2::Sha256,
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

    // A previous attempt may have died mid-restore. Roll the destination back
    // to the snapshot taken before that attempt, then start over: a partially
    // applied receive cannot be resumed safely (R-059).
    let progress_path = restore_progress_path(&source);
    if let Some(prev) = RestoreProgress::load(&progress_path) {
        if prev.dataset == dataset && prev.receiver == receiver && prev.source == source {
            if !prev.snapshot.is_empty() {
                if let Err(e) = agent.rollback_snapshot(&dataset, &prev.snapshot).await {
                    tracing::warn!(
                        "buddy restore: cannot roll {dataset} back to {} after an interrupted restore: {e}",
                        prev.snapshot
                    );
                }
                let _ = agent.destroy_snapshot(&dataset, &prev.snapshot).await;
            }
        }
        let _ = std::fs::remove_file(&progress_path);
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
            if !progress.snapshot.is_empty() {
                if let Err(e) = agent.rollback_snapshot(&dataset, &progress.snapshot).await {
                    tracing::warn!(
                        "buddy restore: cannot roll {dataset} back to {}: {e}",
                        progress.snapshot
                    );
                }
                let _ = agent.destroy_snapshot(&dataset, &progress.snapshot).await;
            }
            let _ = std::fs::remove_file(&progress_path);
            return json(status, serde_json::json!({ "error": error }));
        }
    };
    if !progress.snapshot.is_empty() {
        if let Err(e) = agent.destroy_snapshot(&dataset, &progress.snapshot).await {
            tracing::warn!(
                "buddy restore: cannot destroy the pre-restore snapshot {}: {e}",
                progress.snapshot
            );
        }
    }
    let _ = std::fs::remove_file(&progress_path);

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
struct RestoreProgress {
    dataset: String,
    source: String,
    receiver: String,
    snapshot: String,
    applied_chains: Vec<String>,
    chunks: usize,
    plain_bytes: i64,
}

impl RestoreProgress {
    fn load(path: &str) -> Option<Self> {
        let data = std::fs::read(path).ok()?;
        serde_json::from_slice(&data).ok()
    }

    fn save(&self, path: &str) -> Result<(), String> {
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

fn restore_progress_path(source: &str) -> String {
    let digest = sha2::Sha256::digest(source.as_bytes());
    format!("{}/restore-{}.json", sequence_dir(), hex(&digest[..8]))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::{restore_progress_path, RestoreProgress};

    #[test]
    fn restore_progress_round_trips_and_is_keyed_per_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("restore.json");
        let path = path.to_str().unwrap();
        assert_ne!(
            restore_progress_path("naslos-a/data"),
            restore_progress_path("naslos-a/other")
        );
        let progress = RestoreProgress {
            dataset: "test/restored".to_string(),
            source: "naslos-a/data".to_string(),
            receiver: "https://naslos-b".to_string(),
            snapshot: "buddy-restore-1-abcd".to_string(),
            applied_chains: vec!["aaaa".to_string(), "bbbb".to_string()],
            chunks: 7,
            plain_bytes: 4096,
        };
        progress.save(path).unwrap();
        let loaded = RestoreProgress::load(path).unwrap();
        assert_eq!(loaded.dataset, "test/restored");
        assert_eq!(loaded.applied_chains.len(), 2);
        assert_eq!(loaded.chunks, 7);
        let raw = std::fs::read_to_string(path).unwrap();
        assert!(raw.contains("appliedChains"), "{raw}");
    }
}
