//! Async instance-side buddy send (port of `server/buddy_jobs.go`).
//!
//! A send snapshots a local dataset, streams `zfs send` through the buddy client
//! and publishes a signed manifest. It runs as a server-owned job so a
//! disconnected operator cannot kill the transfer; the resume-state file makes a
//! cancelled or killed send continue the same chain.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;

use crate::buddy::client::{ChainState, Client, PushOptions, PushProgress};
use crate::buddy::keys::load_identity;
use crate::buddy::peers::validate_source;
use crate::json;
use crate::state::AppState;

/// The resume state of an interrupted instance-side send.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct InstanceSendState {
    receiver: String,
    source: String,
    dataset: String,
    #[serde(rename = "toSnapshot")]
    to_snapshot: String,
    #[serde(rename = "fromSnapshot")]
    from_snapshot: String,
    #[serde(rename = "fromGUID")]
    from_guid: String,
    #[serde(rename = "toGUID")]
    to_guid: String,
    chain: ChainState,
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
struct SendResult {
    status: String,
    dataset: String,
    source: String,
    receiver: String,
    snapshot: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    base: String,
    #[serde(rename = "baseGUID", skip_serializing_if = "String::is_empty")]
    base_guid: String,
    #[serde(rename = "toGUID", skip_serializing_if = "String::is_empty")]
    to_guid: String,
    incremental: bool,
    resumed: bool,
    raw: bool,
    chain: String,
    chunks: usize,
    uploaded: usize,
    skipped: usize,
    #[serde(rename = "plainBytes")]
    plain_bytes: i64,
    #[serde(rename = "sealedBytes")]
    sealed_bytes: i64,
    #[serde(rename = "estimatedBytes", skip_serializing_if = "is_zero_i64")]
    estimated_bytes: i64,
    #[serde(rename = "prunedChains", skip_serializing_if = "is_zero_usize")]
    pruned_chains: usize,
    #[serde(rename = "durationSeconds")]
    duration_seconds: f64,
}

fn is_zero_i64(n: &i64) -> bool {
    *n == 0
}
fn is_zero_usize(n: &usize) -> bool {
    *n == 0
}

#[derive(Debug, Clone, Serialize)]
pub struct JobPublic {
    id: String,
    dataset: String,
    source: String,
    receiver: String,
    #[serde(rename = "pruneKeep", skip_serializing_if = "is_zero_usize")]
    prune_keep: usize,
    raw: bool,
    #[serde(skip_serializing_if = "is_false")]
    force: bool,
    #[serde(rename = "scheduleId", skip_serializing_if = "String::is_empty")]
    schedule_id: String,
    state: JobState,
    #[serde(rename = "startedAt")]
    started_at: DateTime<Utc>,
    #[serde(rename = "finishedAt", skip_serializing_if = "Option::is_none")]
    finished_at: Option<DateTime<Utc>>,
    progress: ProgressView,
    incremental: bool,
    resumed: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    snapshot: String,
    #[serde(rename = "estimatedBytes", skip_serializing_if = "is_zero_i64")]
    estimated_bytes: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<SendResult>,
    #[serde(skip_serializing_if = "String::is_empty")]
    error: String,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl JobPublic {
    pub fn job_id(&self) -> &str {
        &self.id
    }
}

#[derive(Debug, Clone, Default, Serialize)]
struct ProgressView {
    chunk: i64,
    #[serde(rename = "plainBytes")]
    plain_bytes: i64,
    #[serde(rename = "sealedBytes")]
    sealed_bytes: i64,
    uploaded: usize,
    skipped: usize,
}

pub struct BuddyJob {
    public: Mutex<JobPublic>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl BuddyJob {
    fn snapshot(&self) -> JobPublic {
        self.public.lock().unwrap().clone()
    }
    fn state(&self) -> JobState {
        self.public.lock().unwrap().state
    }
}

type JobMap = std::collections::HashMap<String, Arc<BuddyJob>>;
type JobOrder = Vec<String>;

/// Keeps running jobs plus the last ~20 finished ones.
#[derive(Default)]
pub struct BuddyJobManager {
    inner: Mutex<(JobMap, JobOrder)>,
}

impl BuddyJobManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn conflicting(&self, receiver: &str, source: &str) -> Option<JobPublic> {
        let inner = self.inner.lock().unwrap();
        for job in inner.0.values() {
            let p = job.snapshot();
            if job.state() == JobState::Running && p.receiver == receiver && p.source == source {
                return Some(p);
            }
        }
        None
    }

    /// Register a job, refusing when the same receiver+source already runs.
    /// The check and the insert share one lock: doing them separately (the old
    /// `conflicting` + `add`) let two jobs resume the same chain state file.
    #[allow(clippy::result_large_err)]
    fn add_if_free(
        &self,
        id: &str,
        job: Arc<BuddyJob>,
        receiver: &str,
        source: &str,
    ) -> Result<(), JobPublic> {
        let mut inner = self.inner.lock().unwrap();
        for existing in inner.0.values() {
            let p = existing.snapshot();
            if existing.state() == JobState::Running && p.receiver == receiver && p.source == source
            {
                return Err(p);
            }
        }
        Self::insert_locked(&mut inner, id, job);
        Ok(())
    }

    fn insert_locked(inner: &mut (JobMap, JobOrder), id: &str, job: Arc<BuddyJob>) {
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

    /// Whether a Buddy send/restore job is running.
    pub fn has_running(&self) -> bool {
        self.inner
            .lock()
            .unwrap()
            .0
            .values()
            .any(|j| j.state() == JobState::Running)
    }

    pub fn get(&self, id: &str) -> Option<Arc<BuddyJob>> {
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

fn identity_path() -> String {
    std::env::var("BUDDY_IDENTITY")
        .unwrap_or_else(|_| "/var/lib/naslos/buddy-identity.json".to_string())
}

fn state_base_dir() -> std::path::PathBuf {
    std::path::Path::new(&identity_path())
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

fn sequence_dir() -> String {
    state_base_dir()
        .join("buddy-sequences")
        .display()
        .to_string()
}

fn send_state_path(receiver: &str, source: &str) -> std::path::PathBuf {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(format!("{receiver}|{source}").as_bytes());
    let digest = h.finalize();
    let name: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
    state_base_dir()
        .join("buddy-sends")
        .join(format!("{name}.json"))
}

/// The newest GUID this instance published to `receiver` for `source`. Kept
/// locally so a send to another receiver can protect this receiver's
/// incremental base without a network call to it.
fn anchor_path(receiver: &str, source: &str) -> std::path::PathBuf {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(format!("{receiver}|{source}").as_bytes());
    let digest = h.finalize();
    let name: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
    state_base_dir()
        .join("buddy-anchors")
        .join(format!("{name}.guid"))
}

fn read_anchor(receiver: &str, source: &str) -> Option<String> {
    let raw = std::fs::read_to_string(anchor_path(receiver, source)).ok()?;
    let guid = raw.trim().to_string();
    (!guid.is_empty()).then_some(guid)
}

fn save_anchor(receiver: &str, source: &str, guid: &str) -> Result<(), String> {
    if guid.is_empty() {
        return Ok(());
    }
    let path = anchor_path(receiver, source);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    crate::chartsrepo::write_atomic(
        &path.display().to_string(),
        format!("{guid}\n").as_bytes(),
        0o600,
        0o700,
    )
}

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

fn random_job_id() -> String {
    let mut buf = [0u8; 16];
    if getrandom::fill(&mut buf).is_err() {
        return format!("{:032x}", Utc::now().timestamp_nanos_opt().unwrap_or(0));
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// `zfs send -n -P` is close but not exact (measured differences are a few
/// hundred bytes to ~0.2 % of the stream), so allow 0.5 % with a small floor.
/// The old 1 MiB floor let a 2 MiB send lose half its data and still publish
/// (SEC-2026-C4).
pub(crate) fn send_shortfall_allowance(estimate: i64) -> i64 {
    const FLOOR: i64 = 16 << 10;
    let relative = estimate / 200;
    if relative > FLOOR {
        relative
    } else {
        FLOOR
    }
}

fn bad_request(msg: &str) -> Response {
    json(StatusCode::BAD_REQUEST, serde_json::json!({ "error": msg }))
}

#[derive(Default, Deserialize)]
struct SendRequest {
    #[serde(default)]
    dataset: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    receiver: String,
    #[serde(default, rename = "pruneKeep")]
    prune_keep: usize,
    #[serde(default)]
    raw: Option<bool>,
    #[serde(default)]
    force: bool,
}

/// `POST /api/buddy/send` — validate and enqueue an async send.
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
    let dataset = req.dataset.trim().to_string();
    let source = req.source.trim().to_string();
    if dataset.is_empty() || source.is_empty() {
        return bad_request("dataset and source are required");
    }
    if let Err(e) = validate_source(&source) {
        return bad_request(&e);
    }
    let receiver = match super::buddy_restore::normalize_receiver_url(&req.receiver) {
        Ok(r) => r,
        Err(e) => return bad_request(&e),
    };
    if load_identity(&identity_path()).is_err() {
        return json(
            StatusCode::PRECONDITION_FAILED,
            serde_json::json!({ "error": "no buddy identity: create one with POST /api/buddy/identity" }),
        );
    }
    if let Some(conflict) = state.buddy_jobs.conflicting(&receiver, &source) {
        return json(
            StatusCode::CONFLICT,
            serde_json::json!({ "error": format!("a backup of this source is already running (job {}, started {})", conflict.id, conflict.started_at.to_rfc3339()) }),
        );
    }
    let raw = req.raw.unwrap_or(true);
    let id = match enqueue_job(
        &state,
        agent,
        dataset,
        source,
        receiver,
        raw,
        req.force,
        req.prune_keep,
        String::new(),
    ) {
        Ok(id) => id,
        Err(conflict) => {
            return json(
                StatusCode::CONFLICT,
                serde_json::json!({ "error": format!("a backup of this source is already running (job {}, started {})", conflict.id, conflict.started_at.to_rfc3339()) }),
            )
        }
    };
    json(
        StatusCode::ACCEPTED,
        serde_json::json!({ "jobId": id, "status": "started" }),
    )
}

#[allow(clippy::too_many_arguments)]
async fn run_send_job(
    state: Arc<AppState>,
    job: Arc<BuddyJob>,
    agent: Arc<crate::agent::Client>,
    dataset: String,
    source: String,
    receiver: String,
    raw: bool,
    force: bool,
    prune_keep: usize,
    schedule_id: String,
) {
    let started = std::time::Instant::now();
    let state_path = send_state_path(&receiver, &source);

    macro_rules! fail {
        ($msg:expr) => {{
            let msg: String = $msg;
            {
                let mut p = job.public.lock().unwrap();
                p.state = JobState::Failed;
                p.error = msg.clone();
                p.finished_at = Some(Utc::now());
            }
            if !schedule_id.is_empty() {
                state
                    .buddy_schedules
                    .record_result(&schedule_id, &receiver, false, &msg);
            }
            notify(&state, &job.snapshot()).await;
            return;
        }};
    }

    let identity = match load_identity(&identity_path()) {
        Ok(i) => Arc::new(i),
        Err(e) => fail!(e),
    };
    let mut client = Client::new(&receiver, identity);
    client.sequence_dir = sequence_dir();

    // Resume only when a state file matches and its snapshot still exists.
    let mut resume: Option<InstanceSendState> = None;
    let mut was_resume = false;
    if !force {
        if let Ok(raw_state) = std::fs::read(&state_path) {
            if let Ok(loaded) = serde_json::from_slice::<InstanceSendState>(&raw_state) {
                if loaded.receiver == receiver
                    && loaded.source == source
                    && loaded.dataset == dataset
                {
                    if let Ok(snaps) = agent.snapshots_with_guid(&dataset).await {
                        if snaps.iter().any(|s| s.name == loaded.to_snapshot) {
                            resume = Some(loaded);
                            was_resume = true;
                        }
                    }
                    if resume.is_none() {
                        let _ = std::fs::remove_file(&state_path);
                    }
                }
            }
        }
    }

    let mut base = String::new();
    let mut base_guid = String::new();
    if let Some(r) = &resume {
        base = r.from_snapshot.clone();
        base_guid = r.from_guid.clone();
    } else if !force {
        if let Ok(previous) = client.manifest(&source, "").await {
            if previous.kind == "zfs-send" && !previous.to_guid.is_empty() {
                if let Ok(snaps) = agent.snapshots_with_guid(&dataset).await {
                    for s in snaps {
                        if s.guid == previous.to_guid {
                            base = s.name;
                            base_guid = s.guid;
                            break;
                        }
                    }
                }
            }
        }
    }

    // Refuse a dataset the host namespace cannot see (mount-propagation trap).
    let mount_state = dataset_mount_state(&agent, &dataset).await;
    if let Some((mounted, mountpoint, _)) = &mount_state {
        if !mounted && mountpoint.starts_with('/') {
            fail!(format!(
                "refusing to back up: dataset {dataset} is not mounted on the node (mountpoint {mountpoint}), so zfs send would capture an empty filesystem; mount it in the host namespace and retry"
            ));
        }
    }

    let mut snapshot = format!(
        "buddy-{}-{}",
        Utc::now().format("%Y%m%dT%H%M%SZ"),
        random_suffix()
    );
    let mut target_guid = String::new();
    if let Some(r) = &resume {
        snapshot = r.to_snapshot.clone();
        target_guid = r.to_guid.clone();
    }
    {
        let mut p = job.public.lock().unwrap();
        if resume.is_some() {
            p.snapshot = snapshot.clone();
        }
        p.resumed = was_resume;
        p.incremental = !base.is_empty();
    }

    let mut send_state = resume;
    if send_state.is_none() {
        if let Err(e) = agent.create_snapshot(&dataset, &snapshot).await {
            fail!(format!("snapshotting {dataset}: {e}"));
        }
        {
            let mut p = job.public.lock().unwrap();
            p.snapshot = snapshot.clone();
        }
        if let Ok(snaps) = agent.snapshots_with_guid(&dataset).await {
            for s in snaps {
                if s.name == snapshot {
                    target_guid = s.guid;
                }
            }
        }
        let chain = match ChainState::new(&source, "zfs-send") {
            Ok(c) => c,
            Err(e) => fail!(format!("creating the chain state: {e}")),
        };
        send_state = Some(InstanceSendState {
            receiver: receiver.clone(),
            source: source.clone(),
            dataset: dataset.clone(),
            to_snapshot: snapshot.clone(),
            from_snapshot: base.clone(),
            from_guid: base_guid.clone(),
            to_guid: target_guid.clone(),
            chain,
        });
    }
    let mut send_state = send_state.unwrap();
    if let Err(e) = save_send_state(&state_path, &send_state) {
        fail!(format!("saving the resume state: {e}"));
    }

    let opts = crate::agent::SendStreamOptions {
        dataset: dataset.clone(),
        to: snapshot.clone(),
        from: base.clone(),
        raw,
        recursive: false,
    };
    let expected = agent.estimate_send(&opts).await.unwrap_or_else(|e| {
        tracing::warn!("buddy send: could not estimate {dataset}@{snapshot}: {e}");
        0
    });
    job.public.lock().unwrap().estimated_bytes = expected;

    let stream = match agent.send_stream(&opts).await {
        Ok(s) => s,
        Err(e) => fail!(format!("starting zfs send: {e}")),
    };
    let mut reader = tokio_util::io::StreamReader::new(stream);

    let progress = {
        let job = job.clone();
        move |p: PushProgress| {
            let mut view = job.public.lock().unwrap();
            view.progress = ProgressView {
                chunk: p.chunk,
                plain_bytes: p.plain_bytes,
                sealed_bytes: p.sealed_bytes,
                uploaded: p.uploaded,
                skipped: p.skipped,
            };
        }
    };
    let before_publish = |pushed: &crate::buddy::client::PushResult| -> Result<(), String> {
        require_publish_evidence(
            &dataset,
            &snapshot,
            &send_state.chain.chain,
            mount_state.as_ref().map(|(_, _, used)| *used),
            pushed.plain_bytes,
            !base.is_empty(),
            expected,
            force,
        )
    };

    let persist_template = send_state.clone();
    let persist_path = state_path.clone();
    let save_state = move |chain: &ChainState| {
        let mut next = persist_template.clone();
        next.chain = chain.clone();
        save_send_state(&persist_path, &next)
    };
    let mut chain_state = send_state.chain.clone();
    let (result, err) = client
        .push(PushOptions {
            source: &source,
            kind: "zfs-send",
            reader: &mut reader,
            state: Some(chain_state.clone()),
            from_snapshot: &base,
            to_snapshot: &snapshot,
            from_guid: &base_guid,
            to_guid: &target_guid,
            prune_keep,
            progress: Some(&progress),
            before_publish: Some(&before_publish),
            save_state: &save_state,
        })
        .await;
    if let Some(new_state) = result.state.clone() {
        chain_state = new_state;
    }
    send_state.chain = chain_state;
    if let Some(e) = err {
        let _ = save_send_state(&state_path, &send_state);
        fail!(e);
    }
    let _ = std::fs::remove_file(&state_path);

    // Record the newest published GUID per receiver so a later send to a
    // different buddy can protect this receiver's incremental base.
    if let Err(e) = save_anchor(&receiver, &source, &target_guid) {
        tracing::warn!("buddy send: cannot record the {receiver} anchor: {e}");
    }
    // Keep one Buddy snapshot by default (the base for the next incremental)
    // and honour the caller's retention; older ones leaked forever (R-059).
    // Every other receiver's base is protected from this prune.
    let protected = protected_guids(&state, &dataset, &source, &receiver, &target_guid);
    prune_buddy_snapshots(&agent, &dataset, prune_keep, &protected).await;

    let res = SendResult {
        status: "backed up".to_string(),
        dataset: dataset.clone(),
        source: source.clone(),
        receiver: receiver.clone(),
        snapshot: snapshot.clone(),
        base: base.clone(),
        base_guid: base_guid.clone(),
        to_guid: target_guid.clone(),
        incremental: !base.is_empty(),
        resumed: was_resume,
        raw,
        chain: result.chain.clone(),
        chunks: result.chunks,
        uploaded: result.uploaded,
        skipped: result.skipped,
        plain_bytes: result.plain_bytes,
        sealed_bytes: result.sealed_bytes,
        estimated_bytes: expected,
        pruned_chains: result.pruned_chains,
        duration_seconds: started.elapsed().as_secs_f64(),
    };
    {
        let mut p = job.public.lock().unwrap();
        p.state = JobState::Succeeded;
        p.result = Some(res);
        p.progress = ProgressView {
            chunk: result.chunks as i64,
            plain_bytes: result.plain_bytes,
            sealed_bytes: result.sealed_bytes,
            uploaded: result.uploaded,
            skipped: result.skipped,
        };
        p.finished_at = Some(Utc::now());
    }
    if !schedule_id.is_empty() {
        state
            .buddy_schedules
            .record_result(&schedule_id, &receiver, true, "");
    }
    notify(&state, &job.snapshot()).await;
}

/// Destroy older `buddy-*` snapshots of `dataset`, never touching a snapshot
/// that is another receiver's incremental base (`protected`). `keep` applies
/// only to the unprotected rest, so a fan-out schedule to several buddies
/// cannot destroy one buddy's base while sending to another.
async fn prune_buddy_snapshots(
    agent: &crate::agent::Client,
    dataset: &str,
    keep: usize,
    protected: &HashSet<String>,
) {
    let keep = keep.max(1);
    let snaps = match agent.snapshots_with_guid(dataset).await {
        Ok(snaps) => snaps,
        Err(e) => {
            tracing::warn!("buddy send: cannot list snapshots of {dataset} for pruning: {e}");
            return;
        }
    };
    let own: Vec<(String, String)> = snaps
        .into_iter()
        .filter(|s| s.dataset == dataset && s.name.starts_with("buddy-"))
        .map(|s| (s.name, s.guid))
        .collect();
    for name in prune_plan_buddy(&own, protected, keep) {
        if let Err(e) = agent.destroy_snapshot(dataset, &name).await {
            tracing::warn!("buddy send: cannot destroy old snapshot {dataset}@{name}: {e}");
        }
    }
}

/// The `buddy-*` snapshots to destroy: protected GUIDs survive, and `keep`
/// counts only the unprotected snapshots, newest last (creation order).
fn prune_plan_buddy(
    snapshots: &[(String, String)],
    protected: &HashSet<String>,
    keep: usize,
) -> Vec<String> {
    let keep = keep.max(1);
    let unprotected: Vec<&(String, String)> = snapshots
        .iter()
        .filter(|(_, guid)| guid.is_empty() || !protected.contains(guid))
        .collect();
    if unprotected.len() <= keep {
        return Vec::new();
    }
    unprotected[..unprotected.len() - keep]
        .iter()
        .map(|(name, _)| name.clone())
        .collect()
}

/// Every receiver this dataset is backed up to: the one being sent to now and
/// the other buddies named by schedules for this dataset.
fn dataset_receivers(state: &AppState, dataset: &str, current: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    if seen.insert(current.to_string()) {
        out.push(current.to_string());
    }
    for entry in state.buddy_schedules.list() {
        if entry.dataset != dataset {
            continue;
        }
        for receiver in entry.receiver_list() {
            if seen.insert(receiver.clone()) {
                out.push(receiver);
            }
        }
    }
    out
}

/// The GUIDs that must survive pruning: every receiver's newest published
/// `to_guid`, from this send's target or the locally persisted anchor. No
/// network call is made to the other receivers.
fn protected_guids(
    state: &AppState,
    dataset: &str,
    source: &str,
    current_receiver: &str,
    current_guid: &str,
) -> HashSet<String> {
    let mut protected = HashSet::new();
    for receiver in dataset_receivers(state, dataset, current_receiver) {
        if receiver == current_receiver {
            if !current_guid.is_empty() {
                protected.insert(current_guid.to_string());
            }
        } else if let Some(guid) = read_anchor(&receiver, source) {
            protected.insert(guid);
        }
    }
    protected
}

fn save_send_state(path: &std::path::Path, state: &InstanceSendState) -> Result<(), String> {
    let data = serde_json::to_vec_pretty(state).map_err(|e| e.to_string())?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    crate::chartsrepo::write_atomic(&path.display().to_string(), &data, 0o600, 0o700)
}

pub(crate) async fn dataset_mount_state(
    agent: &crate::agent::Client,
    dataset: &str,
) -> Option<(bool, String, i64)> {
    let datasets = agent.list_datasets().await.ok()?;
    datasets
        .into_iter()
        .find(|d| d.name == dataset)
        .map(|d| (d.mounted, d.mountpoint.clone(), d.used_bytes))
}

/// Decide whether an uploaded stream may be published. A full send must carry a
/// sizeable fraction of the dataset, which catches the mount-namespace trap
/// where `zfs send` captures an empty filesystem. An incremental send is
/// legitimately small next to the dataset, so it is checked against the
/// `zfs send -nP` estimate for the `base..snapshot` range instead
/// (R-005: applying the full-send ratio to increments rejected every real one).
#[allow(clippy::too_many_arguments)]
pub(crate) fn require_publish_evidence(
    dataset: &str,
    snapshot: &str,
    chain: &str,
    used: Option<i64>,
    plain: i64,
    incremental: bool,
    expected: i64,
    force: bool,
) -> Result<(), String> {
    if expected <= 0 && !force {
        return Err(format!(
            "refusing to publish chain {chain}: the agent could not estimate {dataset}@{snapshot}, so a truncated send would go unnoticed"
        ));
    }
    if expected > 0 && plain + send_shortfall_allowance(expected) < expected {
        return Err(format!(
            "the send stream ended early ({plain} of at least {expected} bytes), so nothing was published. Retry the same send to continue chain {chain}"
        ));
    }
    if incremental {
        return Ok(());
    }
    if let Some(used) = used {
        require_stream_matches_dataset(dataset, used, plain)?;
    } else if !force {
        return Err(format!(
            "refusing to publish chain {chain}: the dataset's used space could not be read, so the content check cannot run"
        ));
    }
    Ok(())
}

pub(crate) fn require_stream_matches_dataset(
    dataset: &str,
    used: i64,
    plain: i64,
) -> Result<(), String> {
    const EMPTY_FLOOR: i64 = 1 << 20;
    const MIN_RATIO: f64 = 0.05;
    if used < EMPTY_FLOOR || plain <= 0 {
        return Ok(());
    }
    if (plain as f64) < (used as f64) * MIN_RATIO {
        return Err(format!(
            "the send captured {plain} bytes but {dataset} reports {used} bytes used, so its contents were not included (the dataset is not visible to zfs send in the host mount namespace); the backup was not published"
        ));
    }
    Ok(())
}

async fn notify(state: &AppState, job: &JobPublic) {
    let (event, notif) = match job.state {
        JobState::Succeeded => (
            crate::notifications::EVENT_BACKUP_SUCCESS,
            crate::notifications::Notification {
                title: format!("Backup of {} to {} succeeded", job.dataset, job.receiver),
                message: format!(
                    "source {} chain {} ({} chunks)",
                    job.source,
                    job.result
                        .as_ref()
                        .map(|r| r.chain.clone())
                        .unwrap_or_default(),
                    job.result.as_ref().map(|r| r.chunks).unwrap_or(0)
                ),
                severity: crate::notifications::SEVERITY_INFO.to_string(),
                tags: vec!["white_check_mark".into(), "naslos".into(), "backup".into()],
            },
        ),
        JobState::Failed => (
            crate::notifications::EVENT_BACKUP_FAILURE,
            crate::notifications::Notification {
                title: format!("Backup of {} to {} failed", job.dataset, job.receiver),
                message: format!("source {}: {}", job.source, job.error),
                severity: crate::notifications::SEVERITY_ERROR.to_string(),
                tags: vec!["x".into(), "naslos".into(), "backup".into()],
            },
        ),
        _ => return,
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

/// `GET /api/buddy/jobs`
pub async fn jobs_get(State(state): State<Arc<AppState>>) -> Response {
    json(
        StatusCode::OK,
        serde_json::json!({ "jobs": state.buddy_jobs.list() }),
    )
}

/// `GET|DELETE /api/buddy/jobs/{id}`
pub async fn job_detail_get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match state.buddy_jobs.get(&id) {
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

pub async fn job_detail_delete(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let Some(job) = state.buddy_jobs.get(&id) else {
        return json(
            StatusCode::NOT_FOUND,
            serde_json::json!({ "error": format!("job {id:?} not found") }),
        );
    };
    if job.state() != JobState::Running {
        return json(
            StatusCode::CONFLICT,
            serde_json::json!({ "error": format!("job {id} is already {:?}", job.state()) }),
        );
    }
    // Abort the task; the resume-state file (saved before the push) makes the
    // send resumable.
    if let Some(handle) = job.handle.lock().unwrap().take() {
        handle.abort();
    }
    {
        let mut p = job.public.lock().unwrap();
        p.state = JobState::Cancelled;
        p.finished_at = Some(Utc::now());
        p.error = "cancelled".to_string();
    }
    json(
        StatusCode::OK,
        serde_json::json!({ "status": "cancelling", "id": id }),
    )
}

/// Create and start a send job. Shared by the manual endpoint and the scheduler.
#[allow(clippy::too_many_arguments, clippy::result_large_err)]
pub fn enqueue_job(
    state: &Arc<AppState>,
    agent: Arc<crate::agent::Client>,
    dataset: String,
    source: String,
    receiver: String,
    raw: bool,
    force: bool,
    prune_keep: usize,
    schedule_id: String,
) -> Result<String, JobPublic> {
    let id = random_job_id();
    let schedule_for_job = schedule_id.clone();
    let job = Arc::new(BuddyJob {
        public: Mutex::new(JobPublic {
            id: id.clone(),
            dataset: dataset.clone(),
            source: source.clone(),
            receiver: receiver.clone(),
            prune_keep,
            raw,
            force,
            schedule_id,
            state: JobState::Running,
            started_at: Utc::now(),
            finished_at: None,
            progress: ProgressView::default(),
            incremental: false,
            resumed: false,
            snapshot: String::new(),
            estimated_bytes: 0,
            result: None,
            error: String::new(),
        }),
        handle: Mutex::new(None),
    });
    state
        .buddy_jobs
        .add_if_free(&id, job.clone(), &receiver, &source)?;
    let state_for_task = state.clone();
    let job_for_task = job.clone();
    let handle = tokio::spawn(async move {
        run_send_job(
            state_for_task,
            job_for_task,
            agent,
            dataset,
            source,
            receiver,
            raw,
            force,
            prune_keep,
            schedule_for_job,
        )
        .await;
    });
    *job.handle.lock().unwrap() = Some(handle);
    Ok(id)
}

// Keep Duration referenced for a future timeout guard.
const _: Duration = Duration::from_secs(0);

#[cfg(test)]
mod tests {
    use super::{
        prune_plan_buddy, require_publish_evidence, require_stream_matches_dataset,
        send_shortfall_allowance,
    };
    use std::collections::HashSet;

    fn snaps(entries: &[(&str, &str)]) -> Vec<(String, String)> {
        entries
            .iter()
            .map(|(name, guid)| ((*name).to_string(), (*guid).to_string()))
            .collect()
    }

    fn protected(guids: &[&str]) -> HashSet<String> {
        guids.iter().map(|g| (*g).to_string()).collect()
    }

    #[test]
    fn keep_n_prunes_only_old_unprotected_snapshots() {
        let snapshots = snaps(&[
            ("buddy-1", "g1"),
            ("buddy-2", "g2"),
            ("buddy-3", "g3"),
            ("buddy-4", "g4"),
        ]);
        assert_eq!(
            prune_plan_buddy(&snapshots, &HashSet::new(), 2),
            vec!["buddy-1".to_string(), "buddy-2".to_string()]
        );
        assert!(prune_plan_buddy(&snapshots, &HashSet::new(), 4).is_empty());
    }

    #[test]
    fn two_receivers_with_different_bases_both_survive() {
        // buddy-1 and buddy-2 are the incremental bases of two different
        // receivers; buddy-3 and buddy-4 are this receiver's own history.
        let snapshots = snaps(&[
            ("buddy-1", "base-a"),
            ("buddy-2", "base-b"),
            ("buddy-3", "own-newer"),
            ("buddy-4", "own-newest"),
        ]);
        let keep = protected(&["base-a", "base-b"]);
        // keep=1 prunes the unprotected rest to one, not the other base.
        assert_eq!(
            prune_plan_buddy(&snapshots, &keep, 1),
            vec!["buddy-3".to_string()]
        );
        // Even keep=0 (clamped to one) never destroys a protected base.
        for name in prune_plan_buddy(&snapshots, &keep, 0) {
            assert_ne!(name, "buddy-1");
            assert_ne!(name, "buddy-2");
        }
    }

    #[test]
    fn a_snapshot_with_an_empty_guid_is_prunable() {
        let snapshots = snaps(&[("buddy-1", ""), ("buddy-2", ""), ("buddy-3", "base-a")]);
        assert_eq!(
            prune_plan_buddy(&snapshots, &protected(&["base-a"]), 1),
            vec!["buddy-1".to_string()]
        );
    }

    #[test]
    fn a_full_send_still_requires_dataset_contents() {
        let used = 10 * 1024 * 1024;
        assert!(require_publish_evidence(
            "tank/data",
            "buddy-1",
            "4",
            Some(used),
            used,
            false,
            used,
            false
        )
        .is_ok());
        // An empty full send is still refused (mount-propagation trap).
        assert!(require_publish_evidence(
            "tank/data",
            "buddy-1",
            "4",
            Some(used),
            0,
            false,
            used,
            false
        )
        .is_err());
        assert!(require_stream_matches_dataset("tank/data", used, 1).is_err());
    }

    #[test]
    fn a_small_incremental_after_a_full_send_is_published() {
        let used = 10 * 1024 * 1024;
        let incremental = 64 * 1024;
        assert!(
            require_publish_evidence(
                "tank/data",
                "buddy-2",
                "4",
                Some(used),
                incremental,
                true,
                incremental,
                false
            )
            .is_ok(),
            "an incremental must not be measured against the whole dataset"
        );
        // The same size is still refused when the range estimate says it was truncated.
        assert!(require_publish_evidence(
            "tank/data",
            "buddy-2",
            "4",
            Some(used),
            incremental,
            true,
            used,
            false
        )
        .is_err());
    }

    #[test]
    fn a_half_truncated_small_send_is_refused() {
        let estimate = 2 * 1024 * 1024;
        assert!(1_048_576 + send_shortfall_allowance(estimate) < estimate);
    }

    #[test]
    fn the_measured_estimate_skew_is_tolerated() {
        // +232 B on a 44 KB send and -120 KB on a 57 MB incremental.
        assert!(44_000 + send_shortfall_allowance(44_232) >= 44_232);
        assert!(57_000_000 + send_shortfall_allowance(57_000_000) >= 57_000_000);
    }
}
