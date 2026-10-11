//! Dynamic DNS (port of `api/internal/ddns`). Entries are persisted as JSON; a
//! background manager detects the public IP on an interval and drives the
//! provider's DDNS driver. Credentials live in Kubernetes Secrets and are never
//! returned by the API.

use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::providers::Registry;

pub mod drivers;

pub use drivers::{default_driver, UpdateRequest};

/// Identifies the API to provider APIs.
pub const USER_AGENT: &str = "naslos-api/ddns";

/// One dynamic-DNS record (never holds credential values).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    pub id: String,
    pub provider: String,
    #[serde(default)]
    pub zone: String,
    #[serde(default)]
    pub record: String,
    #[serde(rename = "recordType", default)]
    pub record_type: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub ttl: i64,
    #[serde(default)]
    pub enabled: bool,
    #[serde(
        rename = "providerConfig",
        default,
        skip_serializing_if = "HashMap::is_empty"
    )]
    pub provider_config: HashMap<String, String>,
    #[serde(
        rename = "credentialsSecret",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub credentials_secret: String,
    #[serde(
        rename = "credentialFields",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub credential_fields: Vec<String>,
    #[serde(rename = "lastIP", default, skip_serializing_if = "String::is_empty")]
    pub last_ip: String,
    #[serde(
        rename = "lastStatus",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub last_status: String,
    #[serde(
        rename = "lastError",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub last_error: String,
    #[serde(rename = "lastRunAt", default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<DateTime<Utc>>,
    #[serde(rename = "nextRunAt", default, skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<DateTime<Utc>>,
    #[serde(
        rename = "lastUpdateAt",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub last_update_at: Option<DateTime<Utc>>,
    #[serde(rename = "createdAt", default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(rename = "updatedAt", default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
}

fn is_zero(n: &i64) -> bool {
    *n == 0
}

impl Entry {
    pub fn secret_name(&self) -> String {
        format!("naslos-ddns-{}", self.id)
    }
}

/// A random DNS-1123-safe entry id.
pub fn new_id() -> String {
    let mut buf = [0u8; 8];
    if getrandom::getrandom(&mut buf).is_err() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        return format!("ddns-{nanos}");
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// Persists entries as JSON with atomic writes.
pub struct Store {
    path: String,
    entries: Mutex<HashMap<String, Entry>>,
}

impl Store {
    pub fn new(path: &str) -> Self {
        Self {
            path: path.to_string(),
            entries: Mutex::new(HashMap::new()),
        }
    }

    pub fn load(&self) -> Result<(), String> {
        if self.path.is_empty() {
            return Ok(());
        }
        let data = match std::fs::read(&self.path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(format!("reading ddns file: {e}")),
        };
        let list: Vec<Entry> =
            serde_json::from_slice(&data).map_err(|e| format!("parsing ddns file: {e}"))?;
        let mut guard = self.entries.lock().unwrap();
        for e in list {
            if !e.id.is_empty() {
                guard.insert(e.id.clone(), e);
            }
        }
        Ok(())
    }

    pub fn list(&self) -> Vec<Entry> {
        let guard = self.entries.lock().unwrap();
        let mut out: Vec<Entry> = guard.values().cloned().collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    pub fn get(&self, id: &str) -> Result<Entry, String> {
        self.entries
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| format!("ddns entry {id:?} not found"))
    }

    pub fn put(&self, e: Entry) -> Result<(), String> {
        let mut guard = self.entries.lock().unwrap();
        guard.insert(e.id.clone(), e);
        self.save_locked(&guard)
    }

    pub fn delete(&self, id: &str) -> Result<(), String> {
        let mut guard = self.entries.lock().unwrap();
        if guard.remove(id).is_none() {
            return Err(format!("ddns entry {id:?} not found"));
        }
        self.save_locked(&guard)
    }

    pub fn update(&self, id: &str, mutate: impl FnOnce(&mut Entry)) -> Result<Entry, String> {
        let mut guard = self.entries.lock().unwrap();
        let e = guard
            .get_mut(id)
            .ok_or_else(|| format!("ddns entry {id:?} not found"))?;
        mutate(e);
        let out = e.clone();
        self.save_locked(&guard)?;
        Ok(out)
    }

    fn save_locked(&self, guard: &HashMap<String, Entry>) -> Result<(), String> {
        if self.path.is_empty() {
            return Ok(());
        }
        let mut list: Vec<&Entry> = guard.values().collect();
        list.sort_by(|a, b| a.id.cmp(&b.id));
        let mut data =
            serde_json::to_vec_pretty(&list).map_err(|e| format!("encoding ddns entries: {e}"))?;
        data.push(b'\n');
        crate::chartsrepo::write_atomic(&self.path, &data, 0o600, 0o750)
    }
}

/// DNS lookup seam for record pre-checks and DNS IP fetchers.
#[async_trait::async_trait]
pub trait Resolver: Send + Sync {
    async fn lookup_ip(&self, host: &str) -> Result<Vec<IpAddr>, String>;
}

/// The system resolver.
pub struct SystemResolver;

#[async_trait::async_trait]
impl Resolver for SystemResolver {
    async fn lookup_ip(&self, host: &str) -> Result<Vec<IpAddr>, String> {
        let addrs = tokio::net::lookup_host((host, 0))
            .await
            .map_err(|e| e.to_string())?;
        Ok(addrs.map(|a| a.ip()).collect())
    }
}

/// The shared HTTP client for provider APIs.
pub fn default_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent(USER_AGENT)
        .build()
        .unwrap_or_default()
}

/// Build the record's fully qualified name.
pub fn fqdn(zone: &str, record: &str) -> String {
    let record = record.trim();
    let zone = zone.trim();
    if record.is_empty() || record == "@" {
        return zone.to_string();
    }
    if record == zone || record.ends_with(&format!(".{zone}")) {
        return record.to_string();
    }
    format!("{record}.{zone}")
}

/// The record's subdomain label (`""` for the apex).
pub fn zone_label(zone: &str, record: &str) -> String {
    let record = record.trim();
    if record.is_empty() || record == "@" || record == zone {
        return String::new();
    }
    record
        .strip_suffix(&format!(".{zone}"))
        .unwrap_or(record)
        .to_string()
}

pub fn config_value(config: &HashMap<String, String>, key: &str) -> String {
    config
        .get(key)
        .map(|v| v.trim().to_string())
        .unwrap_or_default()
}

pub fn secret_value(secret: &HashMap<String, String>, key: &str) -> String {
    secret
        .get(key)
        .map(|v| v.trim().to_string())
        .unwrap_or_default()
}

pub fn normalize_ttl(ttl: i64) -> i64 {
    if ttl <= 0 {
        0
    } else {
        ttl
    }
}

pub fn truncate(s: &str) -> String {
    let s = s.trim();
    if s.len() > 200 {
        s[..200].to_string()
    } else {
        s.to_string()
    }
}

/// Fetch a public IP from a plain-text IP source URL and validate it.
pub async fn detect_ip(client: &reqwest::Client, source: &str) -> Result<String, String> {
    let source = source.trim();
    if source.is_empty() {
        return Err("no public-IP source is configured".to_string());
    }
    let resp = client
        .get(source)
        .send()
        .await
        .map_err(|e| format!("detecting the public IP: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!(
            "IP source {source} returned HTTP {}",
            resp.status().as_u16()
        ));
    }
    let body = resp
        .text()
        .await
        .map_err(|e| format!("reading the IP source response: {e}"))?;
    let ip: IpAddr = body
        .trim()
        .parse()
        .map_err(|_| format!("IP source {source} returned {:?}, not an IP", body.trim()))?;
    Ok(ip.to_string())
}

/// Try each source in order and return the first valid address of the requested
/// family (HTTP(S) URL or `dns:opendns` / `dns:google`).
pub async fn detect_ip_any(
    client: &reqwest::Client,
    want_v6: bool,
    sources: &[String],
) -> Result<String, String> {
    if sources.is_empty() {
        return Err("no public-IP source is configured".to_string());
    }
    let mut errs = Vec::new();
    for source in sources {
        let source = source.trim();
        if source.is_empty() {
            continue;
        }
        let result = match source.strip_prefix("dns:") {
            Some(name) => detect_dns_ip(name, want_v6).await,
            None => detect_ip(client, source).await,
        };
        match result {
            Ok(ip) => {
                if ip_matches_family(&ip, want_v6) {
                    return Ok(ip);
                }
                errs.push(format!(
                    "source {source} returned {ip}, wrong address family"
                ));
            }
            Err(e) => errs.push(e),
        }
    }
    Err(format!("all public-IP sources failed: {}", errs.join("; ")))
}

fn ip_matches_family(ip: &str, want_v6: bool) -> bool {
    match ip.parse::<IpAddr>() {
        Ok(parsed) => want_v6 == parsed.is_ipv6(),
        Err(_) => false,
    }
}

/// Resolve the public IP through a well-known DNS echo service.
async fn detect_dns_ip(name: &str, want_v6: bool) -> Result<String, String> {
    use hickory_resolver::config::{NameServerConfigGroup, ResolverConfig, ResolverOpts};
    use hickory_resolver::TokioAsyncResolver;

    let (server, host, use_txt) = match name.trim() {
        "opendns" => ("208.67.222.222", "myip.opendns.com", false),
        "google" => ("8.8.8.8", "o-o.myaddr.l.google.com", true),
        other => return Err(format!("unknown DNS source {other:?}")),
    };
    let ip: IpAddr = server.parse().map_err(|_| "bad DNS server".to_string())?;
    let group = NameServerConfigGroup::from_ips_clear(&[ip], 53, true);
    let resolver = TokioAsyncResolver::tokio(
        ResolverConfig::from_parts(None, vec![], group),
        ResolverOpts::default(),
    );
    if use_txt {
        let txts = resolver
            .txt_lookup(host)
            .await
            .map_err(|e| format!("dns:{name}: {e}"))?;
        for txt in txts.iter() {
            let value = txt.to_string().trim_matches('"').trim().to_string();
            if let Ok(parsed) = value.parse::<IpAddr>() {
                return Ok(parsed.to_string());
            }
        }
        Err(format!("dns:{name} returned no address"))
    } else {
        let lookup = resolver
            .lookup_ip(host)
            .await
            .map_err(|e| format!("dns:{name}: {e}"))?;
        for addr in lookup.iter() {
            if want_v6 == addr.is_ipv6() {
                return Ok(addr.to_string());
            }
        }
        lookup
            .iter()
            .next()
            .map(|a| a.to_string())
            .ok_or_else(|| format!("dns:{name} returned no address"))
    }
}

/// Options for the manager.
pub struct Options {
    pub registry: Arc<Registry>,
    pub store_path: String,
    pub apps_namespace: String,
    pub kube: Option<Arc<crate::kube::Client>>,
    pub ip_sources: Vec<String>,
    pub ipv6_sources: Vec<String>,
    pub interval: Duration,
    pub cooldown: Duration,
}

/// Detects the public IP and keeps entries converged.
pub struct Manager {
    registry: Arc<Registry>,
    store: Store,
    apps_namespace: String,
    kube: Option<Arc<crate::kube::Client>>,
    ip_sources: Vec<String>,
    ipv6_sources: Vec<String>,
    interval: Duration,
    cooldown: Duration,
    client: reqwest::Client,
    resolver: Arc<dyn Resolver>,
    last_load_err: Option<String>,
}

impl Manager {
    pub fn new(opts: Options) -> Self {
        let mut m = Self {
            registry: opts.registry,
            store: Store::new(&opts.store_path),
            apps_namespace: opts.apps_namespace,
            kube: opts.kube,
            ip_sources: opts.ip_sources,
            ipv6_sources: opts.ipv6_sources,
            interval: if opts.interval.is_zero() {
                Duration::from_secs(300)
            } else {
                opts.interval
            },
            cooldown: if opts.cooldown.is_zero() {
                Duration::from_secs(300)
            } else {
                opts.cooldown
            },
            client: default_client(),
            resolver: Arc::new(SystemResolver),
            last_load_err: None,
        };
        if let Err(e) = m.store.load() {
            m.last_load_err = Some(e);
        }
        m
    }

    pub fn load_error(&self) -> Option<String> {
        self.last_load_err.clone()
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// Run the reconcile loop immediately, then on each tick.
    pub async fn run_loop(self: Arc<Self>) {
        self.reconcile(false).await;
        let mut ticker = tokio::time::interval(self.interval);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            self.reconcile(false).await;
        }
    }

    /// Detect the public IP once and converge every enabled entry.
    pub async fn reconcile(&self, force: bool) {
        let entries = self.store.list();
        if entries.is_empty() {
            return;
        }
        let (mut need4, mut need6) = (false, false);
        for e in &entries {
            if !e.enabled {
                continue;
            }
            if e.record_type == "AAAA" {
                need6 = true;
            } else {
                need4 = true;
            }
        }
        let mut v4 = String::new();
        let mut v6 = String::new();
        let mut err4: Option<String> = None;
        let mut err6: Option<String> = None;
        if need4 {
            match detect_ip_any(&self.client, false, &self.ip_sources).await {
                Ok(ip) => v4 = ip,
                Err(e) => err4 = Some(e),
            }
        }
        if need6 {
            match detect_ip_any(&self.client, true, &self.ipv6_sources).await {
                Ok(ip) => v6 = ip,
                Err(e) => err6 = Some(e),
            }
        }
        for e in self.store.list() {
            if !e.enabled {
                continue;
            }
            let (ip, ip_err) = if e.record_type == "AAAA" {
                (v6.clone(), err6.clone())
            } else {
                (v4.clone(), err4.clone())
            };
            if let Some(err) = ip_err {
                self.record_error(&e.id, &err);
                continue;
            }
            let _ = self.run_entry(e, &ip, force).await;
        }
    }

    /// Reconcile one entry, detecting the IP for its record type.
    pub async fn run(&self, id: &str, force: bool) -> Result<(), String> {
        let e = self.store.get(id)?;
        let want_v6 = e.record_type == "AAAA";
        let sources = if want_v6 {
            &self.ipv6_sources
        } else {
            &self.ip_sources
        };
        let ip = match detect_ip_any(&self.client, want_v6, sources).await {
            Ok(ip) => ip,
            Err(e) => {
                self.record_error(id, &e);
                return Err(e);
            }
        };
        self.run_entry(e, &ip, force).await
    }

    async fn run_entry(&self, e: Entry, ip: &str, force: bool) -> Result<(), String> {
        let (driver, req) = match self.prepare(e.clone(), ip).await {
            Ok(v) => v,
            Err(err) => {
                self.record_error(&e.id, &err);
                return Err(err);
            }
        };
        if !force && self.should_skip(&e, &req, ip).await {
            self.record_skip(&e.id, ip);
            return Ok(());
        }
        if let Err(err) = driver.update(&req).await {
            let safe = sanitize_error(&err, &req.secret);
            self.record_error(&e.id, &safe);
            return Err(safe);
        }
        self.record_update(&e.id, ip);
        Ok(())
    }

    /// Decide whether the record already holds the public IP.
    async fn should_skip(&self, e: &Entry, req: &UpdateRequest, ip: &str) -> bool {
        let now = Utc::now();
        if !self.cooldown.is_zero() {
            if let Some(last) = e.last_update_at {
                if (now - last)
                    .to_std()
                    .map(|d| d < self.cooldown)
                    .unwrap_or(false)
                {
                    return true;
                }
            }
        }
        if req
            .config
            .get("proxied")
            .map(|v| v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
        {
            return e.last_ip == ip && e.last_status == "ok";
        }
        match self
            .resolve_record(&fqdn(&req.zone, &req.record), &req.record_type)
            .await
        {
            Ok(ips) => ips.iter().any(|r| r == ip),
            Err(_) => false,
        }
    }

    async fn resolve_record(&self, host: &str, record_type: &str) -> Result<Vec<String>, String> {
        let addrs = self.resolver.lookup_ip(host).await?;
        let want_v6 = record_type == "AAAA";
        Ok(addrs
            .into_iter()
            .filter(|ip| want_v6 == ip.is_ipv6())
            .map(|ip| ip.to_string())
            .collect())
    }

    /// Resolve the provider, credentials and config for an entry.
    async fn prepare(
        &self,
        e: Entry,
        ip: &str,
    ) -> Result<(Box<dyn drivers::Driver>, UpdateRequest), String> {
        let p = self
            .registry
            .get(&e.provider)
            .ok_or_else(|| format!("unknown provider {:?}", e.provider))?;
        if !p.has_ddns() {
            return Err(format!(
                "provider {:?} does not support dynamic DNS",
                e.provider
            ));
        }
        let secret = self.read_secret(&e, p).await?;
        let mut config: HashMap<String, String> = p
            .ddns
            .as_ref()
            .map(|d| d.defaults.clone())
            .unwrap_or_default();
        for (k, v) in &e.provider_config {
            config.insert(k.clone(), v.clone());
        }
        let driver_name = p
            .ddns
            .as_ref()
            .map(|d| d.driver.clone())
            .unwrap_or_default();
        let driver = default_driver(&driver_name, self.client.clone())?;
        Ok((
            driver,
            UpdateRequest {
                zone: e.zone,
                record: e.record,
                record_type: e.record_type,
                ip: ip.to_string(),
                ttl: e.ttl,
                config,
                secret,
            },
        ))
    }

    async fn read_secret(
        &self,
        e: &Entry,
        p: &crate::providers::Provider,
    ) -> Result<HashMap<String, String>, String> {
        let fields = p.secret_fields();
        if fields.is_empty() {
            return Ok(HashMap::new());
        }
        if e.credentials_secret.is_empty() {
            return Err(format!(
                "provider {:?} needs credentials but the entry has no Secret",
                p.name
            ));
        }
        let Some(kube) = &self.kube else {
            return Err(format!(
                "no Kubernetes client available to read {}",
                e.credentials_secret
            ));
        };
        let secret = kube
            .get_json("secret", &e.credentials_secret, &self.apps_namespace)
            .await?
            .ok_or_else(|| {
                format!(
                    "reading Secret {}/{}: not found",
                    self.apps_namespace, e.credentials_secret
                )
            })?;
        let data = secret
            .get("data")
            .and_then(|d| d.as_object())
            .cloned()
            .unwrap_or_default();
        let mut out = HashMap::new();
        for f in &fields {
            let key = f.secret_key_or();
            if let Some(v) = data.get(&key).and_then(|v| v.as_str()) {
                if let Ok(raw) = base64_decode(v) {
                    out.insert(f.key.clone(), String::from_utf8_lossy(&raw).into_owned());
                }
            }
        }
        for f in &fields {
            if f.required && out.get(&f.key).map(|v| v.trim().is_empty()).unwrap_or(true) {
                return Err(format!(
                    "Secret {} is missing {}",
                    e.credentials_secret,
                    f.secret_key_or()
                ));
            }
        }
        Ok(out)
    }

    fn record_error(&self, id: &str, message: &str) {
        self.record(id, "error", message, "");
    }

    fn record_skip(&self, id: &str, ip: &str) {
        self.record(id, "ok", "", ip);
    }

    fn record_update(&self, id: &str, ip: &str) {
        let now = Utc::now();
        let interval = chrono::Duration::from_std(self.interval).unwrap_or_default();
        let _ = self.store.update(id, |e| {
            e.last_run_at = Some(now);
            e.next_run_at = Some(now + interval);
            e.last_update_at = Some(now);
            e.last_status = "ok".to_string();
            e.last_error.clear();
            e.last_ip = ip.to_string();
        });
    }

    fn record(&self, id: &str, status: &str, last_err: &str, ip: &str) {
        let now = Utc::now();
        let interval = chrono::Duration::from_std(self.interval).unwrap_or_default();
        let _ = self.store.update(id, |e| {
            e.last_run_at = Some(now);
            e.next_run_at = Some(now + interval);
            e.last_status = status.to_string();
            e.last_error = last_err.to_string();
            if !ip.is_empty() {
                e.last_ip = ip.to_string();
            }
        });
    }
}

/// Redact any secret value from an error before it is recorded or returned.
pub fn sanitize_error(msg: &str, secret: &HashMap<String, String>) -> String {
    let mut out = msg.to_string();
    for value in secret.values() {
        if value.len() >= 4 {
            out = out.replace(value, "***");
        }
    }
    out
}

/// Write a credential Secret, merging over existing keys.
pub async fn write_secret(
    kube: &crate::kube::Client,
    namespace: &str,
    name: &str,
    data: &HashMap<String, String>,
) -> Result<(), String> {
    if name.is_empty() {
        return Err("no Secret name".to_string());
    }
    let existing = kube.get_json("secret", name, namespace).await?;
    let mut merged: HashMap<String, String> = HashMap::new();
    if let Some(existing) = &existing {
        if let Some(d) = existing.get("data").and_then(|v| v.as_object()) {
            for (k, v) in d {
                if let Some(s) = v.as_str() {
                    if let Ok(raw) = base64_decode(s) {
                        merged.insert(k.clone(), String::from_utf8_lossy(&raw).into_owned());
                    }
                }
            }
        }
    }
    for (k, v) in data {
        merged.insert(k.clone(), v.clone());
    }
    kube.write_secret(namespace, name, &merged).await
}

/// Delete a credential Secret, ignoring a missing one.
pub async fn delete_secret(
    kube: &crate::kube::Client,
    namespace: &str,
    name: &str,
) -> Result<(), String> {
    if name.is_empty() {
        return Ok(());
    }
    kube.delete("secret", name, namespace).await
}

fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fqdn_and_label() {
        assert_eq!(fqdn("example.com", "home"), "home.example.com");
        assert_eq!(fqdn("example.com", "@"), "example.com");
        assert_eq!(zone_label("example.com", "home"), "home");
        assert_eq!(zone_label("example.com", "@"), "");
    }

    #[test]
    fn store_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ddns.json");
        let store = Store::new(&path.display().to_string());
        store
            .put(Entry {
                id: "abc".into(),
                provider: "cloudflare".into(),
                ..Default::default()
            })
            .unwrap();
        let reloaded = Store::new(&path.display().to_string());
        reloaded.load().unwrap();
        assert_eq!(reloaded.get("abc").unwrap().provider, "cloudflare");
    }

    #[test]
    fn sanitize_error_redacts_secrets() {
        let mut secret = HashMap::new();
        secret.insert("token".to_string(), "supersecret".to_string());
        let out = sanitize_error("failed with supersecret in url", &secret);
        assert!(!out.contains("supersecret"));
        assert!(out.contains("***"));
    }

    #[test]
    fn ip_family_matching() {
        assert!(ip_matches_family("1.2.3.4", false));
        assert!(!ip_matches_family("1.2.3.4", true));
        assert!(ip_matches_family("2001:db8::1", true));
    }
}
