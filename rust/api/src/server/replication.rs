//! Local ZFS replication (FR-REP): copy a dataset to another dataset on the
//! same node through the agent's streaming `zfs send`/`zfs receive`.
//!
//! A job snapshots the source, picks the newest common snapshot GUID as the
//! incremental base (never a name, so a renamed snapshot still matches), and
//! pipes the stream from the agent's send endpoint straight into its receive
//! endpoint. Jobs are server-owned and in memory, like Buddy sends: a client
//! disconnect does not stop a transfer, and a restart abandons running jobs
//! (the received snapshots remain the durable outcome).
//!
//! Retention is deliberately narrow: `pruneKeep` destroys only destination
//! snapshots carrying this source|destination pair's replication prefix. A
//! user snapshot and another pair's replica are never removed, and source
//! snapshots are left in place for the operator's own snapshot policy.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;

use crate::agent::{Dataset, SendStreamOptions, SnapshotInfo};
use crate::dataset_encryption::{self, RawKey};
use crate::json;
use crate::state::AppState;

/// Every snapshot this feature creates starts with this prefix.
pub const SNAPSHOT_PREFIX: &str = "naslos-rep-";

/// Upper bound on retained destination snapshots per pair.
pub const MAX_PRUNE_KEEP: usize = 1000;

fn is_encrypted(dataset: &Dataset) -> bool {
    !dataset.encryption.is_empty() && dataset.encryption != "off" && dataset.encryption != "-"
}

fn encryption_root_of(dataset: &Dataset) -> String {
    if dataset.encryption_root.is_empty() {
        dataset.name.clone()
    } else {
        dataset.encryption_root.clone()
    }
}

/// How encryption constrains a replication pair (FR-ENC-07).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncryptionPlan {
    /// Neither side is encrypted: the existing property-less stream.
    Plain,
    /// An encrypted source goes raw (`zfs send -w`) into a destination the job
    /// creates. A raw full stream cannot overwrite an existing dataset, and
    /// `zfs receive -F` is refused for encrypted datasets.
    RawNew { source_root: String },
    /// An existing raw replica receives an incremental with the same key.
    RawIncremental { source_root: String },
}

impl EncryptionPlan {
    pub fn is_raw(&self) -> bool {
        !matches!(self, EncryptionPlan::Plain)
    }

    pub fn creates_destination(&self) -> bool {
        matches!(self, EncryptionPlan::RawNew { .. })
    }

    pub fn source_root(&self) -> Option<&str> {
        match self {
            EncryptionPlan::Plain => None,
            EncryptionPlan::RawNew { source_root }
            | EncryptionPlan::RawIncremental { source_root } => Some(source_root),
        }
    }
}

/// Decide how encryption affects a pair from the agent's dataset list.
///
/// The rules follow OpenZFS: a raw full receive always creates the destination
/// (an existing destination needs `-F`, which ZFS refuses for encrypted
/// datasets), a raw stream cannot land on an unencrypted dataset, and a raw
/// chain keeps the source key, so a replica whose key differs must be refused
/// rather than mixed.
pub fn plan_encryption(
    datasets: &[Dataset],
    source: &str,
    destination: &str,
    recursive: bool,
) -> Result<EncryptionPlan, String> {
    let src = datasets
        .iter()
        .find(|d| d.name == source)
        .ok_or_else(|| format!("source dataset {source:?} does not exist"))?;
    let dst = datasets.iter().find(|d| d.name == destination);
    if !is_encrypted(src) {
        if let Some(dst) = dst {
            if is_encrypted(dst) {
                return Err(format!(
                    "destination {destination:?} is encrypted but the source is not; plain and raw replication are never mixed"
                ));
            }
        }
        return Ok(EncryptionPlan::Plain);
    }

    let source_root = encryption_root_of(src);
    if recursive {
        let prefix = format!("{source}/");
        let foreign_root = datasets
            .iter()
            .filter(|d| d.name == source || d.name.starts_with(&prefix))
            .filter(|d| is_encrypted(d))
            .map(encryption_root_of)
            .find(|root| root != &source_root);
        if let Some(root) = foreign_root {
            return Err(format!(
                "recursive raw replication requires one encryption root; the source tree also uses {root:?}. Replicate that dataset separately"
            ));
        }
    }

    match dst {
        Some(dst) => {
            if !is_encrypted(dst) {
                return Err(format!(
                    "destination {destination:?} exists and is not encrypted; a raw stream cannot be received on top of an unencrypted dataset. Destroy it or choose a new destination"
                ));
            }
            if encryption_root_of(dst) != dst.name {
                return Err(format!(
                    "destination {destination:?} inherits its key from {:?}; raw replication needs a dedicated encrypted dataset",
                    encryption_root_of(dst)
                ));
            }
            Ok(EncryptionPlan::RawIncremental { source_root })
        }
        None => {
            let parent = destination
                .rsplit_once('/')
                .map(|(parent, _)| parent)
                .ok_or_else(|| format!("destination {destination:?} must be <pool>/<name>"))?;
            if !datasets.iter().any(|d| d.name == parent) {
                return Err(format!(
                    "destination parent {parent:?} does not exist; encrypted replication creates the destination, so its parent dataset must already exist"
                ));
            }
            Ok(EncryptionPlan::RawNew { source_root })
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum JobState {
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Default, Serialize)]
struct ProgressView {
    bytes: i64,
    #[serde(rename = "estimatedBytes")]
    estimated_bytes: i64,
}

#[derive(Debug, Clone, Default, Serialize)]
struct ReplicationResult {
    status: String,
    source: String,
    destination: String,
    snapshot: String,
    #[serde(rename = "baseSnapshot", skip_serializing_if = "String::is_empty")]
    base_snapshot: String,
    #[serde(rename = "baseGUID", skip_serializing_if = "String::is_empty")]
    base_guid: String,
    #[serde(rename = "toGUID", skip_serializing_if = "String::is_empty")]
    to_guid: String,
    incremental: bool,
    recursive: bool,
    raw: bool,
    bytes: i64,
    #[serde(rename = "estimatedBytes")]
    estimated_bytes: i64,
    #[serde(rename = "prunedSnapshots")]
    pruned_snapshots: usize,
    #[serde(skip_serializing_if = "String::is_empty")]
    mountpoint: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
    #[serde(rename = "retentionError", skip_serializing_if = "String::is_empty")]
    retention_error: String,
    #[serde(rename = "durationSeconds")]
    duration_seconds: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct JobPublic {
    id: String,
    source: String,
    destination: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    snapshot: String,
    recursive: bool,
    /// The stream is raw (`zfs send -w`) for an encrypted source.
    raw: bool,
    #[serde(rename = "pruneKeep", skip_serializing_if = "is_zero_usize")]
    prune_keep: usize,
    state: JobState,
    #[serde(rename = "startedAt")]
    started_at: DateTime<Utc>,
    #[serde(rename = "finishedAt", skip_serializing_if = "Option::is_none")]
    finished_at: Option<DateTime<Utc>>,
    progress: ProgressView,
    incremental: bool,
    #[serde(rename = "estimatedBytes", skip_serializing_if = "is_zero_i64")]
    estimated_bytes: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<ReplicationResult>,
    #[serde(skip_serializing_if = "String::is_empty")]
    error: String,
}

impl JobPublic {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn destination(&self) -> &str {
        &self.destination
    }

    pub fn is_running(&self) -> bool {
        self.state == JobState::Running
    }
}

fn is_zero_usize(n: &usize) -> bool {
    *n == 0
}

fn is_zero_i64(n: &i64) -> bool {
    *n == 0
}

pub struct ReplicationJob {
    public: Mutex<JobPublic>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl ReplicationJob {
    fn snapshot(&self) -> JobPublic {
        self.public.lock().unwrap().clone()
    }

    fn state(&self) -> JobState {
        self.public.lock().unwrap().state
    }
}

type JobMap = HashMap<String, Arc<ReplicationJob>>;
type JobOrder = Vec<String>;

/// Keeps running jobs plus the last ~20 finished ones.
#[derive(Default)]
pub struct ReplicationJobManager {
    inner: Mutex<(JobMap, JobOrder)>,
}

impl ReplicationJobManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn conflicting(&self, destination: &str) -> Option<JobPublic> {
        let inner = self.inner.lock().unwrap();
        for job in inner.0.values() {
            if job.state() == JobState::Running
                && destinations_overlap(&job.snapshot().destination, destination)
            {
                return Some(job.snapshot());
            }
        }
        None
    }

    /// Register a job, refusing a destination that an in-flight job is already
    /// writing (or one nested under it). The check and the insert share one
    /// lock so two requests cannot both pass.
    #[allow(clippy::result_large_err)]
    fn add_if_free(&self, id: &str, job: Arc<ReplicationJob>) -> Result<(), JobPublic> {
        let mut inner = self.inner.lock().unwrap();
        let destination = job.snapshot().destination;
        for existing in inner.0.values() {
            let p = existing.snapshot();
            if existing.state() == JobState::Running
                && destinations_overlap(&p.destination, &destination)
            {
                return Err(p);
            }
        }
        Self::insert_locked(&mut inner, id, job);
        Ok(())
    }

    fn insert_locked(inner: &mut (JobMap, JobOrder), id: &str, job: Arc<ReplicationJob>) {
        inner.0.insert(id.to_string(), job);
        inner.1.push(id.to_string());
        let mut kept: Vec<String> = Vec::new();
        let mut finished = 0;
        for id in inner.1.iter().rev() {
            if let Some(j) = inner.0.get(id) {
                if j.state() == JobState::Running || finished < 20 {
                    kept.push(id.clone());
                    if j.state() != JobState::Running {
                        finished += 1;
                    }
                }
            }
        }
        kept.reverse();
        inner.0.retain(|id, _| kept.contains(id));
        inner.1 = kept;
    }

    /// Test-only seam: record a finished job so the ownership guards can be
    /// exercised without running a stream.
    #[cfg(test)]
    pub(crate) fn remember_for_test(&self, source: &str, destination: &str) {
        self.remember_with_state_for_test(source, destination, JobState::Succeeded);
    }

    /// Test-only seam: a job that is still writing, which must block a
    /// dataset destroy.
    #[cfg(test)]
    pub(crate) fn remember_running_for_test(&self, source: &str, destination: &str) {
        self.remember_with_state_for_test(source, destination, JobState::Running);
    }

    #[cfg(test)]
    fn remember_with_state_for_test(&self, source: &str, destination: &str, state: JobState) {
        let now = Utc::now();
        let finished = (state != JobState::Running).then_some(now);
        let public = JobPublic {
            id: "test-job".to_string(),
            source: source.to_string(),
            destination: destination.to_string(),
            snapshot: String::new(),
            recursive: false,
            raw: false,
            prune_keep: 0,
            state,
            started_at: now,
            finished_at: finished,
            progress: ProgressView::default(),
            incremental: false,
            estimated_bytes: 0,
            result: None,
            error: String::new(),
        };
        let job = Arc::new(ReplicationJob {
            public: Mutex::new(public),
            handle: Mutex::new(None),
        });
        let mut inner = self.inner.lock().unwrap();
        Self::insert_locked(&mut inner, "test-job", job);
    }

    pub fn get(&self, id: &str) -> Option<Arc<ReplicationJob>> {
        self.inner.lock().unwrap().0.get(id).cloned()
    }

    pub fn list(&self) -> Vec<JobPublic> {
        let inner = self.inner.lock().unwrap();
        inner
            .1
            .iter()
            .filter_map(|id| inner.0.get(id).map(|j| j.snapshot()))
            .collect()
    }
}

/// Two destinations conflict when they are the same dataset or one contains
/// the other: receiving into a parent and a child at once would interleave two
/// writes in one hierarchy.
pub fn destinations_overlap(a: &str, b: &str) -> bool {
    a == b || a.starts_with(&format!("{b}/")) || b.starts_with(&format!("{a}/"))
}

/// Refuse identical or nested source/destination pairs. A destination inside
/// the source (or vice versa) would make a recursive send and its receive
/// overlap and can overwrite the live dataset's hierarchy.
pub fn validate_pair(source: &str, destination: &str) -> Result<(), String> {
    crate::server::zfs::validate_dataset_path(source)?;
    crate::server::zfs::validate_dataset_path(destination)?;
    if source == destination {
        return Err("source and destination must be different datasets".to_string());
    }
    if destination.starts_with(&format!("{source}/")) {
        return Err(format!(
            "destination {destination:?} is inside the source {source:?}; choose a dataset outside the source hierarchy"
        ));
    }
    if source.starts_with(&format!("{destination}/")) {
        return Err(format!(
            "source {source:?} is inside the destination {destination:?}; choose a destination outside the source hierarchy"
        ));
    }
    Ok(())
}

/// An injective prefix token for one source|destination pair.
pub fn replication_token(source: &str, destination: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(source.as_bytes());
    h.update(b"|");
    h.update(destination.as_bytes());
    let digest = h.finalize();
    digest[..16].iter().map(|b| format!("{b:02x}")).collect()
}

/// The prefix every destination snapshot this pair creates starts with.
pub fn snapshot_prefix(source: &str, destination: &str) -> String {
    format!(
        "{SNAPSHOT_PREFIX}{}-",
        replication_token(source, destination)
    )
}

/// A replication-owned snapshot name: `naslos-rep-<token>-<UTC timestamp>`.
pub fn snapshot_name(source: &str, destination: &str, now: DateTime<Utc>) -> String {
    format!(
        "{}{}",
        snapshot_prefix(source, destination),
        now.format("%Y%m%dT%H%M%SZ")
    )
}

/// What the send has to do, from the source's snapshot list (creation order)
/// and the destination's. Matching is by GUID so a snapshot that was renamed
/// on either side still counts as the same point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendPlan {
    UpToDate,
    Full,
    Incremental { base: SnapshotInfo },
}

pub fn plan_send(
    source: &[SnapshotInfo],
    destination: &[SnapshotInfo],
    target: &str,
) -> Result<SendPlan, String> {
    let position = source
        .iter()
        .position(|s| s.name == target)
        .ok_or_else(|| format!("snapshot {target:?} does not exist on the source"))?;
    let target = &source[position];
    if !target.guid.is_empty()
        && destination
            .iter()
            .any(|s| !s.guid.is_empty() && s.guid == target.guid)
    {
        return Ok(SendPlan::UpToDate);
    }
    let known: HashSet<&str> = destination
        .iter()
        .map(|s| s.guid.as_str())
        .filter(|g| !g.is_empty())
        .collect();
    for candidate in source[..position].iter().rev() {
        if !candidate.guid.is_empty() && known.contains(candidate.guid.as_str()) {
            return Ok(SendPlan::Incremental {
                base: candidate.clone(),
            });
        }
    }
    Ok(SendPlan::Full)
}

/// The source snapshots a recursive send covers, as `(dataset, guid)` pairs,
/// shallowest first. A sibling whose name merely shares the source prefix is
/// not included.
pub fn expected_recursive_snapshots(
    source_snapshots: &[SnapshotInfo],
    source: &str,
    target: &str,
) -> Vec<(String, String)> {
    let mut expected: Vec<(String, String)> = source_snapshots
        .iter()
        .filter(|snapshot| snapshot.name == target && !snapshot.guid.is_empty())
        .filter_map(|snapshot| {
            let relative = snapshot.dataset.strip_prefix(source)?;
            if relative.is_empty() || relative.starts_with('/') {
                Some((snapshot.dataset.clone(), snapshot.guid.clone()))
            } else {
                None
            }
        })
        .collect();
    expected.sort_by_key(|(dataset, _)| dataset.matches('/').count());
    expected
}

/// Recursive source snapshots whose GUID is not present on the destination,
/// reduced to the top-most missing subtree so one send covers every descendant
/// below it. A same-named child snapshot with a different GUID is not a match.
pub fn missing_recursive_replicas(
    expected: &[(String, String)],
    destination_snapshots: &[SnapshotInfo],
    source: &str,
    destination: &str,
) -> Vec<String> {
    let mut guids: HashMap<&str, HashSet<&str>> = HashMap::new();
    for snapshot in destination_snapshots {
        guids
            .entry(snapshot.dataset.as_str())
            .or_default()
            .insert(snapshot.guid.as_str());
    }
    let mut missing: Vec<String> = Vec::new();
    for (dataset, guid) in expected {
        let Some(relative) = dataset.strip_prefix(source) else {
            continue;
        };
        if !relative.is_empty() && !relative.starts_with('/') {
            continue;
        }
        let replica = format!("{destination}{relative}");
        if guids
            .get(replica.as_str())
            .is_some_and(|set| set.contains(guid.as_str()))
        {
            continue;
        }
        if missing
            .iter()
            .any(|parent| dataset.starts_with(&format!("{parent}/")))
        {
            continue;
        }
        missing.push(dataset.clone());
    }
    missing
}

/// Verify a recursive replica and return the top-most subtree sends needed to
/// repair it. Two recursive GUID listings cover the whole tree, so a renamed
/// snapshot is matched by GUID and a missing child dataset is found without a
/// call per descendant.
async fn recursive_repair_datasets(
    agent: &crate::agent::Client,
    source: &str,
    destination: &str,
    target: &str,
) -> Result<Vec<String>, String> {
    let source_snapshots = agent
        .snapshots_with_guid(source)
        .await
        .map_err(|e| format!("listing recursive source snapshots: {e}"))?;
    let destination_snapshots = agent
        .snapshots_with_guid(destination)
        .await
        .map_err(|e| format!("listing recursive destination snapshots: {e}"))?;
    if source_snapshots
        .iter()
        .any(|snapshot| snapshot.name == target && snapshot.dataset.is_empty())
    {
        return Err(
            "the agent did not report which dataset owns a snapshot; update the agent".to_string(),
        );
    }
    Ok(missing_recursive_replicas(
        &expected_recursive_snapshots(&source_snapshots, source, target),
        &destination_snapshots,
        source,
        destination,
    ))
}

/// Plan retention for one pair: from the destination's full snapshot paths,
/// return the `(dataset, snapshot)` pairs to destroy so each dataset keeps the
/// newest `keep` snapshots carrying `prefix`. Names without the prefix (user
/// snapshots, another pair's replica) are never selected.
pub fn prune_plan(
    paths: &[String],
    root: &str,
    prefix: &str,
    keep: usize,
) -> Vec<(String, String)> {
    if keep == 0 {
        return Vec::new();
    }
    let mut totals: HashMap<String, usize> = HashMap::new();
    for path in paths {
        let Some((dataset, snap)) = path.split_once('@') else {
            continue;
        };
        if dataset != root && !dataset.starts_with(&format!("{root}/")) {
            continue;
        }
        if snap.starts_with(prefix) {
            *totals.entry(dataset.to_string()).or_insert(0) += 1;
        }
    }

    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut delete = Vec::new();
    // Creation order is oldest first: the first `total - keep` entries of a
    // dataset are the oldest and are the ones retention removes.
    for path in paths {
        let Some((dataset, snap)) = path.split_once('@') else {
            continue;
        };
        let Some(total) = totals.get(dataset) else {
            continue;
        };
        if !snap.starts_with(prefix) {
            continue;
        }
        let count = seen.entry(dataset.to_string()).or_insert(0);
        *count += 1;
        if *count <= total.saturating_sub(keep) {
            delete.push((dataset.to_string(), snap.to_string()));
        }
    }
    delete
}

/// Why a full stream must not replace this destination, if anything. `-F` is
/// only allowed when every existing destination snapshot belongs to this pair
/// and no child dataset exists (a recursive `-F` would destroy datasets missing
/// on the source).
pub fn replacement_blocker(
    snapshot_paths: &[String],
    dataset_names: &[String],
    destination: &str,
    prefix: &str,
) -> Option<String> {
    if let Some((_, snap)) = snapshot_paths
        .iter()
        .filter_map(|p| p.split_once('@'))
        .find(|(_, snap)| !snap.starts_with(prefix))
    {
        return Some(format!(
            "destination {destination} holds snapshot {snap:?} that replication did not create; refusing to overwrite it. Remove that snapshot or choose another destination"
        ));
    }
    if dataset_names
        .iter()
        .any(|name| name.starts_with(&format!("{destination}/")))
    {
        return Some(format!(
            "destination {destination} has child datasets; a full replication stream could replace them. Destroy the replica dataset first"
        ));
    }
    None
}

async fn replacement_gate(
    agent: &crate::agent::Client,
    dataset: &str,
    prefix: &str,
    dataset_names: &[String],
) -> Result<(), String> {
    let paths = agent
        .snapshot_paths(dataset)
        .await
        .map_err(|e| format!("listing destination snapshots: {e}"))?;
    match replacement_blocker(&paths, dataset_names, dataset, prefix) {
        Some(reason) => Err(reason),
        None => Ok(()),
    }
}

async fn destination_can_be_replaced(
    state: &AppState,
    agent: &crate::agent::Client,
    destination: &str,
    prefix: &str,
) -> Result<(), String> {
    let datasets = agent
        .list_datasets()
        .await
        .map_err(|e| format!("listing datasets: {e}"))?;
    // A forced full receive replaces the destination exactly like a destroy:
    // hold it to the same share/app/VM ownership guards (S-9).
    crate::server::zfs::dataset_consumer_conflict(state, destination, &datasets)?;
    let names: Vec<String> = datasets.into_iter().map(|d| d.name).collect();
    replacement_gate(agent, destination, prefix, &names).await
}

/// Pipe one agent send stream into one receive call and add its bytes to the
/// job progress. Returns the bytes streamed plus the agent's receive report
/// (mountpoint and warnings).
#[allow(clippy::too_many_arguments)]
async fn stream_one(
    agent: &crate::agent::Client,
    job: &Arc<ReplicationJob>,
    opts: SendStreamOptions,
    destination: &str,
    force: bool,
    raw: bool,
    unlock_key: Option<&[u8]>,
    progress: &Arc<AtomicI64>,
) -> Result<(i64, crate::agent::ReceiveOutcome), String> {
    let before = progress.load(Ordering::Relaxed);
    let stream = agent
        .send_stream(&opts)
        .await
        .map_err(|e| format!("starting zfs send: {e}"))?;
    let progress_for_stream = progress.clone();
    let job_for_stream = job.clone();
    let stream = stream.map(move |item| {
        if let Ok(bytes) = &item {
            let total = progress_for_stream.fetch_add(bytes.len() as i64, Ordering::Relaxed)
                + bytes.len() as i64;
            if let Ok(mut p) = job_for_stream.public.lock() {
                p.progress.bytes = total;
            }
        }
        item
    });
    let reader = tokio_util::io::StreamReader::new(stream);
    // `receive_stream_body` takes the API dataset lock for the destination;
    // the agent holds the same lock until the last byte is written. A mount
    // failure is an error, not a silent warning (P-6). For a raw stream the
    // agent loads the key before finalizing so the mount can succeed.
    let outcome = agent
        .receive_stream_body_outcome_raw(destination, force, raw, unlock_key, reader)
        .await
        .map_err(|e| format!("zfs receive into {destination} failed: {e}"))?;
    Ok((progress.load(Ordering::Relaxed) - before, outcome))
}

fn bad_request(msg: &str) -> Response {
    json(StatusCode::BAD_REQUEST, serde_json::json!({ "error": msg }))
}

fn random_job_id() -> String {
    let mut buf = [0u8; 16];
    if getrandom::fill(&mut buf).is_err() {
        return format!("{:032x}", Utc::now().timestamp_nanos_opt().unwrap_or(0));
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// Two random bytes keep two runs of the same pair in one second apart.
fn random_suffix() -> String {
    let mut buf = [0u8; 2];
    if getrandom::fill(&mut buf).is_err() {
        return format!(
            "{:04x}",
            Utc::now().timestamp_nanos_opt().unwrap_or(0) & 0xffff
        );
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Default, Deserialize)]
struct SendRequest {
    #[serde(default)]
    source: String,
    #[serde(default)]
    destination: String,
    #[serde(default)]
    snapshot: String,
    #[serde(default)]
    recursive: bool,
    #[serde(default, rename = "pruneKeep")]
    prune_keep: usize,
}

/// `POST /api/replication/send` — validate and enqueue a local replication job.
pub async fn send_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let Some(agent) = state.agent.clone() else {
        return json(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({ "error": "the API has no agent client, so it cannot stream ZFS data" }),
        );
    };
    let req: SendRequest = match super::zfs::read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let source = req.source.trim().to_string();
    let destination = req.destination.trim().to_string();
    if source.is_empty() || destination.is_empty() {
        return bad_request("source and destination are required");
    }
    if let Err(e) = validate_pair(&source, &destination) {
        return bad_request(&e);
    }
    let snapshot = req.snapshot.trim().to_string();
    if !snapshot.is_empty() {
        if let Err(e) = super::zfs::validate_snapshot_name(&snapshot) {
            return bad_request(&e);
        }
    }
    if req.prune_keep > MAX_PRUNE_KEEP {
        return bad_request(&format!("pruneKeep must be at most {MAX_PRUNE_KEEP}"));
    }

    let datasets = match agent.list_datasets().await {
        Ok(d) => d,
        Err(e) => return super::zfs::write_agent_error(&e),
    };
    if !datasets.iter().any(|d| d.name == source) {
        return bad_request(&format!("source dataset {source:?} does not exist"));
    }
    // Validate the supplied snapshot before the raw-key preflight writes the
    // destination Secret, so a bad request cannot leave key material behind.
    if !snapshot.is_empty() {
        let snaps = match agent.snapshots_with_guid_shallow(&source).await {
            Ok(s) => s,
            Err(e) => return super::zfs::write_agent_error(&e),
        };
        if !snaps.iter().any(|s| s.name == snapshot) {
            return bad_request(&format!(
                "snapshot {snapshot:?} does not exist on source {source:?}"
            ));
        }
    }
    let prepared =
        match prepare_encryption(&state, &datasets, &source, &destination, req.recursive).await {
            Ok(prepared) => prepared,
            Err(resp) => return resp,
        };
    if !prepared.raw && !prepared.destination_exists {
        return bad_request(&format!(
            "destination dataset {destination:?} does not exist; create it before replicating into it"
        ));
    }

    if let Some(conflict) = state.replication_jobs.conflicting(&destination) {
        return json(
            StatusCode::CONFLICT,
            serde_json::json!({ "error": format!("a replication job is already writing this destination (job {}, started {})", conflict.id, conflict.started_at.to_rfc3339()) }),
        );
    }

    match enqueue_job(
        &state,
        agent,
        source,
        destination,
        snapshot,
        req.recursive,
        req.prune_keep,
        prepared,
    ) {
        Ok(id) => json(
            StatusCode::ACCEPTED,
            serde_json::json!({ "jobId": id, "status": "started" }),
        ),
        Err(conflict) => json(
            StatusCode::CONFLICT,
            serde_json::json!({ "error": format!("a replication job is already writing this destination (job {}, started {})", conflict.id, conflict.started_at.to_rfc3339()) }),
        ),
    }
}

/// The validated encryption decision for one request, including the raw key
/// the agent must load after a raw receive and the destination Secret written
/// for a first raw replica.
struct PreparedEncryption {
    raw: bool,
    created_destination: bool,
    destination_exists: bool,
    unlock_key: Option<RawKey>,
}

/// Resolve [`EncryptionPlan`] and, for a raw pair, the key custody:
///
/// - a first raw replica gets the source key written as its own Secret (the
///   raw-received dataset keeps the source key), so unlock-after-boot works;
/// - an existing replica must hold exactly the source key, or the chain is
///   mixed and refused.
#[allow(clippy::result_large_err)]
async fn prepare_encryption(
    state: &AppState,
    datasets: &[Dataset],
    source: &str,
    destination: &str,
    recursive: bool,
) -> Result<PreparedEncryption, Response> {
    let plan = match plan_encryption(datasets, source, destination, recursive) {
        Ok(plan) => plan,
        Err(e) => return Err(bad_request(&e)),
    };
    let destination_exists = datasets.iter().any(|d| d.name == destination);
    let Some(root) = plan.source_root().map(str::to_string) else {
        return Ok(PreparedEncryption {
            raw: false,
            created_destination: false,
            destination_exists,
            unlock_key: None,
        });
    };

    let source_key = super::zfs::read_dataset_key(state, &root).await?;
    if plan.creates_destination() {
        let Some(kube) = state.kube.as_ref() else {
            return Err(json(
                StatusCode::SERVICE_UNAVAILABLE,
                serde_json::json!({ "error": "encrypted replication needs the Kubernetes client to store the replica key; none is configured" }),
            ));
        };
        let namespace = dataset_encryption::keys_namespace();
        let object =
            match dataset_encryption::key_secret_object(destination, &source_key, &namespace) {
                Ok(object) => object,
                Err(e) => return Err(bad_request(&e)),
            };
        // The destination does not exist (the plan guarantees a raw-new pair),
        // so an existing Secret for it is a leftover from a failed first run.
        // Replacing it with the same source key is safe and avoids stranding
        // the pair on a stale key.
        if let Err(e) = kube.apply(&object).await {
            return Err(json(
                StatusCode::BAD_GATEWAY,
                serde_json::json!({ "error": format!("writing the destination key Secret: {e}") }),
            ));
        }
    } else {
        let replica_key = super::zfs::read_dataset_key(state, destination).await?;
        if replica_key.as_bytes() != source_key.as_bytes() {
            return Err(json(
                StatusCode::CONFLICT,
                serde_json::json!({
                    "error": format!(
                        "the replica {destination:?} uses a different key than the source; raw replication keeps the source key and never mixes chains. Destroy the replica or choose a new destination"
                    )
                }),
            ));
        }
    }
    Ok(PreparedEncryption {
        raw: plan.is_raw(),
        created_destination: plan.creates_destination(),
        destination_exists,
        unlock_key: Some(source_key),
    })
}

/// Remove the destination key Secret after a failed first raw receive, but only
/// when the dataset never landed: destroying the only key of an existing
/// replica would make it unrecoverable.
async fn rollback_created_destination(
    state: &AppState,
    agent: &crate::agent::Client,
    destination: &str,
) {
    let exists = match agent.list_datasets().await {
        Ok(datasets) => datasets.iter().any(|d| d.name == destination),
        Err(_) => true,
    };
    if exists {
        return;
    }
    let Some(kube) = state.kube.as_ref() else {
        return;
    };
    let Ok(name) = dataset_encryption::secret_name_for_dataset(destination) else {
        return;
    };
    let namespace = dataset_encryption::keys_namespace();
    if let Err(e) = kube.delete("secret", &name, &namespace).await {
        tracing::warn!(
            "could not remove the destination key Secret {} after a failed replication: {e}",
            crate::logsafe::field(&name)
        );
    }
}

fn finish_failed(job: &ReplicationJob, msg: String) {
    let mut p = job.public.lock().unwrap();
    // A cancel may have won the race while an in-flight agent call returned.
    // Terminal states are first-writer-wins; never overwrite `cancelled`.
    if p.state != JobState::Running {
        return;
    }
    p.state = JobState::Failed;
    p.error = msg;
    p.finished_at = Some(Utc::now());
}

fn finish_succeeded(job: &ReplicationJob, result: ReplicationResult) {
    let mut p = job.public.lock().unwrap();
    if p.state != JobState::Running {
        return;
    }
    p.state = JobState::Succeeded;
    p.result = Some(result);
    p.finished_at = Some(Utc::now());
}

#[allow(clippy::too_many_arguments)]
async fn run_job(
    state: Arc<AppState>,
    job: Arc<ReplicationJob>,
    agent: Arc<crate::agent::Client>,
    source: String,
    destination: String,
    requested_snapshot: String,
    recursive: bool,
    prune_keep: usize,
    prepared: PreparedEncryption,
) {
    let PreparedEncryption {
        raw,
        created_destination,
        destination_exists,
        unlock_key,
    } = prepared;
    let started = Instant::now();
    macro_rules! fail {
        ($msg:expr) => {{
            let msg = $msg;
            if created_destination {
                rollback_created_destination(&state, &agent, &destination).await;
            }
            finish_failed(&job, msg);
            return;
        }};
    }

    let mut source_snaps = match agent.snapshots_with_guid_shallow(&source).await {
        Ok(s) => s,
        Err(e) => fail!(format!("listing source snapshots: {e}")),
    };

    let mut created = false;
    let target = if requested_snapshot.is_empty() {
        let name = format!(
            "{}-{}",
            snapshot_name(&source, &destination, Utc::now()),
            random_suffix()
        );
        if let Err(e) = agent.create_snapshot_with(&source, &name, recursive).await {
            fail!(format!("snapshotting {source}: {e}"));
        }
        created = true;
        job.public.lock().unwrap().snapshot = name.clone();
        name
    } else {
        requested_snapshot
    };

    if created {
        source_snaps = match agent.snapshots_with_guid_shallow(&source).await {
            Ok(s) => s,
            Err(e) => fail!(format!("listing source snapshots after the snapshot: {e}")),
        };
    }
    let Some(target_info) = source_snaps.iter().find(|s| s.name == target).cloned() else {
        fail!(format!(
            "snapshot {target:?} does not exist on source {source:?}"
        ));
    };
    job.public.lock().unwrap().snapshot = target.clone();

    let destination_snaps = if destination_exists {
        match agent.snapshots_with_guid_shallow(&destination).await {
            Ok(s) => s,
            Err(e) => fail!(format!("listing destination snapshots: {e}")),
        }
    } else {
        // A raw-new pair has no destination yet; the stream creates it.
        Vec::new()
    };
    let plan = match plan_send(&source_snaps, &destination_snaps, &target) {
        Ok(p) => p,
        Err(e) => fail!(e),
    };
    // The root target GUID can be present while a descendant replica is
    // missing. The destination's newest snapshot is already the target, so a
    // stream from an older base cannot be received without rolling it back;
    // repair by sending each missing child subtree on its own instead.
    let repair_datasets = if recursive && plan == SendPlan::UpToDate {
        match recursive_repair_datasets(&agent, &source, &destination, &target).await {
            Ok(datasets) => datasets,
            Err(e) => fail!(e),
        }
    } else {
        Vec::new()
    };

    let prefix = snapshot_prefix(&source, &destination);
    let mut result = ReplicationResult {
        source: source.clone(),
        destination: destination.clone(),
        snapshot: target.clone(),
        to_guid: target_info.guid.clone(),
        recursive,
        raw,
        ..Default::default()
    };

    let progress = Arc::new(AtomicI64::new(0));
    let mut sent_any = false;
    if plan != SendPlan::UpToDate {
        let (from, base_snapshot, base_guid) = match &plan {
            SendPlan::Incremental { base } => {
                (base.name.clone(), base.name.clone(), base.guid.clone())
            }
            _ => (String::new(), String::new(), String::new()),
        };
        let force = plan == SendPlan::Full && destination_exists && !raw;
        // A full raw stream cannot replace an existing replica: ZFS refuses
        // `zfs receive -F` when it would destroy/overwrite an encrypted dataset.
        if raw && plan == SendPlan::Full && destination_exists {
            fail!("encrypted replication cannot replace an existing replica with a full stream (zfs receive -F is refused for encrypted datasets); destroy the replica or choose a new destination".to_string());
        }
        if force {
            if let Err(e) = destination_can_be_replaced(&state, &agent, &destination, &prefix).await
            {
                fail!(e);
            }
        }
        result.base_snapshot = base_snapshot;
        result.base_guid = base_guid.clone();
        result.incremental = !from.is_empty();
        job.public.lock().unwrap().incremental = result.incremental;

        let opts = SendStreamOptions {
            dataset: source.clone(),
            to: target.clone(),
            from,
            raw,
            recursive,
        };
        let expected = agent.estimate_send(&opts).await.unwrap_or_else(|e| {
            tracing::warn!("replication: could not estimate {source}@{target}: {e}");
            0
        });
        {
            let mut p = job.public.lock().unwrap();
            p.estimated_bytes = expected;
            p.progress.estimated_bytes = expected;
        }
        result.estimated_bytes = expected;

        match stream_one(
            &agent,
            &job,
            opts,
            &destination,
            force,
            raw,
            unlock_key.as_ref().map(|k| k.as_bytes()),
            &progress,
        )
        .await
        {
            Ok((bytes, received)) => {
                result.bytes = bytes;
                if !received.mountpoint.is_empty() {
                    result.mountpoint = received.mountpoint;
                }
                result.warnings.extend(received.warnings);
            }
            Err(e) => fail!(e),
        };
        result.status = "replicated".to_string();
        sent_any = true;
    }

    // Each missing child is a full send. A stale child that exists without the
    // target GUID is replaced with `-F` only after the same safety gate a
    // top-level full send uses; a foreign snapshot or child dataset is refused.
    if !repair_datasets.is_empty() {
        let datasets = match agent.list_datasets().await {
            Ok(datasets) => datasets,
            Err(e) => fail!(format!("listing datasets: {e}")),
        };
        let dataset_names: Vec<String> = datasets.iter().map(|d| d.name.clone()).collect();
        for dataset in &repair_datasets {
            let relative = dataset.strip_prefix(&source).unwrap_or_default();
            let replica = format!("{destination}{relative}");
            let exists = dataset_names.iter().any(|name| name == &replica);
            if exists {
                // A raw stream cannot replace an existing child either: the
                // replacement path is `-F`, which ZFS refuses for encryption.
                if raw {
                    fail!(format!(
                        "encrypted replication cannot replace the existing child replica {replica} with a full stream; destroy it first"
                    ));
                }
                if let Err(e) =
                    crate::server::zfs::dataset_consumer_conflict(&state, &replica, &datasets)
                {
                    fail!(e);
                }
                if let Err(e) = replacement_gate(&agent, &replica, &prefix, &dataset_names).await {
                    fail!(e);
                }
            }
            let opts = SendStreamOptions {
                dataset: dataset.clone(),
                to: target.clone(),
                from: String::new(),
                raw,
                recursive: true,
            };
            let expected = agent.estimate_send(&opts).await.unwrap_or_else(|e| {
                tracing::warn!("replication: could not estimate {dataset}@{target}: {e}");
                0
            });
            result.estimated_bytes += expected;
            {
                let mut p = job.public.lock().unwrap();
                p.estimated_bytes = result.estimated_bytes;
                p.progress.estimated_bytes = result.estimated_bytes;
            }
            let child_force = exists && !raw;
            // A raw recursive tree may contain an unencrypted child; its replica
            // is unencrypted too, so it must be received without a key.
            let child_encrypted = datasets
                .iter()
                .find(|d| &d.name == dataset)
                .map(is_encrypted)
                .unwrap_or(false);
            let child_key = if raw && child_encrypted {
                unlock_key.as_ref().map(|k| k.as_bytes())
            } else {
                None
            };
            let (sent, received) = match stream_one(
                &agent,
                &job,
                opts,
                &replica,
                child_force,
                raw,
                child_key,
                &progress,
            )
            .await
            {
                Ok(v) => v,
                Err(e) => fail!(e),
            };
            result.bytes += sent;
            result.warnings.extend(received.warnings);
        }
        result.status = "replicated".to_string();
        result.incremental = false;
        job.public.lock().unwrap().incremental = false;
        sent_any = true;
    }
    if !sent_any {
        result.status = "up-to-date".to_string();
    }

    if prune_keep > 0 && sent_any {
        match agent.snapshot_paths(&destination).await {
            Ok(paths) => {
                let mut errors = Vec::new();
                for (dataset, snap) in prune_plan(&paths, &destination, &prefix, prune_keep) {
                    match agent.destroy_snapshot(&dataset, &snap).await {
                        Ok(()) => result.pruned_snapshots += 1,
                        Err(e) => errors.push(format!("{dataset}@{snap}: {e}")),
                    }
                }
                result.retention_error = errors.join("; ");
            }
            Err(e) => result.retention_error = format!("listing destination snapshots: {e}"),
        }
    }
    result.duration_seconds = started.elapsed().as_secs_f64();

    finish_succeeded(&job, result);
}

/// Create and start a replication job.
#[allow(clippy::result_large_err, clippy::too_many_arguments)]
fn enqueue_job(
    state: &Arc<AppState>,
    agent: Arc<crate::agent::Client>,
    source: String,
    destination: String,
    snapshot: String,
    recursive: bool,
    prune_keep: usize,
    prepared: PreparedEncryption,
) -> Result<String, JobPublic> {
    let id = random_job_id();
    let job = Arc::new(ReplicationJob {
        public: Mutex::new(JobPublic {
            id: id.clone(),
            source: source.clone(),
            destination: destination.clone(),
            snapshot: snapshot.clone(),
            recursive,
            raw: prepared.raw,
            prune_keep,
            state: JobState::Running,
            started_at: Utc::now(),
            finished_at: None,
            progress: ProgressView::default(),
            incremental: false,
            estimated_bytes: 0,
            result: None,
            error: String::new(),
        }),
        handle: Mutex::new(None),
    });
    state.replication_jobs.add_if_free(&id, job.clone())?;
    let job_for_task = job.clone();
    let snapshot_for_task = snapshot.clone();
    let state_for_task = state.clone();
    let handle = tokio::spawn(async move {
        run_job(
            state_for_task,
            job_for_task,
            agent,
            source,
            destination,
            snapshot_for_task,
            recursive,
            prune_keep,
            prepared,
        )
        .await;
    });
    *job.handle.lock().unwrap() = Some(handle);
    Ok(id)
}

/// `GET /api/replication/jobs`
pub async fn jobs_get(State(state): State<Arc<AppState>>) -> Response {
    json(
        StatusCode::OK,
        serde_json::json!({ "jobs": state.replication_jobs.list() }),
    )
}

/// `GET /api/replication/jobs/{id}`
pub async fn job_detail_get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match state.replication_jobs.get(&id) {
        Some(job) => json(
            StatusCode::OK,
            serde_json::to_value(job.snapshot()).unwrap(),
        ),
        None => json(
            StatusCode::NOT_FOUND,
            serde_json::json!({ "error": format!("job {id:?} not found") }),
        ),
    }
}

/// `DELETE /api/replication/jobs/{id}` — cancel a running job.
pub async fn job_detail_delete(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let Some(job) = state.replication_jobs.get(&id) else {
        return json(
            StatusCode::NOT_FOUND,
            serde_json::json!({ "error": format!("job {id:?} not found") }),
        );
    };
    // Claim the terminal state under the same lock run_job uses, so a late
    // success/failure cannot overwrite the cancellation (and a second cancel
    // cannot win twice).
    let already = {
        let mut p = job.public.lock().unwrap();
        if p.state != JobState::Running {
            Some(p.state)
        } else {
            p.state = JobState::Cancelled;
            p.finished_at = Some(Utc::now());
            p.error = "cancelled".to_string();
            None
        }
    };
    if let Some(state) = already {
        return json(
            StatusCode::CONFLICT,
            serde_json::json!({ "error": format!("job {id} is already {state:?}") }),
        );
    }
    if let Some(handle) = job.handle.lock().unwrap().take() {
        handle.abort();
    }
    json(
        StatusCode::OK,
        serde_json::json!({ "status": "cancelling", "id": id }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn snap(name: &str, guid: &str) -> SnapshotInfo {
        SnapshotInfo {
            dataset: String::new(),
            name: name.into(),
            guid: guid.into(),
            created: String::new(),
        }
    }

    fn snap_on(dataset: &str, name: &str, guid: &str) -> SnapshotInfo {
        SnapshotInfo {
            dataset: dataset.into(),
            name: name.into(),
            guid: guid.into(),
            created: String::new(),
        }
    }

    fn path(ds: &str, snap: &str) -> String {
        format!("{ds}@{snap}")
    }

    #[test]
    fn ids_and_names_are_replication_owned() {
        let now = Utc.with_ymd_and_hms(2026, 1, 5, 10, 15, 0).unwrap();
        let name = snapshot_name("tank/src", "backup/dst", now);
        assert!(name.starts_with("naslos-rep-"));
        assert!(name.ends_with("20260105T101500Z"));
        assert!(name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '+')));
        assert_ne!(
            snapshot_prefix("tank/a", "backup/b"),
            snapshot_prefix("tank/b", "backup/a")
        );
    }

    #[test]
    fn identical_and_nested_pairs_are_refused() {
        assert!(validate_pair("tank/src", "tank/src").is_err());
        assert!(validate_pair("tank/src", "tank/src/child").is_err());
        assert!(validate_pair("tank/src/child", "tank/src").is_err());
        assert!(validate_pair("tank/src", "backup/dst").is_ok());
        assert!(validate_pair("tank/src", "tank/src2").is_ok());
    }

    #[test]
    fn plan_uses_the_newest_common_guid() {
        let source = vec![
            snap("a", "g1"),
            snap("renamed-b", "g2"),
            snap("c", "g3"),
            snap("d", "g4"),
        ];
        let destination = vec![snap("other-name", "g1"), snap("b-old", "g2")];
        assert_eq!(
            plan_send(&source, &destination, "d").unwrap(),
            SendPlan::Incremental {
                base: snap("renamed-b", "g2")
            }
        );
        // A destination that already holds the target GUID has nothing to do.
        assert_eq!(
            plan_send(&source, &[snap("d", "g4")], "d").unwrap(),
            SendPlan::UpToDate
        );
        // No common GUID means a full send.
        assert_eq!(
            plan_send(&source, &[snap("x", "gx")], "d").unwrap(),
            SendPlan::Full
        );
        assert!(plan_send(&source, &destination, "missing").is_err());
    }

    #[test]
    fn retention_keeps_the_newest_own_snapshots_per_dataset() {
        let paths = vec![
            path("backup/dst", "naslos-rep-abcd-old1"),
            path("backup/dst", "manual-keep"),
            path("backup/dst", "naslos-rep-abcd-old2"),
            path("backup/dst", "naslos-rep-abcd-new1"),
            path("backup/dst/child", "naslos-rep-abcd-old1"),
            path("backup/dst/child", "naslos-rep-abcd-new1"),
            path("other/data", "naslos-rep-abcd-old1"),
        ];
        let delete = prune_plan(&paths, "backup/dst", "naslos-rep-abcd-", 2);
        assert_eq!(
            delete,
            vec![("backup/dst".to_string(), "naslos-rep-abcd-old1".to_string())]
        );
        assert!(prune_plan(&paths, "backup/dst", "naslos-rep-abcd-", 0).is_empty());
    }

    #[test]
    fn a_full_send_refuses_foreign_snapshots_and_child_datasets() {
        let prefix = snapshot_prefix("tank/src", "backup/dst");
        let own = vec![path("backup/dst", &format!("{prefix}old"))];
        let user = vec![path("backup/dst", "manual-keep")];
        let no_children = vec!["tank/src".to_string(), "backup/dst".to_string()];
        assert!(replacement_blocker(&own, &no_children, "backup/dst", &prefix).is_none());
        let reason = replacement_blocker(&user, &no_children, "backup/dst", &prefix)
            .expect("a user snapshot must block the full send");
        assert!(reason.contains("replication did not create"), "{reason}");
        let children = vec![
            "tank/src".to_string(),
            "backup/dst".to_string(),
            "backup/dst/child".to_string(),
        ];
        let reason = replacement_blocker(&own, &children, "backup/dst", &prefix)
            .expect("a child dataset must block the full send");
        assert!(reason.contains("child datasets"), "{reason}");
    }

    #[test]
    fn expected_recursive_snapshots_are_shallowest_first_and_boundary_checked() {
        let snapshots = vec![
            snap_on("tank/src/a/b", "snap", "g3"),
            snap_on("tank/src", "snap", "g1"),
            snap_on("tank/src/a", "snap", "g2"),
            snap_on("tank/src2/sibling", "snap", "gx"),
            snap_on("tank/src/a", "other", "gy"),
        ];
        assert_eq!(
            expected_recursive_snapshots(&snapshots, "tank/src", "snap"),
            vec![
                ("tank/src".to_string(), "g1".to_string()),
                ("tank/src/a".to_string(), "g2".to_string()),
                ("tank/src/a/b".to_string(), "g3".to_string())
            ]
        );
    }

    #[test]
    fn missing_recursive_replicas_uses_guids_and_the_topmost_subtree() {
        let expected = vec![
            ("tank/src".to_string(), "g1".to_string()),
            ("tank/src/child".to_string(), "g2".to_string()),
            ("tank/src/child/grandchild".to_string(), "g3".to_string()),
            ("tank/src/other".to_string(), "g4".to_string()),
        ];
        let destination = vec![
            snap_on("backup/dst", "snap", "g1"),
            // A renamed snapshot still matches by GUID.
            snap_on("backup/dst/other", "renamed", "g4"),
            // The child exists by name but is not the source snapshot.
            snap_on("backup/dst/child", "snap", "g-DIFFERENT"),
        ];
        assert_eq!(
            missing_recursive_replicas(&expected, &destination, "tank/src", "backup/dst"),
            vec!["tank/src/child".to_string()]
        );

        let complete = vec![
            snap_on("backup/dst", "snap", "g1"),
            snap_on("backup/dst/child", "renamed-child", "g2"),
            snap_on("backup/dst/child/grandchild", "snap", "g3"),
            snap_on("backup/dst/other", "snap", "g4"),
        ];
        assert!(
            missing_recursive_replicas(&expected, &complete, "tank/src", "backup/dst").is_empty()
        );
    }

    fn job_with_state(state: JobState) -> ReplicationJob {
        ReplicationJob {
            public: Mutex::new(JobPublic {
                id: "job".to_string(),
                source: String::new(),
                destination: String::new(),
                snapshot: String::new(),
                recursive: false,
                raw: false,
                prune_keep: 0,
                state,
                started_at: Utc::now(),
                finished_at: None,
                progress: ProgressView::default(),
                incremental: false,
                estimated_bytes: 0,
                result: None,
                error: String::new(),
            }),
            handle: Mutex::new(None),
        }
    }

    #[test]
    fn a_cancelled_job_is_not_overwritten_by_a_late_terminal_write() {
        let job = job_with_state(JobState::Cancelled);
        finish_failed(&job, "late failure".to_string());
        assert_eq!(job.state(), JobState::Cancelled);
        assert!(job.snapshot().error.is_empty());

        finish_succeeded(&job, ReplicationResult::default());
        assert_eq!(job.state(), JobState::Cancelled);
        assert!(job.snapshot().result.is_none());

        let running = job_with_state(JobState::Running);
        finish_succeeded(&running, ReplicationResult::default());
        assert_eq!(running.state(), JobState::Succeeded);
        assert!(running.snapshot().result.is_some());
    }

    #[test]
    fn destination_overlap_covers_nesting() {
        assert!(destinations_overlap("backup/a", "backup/a"));
        assert!(destinations_overlap("backup/a", "backup/a/child"));
        assert!(destinations_overlap("backup/a/child", "backup/a"));
        assert!(!destinations_overlap("backup/a", "backup/b"));
        assert!(!destinations_overlap("backup/a", "backup/ab"));
    }

    fn ds(name: &str, encryption: &str, root: &str) -> Dataset {
        Dataset {
            name: name.into(),
            encryption: encryption.into(),
            encryption_root: root.into(),
            ..Default::default()
        }
    }

    fn plain(name: &str) -> Dataset {
        Dataset {
            name: name.into(),
            encryption: "off".into(),
            ..Default::default()
        }
    }

    #[test]
    fn encryption_plan_keeps_plain_pairs_unchanged() {
        let datasets = vec![plain("tank/src"), plain("backup/dst")];
        assert_eq!(
            plan_encryption(&datasets, "tank/src", "backup/dst", false).unwrap(),
            EncryptionPlan::Plain
        );
        // The destination existence rule stays the caller's job for a plain pair.
        assert_eq!(
            plan_encryption(&datasets, "tank/src", "backup/new", false).unwrap(),
            EncryptionPlan::Plain
        );
    }

    #[test]
    fn encryption_plan_creates_a_raw_destination_only_under_an_existing_parent() {
        let datasets = vec![ds("tank/src", "aes-256-gcm", "tank/src"), plain("backup")];
        assert_eq!(
            plan_encryption(&datasets, "tank/src", "backup/replica", false).unwrap(),
            EncryptionPlan::RawNew {
                source_root: "tank/src".into()
            }
        );
        let err = plan_encryption(&datasets, "tank/src", "missing/replica", false).unwrap_err();
        assert!(err.contains("parent"), "{err}");
        assert!(plan_encryption(&datasets, "tank/src", "backup", false).is_err());
    }

    #[test]
    fn encryption_plan_uses_the_inherited_source_root_and_refuses_mixed_chains() {
        // The source inherits the key of an ancestor; the replication key is the root's.
        let datasets = vec![
            ds("tank/enc", "aes-256-gcm", "tank/enc"),
            ds("tank/enc/src", "aes-256-gcm", "tank/enc"),
            plain("backup"),
        ];
        assert_eq!(
            plan_encryption(&datasets, "tank/enc/src", "backup/replica", false).unwrap(),
            EncryptionPlan::RawNew {
                source_root: "tank/enc".into()
            }
        );

        // An unencrypted destination can never take a raw stream.
        let mut mixed = datasets.clone();
        mixed.push(plain("backup/dst"));
        let err = plan_encryption(&mixed, "tank/enc/src", "backup/dst", false).unwrap_err();
        assert!(err.contains("not encrypted"), "{err}");

        // A plain source must not land in an encrypted destination.
        let err = plan_encryption(
            &[
                plain("tank/src"),
                ds("backup/dst", "aes-256-gcm", "backup/dst"),
            ],
            "tank/src",
            "backup/dst",
            false,
        )
        .unwrap_err();
        assert!(err.contains("never mixed"), "{err}");

        // A destination that inherits a parent key is not a dedicated replica.
        let err = plan_encryption(
            &[
                ds("tank/src", "aes-256-gcm", "tank/src"),
                ds("tank/enc/dst", "aes-256-gcm", "tank/enc"),
            ],
            "tank/src",
            "tank/enc/dst",
            false,
        )
        .unwrap_err();
        assert!(err.contains("inherits its key"), "{err}");
    }

    #[test]
    fn encryption_plan_requires_one_root_for_a_recursive_raw_send() {
        let datasets = vec![
            ds("tank/src", "aes-256-gcm", "tank/src"),
            ds("tank/src/inherits", "aes-256-gcm", "tank/src"),
            plain("backup"),
        ];
        assert!(matches!(
            plan_encryption(&datasets, "tank/src", "backup/replica", true).unwrap(),
            EncryptionPlan::RawNew { .. }
        ));

        let mut two_roots = datasets.clone();
        two_roots.push(ds("tank/src/own", "aes-256-gcm", "tank/src/own"));
        let err = plan_encryption(&two_roots, "tank/src", "backup/replica", true).unwrap_err();
        assert!(err.contains("one encryption root"), "{err}");
        // A non-recursive send does not touch the child, so it stays allowed.
        assert!(matches!(
            plan_encryption(&two_roots, "tank/src", "backup/replica", false).unwrap(),
            EncryptionPlan::RawNew { .. }
        ));
    }

    #[test]
    fn encryption_plan_allows_incrementals_into_an_existing_raw_replica() {
        let datasets = vec![
            ds("tank/src", "aes-256-gcm", "tank/src"),
            ds("backup/replica", "aes-256-gcm", "backup/replica"),
        ];
        assert_eq!(
            plan_encryption(&datasets, "tank/src", "backup/replica", false).unwrap(),
            EncryptionPlan::RawIncremental {
                source_root: "tank/src".into()
            }
        );
    }
}
