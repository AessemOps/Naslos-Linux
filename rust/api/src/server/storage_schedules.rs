//! Scheduled storage maintenance: periodic snapshots with retention and
//! per-pool scrub schedules (FR-STO-10/11).
//!
//! Entries persist as JSON beside the other state files. The runner ticks
//! (default a minute), creates snapshots through the agent, and prunes only the
//! snapshots it named itself (`naslos-auto-<schedule>-*`), so a user snapshot
//! is never deleted by retention. A run missed while the API was down fires
//! once at startup (catch-up), matching the buddy scheduler.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::json;
use crate::schedule::{next_run, parse_run_at, Cadence};
use crate::state::AppState;

/// Snapshot names are owned by the scheduler: a schedule may only delete names
/// that start with its own prefix.
pub const SNAPSHOT_NAME_PREFIX: &str = "naslos-auto-";

/// Upper bound on retained snapshots per dataset.
pub const MAX_KEEP: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScheduleKind {
    Snapshot,
    Scrub,
}

/// One scheduled storage task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageSchedule {
    pub id: String,
    pub kind: ScheduleKind,
    /// Dataset for a snapshot schedule, pool name for a scrub schedule.
    pub target: String,
    #[serde(default)]
    pub cadence: Cadence,
    #[serde(default, rename = "runAt")]
    pub run_at: String,
    /// How many scheduler-owned snapshots to keep (0 = keep all). Always 0 for
    /// scrub schedules.
    #[serde(default)]
    pub keep: usize,
    /// Snapshot child datasets too (`zfs snapshot -r`).
    #[serde(default)]
    pub recursive: bool,
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

impl Default for StorageSchedule {
    fn default() -> Self {
        Self {
            id: String::new(),
            kind: ScheduleKind::Snapshot,
            target: String::new(),
            cadence: Cadence::Daily,
            run_at: String::new(),
            keep: 0,
            recursive: false,
            enabled: true,
            last_run: None,
            last_result: String::new(),
            last_error: String::new(),
            next_run: None,
        }
    }
}

/// Generates an injective token for the schedule's snapshot-name prefix.
///
/// Ids are opaque store keys, so the token hex-encodes every byte instead of
/// stripping characters: `"a-b"` and `"ab"` must not map to the same prefix, or
/// one schedule's retention could delete another schedule's snapshots.
fn schedule_token(id: &str) -> String {
    if id.is_empty() {
        return "schedule".to_string();
    }
    id.as_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

/// The prefix every snapshot this schedule creates starts with.
pub fn snapshot_prefix(id: &str) -> String {
    format!("{SNAPSHOT_NAME_PREFIX}{}-", schedule_token(id))
}

/// A schedule-owned snapshot name: `naslos-auto-<token>-<UTC timestamp>`.
pub fn snapshot_name(id: &str, now: DateTime<Utc>) -> String {
    format!("{}{}", snapshot_prefix(id), now.format("%Y%m%dT%H%M%SZ"))
}

/// Plan retention: from the full snapshot paths under `root` (oldest first),
/// return the `(dataset, snapshot)` pairs to destroy so each dataset keeps the
/// newest `keep` scheduler-owned snapshots. Only names carrying `prefix` are
/// considered, so user snapshots and other schedules' snapshots are untouched.
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
    // dataset are the oldest ones and are the ones retention removes.
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

/// Whether `zpool status`'s scan line reports a scrub or resilver in progress.
pub fn scrub_in_progress(scan: &str) -> bool {
    let scan = scan.to_ascii_lowercase();
    scan.contains("scrub in progress") || scan.contains("resilver in progress")
}

/// The `scan:` field from a raw `zpool status` body. Scrub polling uses the
/// status body (not the full health endpoint, which also samples `zpool iostat`)
/// so a slow or stuck scrub costs one cheap command per tick.
pub fn scan_line(status: &str) -> String {
    for line in status.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("scan:") {
            return rest.trim().to_string();
        }
    }
    String::new()
}

/// A finished scrub's outcome, from `zpool status`'s scan line. `None` while a
/// scrub is still running, has never run, or the pool is only resilvering.
pub fn scrub_finished(scan: &str) -> Option<Result<String, String>> {
    let scan = scan.trim();
    let lower = scan.to_ascii_lowercase();
    if scrub_in_progress(scan) {
        return None;
    }
    if !(lower.starts_with("scrub ")
        || lower.contains("scrub repaired")
        || lower.contains("scrub finished"))
    {
        return None;
    }
    if lower.contains("with 0 errors") {
        Some(Ok(scan.to_string()))
    } else {
        Some(Err(scan.to_string()))
    }
}

/// Persists schedule entries as JSON.
pub struct StorageScheduleStore {
    path: String,
    entries: Mutex<HashMap<String, StorageSchedule>>,
}

impl StorageScheduleStore {
    pub fn new(path: &str) -> Self {
        Self {
            path: path.to_string(),
            entries: Mutex::new(HashMap::new()),
        }
    }

    pub fn from_env() -> Self {
        Self::new(
            &std::env::var("STORAGE_SCHEDULES")
                .unwrap_or_else(|_| "/var/lib/naslos/storage-schedules.json".to_string()),
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
        let list: Vec<StorageSchedule> = serde_json::from_slice(&data)
            .map_err(|e| format!("parsing storage schedule store {}: {e}", self.path))?;
        for e in list {
            if e.id.is_empty() {
                continue;
            }
            guard.insert(e.id.clone(), e);
        }
        Ok(())
    }

    fn save(&self, guard: &HashMap<String, StorageSchedule>) -> Result<(), String> {
        if self.path.is_empty() {
            return Ok(());
        }
        let mut list: Vec<&StorageSchedule> = guard.values().collect();
        list.sort_by(|a, b| a.id.cmp(&b.id));
        let mut data =
            serde_json::to_vec_pretty(&list).map_err(|e| format!("encoding schedules: {e}"))?;
        data.push(b'\n');
        crate::chartsrepo::write_atomic(&self.path, &data, 0o600, 0o750)
    }

    pub fn list(&self) -> Vec<StorageSchedule> {
        let guard = self.entries.lock().unwrap();
        let mut out: Vec<StorageSchedule> = guard.values().cloned().collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    pub fn get(&self, id: &str) -> Option<StorageSchedule> {
        self.entries.lock().unwrap().get(id).cloned()
    }

    pub fn put(&self, e: StorageSchedule) -> Result<(), String> {
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

    /// Advance the next run without recording an outcome.
    pub fn advance_next_run(&self, id: &str, now: DateTime<Utc>) {
        let mut guard = self.entries.lock().unwrap();
        if let Some(e) = guard.get_mut(id) {
            e.next_run = Some(next_run(e.cadence, &e.run_at, now));
            let _ = self.save(&guard);
        }
    }

    pub fn record_result(&self, id: &str, result: &str, err: &str) {
        let mut guard = self.entries.lock().unwrap();
        let Some(e) = guard.get_mut(id) else { return };
        let now = Utc::now();
        e.last_run = Some(now);
        e.last_result = result.to_string();
        e.last_error = err.to_string();
        e.next_run = Some(next_run(e.cadence, &e.run_at, now));
        let _ = self.save(&guard);
    }

    /// Record a failed attempt that must be retried on the next tick (the agent
    /// was unreachable), without consuming the schedule's interval.
    pub fn record_failure_keep_next(&self, id: &str, err: &str) {
        let mut guard = self.entries.lock().unwrap();
        let Some(e) = guard.get_mut(id) else { return };
        e.last_run = Some(Utc::now());
        e.last_result = "failed".to_string();
        e.last_error = err.to_string();
        let _ = self.save(&guard);
    }
}

/// Validate a schedule. The agent checks are best-effort, like the buddy
/// scheduler's dataset check.
pub async fn validate_schedule(state: &AppState, e: &mut StorageSchedule) -> Result<(), String> {
    if e.target.trim().is_empty() {
        return Err("a dataset (snapshots) or pool (scrub) is required".to_string());
    }
    parse_run_at(e.cadence, &e.run_at)?;
    if e.keep > MAX_KEEP {
        return Err(format!("keep must be at most {MAX_KEEP}"));
    }
    match e.kind {
        ScheduleKind::Snapshot => {
            crate::server::zfs::validate_dataset_path(e.target.trim())?;
            e.target = e.target.trim().to_string();
            if let Some(agent) = &state.agent {
                if let Ok(datasets) = agent.list_datasets().await {
                    if !datasets.iter().any(|d| d.name == e.target) {
                        return Err(format!("dataset {:?} is not known to the agent", e.target));
                    }
                }
            }
        }
        ScheduleKind::Scrub => {
            if !crate::server::zfs::is_valid_pool_name(e.target.trim()) {
                return Err(format!("invalid pool name {:?}", e.target));
            }
            e.target = e.target.trim().to_string();
            if e.cadence == Cadence::Hourly {
                return Err("scrub schedules must be daily or weekly".to_string());
            }
            if e.keep != 0 {
                return Err("a scrub schedule cannot keep snapshots".to_string());
            }
            e.recursive = false;
            if let Some(agent) = &state.agent {
                if let Ok(pools) = agent.list_pools().await {
                    if !pools.iter().any(|p| p.name == e.target) {
                        return Err(format!("pool {:?} is not known to the agent", e.target));
                    }
                }
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
    kind: ScheduleKindWrapper,
    #[serde(default)]
    target: String,
    #[serde(default)]
    cadence: Option<Cadence>,
    #[serde(default, rename = "runAt")]
    run_at: String,
    #[serde(default)]
    keep: usize,
    #[serde(default)]
    recursive: bool,
    #[serde(default)]
    enabled: Option<bool>,
}

/// `kind` is required on create; the wrapper gives a clear error otherwise.
#[derive(Default, Deserialize)]
struct ScheduleKindWrapper(Option<ScheduleKind>);

impl ScheduleKindWrapper {
    fn take(self) -> Result<ScheduleKind, String> {
        self.0
            .ok_or_else(|| "kind must be \"snapshot\" or \"scrub\"".to_string())
    }
}

#[allow(clippy::result_large_err)]
fn agent(state: &AppState) -> Result<Arc<crate::agent::Client>, Response> {
    state.agent.clone().ok_or_else(|| {
        json(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({ "error": "agent client not configured" }),
        )
    })
}

/// `GET|POST|DELETE /api/storage/schedules`
pub async fn schedules_get(State(state): State<Arc<AppState>>) -> Response {
    json(
        StatusCode::OK,
        serde_json::json!({ "schedules": state.storage_schedules.list() }),
    )
}

pub async fn schedules_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let req: ScheduleRequest = match super::zfs::read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let kind = match req.kind.take() {
        Ok(k) => k,
        Err(e) => return bad_request(&e),
    };
    let mut entry = StorageSchedule {
        id: req.id.trim().to_string(),
        kind,
        target: req.target.trim().to_string(),
        cadence: req.cadence.unwrap_or(Cadence::Daily),
        run_at: req.run_at.trim().to_string(),
        keep: req.keep,
        recursive: req.recursive,
        enabled: req.enabled.unwrap_or(true),
        ..Default::default()
    };
    if entry.id.is_empty() {
        let mut buf = [0u8; 16];
        let _ = getrandom::fill(&mut buf);
        entry.id = buf.iter().map(|b| format!("{b:02x}")).collect();
    } else if let Some(existing) = state.storage_schedules.get(&entry.id) {
        entry.last_run = existing.last_run;
        entry.last_result = existing.last_result;
        entry.last_error = existing.last_error;
    } else {
        // Only server-generated ids may become retention prefixes. Accepting an
        // arbitrary client id would let two schedules collide after token
        // encoding and prune each other's snapshots.
        return bad_request("unknown schedule id; omit id to create a new schedule");
    }
    if let Err(e) = validate_schedule(&state, &mut entry).await {
        return bad_request(&e);
    }
    entry.next_run = Some(next_run(entry.cadence, &entry.run_at, Utc::now()));
    if let Err(e) = state.storage_schedules.put(entry.clone()) {
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
        return bad_request("id is required");
    }
    match state.storage_schedules.remove(id.trim()) {
        Ok(()) => json(
            StatusCode::OK,
            serde_json::json!({ "status": "deleted", "id": id.trim() }),
        ),
        Err(e) => json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e })),
    }
}

/// `POST /api/volumes/zfs/{pool}/scrub` — start a scrub now.
pub async fn pool_scrub_post(
    State(state): State<Arc<AppState>>,
    Path(pool): Path<String>,
) -> Response {
    if !super::zfs::is_valid_pool_name(&pool) {
        return bad_request("invalid pool name");
    }
    let client = match agent(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let health = match client.pool_health(&pool).await {
        Ok(h) => h,
        Err(e) => return super::zfs::write_agent_error(&e),
    };
    if scrub_in_progress(&health.scan) {
        return json(
            StatusCode::CONFLICT,
            serde_json::json!({ "error": "a scrub or resilver is already running on this pool" }),
        );
    }
    match client.scrub_pool(&pool).await {
        Ok(()) => {
            tracing::info!("started scrub on pool {}", crate::logsafe::field(&pool));
            json(
                StatusCode::ACCEPTED,
                serde_json::json!({ "status": "scrub started", "pool": pool }),
            )
        }
        Err(e) => super::zfs::write_agent_error(&e),
    }
}

fn bad_request(msg: &str) -> Response {
    json(StatusCode::BAD_REQUEST, serde_json::json!({ "error": msg }))
}

/// Record finished scrub outcomes for schedules whose scrub was started.
async fn refresh_scrub_results(state: &AppState) {
    let Some(client) = state.agent.clone() else {
        return;
    };
    for entry in state.storage_schedules.list() {
        if entry.kind != ScheduleKind::Scrub || entry.last_result != "started" {
            continue;
        }
        let Ok(status) = client.pool_status(&entry.target).await else {
            continue;
        };
        match scrub_finished(&scan_line(&status)) {
            Some(Ok(_)) => state.storage_schedules.record_result(&entry.id, "ok", ""),
            Some(Err(detail)) => state
                .storage_schedules
                .record_result(&entry.id, "failed", &detail),
            None => {}
        }
    }
}

/// Start every due, enabled schedule.
pub async fn run_due_schedules(state: &Arc<AppState>, now: DateTime<Utc>) {
    refresh_scrub_results(state).await;
    for entry in state.storage_schedules.list() {
        if !entry.enabled {
            continue;
        }
        match entry.next_run {
            Some(next) if next > now => continue,
            None => {
                let mut e = entry.clone();
                e.next_run = Some(next_run(e.cadence, &e.run_at, now));
                let _ = state.storage_schedules.put(e);
                continue;
            }
            _ => {}
        }
        match entry.kind {
            ScheduleKind::Snapshot => run_snapshot_schedule(state, &entry).await,
            ScheduleKind::Scrub => run_scrub_schedule(state, &entry).await,
        }
    }
}

/// Whether an agent error means "try again" rather than "this request is
/// wrong". Transport failures and degraded/unavailable upstreams are retryable;
/// a validation error (400) or a real conflict is not.
fn retryable_agent_error(err: &crate::agent::AgentError) -> bool {
    matches!(err.status, 0 | 502 | 503 | 504)
}

async fn run_snapshot_schedule(state: &Arc<AppState>, entry: &StorageSchedule) {
    let Some(agent) = state.agent.clone() else {
        tracing::warn!(
            "storage scheduler: no agent client; skipping snapshot {}",
            entry.id
        );
        return;
    };
    let now = Utc::now();
    let name = snapshot_name(&entry.id, now);
    if let Err(e) = agent
        .create_snapshot_with(&entry.target, &name, entry.recursive)
        .await
    {
        tracing::warn!(
            "storage scheduler: snapshot {} of {} failed: {}",
            name,
            entry.target,
            e
        );
        if retryable_agent_error(&e) {
            // Transport failure (agent not ready during an upgrade, restarted
            // DaemonSet): keep the due time so the next tick retries instead of
            // losing a daily or weekly snapshot.
            state
                .storage_schedules
                .record_failure_keep_next(&entry.id, &e.to_string());
        } else {
            state
                .storage_schedules
                .record_result(&entry.id, "failed", &e.to_string());
        }
        return;
    }

    let mut prune_error = String::new();
    if entry.keep > 0 {
        match agent.snapshot_paths(&entry.target).await {
            Ok(paths) => {
                let prefix = snapshot_prefix(&entry.id);
                for (dataset, snap) in prune_plan(&paths, &entry.target, &prefix, entry.keep) {
                    if let Err(e) = agent.destroy_snapshot(&dataset, &snap).await {
                        if !prune_error.is_empty() {
                            prune_error.push_str("; ");
                        }
                        prune_error.push_str(&format!("{dataset}@{snap}: {e}"));
                    }
                }
            }
            Err(e) => prune_error = format!("listing snapshots for retention: {e}"),
        }
    }
    if prune_error.is_empty() {
        state.storage_schedules.record_result(&entry.id, "ok", "");
    } else {
        tracing::warn!(
            "storage scheduler: {} created but retention was incomplete: {}",
            name,
            prune_error
        );
        state
            .storage_schedules
            .record_result(&entry.id, "ok", &prune_error);
    }
}

async fn run_scrub_schedule(state: &Arc<AppState>, entry: &StorageSchedule) {
    let Some(agent) = state.agent.clone() else {
        tracing::warn!(
            "storage scheduler: no agent client; skipping scrub {}",
            entry.id
        );
        return;
    };
    let status = match agent.pool_status(&entry.target).await {
        Ok(status) => status,
        Err(e) => {
            if retryable_agent_error(&e) {
                state
                    .storage_schedules
                    .record_failure_keep_next(&entry.id, &format!("reading pool status: {e}"));
            } else {
                state.storage_schedules.record_result(
                    &entry.id,
                    "failed",
                    &format!("reading pool status: {e}"),
                );
            }
            return;
        }
    };
    if scrub_in_progress(&scan_line(&status)) {
        if entry.last_result == "started" {
            // This schedule's own scrub is still running. Advance without
            // replacing `started`, so the completion is still derived later.
            state
                .storage_schedules
                .advance_next_run(&entry.id, Utc::now());
        } else {
            state.storage_schedules.record_result(
                &entry.id,
                "skipped",
                "a scrub or resilver is already running",
            );
        }
        return;
    }
    match agent.scrub_pool(&entry.target).await {
        Ok(()) => state
            .storage_schedules
            .record_result(&entry.id, "started", ""),
        Err(e) if retryable_agent_error(&e) => state
            .storage_schedules
            .record_failure_keep_next(&entry.id, &e.to_string()),
        Err(e) => state
            .storage_schedules
            .record_result(&entry.id, "failed", &e.to_string()),
    }
}

fn scheduler_interval() -> std::time::Duration {
    std::env::var("STORAGE_SCHEDULER_INTERVAL_MS")
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
    tracing::info!("storage scheduler started (interval {:?})", interval);
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
    use chrono::TimeZone;

    fn path(ds: &str, snap: &str) -> String {
        format!("{ds}@{snap}")
    }

    #[test]
    fn names_are_scheduler_owned_and_parseable() {
        let now = Utc.with_ymd_and_hms(2026, 1, 5, 10, 15, 0).unwrap();
        let name = snapshot_name("abcdef1234567890", now);
        assert!(name.starts_with("naslos-auto-61626364656631323334353637383930-"));
        assert!(name.ends_with("20260105T101500Z"));
        assert!(naslos_agent_snapshot_charset(&name));
    }

    #[test]
    fn distinct_ids_never_share_a_retention_prefix() {
        // The token must be injective: stripping punctuation would make these
        // two schedules prune each other's snapshots.
        assert_ne!(snapshot_prefix("a-b"), snapshot_prefix("ab"));
        assert_ne!(snapshot_prefix("AB"), snapshot_prefix("ab"));
        assert!(naslos_agent_snapshot_charset(&snapshot_prefix("a-b:.")));
    }

    fn naslos_agent_snapshot_charset(name: &str) -> bool {
        !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '+' | '-'))
    }

    #[test]
    fn retention_keeps_the_newest_and_never_touches_foreign_names() {
        let paths = vec![
            path("tank/data", "naslos-auto-abcd1234-old1"),
            path("tank/data", "manual-keep"),
            path("tank/data", "naslos-auto-abcd1234-old2"),
            path("tank/data", "naslos-auto-abcd1234-new1"),
            path("tank/data/child", "naslos-auto-abcd1234-old1"),
            path("tank/data/child", "naslos-auto-abcd1234-new1"),
            path("other/data", "naslos-auto-abcd1234-old1"),
        ];
        let delete = prune_plan(&paths, "tank/data", "naslos-auto-abcd1234-", 2);
        assert_eq!(
            delete,
            vec![(
                "tank/data".to_string(),
                "naslos-auto-abcd1234-old1".to_string()
            )]
        );
    }

    #[test]
    fn retention_is_disabled_by_zero() {
        let paths = vec![path("tank/data", "naslos-auto-abcd1234-old1")];
        assert!(prune_plan(&paths, "tank/data", "naslos-auto-abcd1234-", 0).is_empty());
    }

    #[test]
    fn scan_field_is_extracted_from_zpool_status() {
        let status = "  pool: tank\n state: ONLINE\n  scan: scrub repaired 0B in 00:01:23 with 0 errors on Sun Jan  4 02:00:00 2026\nconfig:\n";
        assert_eq!(
            scan_line(status),
            "scrub repaired 0B in 00:01:23 with 0 errors on Sun Jan  4 02:00:00 2026"
        );
        assert_eq!(scan_line("pool: tank\nstate: ONLINE\n"), "");
    }

    #[test]
    fn scrub_scan_lines_are_classified() {
        assert!(scrub_in_progress(
            "scrub in progress since Sun Jan  4 01:00:00 2026"
        ));
        assert!(scrub_in_progress("resilver in progress since ..."));
        assert_eq!(
            scrub_finished(
                "scrub repaired 0B in 00:01:23 with 0 errors on Sun Jan  4 02:00:00 2026"
            ),
            Some(Ok(
                "scrub repaired 0B in 00:01:23 with 0 errors on Sun Jan  4 02:00:00 2026".into()
            ))
        );
        assert!(matches!(
            scrub_finished(
                "scrub repaired 1.2M in 00:01:23 with 2 errors on Sun Jan  4 02:00:00 2026"
            ),
            Some(Err(_))
        ));
        assert_eq!(scrub_finished("none requested"), None);
    }
}
