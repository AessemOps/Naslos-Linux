//! Scheduled buddy backups (port of `server/buddy_schedules.go`).
//!
//! Entries persist as JSON beside the peer registry. The runner ticks (default a
//! minute) and starts a send job through the same path as "Back up now", so the
//! job manager's (receiver, source) exclusion covers both. A run missed while
//! the API was down fires once at startup (catch-up).

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::buddy::keys::load_identity;
use crate::buddy::peers::validate_source;
use crate::json;
use crate::schedule::{
    advance_next_run as advance_fan_out, next_run as cadence_next_run, parse_run_at,
    record_target_result, Cadence, FanOutEntry, JsonStore, JsonStoreItem,
};
use crate::state::AppState;

/// One scheduled backup.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScheduleEntry {
    pub id: String,
    #[serde(default)]
    pub dataset: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub receiver: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub receivers: Vec<String>,
    #[serde(
        default,
        skip_serializing_if = "HashMap::is_empty",
        rename = "receiverResults"
    )]
    pub receiver_results: HashMap<String, String>,
    #[serde(default)]
    pub cadence: Cadence,
    #[serde(default, rename = "runAt")]
    pub run_at: String,
    #[serde(default, rename = "pruneKeep", skip_serializing_if = "is_zero")]
    pub prune_keep: usize,
    /// Force a full send every Nth scheduled run, so an endless incremental
    /// lineage is periodically rebased onto a fresh full.
    #[serde(default = "default_full_every", rename = "fullEvery")]
    pub full_every: usize,
    /// Scheduled fires since the last forced full.
    #[serde(default, rename = "sendsSinceFull")]
    pub sends_since_full: usize,
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
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

fn default_full_every() -> usize {
    30
}

impl ScheduleEntry {
    /// The buddies this entry backs up to, de-duplicated, order kept.
    pub fn receiver_list(&self) -> Vec<String> {
        if self.receivers.is_empty() {
            let r = self.receiver.trim();
            return if r.is_empty() {
                Vec::new()
            } else {
                vec![r.to_string()]
            };
        }
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for r in &self.receivers {
            let r = r.trim();
            if r.is_empty() || !seen.insert(r.to_string()) {
                continue;
            }
            out.push(r.to_string());
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
        self.receiver_list()
    }

    fn target_results(&mut self) -> &mut HashMap<String, String> {
        &mut self.receiver_results
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

/// Persists schedule entries as JSON.
pub struct ScheduleStore {
    inner: JsonStore<ScheduleEntry>,
}

impl ScheduleStore {
    pub fn new(path: &str) -> Self {
        Self {
            inner: JsonStore::new(path),
        }
    }

    pub fn from_env() -> Self {
        Self::new(
            &std::env::var("BUDDY_SCHEDULES")
                .unwrap_or_else(|_| "/var/lib/naslos/buddy-schedules.json".to_string()),
        )
    }

    pub fn load(&self) -> Result<(), String> {
        self.inner.load()
    }

    pub fn list(&self) -> Vec<ScheduleEntry> {
        self.inner.list()
    }

    pub fn get(&self, id: &str) -> Option<ScheduleEntry> {
        self.inner.get(id)
    }

    /// Persist-then-swap: a failed save leaves memory identical to the file,
    /// so a deleted schedule cannot reappear after a restart (B-3).
    pub fn put(&self, e: ScheduleEntry) -> Result<(), String> {
        self.inner.put(e)
    }

    pub fn remove(&self, id: &str) -> Result<(), String> {
        self.inner.require_loaded()?;
        if self.inner.get(id).is_none() {
            return Err(format!("schedule {id:?} not found"));
        }
        self.inner.remove(id)
    }

    /// Record one destination's outcome; the entry is "ok" only when every
    /// destination succeeded. Best effort by design (it is progress, not a
    /// security decision), but the candidate is still saved before the live
    /// map is replaced so a failed write never divides the two.
    pub fn record_result(&self, id: &str, receiver: &str, ok: bool, job_err: &str) {
        let now = Utc::now();
        let _ = self.inner.update(id, |e| {
            let fallback = e.receiver.clone();
            let next = compute_next_run(e, now);
            record_target_result(e, receiver, &fallback, ok, job_err, now, next);
            Ok(())
        });
    }

    /// Count a fire of the schedule; a reset to zero follows a forced full.
    pub fn record_sends(&self, id: &str, forced_full: bool) {
        let _ = self.inner.update(id, |e| {
            e.sends_since_full = if forced_full {
                0
            } else {
                e.sends_since_full.saturating_add(1)
            };
            Ok(())
        });
    }

    /// Advance an entry's next run (called once when a fan-out starts).
    pub fn advance_next_run(&self, id: &str, now: DateTime<Utc>) {
        let _ = self.inner.update(id, |e| {
            let next = compute_next_run(e, now);
            advance_fan_out(e, next);
            Ok(())
        });
    }
}

/// The next run strictly after `now`.
pub fn compute_next_run(e: &ScheduleEntry, now: DateTime<Utc>) -> DateTime<Utc> {
    cadence_next_run(e.cadence, &e.run_at, now)
}

/// Validate a schedule. The agent dataset check is best-effort.
pub async fn validate_schedule(state: &AppState, e: &mut ScheduleEntry) -> Result<(), String> {
    if e.dataset.trim().is_empty() {
        return Err("dataset is required".to_string());
    }
    if e.dataset.contains('@') || e.dataset.starts_with('/') || e.dataset.contains("..") {
        return Err(format!("invalid dataset {:?}", e.dataset));
    }
    validate_source(e.source.trim())?;
    let receivers = e.receiver_list();
    if receivers.is_empty() {
        return Err("at least one buddy (receiver) is required".to_string());
    }
    let mut normalized = Vec::new();
    for receiver in receivers {
        normalized.push(super::buddy_restore::normalize_receiver_url(&receiver)?);
    }
    e.receivers = normalized.clone();
    e.receiver = normalized[0].clone();
    parse_run_at(e.cadence, &e.run_at)?;
    if let Some(agent) = &state.agent {
        if let Ok(datasets) = agent.list_datasets().await {
            if !datasets.iter().any(|d| d.name == e.dataset) {
                return Err(format!("dataset {:?} is not known to the agent", e.dataset));
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
    source: String,
    #[serde(default)]
    receiver: String,
    #[serde(default)]
    receivers: Vec<String>,
    #[serde(default)]
    cadence: Cadence,
    #[serde(default, rename = "runAt")]
    run_at: String,
    #[serde(default, rename = "pruneKeep")]
    prune_keep: usize,
    #[serde(default, rename = "fullEvery")]
    full_every: Option<usize>,
    #[serde(default)]
    enabled: Option<bool>,
}

/// `GET|POST|DELETE /api/buddy/schedules`
pub async fn schedules_get(State(state): State<Arc<AppState>>) -> Response {
    json(
        StatusCode::OK,
        serde_json::json!({ "schedules": state.buddy_schedules.list() }),
    )
}

pub async fn schedules_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let req: ScheduleRequest = match super::zfs::read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let mut entry = ScheduleEntry {
        id: req.id.trim().to_string(),
        dataset: req.dataset.trim().to_string(),
        source: req.source.trim().to_string(),
        receiver: req.receiver.trim().to_string(),
        receivers: req.receivers,
        cadence: req.cadence,
        run_at: req.run_at.trim().to_string(),
        prune_keep: req.prune_keep,
        full_every: req.full_every.unwrap_or_else(default_full_every),
        enabled: req.enabled.unwrap_or(true),
        ..Default::default()
    };
    if entry.id.is_empty() {
        let mut buf = [0u8; 16];
        let _ = getrandom::fill(&mut buf);
        entry.id = buf.iter().map(|b| format!("{b:02x}")).collect();
    } else if let Some(existing) = state.buddy_schedules.get(&entry.id) {
        entry.last_run = existing.last_run;
        entry.last_result = existing.last_result;
        entry.last_error = existing.last_error;
        entry.receiver_results = existing.receiver_results;
        entry.sends_since_full = existing.sends_since_full;
    }
    if let Err(e) = validate_schedule(&state, &mut entry).await {
        return json(StatusCode::BAD_REQUEST, serde_json::json!({ "error": e }));
    }
    entry.next_run = Some(compute_next_run(&entry, Utc::now()));
    if let Err(e) = state.buddy_schedules.put(entry.clone()) {
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
    let id = query.get("id").cloned().unwrap_or_default();
    if id.trim().is_empty() {
        return json(
            StatusCode::BAD_REQUEST,
            serde_json::json!({ "error": "id is required" }),
        );
    }
    match state.buddy_schedules.remove(id.trim()) {
        Ok(()) => json(
            StatusCode::OK,
            serde_json::json!({ "status": "deleted", "id": id.trim() }),
        ),
        Err(e) => json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e })),
    }
}

/// Start one job per due, enabled entry.
pub async fn run_due_schedules(state: &Arc<AppState>, now: DateTime<Utc>) {
    for entry in state.buddy_schedules.list() {
        if !entry.enabled {
            continue;
        }
        match entry.next_run {
            Some(next) if next > now => continue,
            None => {
                // Never computed (older store): schedule from now, do not fire.
                let mut e = entry.clone();
                e.next_run = Some(compute_next_run(&e, now));
                let _ = state.buddy_schedules.put(e);
                continue;
            }
            _ => {}
        }
        start_schedule_job(state, entry).await;
    }
}

/// Whether this fire must force a full send. Retention must be enabled, or
/// the new full would accumulate beside the old lineage forever.
fn force_full_due(entry: &ScheduleEntry) -> bool {
    entry.prune_keep > 0 && entry.full_every > 0 && entry.sends_since_full + 1 >= entry.full_every
}

async fn start_schedule_job(state: &Arc<AppState>, entry: ScheduleEntry) {
    let Some(agent) = state.agent.clone() else {
        tracing::warn!("buddy scheduler: skipping {}: no agent client", entry.id);
        return;
    };
    if load_identity(&identity_path()).is_err() {
        tracing::warn!("buddy scheduler: skipping {}: no identity", entry.id);
        return;
    }
    let receivers = entry.receiver_list();
    if receivers.is_empty() {
        return;
    }
    let force = force_full_due(&entry);
    let mut started = 0;
    for receiver in receivers {
        if let Some(conflict) = state.buddy_jobs.conflicting(&receiver, &entry.source) {
            tracing::warn!(
                "buddy scheduler: {}: buddy {} already has job {} running",
                entry.id,
                receiver,
                conflict.job_id()
            );
            continue;
        }
        if super::buddy_send::enqueue_job(
            state,
            agent.clone(),
            entry.dataset.clone(),
            entry.source.clone(),
            receiver.clone(),
            true,
            force,
            entry.prune_keep,
            entry.id.clone(),
        )
        .is_err()
        {
            continue;
        }
        started += 1;
    }
    if started > 0 {
        state.buddy_schedules.record_sends(&entry.id, force);
        state
            .buddy_schedules
            .advance_next_run(&entry.id, Utc::now());
    }
}

fn identity_path() -> String {
    std::env::var("BUDDY_IDENTITY")
        .unwrap_or_else(|_| "/var/lib/naslos/buddy-identity.json".to_string())
}

fn scheduler_interval() -> std::time::Duration {
    std::env::var("BUDDY_SCHEDULER_INTERVAL_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map(std::time::Duration::from_millis)
        .unwrap_or(std::time::Duration::from_secs(60))
}

/// Catch-up once, then run due schedules on the interval.
pub async fn run_scheduler(state: Arc<AppState>) {
    let interval = scheduler_interval();
    tracing::info!("buddy scheduler started (interval {:?})", interval);
    crate::schedule::run_catch_up_then_tick(interval, |now| {
        let state = state.clone();
        async move { run_due_schedules(&state, now).await }
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, TimeZone, Weekday};

    #[test]
    fn an_old_schedule_file_loads_with_full_every_defaulting_to_30() {
        let json = r#"{"id":"s1","dataset":"tank/data","source":"naslos-data/data","receiver":"https://b","enabled":true}"#;
        let entry: ScheduleEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.full_every, 30);
        assert_eq!(entry.sends_since_full, 0);
    }

    #[test]
    fn full_every_is_configurable_and_round_trips() {
        let json = r#"{"id":"s1","fullEvery":7,"sendsSinceFull":3}"#;
        let entry: ScheduleEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.full_every, 7);
        assert_eq!(entry.sends_since_full, 3);
        let back: ScheduleEntry =
            serde_json::from_slice(&serde_json::to_vec(&entry).unwrap()).unwrap();
        assert_eq!(back.full_every, 7);
        assert_eq!(back.sends_since_full, 3);
    }

    #[test]
    fn a_full_is_forced_on_the_nth_send_and_the_counter_resets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("schedules.json");
        let store = ScheduleStore::new(path.to_str().unwrap());
        store.load().unwrap();
        store
            .put(ScheduleEntry {
                id: "s1".into(),
                full_every: 3,
                ..Default::default()
            })
            .unwrap();
        let mut entry = store.get("s1").unwrap();
        // Retention disabled: never force, even on the Nth run.
        entry.prune_keep = 0;
        entry.sends_since_full = 2;
        assert!(!force_full_due(&entry));
        entry.prune_keep = 5;
        assert!(force_full_due(&entry));
        entry.sends_since_full = 0;
        assert!(!force_full_due(&entry));
        store.record_sends("s1", false);
        entry = store.get("s1").unwrap();
        entry.prune_keep = 5;
        assert!(!force_full_due(&entry));
        store.record_sends("s1", false);
        entry = store.get("s1").unwrap();
        entry.prune_keep = 5;
        assert!(force_full_due(&entry));
        store.record_sends("s1", true);
        assert_eq!(store.get("s1").unwrap().sends_since_full, 0);
    }

    #[test]
    fn run_at_parsing() {
        assert!(parse_run_at(Cadence::Daily, "02:30").is_ok());
        assert!(parse_run_at(Cadence::Daily, "2:30").is_err());
        assert!(parse_run_at(Cadence::Daily, "24:00").is_err());
        assert!(parse_run_at(Cadence::Weekly, "Mon 02:30").is_ok());
        assert!(parse_run_at(Cadence::Weekly, "Funday 02:30").is_err());
        assert!(parse_run_at(Cadence::Hourly, "").is_ok());
    }

    #[test]
    fn a_failed_schedule_delete_keeps_the_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("schedules.json");
        let store = ScheduleStore::new(path.to_str().unwrap());
        store.load().unwrap();
        store
            .put(ScheduleEntry {
                id: "s1".into(),
                ..Default::default()
            })
            .unwrap();
        // A directory at the store path makes the atomic rename fail even when
        // the tests run as root (permission bits would not).
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(store.remove("s1").is_err());
        assert!(
            store.get("s1").is_some(),
            "a failed delete must not drop the live entry"
        );
    }

    #[test]
    fn a_corrupt_store_is_preserved_and_refuses_mutations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("buddy-schedules.json");
        let corrupt = b"{this is not json";
        std::fs::write(&path, corrupt).unwrap();
        let store = ScheduleStore::new(path.to_str().unwrap());
        let error = store.load().unwrap_err();
        assert!(error.contains("refusing mutations"), "{error}");

        let backups: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("buddy-schedules.json.corrupt-")
            })
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(std::fs::read(backups[0].path()).unwrap(), corrupt);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            corrupt,
            "the original corrupt file must stay in place"
        );

        assert!(store
            .put(ScheduleEntry {
                id: "new".into(),
                ..Default::default()
            })
            .is_err());
        assert!(store.remove("new").is_err());
        assert_eq!(
            std::fs::read(&path).unwrap(),
            corrupt,
            "a refused mutation must never overwrite the corrupt file"
        );
    }

    #[test]
    fn the_on_disk_shape_keeps_the_legacy_field_names() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("buddy-schedules.json");
        let store = ScheduleStore::new(path.to_str().unwrap());
        store.load().unwrap();
        let stamp = Utc.with_ymd_and_hms(2026, 1, 5, 10, 15, 0).unwrap();
        store
            .put(ScheduleEntry {
                id: "s1".into(),
                dataset: "tank/data".into(),
                source: "naslos-a/data".into(),
                receiver: "https://a".into(),
                receivers: vec!["https://a".into()],
                receiver_results: HashMap::from([("https://a".into(), "ok".into())]),
                cadence: Cadence::Daily,
                run_at: "02:30".into(),
                prune_keep: 2,
                full_every: default_full_every(),
                sends_since_full: 0,
                enabled: true,
                last_run: Some(stamp),
                last_result: "ok".into(),
                last_error: String::new(),
                next_run: Some(stamp),
            })
            .unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        for field in [
            "\"runAt\"",
            "\"pruneKeep\"",
            "\"receiverResults\"",
            "\"lastRun\"",
            "\"lastResult\"",
            "\"nextRun\"",
        ] {
            assert!(raw.contains(field), "missing {field}: {raw}");
        }
        assert!(!raw.contains("receiver_results"), "{raw}");
        assert!(!raw.contains("run_at"), "{raw}");
    }

    #[test]
    fn per_receiver_results_aggregate_into_one_outcome() {
        let store = ScheduleStore::new("");
        store
            .put(ScheduleEntry {
                id: "s1".into(),
                receiver: "https://a".into(),
                receivers: vec!["https://a".into(), "https://b".into()],
                enabled: true,
                ..Default::default()
            })
            .unwrap();
        store.record_result("s1", "https://a", true, "");
        store.record_result("s1", "https://b", false, "boom");
        let entry = store.get("s1").unwrap();
        assert_eq!(entry.receiver_results["https://a"], "ok");
        assert_eq!(entry.receiver_results["https://b"], "failed");
        assert_eq!(entry.last_result, "failed");
        assert_eq!(entry.last_error, "backup to https://b failed: boom");
        assert!(entry.last_run.is_some());
        assert!(entry.next_run.is_some());
    }

    #[test]
    fn next_run_is_strictly_after_now() {
        let now = Utc.with_ymd_and_hms(2026, 1, 5, 10, 15, 0).unwrap(); // Monday
        let mut e = ScheduleEntry {
            cadence: Cadence::Hourly,
            ..Default::default()
        };
        assert!(compute_next_run(&e, now) > now);
        e.cadence = Cadence::Daily;
        e.run_at = "02:30".into();
        let next = compute_next_run(&e, now);
        assert!(next > now);
        assert_eq!(next.format("%H:%M").to_string(), "02:30");
        e.cadence = Cadence::Weekly;
        e.run_at = "Mon 02:30".into();
        let next = compute_next_run(&e, now);
        assert_eq!(next.weekday(), Weekday::Mon);
        assert!(next > now);
    }
}
