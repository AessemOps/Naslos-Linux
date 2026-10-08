//! Async app lifecycle jobs (port of `server/app_jobs.go`, FR-APP-18).
//!
//! A cold install blocks on Helm's `--wait` for up to five minutes; running it
//! inside the request meant a closed modal killed the install. Jobs decouple the
//! two: the handler validates, enqueues and answers 202, while the install runs
//! in a server-owned task the operator polls. Jobs live in memory; the app
//! record remains the durable outcome.

use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::apps::{InstallRequest, View};

/// The lifecycle of one app job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    Running,
    Succeeded,
    Failed,
}

/// Which lifecycle operation the job runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobKind {
    Install,
    Upgrade,
    Uninstall,
}

/// The coarse install phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStage {
    Preparing,
    Installing,
    Finalizing,
}

impl JobStage {
    pub fn parse(s: &str) -> JobStage {
        match s {
            "installing" => JobStage::Installing,
            "finalizing" => JobStage::Finalizing,
            _ => JobStage::Preparing,
        }
    }
}

/// The serializable snapshot of one job.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct JobPublic {
    pub id: String,
    pub kind: JobKind,
    pub app: String,
    pub state: JobState,
    pub stage: JobStage,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
    #[serde(rename = "startedAt")]
    pub started_at: DateTime<Utc>,
    #[serde(rename = "finishedAt", skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    #[serde(
        rename = "baseDomain",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub base_domain: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
}

/// One async lifecycle operation.
pub struct AppJob {
    public: Mutex<JobPublic>,
    pub kind: JobKind,
    pub app: String,
    pub request: Option<InstallRequest>,
    pub values: Option<serde_json::Map<String, serde_json::Value>>,
    pub result: Mutex<Option<View>>,
}

impl AppJob {
    pub fn snapshot(&self) -> JobPublic {
        self.public.lock().unwrap().clone()
    }

    pub fn state(&self) -> JobState {
        self.public.lock().unwrap().state
    }

    fn set_stage(&self, stage: &str, message: &str) {
        let mut p = self.public.lock().unwrap();
        p.stage = JobStage::parse(stage);
        p.message = message.to_string();
    }

    fn finish(&self, state: JobState, err: String, view: Option<View>) {
        {
            let mut p = self.public.lock().unwrap();
            p.state = state;
            p.error = err;
            p.finished_at = Some(Utc::now());
        }
        *self.result.lock().unwrap() = view;
    }
}

/// Keeps running jobs plus the last ~20 finished ones.
#[derive(Default)]
pub struct AppJobManager {
    inner: Mutex<JobManagerInner>,
}

#[derive(Default)]
struct JobManagerInner {
    jobs: HashMap<String, Arc<AppJob>>,
    order: Vec<String>,
}

impl AppJobManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// A running job for an app, whatever its kind (Helm serialises operations
    /// on a release).
    pub fn conflicting(&self, app: &str) -> Option<JobPublic> {
        let inner = self.inner.lock().unwrap();
        for job in inner.jobs.values() {
            if job.state() == JobState::Running && job.app == app {
                return Some(job.snapshot());
            }
        }
        None
    }

    pub fn add(&self, job: Arc<AppJob>) {
        let mut inner = self.inner.lock().unwrap();
        let id = job.snapshot().id;
        inner.jobs.insert(id.clone(), job);
        inner.order.push(id);
        // Keep running jobs plus the last ~20 finished ones.
        let mut kept: Vec<String> = Vec::new();
        let mut finished = 0;
        for id in inner.order.iter().rev() {
            let Some(j) = inner.jobs.get(id) else {
                continue;
            };
            let running = j.state() == JobState::Running;
            if running || finished < 20 {
                kept.push(id.clone());
                if !running {
                    finished += 1;
                }
            }
        }
        kept.reverse();
        inner.jobs.retain(|id, _| kept.contains(id));
        inner.order = kept;
    }

    pub fn get(&self, id: &str) -> Option<Arc<AppJob>> {
        self.inner.lock().unwrap().jobs.get(id).cloned()
    }

    pub fn list(&self) -> Vec<JobPublic> {
        let inner = self.inner.lock().unwrap();
        inner
            .order
            .iter()
            .filter_map(|id| inner.jobs.get(id).map(|j| j.snapshot()))
            .collect()
    }

    /// Register and start a lifecycle job. The caller must have checked for a
    /// conflict first.
    pub fn enqueue(
        self: &Arc<Self>,
        kind: JobKind,
        app: &str,
        base_domain: &str,
        request: Option<InstallRequest>,
        values: Option<serde_json::Map<String, serde_json::Value>>,
        manager: Option<Arc<crate::apps::Manager>>,
    ) -> JobPublic {
        let job = Arc::new(AppJob {
            public: Mutex::new(JobPublic {
                id: random_job_id(),
                kind,
                app: app.to_string(),
                state: JobState::Running,
                stage: JobStage::Preparing,
                message: String::new(),
                started_at: Utc::now(),
                finished_at: None,
                base_domain: base_domain.to_string(),
                error: String::new(),
            }),
            kind,
            app: app.to_string(),
            request,
            values,
            result: Mutex::new(None),
        });
        self.add(job.clone());
        let snapshot = job.snapshot();

        let manager_for_task = self.clone();
        let job_for_task = job.clone();
        tokio::spawn(async move {
            run_job(manager_for_task, job_for_task, manager).await;
        });
        snapshot
    }
}

async fn run_job(
    _manager: Arc<AppJobManager>,
    job: Arc<AppJob>,
    manager: Option<Arc<crate::apps::Manager>>,
) {
    let progress = {
        let job = job.clone();
        move |stage: &str, message: &str| job.set_stage(stage, message)
    };
    let progress_ref: Option<&crate::apps::ProgressFn> = Some(&progress);

    let Some(manager) = manager else {
        job.finish(
            JobState::Failed,
            "app management is not available".into(),
            None,
        );
        return;
    };

    let outcome: Result<Option<View>, String> = match job.kind {
        JobKind::Install => {
            let req = job.request.clone().unwrap_or_default();
            manager.install(req, progress_ref).await.map(Some)
        }
        JobKind::Upgrade => manager
            .upgrade(&job.app, job.values.as_ref(), progress_ref)
            .await
            .map(Some),
        JobKind::Uninstall => manager
            .uninstall(&job.app, progress_ref)
            .await
            .map(|_| None),
    };

    match outcome {
        Ok(view) => job.finish(JobState::Succeeded, String::new(), view),
        Err(e) => job.finish(JobState::Failed, e, None),
    }
}

/// A short random hex job id.
fn random_job_id() -> String {
    let mut buf = [0u8; 8];
    if getrandom::getrandom(&mut buf).is_err() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        return format!("job-{nanos:x}");
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running_job(manager: &Arc<AppJobManager>, app: &str) -> Arc<AppJob> {
        let job = Arc::new(AppJob {
            public: Mutex::new(JobPublic {
                id: random_job_id(),
                kind: JobKind::Install,
                app: app.into(),
                state: JobState::Running,
                stage: JobStage::Preparing,
                message: String::new(),
                started_at: Utc::now(),
                finished_at: None,
                base_domain: String::new(),
                error: String::new(),
            }),
            kind: JobKind::Install,
            app: app.into(),
            request: None,
            values: None,
            result: Mutex::new(None),
        });
        manager.add(job.clone());
        job
    }

    #[test]
    fn conflicting_detects_a_running_job() {
        let m = Arc::new(AppJobManager::new());
        let _j = running_job(&m, "nginx");
        assert!(m.conflicting("nginx").is_some());
        assert!(m.conflicting("other").is_none());
    }

    #[test]
    fn list_preserves_enqueue_order() {
        let m = Arc::new(AppJobManager::new());
        let a = running_job(&m, "a");
        let b = running_job(&m, "b");
        let ids: Vec<String> = m.list().into_iter().map(|j| j.id).collect();
        assert_eq!(ids, vec![a.snapshot().id, b.snapshot().id]);
    }
}
