//! Scheduled buddy backups (port of `server/buddy_schedules.go`).
//!
//! Entries persist as JSON beside the peer registry. The runner ticks (default a
//! minute) and starts a send job through the same path as "Back up now", so the
//! job manager's (receiver, source) exclusion covers both. A run missed while
//! the API was down fires once at startup (catch-up).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use chrono::{DateTime, Datelike, Duration as ChronoDuration, TimeZone, Timelike, Utc, Weekday};
use serde::{Deserialize, Serialize};

use crate::buddy::keys::load_identity;
use crate::buddy::peers::validate_source;
use crate::json;
use crate::state::AppState;

/// The scheduler interval for a schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cadence {
    #[default]
    Hourly,
    Daily,
    Weekly,
}

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

/// Persists schedule entries as JSON.
pub struct ScheduleStore {
    path: String,
    entries: Mutex<HashMap<String, ScheduleEntry>>,
}

impl ScheduleStore {
    pub fn new(path: &str) -> Self {
        Self {
            path: path.to_string(),
            entries: Mutex::new(HashMap::new()),
        }
    }

    pub fn from_env() -> Self {
        Self::new(
            &std::env::var("BUDDY_SCHEDULES")
                .unwrap_or_else(|_| "/var/lib/naslos/buddy-schedules.json".to_string()),
        )
    }

    pub fn load(&self) -> Result<(), String> {
        let mut guard = self.entries.lock().unwrap();
        guard.clear();
        if self.path.is_empty() {
            return Ok(());
        }
        let data = match std::fs::read(&self.path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.to_string()),
        };
        let list: Vec<ScheduleEntry> = serde_json::from_slice(&data)
            .map_err(|e| format!("parsing schedule store {}: {e}", self.path))?;
        for e in list {
            if e.id.is_empty() {
                continue;
            }
            guard.insert(e.id.clone(), e);
        }
        Ok(())
    }

    fn save(&self, guard: &HashMap<String, ScheduleEntry>) -> Result<(), String> {
        if self.path.is_empty() {
            return Ok(());
        }
        let mut list: Vec<&ScheduleEntry> = guard.values().collect();
        list.sort_by(|a, b| a.id.cmp(&b.id));
        let mut data =
            serde_json::to_vec_pretty(&list).map_err(|e| format!("encoding schedules: {e}"))?;
        data.push(b'\n');
        crate::chartsrepo::write_atomic(&self.path, &data, 0o600, 0o750)
    }

    pub fn list(&self) -> Vec<ScheduleEntry> {
        let guard = self.entries.lock().unwrap();
        let mut out: Vec<ScheduleEntry> = guard.values().cloned().collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    pub fn get(&self, id: &str) -> Option<ScheduleEntry> {
        self.entries.lock().unwrap().get(id).cloned()
    }

    pub fn put(&self, e: ScheduleEntry) -> Result<(), String> {
        let mut guard = self.entries.lock().unwrap();
        guard.insert(e.id.clone(), e);
        self.save(&guard)
    }

    pub fn remove(&self, id: &str) -> Result<(), String> {
        let mut guard = self.entries.lock().unwrap();
        if guard.remove(id).is_none() {
            return Err(format!("schedule {id:?} not found"));
        }
        self.save(&guard)
    }

    /// Record one destination's outcome; the entry is "ok" only when every
    /// destination succeeded.
    pub fn record_result(&self, id: &str, receiver: &str, ok: bool, job_err: &str) {
        let mut guard = self.entries.lock().unwrap();
        let Some(e) = guard.get_mut(id) else { return };
        let now = Utc::now();
        e.last_run = Some(now);
        let receiver = if receiver.is_empty() {
            e.receiver.clone()
        } else {
            receiver.to_string()
        };
        e.receiver_results.insert(
            receiver.clone(),
            if ok { "ok".into() } else { "failed".into() },
        );
        let failed = e.receiver_list().into_iter().find(|d| {
            e.receiver_results
                .get(d)
                .map(|v| v == "failed")
                .unwrap_or(false)
        });
        if failed.is_none() {
            e.last_result = "ok".to_string();
            e.last_error.clear();
            e.next_run = Some(compute_next_run(e, now));
            let _ = self.save(&guard);
            return;
        }
        let failed = failed.unwrap();
        e.last_result = "failed".to_string();
        if !ok && !job_err.trim().is_empty() {
            e.last_error = format!("backup to {receiver} failed: {job_err}");
        } else if e.last_error.trim().is_empty() {
            e.last_error = format!("backup to {failed} failed");
        }
        e.next_run = Some(compute_next_run(e, now));
        let _ = self.save(&guard);
    }

    /// Advance an entry's next run (called once when a fan-out starts).
    pub fn advance_next_run(&self, id: &str, now: DateTime<Utc>) {
        let mut guard = self.entries.lock().unwrap();
        if let Some(e) = guard.get_mut(id) {
            e.next_run = Some(compute_next_run(e, now));
            let _ = self.save(&guard);
        }
    }
}

fn parse_hhmm(text: &str) -> Option<(u32, u32)> {
    if text.len() != 5 {
        return None;
    }
    let (h, m) = text.split_once(':')?;
    let h: u32 = h.parse().ok()?;
    let m: u32 = m.parse().ok()?;
    if h > 23 || m > 59 {
        return None;
    }
    Some((h, m))
}

fn parse_run_at(cadence: Cadence, run_at: &str) -> Result<(Weekday, u32, u32), String> {
    let text = run_at.trim();
    match cadence {
        Cadence::Hourly => Ok((Weekday::Sun, 0, 0)),
        Cadence::Daily => parse_hhmm(text)
            .map(|(h, m)| (Weekday::Sun, h, m))
            .ok_or_else(|| "runAt must be HH:MM in UTC (e.g. 02:30)".to_string()),
        Cadence::Weekly => {
            let parts: Vec<&str> = text.split_whitespace().collect();
            if parts.len() != 2 {
                return Err("runAt must be \"Mon 02:30\" in UTC for weekly schedules".to_string());
            }
            let days = [
                (Weekday::Mon, "mon"),
                (Weekday::Tue, "tue"),
                (Weekday::Wed, "wed"),
                (Weekday::Thu, "thu"),
                (Weekday::Fri, "fri"),
                (Weekday::Sat, "sat"),
                (Weekday::Sun, "sun"),
            ];
            let day = days
                .iter()
                .find(|(_, name)| name.eq_ignore_ascii_case(&parts[0].to_lowercase()))
                .map(|(d, _)| *d)
                .ok_or_else(|| {
                    "runAt weekday must be one of Sun Mon Tue Wed Thu Fri Sat".to_string()
                })?;
            let (h, m) = parse_hhmm(parts[1])
                .ok_or_else(|| "runAt time must be HH:MM in UTC (e.g. Mon 02:30)".to_string())?;
            Ok((day, h, m))
        }
    }
}

/// The next run strictly after `now`.
pub fn compute_next_run(e: &ScheduleEntry, now: DateTime<Utc>) -> DateTime<Utc> {
    match e.cadence {
        Cadence::Hourly => {
            let truncated = now
                .with_minute(0)
                .and_then(|t| t.with_second(0))
                .and_then(|t| t.with_nanosecond(0))
                .unwrap_or(now);
            truncated + ChronoDuration::hours(1)
        }
        Cadence::Daily => {
            let Ok((_, h, m)) = parse_run_at(e.cadence, &e.run_at) else {
                return now + ChronoDuration::hours(24);
            };
            let today = now.date_naive();
            let mut next = Utc
                .with_ymd_and_hms(today.year(), today.month(), today.day(), h, m, 0)
                .single()
                .unwrap_or(now);
            if next <= now {
                next += ChronoDuration::hours(24);
            }
            next
        }
        Cadence::Weekly => {
            let Ok((weekday, h, m)) = parse_run_at(e.cadence, &e.run_at) else {
                return now + ChronoDuration::hours(24 * 7);
            };
            let today = now.date_naive();
            let mut next = Utc
                .with_ymd_and_hms(today.year(), today.month(), today.day(), h, m, 0)
                .single()
                .unwrap_or(now);
            if next <= now {
                next += ChronoDuration::hours(24);
            }
            while next.weekday() != weekday {
                next += ChronoDuration::hours(24);
            }
            next
        }
    }
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
        enabled: req.enabled.unwrap_or(true),
        ..Default::default()
    };
    if entry.id.is_empty() {
        let mut buf = [0u8; 16];
        let _ = getrandom::getrandom(&mut buf);
        entry.id = buf.iter().map(|b| format!("{b:02x}")).collect();
    } else if let Some(existing) = state.buddy_schedules.get(&entry.id) {
        entry.last_run = existing.last_run;
        entry.last_result = existing.last_result;
        entry.last_error = existing.last_error;
        entry.receiver_results = existing.receiver_results;
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
        super::buddy_send::enqueue_job(
            state,
            agent.clone(),
            entry.dataset.clone(),
            entry.source.clone(),
            receiver,
            true,
            false,
            entry.prune_keep,
            entry.id.clone(),
        );
        started += 1;
    }
    if started > 0 {
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
    run_due_schedules(&state, Utc::now()).await;
    tracing::info!("buddy scheduler started (interval {:?})", interval);
    let mut ticker = tokio::time::interval(interval);
    ticker.tick().await;
    loop {
        ticker.tick().await;
        run_due_schedules(&state, Utc::now()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
