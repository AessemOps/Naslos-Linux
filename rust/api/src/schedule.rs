//! Cadence math shared by the scheduled jobs (buddy backups, storage
//! maintenance and cloud backup), plus the generic persistence and fan-out
//! helpers those schedulers share. A cadence is hourly, daily (`HH:MM`) or
//! weekly (`Day HH:MM`), always interpreted in UTC so a schedule does not shift
//! when the node's timezone changes.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use chrono::{DateTime, Datelike, Duration as ChronoDuration, TimeZone, Timelike, Utc, Weekday};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// The scheduler interval for a schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cadence {
    #[default]
    Hourly,
    Daily,
    Weekly,
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

/// Parse `runAt` for a cadence. Hourly ignores it; daily is `HH:MM`; weekly is
/// `Day HH:MM` with a three-letter English weekday.
pub fn parse_run_at(cadence: Cadence, run_at: &str) -> Result<(Weekday, u32, u32), String> {
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
pub fn next_run(cadence: Cadence, run_at: &str, now: DateTime<Utc>) -> DateTime<Utc> {
    match cadence {
        Cadence::Hourly => {
            let truncated = now
                .with_minute(0)
                .and_then(|t| t.with_second(0))
                .and_then(|t| t.with_nanosecond(0))
                .unwrap_or(now);
            truncated + ChronoDuration::hours(1)
        }
        Cadence::Daily => {
            let Ok((_, h, m)) = parse_run_at(cadence, run_at) else {
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
            let Ok((weekday, h, m)) = parse_run_at(cadence, run_at) else {
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

/// A persisted record with a stable string id.
pub trait JsonStoreItem {
    fn id(&self) -> &str;
}

/// A JSON list store with atomic writes and corrupt-file preservation.
///
/// Every mutation persists a candidate map first and only swaps it into memory
/// after the write succeeds, so a failed write never leaves memory ahead of the
/// file. A file that cannot be parsed is copied aside with
/// [`crate::shares::backup_corrupt_store`] and mutations are refused until it is
/// repaired; the original file is never overwritten with an empty store.
pub struct JsonStore<T> {
    path: String,
    entries: Mutex<HashMap<String, T>>,
    loaded: AtomicBool,
    load_error: Mutex<Option<String>>,
}

impl<T> JsonStore<T>
where
    T: Serialize + DeserializeOwned + Clone + JsonStoreItem,
{
    /// `path` may be empty for an in-memory store.
    pub fn new(path: &str) -> Self {
        Self {
            path: path.to_string(),
            entries: Mutex::new(HashMap::new()),
            // An empty path is an in-memory test store, not a failed load.
            loaded: AtomicBool::new(path.is_empty()),
            load_error: Mutex::new(None),
        }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    /// Whether the persisted list was read successfully.
    pub fn loaded(&self) -> bool {
        self.loaded.load(Ordering::SeqCst)
    }

    pub fn load_error(&self) -> Option<String> {
        self.load_error.lock().unwrap().clone()
    }

    /// Fail closed unless the persisted store was read successfully.
    pub fn require_loaded(&self) -> Result<(), String> {
        if self.loaded() {
            Ok(())
        } else {
            Err(self
                .load_error()
                .unwrap_or_else(|| format!("the JSON store {} is unavailable", self.path)))
        }
    }

    pub fn load(&self) -> Result<(), String> {
        self.load_with(|_| Ok(false))
    }

    /// Load the file and run `after` on the fresh map before it becomes live.
    /// `after` returns whether it changed the map, which is then persisted.
    pub fn load_with<F>(&self, after: F) -> Result<(), String>
    where
        F: FnOnce(&mut HashMap<String, T>) -> Result<bool, String>,
    {
        self.loaded.store(false, Ordering::SeqCst);
        *self.load_error.lock().unwrap() = None;
        let mut guard = self.entries.lock().unwrap();
        guard.clear();
        if self.path.is_empty() {
            self.loaded.store(true, Ordering::SeqCst);
            return Ok(());
        }
        let data = match std::fs::read(&self.path) {
            Ok(data) => data,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.loaded.store(true, Ordering::SeqCst);
                return Ok(());
            }
            Err(e) => {
                let message = format!(
                    "reading the schedule store {}: {e}; refusing to overwrite it",
                    self.path
                );
                *self.load_error.lock().unwrap() = Some(message.clone());
                return Err(message);
            }
        };
        let list: Vec<T> = match serde_json::from_slice(&data) {
            Ok(list) => list,
            Err(e) => {
                // Keep the operator's data recoverable: save a copy aside,
                // refuse mutations, and report the backup path. The original
                // file is left in place so a restart cannot replace it.
                let backup = match crate::shares::backup_corrupt_store(&self.path, &data) {
                    Ok(path) => format!(" (backup saved to {})", path.display()),
                    Err(backup_error) => format!(" ({backup_error})"),
                };
                let message = format!(
                    "the schedule store {} is corrupt: {e}{backup}; refusing mutations until it is repaired",
                    self.path
                );
                *self.load_error.lock().unwrap() = Some(message.clone());
                return Err(message);
            }
        };
        for entry in list {
            if entry.id().is_empty() {
                continue;
            }
            let id = entry.id().to_string();
            guard.insert(id, entry);
        }
        if after(&mut guard)? {
            self.persist(&guard)?;
        }
        self.loaded.store(true, Ordering::SeqCst);
        Ok(())
    }

    pub fn list(&self) -> Vec<T> {
        let guard = self.entries.lock().unwrap();
        let mut out: Vec<T> = guard.values().cloned().collect();
        out.sort_by(|a, b| a.id().cmp(b.id()));
        out
    }

    pub fn get(&self, id: &str) -> Option<T> {
        self.entries.lock().unwrap().get(id).cloned()
    }

    /// Persist-then-swap: a failed save leaves memory identical to the file.
    pub fn put(&self, entry: T) -> Result<(), String> {
        self.require_loaded()?;
        let id = entry.id().to_string();
        let mut guard = self.entries.lock().unwrap();
        let mut candidate = guard.clone();
        candidate.insert(id, entry);
        self.persist(&candidate)?;
        *guard = candidate;
        Ok(())
    }

    /// Persist-then-swap removal.
    pub fn remove(&self, id: &str) -> Result<(), String> {
        self.require_loaded()?;
        let mut guard = self.entries.lock().unwrap();
        if !guard.contains_key(id) {
            return Err(format!("{id:?} not found"));
        }
        let mut candidate = guard.clone();
        candidate.remove(id);
        self.persist(&candidate)?;
        *guard = candidate;
        Ok(())
    }

    /// Read-modify-write one entry with the same persist-then-swap rule.
    pub fn update<F>(&self, id: &str, mutate: F) -> Result<T, String>
    where
        F: FnOnce(&mut T) -> Result<(), String>,
    {
        self.require_loaded()?;
        let mut guard = self.entries.lock().unwrap();
        let mut candidate = guard.clone();
        let entry = candidate
            .get_mut(id)
            .ok_or_else(|| format!("{id:?} not found"))?;
        mutate(entry)?;
        let updated = entry.clone();
        self.persist(&candidate)?;
        *guard = candidate;
        Ok(updated)
    }

    fn persist(&self, guard: &HashMap<String, T>) -> Result<(), String> {
        if self.path.is_empty() {
            return Ok(());
        }
        let mut list: Vec<&T> = guard.values().collect();
        list.sort_by(|a, b| a.id().cmp(b.id()));
        let mut data = serde_json::to_vec_pretty(&list)
            .map_err(|e| format!("encoding the JSON store {}: {e}", self.path))?;
        data.push(b'\n');
        crate::chartsrepo::write_atomic(&self.path, &data, 0o600, 0o750)
    }
}

/// The shared aggregation state of a schedule that fans one source out to
/// several targets. Concrete entries keep their own serde field names; only the
/// aggregation logic is shared.
pub trait FanOutEntry {
    fn targets(&self) -> Vec<String>;
    fn target_results(&mut self) -> &mut HashMap<String, String>;
    fn last_run(&mut self) -> &mut Option<DateTime<Utc>>;
    fn last_result(&mut self) -> &mut String;
    fn last_error(&mut self) -> &mut String;
    fn next_run(&mut self) -> &mut Option<DateTime<Utc>>;
}

/// Record one target's outcome; the entry is "ok" only when every target
/// succeeded. Best effort by design (it is progress, not a security decision):
/// the caller persists the mutated entry.
pub fn record_target_result<E: FanOutEntry>(
    entry: &mut E,
    target: &str,
    fallback_target: &str,
    ok: bool,
    job_err: &str,
    now: DateTime<Utc>,
    next: DateTime<Utc>,
) {
    *entry.last_run() = Some(now);
    let target = if target.is_empty() {
        fallback_target.to_string()
    } else {
        target.to_string()
    };
    entry.target_results().insert(
        target.clone(),
        if ok { "ok".into() } else { "failed".into() },
    );
    let failed = {
        let targets = entry.targets();
        let results = entry.target_results();
        targets.into_iter().find(|destination| {
            results
                .get(destination)
                .map(|v| v == "failed")
                .unwrap_or(false)
        })
    };
    if failed.is_none() {
        *entry.last_result() = "ok".to_string();
        entry.last_error().clear();
    } else {
        let failed = failed.unwrap_or_default();
        *entry.last_result() = "failed".to_string();
        if !ok && !job_err.trim().is_empty() {
            *entry.last_error() = format!("backup to {target} failed: {job_err}");
        } else if entry.last_error().trim().is_empty() {
            *entry.last_error() = format!("backup to {failed} failed");
        }
    }
    *entry.next_run() = Some(next);
}

/// Advance a fan-out entry's next run (called once when a fan-out starts).
pub fn advance_next_run<E: FanOutEntry>(entry: &mut E, next: DateTime<Utc>) {
    *entry.next_run() = Some(next);
}

/// Run the due-schedule body once at startup (catch-up), then on every
/// interval tick.
pub async fn run_catch_up_then_tick<F, Fut>(interval: std::time::Duration, mut due: F)
where
    F: FnMut(DateTime<Utc>) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    due(Utc::now()).await;
    let mut ticker = tokio::time::interval(interval);
    ticker.tick().await;
    loop {
        ticker.tick().await;
        due(Utc::now()).await;
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
        let next = next_run(Cadence::Hourly, "", now);
        assert!(next > now);
        let next = next_run(Cadence::Daily, "02:30", now);
        assert!(next > now);
        assert_eq!(next.format("%H:%M").to_string(), "02:30");
        let next = next_run(Cadence::Weekly, "Mon 02:30", now);
        assert_eq!(next.weekday(), Weekday::Mon);
        assert!(next > now);
    }
}
