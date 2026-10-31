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
            return json(StatusCode::BAD_GATEWAY, serde_json::json!({ "error": e }));
        }
        match wait {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                return json(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    serde_json::json!({ "error": format!("zfs receive refused chain {}: {e}", chain.chain) }),
                )
            }
            Err(e) => {
                return json(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    serde_json::json!({ "error": format!("zfs receive task failed: {e}") }),
                )
            }
        }
        applied.push(chain.chain.clone());
        total_chunks += result.chunks;
        total_bytes += result.plain_bytes;
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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
