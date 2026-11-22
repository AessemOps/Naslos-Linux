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

use crate::agent::{SendStreamOptions, SnapshotInfo};
use crate::json;
use crate::state::AppState;

/// Every snapshot this feature creates starts with this prefix.
pub const SNAPSHOT_PREFIX: &str = "naslos-rep-";

/// Upper bound on retained destination snapshots per pair.
pub const MAX_PRUNE_KEEP: usize = 1000;

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
    bytes: i64,
    #[serde(rename = "estimatedBytes")]
    estimated_bytes: i64,
    #[serde(rename = "prunedSnapshots")]
    pruned_snapshots: usize,
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

/// Source datasets that carry `target`, shallowest first. A sibling whose
/// name merely shares the source prefix is not included.
pub fn recursive_source_datasets(
    source_paths: &[String],
    source: &str,
    target: &str,
) -> Vec<String> {
    let suffix = format!("@{target}");
    let mut datasets: Vec<String> = source_paths
        .iter()
        .filter_map(|path| path.strip_suffix(&suffix))
        .filter_map(|dataset| {
            let relative = dataset.strip_prefix(source)?;
            if relative.is_empty() || relative.starts_with('/') {
                Some(dataset.to_string())
            } else {
                None
            }
        })
        .collect();
    datasets.sort_by_key(|dataset| dataset.matches('/').count());
    datasets
}

/// Recursive source datasets that have no child replica yet, reduced to the
/// top-most missing subtree so one send covers every descendant below it.
pub fn missing_recursive_datasets(
    source_paths: &[String],
    destination_paths: &[String],
    source: &str,
    destination: &str,
    target: &str,
) -> Vec<String> {
    let suffix = format!("@{target}");
    let present: HashSet<&str> = destination_paths
        .iter()
        .filter_map(|path| path.strip_suffix(&suffix))
        .collect();
    let mut missing: Vec<String> = Vec::new();
    for dataset in recursive_source_datasets(source_paths, source, target) {
        let relative = dataset.strip_prefix(source).unwrap_or_default();
        let expected = format!("{destination}{relative}");
        if present.contains(expected.as_str()) {
            continue;
        }
        if missing
            .iter()
            .any(|parent| dataset.starts_with(&format!("{parent}/")))
        {
            continue;
        }
        missing.push(dataset);
    }
    missing
}

fn target_guid(snapshots: &[SnapshotInfo], target: &str) -> Option<String> {
    snapshots
        .iter()
        .find(|snapshot| snapshot.name == target)
        .map(|snapshot| snapshot.guid.clone())
        .filter(|guid| !guid.is_empty())
}

/// Verify a recursive replica and return the top-most subtree sends needed to
/// repair it. A child present by name but carrying a different GUID is refused
/// rather than mistaken for the same snapshot.
async fn recursive_repair_datasets(
    agent: &crate::agent::Client,
    source: &str,
    destination: &str,
    target: &str,
) -> Result<Vec<String>, String> {
    let source_paths = agent
        .snapshot_paths(source)
        .await
        .map_err(|e| format!("listing recursive source snapshots: {e}"))?;
    let destination_paths = agent
        .snapshot_paths(destination)
        .await
        .map_err(|e| format!("listing recursive destination snapshots: {e}"))?;
    let suffix = format!("@{target}");
    let present: HashSet<String> = destination_paths
        .iter()
        .filter_map(|path| path.strip_suffix(&suffix))
        .map(str::to_string)
        .collect();
    for dataset in recursive_source_datasets(&source_paths, source, target) {
        let relative = dataset.strip_prefix(source).unwrap_or_default();
        let replica = format!("{destination}{relative}");
        if !present.contains(&replica) {
            continue;
        }
        let expected = target_guid(
            &agent
                .snapshots_with_guid_shallow(&dataset)
                .await
                .map_err(|e| format!("reading {dataset}@{target}: {e}"))?,
            target,
        )
        .ok_or_else(|| format!("source {dataset}@{target} has no GUID"))?;
        let actual = target_guid(
            &agent
                .snapshots_with_guid_shallow(&replica)
                .await
                .map_err(|e| format!("reading {replica}@{target}: {e}"))?,
            target,
        )
        .ok_or_else(|| format!("destination {replica}@{target} has no GUID"))?;
        if expected != actual {
            return Err(format!(
                "child replica {replica}@{target} has a different GUID; refusing to treat it as current. Remove that snapshot or choose another destination"
            ));
        }
    }
    Ok(missing_recursive_datasets(
        &source_paths,
        &destination_paths,
        source,
        destination,
        target,
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

async fn destination_can_be_replaced(
    agent: &crate::agent::Client,
    destination: &str,
    prefix: &str,
) -> Result<(), String> {
    let paths = agent
        .snapshot_paths(destination)
        .await
        .map_err(|e| format!("listing destination snapshots: {e}"))?;
    let datasets = agent
        .list_datasets()
        .await
        .map_err(|e| format!("listing datasets: {e}"))?;
    let names: Vec<String> = datasets.into_iter().map(|d| d.name).collect();
    match replacement_blocker(&paths, &names, destination, prefix) {
        Some(reason) => Err(reason),
        None => Ok(()),
    }
}

/// Pipe one agent send stream into one receive call and add its bytes to the
/// job progress. Returns the number of bytes streamed.
async fn stream_one(
    agent: &crate::agent::Client,
    job: &Arc<ReplicationJob>,
    opts: SendStreamOptions,
    destination: &str,
    force: bool,
    progress: &Arc<AtomicI64>,
) -> Result<i64, String> {
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
    agent
        .receive_stream_body(destination, force, reader)
        .await
        .map_err(|e| format!("zfs receive into {destination} failed: {e}"))?;
    Ok(progress.load(Ordering::Relaxed) - before)
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
    if !datasets.iter().any(|d| d.name == destination) {
        return bad_request(&format!(
            "destination dataset {destination:?} does not exist; create it before replicating into it"
        ));
    }
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
    job: Arc<ReplicationJob>,
    agent: Arc<crate::agent::Client>,
    source: String,
    destination: String,
    requested_snapshot: String,
    recursive: bool,
    prune_keep: usize,
) {
    let started = Instant::now();
    macro_rules! fail {
        ($msg:expr) => {{
            finish_failed(&job, $msg);
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

    let destination_snaps = match agent.snapshots_with_guid_shallow(&destination).await {
        Ok(s) => s,
        Err(e) => fail!(format!("listing destination snapshots: {e}")),
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
        let force = plan == SendPlan::Full;
        if force {
            if let Err(e) = destination_can_be_replaced(&agent, &destination, &prefix).await {
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
            raw: false,
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

        result.bytes = match stream_one(&agent, &job, opts, &destination, force, &progress).await {
            Ok(bytes) => bytes,
            Err(e) => fail!(e),
        };
        result.status = "replicated".to_string();
        sent_any = true;
    }

    // Each missing child is a full send into a dataset that does not exist on
    // the destination; the root's newer snapshots are never rolled back.
    if !repair_datasets.is_empty() {
        for dataset in &repair_datasets {
            let relative = dataset.strip_prefix(&source).unwrap_or_default();
            let replica = format!("{destination}{relative}");
            let opts = SendStreamOptions {
                dataset: dataset.clone(),
                to: target.clone(),
                from: String::new(),
                raw: false,
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
            let sent = match stream_one(&agent, &job, opts, &replica, false, &progress).await {
                Ok(bytes) => bytes,
                Err(e) => fail!(e),
            };
            result.bytes += sent;
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
#[allow(clippy::result_large_err)]
fn enqueue_job(
    state: &Arc<AppState>,
    agent: Arc<crate::agent::Client>,
    source: String,
    destination: String,
    snapshot: String,
    recursive: bool,
    prune_keep: usize,
) -> Result<String, JobPublic> {
    let id = random_job_id();
    let job = Arc::new(ReplicationJob {
        public: Mutex::new(JobPublic {
            id: id.clone(),
            source: source.clone(),
            destination: destination.clone(),
            snapshot: snapshot.clone(),
            recursive,
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
    let handle = tokio::spawn(async move {
        run_job(
            job_for_task,
            agent,
            source,
            destination,
            snapshot_for_task,
            recursive,
            prune_keep,
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
    fn missing_recursive_datasets_reports_the_topmost_subtree() {
        let source = vec![
            path("tank/src", "snap"),
            path("tank/src/child", "snap"),
            path("tank/src/child/grandchild", "snap"),
            path("tank/src/other", "snap"),
            path("tank/src/no-snap", "other"),
        ];
        let destination = vec![path("backup/dst", "snap"), path("backup/dst/other", "snap")];
        assert_eq!(
            missing_recursive_datasets(&source, &destination, "tank/src", "backup/dst", "snap"),
            vec!["tank/src/child".to_string()]
        );

        let complete = vec![
            path("backup/dst", "snap"),
            path("backup/dst/child", "snap"),
            path("backup/dst/child/grandchild", "snap"),
            path("backup/dst/other", "snap"),
        ];
        assert!(
            missing_recursive_datasets(&source, &complete, "tank/src", "backup/dst", "snap")
                .is_empty()
        );

        // A same-named snapshot under a sibling dataset is not the replica.
        let wrong = vec![
            path("backup/dst", "snap"),
            path("backup/dst2/child", "snap"),
        ];
        assert_eq!(
            missing_recursive_datasets(&source, &wrong, "tank/src", "backup/dst", "snap"),
            vec!["tank/src/child".to_string(), "tank/src/other".to_string()]
        );
    }

    #[test]
    fn recursive_source_datasets_are_shallowest_first_and_boundary_checked() {
        let paths = vec![
            path("tank/src/a/b", "snap"),
            path("tank/src", "snap"),
            path("tank/src/a", "snap"),
            path("tank/src2/sibling", "snap"),
            path("tank/src/a", "other"),
        ];
        assert_eq!(
            recursive_source_datasets(&paths, "tank/src", "snap"),
            vec![
                "tank/src".to_string(),
                "tank/src/a".to_string(),
                "tank/src/a/b".to_string()
            ]
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
}
