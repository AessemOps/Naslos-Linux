//! Cloud verify: prove the stored sequence is complete, and optionally
//! authenticate every chunk's plaintext.
//!
//! `structure` (the default) is LIST plus signed manifests plus DEK unwrap: it
//! costs no object-egress and reports every missing or wrongly-sized chunk.
//! `content` downloads every chunk, opens it with the chain DEK and compares
//! its SHA-256 to the signed manifest; nothing is written to disk. A scan never
//! stops at the first bad index: the UI gets the full set.

use std::collections::HashMap;
use std::sync::Arc;

use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use super::jobs::{self, CloudJob, JobKind, JobResult, JobState};
use super::s3::{Category, S3Storage};
use super::store::{AnchorPaths, LinkInfo};
use super::{chunk_key, identity_path, load_credentials, source_for_dataset};
use crate::buddy::envelope::{digest_of, open_chunk, unwrap_dek};
use crate::buddy::keys::load_identity;
use crate::state::AppState;

/// The verify body.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyRequest {
    #[serde(default)]
    pub destination: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub chain: String,
    #[serde(default)]
    pub mode: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyMode {
    Structure,
    Content,
}

impl VerifyMode {
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim() {
            "" | "structure" => Ok(Self::Structure),
            "content" => Ok(Self::Content),
            other => Err(format!(
                "mode must be \"structure\" or \"content\", not {other:?}"
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub enum VerifyError {
    NotFound(String),
    BadRequest(String),
    Precondition(String),
    Conflict(String),
    ServiceUnavailable(String),
    Upstream(String),
}

/// Start a verify job.
pub async fn enqueue(
    state: &Arc<AppState>,
    request: VerifyRequest,
    mode: VerifyMode,
) -> Result<String, VerifyError> {
    let cloud = state.cloud.as_ref().ok_or_else(|| {
        VerifyError::ServiceUnavailable("cloud backup is not enabled".to_string())
    })?;
    cloud
        .destinations
        .require_loaded()
        .map_err(VerifyError::ServiceUnavailable)?;
    let destination = cloud
        .destinations
        .get(&request.destination)
        .ok_or_else(|| {
            VerifyError::NotFound(format!("destination {:?} not found", request.destination))
        })?;
    if !destination.enabled {
        return Err(VerifyError::BadRequest(format!(
            "destination {:?} is disabled",
            destination.name
        )));
    }
    let source = source_for_dataset(&request.source).map_err(VerifyError::BadRequest)?;
    let credentials = load_credentials(state, &destination)
        .await
        .map_err(VerifyError::Precondition)?;
    let identity = load_identity(&identity_path()).map_err(VerifyError::Precondition)?;
    let s3 = S3Storage::build(
        &destination,
        &credentials,
        &super::net::NetConfig::from_env(),
    )
    .map_err(|e| VerifyError::Upstream(e.to_string()))?;
    let all = super::store::list_links(&s3, &source, &identity)
        .await
        .map_err(VerifyError::Upstream)?;
    if all.is_empty() {
        return Err(VerifyError::NotFound(format!(
            "no backup of {:?} is stored on this destination",
            request.source
        )));
    }
    let target = if request.chain.trim().is_empty() {
        None
    } else {
        Some(request.chain.trim())
    };
    let links = super::store::select_links(&all, target).map_err(VerifyError::BadRequest)?;
    let anchor = AnchorPaths::new(&cloud.source_state_dir(&destination.id), &source).anchor;
    let last = super::store::last_published(&anchor).map_err(VerifyError::Upstream)?;
    super::store::validate_sequence(&source, &links, target, last)
        .map_err(VerifyError::BadRequest)?;

    let id = jobs::random_job_id();
    let (job, token) = jobs::new_job(
        id.clone(),
        JobKind::Verify,
        destination.id.clone(),
        source.clone(),
        String::new(),
    );
    if let Err(conflict) = cloud.jobs.add_if_free(
        &id,
        job.clone(),
        &destination.id,
        &source,
        "",
        JobKind::Verify,
    ) {
        return Err(VerifyError::Conflict(match conflict {
            jobs::EnqueueConflict::Source => {
                format!("another job for {source} on this destination is running")
            }
            jobs::EnqueueConflict::Dataset => "another restore is running".to_string(),
        }));
    }
    let state_for_task = state.clone();
    tokio::spawn(async move {
        run_verify_job(state_for_task, job, token, request, mode, links).await;
    });
    Ok(id)
}

async fn run_verify_job(
    state: Arc<AppState>,
    job: Arc<CloudJob>,
    token: CancellationToken,
    request: VerifyRequest,
    mode: VerifyMode,
    links: Vec<LinkInfo>,
) {
    let identity = match load_identity(&identity_path()) {
        Ok(identity) => identity,
        Err(e) => {
            job.finish(JobState::Failed, e, None);
            return;
        }
    };
    let Some(cloud) = state.cloud.as_ref() else {
        job.finish(
            JobState::Failed,
            "cloud backup is not enabled".to_string(),
            None,
        );
        return;
    };
    let Some(destination) = cloud.destinations.get(&request.destination) else {
        job.finish(
            JobState::Failed,
            "the destination was removed".to_string(),
            None,
        );
        return;
    };
    let credentials = match load_credentials(&state, &destination).await {
        Ok(credentials) => credentials,
        Err(e) => {
            job.finish(JobState::Failed, e, None);
            return;
        }
    };
    let s3 = match S3Storage::build(
        &destination,
        &credentials,
        &super::net::NetConfig::from_env(),
    ) {
        Ok(s3) => Arc::new(s3),
        Err(e) => {
            job.finish(JobState::Failed, e.to_string(), None);
            return;
        }
    };

    let mut missing: HashMap<String, Vec<i64>> = HashMap::new();
    let mut bad: HashMap<String, Vec<i64>> = HashMap::new();
    let mut plain_bytes = 0i64;
    let total_chunks: i64 = links.iter().map(|l| l.manifest.chunks.len() as i64).sum();
    job.update_progress(|p| p.total_chunks = Some(total_chunks));
    let mut checked = 0i64;

    for (position, link) in links.iter().enumerate() {
        if token.is_cancelled() {
            job.finish(JobState::Cancelled, "cancelled".to_string(), None);
            return;
        }
        job.update_progress(|p| {
            p.link = Some(position + 1);
            p.links = Some(links.len());
        });
        let kek = match identity.kek_bytes() {
            Ok(kek) => kek,
            Err(e) => {
                job.finish(JobState::Failed, e, None);
                return;
            }
        };
        let dek = match unwrap_dek(
            &kek,
            &link.manifest.dek_wrapped,
            &link.manifest.source,
            &link.manifest.chain,
        ) {
            Ok(dek) => dek,
            Err(e) => {
                job.finish(JobState::Failed, format!("chain {}: {e}", link.chain), None);
                return;
            }
        };
        let prefix = match link.manifest.stream_prefix_bytes() {
            Ok(prefix) => prefix,
            Err(e) => {
                job.finish(JobState::Failed, format!("chain {}: {e}", link.chain), None);
                return;
            }
        };
        let link_dir = super::link_prefix(
            &s3.root_prefix(),
            &link.manifest.source,
            &link.lineage,
            link.sequence,
            &link.chain,
        );
        // LIST once per link: structure stops here; content skips known
        // missing chunks but still GETs every present one.
        let objects = match s3.list(&format!("{link_dir}/chunks/")).await {
            Ok(objects) => objects,
            Err(e) => {
                job.finish(JobState::Failed, e.to_string(), None);
                return;
            }
        };
        let mut sizes: HashMap<i64, u64> = HashMap::new();
        for object in objects {
            let key = object.location.as_ref();
            if let Some(super::ParsedLinkKey {
                kind: super::ParsedKeyKind::Chunk { index, generation },
                ..
            }) = super::parse_link_key(&s3.source_root(&link.manifest.source), key)
            {
                // A chunk object at another generation is a superseded tail;
                // the manifest names the live one.
                let live = link
                    .manifest
                    .chunks
                    .iter()
                    .find(|c| c.index == index)
                    .is_some_and(|c| c.generation == generation);
                if live {
                    sizes.insert(index, object.size);
                }
            }
        }
        let mut link_bad: Vec<i64> = Vec::new();
        let mut link_missing: Vec<i64> = Vec::new();
        for chunk in &link.manifest.chunks {
            if token.is_cancelled() {
                job.finish(JobState::Cancelled, "cancelled".to_string(), None);
                return;
            }
            checked += 1;
            job.update_progress(|p| {
                p.chunk = checked;
                p.plain_bytes = plain_bytes;
            });
            let size = sizes.get(&chunk.index).copied();
            let size_ok = size == Some(chunk.sealed_bytes as u64);
            if size.is_none() {
                link_missing.push(chunk.index);
                if mode == VerifyMode::Content {
                    continue;
                }
            } else if !size_ok {
                link_bad.push(chunk.index);
                if mode == VerifyMode::Content {
                    // A wrong-sized object can still decrypt; try it so a
                    // single pass reports hash failures too.
                }
            }
            if mode == VerifyMode::Structure {
                continue;
            }
            let key = chunk_key(&link_dir, chunk.index, chunk.generation);
            let sealed = match s3.get(&key).await {
                Ok(sealed) => sealed,
                Err(e) if e.category == Category::NotFound => {
                    if !link_missing.contains(&chunk.index) {
                        link_missing.push(chunk.index);
                    }
                    continue;
                }
                Err(e) => {
                    job.finish(JobState::Failed, e.to_string(), None);
                    return;
                }
            };
            match open_chunk(
                &dek,
                &prefix,
                chunk.generation,
                &link.manifest.source,
                &link.manifest.chain,
                chunk.index,
                &sealed,
            ) {
                Ok(plain) => {
                    if digest_of(&plain) != chunk.sha256_plain {
                        link_bad.push(chunk.index);
                    } else {
                        plain_bytes += plain.len() as i64;
                    }
                }
                Err(_) => link_bad.push(chunk.index),
            }
        }
        link_missing.sort_unstable();
        link_missing.dedup();
        link_bad.sort_unstable();
        link_bad.dedup();
        if !link_missing.is_empty() {
            missing.insert(link.chain.clone(), link_missing);
        }
        if !link_bad.is_empty() {
            bad.insert(link.chain.clone(), link_bad);
        }
    }

    // The target restore point is the last link in the oldest-first sequence.
    let target = target_link(&links);
    let result = JobResult {
        chain: target.map(|l| l.chain.clone()).unwrap_or_default(),
        snapshot: target
            .map(|l| l.manifest.to_snapshot.clone())
            .unwrap_or_default(),
        kind: target
            .map(|l| {
                if l.incremental() {
                    "incremental"
                } else {
                    "full"
                }
                .to_string()
            })
            .unwrap_or_default(),
        plain_bytes,
        missing_indices: missing.clone(),
        bad_indices: bad.clone(),
        ..Default::default()
    };
    if missing.is_empty() && bad.is_empty() {
        job.finish(JobState::Succeeded, String::new(), Some(result));
    } else {
        let error = format!(
            "verification found {} missing and {} corrupt chunk indices",
            missing.values().map(Vec::len).sum::<usize>(),
            bad.values().map(Vec::len).sum::<usize>(),
        );
        job.finish(JobState::Failed, error, Some(result));
    }
}

/// The verified restore point is the newest link in the apply sequence.
fn target_link(links: &[LinkInfo]) -> Option<&LinkInfo> {
    links.last()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_result_target_is_the_newest_link() {
        let links = vec![
            crate::cloud::store::test_link("lin", 1, "a", ""),
            crate::cloud::store::test_link("lin", 2, "b", "snap-a"),
        ];
        assert_eq!(target_link(&links).unwrap().chain, "b");
        assert!(target_link(&[]).is_none());
    }

    #[test]
    fn the_mode_is_strict_and_defaults_to_structure() {
        assert_eq!(VerifyMode::parse("").unwrap(), VerifyMode::Structure);
        assert_eq!(
            VerifyMode::parse("structure").unwrap(),
            VerifyMode::Structure
        );
        assert_eq!(VerifyMode::parse("content").unwrap(), VerifyMode::Content);
        assert!(VerifyMode::parse("deep").is_err());
    }
}
