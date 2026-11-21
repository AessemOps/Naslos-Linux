//! Cadence math shared by the scheduled jobs (buddy backups and storage
//! maintenance). A cadence is hourly, daily (`HH:MM`) or weekly
//! (`Day HH:MM`), always interpreted in UTC so a schedule does not shift when
//! the node's timezone changes.

use chrono::{DateTime, Datelike, Duration as ChronoDuration, TimeZone, Timelike, Utc, Weekday};
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
