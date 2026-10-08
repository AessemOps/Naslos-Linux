//! Shared API state: auth config, the metrics manager and (later) the ported
//! service clients.

use crate::auth::UserInfo;
use crate::metrics::Manager;
use std::net::IpAddr;
use std::sync::Arc;

/// Trusted-source CIDR. Mirrors the Go `Middleware`'s parsed `trustedCIDRs`.
pub struct Cidr {
    base: Option<IpAddr>,
    len: u8,
}

impl Cidr {
    pub fn parse(s: &str) -> Option<Self> {
        let net = s.trim().parse::<ipnet::IpNet>().ok()?;
        Some(Self {
            base: Some(net.addr()),
            len: net.prefix_len(),
        })
    }

    pub fn contains(&self, ip: &IpAddr) -> bool {
        match (self.base, ip) {
            (Some(IpAddr::V4(b)), IpAddr::V4(x)) => ipnet::Ipv4Net::new(b, self.len)
                .map(|n| n.contains(x))
                .unwrap_or(false),
            (Some(IpAddr::V6(b)), IpAddr::V6(x)) => ipnet::Ipv6Net::new(b, self.len)
                .map(|n| n.contains(x))
                .unwrap_or(false),
            _ => false,
        }
    }
}

pub struct AppState {
    pub proxy_secret: String,
    pub trusted_cidrs: Vec<Cidr>,
    pub metrics: Manager,
    /// Present once the Talos CLI adapter is wired (S1 keeps it optional).
    pub talos: Option<Arc<crate::talos::TalosClient>>,
    /// The agent client (S2). Absent when `AGENT_TOKEN` is unset.
    pub agent: Option<Arc<crate::agent::Client>>,
    /// The share definitions (S3b); interior-mutable for the CRUD handlers.
    pub shares: std::sync::Mutex<crate::shares::Manager>,
    /// The SMB account mirror (S3c).
    pub samba_users: std::sync::Mutex<crate::shares::SambaUserStore>,
    /// The LDAP identity client (S3a). Absent when LDAP config is unusable.
    pub identity: Option<Arc<crate::identity::Client>>,
}

impl AppState {
    /// Build from the process env, fail-closed like the Go server.
    pub fn from_env() -> Result<Self, String> {
        let proxy_secret = std::env::var("PROXY_SHARED_SECRET").unwrap_or_default();
        if proxy_secret.is_empty() {
            return Err(
                "PROXY_SHARED_SECRET is required; set it from the naslos-proxy Secret".into(),
            );
        }
        let cidr_env = std::env::var("TRAEFIK_CIDR").unwrap_or_else(|_| "10.0.0.0/8".to_string());
        let trusted_cidrs = parse_cidrs(&cidr_env)?;

        Ok(Self {
            proxy_secret,
            trusted_cidrs,
            metrics: Manager::new(),
            talos: crate::talos::TalosClient::from_env().map(Arc::new),
            agent: crate::agent::Client::from_env().ok().map(Arc::new),
            shares: std::sync::Mutex::new(crate::shares::Manager::from_env()),
            samba_users: std::sync::Mutex::new(crate::shares::SambaUserStore::from_env()),
            identity: crate::identity::Client::from_env().ok().map(Arc::new),
        })
    }

    /// A state for tests: explicit secret, permissive CIDR, no Talos/agent.
    pub fn for_test(proxy_secret: &str) -> Self {
        Self {
            proxy_secret: proxy_secret.to_string(),
            trusted_cidrs: vec![Cidr::parse("0.0.0.0/0").unwrap()],
            metrics: Manager::new(),
            talos: None,
            agent: None,
            shares: std::sync::Mutex::new(crate::shares::Manager::new("", "/var/mnt")),
            samba_users: std::sync::Mutex::new(crate::shares::SambaUserStore::new("")),
            identity: None,
        }
    }
}

/// Parse a comma-separated CIDR list; a bad entry is a startup error, matching
/// the Go `NewMiddleware`.
pub fn parse_cidrs(s: &str) -> Result<Vec<Cidr>, String> {
    let mut out = Vec::new();
    for raw in s.split(',') {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        match Cidr::parse(raw) {
            Some(c) => out.push(c),
            None => return Err(format!("parsing CIDR {raw:?}: invalid CIDR")),
        }
    }
    Ok(out)
}

/// Whether the user is an admin (used by handlers).
pub fn is_admin(user: &UserInfo) -> bool {
    user.is_admin()
}
