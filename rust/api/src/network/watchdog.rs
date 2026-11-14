//! The API-side watchdog for the try-mode guard (FR-NET-03).
//!
//! The timer lives here so the state machine in `changes.rs` stays a pure
//! sequence of steps: a test can call `Manager::tick` directly with a fake
//! `Node`, while production gets one background loop.

use std::sync::Arc;

use super::changes::{Manager, TICK_INTERVAL};

/// Spawn the single watchdog loop. Safe to call once per process.
pub fn spawn(manager: Arc<Manager>) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(TICK_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            manager.tick().await;
        }
    });
}
