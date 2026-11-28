//! In-memory cloud backup jobs (send, restore and verify).
//!
//! The manager keeps every running job plus the newest 20 finished ones. The
//! running set is the exclusion lock: one job per `(destination, source)` for
//! send/verify and one per restore target dataset. Cancellation is cooperative
//! through a `CancellationToken` so a cancelled job still runs its cleanup
//! (destroy a partial dataset, persist resume state) instead of dying with the
//! task handle.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

/// How many finished jobs are kept in the history.
pub const KEEP_FINISHED: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JobKind {
    Send,
    Restore,
    Verify,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            JobKind::Send => "send",
            JobKind::Restore => "restore",
            JobKind::Verify => "verify",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

/// Live progress. Optional fields are omitted until they are known.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobProgress {
    pub chunk: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_chunks: Option<i64>,
    pub plain_bytes: i64,
    pub uploaded: usize,
    pub skipped: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub links: Option<usize>,
}

/// The outcome detail of a finished job.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobResult {
    pub chain: String,
    pub snapshot: String,
    pub kind: String,
    pub plain_bytes: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pruned: Option<usize>,
    #[serde(
        rename = "missingIndices",
        default,
        skip_serializing_if = "HashMap::is_empty"
    )]
    pub missing_indices: HashMap<String, Vec<i64>>,
    #[serde(
        rename = "badIndices",
        default,
        skip_serializing_if = "HashMap::is_empty"
    )]
    pub bad_indices: HashMap<String, Vec<i64>>,
}

/// The public job shape (`docs/api.md`, DM-12).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobPublic {
    pub id: String,
    pub kind: JobKind,
    pub destination: String,
    pub source: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub dataset: String,
    pub state: JobState,
    pub started_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub error: String,
    pub progress: JobProgress,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<JobResult>,
}

/// A job plus the handles needed to stop it and observe its state.
pub struct CloudJob {
    public: Mutex<JobPublic>,
    cancel: CancellationToken,
}

impl CloudJob {
    pub fn snapshot(&self) -> JobPublic {
        self.public.lock().unwrap().clone()
    }

    pub fn state(&self) -> JobState {
        self.public.lock().unwrap().state
    }

    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// The current progress snapshot.
    pub fn progress(&self) -> JobProgress {
        self.public.lock().unwrap().progress.clone()
    }

    /// Replace progress (kept short and atomic for the UI poller).
    pub fn set_progress(&self, progress: JobProgress) {
        self.public.lock().unwrap().progress = progress;
    }

    /// Update progress in place.
    pub fn update_progress(&self, mutate: impl FnOnce(&mut JobProgress)) {
        mutate(&mut self.public.lock().unwrap().progress);
    }

    /// First-writer-wins terminal transition: a racing cancel or error never
    /// overwrites `cancelled`.
    pub fn finish(&self, state: JobState, error: String, result: Option<JobResult>) {
        let mut p = self.public.lock().unwrap();
        if p.state != JobState::Running {
            return;
        }
        p.state = state;
        p.error = error;
        p.result = result;
        p.finished_at = Some(Utc::now());
    }
}

#[derive(Default)]
struct Inner {
    jobs: HashMap<String, Arc<CloudJob>>,
    order: Vec<String>,
}

/// Keeps running jobs plus the newest finished ones.
#[derive(Default)]
pub struct CloudJobManager {
    inner: Mutex<Inner>,
}

/// Why a job could not be registered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueConflict {
    /// One job per (destination, source) is already running.
    Source,
    /// One restore per target dataset is already running.
    Dataset,
}

impl CloudJobManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// The running job for one `(destination, source)` pair.
    pub fn conflicting(&self, destination: &str, source: &str) -> Option<JobPublic> {
        let inner = self.inner.lock().unwrap();
        for job in inner.jobs.values() {
            let p = job.snapshot();
            if p.state == JobState::Running && p.destination == destination && p.source == source {
                return Some(p);
            }
        }
        None
    }

    /// The running restore for one target dataset.
    pub fn conflicting_dataset(&self, dataset: &str) -> Option<JobPublic> {
        let inner = self.inner.lock().unwrap();
        for job in inner.jobs.values() {
            let p = job.snapshot();
            if p.state == JobState::Running && p.kind == JobKind::Restore && p.dataset == dataset {
                return Some(p);
            }
        }
        None
    }

    /// Register a job, checking and inserting under one lock. `dataset` is
    /// only checked for restore jobs (none of the others write a dataset).
    #[allow(clippy::result_large_err)]
    pub fn add_if_free(
        &self,
        id: &str,
        job: Arc<CloudJob>,
        destination: &str,
        source: &str,
        dataset: &str,
        kind: JobKind,
    ) -> Result<(), EnqueueConflict> {
        let mut inner = self.inner.lock().unwrap();
        for existing in inner.jobs.values() {
            let p = existing.snapshot();
            if p.state != JobState::Running {
                continue;
            }
            if p.destination == destination && p.source == source {
                return Err(EnqueueConflict::Source);
            }
            if kind == JobKind::Restore
                && p.kind == JobKind::Restore
                && !dataset.is_empty()
                && p.dataset == dataset
            {
                return Err(EnqueueConflict::Dataset);
            }
        }
        Self::insert_locked(&mut inner, id, job);
        Ok(())
    }

    fn insert_locked(inner: &mut Inner, id: &str, job: Arc<CloudJob>) {
        inner.jobs.insert(id.to_string(), job);
        inner.order.push(id.to_string());
        let mut kept: Vec<String> = Vec::new();
        let mut finished = 0;
        for id in inner.order.iter().rev() {
            if let Some(j) = inner.jobs.get(id) {
                if j.state() == JobState::Running || finished < KEEP_FINISHED {
                    kept.push(id.clone());
                    if j.state() != JobState::Running {
                        finished += 1;
                    }
                }
            }
        }
        kept.reverse();
        inner.jobs.retain(|id, _| kept.contains(id));
        inner.order = kept;
    }

    pub fn has_running(&self) -> bool {
        self.inner
            .lock()
            .unwrap()
            .jobs
            .values()
            .any(|j| j.state() == JobState::Running)
    }

    /// Whether a running job uses this destination (delete guard).
    pub fn has_running_for_destination(&self, destination: &str) -> bool {
        self.inner.lock().unwrap().jobs.values().any(|job| {
            let p = job.snapshot();
            p.state == JobState::Running && p.destination == destination
        })
    }

    /// Running send/restore jobs whose dataset overlaps `name` (destroy guard).
    pub fn running_dataset_overlaps(&self, name: &str) -> Option<JobPublic> {
        let inner = self.inner.lock().unwrap();
        for job in inner.jobs.values() {
            let p = job.snapshot();
            if p.state != JobState::Running || p.dataset.is_empty() {
                continue;
            }
            if p.kind == JobKind::Restore && p.dataset == name {
                return Some(p);
            }
            // A send reads the source dataset (and its children) and it would
            // be destroyed mid-stream.
            if p.kind == JobKind::Send
                && (p.dataset == name || p.dataset.starts_with(&format!("{name}/")))
            {
                return Some(p);
            }
        }
        None
    }

    pub fn get(&self, id: &str) -> Option<Arc<CloudJob>> {
        self.inner.lock().unwrap().jobs.get(id).cloned()
    }

    /// Running jobs first (newest start first), then finished jobs newest
    /// first, capped to the history bound. The UI resumes the first running job.
    pub fn list(&self) -> Vec<JobPublic> {
        let inner = self.inner.lock().unwrap();
        let mut jobs: Vec<JobPublic> = inner
            .order
            .iter()
            .filter_map(|id| inner.jobs.get(id).map(|j| j.snapshot()))
            .collect();
        jobs.sort_by(|a, b| {
            let ar = a.state == JobState::Running;
            let br = b.state == JobState::Running;
            br.cmp(&ar).then_with(|| b.started_at.cmp(&a.started_at))
        });
        jobs.truncate(KEEP_FINISHED + jobs.iter().filter(|j| j.state == JobState::Running).count());
        jobs
    }

    /// Request cancellation. Returns `None` when the id is unknown, and the
    /// current state when the job already finished.
    pub fn cancel(&self, id: &str) -> Result<(), Option<JobState>> {
        let Some(job) = self.get(id) else {
            return Err(None);
        };
        let state = job.state();
        if state != JobState::Running {
            return Err(Some(state));
        }
        job.cancel.cancel();
        Ok(())
    }

    /// Test helper: register a running job without spawning it.
    #[cfg(test)]
    pub(crate) fn remember_running_for_test(
        &self,
        id: &str,
        kind: JobKind,
        destination: &str,
        source: &str,
        dataset: &str,
    ) {
        let job = Arc::new(CloudJob {
            public: Mutex::new(JobPublic {
                id: id.to_string(),
                kind,
                destination: destination.to_string(),
                source: source.to_string(),
                dataset: dataset.to_string(),
                state: JobState::Running,
                started_at: Utc::now(),
                finished_at: None,
                error: String::new(),
                progress: JobProgress::default(),
                result: None,
            }),
            cancel: CancellationToken::new(),
        });
        self.add_if_free(id, job, destination, source, dataset, kind)
            .unwrap();
    }

    /// Test helper: register a finished job.
    #[cfg(test)]
    pub(crate) fn remember_finished_for_test(
        &self,
        id: &str,
        kind: JobKind,
        destination: &str,
        source: &str,
        dataset: &str,
    ) {
        let job = Arc::new(CloudJob {
            public: Mutex::new(JobPublic {
                id: id.to_string(),
                kind,
                destination: destination.to_string(),
                source: source.to_string(),
                dataset: dataset.to_string(),
                state: JobState::Succeeded,
                started_at: Utc::now(),
                finished_at: Some(Utc::now()),
                error: String::new(),
                progress: JobProgress::default(),
                result: None,
            }),
            cancel: CancellationToken::new(),
        });
        self.add_if_free(id, job, destination, source, dataset, kind)
            .unwrap();
        // add_if_free only registers; the state is already terminal.
    }
}

/// Create a running job record and return it with its cancellation token.
pub fn new_job(
    id: String,
    kind: JobKind,
    destination: String,
    source: String,
    dataset: String,
) -> (Arc<CloudJob>, CancellationToken) {
    let token = CancellationToken::new();
    let job = Arc::new(CloudJob {
        public: Mutex::new(JobPublic {
            id,
            kind,
            destination,
            source,
            dataset,
            state: JobState::Running,
            started_at: Utc::now(),
            finished_at: None,
            error: String::new(),
            progress: JobProgress::default(),
            result: None,
        }),
        cancel: token.clone(),
    });
    (job, token)
}

/// A random 16-byte hex job id.
pub fn random_job_id() -> String {
    let mut buf = [0u8; 16];
    if getrandom::fill(&mut buf).is_err() {
        return format!("{:032x}", Utc::now().timestamp_nanos_opt().unwrap_or(0));
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// The check a push/restore/verify loop calls between chunks.
pub fn cancel_check(token: CancellationToken) -> impl Fn() -> Result<(), String> + Send + Sync {
    move || {
        if token.is_cancelled() {
            Err("cancelled".to_string())
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_send_for_the_same_pair_is_refused_but_a_different_pair_is_not() {
        let manager = CloudJobManager::new();
        manager.remember_running_for_test("a", JobKind::Send, "dest", "tank/data", "tank/data");
        let job = new_job(
            "b".into(),
            JobKind::Send,
            "dest".into(),
            "tank/data".into(),
            "tank/data".into(),
        )
        .0;
        assert_eq!(
            manager
                .add_if_free("b", job, "dest", "tank/data", "tank/data", JobKind::Send)
                .unwrap_err(),
            EnqueueConflict::Source
        );
        let job = new_job(
            "c".into(),
            JobKind::Send,
            "dest".into(),
            "tank/other".into(),
            "tank/other".into(),
        )
        .0;
        manager
            .add_if_free("c", job, "dest", "tank/other", "tank/other", JobKind::Send)
            .unwrap();
    }

    #[test]
    fn a_second_restore_into_the_same_dataset_is_refused() {
        let manager = CloudJobManager::new();
        manager.remember_running_for_test(
            "a",
            JobKind::Restore,
            "dest",
            "tank/src",
            "tank/restored",
        );
        let job = new_job(
            "b".into(),
            JobKind::Restore,
            "other".into(),
            "tank/other-src".into(),
            "tank/restored".into(),
        )
        .0;
        assert_eq!(
            manager
                .add_if_free(
                    "b",
                    job,
                    "other",
                    "tank/other-src",
                    "tank/restored",
                    JobKind::Restore
                )
                .unwrap_err(),
            EnqueueConflict::Dataset
        );
    }

    #[test]
    fn cancel_is_final_and_unknown_ids_are_reported_separately() {
        let manager = CloudJobManager::new();
        manager.remember_finished_for_test("done", JobKind::Send, "d", "s", "s");
        assert_eq!(manager.cancel("missing"), Err(None));
        assert_eq!(manager.cancel("done"), Err(Some(JobState::Succeeded)));
        manager.remember_running_for_test("run", JobKind::Send, "d", "s2", "s2");
        assert_eq!(manager.cancel("run"), Ok(()));
        // The job body observes the token and finishes; a terminal job never
        // gets its state overwritten by a racing error.
        let job = manager.get("run").unwrap();
        job.finish(JobState::Cancelled, "cancelled".into(), None);
        job.finish(JobState::Failed, "late error".into(), None);
        assert_eq!(job.state(), JobState::Cancelled);
    }

    #[test]
    fn the_list_puts_running_jobs_first_and_caps_history() {
        let manager = CloudJobManager::new();
        for i in 0..25 {
            manager.remember_finished_for_test(
                &format!("finished-{i:02}"),
                JobKind::Send,
                "d",
                &format!("s/{i}"),
                &format!("s/{i}"),
            );
        }
        manager.remember_running_for_test("running", JobKind::Send, "d", "s/live", "s/live");
        let list = manager.list();
        assert_eq!(list[0].id, "running");
        assert_eq!(
            list.iter().filter(|j| j.state != JobState::Running).count(),
            KEEP_FINISHED
        );
    }

    #[test]
    fn dataset_overlap_sees_the_source_and_a_restore_target() {
        let manager = CloudJobManager::new();
        manager.remember_running_for_test("send", JobKind::Send, "d", "tank/src", "tank/src");
        assert!(manager.running_dataset_overlaps("tank/src").is_some());
        // Destroying a child does not disturb a non-recursive send of the
        // parent; destroying the parent would disturb a send of the child.
        assert!(manager.running_dataset_overlaps("tank/src/child").is_none());
        manager.remember_running_for_test(
            "send-child",
            JobKind::Send,
            "d2",
            "tank/src/child",
            "tank/src/child",
        );
        assert!(manager.running_dataset_overlaps("tank/src").is_some());
        assert!(manager.running_dataset_overlaps("tank/other").is_none());
        manager.remember_running_for_test(
            "restore",
            JobKind::Restore,
            "d3",
            "tank/src",
            "tank/back",
        );
        assert!(manager.running_dataset_overlaps("tank/back").is_some());
        assert!(manager
            .running_dataset_overlaps("tank/back/child")
            .is_none());
    }

    #[test]
    fn running_jobs_survive_the_history_cap() {
        let manager = CloudJobManager::new();
        manager.remember_running_for_test("live", JobKind::Send, "d", "s/live", "s/live");
        for i in 0..30 {
            manager.remember_finished_for_test(
                &format!("f-{i:02}"),
                JobKind::Send,
                "d",
                &format!("s/f{i}"),
                &format!("s/f{i}"),
            );
        }
        assert!(manager.get("live").is_some());
        assert!(manager.has_running());
    }
}
