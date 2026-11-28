//! Scheduled cloud backups: the corrupt-preserving store, per-destination
//! fan-out and the catch-up ticker.
//!
//! A schedule targets one dataset and one or more destinations. Each due target
//! gets its own job through the same path as "Back up now", so the job
//! manager's `(destination, source)` exclusion covers both. A conflict is
//! recorded as that destination's failure instead of aborting the fan-out.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::json;
use crate::schedule::{
    advance_next_run as advance_fan_out, next_run as cadence_next_run, parse_run_at,
    record_target_result, Cadence, FanOutEntry, JsonStore, JsonStoreItem,
};
use crate::state::AppState;

use super::send::{self, SendParams, StartError};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleEntry {
    pub id: String,
    #[serde(default)]
    pub dataset: String,
    #[serde(default)]
    pub destinations: Vec<String>,
    #[serde(default)]
    pub cadence: Cadence,
    #[serde(default, rename = "runAt")]
    pub run_at: String,
    #[serde(default = "default_keep_lineages", rename = "keepLineages")]
    pub keep_lineages: usize,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, rename = "lastRun", skip_serializing_if = "Option::is_none")]
    pub last_run: Option<DateTime<Utc>>,
    #[serde(
        default,
        rename = "lastResult",
        skip_serializing_if = "String::is_empty"
    )]
    pub last_result: String,
    #[serde(
        default,
        rename = "lastError",
        skip_serializing_if = "String::is_empty"
    )]
    pub last_error: String,
    #[serde(default, rename = "nextRun", skip_serializing_if = "Option::is_none")]
    pub next_run: Option<DateTime<Utc>>,
    #[serde(
        default,
        rename = "destinationResults",
        skip_serializing_if = "HashMap::is_empty"
    )]
    pub destination_results: HashMap<String, String>,
}

fn default_keep_lineages() -> usize {
    2
}

impl Default for ScheduleEntry {
    fn default() -> Self {
        Self {
            id: String::new(),
            dataset: String::new(),
            destinations: Vec::new(),
            cadence: Cadence::Hourly,
            run_at: String::new(),
            keep_lineages: 2,
            enabled: false,
            last_run: None,
            last_result: String::new(),
            last_error: String::new(),
            next_run: None,
            destination_results: HashMap::new(),
        }
    }
}

impl ScheduleEntry {
    /// De-duplicated destinations, order kept.
    pub fn destination_list(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for id in &self.destinations {
            let id = id.trim();
            if id.is_empty() || !seen.insert(id.to_string()) {
                continue;
            }
            out.push(id.to_string());
        }
        out
    }
}

impl JsonStoreItem for ScheduleEntry {
    fn id(&self) -> &str {
        &self.id
    }
}

impl FanOutEntry for ScheduleEntry {
    fn targets(&self) -> Vec<String> {
        self.destination_list()
    }

    fn target_results(&mut self) -> &mut HashMap<String, String> {
        &mut self.destination_results
    }

    fn last_run(&mut self) -> &mut Option<DateTime<Utc>> {
        &mut self.last_run
    }

    fn last_result(&mut self) -> &mut String {
        &mut self.last_result
    }

    fn last_error(&mut self) -> &mut String {
        &mut self.last_error
    }

    fn next_run(&mut self) -> &mut Option<DateTime<Utc>> {
        &mut self.next_run
    }
}

/// Persists schedule entries as JSON with corrupt-file preservation.
pub struct CloudScheduleStore {
    inner: JsonStore<ScheduleEntry>,
}

impl CloudScheduleStore {
    pub fn new(path: &str) -> Self {
        Self {
            inner: JsonStore::new(path),
        }
    }

    pub fn from_env() -> Self {
        Self::new(
            &std::env::var("CLOUD_SCHEDULES")
                .unwrap_or_else(|_| super::DEFAULT_SCHEDULES.to_string()),
        )
    }

    pub fn load(&self) -> Result<(), String> {
        self.inner.load()
    }

    pub fn loaded(&self) -> bool {
        self.inner.loaded()
    }

    pub fn load_error(&self) -> Option<String> {
        self.inner.load_error()
    }

    pub fn require_loaded(&self) -> Result<(), String> {
        self.inner.require_loaded()
    }

    pub fn list(&self) -> Vec<ScheduleEntry> {
        self.inner.list()
    }

    pub fn get(&self, id: &str) -> Option<ScheduleEntry> {
        self.inner.get(id)
    }

    pub fn put(&self, entry: ScheduleEntry) -> Result<(), String> {
        self.inner.put(entry)
    }

    pub fn remove(&self, id: &str) -> Result<(), String> {
        self.inner.require_loaded()?;
        if self.inner.get(id).is_none() {
            return Err(format!("schedule {id:?} not found"));
        }
        self.inner.remove(id)
    }

    /// Record one destination's outcome. `destinationResults` values are
    /// `ok` or `failed: <category>` (docs/api.md).
    pub fn record_result(&self, id: &str, destination: &str, ok: bool, job_err: &str) {
        let now = Utc::now();
        let _ = self.inner.update(id, |entry| {
            let fallback = entry
                .destination_list()
                .first()
                .cloned()
                .unwrap_or_default();
            let next = compute_next_run(entry, now);
            record_target_result(entry, destination, &fallback, ok, job_err, now, next);
            if !ok {
                let key = if destination.trim().is_empty() {
                    fallback
                } else {
                    destination.trim().to_string()
                };
                let value = failure_value(job_err);
                entry.destination_results.insert(key, value);
            }
            // Recompute the aggregate from the (possibly detailed) values:
            // `record_target_result` only recognizes the bare "failed".
            let failed = entry.targets().into_iter().find(|target| {
                entry
                    .destination_results
                    .get(target)
                    .map(|v| v != "ok")
                    .unwrap_or(false)
            });
            match failed {
                None => {
                    entry.last_result = "ok".to_string();
                    entry.last_error.clear();
                }
                Some(target) => {
                    entry.last_result = "failed".to_string();
                    if !ok && !job_err.trim().is_empty() {
                        entry.last_error = format!("backup to {target} failed: {job_err}");
                    } else if entry.last_error.trim().is_empty() {
                        entry.last_error = format!("backup to {target} failed");
                    }
                }
            }
            Ok(())
        });
    }

    pub fn advance_next_run(&self, id: &str, now: DateTime<Utc>) {
        let _ = self.inner.update(id, |entry| {
            let next = compute_next_run(entry, now);
            advance_fan_out(entry, next);
            Ok(())
        });
    }
}

/// `failed: <category>` when the error names one, else `failed`.
pub fn failure_value(error: &str) -> String {
    let error = error.trim();
    if let Some((_, tail)) = error.rsplit_once('(') {
        if let Some(category) = tail.strip_suffix(')') {
            let category = category.trim();
            if !category.is_empty()
                && category.len() <= 32
                && category
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '-' || c.is_ascii_digit())
            {
                return format!("failed: {category}");
            }
        }
    }
    "failed".to_string()
}

pub fn compute_next_run(entry: &ScheduleEntry, now: DateTime<Utc>) -> DateTime<Utc> {
    cadence_next_run(entry.cadence, &entry.run_at, now)
}

/// Validate a schedule against the live destination store and the cadence
/// grammar. The agent dataset check is best-effort, like Buddy's.
pub async fn validate_schedule(state: &AppState, entry: &mut ScheduleEntry) -> Result<(), String> {
    if entry.dataset.trim().is_empty() {
        return Err("dataset is required".to_string());
    }
    crate::server::zfs::validate_dataset_path(&entry.dataset)?;
    let mut destinations = entry.destination_list();
    if destinations.is_empty() {
        return Err("at least one destination is required".to_string());
    }
    let Some(cloud) = state.cloud.as_ref() else {
        return Err("cloud backup is not enabled".to_string());
    };
    cloud.destinations.require_loaded()?;
    for id in &destinations {
        match cloud.destinations.get(id) {
            Some(destination) if destination.enabled => {}
            Some(_) => return Err(format!("destination {id:?} is disabled")),
            None => return Err(format!("destination {id:?} does not exist")),
        }
    }
    parse_run_at(entry.cadence, &entry.run_at)?;
    if !(1..=100).contains(&entry.keep_lineages) {
        return Err("keepLineages must be between 1 and 100".to_string());
    }
    entry.destinations = std::mem::take(&mut destinations);
    if let Some(agent) = &state.agent {
        if let Ok(datasets) = agent.list_datasets().await {
            if !datasets.iter().any(|d| d.name == entry.dataset) {
                return Err(format!(
                    "dataset {:?} is not known to the agent",
                    entry.dataset
                ));
            }
        }
    }
    Ok(())
}

#[derive(Default, Deserialize)]
struct ScheduleRequest {
    #[serde(default)]
    id: String,
    #[serde(default)]
    dataset: String,
    #[serde(default)]
    destinations: Vec<String>,
    #[serde(default)]
    cadence: Cadence,
    #[serde(default, rename = "runAt")]
    run_at: String,
    #[serde(default, rename = "keepLineages")]
    keep_lineages: Option<usize>,
    #[serde(default)]
    enabled: Option<bool>,
}

/// `GET|POST|DELETE /api/cloud/schedules`
pub async fn schedules_get(State(state): State<Arc<AppState>>) -> Response {
    let Some(cloud) = state.cloud.as_ref() else {
        return json(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({ "error": "cloud backup is not enabled" }),
        );
    };
    if cloud.schedules.load_error().is_some() {
        return json(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({ "error": cloud.schedules.load_error() }),
        );
    }
    json(
        StatusCode::OK,
        serde_json::json!({ "schedules": cloud.schedules.list() }),
    )
}

pub async fn schedules_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let Some(cloud) = state.cloud.as_ref() else {
        return json(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({ "error": "cloud backup is not enabled" }),
        );
    };
    if cloud.schedules.load_error().is_some() {
        return json(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({ "error": cloud.schedules.load_error() }),
        );
    }
    let req: ScheduleRequest = match super::super::server::zfs::read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let mut entry = ScheduleEntry {
        id: req.id.trim().to_string(),
        dataset: req.dataset.trim().to_string(),
        destinations: req.destinations,
        cadence: req.cadence,
        run_at: req.run_at.trim().to_string(),
        keep_lineages: req.keep_lineages.unwrap_or(2),
        enabled: req.enabled.unwrap_or(true),
        ..Default::default()
    };
    if entry.id.is_empty() {
        let mut buf = [0u8; 16];
        let _ = getrandom::fill(&mut buf);
        entry.id = buf.iter().map(|b| format!("{b:02x}")).collect();
    } else if let Some(existing) = cloud.schedules.get(&entry.id) {
        entry.last_run = existing.last_run;
        entry.last_result = existing.last_result;
        entry.last_error = existing.last_error;
        entry.destination_results = existing.destination_results;
    }
    if let Err(e) = validate_schedule(&state, &mut entry).await {
        return json(StatusCode::BAD_REQUEST, serde_json::json!({ "error": e }));
    }
    entry.next_run = Some(compute_next_run(&entry, Utc::now()));
    if let Err(e) = cloud.schedules.put(entry.clone()) {
        return json(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "error": format!("saving the schedule: {e}") }),
        );
    }
    json(StatusCode::OK, serde_json::json!({ "schedule": entry }))
}

pub async fn schedules_delete(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(cloud) = state.cloud.as_ref() else {
        return json(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({ "error": "cloud backup is not enabled" }),
        );
    };
    let id = query.get("id").cloned().unwrap_or_default();
    if id.trim().is_empty() {
        return json(
            StatusCode::BAD_REQUEST,
            serde_json::json!({ "error": "id is required" }),
        );
    }
    match cloud.schedules.remove(id.trim()) {
        Ok(()) => json(
            StatusCode::OK,
            serde_json::json!({ "status": "deleted", "id": id.trim() }),
        ),
        Err(e) => json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e })),
    }
}

/// Start one job per due destination.
pub async fn run_due_schedules(state: &Arc<AppState>, now: DateTime<Utc>) {
    let Some(cloud) = state.cloud.as_ref() else {
        return;
    };
    for entry in cloud.schedules.list() {
        if !entry.enabled {
            continue;
        }
        match entry.next_run {
            Some(next) if next > now => continue,
            None => {
                let mut e = entry.clone();
                e.next_run = Some(compute_next_run(&e, now));
                let _ = cloud.schedules.put(e);
                continue;
            }
            _ => {}
        }
        start_schedule_jobs(state, entry).await;
    }
}

async fn start_schedule_jobs(state: &Arc<AppState>, entry: ScheduleEntry) {
    let mut started = 0;
    for destination in entry.destination_list() {
        let params = SendParams {
            dataset: entry.dataset.clone(),
            destination: destination.clone(),
            keep_lineages: entry.keep_lineages,
            force_full: false,
            schedule_id: entry.id.clone(),
        };
        match send::enqueue_send(state, params) {
            Ok(_) => started += 1,
            Err(error) => {
                let message = match error {
                    StartError::NotFound(e) | StartError::BadRequest(e) => e,
                    StartError::ServiceUnavailable(e) => e,
                    StartError::Conflict(e) => e,
                };
                tracing::warn!(
                    "cloud scheduler: {} -> {} skipped: {message}",
                    entry.id,
                    destination
                );
                if let Some(cloud) = state.cloud.as_ref() {
                    cloud
                        .schedules
                        .record_result(&entry.id, &destination, false, &message);
                }
            }
        }
    }
    if started > 0 {
        if let Some(cloud) = state.cloud.as_ref() {
            cloud.schedules.advance_next_run(&entry.id, Utc::now());
        }
    }
}

/// Catch-up once at startup, then tick on the configured interval.
pub async fn run_scheduler(state: Arc<AppState>) {
    let interval = state
        .cloud
        .as_ref()
        .map(|cloud| cloud.scheduler_interval)
        .unwrap_or(std::time::Duration::from_secs(60));
    tracing::info!("cloud scheduler started (interval {:?})", interval);
    crate::schedule::run_catch_up_then_tick(interval, |now| {
        let state = state.clone();
        async move { run_due_schedules(&state, now).await }
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::Cadence;

    fn entry() -> ScheduleEntry {
        ScheduleEntry {
            id: "s1".into(),
            dataset: "tank/data".into(),
            destinations: vec!["d1".into(), "d2".into()],
            cadence: Cadence::Daily,
            run_at: "02:30".into(),
            keep_lineages: 2,
            enabled: true,
            ..Default::default()
        }
    }

    #[test]
    fn the_on_disk_shape_uses_the_api_field_names() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cloud-schedules.json");
        let store = CloudScheduleStore::new(path.to_str().unwrap());
        store.load().unwrap();
        let stamp = Utc::now();
        store
            .put(ScheduleEntry {
                next_run: Some(stamp),
                last_run: Some(stamp),
                ..entry()
            })
            .unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        for field in [
            "\"runAt\"",
            "\"keepLineages\"",
            "\"lastRun\"",
            "\"nextRun\"",
            "\"destinations\"",
        ] {
            assert!(raw.contains(field), "missing {field}: {raw}");
        }
        assert!(!raw.contains("run_at"), "{raw}");
        assert!(!raw.contains("keep_lineages"), "{raw}");
    }

    #[test]
    fn per_destination_results_record_the_category() {
        let store = CloudScheduleStore::new("");
        store.put(entry()).unwrap();
        store.record_result("s1", "d1", true, "");
        store.record_result("s1", "d2", false, "list failed (auth)");
        let saved = store.get("s1").unwrap();
        assert_eq!(saved.destination_results["d1"], "ok");
        assert_eq!(saved.destination_results["d2"], "failed: auth");
        assert_eq!(saved.last_result, "failed");
        assert!(saved.last_error.contains("d2"));
        assert!(saved.last_run.is_some());
        assert!(saved.next_run.is_some());
    }

    #[test]
    fn failure_values_fall_back_when_no_category_is_named() {
        assert_eq!(failure_value("list failed (auth)"), "failed: auth");
        assert_eq!(failure_value("something broke"), "failed");
        assert_eq!(failure_value(""), "failed");
    }

    #[test]
    fn duplicate_destinations_are_collapsed() {
        let mut e = entry();
        e.destinations = vec!["d1".into(), "d1".into(), " d2 ".into()];
        assert_eq!(e.destination_list(), vec!["d1", "d2"]);
    }
}
