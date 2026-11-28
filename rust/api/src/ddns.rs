//! Dynamic DNS (port of `api/internal/ddns`). Entries are persisted as JSON; a
//! background manager detects the public IP on an interval and drives the
//! provider's DDNS driver. Credentials live in Kubernetes Secrets in the
//! key-custody namespace and are never returned by the API.

use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::providers::Registry;

pub mod drivers;

pub use drivers::{default_driver, UpdateRequest};

/// Identifies the API to provider APIs.
pub const USER_AGENT: &str = "naslos-api/ddns";

/// The label Naslos sets on every Secret it writes.
pub const MANAGED_BY_LABEL: &str = "app.kubernetes.io/managed-by";
/// The value of [`MANAGED_BY_LABEL`] that marks a Naslos-managed Secret.
pub const MANAGED_BY_VALUE: &str = "naslos-api";
/// The label naming the DDNS entry a credential Secret belongs to.
pub const DDNS_ENTRY_LABEL: &str = "naslos.io/ddns-entry";

/// How often a failing credential migration is retried before it stops (a
/// broken Secret must not turn every reconcile tick into Kubernetes calls).
const MIGRATE_MAX_ATTEMPTS: u32 = 5;

/// The key-custody namespace DDNS credential Secrets live in. It is the same
/// dedicated, workload-free namespace as dataset keys and cloud credentials
/// (FR-ENC-03, FR-DNS-04): user apps and arbitrary Helm charts run in the apps
/// namespace and can read every Secret there, so provider credentials must
/// never be written beside them.
pub fn keys_namespace() -> String {
    crate::dataset_encryption::keys_namespace()
}

/// The Secret name Naslos generates for an entry it stores.
pub fn generated_secret_name(entry_id: &str) -> String {
    format!("naslos-ddns-{entry_id}")
}

/// The migration decision for one entry's credential Secret. Pure so the
/// upgrade path is unit-tested without a cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationStep {
    /// Nothing to migrate.
    None,
    /// Copy the apps-namespace Secret into the keys namespace. `delete_source`
    /// is set only when Naslos created it, so an operator-provisioned Secret is
    /// copied but left in place.
    Copy { delete_source: bool },
    /// The keys-namespace copy exists; remove the leftover apps-namespace copy
    /// because Naslos created it.
    DeleteSource,
}

/// Decide what the upgrade migration does for one entry's Secret, given which
/// copies exist and whether the apps-namespace copy carries the managed label.
///
/// The keys-namespace copy always wins: when both exist, only a Naslos-created
/// apps-namespace leftover is removed, and an operator Secret is untouched.
pub fn plan_credential_migration(
    entry_id: &str,
    secret_name: &str,
    keys_exists: bool,
    apps_exists: bool,
    apps_managed: bool,
) -> MigrationStep {
    if secret_name.is_empty() {
        return MigrationStep::None;
    }
    let naslos_created = apps_managed || secret_name == generated_secret_name(entry_id);
    if keys_exists {
        if apps_exists && naslos_created {
            MigrationStep::DeleteSource
        } else {
            MigrationStep::None
        }
    } else if apps_exists {
        MigrationStep::Copy {
            delete_source: naslos_created,
        }
    } else {
        MigrationStep::None
    }
}

/// Whether a Secret carries `app.kubernetes.io/managed-by=naslos-api`.
pub fn is_managed_secret(secret: &serde_json::Value) -> bool {
    secret
        .get("metadata")
        .and_then(|m| m.get("labels"))
        .and_then(|labels| labels.get(MANAGED_BY_LABEL))
        .and_then(|value| value.as_str())
        == Some(MANAGED_BY_VALUE)
}

/// Decode a Secret's values into plain text. A real API server returns `data`,
/// but a `stringData` payload (a dry-run object, or a test fixture) is accepted
/// too, exactly as the API server would fold it in. Used for the values the API
/// sends to a provider; the migration never round-trips through this (see
/// [`raw_secret_data`]), so a non-UTF-8 value cannot be silently rewritten.
pub fn secret_values(secret: Option<&serde_json::Value>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (key, encoded) in raw_secret_data(secret) {
        if let Ok(raw) = base64_decode(&encoded) {
            out.insert(key, String::from_utf8_lossy(&raw).into_owned());
        }
    }
    out
}

/// A Secret's `data` map as the raw base64 strings, which is exactly what the
/// API server stores. Copying a Secret means copying this map unchanged: a
/// decode/encode round-trip would corrupt a binary value that is not valid
/// UTF-8, and a verification over the same lossy decode could not notice.
pub fn raw_secret_data(
    secret: Option<&serde_json::Value>,
) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    let Some(secret) = secret else {
        return out;
    };
    if let Some(data) = secret.get("data").and_then(|v| v.as_object()) {
        for (key, value) in data {
            if let Some(encoded) = value.as_str() {
                out.insert(key.clone(), encoded.to_string());
            }
        }
    }
    if let Some(data) = secret.get("stringData").and_then(|v| v.as_object()) {
        for (key, value) in data {
            if let Some(text) = value.as_str() {
                out.insert(key.clone(), base64_encode(text.as_bytes()));
            }
        }
    }
    out
}

/// Merge a write into an existing Secret payload: `set` wins, `remove` deletes,
/// every unrelated key is preserved. Pure, so the merge rule is unit-tested.
pub fn merge_secret_values(
    existing: Option<&serde_json::Value>,
    set: &HashMap<String, String>,
    remove: &[String],
) -> HashMap<String, String> {
    let mut merged = secret_values(existing);
    for (key, value) in set {
        merged.insert(key.clone(), value.clone());
    }
    for key in remove {
        merged.remove(key);
    }
    merged
}

/// The Secret object Naslos applies for an entry's credentials. The labels are
/// part of the object so the same write marks the Secret as Naslos-managed and
/// names its entry (which is what the migration and delete guards check).
fn credential_secret_object(
    name: &str,
    namespace: &str,
    entry_id: &str,
    data: &HashMap<String, String>,
) -> serde_json::Value {
    let mut object = plain_secret_object(name, namespace, data);
    object["metadata"]["labels"] = serde_json::json!({
        "app.kubernetes.io/managed-by": "naslos-api",
        "naslos.io/ddns-entry": entry_id,
    });
    object
}

/// The same object without the Naslos labels, for an operator-owned Secret: it
/// must never be marked Naslos-created, or deleting the entry would remove it.
fn plain_secret_object(
    name: &str,
    namespace: &str,
    data: &HashMap<String, String>,
) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": { "name": name, "namespace": namespace },
        "type": "Opaque",
        "stringData": data,
    })
}

/// A Secret object carrying an already-base64 `data` map, used by the migration
/// so a value is never decoded and re-encoded.
fn raw_secret_object(
    name: &str,
    namespace: &str,
    entry_id: Option<&str>,
    data: &std::collections::BTreeMap<String, String>,
) -> serde_json::Value {
    let mut object = serde_json::json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": { "name": name, "namespace": namespace },
        "type": "Opaque",
        "data": data,
    });
    if let Some(entry_id) = entry_id {
        object["metadata"]["labels"] = serde_json::json!({
            "app.kubernetes.io/managed-by": "naslos-api",
            "naslos.io/ddns-entry": entry_id,
        });
    }
    object
}

/// Verify a copied Secret holds every key/value of the source. Compared on the
/// raw base64 `data` map, so it detects a rewritten or missing value exactly.
pub fn verify_secret_copy(
    expected: &std::collections::BTreeMap<String, String>,
    target: &serde_json::Value,
) -> Result<(), String> {
    let actual = raw_secret_data(Some(target));
    for (key, value) in expected {
        if actual.get(key) != Some(value) {
            return Err(format!(
                "copy verification failed: key {key:?} is missing or differs"
            ));
        }
    }
    Ok(())
}

/// Whether a keys-namespace Secret covers every key an apps-namespace copy
/// holds, which is the only safe condition for retiring that copy.
///
/// A missing key means the keys copy never received that field (an entry saved
/// while the migration was failing), so the apps copy is still the only copy of
/// it. A *differing* value does not: the keys copy is then newer, because every
/// write after the upgrade targets it and nothing writes the apps copy again.
/// Requiring equal values would therefore strand a rotated credential in the
/// apps namespace forever.
pub fn keys_copy_covers_source(
    source: &serde_json::Value,
    target: &serde_json::Value,
) -> Result<(), String> {
    let source = raw_secret_data(Some(source));
    let target = raw_secret_data(Some(target));
    for key in source.keys() {
        if !target.contains_key(key) {
            return Err(format!(
                "copy verification failed: key {key:?} is missing from the keys-namespace Secret"
            ));
        }
    }
    Ok(())
}

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
    /// The Secret name Naslos generates for this entry. Kept in sync with
    /// [`generated_secret_name`].
    pub fn secret_name(&self) -> String {
        generated_secret_name(&self.id)
    }
}

/// A random DNS-1123-safe entry id.
pub fn new_id() -> String {
    let mut buf = [0u8; 8];
    if getrandom::fill(&mut buf).is_err() {
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
    /// Whether the persisted entry list was read successfully. A missing or
    /// empty file is a valid empty store (fresh install); an unparsable file
    /// fails closed so the next mutation cannot overwrite the only copy.
    loaded: std::sync::atomic::AtomicBool,
    /// Why the store failed to load, with recovery guidance. Present exactly
    /// when `loaded` is false.
    load_error: Mutex<Option<String>>,
}

impl Store {
    pub fn new(path: &str) -> Self {
        Self {
            path: path.to_string(),
            entries: Mutex::new(HashMap::new()),
            // An empty path is an in-memory test store, not a failed load.
            loaded: std::sync::atomic::AtomicBool::new(path.is_empty()),
            load_error: Mutex::new(None),
        }
    }

    /// Whether the persisted entry list was read successfully.
    pub fn loaded(&self) -> bool {
        self.loaded.load(std::sync::atomic::Ordering::SeqCst)
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
                .unwrap_or_else(|| format!("ddns store {} is unavailable", self.path)))
        }
    }

    pub fn load(&self) -> Result<(), String> {
        self.loaded
            .store(false, std::sync::atomic::Ordering::SeqCst);
        *self.load_error.lock().unwrap() = None;
        if self.path.is_empty() {
            self.loaded.store(true, std::sync::atomic::Ordering::SeqCst);
            return Ok(());
        }
        let data = match std::fs::read(&self.path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.loaded.store(true, std::sync::atomic::Ordering::SeqCst);
                return Ok(());
            }
            Err(e) => {
                let message = format!(
                    "ddns store {} could not be read: {e}; refusing to overwrite it",
                    self.path
                );
                *self.load_error.lock().unwrap() = Some(message.clone());
                return Err(message);
            }
        };
        let list: Vec<Entry> = match serde_json::from_slice(&data) {
            Ok(list) => list,
            Err(e) => {
                let backup = match crate::shares::backup_corrupt_store(&self.path, &data) {
                    Ok(path) => format!(" (backup saved to {})", path.display()),
                    Err(backup_err) => format!(" ({backup_err})"),
                };
                let message = format!(
                    "ddns store {} is corrupt: {e}{backup}; repair it or replace it with a valid store before changing entries",
                    self.path
                );
                *self.load_error.lock().unwrap() = Some(message.clone());
                return Err(message);
            }
        };
        let mut guard = self.entries.lock().unwrap();
        guard.clear();
        for e in list {
            if !e.id.is_empty() {
                guard.insert(e.id.clone(), e);
            }
        }
        self.loaded.store(true, std::sync::atomic::Ordering::SeqCst);
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
        self.require_loaded()?;
        let mut guard = self.entries.lock().unwrap();
        guard.insert(e.id.clone(), e);
        self.save_locked(&guard)
    }

    pub fn delete(&self, id: &str) -> Result<(), String> {
        self.require_loaded()?;
        let mut guard = self.entries.lock().unwrap();
        if guard.remove(id).is_none() {
            return Err(format!("ddns entry {id:?} not found"));
        }
        self.save_locked(&guard)
    }

    pub fn update(&self, id: &str, mutate: impl FnOnce(&mut Entry)) -> Result<Entry, String> {
        self.require_loaded()?;
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
        self.require_loaded()?;
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
        .redirect(reqwest::redirect::Policy::none())
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
    let mut chars = s.chars();
    let head: String = chars.by_ref().take(200).collect();
    if chars.next().is_some() {
        format!("{head}...")
    } else {
        head
    }
}

/// Fetch a public IP from a plain-text IP source URL and validate it.
pub async fn detect_ip(client: &reqwest::Client, source: &str) -> Result<String, String> {
    let source = source.trim();
    if source.is_empty() {
        return Err("no public-IP source is configured".to_string());
    }
    let mut resp = client
        .get(source)
        .send()
        .await
        .map_err(|_| "detecting the public IP failed".to_string())?;
    if !resp.status().is_success() {
        return Err(format!(
            "IP source {source} returned HTTP {}",
            resp.status().as_u16()
        ));
    }
    // Cap the response with a chunked read before decoding and never let a
    // source advertise a loopback/private/ULA address as the node's public IP.
    const MAX_IP_BODY: usize = 4096;
    let mut bytes = Vec::new();
    while bytes.len() < MAX_IP_BODY {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                let take = (MAX_IP_BODY - bytes.len()).min(chunk.len());
                bytes.extend_from_slice(&chunk[..take]);
            }
            Ok(None) | Err(_) => break,
        }
    }
    let body = String::from_utf8_lossy(&bytes);
    let ip: IpAddr = body.trim().parse().map_err(|_| {
        format!(
            "IP source {source} returned {:?}, not an IP",
            truncate(&body)
        )
    })?;
    if drivers::blocked_ip(&ip) {
        return Err(format!(
            "IP source {source} returned the non-public address {ip}"
        ));
    }
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
    use hickory_resolver::config::{NameServerConfig, ResolverConfig};
    use hickory_resolver::net::runtime::TokioRuntimeProvider;
    use hickory_resolver::TokioResolver;

    let (server, host, use_txt) = match name.trim() {
        "opendns" => ("208.67.222.222", "myip.opendns.com", false),
        "google" => ("8.8.8.8", "o-o.myaddr.l.google.com", true),
        other => return Err(format!("unknown DNS source {other:?}")),
    };
    let ip: IpAddr = server.parse().map_err(|_| "bad DNS server".to_string())?;
    let resolver = TokioResolver::builder_with_config(
        ResolverConfig::from_name_servers(vec![NameServerConfig::udp_and_tcp(ip)]),
        TokioRuntimeProvider::default(),
    )
    .build()
    .map_err(|e| format!("dns:{name}: {e}"))?;
    if use_txt {
        let txts = resolver
            .txt_lookup(host)
            .await
            .map_err(|e| format!("dns:{name}: {e}"))?;
        for record in txts.answers() {
            let value = record.data.to_string().trim_matches('"').trim().to_string();
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
    /// Where DDNS credential Secrets live (the key-custody namespace).
    pub keys_namespace: String,
    /// The apps namespace older releases wrote DDNS Secrets to. Read only by
    /// the startup migration, never by the reconcile path.
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
    store: Arc<Store>,
    keys_namespace: String,
    apps_namespace: String,
    kube: Option<Arc<crate::kube::Client>>,
    ip_sources: Vec<String>,
    ipv6_sources: Vec<String>,
    interval: Duration,
    cooldown: Duration,
    client: reqwest::Client,
    resolver: Arc<dyn Resolver>,
    /// The one-shot upgrade migration out of the apps namespace. It runs once
    /// per process, before the first credential read, and `migrate_lock` makes
    /// concurrent first callers wait instead of reading a half-moved Secret.
    migrated: AtomicBool,
    migrate_attempts: std::sync::atomic::AtomicU32,
    migrate_lock: tokio::sync::Mutex<()>,
}

impl Manager {
    pub fn new(opts: Options) -> Self {
        let m = Self {
            registry: opts.registry,
            store: Arc::new(Store::new(&opts.store_path)),
            keys_namespace: opts.keys_namespace,
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
            migrated: AtomicBool::new(false),
            migrate_attempts: std::sync::atomic::AtomicU32::new(0),
            migrate_lock: tokio::sync::Mutex::new(()),
        };
        if let Err(e) = m.store.load() {
            tracing::warn!("could not load the DDNS entries: {e}");
        }
        m
    }

    /// The namespace DDNS credential Secrets are read from and written to.
    pub fn keys_namespace(&self) -> &str {
        &self.keys_namespace
    }

    /// Why the persisted entry store failed to load, if it did. Handlers
    /// surface this instead of presenting a corrupt store as an empty one.
    pub fn load_error(&self) -> Option<String> {
        self.store.load_error()
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// The shared store, for consumers built beside the manager (app DNS).
    pub fn store_arc(&self) -> Arc<Store> {
        self.store.clone()
    }

    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// Run the reconcile loop immediately, then on each tick.
    pub async fn run_loop(self: Arc<Self>) {
        self.migrate_credentials().await;
        self.reconcile(false).await;
        let mut ticker = tokio::time::interval(self.interval);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            self.reconcile(false).await;
        }
    }

    /// Run the one-shot credential migration unless it already ran. The first
    /// credential read calls this too, so a manager built outside the loop
    /// (tests, the import path) still upgrades an existing install. A failure
    /// leaves the flag unset so a later use retries; success is recorded so the
    /// steady state costs no extra Kubernetes calls.
    pub async fn migrate_credentials(&self) {
        if self.migrated.load(Ordering::SeqCst) {
            return;
        }
        let _guard = self.migrate_lock.lock().await;
        if self.migrated.load(Ordering::SeqCst) {
            return;
        }
        let Some(kube) = &self.kube else {
            return;
        };
        if self.keys_namespace == self.apps_namespace {
            self.migrated.store(true, Ordering::SeqCst);
            return;
        }
        let entries = self.store.list();
        if entries.is_empty() {
            // Leave the flag unset: an entry imported later must still migrate.
            return;
        }
        let mut failed = false;
        for entry in entries {
            if entry.credentials_secret.is_empty() {
                continue;
            }
            if let Err(e) = self.migrate_entry(kube, &entry).await {
                failed = true;
                tracing::warn!(
                    "could not migrate the DDNS credential Secret {} out of {}: {e}",
                    entry.credentials_secret,
                    self.apps_namespace
                );
            }
        }
        if !failed {
            self.migrated.store(true, Ordering::SeqCst);
        } else if self
            .migrate_attempts
            .fetch_add(1, Ordering::SeqCst)
            .saturating_add(1)
            >= MIGRATE_MAX_ATTEMPTS
        {
            // Stop retrying on a persistent error (for example a Secret the API
            // may not read); the reconcile loop records the failure per run.
            tracing::warn!(
                "giving up on the DDNS credential migration after {} failed attempts; \
                 fix the Secret and restart",
                MIGRATE_MAX_ATTEMPTS
            );
            self.migrated.store(true, Ordering::SeqCst);
        }
    }

    /// Move one entry's Secret into the keys namespace. Idempotent: the plan is
    /// derived from what actually exists, and a keys-namespace copy always
    /// wins. Values are never logged.
    async fn migrate_entry(&self, kube: &crate::kube::Client, entry: &Entry) -> Result<(), String> {
        let name = &entry.credentials_secret;
        let keys_copy = kube.get_json("secret", name, &self.keys_namespace).await?;
        let apps_copy = kube.get_json("secret", name, &self.apps_namespace).await?;
        let apps_managed = apps_copy.as_ref().is_some_and(is_managed_secret);
        match plan_credential_migration(
            &entry.id,
            name,
            keys_copy.is_some(),
            apps_copy.is_some(),
            apps_managed,
        ) {
            MigrationStep::None => Ok(()),
            MigrationStep::DeleteSource => {
                // Never delete the last complete copy: an interrupted earlier
                // run, or an upsert that wrote only the submitted fields into an
                // empty keys namespace, can leave a keys Secret that exists but
                // is missing values the apps copy still holds. Only remove the
                // apps copy when the keys copy supersedes it key for key.
                let source = apps_copy.ok_or_else(|| "source Secret disappeared".to_string())?;
                let target = keys_copy.ok_or_else(|| "keys copy disappeared".to_string())?;
                // A differing value means the keys copy is newer (every write
                // after the upgrade targets it), so only a missing key blocks
                // the delete. The write path cannot read the apps copy, so the
                // operator must supply the missing field; saving the entry then
                // adds it to the keys copy and the next start finishes the move.
                keys_copy_covers_source(&source, &target).map_err(|e| {
                    format!(
                        "keeping the apps-namespace copy: the keys-namespace Secret is missing \
                         a credential field ({e}); re-enter it on the entry to finish the move"
                    )
                })?;
                kube.delete("secret", name, &self.apps_namespace).await?;
                tracing::info!(
                    "removed the leftover apps-namespace DDNS credential Secret {name} for entry {}",
                    entry.id
                );
                Ok(())
            }
            MigrationStep::Copy { delete_source } => {
                let source = apps_copy.ok_or_else(|| "source Secret disappeared".to_string())?;
                // Copy the raw base64 `data` map: decoding and re-encoding
                // would rewrite a value that is not valid UTF-8.
                let data = raw_secret_data(Some(&source));
                if data.is_empty() {
                    return Err("source Secret holds no data".to_string());
                }
                // Only a Naslos-created Secret gets the managed labels; an
                // operator Secret is copied unlabelled so deleting the entry
                // never removes it.
                let labels = delete_source.then_some(entry.id.as_str());
                let object = raw_secret_object(name, &self.keys_namespace, labels, &data);
                let created = kube.create(&object).await;
                let target = match created {
                    Ok(()) => kube
                        .get_json("secret", name, &self.keys_namespace)
                        .await?
                        .ok_or_else(|| "the copy is missing after the write".to_string())?,
                    // A concurrent writer (another API replica, an operator)
                    // may have created it first; a keys copy always wins, so
                    // verify what is there instead of failing the migration.
                    Err(e) => kube
                        .get_json("secret", name, &self.keys_namespace)
                        .await?
                        .ok_or_else(|| format!("copying to {}: {e}", self.keys_namespace))?,
                };
                verify_secret_copy(&data, &target)?;
                if delete_source {
                    kube.delete("secret", name, &self.apps_namespace).await?;
                }
                tracing::info!(
                    "moved the DDNS credential Secret {name} for entry {} into {} (verified)",
                    entry.id,
                    self.keys_namespace
                );
                Ok(())
            }
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
        self.migrate_credentials().await;
        let secret = kube
            .get_json("secret", &e.credentials_secret, &self.keys_namespace)
            .await?
            .ok_or_else(|| {
                format!(
                    "reading Secret {}/{}: not found",
                    self.keys_namespace, e.credentials_secret
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

/// Write a credential Secret into the keys namespace, merging over existing
/// keys. A Naslos-generated name is stamped with the managed labels; an
/// operator-supplied Secret keeps its own ownership metadata, so it is never
/// treated as Naslos-created (and so never deleted with the entry).
pub async fn write_secret(
    kube: &crate::kube::Client,
    namespace: &str,
    entry_id: &str,
    name: &str,
    data: &HashMap<String, String>,
) -> Result<(), String> {
    if name.is_empty() {
        return Err("no Secret name".to_string());
    }
    let existing = kube.get_json("secret", name, namespace).await?;
    let merged = merge_secret_values(existing.as_ref(), data, &[]);
    if name != generated_secret_name(entry_id) {
        return kube.write_secret(namespace, name, &merged).await;
    }
    kube.apply(&credential_secret_object(
        name, namespace, entry_id, &merged,
    ))
    .await
}

/// Delete specific keys from a credential Secret. Keys that are not present
/// are ignored; an empty Secret is left in place. The managed labels are
/// re-applied, so a write never drops them.
pub async fn remove_secret_keys(
    kube: &crate::kube::Client,
    namespace: &str,
    name: &str,
    keys: &[String],
) -> Result<(), String> {
    if name.is_empty() || keys.is_empty() {
        return Ok(());
    }
    let Some(existing) = kube.get_json("secret", name, namespace).await? else {
        return Ok(());
    };
    let merged = merge_secret_values(Some(&existing), &HashMap::new(), keys);
    let entry_id = existing
        .get("metadata")
        .and_then(|m| m.get("labels"))
        .and_then(|labels| labels.get(DDNS_ENTRY_LABEL))
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    if entry_id.is_empty() {
        return kube.write_secret(namespace, name, &merged).await;
    }
    kube.apply(&credential_secret_object(
        name, namespace, &entry_id, &merged,
    ))
    .await
}

/// Delete a credential Secret Naslos created, ignoring a missing one. A Secret
/// that is neither labelled `managed-by=naslos-api` nor named by the generated
/// `naslos-ddns-<id>` pattern is an operator Secret and is left alone.
pub async fn delete_secret(
    kube: &crate::kube::Client,
    namespace: &str,
    entry_id: &str,
    name: &str,
) -> Result<(), String> {
    if name.is_empty() {
        return Ok(());
    }
    match kube.get_json("secret", name, namespace).await? {
        None => Ok(()),
        Some(secret) if is_managed_secret(&secret) || name == generated_secret_name(entry_id) => {
            kube.delete("secret", name, namespace).await
        }
        Some(_) => {
            tracing::warn!(
                "not deleting DDNS credential Secret {name} in {namespace}: it is not one Naslos created"
            );
            Ok(())
        }
    }
}

/// Delete both copies of an entry's Secret (keys namespace, and a leftover
/// apps-namespace copy from before the migration). Only Naslos-created Secrets
/// are removed.
pub async fn delete_entry_secrets(
    kube: &crate::kube::Client,
    keys_namespace: &str,
    apps_namespace: &str,
    entry_id: &str,
    name: &str,
) -> Result<(), String> {
    // Attempt both deletes even when the first fails: the entry is already gone
    // from the store, so nothing retries and a short-circuit would strand the
    // pre-upgrade plaintext copy in the apps namespace.
    let keys = delete_secret(kube, keys_namespace, entry_id, name).await;
    let apps = if apps_namespace != keys_namespace {
        delete_secret(kube, apps_namespace, entry_id, name).await
    } else {
        Ok(())
    };
    match (keys, apps) {
        (Err(e), Err(other)) => Err(format!(
            "deleting {keys_namespace}: {e}; deleting {apps_namespace}: {other}"
        )),
        (Err(e), _) => Err(format!("deleting {keys_namespace}: {e}")),
        (_, Err(e)) => Err(format!("deleting {apps_namespace}: {e}")),
        _ => Ok(()),
    }
}

fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .map_err(|e| e.to_string())
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_respects_char_boundaries() {
        let body = "é".repeat(300);
        let out = truncate(&body);
        assert!(out.ends_with("..."));
        assert_eq!(out.chars().filter(|c| *c == 'é').count(), 200);
        assert_eq!(truncate("short"), "short");
    }

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
        store.load().unwrap();
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
    fn a_corrupt_store_is_backed_up_and_mutations_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ddns.json");
        std::fs::write(&path, "{not json").unwrap();
        let store = Store::new(&path.display().to_string());
        let error = store.load().unwrap_err();
        assert!(error.contains("corrupt"), "{error}");
        assert!(error.contains(".corrupt-"), "{error}");
        assert!(!store.loaded());
        assert!(store.load_error().is_some());

        assert!(store
            .put(Entry {
                id: "abc".into(),
                provider: "cloudflare".into(),
                ..Default::default()
            })
            .is_err());
        assert!(store.delete("abc").is_err());
        assert!(store
            .update("abc", |e| e.provider = "other".into())
            .is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{not json");
        assert!(store.list().is_empty());

        let backups = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().contains(".corrupt-"))
            .count();
        assert_eq!(backups, 1, "one corrupt backup");
    }

    #[test]
    fn sanitize_error_redacts_secrets() {
        let mut secret = HashMap::new();
        secret.insert("token".to_string(), "supersecret".to_string());
        let out = sanitize_error("failed with supersecret in url", &secret);
        assert!(!out.contains("supersecret"));
        assert!(out.contains("***"));
    }

    fn secret_with(data: &[(&str, &str)], managed: bool) -> serde_json::Value {
        use base64::Engine;
        let data: serde_json::Map<String, serde_json::Value> = data
            .iter()
            .map(|(k, v)| {
                (
                    (*k).to_string(),
                    serde_json::json!(base64::engine::general_purpose::STANDARD.encode(v)),
                )
            })
            .collect();
        let mut object = serde_json::json!({
            "apiVersion": "v1",
            "kind": "Secret",
            "metadata": { "name": "naslos-ddns-abc", "namespace": "naslos-keys" },
            "data": data,
        });
        if managed {
            object["metadata"]["labels"] = serde_json::json!({
                "app.kubernetes.io/managed-by": "naslos-api",
                "naslos.io/ddns-entry": "abc",
            });
        }
        object
    }

    #[test]
    fn migration_copies_an_apps_secret_without_a_keys_copy() {
        let step = plan_credential_migration("abc", "naslos-ddns-abc", false, true, false);
        assert_eq!(
            step,
            MigrationStep::Copy {
                delete_source: true
            }
        );
    }

    #[test]
    fn migration_keeps_an_operator_secret_in_the_apps_namespace() {
        let step = plan_credential_migration("abc", "operator-creds", false, true, false);
        assert_eq!(
            step,
            MigrationStep::Copy {
                delete_source: false
            }
        );
    }

    #[test]
    fn migration_deletes_a_managed_apps_leftover_when_the_keys_copy_exists() {
        assert_eq!(
            plan_credential_migration("abc", "naslos-ddns-abc", true, true, false),
            MigrationStep::DeleteSource
        );
        assert_eq!(
            plan_credential_migration("abc", "custom-name", true, true, true),
            MigrationStep::DeleteSource
        );
    }

    #[test]
    fn migration_never_touches_an_operator_secret_beside_a_keys_copy() {
        assert_eq!(
            plan_credential_migration("abc", "operator-creds", true, true, false),
            MigrationStep::None
        );
    }

    #[test]
    fn migration_is_a_no_op_when_nothing_was_written_or_already_moved() {
        assert_eq!(
            plan_credential_migration("abc", "naslos-ddns-abc", true, false, false),
            MigrationStep::None
        );
        assert_eq!(
            plan_credential_migration("abc", "naslos-ddns-abc", false, false, false),
            MigrationStep::None
        );
        assert_eq!(
            plan_credential_migration("abc", "", false, true, true),
            MigrationStep::None
        );
    }

    #[test]
    fn migration_is_idempotent_after_a_verified_copy() {
        // First pass copies and deletes; the second sees only the keys copy.
        let first = plan_credential_migration("abc", "naslos-ddns-abc", false, true, true);
        assert_eq!(
            first,
            MigrationStep::Copy {
                delete_source: true
            }
        );
        let second = plan_credential_migration("abc", "naslos-ddns-abc", true, false, false);
        assert_eq!(second, MigrationStep::None);
    }

    #[test]
    fn merge_preserves_unrelated_keys_and_applies_set_and_remove() {
        let existing = secret_with(&[("token", "old"), ("extra", "keep")], true);
        let set = HashMap::from([("token".to_string(), "new".to_string())]);
        let merged = merge_secret_values(Some(&existing), &set, &["extra".to_string()]);
        assert_eq!(merged.get("token").map(String::as_str), Some("new"));
        assert!(!merged.contains_key("extra"));

        let only_set = merge_secret_values(Some(&existing), &set, &[]);
        assert_eq!(only_set.get("extra").map(String::as_str), Some("keep"));
        assert_eq!(only_set.get("token").map(String::as_str), Some("new"));

        let created = merge_secret_values(None, &set, &[]);
        assert_eq!(created.get("token").map(String::as_str), Some("new"));
        assert_eq!(created.len(), 1);
    }

    #[test]
    fn verify_accepts_an_exact_copy_and_rejects_a_diff_or_missing_key() {
        let source = secret_with(&[("username", "u"), ("password", "p")], true);
        let expected = raw_secret_data(Some(&source));
        assert!(verify_secret_copy(&expected, &source).is_ok());

        let missing = secret_with(&[("username", "u")], true);
        let error = verify_secret_copy(&expected, &missing).unwrap_err();
        assert!(error.contains("password"), "{error}");
        assert!(!error.contains("p\""), "the value must not leak: {error}");

        let different = secret_with(&[("username", "u"), ("password", "other")], true);
        assert!(verify_secret_copy(&expected, &different).is_err());
    }

    #[test]
    fn a_partial_keys_copy_does_not_supersede_a_complete_apps_copy() {
        // The regression the migration guards: an upsert wrote only the
        // submitted field into an empty keys namespace, so the apps copy still
        // holds a field the keys copy lacks. Deleting it would lose that field.
        let apps = secret_with(&[("username", "u"), ("password", "p")], true);
        let partial = secret_with(&[("username", "u")], true);
        let error = keys_copy_covers_source(&apps, &partial).unwrap_err();
        assert!(error.contains("password"), "{error}");

        // The reverse is fine: the apps copy needs no extra keys.
        let full = secret_with(&[("username", "u")], true);
        let superset = secret_with(&[("username", "u"), ("password", "p")], true);
        assert!(keys_copy_covers_source(&full, &superset).is_ok());
    }

    #[test]
    fn a_rotated_value_still_counts_as_covered() {
        // A differing value means the keys copy is newer: every write after the
        // upgrade targets it, so requiring equality would strand the rotated
        // plaintext copy in the apps namespace forever.
        let apps = secret_with(&[("username", "u"), ("password", "old")], true);
        let newer = secret_with(&[("username", "u"), ("password", "new")], true);
        assert!(keys_copy_covers_source(&apps, &newer).is_ok());
        assert!(verify_secret_copy(&raw_secret_data(Some(&apps)), &newer).is_err());
    }

    #[test]
    fn a_copy_preserves_non_utf8_bytes_exactly() {
        // 0xff is not valid UTF-8; a lossy round-trip would rewrite it.
        use base64::Engine;
        let binary = base64::engine::general_purpose::STANDARD.encode([0xffu8, 0x00, 0xfe]);
        let source = serde_json::json!({
            "metadata": { "labels": {} },
            "data": { "blob": binary.clone() },
        });
        let copied = raw_secret_object(
            "naslos-ddns-abc",
            "naslos-keys",
            Some("abc"),
            &raw_secret_data(Some(&source)),
        );
        assert_eq!(copied["data"]["blob"], binary);
        assert!(verify_secret_copy(&raw_secret_data(Some(&source)), &copied).is_ok());
    }

    #[test]
    fn raw_data_folds_string_data_the_way_the_api_server_does() {
        let plain = serde_json::json!({
            "metadata": {},
            "stringData": { "username": "home" },
        });
        let raw = raw_secret_data(Some(&plain));
        assert_eq!(
            raw.get("username").map(String::as_str),
            Some(base64_encode(b"home").as_str())
        );
        assert_eq!(
            secret_values(Some(&plain))
                .get("username")
                .map(String::as_str),
            Some("home")
        );
    }

    #[test]
    fn only_the_managed_label_counts_as_naslos_created() {
        assert!(is_managed_secret(&secret_with(&[("k", "v")], true)));
        assert!(!is_managed_secret(&secret_with(&[("k", "v")], false)));
        assert!(!is_managed_secret(&serde_json::json!({})));
    }

    #[test]
    fn ip_family_matching() {
        assert!(ip_matches_family("1.2.3.4", false));
        assert!(!ip_matches_family("1.2.3.4", true));
        assert!(ip_matches_family("2001:db8::1", true));
    }
}
