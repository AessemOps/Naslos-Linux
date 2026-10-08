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
    /// The chart-repository manager (S4d). Absent when no cache is configured.
    pub charts: Option<Arc<crate::chartsrepo::Manager>>,
    /// The swappable catalog snapshot (S4d).
    pub catalog: Arc<CatalogHolder>,
    /// The installed-app manager (S4e). Absent when chart repos are unavailable.
    pub app_manager: Option<Arc<crate::apps::Manager>>,
    /// The in-memory async app-job registry (S4e).
    pub app_jobs: Arc<crate::server::app_jobs::AppJobManager>,
    /// The primary base domain app subdomains hang off.
    pub base_domain: String,
}

/// Holds the current catalog snapshot; refresh replaces it atomically.
#[derive(Default)]
pub struct CatalogHolder {
    inner: std::sync::Mutex<Option<Arc<crate::catalog::Catalog>>>,
}

impl CatalogHolder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn load(&self) -> Option<Arc<crate::catalog::Catalog>> {
        self.inner.lock().unwrap().clone()
    }

    pub fn store(&self, catalog: Arc<crate::catalog::Catalog>) {
        *self.inner.lock().unwrap() = Some(catalog);
    }
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

        let (charts, catalog) = build_chart_repos();
        let app_manager = build_app_manager(&charts, &catalog);

        Ok(Self {
            proxy_secret,
            trusted_cidrs,
            metrics: Manager::new(),
            talos: crate::talos::TalosClient::from_env().map(Arc::new),
            agent: crate::agent::Client::from_env().ok().map(Arc::new),
            shares: std::sync::Mutex::new(crate::shares::Manager::from_env()),
            samba_users: std::sync::Mutex::new(crate::shares::SambaUserStore::from_env()),
            identity: crate::identity::Client::from_env().ok().map(Arc::new),
            charts: charts.clone(),
            catalog,
            app_manager,
            app_jobs: Arc::new(crate::server::app_jobs::AppJobManager::new()),
            base_domain: std::env::var("BASE_DOMAIN").unwrap_or_default(),
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
            charts: None,
            catalog: Arc::new(CatalogHolder::new()),
            app_manager: None,
            app_jobs: Arc::new(crate::server::app_jobs::AppJobManager::new()),
            base_domain: String::new(),
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

/// Build the chart-repository manager and the initial catalog snapshot.
/// Credentials come from a Kubernetes Secret in the Go server; the Rust port
/// wires a public-only provider for now (documented in the plan), so private
/// sources are added but not yet clonable.
fn build_chart_repos() -> (Option<Arc<crate::chartsrepo::Manager>>, Arc<CatalogHolder>) {
    use crate::chartsrepo::{GitCliBackend, Manager, Source, Store, DEFAULT_TTL};

    let sources = Arc::new(Store::new(
        &std::env::var("SOURCES_CONFIG")
            .unwrap_or_else(|_| "/var/lib/naslos/sources.json".to_string()),
    ));
    if let Err(e) = sources.load() {
        tracing::warn!("could not load chart sources: {e}");
    }

    let cache_dir =
        std::env::var("CHARTS_CACHE_DIR").unwrap_or_else(|_| "/var/lib/naslos/charts".to_string());
    let ttl = std::env::var("CHARTS_TTL")
        .ok()
        .and_then(|v| parse_go_duration(&v))
        .unwrap_or(DEFAULT_TTL);
    let charts = Arc::new(Manager::new(
        sources.clone(),
        &cache_dir,
        ttl,
        None,
        Arc::new(GitCliBackend::new()),
    ));

    // Seed the official source once, from the chart's environment.
    if let Ok(url) = std::env::var("SOURCES_OFFICIAL_URL") {
        if !url.is_empty() {
            let name =
                std::env::var("SOURCES_OFFICIAL_NAME").unwrap_or_else(|_| "naslos".to_string());
            if sources.get(&name).is_err() {
                let official = Source {
                    name,
                    display_name: std::env::var("SOURCES_OFFICIAL_DISPLAY")
                        .unwrap_or_else(|_| "NaslosCharts".to_string()),
                    url,
                    official: true,
                    ..Default::default()
                };
                if let Err(e) = charts.add_source(official) {
                    tracing::warn!("could not seed the official chart source: {e}");
                }
            }
        }
    }

    let catalog = Arc::new(CatalogHolder::new());
    catalog.store(Arc::new(build_catalog(&charts)));
    (Some(charts), catalog)
}

/// Build the catalog snapshot from every source/channel working tree.
pub fn build_catalog(charts: &crate::chartsrepo::Manager) -> crate::catalog::Catalog {
    let mut refs = Vec::new();
    for src in charts.sources() {
        for channel in src.channel_names() {
            let Ok(dir) = charts.dir(&src.name, &channel) else {
                continue;
            };
            refs.push(crate::catalog::SourceRef {
                name: src.name.clone(),
                display_name: src.display_name.clone(),
                channel,
                dir,
                official: src.official,
            });
        }
    }
    crate::catalog::Catalog::load(&refs)
}

/// Parse a Go duration string ("15m", "1h30m", "30s") into a `Duration`.
fn parse_go_duration(s: &str) -> Option<std::time::Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let mut total = std::time::Duration::ZERO;
    let mut num = String::new();
    for ch in s.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            num.push(ch);
            continue;
        }
        let value: f64 = num.parse().ok()?;
        num.clear();
        let unit = match ch {
            's' => 1.0,
            'm' => 60.0,
            'h' => 3600.0,
            _ => return None,
        };
        total += std::time::Duration::from_secs_f64(value * unit);
    }
    Some(total)
}

/// Build the installed-app manager (S4e). Routing (S5) and Service discovery
/// (kube) are wired as `None` for now, so installs record + render but do not
/// yet route; that is documented in the plan.
fn build_app_manager(
    charts: &Option<Arc<crate::chartsrepo::Manager>>,
    catalog: &Arc<CatalogHolder>,
) -> Option<Arc<crate::apps::Manager>> {
    let charts = charts.clone()?;
    let apps_ns = std::env::var("APPS_NAMESPACE").unwrap_or_else(|_| "naslos-apps".to_string());
    let apps_priv_ns = std::env::var("APPS_PRIVILEGED_NAMESPACE")
        .unwrap_or_else(|_| "naslos-apps-priv".to_string());
    let catalog_holder = catalog.clone();
    let cfg = crate::apps::Config {
        store_path: std::env::var("APPS_CONFIG")
            .unwrap_or_else(|_| "/var/lib/naslos/apps.json".to_string()),
        helm: Arc::new(crate::helm::Client::new(&apps_ns)),
        helm_privileged: Some(Arc::new(crate::helm::Client::new(&apps_priv_ns))),
        privileged_namespace: apps_priv_ns,
        charts,
        catalog: Arc::new(move || catalog_holder.load()),
        router: None,
        discoverer: None,
        base_domain: std::env::var("BASE_DOMAIN").unwrap_or_default(),
        sso_domains: None,
    };
    match crate::apps::Manager::new(cfg) {
        Ok(m) => Some(Arc::new(m)),
        Err(e) => {
            tracing::warn!("could not load installed-app records: {e}");
            None
        }
    }
}
