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

/// Per-source auth failure tracking, keyed by the socket peer IP **and** by
/// the presented token digest. In memory only. A valid token is checked
/// before this table and is never blocked (R-024); the per-token key stops a
/// fail-only client from exhausting its own bucket and taking every other
/// client behind the same proxy address down with it.
#[derive(Default)]
pub struct FailureTrack {
    inner: Mutex<HashMap<String, Entry>>,
}

#[derive(Default)]
struct Entry {
    failures: VecDeque<Instant>,
    blocked_until: Option<Instant>,
}

impl FailureTrack {
    /// The key for a socket peer address.
    pub fn ip_key(ip: IpAddr) -> String {
        format!("ip:{ip}")
    }

    /// The key for the presented bearer token. Only a digest prefix is kept,
    /// never the plaintext. An empty header shares one bucket.
    pub fn token_key(presented: &str) -> String {
        if presented.is_empty() {
            return "token:-".to_string();
        }
        let digest = crate::mcp::store::sha256_hex_for_tracking(presented.as_bytes());
        format!("token:{}", &digest[..32])
    }

    /// Whether a key is currently blocked. Also drops expired bookkeeping so
    /// the map cannot grow without bound on a long-lived process.
    pub fn is_blocked(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut inner = self.inner.lock().unwrap();
        inner.retain(|_, entry| {
            entry
                .failures
                .retain(|t| now.duration_since(*t) < FAIL_WINDOW);
            entry.blocked_until.is_some_and(|until| until > now) || !entry.failures.is_empty()
        });
        inner
            .get(key)
            .and_then(|e| e.blocked_until)
            .is_some_and(|until| until > now)
    }

    pub fn record_failure(&self, key: &str) {
        let now = Instant::now();
        let mut inner = self.inner.lock().unwrap();
        let entry = inner.entry(key.to_string()).or_default();
        entry.failures.push_back(now);
        entry
            .failures
            .retain(|t| now.duration_since(*t) < FAIL_WINDOW);
        if entry.failures.len() >= FAIL_LIMIT {
            entry.blocked_until = Some(now + FAIL_WINDOW);
            entry.failures.clear();
        }
    }

    pub fn record_success(&self, key: &str) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(entry) = inner.get_mut(key) {
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
            assert!(!track.is_blocked(&FailureTrack::ip_key(ip)));
            track.record_failure(&FailureTrack::ip_key(ip));
        }
        assert!(
            track.is_blocked(&FailureTrack::ip_key(ip)),
            "10 failures must block the IP"
        );
        track.record_success(&FailureTrack::ip_key(ip));
        assert!(!track.is_blocked(&FailureTrack::ip_key(ip)));
    }

    #[test]
    fn other_ips_are_unaffected() {
        let track = FailureTrack::default();
        let a = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 9));
        let b = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 10));
        for _ in 0..FAIL_LIMIT {
            track.record_failure(&FailureTrack::ip_key(a));
        }
        assert!(track.is_blocked(&FailureTrack::ip_key(a)));
        assert!(!track.is_blocked(&FailureTrack::ip_key(b)));
    }
}
