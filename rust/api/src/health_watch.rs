//! Periodic ZFS/agent health watcher (restores the Go `startHealthWatcher`
//! behaviour lost in the Rust port, SEC-2026-H6).
//!
//! It notifies only on transitions: a pool that stays degraded does not spam
//! ntfy every minute, and a pool that recovers can notify again on the next
//! failure.

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
    use super::{is_unhealthy, pool_event};
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
}
