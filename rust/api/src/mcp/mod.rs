//! The MCP endpoint (FR-MCP): a Streamable HTTP MCP server inside the API
//! pod, gated by an operator toggle and scoped bearer tokens.
//!
//! Request path: Traefik `Host(apex) && PathPrefix('/mcp')` (no Authelia
//! forwardAuth) -> [`auth::guard`] (proxy secret + CIDR + bearer token,
//! SEC-10 has no opt-out) -> [`service::entry`] (rmcp Streamable HTTP,
//! stateless) -> [`tools`] (declarative registry) -> the internal router
//! (the real handlers with a synthetic `mcp:<token>` identity).

pub mod auth;
pub mod service;
pub mod store;
pub mod tools;

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use store::Store;

/// After this many failed bearer attempts from one source IP inside
/// `FAIL_WINDOW`, the IP is refused outright for `FAIL_WINDOW` more.
const FAIL_LIMIT: usize = 10;
const FAIL_WINDOW: Duration = Duration::from_secs(5 * 60);

/// Per-source-IP auth failure tracking. In memory only; the peer address is
/// the real socket address (Traefik's pod IP), so it cannot be spoofed.
#[derive(Default)]
pub struct FailureTrack {
    inner: Mutex<HashMap<IpAddr, Entry>>,
}

#[derive(Default)]
struct Entry {
    failures: VecDeque<Instant>,
    blocked_until: Option<Instant>,
}

impl FailureTrack {
    /// Whether an IP is currently blocked. Also drops expired bookkeeping so
    /// the map cannot grow without bound on a long-lived process.
    pub fn is_blocked(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let mut inner = self.inner.lock().unwrap();
        inner.retain(|_, entry| {
            entry
                .failures
                .retain(|t| now.duration_since(*t) < FAIL_WINDOW);
            entry.blocked_until.is_some_and(|until| until > now) || !entry.failures.is_empty()
        });
        inner
            .get(&ip)
            .and_then(|e| e.blocked_until)
            .is_some_and(|until| until > now)
    }

    pub fn record_failure(&self, ip: IpAddr) {
        let now = Instant::now();
        let mut inner = self.inner.lock().unwrap();
        let entry = inner.entry(ip).or_default();
        entry.failures.push_back(now);
        entry
            .failures
            .retain(|t| now.duration_since(*t) < FAIL_WINDOW);
        if entry.failures.len() >= FAIL_LIMIT {
            entry.blocked_until = Some(now + FAIL_WINDOW);
            entry.failures.clear();
        }
    }

    pub fn record_success(&self, ip: IpAddr) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(entry) = inner.get_mut(&ip) {
            entry.failures.clear();
            entry.blocked_until = None;
        }
    }
}

/// Everything the MCP endpoint keeps on the `AppState`.
pub struct McpState {
    pub store: Store,
    pub failures: FailureTrack,
}

impl McpState {
    pub fn from_env() -> Self {
        Self {
            store: Store::from_env(),
            failures: FailureTrack::default(),
        }
    }

    pub fn for_test() -> Self {
        Self {
            store: Store::new(""),
            failures: FailureTrack::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn failures_block_after_the_limit_and_clear_on_success() {
        let track = FailureTrack::default();
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 9));
        for _ in 0..FAIL_LIMIT {
            assert!(!track.is_blocked(ip));
            track.record_failure(ip);
        }
        assert!(track.is_blocked(ip), "10 failures must block the IP");
        track.record_success(ip);
        assert!(!track.is_blocked(ip));
    }

    #[test]
    fn other_ips_are_unaffected() {
        let track = FailureTrack::default();
        let a = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 9));
        let b = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 10));
        for _ in 0..FAIL_LIMIT {
            track.record_failure(a);
        }
        assert!(track.is_blocked(a));
        assert!(!track.is_blocked(b));
    }
}
