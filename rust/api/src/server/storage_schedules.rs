//! Scheduled storage maintenance: periodic snapshots with retention and
//! per-pool scrub schedules (FR-STO-10/11).
//!
//! Entries persist as JSON beside the other state files. The runner ticks
//! (default a minute), creates snapshots through the agent, and prunes only
//! snapshots whose `user:naslos:schedule` property names the schedule, so a
//! user snapshot or another schedule's snapshot is never deleted by retention.
//! Scheduled names use the shadow-copy-compatible `naslos-%Y-%m-%d_%H-%M-%S`
//! UTC format; legacy 0.6.0 `naslos-auto-<schedule>-*` names stay eligible for
//! retention for one release so upgrading does not orphan them. A run missed
//! while the API was down fires once at startup (catch-up), matching the buddy
//! scheduler.

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

/// Legacy snapshot name prefix (`naslos-auto-<schedule>-*`). Scheduled
/// snapshots now carry their owner in a ZFS user property; this prefix is kept
/// only so 0.6.0 schedules can still find and prune their older snapshots.
pub const SNAPSHOT_NAME_PREFIX: &str = "naslos-auto-";

/// The shadow-copy-compatible timestamp format (UTC).
pub const SHADOW_COPY_SNAPSHOT_FORMAT: &str = "naslos-%Y-%m-%d_%H-%M-%S";

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
    /// When the current run of transient agent failures started. Anchors the
    /// one-hour retry window so it accumulates across retries instead of being
    /// reset by every attempt. Cleared by a normal result.
    #[serde(
        default,
        rename = "retrySince",
        skip_serializing_if = "Option::is_none"
    )]
    pub retry_since: Option<DateTime<Utc>>,
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
            retry_since: None,
        }
    }
}

/// Generates an injective token for a legacy snapshot-name prefix.
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

/// The legacy prefix a 0.6.0 schedule created: `naslos-auto-<token>-`.
pub fn snapshot_prefix(id: &str) -> String {
    format!("{SNAPSHOT_NAME_PREFIX}{}-", schedule_token(id))
}

/// A scheduled snapshot name in UTC: `naslos-YYYY-MM-DD_HH-MM-SS`.
///
/// The format is the contract `vfs_shadow_copy2` parses (`shadow:format`),
/// which is why it cannot carry the schedule id. Ownership is recorded in the
/// `user:naslos:schedule` user property instead.
pub fn snapshot_name(now: DateTime<Utc>) -> String {
    now.format(SHADOW_COPY_SNAPSHOT_FORMAT).to_string()
}

/// Whether two snapshot schedules would write the same snapshot name on a
/// dataset. The targets collide when they are equal, or when either schedule is
/// recursive and the other target is inside its tree.
fn snapshot_targets_overlap(
    a_target: &str,
    a_recursive: bool,
    b_target: &str,
    b_recursive: bool,
) -> bool {
    a_target == b_target
        || (a_recursive && b_target.starts_with(&format!("{a_target}/")))
        || (b_recursive && a_target.starts_with(&format!("{b_target}/")))
}

pub(crate) fn snapshot_schedules_overlap(a: &StorageSchedule, b: &StorageSchedule) -> bool {
    a.kind == ScheduleKind::Snapshot
        && b.kind == ScheduleKind::Snapshot
        && snapshot_targets_overlap(&a.target, a.recursive, &b.target, b.recursive)
}

/// Disable the extra enabled schedules a 0.6.0 store could contain now that the
/// snapshot name no longer carries the schedule id. The lexicographically first
/// id in each overlapping group stays enabled; the rest are disabled with an
/// explanation so an operator can delete or retarget them. Returns whether the
/// store changed and must be persisted.
fn disable_legacy_overlaps(entries: &mut HashMap<String, StorageSchedule>) -> bool {
    let mut ids: Vec<String> = entries.keys().cloned().collect();
    ids.sort();
    let mut kept: Vec<StorageSchedule> = Vec::new();
    let mut changed = false;
    for id in ids {
        let Some(entry) = entries.get(&id).cloned() else {
            continue;
        };
        if entry.kind != ScheduleKind::Snapshot || !entry.enabled {
            continue;
        }
        if kept
            .iter()
            .any(|kept| snapshot_schedules_overlap(kept, &entry))
        {
            if let Some(entry) = entries.get_mut(&id) {
                entry.enabled = false;
                entry.last_error = format!(
                    "disabled automatically after the shadow-copy naming upgrade: another snapshot schedule already covers {:?}; delete this schedule or point it at another dataset",
                    entry.target
                );
                tracing::warn!(
                    "storage scheduler: disabled overlapping legacy schedule {} on {}",
                    id,
                    entry.target
                );
                changed = true;
            }
        } else {
            kept.push(entry);
        }
    }
    changed
}

/// A snapshot plus the schedule that owns it (`user:naslos:schedule`). An
/// empty schedule means no property was recorded (a user snapshot, or a legacy
/// 0.6.0 scheduled snapshot).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedSnapshot {
    pub path: String,
    pub schedule: String,
}

/// Plan retention: from the snapshots under `root` (oldest first), return the
/// `(dataset, snapshot)` pairs to destroy so each dataset keeps the newest
/// `keep` snapshots owned by `schedule_id`.
///
/// Ownership is the property value, not the name. During the one-release
/// compatibility window a snapshot with no property is also owned when its name
/// starts with `legacy_prefix`; that is the only way an old 0.6.0 schedule can
/// still prune its snapshots.
pub fn prune_plan(
    snapshots: &[OwnedSnapshot],
    root: &str,
    schedule_id: &str,
    legacy_prefix: &str,
    keep: usize,
) -> Vec<(String, String)> {
    if keep == 0 {
        return Vec::new();
    }
    let owned = |snapshot: &OwnedSnapshot| -> bool {
        if !snapshot.schedule.is_empty() {
            return snapshot.schedule == schedule_id;
        }
        snapshot
            .path
            .split_once('@')
            .is_some_and(|(_, snap)| snap.starts_with(legacy_prefix))
    };
    let mut totals: HashMap<String, usize> = HashMap::new();
    for snapshot in snapshots {
        let Some((dataset, _)) = snapshot.path.split_once('@') else {
            continue;
        };
        if dataset != root && !dataset.starts_with(&format!("{root}/")) {
            continue;
        }
        if owned(snapshot) {
            *totals.entry(dataset.to_string()).or_insert(0) += 1;
        }
    }

    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut delete = Vec::new();
    // Creation order is oldest first: the first `total - keep` entries of a
    // dataset are the oldest ones and are the ones retention removes.
    for snapshot in snapshots {
        let Some((dataset, snap)) = snapshot.path.split_once('@') else {
            continue;
        };
        let Some(total) = totals.get(dataset) else {
            continue;
        };
        if !owned(snapshot) {
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

/// How long a scheduled run keeps retrying a transient agent failure before
/// the attempt is recorded as a normal failure and the cadence advances.
const AGENT_RETRY_WINDOW_SECS: i64 = 60 * 60;

/// Persists schedule entries as JSON.
pub struct StorageScheduleStore {
    path: String,
    entries: Mutex<HashMap<String, StorageSchedule>>,
    /// Set when the backing file could not be parsed. Mutations are refused so
    /// a corrupt store is never overwritten with an empty one.
    load_error: Mutex<Option<String>>,
}

impl StorageScheduleStore {
    pub fn new(path: &str) -> Self {
        Self {
            path: path.to_string(),
            entries: Mutex::new(HashMap::new()),
            load_error: Mutex::new(None),
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
        *self.load_error.lock().unwrap() = None;
        if self.path.is_empty() {
            return Ok(());
        }
        let data = match std::fs::read(&self.path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                let message = format!("reading storage schedule store {}: {e}", self.path);
                *self.load_error.lock().unwrap() = Some(message.clone());
                return Err(message);
            }
        };
        let list: Vec<StorageSchedule> = match serde_json::from_slice(&data) {
            Ok(list) => list,
            Err(e) => {
                // Keep the operator's data recoverable: move the unparseable
                // file aside, refuse mutations, and report the backup path.
                let backup = format!("{}.corrupt-{}", self.path, Utc::now().timestamp());
                let backup = match std::fs::rename(&self.path, &backup) {
                    Ok(()) => backup,
                    Err(rename_error) => {
                        let message = format!(
                            "parsing storage schedule store {}: {e} (could not preserve a backup: {rename_error})",
                            self.path
                        );
                        *self.load_error.lock().unwrap() = Some(message.clone());
                        return Err(message);
                    }
                };
                let message = format!(
                    "parsing storage schedule store {}: {e}; preserved the corrupt file at {backup} and refusing mutations",
                    self.path
                );
                *self.load_error.lock().unwrap() = Some(message.clone());
                return Err(message);
            }
        };
        for e in list {
            if e.id.is_empty() {
                continue;
            }
            guard.insert(e.id.clone(), e);
        }
        // A 0.6.0 store may predate the one-snapshot-schedule-per-tree rule.
        // Disable the extras now instead of letting them collide at every tick
        // (and instead of blocking every update with a 400).
        if disable_legacy_overlaps(&mut guard) {
            self.save(&guard)?;
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

    fn mutation_error(&self) -> Option<String> {
        self.load_error.lock().unwrap().clone()
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

    /// A snapshot of the store, for validation that must not race an insert.
    fn snapshot(&self) -> HashMap<String, StorageSchedule> {
        self.entries.lock().unwrap().clone()
    }

    /// Insert a schedule without the API-level overlap guard. Config import
    /// and tests seed legacy/ambiguous states through this path; the HTTP
    /// handler uses [`put_checked`](Self::put_checked).
    pub fn put(&self, e: StorageSchedule) -> Result<(), String> {
        self.put_inner(e, false)
    }

    /// Insert a schedule with the overlap check under the same lock as the
    /// insert, so two requests validated against the same pre-insert state
    /// cannot both succeed.
    pub fn put_checked(&self, e: StorageSchedule) -> Result<(), String> {
        self.put_inner(e, true)
    }

    fn put_inner(&self, e: StorageSchedule, enforce_overlap: bool) -> Result<(), String> {
        if let Some(error) = self.mutation_error() {
            return Err(error);
        }
        let mut guard = self.entries.lock().unwrap();
        if enforce_overlap {
            if let Some(other) = overlap_conflict(&guard, &e) {
                return Err(overlap_message(&e, &other));
            }
        }
        // Publish only after a successful save, so a failed write leaves the
        // in-memory store unchanged.
        let mut candidate = guard.clone();
        candidate.insert(e.id.clone(), e);
        self.save(&candidate)?;
        *guard = candidate;
        Ok(())
    }

    pub fn remove(&self, id: &str) -> Result<(), String> {
        if let Some(error) = self.mutation_error() {
            return Err(error);
        }
        let mut guard = self.entries.lock().unwrap();
        if guard.remove(id).is_none() {
            return Err(format!("schedule {id:?} not found"));
        }
        self.save(&guard)
    }

    /// Replace a schedule's editable fields while preserving the runtime state
    /// (`lastRun`, `lastResult`, `lastError`) recorded by the scheduler. The
    /// merge happens under the store lock so a result recorded while the
    /// request was being validated is not overwritten. No API-level overlap
    /// guard: config import uses this path.
    pub fn update_config(&self, e: StorageSchedule) -> Result<StorageSchedule, String> {
        self.update_config_inner(e, false)
    }

    /// Merge an edit with the overlap check under the store lock, so an edit
    /// that moves a schedule onto a just-created one is refused atomically.
    pub fn update_config_checked(&self, e: StorageSchedule) -> Result<StorageSchedule, String> {
        self.update_config_inner(e, true)
    }

    fn update_config_inner(
        &self,
        e: StorageSchedule,
        enforce_overlap: bool,
    ) -> Result<StorageSchedule, String> {
        if let Some(error) = self.mutation_error() {
            return Err(error);
        }
        let mut guard = self.entries.lock().unwrap();
        let Some(existing) = guard.get(&e.id) else {
            return Err(format!("schedule {:?} not found", e.id));
        };
        // Merge into a candidate, never into the live map: a refused overlap
        // or a failed save must leave the stored runtime state untouched.
        let mut merged = existing.clone();
        merged.kind = e.kind;
        merged.target = e.target;
        merged.cadence = e.cadence;
        merged.run_at = e.run_at;
        merged.keep = e.keep;
        merged.recursive = e.recursive;
        merged.enabled = e.enabled;
        merged.next_run = e.next_run;
        // Re-check the overlap under the same lock as the merge and insert, so
        // an edit that moves a schedule onto a just-created one is refused
        // atomically instead of at the next scheduler tick.
        if enforce_overlap {
            if let Some(other) = overlap_conflict(&guard, &merged) {
                return Err(overlap_message(&merged, &other));
            }
        }
        let mut candidate = guard.clone();
        candidate.insert(merged.id.clone(), merged.clone());
        self.save(&candidate)?;
        *guard = candidate;
        Ok(merged)
    }

    /// Advance the next run without recording an outcome.
    pub fn advance_next_run(&self, id: &str, now: DateTime<Utc>) {
        let mut guard = self.entries.lock().unwrap();
        if let Some(e) = guard.get_mut(id) {
            e.next_run = Some(next_run(e.cadence, &e.run_at, now));
            if let Err(err) = self.save(&guard) {
                tracing::warn!("storage scheduler: could not persist schedule state: {err}");
            }
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
        // A normal outcome ends the transient-failure window.
        e.retry_since = None;
        if let Err(save_err) = self.save(&guard) {
            tracing::warn!("storage scheduler: could not persist schedule state: {save_err}");
        }
    }

    /// Record a failed attempt that must be retried on the next tick (the agent
    /// was unreachable), without consuming the schedule's interval. The retry
    /// window is bounded: after an hour of transient failures the attempt is
    /// recorded as a normal failure and the cadence advances, so a permanently
    /// broken agent does not retry forever.
    pub fn record_failure_keep_next(&self, id: &str, err: &str) {
        self.record_failure_keep_next_at(id, err, Utc::now());
    }

    /// [`record_failure_keep_next`](Self::record_failure_keep_next) with an
    /// explicit clock, so the retry-window accumulation is testable.
    fn record_failure_keep_next_at(&self, id: &str, err: &str, now: DateTime<Utc>) {
        let mut guard = self.entries.lock().unwrap();
        let Some(e) = guard.get_mut(id) else { return };
        // Anchor the window on the transition into failure; `last_run` moves
        // on every retry and must not restart the hour.
        let retry_since = e.retry_since.unwrap_or(now);
        e.retry_since = Some(retry_since);
        e.last_run = Some(now);
        e.last_result = "failed".to_string();
        e.last_error = err.to_string();
        if (now - retry_since).num_seconds() >= AGENT_RETRY_WINDOW_SECS {
            e.next_run = Some(next_run(e.cadence, &e.run_at, now));
            e.retry_since = None;
        }
        if let Err(save_err) = self.save(&guard) {
            tracing::warn!("storage scheduler: could not persist schedule state: {save_err}");
        }
    }
}

/// The user-facing overlap refusal, shared by request validation and the
/// authoritative check under the store lock.
fn overlap_message(e: &StorageSchedule, other: &StorageSchedule) -> String {
    format!(
        "dataset {:?} already has snapshot schedule {:?} covering {:?}{}",
        e.target,
        other.id,
        other.target,
        if other.recursive { " recursively" } else { "" }
    )
}

/// The enabled snapshot schedule `e` would collide with, if any. An update
/// that leaves its own target/recursive/enabled unchanged may keep an overlap
/// it inherited from a legacy store, so the operator can still edit `keep` or
/// disable it.
fn overlap_conflict(
    entries: &HashMap<String, StorageSchedule>,
    e: &StorageSchedule,
) -> Option<StorageSchedule> {
    if e.kind != ScheduleKind::Snapshot || !e.enabled {
        return None;
    }
    let unchanged = entries
        .get(&e.id)
        .is_some_and(|old| old.enabled && old.target == e.target && old.recursive == e.recursive);
    if unchanged {
        return None;
    }
    entries
        .values()
        .find(|s| s.id != e.id && s.enabled && snapshot_schedules_overlap(s, e))
        .cloned()
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
            // Snapshot names are the shadow-copy timestamp only, so two
            // enabled schedules covering the same dataset (exactly or through a
            // recursive ancestor/descendant) would create the same name and
            // fight over ownership.
            // Advisory check for a fast 400. The authoritative check runs
            // under the store lock in `put`/`update_config`, so a schedule
            // created between this read and the insert cannot slip in.
            if let Some(other) = overlap_conflict(&state.storage_schedules.snapshot(), e) {
                return Err(overlap_message(e, &other));
            }
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
    let requested_id = req.id.trim().to_string();
    let existing = if requested_id.is_empty() {
        None
    } else {
        match state.storage_schedules.get(&requested_id) {
            Some(existing) => Some(existing),
            // Only server-generated ids may become retention prefixes.
            // Accepting an arbitrary client id would let two schedules collide
            // after token encoding and prune each other's snapshots.
            None => return bad_request("unknown schedule id; omit id to create a new schedule"),
        }
    };
    let updating = existing.is_some();
    let id = if updating {
        requested_id
    } else {
        let mut buf = [0u8; 16];
        let _ = getrandom::fill(&mut buf);
        buf.iter().map(|b| format!("{b:02x}")).collect()
    };
    let mut entry = StorageSchedule {
        id,
        kind,
        target: req.target.trim().to_string(),
        cadence: req.cadence.unwrap_or(Cadence::Daily),
        run_at: req.run_at.trim().to_string(),
        keep: req.keep,
        recursive: req.recursive,
        // An update that omits `enabled` keeps the stored state. That matters
        // for a legacy schedule the load migration disabled: editing its keep
        // count must not silently re-enable a colliding schedule.
        enabled: req
            .enabled
            .unwrap_or_else(|| existing.as_ref().is_none_or(|s| s.enabled)),
        ..Default::default()
    };
    if let Err(e) = validate_schedule(&state, &mut entry).await {
        return bad_request(&e);
    }
    entry.next_run = Some(next_run(entry.cadence, &entry.run_at, Utc::now()));
    let saved = if updating {
        // Merge under the store lock; a stale read here would overwrite a
        // lastRun/lastResult recorded while validation was awaiting the agent.
        match state.storage_schedules.update_config_checked(entry) {
            Ok(saved) => saved,
            // The overlap can only surface here when another request created
            // the conflicting schedule after validation; it is still a 400.
            Err(e) if e.contains("already has snapshot schedule") => return bad_request(&e),
            Err(e) => {
                return json(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    serde_json::json!({ "error": format!("saving the schedule: {e}") }),
                )
            }
        }
    } else {
        if let Err(e) = state.storage_schedules.put_checked(entry.clone()) {
            if e.contains("already has snapshot schedule") {
                return bad_request(&e);
            }
            return json(
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({ "error": format!("saving the schedule: {e}") }),
            );
        }
        entry
    };
    json(StatusCode::OK, serde_json::json!({ "schedule": saved }))
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
        // A missing id is a 404, but a corrupt/unwritable store is a server
        // failure: the record may still exist.
        Err(e) if e.contains("not found") => {
            json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e }))
        }
        Err(e) => json(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "error": format!("saving the schedule store: {e}") }),
        ),
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
        let Ok(health) = client.pool_health(&entry.target).await else {
            continue;
        };
        match scrub_finished(&health.scan) {
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
/// wrong". Transport failures and every 5xx upstream response are retryable;
/// a validation error (400) or a real conflict is not.
fn retryable_agent_error(err: &crate::agent::AgentError) -> bool {
    err.status == 0 || (500..=599).contains(&err.status)
}

/// Remove the snapshots a snapshot run just created after its ownership
/// property could not be recorded. The name is the one this run generated, so
/// only its own (possibly recursive) snapshots are targeted; a later retention
/// pass could not identify them without the property.
async fn destroy_untagged_run_snapshots(
    agent: &crate::agent::Client,
    entry: &StorageSchedule,
    name: &str,
) -> Result<(), String> {
    let root = format!("{}@{name}", entry.target);
    let child_prefix = format!("{}/", entry.target);
    let suffix = format!("@{name}");
    let mut errors: Vec<String> = Vec::new();

    // A non-recursive run created exactly one snapshot; never sweep descendants
    // by name, because one of those could be an unrelated pre-existing snapshot.
    if !entry.recursive {
        if let Err(e) = agent.destroy_snapshot(&entry.target, name).await {
            errors.push(format!("{root}: {e}"));
        }
    } else {
        match agent.snapshot_paths(&entry.target).await {
            Ok(paths) => {
                let mut targets: Vec<(String, String)> = paths
                    .iter()
                    .filter(|path| {
                        *path == &root
                            || (path.starts_with(&child_prefix) && path.ends_with(&suffix))
                    })
                    .filter_map(|path| {
                        path.split_once('@')
                            .map(|(dataset, snap)| (dataset.to_string(), snap.to_string()))
                    })
                    .collect();
                if targets.is_empty() {
                    targets.push((entry.target.clone(), name.to_string()));
                }
                for (dataset, snap) in targets {
                    if let Err(e) = agent.destroy_snapshot(&dataset, &snap).await {
                        errors.push(format!("{dataset}@{snap}: {e}"));
                    }
                }
            }
            Err(e) => {
                // The listing route predates the property route, so this is
                // rare. Remove what we can; the descendant snapshots may remain.
                match agent.destroy_snapshot(&entry.target, name).await {
                    Ok(()) => errors.push(format!(
                        "listing snapshots for cleanup failed ({e}); only the root snapshot was removed"
                    )),
                    Err(destroy_err) => errors.push(format!("{root}: {destroy_err}")),
                }
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
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
    let name = snapshot_name(now);
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

    // Retention trusts the property, not the name. Tag before listing so the
    // snapshot just created belongs to this schedule even if the run is checked
    // immediately afterwards.
    if let Err(e) = agent
        .set_snapshot_schedule(&entry.target, &name, &entry.id, entry.recursive)
        .await
    {
        let cleanup = destroy_untagged_run_snapshots(agent.as_ref(), entry, &name).await;
        let mut message =
            format!("snapshot {name} was created but its schedule property was not recorded: {e}");
        match cleanup {
            Ok(()) => message.push_str("; the untagged snapshot was removed"),
            Err(cleanup_error) => message.push_str(&format!(
                "; removing the untagged snapshot failed: {cleanup_error}"
            )),
        }
        tracing::warn!("storage scheduler: {}", message);
        state
            .storage_schedules
            .record_result(&entry.id, "failed", &message);
        return;
    }

    let mut prune_error = String::new();
    if entry.keep > 0 {
        match agent.snapshot_schedules(&entry.target).await {
            Ok(snapshots) => {
                let snapshots: Vec<OwnedSnapshot> = snapshots
                    .into_iter()
                    .map(|s| OwnedSnapshot {
                        path: s.path,
                        schedule: s.schedule,
                    })
                    .collect();
                let legacy_prefix = snapshot_prefix(&entry.id);
                for (dataset, snap) in prune_plan(
                    &snapshots,
                    &entry.target,
                    &entry.id,
                    &legacy_prefix,
                    entry.keep,
                ) {
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
    let health = match agent.pool_health(&entry.target).await {
        Ok(health) => health,
        Err(e) => {
            if entry.last_result == "started" {
                // The scrub may still be running. A failed health read must not
                // replace `started`, or the completion could never be derived.
                if retryable_agent_error(&e) {
                    tracing::warn!(
                        "storage scheduler: could not read the scrub status for {} (will retry): {e}",
                        entry.id
                    );
                } else {
                    tracing::warn!(
                        "storage scheduler: could not read the scrub status for {}; keeping started and deferring: {e}",
                        entry.id
                    );
                    state
                        .storage_schedules
                        .advance_next_run(&entry.id, Utc::now());
                }
                return;
            }
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
    if scrub_in_progress(&health.scan) {
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

    fn owned(ds: &str, snap: &str, schedule: &str) -> OwnedSnapshot {
        OwnedSnapshot {
            path: format!("{ds}@{snap}"),
            schedule: schedule.to_string(),
        }
    }

    #[test]
    fn scheduled_names_use_the_shadow_copy_utc_format() {
        let now = Utc.with_ymd_and_hms(2026, 1, 5, 10, 15, 0).unwrap();
        assert_eq!(snapshot_name(now), "naslos-2026-01-05_10-15-00");
        assert!(naslos_agent_snapshot_charset(&snapshot_name(now)));
    }

    #[test]
    fn legacy_prefixes_stay_injective() {
        // The token must be injective: stripping punctuation would make these
        // two schedules prune each other's snapshots during the legacy window.
        assert_ne!(snapshot_prefix("a-b"), snapshot_prefix("ab"));
        assert_ne!(snapshot_prefix("AB"), snapshot_prefix("ab"));
        assert!(naslos_agent_snapshot_charset(&snapshot_prefix("a-b:.")));
    }

    #[test]
    fn legacy_060_prefix_matches_the_captured_release_fixture() {
        // Literal fixture captured from the 0.6.0 derivation: the token is the
        // lower-case hex of the UTF-8 id, not a sanitized label.
        assert_eq!(snapshot_prefix("a-b"), "naslos-auto-612d62-");
        assert_eq!(snapshot_prefix("abcd1234"), "naslos-auto-6162636431323334-");
        assert_eq!(snapshot_prefix(""), "naslos-auto-schedule-");
    }

    #[test]
    fn a_corrupt_store_is_preserved_and_refuses_mutations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("storage-schedules.json");
        std::fs::write(&path, b"{this is not json").unwrap();
        let store = StorageScheduleStore::new(path.to_str().unwrap());
        let error = store.load().unwrap_err();
        assert!(error.contains("refusing mutations"), "{error}");

        let backups: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("storage-schedules.json.corrupt-")
            })
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(
            std::fs::read(backups[0].path()).unwrap(),
            b"{this is not json"
        );

        let mut entry = stored_schedule("new", "tank/data", false, true);
        entry.kind = ScheduleKind::Snapshot;
        assert!(store.put(entry).is_err());
        assert!(store.remove("new").is_err());
    }

    #[test]
    fn a_transient_failure_retries_then_records_a_failure_after_the_window() {
        let store = StorageScheduleStore::new("");
        let mut entry = stored_schedule("retry", "tank/data", false, true);
        entry.cadence = Cadence::Daily;
        entry.run_at = "02:30".into();
        let due = Utc.with_ymd_and_hms(2026, 1, 5, 2, 30, 0).unwrap();
        entry.next_run = Some(due);
        store.put(entry).unwrap();

        // First real failure: `nextRun` is kept so the next tick retries it,
        // and the anchor is recorded.
        store.record_failure_keep_next_at("retry", "agent unavailable", due);
        let first = store.get("retry").unwrap();
        assert_eq!(first.last_result, "failed");
        assert_eq!(first.next_run, Some(due));
        assert_eq!(first.retry_since, Some(due));

        // A second consecutive real failure half an hour later moves
        // `lastRun` but not the retry anchor: the hour keeps accumulating.
        store.record_failure_keep_next_at(
            "retry",
            "agent unavailable",
            due + chrono::Duration::minutes(30),
        );
        let second = store.get("retry").unwrap();
        assert_eq!(second.retry_since, Some(due));
        assert_eq!(second.next_run, Some(due), "still inside the window");

        // The third failure passes the one-hour window: the attempt becomes a
        // normal failure and the cadence advances.
        let past = due + chrono::Duration::minutes(61);
        store.record_failure_keep_next_at("retry", "agent unavailable", past);
        let last = store.get("retry").unwrap();
        assert!(last.next_run.unwrap() > past);
        assert_eq!(last.retry_since, None);

        // A normal result also closes the window.
        store.record_failure_keep_next_at("retry", "agent unavailable", past);
        store.record_result("retry", "ok", "");
        assert_eq!(store.get("retry").unwrap().retry_since, None);
    }

    #[test]
    fn the_store_refuses_an_overlapping_insert_under_its_own_lock() {
        let store = StorageScheduleStore::new("");
        store
            .put_checked(stored_schedule("a", "tank/data", false, true))
            .unwrap();

        // This bypasses `validate_schedule`: only the store lock can stop a
        // second enabled snapshot schedule for the same dataset.
        let err = store
            .put_checked(stored_schedule("b", "tank/data", false, true))
            .expect_err("an overlapping insert must be refused");
        assert!(err.contains("already has snapshot schedule"), "{err}");
        assert!(store.get("b").is_none(), "the refused insert is not stored");

        // Disabled entries may overlap: they never run.
        store
            .put_checked(stored_schedule("c", "tank/data", false, false))
            .unwrap();

        // Re-targeting an existing schedule onto another's dataset is refused
        // atomically and leaves the stored record untouched.
        store
            .put_checked(stored_schedule("d", "tank/other", false, true))
            .unwrap();
        let err = store
            .update_config_checked(stored_schedule("d", "tank/data", false, true))
            .expect_err("a move onto another schedule must be refused");
        assert!(err.contains("already has snapshot schedule"), "{err}");
        assert_eq!(store.get("d").unwrap().target, "tank/other");
    }

    fn naslos_agent_snapshot_charset(name: &str) -> bool {
        !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '+' | '-'))
    }

    #[test]
    fn retention_uses_the_schedule_property_not_the_name() {
        let id = "abcd1234";
        let snapshots = vec![
            owned("tank/data", "naslos-2026-01-01_00-00-00", id),
            owned("tank/data", "naslos-2026-01-02_00-00-00", id),
            // Another schedule's snapshot is never retained against this one,
            // even when its name matches the shadow-copy format.
            owned("tank/data", "naslos-2026-01-03_00-00-00", "other"),
            // A user snapshot has no property and must never be deleted.
            owned("tank/data", "manual-keep", ""),
            owned("tank/data", "naslos-2026-01-04_00-00-00", id),
            owned("tank/data/child", "naslos-2026-01-01_00-00-00", id),
            owned("tank/data/child", "naslos-2026-01-02_00-00-00", id),
            owned("other/data", "naslos-2026-01-01_00-00-00", id),
        ];
        let delete = prune_plan(&snapshots, "tank/data", id, "naslos-auto-abcd1234-", 2);
        assert_eq!(
            delete,
            vec![(
                "tank/data".to_string(),
                "naslos-2026-01-01_00-00-00".to_string()
            )]
        );
    }

    #[test]
    fn legacy_060_names_remain_retained_for_one_release() {
        let id = "abcd1234";
        let legacy = snapshot_prefix(id);
        let snapshots = vec![
            owned("tank/data", &format!("{legacy}20260101T000000Z"), ""),
            owned("tank/data", &format!("{legacy}20260102T000000Z"), ""),
            owned("tank/data", "manual-keep", ""),
            owned("tank/data", "naslos-2026-01-03_00-00-00", id),
        ];
        let delete = prune_plan(&snapshots, "tank/data", id, &legacy, 2);
        assert_eq!(
            delete,
            vec![("tank/data".to_string(), format!("{legacy}20260101T000000Z"))]
        );
    }

    #[test]
    fn a_property_from_another_schedule_beats_a_legacy_looking_name() {
        let id = "abcd1234";
        let legacy = snapshot_prefix(id);
        let snapshots = vec![
            owned("tank/data", &format!("{legacy}20260101T000000Z"), "other"),
            owned("tank/data", "naslos-2026-01-02_00-00-00", id),
        ];
        // The legacy-looking name belongs to another schedule (the property is
        // authoritative), so only this schedule's one snapshot counts.
        assert!(prune_plan(&snapshots, "tank/data", id, &legacy, 1).is_empty());
    }

    #[test]
    fn retention_is_disabled_by_zero() {
        let snapshots = vec![owned("tank/data", "naslos-2026-01-01_00-00-00", "abcd1234")];
        assert!(prune_plan(&snapshots, "tank/data", "abcd1234", "", 0).is_empty());
    }

    #[test]
    fn snapshot_target_overlap_covers_recursive_descendants_only() {
        assert!(snapshot_targets_overlap(
            "tank/data",
            false,
            "tank/data",
            false
        ));
        assert!(snapshot_targets_overlap(
            "tank/data",
            true,
            "tank/data/child",
            false
        ));
        assert!(snapshot_targets_overlap(
            "tank/data/child",
            false,
            "tank/data",
            true
        ));
        // A non-recursive parent only snapshots itself.
        assert!(!snapshot_targets_overlap(
            "tank/data",
            false,
            "tank/data/child",
            false
        ));
        assert!(!snapshot_targets_overlap(
            "tank/data/a",
            true,
            "tank/data/b",
            true
        ));
        assert!(!snapshot_targets_overlap(
            "tank/data",
            true,
            "tank/data2",
            false
        ));
    }

    fn stored_schedule(id: &str, target: &str, recursive: bool, enabled: bool) -> StorageSchedule {
        StorageSchedule {
            id: id.to_string(),
            kind: ScheduleKind::Snapshot,
            target: target.to_string(),
            cadence: Cadence::Daily,
            run_at: "02:30".to_string(),
            recursive,
            enabled,
            ..Default::default()
        }
    }

    #[test]
    fn loading_disables_preexisting_overlapping_schedules() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("storage-schedules.json");
        let list = vec![
            stored_schedule("a", "tank/data", true, true),
            // Exact duplicate of `a`.
            stored_schedule("b", "tank/data", false, true),
            // Nested in `a`'s recursive tree.
            stored_schedule("c", "tank/data/child", false, true),
            stored_schedule("d", "tank/other", false, true),
            // Disabled entries never run, so they are not touched.
            stored_schedule("e", "tank/other/child", false, false),
        ];
        std::fs::write(&path, serde_json::to_vec(&list).unwrap()).unwrap();

        let store = StorageScheduleStore::new(path.to_str().unwrap());
        store.load().unwrap();
        assert!(store.get("a").unwrap().enabled);
        let b = store.get("b").unwrap();
        assert!(!b.enabled);
        assert!(b.last_error.contains("disabled automatically"), "{b:?}");
        let c = store.get("c").unwrap();
        assert!(!c.enabled);
        assert!(c.last_error.contains("disabled automatically"), "{c:?}");
        assert!(store.get("d").unwrap().enabled);
        assert!(!store.get("e").unwrap().enabled);

        // The migration is persisted, not just applied in memory.
        let reloaded = StorageScheduleStore::new(path.to_str().unwrap());
        reloaded.load().unwrap();
        assert!(!reloaded.get("b").unwrap().enabled);
        assert!(!reloaded.get("c").unwrap().enabled);
    }

    #[test]
    fn config_update_preserves_the_scheduler_runtime_state() {
        let store = StorageScheduleStore::new("");
        let mut entry = StorageSchedule {
            id: "abc".to_string(),
            kind: ScheduleKind::Snapshot,
            target: "tank/data".to_string(),
            cadence: Cadence::Daily,
            run_at: "02:30".to_string(),
            keep: 7,
            enabled: true,
            ..Default::default()
        };
        store.put(entry.clone()).unwrap();
        // A run finishes while the edit request is still being validated.
        store.record_result("abc", "ok", "");

        entry.keep = 14;
        let saved = store.update_config(entry).unwrap();
        assert_eq!(saved.keep, 14);
        assert_eq!(saved.last_result, "ok");
        assert!(saved.last_run.is_some());
        assert_eq!(store.get("abc").unwrap().last_result, "ok");
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
