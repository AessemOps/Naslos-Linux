//! Periodic ZFS/agent health watcher (restores the Go `startHealthWatcher`
//! behaviour lost in the Rust port, SEC-2026-H6).
//!
//! It notifies only on transitions: a pool that stays degraded does not spam
//! ntfy every minute, and a pool that recovers can notify again on the next
//! failure.

use crate::agent::{DiskHealthReport, SmartInfo};
use crate::notifications::{
    Notification, EVENT_DISK_FAILURE, EVENT_ZFS_HEALTH, SEVERITY_CRITICAL, SEVERITY_ERROR,
    SEVERITY_WARNING,
};
use crate::state::AppState;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

/// Whether a zpool health string is not fully healthy.
pub fn is_unhealthy(health: &str) -> bool {
    let h = health.trim().to_ascii_uppercase();
    !(h.is_empty() || h == "ONLINE" || h.starts_with("ONLINE ("))
}

/// One transition-tracked disk alert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskAlert {
    pub key: String,
    pub event: &'static str,
    pub severity: &'static str,
    pub title: String,
    pub message: String,
}

fn threshold_from_env(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default)
}

fn disk_health_enabled() -> bool {
    std::env::var("DISK_HEALTH_ENABLED")
        .map(|v| !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "no"))
        .unwrap_or(true)
}

/// Turn the current per-disk report into alerts. Pure so the transition rules
/// (including the threshold boundaries) are unit-tested without an agent.
pub fn disk_alerts(
    report: &DiskHealthReport,
    temperature_c: u64,
    wear_percent: u64,
) -> Vec<DiskAlert> {
    let mut alerts = Vec::new();
    for disk in &report.disks {
        let device = &disk.device;
        let smart: &SmartInfo = &disk.smart;

        if smart.available && smart.health.eq_ignore_ascii_case("FAILED") {
            alerts.push(DiskAlert {
                key: format!("disk:{device}:smart"),
                event: EVENT_DISK_FAILURE,
                severity: SEVERITY_ERROR,
                title: format!("Disk {device} SMART health FAILED"),
                message: format!(
                    "SMART reports disk {device} as failing. Replace it and resilver the pool."
                ),
            });
        }
        if smart.available {
            if let Some(temp) = smart.temperature_c {
                if temp > 0 && temp as u64 > temperature_c {
                    alerts.push(DiskAlert {
                        key: format!("disk:{device}:temperature"),
                        event: EVENT_ZFS_HEALTH,
                        severity: SEVERITY_WARNING,
                        title: format!("Disk {device} is at {temp}°C"),
                        message: format!(
                            "Disk {device} is above the {temperature_c}°C warning threshold. Check airflow and the disk."
                        ),
                    });
                }
            }
            if let Some(wear) = smart.wear_percent {
                if wear as u64 >= wear_percent {
                    alerts.push(DiskAlert {
                        key: format!("disk:{device}:wear"),
                        event: EVENT_ZFS_HEALTH,
                        severity: SEVERITY_WARNING,
                        title: format!("Disk {device} wear is {wear}%"),
                        message: format!(
                            "Disk {device} has reached the {wear_percent}% wear warning threshold; plan a replacement."
                        ),
                    });
                }
            }
        }
        if let Some(zfs) = &disk.zfs {
            let state = zfs.state.to_ascii_uppercase();
            let (event, severity) = if state.contains("FAULTED")
                || state.contains("OFFLINE")
                || state.contains("UNAVAIL")
                || state.contains("REMOVED")
            {
                (EVENT_DISK_FAILURE, SEVERITY_ERROR)
            } else if state.contains("DEGRADED") {
                (EVENT_ZFS_HEALTH, SEVERITY_WARNING)
            } else {
                continue;
            };
            alerts.push(DiskAlert {
                key: format!("disk:{device}:zfs"),
                event,
                severity,
                title: format!("Disk {device} is {state} in pool {}", zfs.pool),
                message: format!(
                    "ZFS reports /dev/{} ({}) as {} in pool {} (vdev {}). Check `zpool status {}`.",
                    device.trim_start_matches("/dev/"),
                    zfs.vdev,
                    state,
                    zfs.pool,
                    zfs.vdev,
                    zfs.pool
                ),
            });
        }
    }
    alerts
}

fn pool_event(health: &str) -> (&'static str, &'static str) {
    let h = health.to_ascii_uppercase();
    if h.contains("FAULTED")
        || h.contains("OFFLINE")
        || h.contains("UNAVAIL")
        || h.contains("REMOVED")
    {
        (EVENT_DISK_FAILURE, SEVERITY_ERROR)
    } else {
        (EVENT_ZFS_HEALTH, SEVERITY_WARNING)
    }
}

pub async fn run(state: Arc<AppState>) {
    let interval = std::env::var("HEALTH_INTERVAL_SECONDS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(60));
    let mut unhealthy: HashSet<String> = HashSet::new();
    loop {
        check_once(&state, &mut unhealthy).await;
        tokio::time::sleep(interval).await;
    }
}

/// One poll. Public for the transition tests.
pub async fn check_once(state: &AppState, unhealthy: &mut HashSet<String>) {
    let Some(agent) = &state.agent else {
        return;
    };
    match agent.list_pools().await {
        Err(e) => {
            if unhealthy.insert("agent".to_string()) {
                send(
                    state,
                    EVENT_ZFS_HEALTH,
                    "Naslos agent unreachable",
                    &format!("The agent API failed: {e}"),
                    SEVERITY_CRITICAL,
                )
                .await;
            }
        }
        Ok(pools) => {
            if unhealthy.remove("agent") {
                send(
                    state,
                    EVENT_ZFS_HEALTH,
                    "Naslos agent reachable again",
                    "The agent API is answering again.",
                    SEVERITY_WARNING,
                )
                .await;
            }
            let live: HashSet<String> = pools.iter().map(|p| format!("pool:{}", p.name)).collect();
            unhealthy.retain(|k| !k.starts_with("pool:") || live.contains(k));
            for pool in &pools {
                let key = format!("pool:{}", pool.name);
                if is_unhealthy(&pool.health) {
                    if unhealthy.insert(key) {
                        let (event, severity) = pool_event(&pool.health);
                        send(
                            state,
                            event,
                            &format!("ZFS pool {} is {}", pool.name, pool.health),
                            &format!(
                                "Pool {} reports health {}. Check `zpool status {}`.",
                                pool.name, pool.health, pool.name
                            ),
                            severity,
                        )
                        .await;
                    }
                } else {
                    unhealthy.remove(&key);
                }
            }
        }
    }

    if !disk_health_enabled() {
        unhealthy.retain(|key| !key.starts_with("disk:"));
        return;
    }
    let Ok(report) = agent.disk_health().await else {
        // An older agent (404) or a transient failure: the pool watcher above
        // already owns the agent-unreachable alert, and stale disk keys must
        // not be cleared on a failed poll.
        return;
    };
    let temperature = threshold_from_env("DISK_HEALTH_TEMPERATURE_C", 55);
    let wear = threshold_from_env("DISK_HEALTH_WEAR_PERCENT", 80);
    let alerts = disk_alerts(&report, temperature, wear);
    let live: HashSet<String> = alerts.iter().map(|a| a.key.clone()).collect();
    let present: HashSet<String> = report.disks.iter().map(|d| d.device.clone()).collect();

    let recovered: Vec<String> = unhealthy
        .iter()
        .filter(|key| {
            if !key.starts_with("disk:") || live.contains(*key) {
                return false;
            }
            // A removed disk must not produce a recovery notification.
            key.split(':')
                .nth(1)
                .map(|device| present.contains(device))
                .unwrap_or(false)
        })
        .cloned()
        .collect();
    for key in recovered {
        unhealthy.remove(&key);
        let device = key.split(':').nth(1).unwrap_or("disk");
        send(
            state,
            EVENT_ZFS_HEALTH,
            &format!("Disk {device} health recovered"),
            &format!("The previous disk health alert for {device} has cleared."),
            SEVERITY_WARNING,
        )
        .await;
    }
    // Drop keys for disks that disappeared without a recovery notice.
    unhealthy.retain(|key| {
        if !key.starts_with("disk:") {
            return true;
        }
        live.contains(key)
            || key
                .split(':')
                .nth(1)
                .map(|device| present.contains(device))
                .unwrap_or(false)
    });

    for alert in alerts {
        if unhealthy.insert(alert.key) {
            send(
                state,
                alert.event,
                &alert.title,
                &alert.message,
                alert.severity,
            )
            .await;
        }
    }
}

async fn send(state: &AppState, event: &str, title: &str, message: &str, severity: &str) {
    if !state.notifications.event_enabled(event) {
        return;
    }
    let notif = Notification {
        title: title.to_string(),
        message: message.to_string(),
        severity: severity.to_string(),
        tags: vec!["floppy_disk".to_string()],
    };
    if let Err(e) = state.notifications.send(&notif).await {
        tracing::warn!("health notification failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::{disk_alerts, is_unhealthy, pool_event};
    use crate::agent::{DiskHealth, DiskHealthReport, SmartInfo, ZfsDiskHealth};
    use crate::notifications::{EVENT_DISK_FAILURE, EVENT_ZFS_HEALTH};

    #[test]
    fn only_online_is_healthy() {
        assert!(!is_unhealthy("ONLINE"));
        assert!(!is_unhealthy("online (no errors)"));
        assert!(!is_unhealthy(""));
        assert!(is_unhealthy("DEGRADED"));
        assert!(is_unhealthy("FAULTED"));
        assert!(is_unhealthy("UNAVAIL"));
    }

    #[test]
    fn disk_events_are_used_for_faults() {
        assert_eq!(pool_event("DEGRADED").0, EVENT_ZFS_HEALTH);
        assert_eq!(pool_event("FAULTED").0, EVENT_DISK_FAILURE);
        assert_eq!(pool_event("OFFLINE").0, EVENT_DISK_FAILURE);
    }

    fn disk(
        device: &str,
        health: &str,
        temperature_c: Option<i64>,
        wear_percent: Option<u8>,
        zfs_state: Option<&str>,
    ) -> DiskHealth {
        DiskHealth {
            device: device.to_string(),
            smart: SmartInfo {
                available: true,
                enabled: true,
                health: health.to_string(),
                temperature_c,
                wear_percent,
                ..Default::default()
            },
            zfs: zfs_state.map(|state| ZfsDiskHealth {
                pool: "tank".to_string(),
                vdev: "mirror-0".to_string(),
                state: state.to_string(),
                read_errors: 0,
                write_errors: 0,
                cksum_errors: 0,
            }),
        }
    }

    #[test]
    fn healthy_disk_has_no_alerts() {
        let report = DiskHealthReport {
            smartctl_available: true,
            disks: vec![disk(
                "/dev/sda",
                "PASSED",
                Some(34),
                Some(4),
                Some("ONLINE"),
            )],
            ..Default::default()
        };
        assert!(disk_alerts(&report, 55, 80).is_empty());
    }

    #[test]
    fn failed_smart_is_a_disk_failure() {
        let report = DiskHealthReport {
            smartctl_available: true,
            disks: vec![disk("/dev/sda", "FAILED", None, None, None)],
            ..Default::default()
        };
        let alerts = disk_alerts(&report, 55, 80);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].event, EVENT_DISK_FAILURE);
        assert_eq!(alerts[0].severity, "error");
        assert_eq!(alerts[0].key, "disk:/dev/sda:smart");
    }

    #[test]
    fn temperature_and_wear_thresholds_are_exact() {
        let at_limit = DiskHealthReport {
            smartctl_available: true,
            disks: vec![disk("/dev/sda", "PASSED", Some(55), Some(79), None)],
            ..Default::default()
        };
        assert!(disk_alerts(&at_limit, 55, 80).is_empty());

        let over_limit = DiskHealthReport {
            smartctl_available: true,
            disks: vec![disk("/dev/sda", "PASSED", Some(56), Some(80), None)],
            ..Default::default()
        };
        let alerts = disk_alerts(&over_limit, 55, 80);
        assert_eq!(alerts.len(), 2);
        assert!(alerts.iter().any(|a| a.key.ends_with(":temperature")));
        assert!(alerts.iter().any(|a| a.key.ends_with(":wear")));
        assert!(alerts.iter().all(|a| a.event == EVENT_ZFS_HEALTH));
    }

    #[test]
    fn faulted_vdev_is_a_disk_failure_and_degraded_is_a_warning() {
        let faulted = DiskHealthReport {
            smartctl_available: true,
            disks: vec![disk("/dev/sdb", "PASSED", None, None, Some("FAULTED"))],
            ..Default::default()
        };
        let alerts = disk_alerts(&faulted, 55, 80);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].event, EVENT_DISK_FAILURE);
        assert_eq!(alerts[0].severity, "error");

        let degraded = DiskHealthReport {
            smartctl_available: true,
            disks: vec![disk("/dev/sdb", "PASSED", None, None, Some("DEGRADED"))],
            ..Default::default()
        };
        let alerts = disk_alerts(&degraded, 55, 80);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].event, EVENT_ZFS_HEALTH);
        assert_eq!(alerts[0].severity, "warning");
    }

    #[test]
    fn unavailable_smart_is_not_an_alert() {
        let report = DiskHealthReport {
            smartctl_available: false,
            disks: vec![DiskHealth {
                device: "/dev/vda".to_string(),
                smart: SmartInfo {
                    available: false,
                    message: "SMART support is unavailable".to_string(),
                    ..Default::default()
                },
                zfs: None,
            }],
            ..Default::default()
        };
        assert!(disk_alerts(&report, 55, 80).is_empty());
    }
}
