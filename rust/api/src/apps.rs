//! Installed-app records and lifecycle orchestration (port of
//! `api/internal/apps`).
//!
//! Records are persisted as JSON beside the other state files and carry the
//! exposure settings the routing layer renders from.

use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::app_env::{self, EnvMeta, EnvVar};
use crate::catalog::Catalog;
use crate::chartsrepo::is_dns1123_label;
use crate::helm;
use crate::routing::{
    internet_policy_name, validate_protected_paths, APP_LABEL, MANAGED_BY_LABEL, MANAGED_BY_VALUE,
};

const RELEASE_NAME_TEMPLATE: &str = "{{ .Release.Name }}";

/// A record whose dataset mounts use the platform-chosen `/datasets/<name>`
/// layout carries this marker so the startup migration runs once.
pub const DATASETS_LAYOUT_V2: &str = "v2";

/// The Helm keys the API owns. They are stripped from client-supplied values
/// and re-injected from the reconciled dataset selection.
pub const PLATFORM_DATASET_KEYS: [&str; 3] = ["datasets", "datasetsClaim", "datasetsClaims"];

/// Platform-owned Helm values that are never accepted from a client.
pub const PLATFORM_ENV_KEYS: [&str; 1] = [app_env::CHECKSUM_VALUE];

/// Resolve the release-name template in a declared service.
pub fn render_service_name(name: &str, release: &str) -> String {
    name.trim()
        .replace(RELEASE_NAME_TEMPLATE, release)
        .trim()
        .to_string()
}

/// The exposure configuration of an installed app.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Exposure {
    #[serde(default)]
    pub subdomain: String,
    #[serde(default)]
    pub tls: bool,
    #[serde(default)]
    pub auth: bool,
    #[serde(default, rename = "localOnly")]
    pub local_only: bool,
    /// Path prefixes that require Authelia even when `auth` is false. The app
    /// itself stays public; only the listed paths get forwardAuth.
    #[serde(
        default,
        rename = "protectedPaths",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub protected_paths: Vec<String>,
    /// Outbound Internet access (default on). Off keeps the app on the
    /// namespace's DNS + app-to-app-only egress policy.
    #[serde(default = "default_true")]
    pub internet: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub service: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub port: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub scheme: String,
}

fn default_true() -> bool {
    true
}

impl Default for Exposure {
    fn default() -> Self {
        Self {
            subdomain: String::new(),
            tls: false,
            auth: false,
            local_only: false,
            protected_paths: Vec::new(),
            internet: true,
            service: String::new(),
            port: 0,
            scheme: String::new(),
        }
    }
}

fn is_zero(n: &i64) -> bool {
    *n == 0
}

/// The CNAME this platform maintains for an exposed app (FR-DNS-09).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct DnsState {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fqdn: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub target: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provider: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub zone: String,
    #[serde(rename = "recordId", default, skip_serializing_if = "String::is_empty")]
    pub record_id: String,
    /// Whether this platform created (or adopted) the record.
    #[serde(default)]
    pub owned: bool,
    /// `ok` or `error`. Errors never fail the app operation.
    #[serde(default)]
    pub status: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
    #[serde(rename = "updatedAt", default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
    /// The base domain the record lives under (needed to delete an old record
    /// after the app moved domains).
    #[serde(
        rename = "baseDomain",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub base_domain: String,
}

/// A partial exposure update. Omitted fields keep their stored value instead
/// of defaulting to `false`, which used to silently disable TLS/auth on a
/// partial PUT (SEC-2026-D11).
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct ExposurePatch {
    #[serde(default)]
    pub subdomain: Option<String>,
    #[serde(default)]
    pub tls: Option<bool>,
    #[serde(default)]
    pub auth: Option<bool>,
    #[serde(default, rename = "localOnly")]
    pub local_only: Option<bool>,
    #[serde(default, rename = "protectedPaths")]
    pub protected_paths: Option<Vec<String>>,
    #[serde(default)]
    pub internet: Option<bool>,
    #[serde(default)]
    pub service: Option<String>,
    #[serde(default)]
    pub port: Option<i64>,
    #[serde(default)]
    pub scheme: Option<String>,
}

impl ExposurePatch {
    pub fn apply(&self, current: &Exposure) -> Exposure {
        let mut merged = current.clone();
        if let Some(v) = &self.subdomain {
            merged.subdomain = v.clone();
        }
        if let Some(v) = self.tls {
            merged.tls = v;
        }
        if let Some(v) = self.auth {
            merged.auth = v;
        }
        if let Some(v) = self.local_only {
            merged.local_only = v;
        }
        if let Some(v) = &self.protected_paths {
            merged.protected_paths = v.clone();
        }
        if let Some(v) = self.internet {
            merged.internet = v;
        }
        if let Some(v) = &self.service {
            merged.service = v.clone();
        }
        if let Some(v) = self.port {
            if v != 0 {
                merged.port = v;
            }
        }
        if let Some(v) = &self.scheme {
            merged.scheme = v.clone();
        }
        merged
    }
}

/// A persisted installed app.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Record {
    pub name: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub channel: String,
    #[serde(default, rename = "chartPath")]
    pub chart_path: String,
    #[serde(default, rename = "chartVersion")]
    pub chart_version: String,
    #[serde(default)]
    pub values: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub exposure: Exposure,
    #[serde(
        default,
        rename = "baseDomain",
        skip_serializing_if = "String::is_empty"
    )]
    pub base_domain: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub namespace: String,
    /// Registry datasets attached to this app (D3/F1).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub datasets: Vec<String>,
    /// Marks a record migrated to the `/datasets/<name>` mount layout. Empty
    /// for records that predate the layout change or carry no datasets.
    #[serde(
        default,
        rename = "datasetsLayout",
        skip_serializing_if = "String::is_empty"
    )]
    pub datasets_layout: String,
    #[serde(rename = "createdAt")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(rename = "updatedAt")]
    pub updated_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub orphaned: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub privileged: bool,
    #[serde(
        default,
        rename = "lastError",
        skip_serializing_if = "String::is_empty"
    )]
    pub last_error: String,
    /// Names (and secret flags) of the app's custom environment variables. The
    /// values live only in the `naslos-env-<release>` Secret.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<EnvMeta>,
    /// The CNAME the platform maintains for the app's exposed hostname.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dns: Option<DnsState>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// A record enriched with live release status for API responses.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct View {
    #[serde(flatten)]
    pub record: Record,
    pub namespace: String,
    pub status: String,
    pub chart: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub url: String,
    /// Values masked on read because the catalog marks them as secrets. A
    /// client echoes them back as `null` to keep the stored value.
    #[serde(
        default,
        rename = "redactedKeys",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub redacted_keys: Vec<String>,
    /// Catalog display name, falling back to the release name.
    #[serde(rename = "displayName")]
    pub display_name: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default, rename = "iconUrl")]
    pub icon_url: String,
    /// Live Helm chart version when the release is readable, else the
    /// record's `chartVersion`; empty when neither is known.
    #[serde(rename = "installedVersion")]
    pub installed_version: String,
    #[serde(default, rename = "latestVersion")]
    pub latest_version: String,
    #[serde(default, rename = "latestSource")]
    pub latest_source: String,
    #[serde(default, rename = "latestChannel")]
    pub latest_channel: String,
    #[serde(default, rename = "updateAvailable")]
    pub update_available: bool,
}

/// One app with a newer chart in the catalog (the record-only summary used by
/// the tab badge and the sidebar dot).
#[derive(Debug, Clone, serde::Serialize)]
pub struct UpdateSummary {
    pub name: String,
    #[serde(rename = "installedVersion")]
    pub installed_version: String,
    #[serde(rename = "latestVersion")]
    pub latest_version: String,
}

/// A chart version that may carry a prerelease/build suffix. `crate::semver`
/// is strict (release tags only), but catalog charts legitimately use
/// `0.1.3-rc1`, so those are compared with semver precedence instead of plain
/// inequality.
struct LooseVersion {
    core: crate::semver::Version,
    prerelease: Option<Vec<String>>,
}

fn parse_loose_version(input: &str) -> Option<LooseVersion> {
    let input = input.trim();
    let input = input.strip_prefix('v').unwrap_or(input);
    // Build metadata never affects precedence.
    let input = input.split('+').next().unwrap_or(input);
    let (core, prerelease) = match input.split_once('-') {
        Some((core, prerelease)) if !prerelease.is_empty() => (core, Some(prerelease)),
        Some(_) => return None,
        None => (input, None),
    };
    let core = crate::semver::Version::parse(core)?;
    Some(LooseVersion {
        core,
        prerelease: prerelease.map(|pre| pre.split('.').map(str::to_string).collect()),
    })
}

fn compare_prerelease(left: &[String], right: &[String]) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    for (left, right) in left.iter().zip(right) {
        let ord = match (left.parse::<u64>().ok(), right.parse::<u64>().ok()) {
            (Some(left), Some(right)) => left.cmp(&right),
            // Numeric identifiers always have lower precedence than
            // alphanumeric ones.
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => left.cmp(right),
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    left.len().cmp(&right.len())
}

fn compare_loose(left: &LooseVersion, right: &LooseVersion) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match left.core.cmp(&right.core) {
        Ordering::Equal => match (&left.prerelease, &right.prerelease) {
            (None, None) => Ordering::Equal,
            // A release outranks any prerelease of the same core version.
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(left), Some(right)) => compare_prerelease(left, right),
        },
        other => other,
    }
}

/// Whether the catalog offers a newer version than the installed one. Semver
/// precedence (including prerelease/build forms charts actually use) is used
/// when both parse, so a downgrade is never reported as an update; otherwise
/// compare the trimmed strings with any leading `v` stripped. An empty latest
/// version never reports an update.
pub fn update_available(installed: &str, latest: &str) -> bool {
    let installed = installed.trim();
    let latest = latest.trim();
    if installed.is_empty() || latest.is_empty() {
        return false;
    }
    match (parse_loose_version(installed), parse_loose_version(latest)) {
        (Some(installed), Some(latest)) => {
            compare_loose(&latest, &installed) == std::cmp::Ordering::Greater
        }
        _ => installed.trim_start_matches('v') != latest.trim_start_matches('v'),
    }
}

/// Fill the catalog-derived and live-version fields on a view. `live_version`
/// is Helm's release version when it could be read.
fn enrich_view(
    view: &mut View,
    live_version: Option<&str>,
    catalog_app: Option<&crate::catalog::App>,
) {
    let live = live_version.map(str::trim).filter(|v| !v.is_empty());
    view.installed_version = live.unwrap_or(view.record.chart_version.trim()).to_string();
    view.display_name = catalog_app
        .map(|app| app.display_name.trim())
        .filter(|name| !name.is_empty())
        .unwrap_or(view.record.name.as_str())
        .to_string();
    view.icon = catalog_app.map(|app| app.icon.clone()).unwrap_or_default();
    view.icon_url = catalog_app
        .map(|app| app.icon_url.clone())
        .unwrap_or_default();
    view.latest_version = catalog_app
        .map(|app| app.version.trim().to_string())
        .unwrap_or_default();
    view.latest_source = catalog_app
        .map(|app| app.source.clone())
        .unwrap_or_default();
    view.latest_channel = catalog_app
        .map(|app| app.channel.clone())
        .unwrap_or_default();
    view.update_available = !view.record.orphaned
        && catalog_app.is_some()
        && update_available(&view.installed_version, &view.latest_version);
}

/// A Service rendered by an installed release.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct DiscoveredService {
    pub name: String,
    pub port: i64,
    pub scheme: String,
    #[serde(default, rename = "portName", skip_serializing_if = "String::is_empty")]
    pub port_name: String,
}

/// Returns the current catalog snapshot.
pub type CatalogProvider = Arc<dyn Fn() -> Option<Arc<Catalog>> + Send + Sync>;

/// Returns the effective SSO domain list.
pub type SsoDomainsFn = Arc<dyn Fn() -> Vec<String> + Send + Sync>;

/// Finds the Services a release rendered.
#[async_trait::async_trait]
pub trait ServiceDiscoverer: Send + Sync {
    async fn services_for_release(
        &self,
        namespace: &str,
        release: &str,
    ) -> Result<Vec<DiscoveredService>, String>;
}

/// Renders and applies the exposure layer for a record.
#[async_trait::async_trait]
pub trait Router: Send + Sync {
    async fn apply(
        &self,
        rec: &Record,
        namespace: &str,
        base_domain: &str,
        sso_domains: &[String],
    ) -> Result<(), String>;
    async fn delete(&self, namespace: &str, name: &str) -> Result<(), String>;
    /// Delete only the app's public-Internet egress policy. Install rollback
    /// uses this: the pre-created policy is the only routing object that
    /// attempt created, and `delete` would also remove a live route.
    async fn delete_internet(&self, name: &str, namespace: &str) -> Result<(), String> {
        let _ = (name, namespace);
        Ok(())
    }
    /// Grant the app's public-Internet egress policy before its release
    /// exists. Install calls this before the Helm wait so an app whose first
    /// start needs the public network (Bazarr fetches releases and
    /// announcements) can pass its readiness probe; `apply` repeats it once
    /// the install succeeds.
    async fn ensure_internet(&self, name: &str, namespace: &str) -> Result<(), String> {
        let _ = (name, namespace);
        Ok(())
    }
    /// Ensure the Authelia portal is routed on every non-primary SSO domain.
    /// The primary domain's portal is part of the platform chart.
    async fn reconcile_portals(&self, domains: &[String], primary: &str) -> Result<(), String> {
        let _ = (domains, primary);
        Ok(())
    }
}

/// Maintains the DNS record for an exposed app (FR-DNS-09).
#[async_trait::async_trait]
pub trait AppDns: Send + Sync {
    /// Converge the app's CNAME. `None` means no record is desired (absent or
    /// removed); errors are returned as an error state, never as a failure.
    async fn reconcile(&self, rec: &Record, base_domain: &str) -> Option<DnsState>;
    /// Delete the record this platform owns, if any.
    async fn remove(&self, rec: &Record) -> Result<(), String>;
}

/// A synchronous progress observer.
pub type ProgressFn = dyn Fn(&str, &str) + Send + Sync;

/// Lifecycle stages reported by the operations.
pub const STAGE_PREPARING: &str = "preparing";
pub const STAGE_INSTALLING: &str = "installing";
pub const STAGE_FINALIZING: &str = "finalizing";

/// Bounds the manager's external dependencies.
pub struct Config {
    pub store_path: String,
    pub helm: Arc<helm::Client>,
    pub helm_privileged: Option<Arc<helm::Client>>,
    pub privileged_namespace: String,
    pub charts: Arc<crate::chartsrepo::Manager>,
    pub catalog: CatalogProvider,
    pub router: Option<Arc<dyn Router>>,
    pub discoverer: Option<Arc<dyn ServiceDiscoverer>>,
    pub base_domain: String,
    pub sso_domains: Option<SsoDomainsFn>,
    /// Per-app namespace pool and dataset provisioning (D3/F1). Empty pools
    /// keep the legacy shared-namespace behaviour.
    pub namespace_pool: Vec<String>,
    pub privileged_namespace_pool: Vec<String>,
    /// Initial baseline limit when no owner setting is stored. The chart
    /// provisions the whole pool; only the first `limit` slots are handed out,
    /// and the owner can raise that up to the provisioned count from the UI.
    pub namespace_pool_limit: Option<usize>,
    /// Where the owner-set baseline limit is persisted (beside the other app
    /// state files). Empty disables persistence.
    pub pool_settings_path: String,
    pub kube: Option<Arc<crate::kube::Client>>,
    pub datasets: Option<Arc<crate::datasets::Registry>>,
    /// Image for a pool-scoped local-path provisioner, created on demand.
    pub pool_provisioner_image: String,
    /// App CNAME maintenance (FR-DNS-09); absent when DNS is unavailable.
    pub app_dns: Option<Arc<dyn AppDns>>,
}

/// What reconciling the env Secret did, so a failed Helm call can undo it.
#[derive(Default)]
struct EnvOutcome {
    /// New variable metadata; `None` keeps the record's existing list.
    meta: Option<Vec<EnvMeta>>,
    /// Previous Secret data (`None` when it did not exist).
    previous: Option<std::collections::BTreeMap<String, String>>,
    /// Whether this call created/updated (or deleted) the Secret.
    wrote: bool,
    /// A non-fatal problem worth surfacing on the app (e.g. chart capability
    /// removed, Secret deleted out of band).
    warning: String,
}

/// What reconciling the env Secret did, so a failed Helm call can undo it.
#[derive(Default)]
struct EnvOutcome {
    /// New variable metadata; `None` keeps the record's existing list.
    meta: Option<Vec<EnvMeta>>,
    /// Previous Secret data (`None` when it did not exist).
    previous: Option<std::collections::BTreeMap<String, String>>,
    /// Whether this call created/updated (or deleted) the Secret.
    wrote: bool,
    /// A non-fatal problem worth surfacing on the app (e.g. chart capability
    /// removed, Secret deleted out of band).
    warning: String,
}

/// Coordinates records and lifecycle operations.
pub struct Manager {
    store: Store,
    helm: Arc<helm::Client>,
    helm_priv: Option<Arc<helm::Client>>,
    priv_ns: String,
    charts: Arc<crate::chartsrepo::Manager>,
    catalog: CatalogProvider,
    router: Option<Arc<dyn Router>>,
    discoverer: Option<Arc<dyn ServiceDiscoverer>>,
    base_domain: String,
    sso_domains: Option<SsoDomainsFn>,
    namespace_pool: Vec<String>,
    privileged_namespace_pool: Vec<String>,
    /// Owner-set usable baseline slots (<= `namespace_pool.len()`).
    namespace_pool_limit: Mutex<usize>,
    pool_settings_path: String,
    kube: Option<Arc<crate::kube::Client>>,
    datasets: Option<Arc<crate::datasets::Registry>>,
    pool_provisioner_image: String,
    app_dns: Option<Arc<dyn AppDns>>,
    /// CPU/RAM/network cache for the Installed tab (`GET /api/apps/usage`).
    usage: crate::app_usage::UsageCollector,
    /// Namespaces reserved by installs that have not recorded a release yet.
    /// `_mu` was never taken, so two concurrent installs picked the same
    /// "isolated" namespace; reservations are the fix.
    reservations: Arc<Mutex<std::collections::HashSet<String>>>,
    /// One async lock per app. HTTP lifecycle jobs are serialised by
    /// `AppJobManager`; the startup layout migration runs outside it and takes
    /// the same lock so it cannot race a user install/upgrade/uninstall.
    app_locks: std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

/// Frees a namespace reservation when the install finishes, succeeds or not.
struct NamespaceReservation {
    reserved: Arc<Mutex<std::collections::HashSet<String>>>,
    namespace: String,
}

impl Drop for NamespaceReservation {
    fn drop(&mut self) {
        if let Ok(mut set) = self.reserved.lock() {
            set.remove(&self.namespace);
        }
    }
}

const POOL_PROVISIONER_NAMESPACE: &str = "local-path-storage";
const POOL_PROVISIONER_SA: &str = "local-path-provisioner-service-account";

/// Validate a pool name used in a StorageClass path. Pool names are restricted
/// by the agent; this is the second gate before the name becomes a filesystem
/// path and a Kubernetes object name.
fn validate_pool_for_storage(pool: &str) -> Result<(), String> {
    if pool.is_empty() || pool.len() > 63 {
        return Err("pool name must be 1..63 characters".into());
    }
    let mut chars = pool.chars();
    let first = chars.next().unwrap_or(' ');
    if !first.is_ascii_alphanumeric() {
        return Err(format!("invalid pool name {pool:?}"));
    }
    if !pool
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
    {
        return Err(format!("invalid pool name {pool:?}"));
    }
    Ok(())
}

/// The per-pool StorageClass / provisioner name. Kubernetes object names are
/// lowercase, so slug the pool and append a short hash of the exact name to
/// avoid collisions between e.g. `Data` and `data`.
pub fn pool_storage_class(pool: &str) -> String {
    let mut slug: String = pool
        .to_ascii_lowercase()
        .chars()
        .take(40)
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                c
            } else {
                '-'
            }
        })
        .collect();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    let slug = slug.trim_matches('-');
    let hash = crate::pack::hex_sha256(pool.as_bytes());
    let suffix = &hash[..8];
    if slug.is_empty() {
        format!("naslos-pool-{suffix}")
    } else {
        format!("naslos-pool-{slug}-{suffix}")
    }
}

/// Set every `persistence.<key>.storageClass` in the chart values. Charts in
/// the catalog use this shape (including nested `postgres.persistence`); the
/// managed-dataset claim uses its own `naslos-app-data` class and is untouched.
pub fn set_persistence_storage_class(value: &mut serde_json::Value, class: &str) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::Object(persistence)) = map.get_mut("persistence") {
                if persistence.contains_key("storageClass") {
                    persistence.insert(
                        "storageClass".to_string(),
                        serde_json::Value::String(class.to_string()),
                    );
                }
                for child in persistence.values_mut() {
                    if let Some(child) = child.as_object_mut() {
                        child.insert(
                            "storageClass".to_string(),
                            serde_json::Value::String(class.to_string()),
                        );
                    }
                }
            }
            for child in map.values_mut() {
                set_persistence_storage_class(child, class);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                set_persistence_storage_class(item, class);
            }
        }
        _ => {}
    }
}

fn pool_provisioner_objects(
    pool: &str,
    class: &str,
    image: &str,
) -> (serde_json::Value, serde_json::Value, serde_json::Value) {
    let path = format!("/var/mnt/{pool}/apps");
    let config = serde_json::json!({
        "nodePathMap": [{
            "node": "DEFAULT_PATH_FOR_NON_LISTED_NODES",
            "paths": [path],
        }],
    })
    .to_string();
    let setup = "#!/bin/sh\nset -eu\nmkdir -m 0777 -p \"$VOL_DIR\"\n";
    let teardown = "#!/bin/sh\nset -eu\nrm -rf \"$VOL_DIR\"\n";
    let helper = "apiVersion: v1\nkind: Pod\nmetadata:\n  name: helper-pod\nspec:\n  priorityClassName: system-node-critical\n  tolerations:\n  - key: node.kubernetes.io/disk-pressure\n    operator: Exists\n    effect: NoSchedule\n  containers:\n  - name: helper-pod\n    image: busybox\n    imagePullPolicy: IfNotPresent\n";
    let configmap = serde_json::json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": {
            "name": format!("{class}-config"),
            "namespace": POOL_PROVISIONER_NAMESPACE,
            "labels": { "naslos.local/pool": pool, "app.kubernetes.io/component": "app-pool-storage" },
        },
        "data": {
            "config.json": config,
            "setup": setup,
            "teardown": teardown,
            "helperPod.yaml": helper,
        },
    });
    let deployment = serde_json::json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": {
            "name": format!("{class}-provisioner"),
            "namespace": POOL_PROVISIONER_NAMESPACE,
            "labels": { "naslos.local/pool": pool, "app.kubernetes.io/component": "app-pool-storage" },
        },
        "spec": {
            "replicas": 1,
            "selector": { "matchLabels": { "app": format!("{class}-provisioner") } },
            "template": {
                "metadata": { "labels": { "app": format!("{class}-provisioner"), "naslos.local/pool": pool } },
                "spec": {
                    "serviceAccountName": POOL_PROVISIONER_SA,
                    "containers": [{
                        "name": "local-path-provisioner",
                        "image": image,
                        "imagePullPolicy": "IfNotPresent",
                        "command": [
                            "local-path-provisioner",
                            "--debug",
                            "start",
                            "--config",
                            "/etc/config/config.json",
                            "--provisioner-name",
                            class,
                            "--configmap-name",
                            format!("{class}-config"),
                        ],
                        "env": [{
                            "name": "POD_NAMESPACE",
                            "valueFrom": { "fieldRef": { "fieldPath": "metadata.namespace" } },
                        }],
                        "volumeMounts": [{
                            "name": "config-volume",
                            "mountPath": "/etc/config/",
                        }],
                    }],
                    "volumes": [{
                        "name": "config-volume",
                        "configMap": { "name": format!("{class}-config") },
                    }],
                },
            },
        },
    });
    let storage_class = serde_json::json!({
        "apiVersion": "storage.k8s.io/v1",
        "kind": "StorageClass",
        "metadata": {
            "name": class,
            "labels": { "naslos.local/pool": pool, "app.kubernetes.io/component": "app-pool-storage" },
        },
        "provisioner": class,
        "volumeBindingMode": "WaitForFirstConsumer",
        "reclaimPolicy": "Delete",
    });
    (configmap, deployment, storage_class)
}

impl Manager {
    pub fn new(cfg: Config) -> Result<Self, String> {
        let mut store = Store::new(&cfg.store_path);
        store.load()?;
        // The owner's saved limit wins; otherwise the chart's default. Clamp to
        // the provisioned pool so a smaller chart (or a downgrade) cannot hand
        // out a slot that does not exist.
        let stored_limit = load_pool_settings(&cfg.pool_settings_path);
        let limit = clamp_pool_limit(
            stored_limit
                .or(cfg.namespace_pool_limit)
                .unwrap_or(cfg.namespace_pool.len()),
            cfg.namespace_pool.len(),
        );
        Ok(Self {
            store,
            helm: cfg.helm,
            helm_priv: cfg.helm_privileged,
            priv_ns: cfg.privileged_namespace,
            charts: cfg.charts,
            catalog: cfg.catalog,
            router: cfg.router,
            discoverer: cfg.discoverer,
            base_domain: cfg.base_domain,
            sso_domains: cfg.sso_domains,
            namespace_pool: cfg.namespace_pool,
            privileged_namespace_pool: cfg.privileged_namespace_pool,
            namespace_pool_limit: Mutex::new(limit),
            pool_settings_path: cfg.pool_settings_path,
            kube: cfg.kube,
            datasets: cfg.datasets,
            pool_provisioner_image: cfg.pool_provisioner_image,
            app_dns: cfg.app_dns,
            usage: crate::app_usage::UsageCollector::default(),
            reservations: Arc::new(Mutex::new(std::collections::HashSet::new())),
            app_locks: Mutex::new(HashMap::new()),
        })
    }

    /// The number of baseline slots currently handed out.
    fn baseline_limit(&self) -> usize {
        self.namespace_pool_limit
            .lock()
            .map(|l| *l)
            .unwrap_or_else(|_| self.namespace_pool.len())
            .min(self.namespace_pool.len())
    }

    /// Slot accounting for both pools: provisioned namespaces, the usable
    /// limit, and how many are taken by records or in-flight installs.
    pub fn pool_status(&self) -> serde_json::Value {
        let recorded: Vec<String> = self
            .store
            .list()
            .iter()
            .map(|r| self.display_namespace(r))
            .collect();
        let reservations = self
            .reservations
            .lock()
            .map(|r| r.clone())
            .unwrap_or_default();
        let used_in = |pool: &[String]| -> usize {
            pool.iter()
                .filter(|ns| recorded.iter().any(|r| r == *ns) || reservations.contains(*ns))
                .count()
        };
        let baseline_limit = self.baseline_limit();
        let baseline_used = used_in(&self.namespace_pool);
        let privileged_used = used_in(&self.privileged_namespace_pool);
        serde_json::json!({
            "baseline": {
                "provisioned": self.namespace_pool.len(),
                "limit": baseline_limit,
                "used": baseline_used,
                "available": baseline_limit.saturating_sub(baseline_used),
            },
            "privileged": {
                "provisioned": self.privileged_namespace_pool.len(),
                "limit": self.privileged_namespace_pool.len(),
                "used": privileged_used,
                "available": self
                    .privileged_namespace_pool
                    .len()
                    .saturating_sub(privileged_used),
            },
        })
    }

    /// Change how many baseline slots are usable and persist it. The limit can
    /// grow up to the chart-provisioned pool (no platform upgrade needed) and
    /// shrink only down to the number of apps already installed.
    pub fn set_namespace_limit(&self, limit: usize) -> Result<serde_json::Value, String> {
        let provisioned = self.namespace_pool.len();
        let used = self.pool_status()["baseline"]["used"].as_u64().unwrap_or(0) as usize;
        validate_pool_limit(limit, provisioned, used)?;
        if !self.pool_settings_path.is_empty() {
            let data = serde_json::to_vec_pretty(&PoolSettings { limit: Some(limit) })
                .map_err(|e| format!("encoding app namespace settings: {e}"))?;
            crate::chartsrepo::write_atomic(&self.pool_settings_path, &data, 0o600, 0o750)?;
        }
        {
            // Drop the guard before `pool_status()` relocks it.
            let mut current = self
                .namespace_pool_limit
                .lock()
                .map_err(|_| "app namespace limit is poisoned".to_string())?;
            *current = limit;
        }
        Ok(self.pool_status())
    }

    fn helm_for(&self, rec: &Record) -> helm::Client {
        self.helm_client(rec.privileged, &self.target_namespace(rec))
    }

    fn helm_client(&self, privileged: bool, namespace: &str) -> helm::Client {
        let base = if privileged {
            self.helm_priv.as_ref().unwrap_or(&self.helm)
        } else {
            &self.helm
        };
        base.for_namespace(namespace)
    }

    fn app_lock(&self, name: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.app_locks.lock().unwrap();
        locks
            .entry(name.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// Whether a namespace is one of the configured per-app isolation pools.
    /// A record installed while isolation was disabled lives in the shared
    /// namespace and must not gain datasets.
    fn is_isolated_namespace(&self, namespace: &str) -> bool {
        !namespace.is_empty()
            && (self.namespace_pool.iter().any(|n| n == namespace)
                || self
                    .privileged_namespace_pool
                    .iter()
                    .any(|n| n == namespace))
    }

    pub fn record_is_isolated(&self, rec: &Record) -> bool {
        self.is_isolated_namespace(&rec.namespace)
    }

    /// The namespace a record's workloads run in (its isolated slot, the
    /// privileged namespace, or the legacy shared namespace).
    pub fn record_namespace(&self, rec: &Record) -> String {
        self.target_namespace(rec)
    }

    fn target_namespace(&self, rec: &Record) -> String {
        if !rec.namespace.is_empty() {
            rec.namespace.clone()
        } else if rec.privileged && !self.priv_ns.is_empty() {
            self.priv_ns.clone()
        } else {
            self.helm.namespace().to_string()
        }
    }

    /// Pick a free namespace from the pool (or the legacy shared namespace
    /// when no pool is configured) and reserve it for this install.
    ///
    /// The record is only written after Helm finishes, so reading persisted
    /// records alone lets two concurrent installs claim the same namespace.
    fn reserve_namespace(
        &self,
        privileged: bool,
    ) -> Result<(String, Option<NamespaceReservation>), String> {
        let limit = if privileged {
            self.privileged_namespace_pool.len()
        } else {
            self.baseline_limit()
        };
        let pool = if privileged {
            &self.privileged_namespace_pool
        } else {
            &self.namespace_pool
        };
        if pool.is_empty() {
            return Ok((
                if privileged && !self.priv_ns.is_empty() {
                    self.priv_ns.clone()
                } else {
                    self.helm.namespace().to_string()
                },
                None,
            ));
        }

        let mut reservations = self
            .reservations
            .lock()
            .map_err(|_| "app namespace reservations are poisoned".to_string())?;
        let mut used: std::collections::HashSet<String> = self
            .store
            .list()
            .iter()
            .map(|r| self.display_namespace(r))
            .collect();
        used.extend(reservations.iter().cloned());

        let usable = &pool[..limit.min(pool.len())];
        let namespace = pick_namespace(usable, &used).ok_or_else(|| {
            format!(
                "no free app namespace slot is available (limit {limit} of {} provisioned); raise the limit in Apps → Installed",
                pool.len()
            )
        })?;
        reservations.insert(namespace.clone());
        Ok((
            namespace.clone(),
            Some(NamespaceReservation {
                reserved: self.reservations.clone(),
                namespace,
            }),
        ))
    }

    /// Delete API-created Internet policies in `namespace` that no record
    /// backs. An install reserves the namespace before it records a release,
    /// so an attempt that dies between pre-creating the policy and recording
    /// (API restart, crash) would otherwise leave a namespace-wide public
    /// egress allow for the next app in that namespace.
    async fn prune_orphan_internet_policies(&self, namespace: &str) {
        let Some(kube) = &self.kube else {
            return;
        };
        let active: std::collections::HashSet<String> = self
            .store
            .list()
            .iter()
            .filter(|r| self.target_namespace(r) == namespace)
            .map(|r| r.name.clone())
            .collect();
        let list = match kube
            .list_json(
                "networkpolicy",
                namespace,
                Some(&format!("{MANAGED_BY_LABEL}={MANAGED_BY_VALUE}")),
            )
            .await
        {
            Ok(list) => list,
            Err(e) => {
                tracing::warn!("app internet policy sweep for {namespace}: {e}");
                return;
            }
        };
        for name in orphan_internet_policies(&list, &active) {
            if let Err(e) = kube.delete("networkpolicy", &name, namespace).await {
                tracing::warn!("removing orphaned Internet policy {namespace}/{name}: {e}");
            } else {
                tracing::info!("removed orphaned Internet policy {namespace}/{name}");
            }
        }
    }

    pub fn records(&self) -> Vec<Record> {
        self.store.list()
    }

    pub fn get(&self, name: &str) -> Result<Record, String> {
        self.store.get(name)
    }

    pub fn base_domain(&self) -> &str {
        &self.base_domain
    }

    pub fn sso_domains(&self) -> Vec<String> {
        self.sso_domains.as_ref().map(|f| f()).unwrap_or_default()
    }

    pub fn auth_allowed(&self, base_domain: &str) -> bool {
        let base = if base_domain.is_empty() {
            self.base_domain.as_str()
        } else {
            base_domain
        };
        if base.is_empty() {
            return false;
        }
        self.sso_domains().iter().any(|d| d == base)
    }

    /// Reconcile the per-app env Secret and inject its checksum into `values`.
    ///
    /// `incoming` is the submitted list for an install/update; `None` keeps the
    /// stored Secret (upgrades that do not touch environment variables). The
    /// returned outcome carries the previous Secret data so the caller can
    /// restore it when the Helm operation fails.
    #[allow(clippy::too_many_arguments)]
    async fn prepare_env(
        &self,
        rec_name: &str,
        namespace: &str,
        app: &crate::catalog::App,
        chart_dir: &str,
        values: &mut serde_json::Map<String, serde_json::Value>,
        incoming: Option<&[EnvVar]>,
        stored: &[EnvMeta],
        helm: &helm::Client,
    ) -> Result<EnvOutcome, String> {
        let supported = app
            .capabilities
            .iter()
            .any(|capability| capability == app_env::CAPABILITY);
        if !supported {
            if incoming.map(|vars| !vars.is_empty()).unwrap_or(false) {
                return Err(format!(
                    "chart {:?} does not support custom environment variables",
                    app.name
                ));
            }
            let mut outcome = EnvOutcome::default();
            if !stored.is_empty() {
                outcome.warning = "the chart no longer supports custom environment variables; the existing naslos-env Secret is kept but is no longer mounted"
                    .to_string();
            }
            return Ok(outcome);
        }

        let existing_secret = match &self.kube {
            Some(kube) => app_env::read_secret(kube, namespace, rec_name).await?,
            None => None,
        };
        let existing = existing_secret.clone().unwrap_or_default();
        let (data, meta) = match incoming {
            Some(vars) => (
                app_env::resolve(&existing, vars)?,
                Some(EnvMeta::from_vars(vars)),
            ),
            None => (existing.clone(), None),
        };

        let rendered = helm
            .template_dir(
                rec_name,
                chart_dir,
                &serde_json::Value::Object(values.clone()),
            )
            .await
            .map_err(|e| format!("rendering the chart to check environment variables: {e}"))?;
        let reserved = app_env::chart_env_names(&rendered, &app_env::secret_name(rec_name))?;
        for name in data.keys() {
            if reserved.contains(name) {
                return Err(format!("env var {name} is set by the chart"));
            }
        }

        // Injected on every reconcile so a hand-deleted Secret self-heals and
        // an unchanged Secret does not trigger a rollout.
        values.insert(
            app_env::CHECKSUM_VALUE.to_string(),
            serde_json::Value::String(app_env::checksum(&data)),
        );

        let Some(kube) = &self.kube else {
            if meta.as_ref().map(|m| !m.is_empty()).unwrap_or(false) {
                return Err(
                    "cannot store environment variables: no Kubernetes client is configured"
                        .to_string(),
                );
            }
            return Ok(EnvOutcome {
                meta,
                previous: existing_secret,
                wrote: false,
                warning: String::new(),
            });
        };

        let wrote = meta.is_some();
        if wrote {
            if data.is_empty() {
                app_env::delete_secret(kube, namespace, rec_name).await?;
            } else {
                app_env::write_secret(kube, namespace, rec_name, rec_name, &data).await?;
            }
        }

        let warning = if !stored.is_empty() && existing_secret.is_none() {
            "the naslos-env Secret is missing; environment variables will be empty".to_string()
        } else {
            String::new()
        };
        Ok(EnvOutcome {
            meta,
            previous: existing_secret,
            wrote,
            warning,
        })
    }

    /// Restore the env Secret after a failed Helm operation.
    async fn restore_env(
        &self,
        namespace: &str,
        release: &str,
        previous: Option<&std::collections::BTreeMap<String, String>>,
    ) {
        let Some(kube) = &self.kube else {
            return;
        };
        let result = match previous {
            Some(data) => app_env::write_secret(kube, namespace, release, release, data).await,
            None => app_env::delete_secret(kube, namespace, release).await,
        };
        if let Err(e) = result {
            tracing::warn!("app {release}: restoring the environment Secret failed: {e}");
        }
    }

    /// Environment variables with values for `GET /api/apps/{name}`. Secret
    /// values are always `null`; a missing Secret reports `null` values.
    pub async fn env_detail(&self, name: &str) -> Result<Vec<serde_json::Value>, String> {
        let rec = self.store.get(name)?;
        if rec.env.is_empty() {
            return Ok(Vec::new());
        }
        let data = match &self.kube {
            Some(kube) => app_env::read_secret(kube, &self.target_namespace(&rec), name).await?,
            None => None,
        };
        Ok(rec
            .env
            .iter()
            .map(|meta| {
                let value = if meta.secret {
                    None
                } else {
                    data.as_ref().and_then(|data| data.get(&meta.name)).cloned()
                };
                serde_json::json!({ "name": meta.name, "value": value, "secret": meta.secret })
            })
            .collect())
    }

    /// Install resolves the chart from the catalog, installs it, records the
    /// exposure and applies routing.
    pub async fn install(
        &self,
        req: InstallRequest,
        progress: Option<&ProgressFn>,
    ) -> Result<View, String> {
        validate_release_name(&req.name)?;
        let lock = self.app_lock(&req.name);
        let _guard = lock.lock().await;
        // Reject a duplicate install before reserving a namespace: otherwise a
        // second Install of an existing app fails later with a misleading
        // "no free per-app namespace" (the namespace pool can be full) and the
        // operator is not told the real reason.
        ensure_not_installed(&self.store, &req.name)?;
        report(
            progress,
            STAGE_PREPARING,
            &format!("Resolving {} from the catalog", req.name),
        );
        let catalog = (self.catalog)().ok_or("catalog is not available")?;
        let app = catalog.get(&req.name)?;
        // The HTTP layer checked the catalog snapshot it showed the operator.
        // The job resolves the (swappable) catalog again, so re-check here: a
        // refresh between confirm and execution must not install privileged
        // with only the generic confirmation.
        if app.privileged && !req.privileged_confirmed {
            return Err(
                "this app now requires privileged mode; review that and confirm the install again"
                    .to_string(),
            );
        }
        let chart_dir = self.resolve_chart(&app).await?;
        crate::chartsrepo::assert_regular_tree(&chart_dir)?;

        // Load the chart defaults so pool-backed installs can override every
        // persistence key, including keys the catalog schema does not expose.
        let chart_values = self.helm.show_values(&chart_dir).await?;
        // The API owns the dataset Helm values: never let a client point a
        // chart at an arbitrary claim or host path, and keep schema validation
        // from seeing platform-managed keys.
        let mut client_values = req.values.clone().unwrap_or_default();
        strip_dataset_values(&mut client_values);
        let mut values = merge_values(&chart_values, app.default_values.as_object());
        values = merge_values(&serde_json::Value::Object(values), Some(&client_values));
        coerce_schema_numbers(&app.schema, &mut values);
        validate_install_values(&app, &values)?;
        validate_resource_quantities(&values)?;
        let datasets = normalize_dataset_names(&req.datasets)?;
        let (namespace, _reservation) = self.reserve_namespace(app.privileged)?;
        self.validate_resource_quota(&namespace, &values).await?;
        let mut exposure = exposure_for(&app, req.exposure.as_ref(), &req.name);
        // The catalog's default subdomain was never validated on the install
        // path; an invalid host must not reach routing or an app CNAME.
        validate_subdomain(&exposure.subdomain)?;
        validate_protected_paths(&exposure.protected_paths)?;
        // Reject an unenforceable opt-out before creating any storage.
        self.validate_internet(&namespace, &exposure)?;
        // A namespace whose previous install died before recording a release
        // can still carry that attempt's namespace-wide egress allow.
        self.prune_orphan_internet_policies(&namespace).await;
        let provision = self
            .provision_datasets(&req.name, &namespace, app.privileged, &datasets, &[])
            .await?;
        for (key, value) in &provision.values {
            values.insert(key.clone(), value.clone());
        }
        self.provision_pool_storage(&req.pool, &mut values).await?;

        // Write the env Secret before Helm: the chart mounts it optionally, so
        // the first pod starts with the variables already present.
        let env_helm = self.helm_client(app.privileged, &namespace);
        let env_outcome = match self
            .prepare_env(
                &req.name,
                &namespace,
                &app,
                &chart_dir,
                &mut values,
                Some(&req.env),
                &[],
                &env_helm,
            )
            .await
        {
            Ok(outcome) => outcome,
            Err(e) => {
                self.rollback_provision(&req.name, &provision).await;
                return Err(e);
            }
        };

        // Grant public egress before the install wait. A dedicated baseline
        // namespace is default-deny for egress and the `<app>-internet` policy
        // is normally reconciled only after a successful install, but apps
        // such as Bazarr block on public startup calls (release and
        // announcement checks) and would never pass readiness, so Helm would
        // roll every attempt back. Shared and privileged namespaces keep the
        // post-install behavior: their policy is namespace-scoped and must not
        // exist before the app does.
        let precreated_internet = self.preinstall_internet(&namespace, &exposure);
        if precreated_internet {
            if let Some(router) = &self.router {
                if let Err(e) = router.ensure_internet(&req.name, &namespace).await {
                    // The apply may have reached the server before the client
                    // timed out; do not leave a namespace-wide allow behind.
                    if let Err(cleanup) = router.delete_internet(&req.name, &namespace).await {
                        tracing::warn!(
                            "app {:?}: cleaning up the Internet policy after a failed create: {cleanup}",
                            req.name
                        );
                    }
                    self.rollback_provision(&req.name, &provision).await;
                    return Err(format!("creating the Internet egress policy: {e}"));
                }
            }
        }

        report(
            progress,
            STAGE_INSTALLING,
            &format!("Installing {}", req.name),
        );
        if let Err(e) = self
            .helm_client(app.privileged, &namespace)
            .install_dir(
                &req.name,
                &chart_dir,
                &serde_json::Value::Object(values.clone()),
            )
            .await
        {
            // Do not leak a PV/PVC or a Secret when Helm fails before the
            // record exists.
            self.rollback_provision(&req.name, &provision).await;
            if env_outcome.wrote {
                self.restore_env(&namespace, &req.name, env_outcome.previous.as_ref())
                    .await;
            }
            // Nor hand the reserved namespace's egress allow to the next app.
            // Delete only the policy: no route of this release exists yet.
            if precreated_internet {
                if let Some(router) = &self.router {
                    if let Err(cleanup) = router.delete_internet(&req.name, &namespace).await {
                        tracing::warn!(
                            "app {:?}: cleaning up the Internet policy after a failed install: {cleanup}",
                            req.name
                        );
                    }
                }
            }
            return Err(e);
        }

        report(
            progress,
            STAGE_FINALIZING,
            "Recording the install and applying routing",
        );
        exposure = self
            .fill_discovered_service(&req.name, &namespace, exposure)
            .await;

        // Platform-owned keys are never persisted in the app record; they are
        // recomputed on every install/upgrade.
        for key in PLATFORM_ENV_KEYS {
            values.remove(key);
        }
        let now = Utc::now();
        let base_domain = if req.base_domain.is_empty() {
            self.base_domain.clone()
        } else {
            req.base_domain.clone()
        };
        let rec = Record {
            name: req.name.clone(),
            source: app.source.clone(),
            channel: app.channel.clone(),
            chart_path: app.chart_path.clone(),
            chart_version: app.version.clone(),
            values,
            exposure,
            base_domain: base_domain.clone(),
            namespace,
            datasets: datasets.clone(),
            datasets_layout: if datasets.is_empty()
                || !chart_tree_mentions_datasets(std::path::Path::new(&chart_dir))
            {
                String::new()
            } else {
                DATASETS_LAYOUT_V2.to_string()
            },
            privileged: app.privileged,
            env: env_outcome.meta.unwrap_or_default(),
            created_at: Some(now),
            updated_at: Some(now),
            ..Default::default()
        };
        if let Err(e) = self.store.upsert(rec.clone()) {
            // Helm succeeded but the platform cannot record the release.
            // Undo the install so a retry is not blocked by an unmanaged
            // release and no namespace-wide egress allow is left behind.
            if let Err(cleanup) = self.helm_for(&rec).uninstall(&req.name).await {
                tracing::warn!(
                    "app {:?}: rolling back an unrecorded install: {cleanup}",
                    req.name
                );
            }
            if let Some(router) = &self.router {
                if let Err(cleanup) = router.delete(&rec.namespace, &req.name).await {
                    tracing::warn!(
                        "app {:?}: removing routing for an unrecorded install: {cleanup}",
                        req.name
                    );
                }
            }
            self.rollback_provision(&req.name, &provision).await;
            if env_outcome.wrote {
                self.restore_env(&rec.namespace, &req.name, env_outcome.previous.as_ref())
                    .await;
            }
            return Err(format!("recording the install: {e}"));
        }
        self.apply_route(&rec, &req.base_domain).await;
        self.reconcile_app_dns(&rec).await;
        self.view(&rec.name).await
    }

    /// Upgrade reconciles an installed app against its chart, replacing values
    /// and, when `datasets` is given, reconciling the attached datasets.
    ///
    /// `None` keeps the stored selection; `Some(&[])` detaches every dataset.
    pub async fn upgrade(
        &self,
        name: &str,
        values: Option<&serde_json::Map<String, serde_json::Value>>,
        datasets: Option<&[String]>,
        env: Option<&[EnvVar]>,
        progress: Option<&ProgressFn>,
    ) -> Result<View, String> {
        let lock = self.app_lock(name);
        let _guard = lock.lock().await;
        report(
            progress,
            STAGE_PREPARING,
            &format!("Resolving the chart for {name}"),
        );
        let mut rec = self.store.get(name)?;
        if rec.orphaned {
            return Err(format!(
                "app {name:?} is orphaned and cannot be reconfigured"
            ));
        }
        let desired = match datasets {
            Some(list) => normalize_dataset_names(list)?,
            None => rec.datasets.clone(),
        };
        if desired != rec.datasets && !self.is_isolated_namespace(&rec.namespace) {
            return Err(format!(
                "app {name:?} does not run in an isolated namespace; reinstall it into one to change its datasets"
            ));
        }
        // An isolated app's chart has no fallback volume: the shared
        // `naslos-datasets` claim only exists in the legacy apps namespace.
        if desired.is_empty()
            && !rec.datasets.is_empty()
            && self.is_isolated_namespace(&rec.namespace)
        {
            return Err(format!(
                "app {name:?} needs at least one dataset; keep one or uninstall the app to remove its storage"
            ));
        }
        let catalog = (self.catalog)().ok_or("catalog is not available")?;
        let app = catalog.get(name)?;
        let chart_dir = self.resolve_chart(&app).await?;
        crate::chartsrepo::assert_regular_tree(&chart_dir)?;
        let mut client_values = values.cloned().unwrap_or_default();
        strip_dataset_values(&mut client_values);
        let mut merged = merge_values(
            &serde_json::Value::Object(rec.values.clone()),
            Some(&client_values),
        );

        // A redacted secret comes back as `null`; keep the stored value.
        for key in secret_value_keys(&app.schema) {
            if merged.get(&key).map(|v| v.is_null()).unwrap_or(false) {
                if let Some(previous) = rec.values.get(&key) {
                    merged.insert(key, previous.clone());
                }
            }
        }
        coerce_schema_numbers(&app.schema, &mut merged);
        // `install` validated the catalog schema; `upgrade` did not.
        validate_install_values(&app, &merged)?;
        validate_resource_quantities(&merged)?;
        self.validate_resource_quota(&self.target_namespace(&rec), &merged)
            .await?;

        let chart_supports_datasets =
            chart_tree_mentions_datasets(std::path::Path::new(&chart_dir));
        let removed: Vec<String> = rec
            .datasets
            .iter()
            .filter(|n| !desired.contains(n))
            .cloned()
            .collect();
        let provision = self
            .provision_datasets(
                name,
                &self.target_namespace(&rec),
                app.privileged,
                &desired,
                &rec.datasets,
            )
            .await?;

        // Reconcile the platform-owned keys after validation (the catalog
        // schema never sees them). When every dataset is detached, fall back to
        // the chart's own values for the legacy single-claim contract.
        merged.remove("datasets");
        merged.remove("datasetsClaims");
        if desired.is_empty() {
            if !rec.datasets.is_empty() {
                let chart_defaults = self.helm.show_values(&chart_dir).await?;
                let fallback = if app.privileged {
                    "hostPath"
                } else {
                    "datasetsClaim"
                };
                // Honour a hostPath the operator submitted in this request;
                // otherwise restore the chart default.
                if !client_values.contains_key(fallback) {
                    match chart_defaults.get(fallback) {
                        Some(value) => {
                            merged.insert(fallback.to_string(), value.clone());
                        }
                        None => {
                            merged.remove(fallback);
                        }
                    }
                }
            }
        } else {
            for (key, value) in &provision.values {
                merged.insert(key.clone(), value.clone());
            }
        }

        let env_helm = self.helm_for(&rec);
        let env_outcome = match self
            .prepare_env(
                name,
                &self.target_namespace(&rec),
                &app,
                &chart_dir,
                &mut merged,
                env,
                &rec.env,
                &env_helm,
            )
            .await
        {
            Ok(outcome) => outcome,
            Err(e) => {
                self.rollback_provision(name, &provision).await;
                return Err(e);
            }
        };

        report(progress, STAGE_INSTALLING, &format!("Upgrading {name}"));
        if let Err(e) = env_helm
            .upgrade_dir(name, &chart_dir, &serde_json::Value::Object(merged.clone()))
            .await
        {
            // Only the resources and attachments this operation created are
            // rollback candidates; pre-existing ones are left alone.
            self.rollback_provision(name, &provision).await;
            if env_outcome.wrote {
                self.restore_env(
                    &self.target_namespace(&rec),
                    name,
                    env_outcome.previous.as_ref(),
                )
                .await;
            }
            return Err(e);
        }

        report(
            progress,
            STAGE_FINALIZING,
            "Recording the new configuration",
        );
        // Removed datasets are cleaned up only after Helm accepted the new
        // values; failures here leave the claims behind rather than the app
        // pointing at deleted storage.
        if !removed.is_empty() {
            self.delete_claims(name, &self.target_namespace(&rec), &removed)
                .await;
        }
        // The checksum is platform-owned and recomputed, never persisted.
        for key in PLATFORM_ENV_KEYS {
            merged.remove(key);
        }
        rec.values = merged;
        rec.datasets = desired;
        if let Some(meta) = env_outcome.meta {
            rec.env = meta;
        }
        if chart_supports_datasets {
            rec.datasets_layout = DATASETS_LAYOUT_V2.to_string();
        }
        rec.chart_version = app.version.clone();
        rec.updated_at = Some(Utc::now());
        rec.last_error = env_outcome.warning;
        self.store.upsert(rec)?;
        self.view(name).await
    }

    /// Upgrade every installed app with datasets to the `/datasets/<name>`
    /// mount layout, once. Charts that do not read `.Values.datasets` yet are
    /// skipped so a premature API deploy cannot lock the old layout in.
    pub async fn migrate_dataset_layout(
        &self,
        progress: Option<&ProgressFn>,
    ) -> Result<(), String> {
        let Some(catalog) = (self.catalog)() else {
            tracing::warn!("dataset layout migration skipped: catalog is not available");
            return Ok(());
        };
        for rec in self.store.list() {
            if rec.orphaned || rec.datasets.is_empty() || rec.datasets_layout == DATASETS_LAYOUT_V2
            {
                continue;
            }
            if !self.is_isolated_namespace(&rec.namespace) {
                tracing::warn!(
                    "app {:?}: cannot migrate the dataset layout outside an isolated namespace; reinstall it",
                    rec.name
                );
                continue;
            }
            let app = match catalog.get(&rec.name) {
                Ok(app) => app,
                Err(e) => {
                    tracing::warn!("app {:?}: dataset layout migration skipped: {e}", rec.name);
                    continue;
                }
            };
            let chart_dir = match self.resolve_chart(&app).await {
                Ok(dir) => dir,
                Err(e) => {
                    tracing::warn!("app {:?}: dataset layout migration skipped: {e}", rec.name);
                    continue;
                }
            };
            if !chart_tree_mentions_datasets(std::path::Path::new(&chart_dir)) {
                tracing::info!(
                    "app {:?}: chart does not read .Values.datasets yet; leaving the layout unchanged",
                    rec.name
                );
                continue;
            }
            report(
                progress,
                STAGE_PREPARING,
                &format!("Migrating datasets for {}", rec.name),
            );
            if let Err(e) = self.upgrade(&rec.name, None, None, None, progress).await {
                // Leave the record unmarked so the next start retries.
                tracing::warn!("app {:?}: dataset layout migration failed: {e}", rec.name);
            }
        }
        Ok(())
    }

    /// SetExposure updates the exposure settings and re-renders the route.
    pub async fn set_exposure(
        &self,
        name: &str,
        patch: &ExposurePatch,
        base_domain: &str,
    ) -> Result<View, String> {
        let mut rec = self.store.get(name)?;
        let mut merged = patch.apply(&rec.exposure);
        validate_subdomain(&merged.subdomain)?;
        validate_protected_paths(&merged.protected_paths)?;
        let base_domain = if base_domain.is_empty() {
            if rec.base_domain.is_empty() {
                self.base_domain.clone()
            } else {
                rec.base_domain.clone()
            }
        } else {
            base_domain.to_string()
        };
        if merged.auth && !self.auth_allowed(&base_domain) {
            return Err("auth requires the domain to be in the SSO domain list".to_string());
        }
        let namespace = self.target_namespace(&rec);
        self.validate_internet(&namespace, &merged)?;
        if let Some(service) = &patch.service {
            if !service.is_empty() {
                merged.service = render_service_name(service, &rec.name);
            }
        }
        if merged.service.is_empty() {
            merged = self
                .fill_discovered_service(&rec.name, &namespace, merged)
                .await;
        }
        rec.exposure = merged;
        rec.base_domain = base_domain.clone();
        rec.updated_at = Some(Utc::now());
        rec.last_error.clear();
        self.store.upsert(rec.clone())?;
        self.apply_route(&rec, &base_domain).await;
        self.reconcile_app_dns(&rec).await;
        self.view(name).await
    }

    /// Uninstall removes the release, its route and its record.
    pub async fn uninstall(&self, name: &str, progress: Option<&ProgressFn>) -> Result<(), String> {
        let lock = self.app_lock(name);
        let _guard = lock.lock().await;
        report(
            progress,
            STAGE_PREPARING,
            &format!("Loading the record for {name}"),
        );
        let rec = self.store.get(name)?;
        if !rec.orphaned {
            report(
                progress,
                STAGE_INSTALLING,
                &format!("Removing the {name} release"),
            );
            self.helm_for(&rec).uninstall(name).await?;
        }
        report(progress, STAGE_FINALIZING, "Removing the route and record");
        if let Some(router) = &self.router {
            router.delete(&self.target_namespace(&rec), name).await?;
        }
        if let Some(dns) = &self.app_dns {
            if let Err(e) = dns.remove(&rec).await {
                tracing::warn!("app {name}: removing its DNS record failed: {e}");
            }
        }
        // The Secret is not part of the Helm release; remove it explicitly.
        if let Some(kube) = &self.kube {
            if let Err(e) = app_env::delete_secret(kube, &self.target_namespace(&rec), name).await {
                tracing::warn!("app {name}: removing the environment Secret failed: {e}");
            }
        }
        if let Err(e) = self.delete_datasets(&rec).await {
            tracing::warn!("app {name}: dataset cleanup failed: {e}");
        }
        self.store.delete(name)
    }

    /// All records with live status, looked up in each record's namespace.
    pub async fn list(&self) -> Result<Vec<View>, String> {
        let catalog = (self.catalog)();
        let mut views = Vec::new();
        let mut first_err: Option<String> = None;
        for rec in self.store.list() {
            let ns = self.display_namespace(&rec);
            let mut view = View {
                namespace: ns.clone(),
                chart: rec.chart_path.clone(),
                status: "missing".to_string(),
                url: self.url_for(&rec),
                record: rec.clone(),
                redacted_keys: Vec::new(),
                ..Default::default()
            };
            view.redacted_keys = self.redact_record_values(&mut view.record);
            // The list endpoint does not read the env Secret; keep the
            // variable names for the detail endpoint only.
            view.record.env.clear();
            let mut live_version: Option<String> = None;
            match self.helm_for(&rec).get(&rec.name).await {
                Ok(rel) => {
                    view.status = rel.status.clone();
                    if !rel.chart.is_empty() {
                        view.chart = rel.chart;
                    }
                    if !rel.version.is_empty() {
                        live_version = Some(rel.version);
                    }
                }
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
            let catalog_app = catalog.as_ref().and_then(|c| c.get(&rec.name).ok());
            enrich_view(&mut view, live_version.as_deref(), catalog_app.as_ref());
            views.push(view);
        }
        if views.is_empty() {
            if let Some(e) = first_err {
                return Err(e);
            }
        }
        views.sort_by(|a, b| a.record.name.cmp(&b.record.name));
        Ok(views)
    }

    fn display_namespace(&self, rec: &Record) -> String {
        self.target_namespace(rec)
    }

    /// Create orphaned records for releases that predate the refactor.
    pub fn backfill(&self, platform_release: &str, releases: &[helm::App]) -> Result<(), String> {
        for rel in releases {
            if rel.name == platform_release {
                continue;
            }
            if self.store.get(&rel.name).is_ok() {
                continue;
            }
            let rec = Record {
                name: rel.name.clone(),
                chart_version: rel.version.clone(),
                values: match &rel.values {
                    serde_json::Value::Object(m) => m.clone(),
                    _ => Default::default(),
                },
                namespace: rel.namespace.clone(),
                orphaned: true,
                created_at: Some(Utc::now()),
                updated_at: Some(Utc::now()),
                ..Default::default()
            };
            self.store.upsert(rec)?;
        }
        Ok(())
    }

    fn dns_base_domain(&self, rec: &Record) -> String {
        if rec.base_domain.is_empty() {
            self.base_domain.clone()
        } else {
            rec.base_domain.clone()
        }
    }

    /// Converge one app's CNAME and persist the result.
    async fn reconcile_app_dns(&self, rec: &Record) {
        let Some(dns) = &self.app_dns else {
            return;
        };
        let state = dns.reconcile(rec, &self.dns_base_domain(rec)).await;
        if let Err(e) = self.store.update(&rec.name, |r| r.dns = state) {
            tracing::warn!("app {:?}: recording DNS state: {e}", rec.name);
        }
    }

    /// The manual retry endpoint: converge DNS now and return the fresh view.
    pub async fn reconcile_dns(&self, name: &str) -> Result<View, String> {
        let rec = self.store.get(name)?;
        if rec.orphaned {
            return Err(format!("app {name:?} is orphaned and cannot be reconciled"));
        }
        self.reconcile_app_dns(&rec).await;
        self.view(name).await
    }

    /// Reconcile every app whose exposure uses `base_domain`.
    pub async fn reconcile_dns_for_domain(&self, base_domain: &str) {
        if self.app_dns.is_none() {
            return;
        }
        for rec in self.store.list() {
            if rec.orphaned {
                continue;
            }
            if self.dns_base_domain(&rec) == base_domain {
                self.reconcile_app_dns(&rec).await;
            }
        }
    }

    /// Delete every record this platform owns under `base_domain` (used before
    /// a domain or its credentials are removed).
    pub async fn remove_dns_for_domain(&self, base_domain: &str) {
        let Some(dns) = &self.app_dns else {
            return;
        };
        for rec in self.store.list() {
            let owned = rec.dns.as_ref().map(|state| state.owned).unwrap_or(false);
            if !owned || self.dns_base_domain(&rec) != base_domain {
                continue;
            }
            match dns.remove(&rec).await {
                Ok(()) => {
                    let _ = self.store.update(&rec.name, |r| r.dns = None);
                }
                Err(e) => {
                    tracing::warn!(
                        "app {:?}: removing its DNS record before the domain changes: {e}",
                        rec.name
                    );
                    let _ = self.store.update(&rec.name, |r| {
                        if let Some(state) = &mut r.dns {
                            state.status = "error".to_string();
                            state.error = e.clone();
                            state.updated_at = Some(Utc::now());
                        }
                    });
                }
            }
        }
    }

    /// Retry records that failed (called from the catalog refresher loop).
    pub async fn reconcile_dns_errors(&self) {
        if self.app_dns.is_none() {
            return;
        }
        for rec in self.store.list() {
            let failed = rec
                .dns
                .as_ref()
                .map(|state| state.status == "error")
                .unwrap_or(false);
            if failed {
                self.reconcile_app_dns(&rec).await;
            }
        }
    }

    /// Re-apply routing for every non-orphaned record.
    pub async fn reconcile_routes(&self) -> Result<(), String> {
        // Drop egress policies a previous process pre-created for an install
        // that never recorded a release. This runs even when routing is
        // disabled: the policy is a namespace-wide egress allow.
        for namespace in &self.namespace_pool {
            self.prune_orphan_internet_policies(namespace).await;
        }
        if self.app_dns.is_some() {
            for rec in self.store.list() {
                if !rec.orphaned {
                    self.reconcile_app_dns(&rec).await;
                }
            }
        }
        let Some(router) = &self.router else {
            return Ok(());
        };
        let sso = self.sso_domains();
        let mut errs = Vec::new();
        for rec in self.store.list() {
            if rec.orphaned {
                continue;
            }
            let base = if rec.base_domain.is_empty() {
                self.base_domain.clone()
            } else {
                rec.base_domain.clone()
            };
            if let Err(e) = router
                .apply(&rec, &self.target_namespace(&rec), &base, &sso)
                .await
            {
                errs.push(format!("app {:?}: {e}", rec.name));
            }
        }
        // A non-primary SSO domain (e.g. the public app domain) needs its own
        // `/authelia` portal route: the forwardAuth redirect points at
        // `https://<base-domain>/authelia/`. Without this the redirect hits
        // Traefik's default 404.
        if let Err(e) = router.reconcile_portals(&sso, &self.base_domain).await {
            errs.push(format!("sso portals: {e}"));
        }
        if errs.is_empty() {
            Ok(())
        } else {
            Err(errs.join("; "))
        }
    }

    async fn apply_route(&self, rec: &Record, base_domain: &str) {
        let Some(router) = &self.router else {
            return;
        };
        let base = if base_domain.is_empty() {
            if rec.base_domain.is_empty() {
                self.base_domain.clone()
            } else {
                rec.base_domain.clone()
            }
        } else {
            base_domain.to_string()
        };
        if let Err(e) = router
            .apply(rec, &self.target_namespace(rec), &base, &self.sso_domains())
            .await
        {
            let _ = self.store.update(&rec.name, |r| r.last_error = e.clone());
        }
    }

    /// The Services a release rendered.
    pub async fn discover_services(&self, name: &str) -> Result<Vec<DiscoveredService>, String> {
        let rec = self.store.get(name)?;
        let Some(d) = &self.discoverer else {
            return Ok(Vec::new());
        };
        d.services_for_release(&self.target_namespace(&rec), name)
            .await
    }

    fn isolation_enabled(&self) -> bool {
        !self.namespace_pool.is_empty() || !self.privileged_namespace_pool.is_empty()
    }

    /// An app can only opt out of Internet access when it has a dedicated
    /// baseline namespace: the per-app policy is namespace-scoped, and a
    /// privileged namespace's base policy always allows public egress.
    fn internet_opt_out_supported(&self, namespace: &str) -> bool {
        self.namespace_pool.iter().any(|n| n == namespace)
    }

    /// Whether to apply the Internet policy before the install wait. Only a
    /// dedicated baseline namespace qualifies; see `install`.
    fn preinstall_internet(&self, namespace: &str, exposure: &Exposure) -> bool {
        preinstall_internet(namespace, exposure, &self.namespace_pool)
    }

    fn validate_internet(&self, namespace: &str, exposure: &Exposure) -> Result<(), String> {
        if !exposure.internet && !self.internet_opt_out_supported(namespace) {
            return Err(
                "Internet access can only be turned off for a baseline app with per-app isolation (apps.isolation.enabled); privileged app namespaces always allow outbound Internet"
                    .to_string(),
            );
        }
        Ok(())
    }

    /// Compare the chart's aggregate `resources` totals with the per-app
    /// namespace quota before Helm. The quota's admission error is otherwise
    /// only visible as a `--wait` timeout after a failed rollout. Fails open
    /// when the quota cannot be read, so a rolling API/chart upgrade does not
    /// block installs.
    async fn validate_resource_quota(
        &self,
        namespace: &str,
        values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), String> {
        let Some(kube) = &self.kube else {
            return Ok(());
        };
        let quota = match kube
            .get_json("resourcequota", "naslos-app-quota", namespace)
            .await
        {
            Ok(Some(quota)) => quota,
            Ok(None) => return Ok(()),
            Err(e) => {
                tracing::warn!("app namespace {namespace}: reading the resource quota: {e}");
                return Ok(());
            }
        };
        let Some(hard) = quota.pointer("/spec/hard").and_then(|v| v.as_object()) else {
            return Ok(());
        };
        let totals = resource_totals(values);
        let checks: [(&str, f64, bool); 4] = [
            ("requests.cpu", totals.requests_cpu, true),
            ("requests.memory", totals.requests_memory, false),
            ("limits.cpu", totals.limits_cpu, true),
            ("limits.memory", totals.limits_memory, false),
        ];
        let mut problems = Vec::new();
        for (key, requested, cpu) in checks {
            let Some(allowed) = hard
                .get(key)
                .and_then(|v| v.as_str())
                .and_then(quantity_number)
            else {
                continue;
            };
            if requested > allowed + 1e-9 {
                let requested = if cpu {
                    format_cores(requested)
                } else {
                    format_bytes(requested)
                };
                let allowed_raw = hard.get(key).and_then(|v| v.as_str()).unwrap_or("?");
                problems.push(format!("{key} {requested} > quota {allowed_raw}"));
            }
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "resources exceed the per-app namespace quota in {namespace}: {}. Raise apps.isolation.quota in the platform chart to allow it.",
                problems.join("; ")
            ))
        }
    }

    /// The per-app namespace quota the resource editor must respect.
    pub async fn resource_quota(&self, name: &str) -> Result<Option<serde_json::Value>, String> {
        let rec = self.store.get(name)?;
        let namespace = self.target_namespace(&rec);
        let Some(kube) = &self.kube else {
            return Ok(None);
        };
        let Some(quota) = kube
            .get_json("resourcequota", "naslos-app-quota", &namespace)
            .await?
        else {
            return Ok(None);
        };
        let hard = quota
            .pointer("/spec/hard")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        Ok(Some(serde_json::json!({
            "available": true,
            "namespace": namespace,
            "hard": hard,
        })))
    }

    /// Create the per-app PV/PVCs for the selected registry datasets and return
    /// the Helm values the chart needs. Baseline apps get a claim bound to the
    /// dataset; privileged apps get the specific host path (they can mount it).
    ///
    /// `existing` is the dataset list the record already carried. Only claims
    /// and registry attachments created by this call are rollback candidates.
    async fn provision_datasets(
        &self,
        app: &str,
        namespace: &str,
        privileged: bool,
        selected: &[String],
        existing: &[String],
    ) -> Result<DatasetProvision, String> {
        let mut provision = DatasetProvision::default();
        if selected.is_empty() {
            return Ok(provision);
        }
        if !self.isolation_enabled() {
            return Err(
                "attaching datasets requires per-app namespaces (apps.isolation.enabled=true)"
                    .to_string(),
            );
        }
        let Some(registry) = &self.datasets else {
            return Err("the dataset registry is not configured".to_string());
        };
        let Some(kube) = &self.kube else {
            return Err("the Kubernetes client is not configured".to_string());
        };
        // Resolve every record before touching the registry or the cluster.
        let mut records = Vec::with_capacity(selected.len());
        for name in selected {
            let record = registry
                .get(name)
                .ok_or_else(|| format!("dataset {name:?} is not registered"))?;
            records.push(record);
        }
        // Reserve each dataset before creating a claim. The reservation is
        // persisted, so two concurrent installs cannot both claim a private
        // dataset. Only newly attached entries are detached on failure.
        for name in selected {
            let already = existing.iter().any(|e| e == name);
            if let Err(e) = registry.attach(name, app) {
                self.rollback_provision(app, &provision).await;
                return Err(e);
            }
            if !already {
                provision.newly_attached.push(name.clone());
            }
        }

        if !privileged {
            for (name, record) in selected.iter().zip(records.iter()) {
                let object_name = claim_name(app, name);
                if object_name.len() > 253 {
                    self.rollback_provision(app, &provision).await;
                    return Err(format!("dataset claim name {object_name:?} is too long"));
                }
                let pv_existed = match self
                    .dataset_object_exists(kube, "pv", &object_name, "", app, name)
                    .await
                {
                    Ok(existed) => existed,
                    Err(e) => {
                        self.rollback_provision(app, &provision).await;
                        return Err(e);
                    }
                };
                let pvc_existed = match self
                    .dataset_object_exists(kube, "pvc", &object_name, namespace, app, name)
                    .await
                {
                    Ok(existed) => existed,
                    Err(e) => {
                        self.rollback_provision(app, &provision).await;
                        return Err(e);
                    }
                };
                let pv = serde_json::json!({
                    "apiVersion": "v1",
                    "kind": "PersistentVolume",
                    "metadata": {
                        "name": object_name,
                        "labels": {
                            "naslos.local/managed-dataset": name,
                            "naslos.local/app": app,
                        },
                    },
                    "spec": {
                        "capacity": { "storage": "10Ti" },
                        "accessModes": ["ReadWriteOnce"],
                        "persistentVolumeReclaimPolicy": "Retain",
                        "storageClassName": "naslos-app-data",
                        "hostPath": { "path": record.host_path, "type": "Directory" },
                    },
                });
                let pvc = serde_json::json!({
                    "apiVersion": "v1",
                    "kind": "PersistentVolumeClaim",
                    "metadata": {
                        "name": object_name,
                        "namespace": namespace,
                        "labels": {
                            "naslos.local/managed-dataset": name,
                            "naslos.local/app": app,
                        },
                    },
                    "spec": {
                        "accessModes": ["ReadWriteOnce"],
                        "storageClassName": "naslos-app-data",
                        "volumeName": object_name,
                        "resources": { "requests": { "storage": "10Ti" } },
                    },
                });
                if let Err(e) = kube.apply(&pv).await {
                    self.rollback_provision(app, &provision).await;
                    return Err(format!("creating dataset PV {object_name}: {e}"));
                }
                if !pv_existed {
                    provision
                        .created
                        .push(("pv".to_string(), object_name.clone(), String::new()));
                }
                if let Err(e) = kube.apply(&pvc).await {
                    self.rollback_provision(app, &provision).await;
                    return Err(format!("creating dataset PVC {object_name}: {e}"));
                }
                if !pvc_existed {
                    provision.created.push((
                        "pvc".to_string(),
                        object_name.clone(),
                        namespace.to_string(),
                    ));
                }
            }
        }
        provision.values = dataset_helm_values(app, privileged, selected, registry)?;
        Ok(provision)
    }

    /// Whether a PV/PVC with this claim name already exists, failing closed when
    /// it belongs to another app or dataset.
    async fn dataset_object_exists(
        &self,
        kube: &crate::kube::Client,
        kind: &str,
        name: &str,
        namespace: &str,
        app: &str,
        dataset: &str,
    ) -> Result<bool, String> {
        let object = kube
            .get_json(kind, name, namespace)
            .await
            .map_err(|e| format!("reading {kind} {name}: {e}"))?;
        let Some(object) = object else {
            return Ok(false);
        };
        let labels = object.pointer("/metadata/labels");
        let owner = labels
            .and_then(|l| l.get("naslos.local/app"))
            .and_then(|v| v.as_str());
        let managed = labels
            .and_then(|l| l.get("naslos.local/managed-dataset"))
            .and_then(|v| v.as_str());
        if owner != Some(app) || managed != Some(dataset) {
            return Err(format!(
                "{kind} {name:?} already exists and is not owned by app {app:?} dataset {dataset:?}; refusing to overwrite it"
            ));
        }
        Ok(true)
    }

    /// Delete every PV/PVC created by one provisioning attempt and detach the
    /// registry entries it newly attached.
    async fn rollback_provision(&self, app: &str, provision: &DatasetProvision) {
        if let Some(kube) = &self.kube {
            // Claims first: a bound PV carries the `pv-protection` finalizer
            // and a waiting `kubectl delete` would block until the timeout.
            for kind in ["pvc", "pv"] {
                for (created_kind, name, namespace) in &provision.created {
                    if created_kind == kind {
                        let _ = kube.delete(kind, name, namespace).await;
                    }
                }
            }
        }
        if let Some(registry) = &self.datasets {
            for name in &provision.newly_attached {
                let _ = registry.detach(name, app);
            }
        }
    }

    /// Ensure a pool-scoped local-path provisioner and point every chart
    /// persistence PVC at it. The provisioner's hostPath PVs land under
    /// `/var/mnt/<pool>/apps`.
    async fn provision_pool_storage(
        &self,
        pool: &str,
        values: &mut serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), String> {
        if pool.is_empty() {
            return Ok(());
        }
        validate_pool_for_storage(pool)?;
        let kube = self
            .kube
            .as_ref()
            .ok_or("storage pools require a Kubernetes client")?;
        let class = pool_storage_class(pool);
        let (configmap, deployment, storage_class) =
            pool_provisioner_objects(pool, &class, &self.pool_provisioner_image);
        kube.apply(&configmap)
            .await
            .map_err(|e| format!("creating pool provisioner config: {e}"))?;
        kube.apply(&deployment)
            .await
            .map_err(|e| format!("creating pool provisioner: {e}"))?;
        kube.apply(&storage_class)
            .await
            .map_err(|e| format!("creating pool StorageClass: {e}"))?;

        let deployment_name = format!("{class}-provisioner");
        let mut ready = false;
        for _ in 0..30 {
            if let Ok(Some(status)) = kube
                .get_json("deployment", &deployment_name, POOL_PROVISIONER_NAMESPACE)
                .await
            {
                let replicas = status
                    .pointer("/status/readyReplicas")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                if replicas >= 1 {
                    ready = true;
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
        if !ready {
            return Err(format!(
                "pool provisioner {deployment_name} did not become ready"
            ));
        }
        let mut as_value = serde_json::Value::Object(values.clone());
        set_persistence_storage_class(&mut as_value, &class);
        *values = as_value.as_object().cloned().unwrap_or_default();
        Ok(())
    }

    async fn delete_claims(&self, app: &str, namespace: &str, datasets: &[String]) {
        if let Some(registry) = &self.datasets {
            for name in datasets {
                let _ = registry.detach(name, app);
            }
        }
        let Some(kube) = &self.kube else {
            return;
        };
        for name in datasets {
            let object_name = claim_name(app, name);
            // App/dataset names both contain `-`, so two pairs can produce the
            // same claim name. Only delete objects this app and dataset own.
            if let Ok(true) = self
                .dataset_object_exists(kube, "pvc", &object_name, namespace, app, name)
                .await
            {
                let _ = kube.delete("pvc", &object_name, namespace).await;
            }
            if let Ok(true) = self
                .dataset_object_exists(kube, "pv", &object_name, "", app, name)
                .await
            {
                let _ = kube.delete("pv", &object_name, "").await;
            }
        }
    }

    async fn delete_datasets(&self, rec: &Record) -> Result<(), String> {
        self.delete_claims(&rec.name, &self.target_namespace(rec), &rec.datasets)
            .await;
        Ok(())
    }

    async fn fill_discovered_service(
        &self,
        release: &str,
        namespace: &str,
        e: Exposure,
    ) -> Exposure {
        if !e.service.is_empty() || self.discoverer.is_none() {
            return e;
        }
        let d = self.discoverer.as_ref().unwrap();
        match d.services_for_release(namespace, release).await {
            Ok(services) if !services.is_empty() => {
                let mut e = e;
                e.service = services[0].name.clone();
                e.port = services[0].port;
                e.scheme = services[0].scheme.clone();
                e
            }
            _ => e,
        }
    }

    async fn resolve_chart(&self, app: &crate::catalog::App) -> Result<String, String> {
        self.charts.ensure_fresh(&app.source, &app.channel).await?;
        self.charts.app_dir(&app.source, &app.channel, &app.name)
    }

    pub async fn view(&self, name: &str) -> Result<View, String> {
        let rec = self.store.get(name)?;
        let ns = self.display_namespace(&rec);
        let mut view = View {
            namespace: ns,
            chart: rec.chart_path.clone(),
            status: "missing".to_string(),
            url: self.url_for(&rec),
            record: rec.clone(),
            redacted_keys: Vec::new(),
            ..Default::default()
        };
        view.redacted_keys = self.redact_record_values(&mut view.record);
        let mut live_version: Option<String> = None;
        if let Ok(rel) = self.helm_for(&rec).get(name).await {
            view.status = rel.status.clone();
            if !rel.chart.is_empty() {
                view.chart = rel.chart;
            }
            if !rel.version.is_empty() {
                live_version = Some(rel.version);
            }
        }
        let catalog_app = (self.catalog)().and_then(|c| c.get(name).ok());
        enrich_view(&mut view, live_version.as_deref(), catalog_app.as_ref());
        Ok(view)
    }

    /// The `(app name, workload namespace)` pairs the usage collector attributes
    /// pods to.
    pub fn targets(&self) -> Vec<(String, String)> {
        self.store
            .list()
            .into_iter()
            .map(|rec| {
                let namespace = self.display_namespace(&rec);
                (rec.name, namespace)
            })
            .collect()
    }

    /// Record-only update summary for the tab badge and sidebar dot (no Helm
    /// calls). Orphaned apps and names without a catalog entry are skipped.
    pub fn update_summaries(&self) -> (usize, Vec<UpdateSummary>) {
        let Some(catalog) = (self.catalog)() else {
            return (0, Vec::new());
        };
        let mut apps: Vec<UpdateSummary> = self
            .store
            .list()
            .into_iter()
            .filter(|rec| !rec.orphaned)
            .filter_map(|rec| {
                let app = catalog.get(&rec.name).ok()?;
                if !update_available(rec.chart_version.trim(), app.version.trim()) {
                    return None;
                }
                Some(UpdateSummary {
                    name: rec.name.clone(),
                    installed_version: rec.chart_version.trim().to_string(),
                    latest_version: app.version.trim().to_string(),
                })
            })
            .collect();
        apps.sort_by(|a, b| a.name.cmp(&b.name));
        let count = apps.len();
        (count, apps)
    }

    /// Live usage for every recorded app. Never fails: an unavailable kube
    /// client or a kubelet read error is reported as `available:false`.
    pub async fn usage(&self) -> crate::app_usage::UsageReport {
        let Some(kube) = &self.kube else {
            return crate::app_usage::UsageReport::unavailable(
                "no Kubernetes client is configured",
            );
        };
        self.usage.collect(kube, &self.targets()).await
    }

    /// Mask secret values in a record for API responses (returns the keys).
    pub fn redact_record_values(&self, record: &mut Record) -> Vec<String> {
        let keys = match (self.catalog)().and_then(|c| c.get(&record.name).ok()) {
            Some(app) => secret_value_keys(&app.schema),
            None => return Vec::new(),
        };
        let mut redacted: Vec<String> = keys
            .into_iter()
            .filter(|key| {
                record
                    .values
                    .get(key)
                    .map(|v| !v.is_null())
                    .unwrap_or(false)
            })
            .collect();
        for key in &redacted {
            record.values.insert(key.clone(), serde_json::Value::Null);
        }
        redacted.sort();
        redacted
    }

    fn url_for(&self, rec: &Record) -> String {
        if rec.exposure.subdomain.is_empty() {
            return String::new();
        }
        let base = if rec.base_domain.is_empty() {
            self.base_domain.clone()
        } else {
            rec.base_domain.clone()
        };
        if base.is_empty() {
            return String::new();
        }
        let host = format!("{}.{}", rec.exposure.subdomain, base);
        if rec.exposure.tls {
            format!("https://{host}")
        } else {
            format!("http://{host}")
        }
    }
}

/// `Install` request input.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct InstallRequest {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub values: Option<serde_json::Map<String, serde_json::Value>>,
    /// Install-time exposure overrides. A patch (not the persisted struct) so
    /// an omitted field keeps the catalog default instead of deserializing a
    /// bool to `false`/an `internet` default of `true`.
    #[serde(default)]
    pub exposure: Option<ExposurePatch>,
    #[serde(default, rename = "baseDomain")]
    pub base_domain: String,
    /// Registered datasets to attach (D3/F1); empty keeps the app claim-less.
    #[serde(default)]
    pub datasets: Vec<String>,
    /// ZFS pool for the chart's own persistence PVCs; empty uses local-path.
    #[serde(default)]
    pub pool: String,
    /// The operator reviewed the privileged warning. Re-checked at run time.
    #[serde(default, rename = "privilegedConfirmed")]
    pub privileged_confirmed: bool,
    /// Custom environment variables (requires the chart's `env` capability).
    #[serde(default)]
    pub env: Vec<EnvVar>,
}

/// First namespace in `pool` that is neither recorded nor reserved.
fn pick_namespace(pool: &[String], used: &std::collections::HashSet<String>) -> Option<String> {
    pool.iter().find(|ns| !used.contains(*ns)).cloned()
}

/// A persisted limit larger than the provisioned pool (chart downgrade) is
/// clamped so the API never hands out a namespace that does not exist.
fn clamp_pool_limit(limit: usize, provisioned: usize) -> usize {
    if provisioned == 0 {
        limit
    } else {
        limit.clamp(1, provisioned)
    }
}

/// Reject a limit that is outside the provisioned pool or below the apps that
/// are already installed (lowering it further would orphan running slots).
fn validate_pool_limit(limit: usize, provisioned: usize, used: usize) -> Result<(), String> {
    if provisioned == 0 {
        return Err(
            "per-app isolation is disabled; there is no app namespace pool to size".to_string(),
        );
    }
    if limit == 0 || limit > provisioned {
        return Err(format!(
            "the app namespace limit must be between 1 and {provisioned}"
        ));
    }
    if limit < used {
        return Err(format!(
            "{used} app(s) already use a slot; the limit cannot be lower than {used}"
        ));
    }
    Ok(())
}

/// Property names the catalog marks as secrets (`format: password` or
/// `writeOnly: true`), at any nesting depth.
pub fn secret_value_keys(schema: &serde_json::Value) -> std::collections::HashSet<String> {
    fn walk(node: &serde_json::Value, out: &mut std::collections::HashSet<String>) {
        match node {
            serde_json::Value::Object(map) => {
                if let Some(serde_json::Value::Object(props)) = map.get("properties") {
                    for (key, prop) in props {
                        let secret = prop
                            .get("format")
                            .and_then(|f| f.as_str())
                            .map(|f| f == "password")
                            .unwrap_or(false)
                            || prop
                                .get("writeOnly")
                                .and_then(|w| w.as_bool())
                                .unwrap_or(false);
                        if secret {
                            out.insert(key.clone());
                        }
                    }
                }
                for value in map.values() {
                    walk(value, out);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    walk(item, out);
                }
            }
            _ => {}
        }
    }
    let mut out = std::collections::HashSet::new();
    walk(schema, &mut out);
    out
}

fn report(progress: Option<&ProgressFn>, stage: &str, message: &str) {
    if let Some(p) = progress {
        p(stage, message);
    }
}

/// Derive exposure from the catalog defaults, the declared service and the
/// caller's overrides.
pub fn exposure_for(
    app: &crate::catalog::App,
    override_: Option<&ExposurePatch>,
    release: &str,
) -> Exposure {
    let mut e = Exposure {
        subdomain: app.exposure.subdomain.clone(),
        tls: app.exposure.tls,
        auth: app.exposure.auth,
        local_only: app.exposure.local_only,
        protected_paths: app.exposure.protected_paths.clone(),
        internet: app.exposure.internet,
        ..Default::default()
    };
    if !app.services.is_empty() {
        e.service = render_service_name(&app.services[0].name, release);
        e.port = app.services[0].port;
        e.scheme = app.services[0].scheme.clone();
    }
    if let Some(o) = override_ {
        if let Some(v) = &o.subdomain {
            if !v.is_empty() {
                e.subdomain = v.clone();
            }
        }
        if let Some(v) = o.tls {
            e.tls = v;
        }
        if let Some(v) = o.auth {
            e.auth = v;
        }
        if let Some(v) = o.local_only {
            e.local_only = v;
        }
        if let Some(v) = o.internet {
            e.internet = v;
        }
        if let Some(v) = &o.service {
            if !v.is_empty() {
                e.service = render_service_name(v, release);
            }
        }
        if let Some(v) = o.port {
            if v != 0 {
                e.port = v;
            }
        }
        if let Some(v) = &o.scheme {
            e.scheme = v.clone();
        }
    }
    if e.scheme.is_empty() {
        e.scheme = "http".to_string();
    }
    e
}

/// Whether an install may pre-create the Internet egress policy: the app must
/// want it and run in a dedicated baseline namespace. The shared apps
/// namespace and privileged namespaces are excluded because the policy selects
/// every pod in its namespace.
pub fn preinstall_internet(namespace: &str, exposure: &Exposure, baseline_pool: &[String]) -> bool {
    exposure.internet && baseline_pool.iter().any(|n| n == namespace)
}

/// NetworkPolicy names from a `kubectl -o json` list that the API created for
/// an app with no record in the namespace. Policies without the API labels are
/// ignored so an operator-managed policy is never deleted.
pub fn orphan_internet_policies(
    list: &serde_json::Value,
    active: &std::collections::HashSet<String>,
) -> Vec<String> {
    let Some(items) = list.get("items").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in items {
        let meta = item.get("metadata");
        let labels = meta.and_then(|m| m.get("labels"));
        let managed = labels
            .and_then(|l| l.get(MANAGED_BY_LABEL))
            .and_then(|v| v.as_str());
        let app = labels
            .and_then(|l| l.get(APP_LABEL))
            .and_then(|v| v.as_str());
        let name = meta.and_then(|m| m.get("name")).and_then(|v| v.as_str());
        let (Some(app), Some(name)) = (app, name) else {
            continue;
        };
        if managed != Some(MANAGED_BY_VALUE) || name != internet_policy_name(app) {
            continue;
        }
        if !active.contains(app) {
            out.push(name.to_string());
        }
    }
    out
}

/// Coerce numeric strings for schema fields typed `integer`/`number`. Browser
/// forms submit every input as a string, and the catalog schema rejects
/// `"10001"` for an integer; doing this server-side keeps older UI builds and
/// direct API clients working.
pub fn coerce_schema_numbers(
    schema: &serde_json::Value,
    values: &mut serde_json::Map<String, serde_json::Value>,
) {
    let Some(properties) = schema.get("properties").and_then(|p| p.as_object()) else {
        return;
    };
    for (key, property) in properties {
        let Some(kind) = property.get("type").and_then(|t| t.as_str()) else {
            continue;
        };
        let Some(value) = values.get_mut(key) else {
            continue;
        };
        let Some(raw) = value.as_str() else {
            continue;
        };
        match kind {
            "integer" => {
                if let Ok(parsed) = raw.trim().parse::<i64>() {
                    *value = serde_json::json!(parsed);
                }
            }
            "number" => {
                if let Ok(parsed) = raw.trim().parse::<f64>() {
                    *value = serde_json::json!(parsed);
                }
            }
            _ => {}
        }
    }
}

/// Deep-merge overlay into base and return a new map.
/// Validate the merged install values against the catalog's JSON schema
/// (SEC-2026-D7). A missing/null schema means the catalog does not constrain
/// the values; an invalid schema is a catalog defect and fails closed.
fn validate_install_values(
    app: &crate::catalog::App,
    values: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    if app.schema.is_null() {
        return Ok(());
    }
    let validator = jsonschema::validator_for(&app.schema)
        .map_err(|e| format!("app {:?} has an invalid values schema: {e}", app.name))?;
    let instance = serde_json::Value::Object(values.clone());
    let errors: Vec<String> = validator
        .iter_errors(&instance)
        .map(|e| e.to_string())
        .take(8)
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "invalid values for {:?}: {}",
            app.name,
            errors.join("; ")
        ))
    }
}

/// The container resource maps inside a chart's `resources` value: the value
/// itself for flat charts, or each component entry for multi-container charts.
fn resource_groups(
    resources: &serde_json::Map<String, serde_json::Value>,
) -> Vec<(&str, &serde_json::Map<String, serde_json::Value>)> {
    let has_sections = |entry: &serde_json::Map<String, serde_json::Value>| {
        entry.get("requests").is_some_and(|v| v.is_object())
            || entry.get("limits").is_some_and(|v| v.is_object())
    };
    if has_sections(resources) {
        return vec![("resources", resources)];
    }
    resources
        .iter()
        .filter_map(|(key, value)| {
            value
                .as_object()
                .filter(|entry| has_sections(entry))
                .map(|entry| (key.as_str(), entry))
        })
        .collect()
}

/// A Kubernetes quantity in the forms the catalog charts use: a non-negative
/// decimal number with an optional decimal (`m`, `k`, `M`, …) or binary
/// (`Ki`, `Mi`, `Gi`, …) suffix, returned in base units (cores or bytes).
fn quantity_number(raw: &str) -> Option<f64> {
    let text = raw.trim();
    let number_end = text
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_digit() || *c == '.' || *c == '+' || *c == '-'))
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    let (number, suffix) = text.split_at(number_end);
    let parsed = number.parse::<f64>().ok()?;
    if !parsed.is_finite() || parsed < 0.0 {
        return None;
    }
    let multiplier = match suffix {
        "" => 1.0,
        "n" => 1e-9,
        "u" => 1e-6,
        "m" => 1e-3,
        "k" => 1e3,
        "M" => 1e6,
        "G" => 1e9,
        "T" => 1e12,
        "P" => 1e15,
        "E" => 1e18,
        "Ki" => 1024.0,
        "Mi" => 1024.0 * 1024.0,
        "Gi" => 1024.0 * 1024.0 * 1024.0,
        "Ti" => 1024f64.powi(4),
        "Pi" => 1024f64.powi(5),
        "Ei" => 1024f64.powi(6),
        _ => return None,
    };
    Some(parsed * multiplier)
}

fn is_valid_quantity(raw: &str) -> bool {
    quantity_number(raw).is_some()
}

/// The aggregate resource demand of a chart's `resources` value, in cores and
/// bytes. Multi-container charts sum every component group.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct ResourceTotals {
    requests_cpu: f64,
    requests_memory: f64,
    limits_cpu: f64,
    limits_memory: f64,
}

fn resource_totals(values: &serde_json::Map<String, serde_json::Value>) -> ResourceTotals {
    let mut totals = ResourceTotals::default();
    let Some(resources) = values.get("resources").and_then(|v| v.as_object()) else {
        return totals;
    };
    for (_, entry) in resource_groups(resources) {
        for section in ["requests", "limits"] {
            let Some(quantities) = entry.get(section).and_then(|v| v.as_object()) else {
                continue;
            };
            for (field, raw) in quantities {
                if field != "cpu" && field != "memory" {
                    continue;
                }
                let value = match raw {
                    serde_json::Value::String(s) => quantity_number(s),
                    serde_json::Value::Number(n) => n.as_f64(),
                    _ => None,
                };
                let Some(value) = value else {
                    continue;
                };
                match (section, field.as_str()) {
                    ("requests", "cpu") => totals.requests_cpu += value,
                    ("requests", "memory") => totals.requests_memory += value,
                    ("limits", "cpu") => totals.limits_cpu += value,
                    ("limits", "memory") => totals.limits_memory += value,
                    _ => {}
                }
            }
        }
    }
    totals
}

fn format_cores(cores: f64) -> String {
    if cores >= 1.0 {
        let rounded = (cores * 1000.0).round() / 1000.0;
        format!("{rounded}")
    } else {
        format!("{}m", (cores * 1000.0).round() as i64)
    }
}

fn format_bytes(bytes: f64) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    if bytes >= GIB {
        format!("{:.2}Gi", bytes / GIB)
    } else {
        format!("{:.0}Mi", bytes / MIB)
    }
}

/// Reject invalid CPU/memory quantities before Helm hands them to Kubernetes,
/// where the failure would surface as an opaque `--wait` timeout. Other keys
/// under `requests`/`limits` (e.g. `ephemeral-storage`) are left to Kubernetes.
pub fn validate_resource_quantities(
    values: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    let Some(resources) = values.get("resources").and_then(|v| v.as_object()) else {
        return Ok(());
    };
    for (group, entry) in resource_groups(resources) {
        for section in ["requests", "limits"] {
            let Some(quantities) = entry.get(section).and_then(|v| v.as_object()) else {
                continue;
            };
            for (field, raw) in quantities {
                if field != "cpu" && field != "memory" {
                    continue;
                }
                let text = match raw {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    _ => {
                        return Err(format!(
                        "resources {group}.{section}.{field} must be a quantity string or number"
                    ))
                    }
                };
                if !is_valid_quantity(&text) {
                    return Err(format!(
                        "resources {group}.{section}.{field} has invalid quantity {text:?}; use a Kubernetes quantity such as 100m, 0.5 or 256Mi"
                    ));
                }
            }
        }
    }
    Ok(())
}

pub fn merge_values(
    base: &serde_json::Value,
    overlay: Option<&serde_json::Map<String, serde_json::Value>>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut out = match base {
        serde_json::Value::Object(m) => m.clone(),
        _ => serde_json::Map::new(),
    };
    if let Some(overlay) = overlay {
        for (k, v) in overlay {
            if let (
                Some(serde_json::Value::Object(existing)),
                serde_json::Value::Object(incoming),
            ) = (out.get(k), v)
            {
                out.insert(
                    k.clone(),
                    serde_json::Value::Object(merge_values(
                        &serde_json::Value::Object(existing.clone()),
                        Some(incoming),
                    )),
                );
                continue;
            }
            out.insert(k.clone(), v.clone());
        }
    }
    out
}

/// The PV/PVC name for one app/dataset pair.
pub fn claim_name(app: &str, dataset: &str) -> String {
    format!("naslos-ds-{app}-{dataset}")
}

/// Trim and dedupe a dataset selection, preserving order. An empty name is a
/// client error; registry existence is checked by the provisioning step.
pub fn normalize_dataset_names(selected: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for raw in selected {
        let name = raw.trim();
        if name.is_empty() {
            return Err("dataset names must not be empty".to_string());
        }
        if !out.iter().any(|existing| existing == name) {
            out.push(name.to_string());
        }
    }
    Ok(out)
}

/// Remove the Helm values the API owns from client-supplied values.
pub fn strip_dataset_values(values: &mut serde_json::Map<String, serde_json::Value>) {
    for key in PLATFORM_DATASET_KEYS {
        values.remove(key);
    }
    for key in PLATFORM_ENV_KEYS {
        values.remove(key);
    }
}

/// Build the Helm values for a dataset selection: one entry per dataset plus
/// the legacy single-claim keys (baseline) or the first host path (privileged)
/// so older chart releases keep working.
pub fn dataset_helm_values(
    app: &str,
    privileged: bool,
    names: &[String],
    registry: &crate::datasets::Registry,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let mut values = serde_json::Map::new();
    if names.is_empty() {
        return Ok(values);
    }
    let mut entries = Vec::new();
    let mut claims = Vec::new();
    let mut first_host_path: Option<String> = None;
    for name in names {
        let record = registry
            .get(name)
            .ok_or_else(|| format!("dataset {name:?} is not registered"))?;
        let mount_path = format!("/datasets/{name}");
        if privileged {
            if first_host_path.is_none() {
                first_host_path = Some(record.host_path.clone());
            }
            entries.push(serde_json::json!({
                "name": name,
                "mountPath": mount_path,
                "hostPath": record.host_path,
            }));
        } else {
            let claim = claim_name(app, name);
            claims.push(serde_json::json!(claim));
            entries.push(serde_json::json!({
                "name": name,
                "mountPath": mount_path,
                "claim": claim,
            }));
        }
    }
    values.insert("datasets".to_string(), serde_json::Value::Array(entries));
    if privileged {
        if let Some(host_path) = first_host_path {
            values.insert("hostPath".to_string(), serde_json::Value::String(host_path));
        }
    } else {
        values.insert(
            "datasetsClaim".to_string(),
            claims.first().cloned().unwrap_or_default(),
        );
        values.insert(
            "datasetsClaims".to_string(),
            serde_json::Value::Array(claims),
        );
    }
    Ok(values)
}

/// Whether a chart tree references the platform `datasets` value. A chart must
/// be updated before the startup migration rewrites its mount layout.
pub fn chart_tree_mentions_datasets(root: &std::path::Path) -> bool {
    // `.Values.datasets` must not match the legacy `.Values.datasetsClaim`.
    fn mentions(data: &str) -> bool {
        const NEEDLE: &str = ".Values.datasets";
        let mut rest = data;
        while let Some(idx) = rest.find(NEEDLE) {
            let after = &rest[idx + NEEDLE.len()..];
            match after.chars().next() {
                Some(c) if c.is_alphanumeric() || c == '_' => rest = after,
                _ => return true,
            }
        }
        false
    }
    fn walk(dir: &std::path::Path) -> bool {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return false;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if walk(&path) {
                    return true;
                }
                continue;
            }
            if let Ok(data) = std::fs::read_to_string(&path) {
                if mentions(&data) {
                    return true;
                }
            }
        }
        false
    }
    walk(root)
}

/// What one `provision_datasets` call created, so a failed Helm run only rolls
/// back that call's work.
#[derive(Debug, Default)]
pub struct DatasetProvision {
    pub values: serde_json::Map<String, serde_json::Value>,
    /// `(kind, name, namespace)` for every PV/PVC this call created.
    pub created: Vec<(String, String, String)>,
    /// Registry datasets this call attached for the first time.
    pub newly_attached: Vec<String>,
}

pub fn validate_release_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("app name is required".to_string());
    }
    if name.len() > 53 {
        return Err(format!("app name {name:?} must be 53 characters or fewer"));
    }
    if !is_dns1123_label(name) {
        return Err(format!(
            "app name {name:?} must be a lowercase DNS-1123 label"
        ));
    }
    Ok(())
}

/// Refuse an Install for a release the platform already records. The caller
/// must reconfigure the existing app; reinstalling it would also discard its
/// stored values and, when the namespace pool is full, fail with the wrong
/// error.
fn ensure_not_installed(store: &Store, name: &str) -> Result<(), String> {
    if store.get(name).is_ok() {
        return Err(format!(
            "app {name:?} is already installed; reconfigure or uninstall it instead"
        ));
    }
    Ok(())
}

pub fn validate_subdomain(subdomain: &str) -> Result<(), String> {
    if subdomain.is_empty() || is_dns1123_label(subdomain) {
        Ok(())
    } else {
        Err(format!(
            "subdomain {subdomain:?} must be a lowercase DNS-1123 label"
        ))
    }
}

/// Persists records as JSON.
pub struct Store {
    path: String,
    records: Mutex<HashMap<String, Record>>,
}

impl Store {
    pub fn new(path: &str) -> Self {
        Self {
            path: path.to_string(),
            records: Mutex::new(HashMap::new()),
        }
    }

    pub fn load(&mut self) -> Result<(), String> {
        if self.path.is_empty() {
            return Ok(());
        }
        let data = match std::fs::read(&self.path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(format!("reading apps file: {e}")),
        };
        let list: Vec<Record> =
            serde_json::from_slice(&data).map_err(|e| format!("parsing apps file: {e}"))?;
        let mut guard = self.records.lock().unwrap();
        for mut rec in list {
            if rec.name.is_empty() {
                continue;
            }
            rec.values = merge_values(&serde_json::Value::Object(rec.values), None);
            guard.insert(rec.name.clone(), rec);
        }
        Ok(())
    }

    pub fn list(&self) -> Vec<Record> {
        let guard = self.records.lock().unwrap();
        let mut out: Vec<Record> = guard.values().map(clone_record).collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    pub fn get(&self, name: &str) -> Result<Record, String> {
        self.records
            .lock()
            .unwrap()
            .get(name)
            .map(clone_record)
            .ok_or_else(|| format!("app {name:?} not found"))
    }

    pub fn upsert(&self, rec: Record) -> Result<(), String> {
        let mut guard = self.records.lock().unwrap();
        guard.insert(rec.name.clone(), clone_record(&rec));
        self.save_locked(&guard)
    }

    pub fn update(&self, name: &str, mutate: impl FnOnce(&mut Record)) -> Result<Record, String> {
        let mut guard = self.records.lock().unwrap();
        let rec = guard
            .get_mut(name)
            .ok_or_else(|| format!("app {name:?} not found"))?;
        mutate(rec);
        let out = clone_record(rec);
        self.save_locked(&guard)?;
        Ok(out)
    }

    pub fn delete(&self, name: &str) -> Result<(), String> {
        let mut guard = self.records.lock().unwrap();
        if guard.remove(name).is_none() {
            return Err(format!("app {name:?} not found"));
        }
        self.save_locked(&guard)
    }

    fn save_locked(&self, guard: &HashMap<String, Record>) -> Result<(), String> {
        if self.path.is_empty() {
            return Ok(());
        }
        let mut list: Vec<&Record> = guard.values().collect();
        list.sort_by(|a, b| a.name.cmp(&b.name));
        let mut data =
            serde_json::to_vec_pretty(&list).map_err(|e| format!("encoding apps: {e}"))?;
        data.push(b'\n');
        crate::chartsrepo::write_atomic(&self.path, &data, 0o600, 0o750)
    }
}

fn clone_record(rec: &Record) -> Record {
    let mut dup = rec.clone();
    dup.values = merge_values(&serde_json::Value::Object(rec.values.clone()), None);
    dup
}

/// The default apps state path (overridable by `APPS_CONFIG`).
pub fn default_store_path() -> String {
    std::env::var("APPS_CONFIG").unwrap_or_else(|_| "/var/lib/naslos/apps.json".to_string())
}

/// Persisted owner setting for how many provisioned baseline slots are usable.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct PoolSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    limit: Option<usize>,
}

/// Read the saved limit. A missing file means "use the chart default"; a
/// corrupt file is ignored (with a warning) rather than failing startup.
fn load_pool_settings(path: &str) -> Option<usize> {
    if path.is_empty() {
        return None;
    }
    let data = std::fs::read(path).ok()?;
    match serde_json::from_slice::<PoolSettings>(&data) {
        Ok(settings) => settings.limit,
        Err(e) => {
            tracing::warn!("ignoring app namespace settings {path}: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_name_validation() {
        assert!(validate_release_name("nginx").is_ok());
        assert!(validate_release_name("").is_err());
        assert!(validate_release_name("Bad_Name").is_err());
        assert!(validate_release_name(&"a".repeat(54)).is_err());
    }

    #[test]
    fn update_available_compares_versions() {
        assert!(update_available("0.1.2", "0.1.3"));
        assert!(!update_available("0.1.3", "0.1.2"));
        assert!(!update_available("0.1.3", "0.1.3"));
        // A downgrade in the catalog is not an update.
        assert!(!update_available("v0.2.0", "0.1.9"));
        // `v` prefix and whitespace are tolerated on both sides.
        assert!(update_available(" v0.1.2 ", "0.1.3"));
        // Pre-release chart versions use semver precedence: a release is
        // newer than its rc, but a lower rc is not newer than a higher one.
        assert!(update_available("0.1.3-rc1", "0.1.3"));
        assert!(!update_available("0.1.3", "0.1.3-rc1"));
        assert!(update_available("0.1.3-rc1", "0.1.3-rc2"));
        assert!(!update_available("0.1.3-rc1", "v0.1.3-rc1"));
        // Build metadata never changes precedence.
        assert!(!update_available("0.1.3+abc", "0.1.3"));
        // Unknown versions never report an update.
        assert!(!update_available("", "0.1.3"));
        assert!(!update_available("0.1.2", ""));
        assert!(!update_available("", ""));
    }

    #[test]
    fn enrich_view_uses_catalog_and_live_version() {
        let app = crate::catalog::App {
            name: "jellyfin".to_string(),
            display_name: "Jellyfin".to_string(),
            icon: "🎬".to_string(),
            icon_url: "https://example.test/jellyfin.png".to_string(),
            version: "0.1.3".to_string(),
            source: "official".to_string(),
            channel: "Prod".to_string(),
            ..Default::default()
        };
        let mut view = View {
            record: Record {
                name: "jellyfin".to_string(),
                chart_version: "0.1.2".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        enrich_view(&mut view, Some("0.1.2"), Some(&app));
        assert_eq!(view.display_name, "Jellyfin");
        assert_eq!(view.icon, "🎬");
        assert_eq!(view.icon_url, "https://example.test/jellyfin.png");
        assert_eq!(view.installed_version, "0.1.2");
        assert_eq!(view.latest_version, "0.1.3");
        assert_eq!(view.latest_source, "official");
        assert_eq!(view.latest_channel, "Prod");
        assert!(view.update_available);

        // No catalog entry: fall back to the release name, never a badge.
        let mut view = View {
            record: Record {
                name: "orphan".to_string(),
                chart_version: "9.9.9".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        enrich_view(&mut view, None, None);
        assert_eq!(view.display_name, "orphan");
        assert_eq!(view.installed_version, "9.9.9");
        assert!(!view.update_available);

        // An orphaned record never advertises an update even with a catalog hit.
        let mut view = View {
            record: Record {
                name: "jellyfin".to_string(),
                chart_version: "0.1.2".to_string(),
                orphaned: true,
                ..Default::default()
            },
            ..Default::default()
        };
        enrich_view(&mut view, None, Some(&app));
        assert!(!view.update_available);
    }

    #[test]
    fn duplicate_install_is_rejected() {
        let store = Store::new("");
        assert!(ensure_not_installed(&store, "bazarr").is_ok());
        store
            .upsert(Record {
                name: "bazarr".to_string(),
                ..Record::default()
            })
            .unwrap();
        let err = ensure_not_installed(&store, "bazarr").unwrap_err();
        assert!(err.contains("already installed"), "{err}");
    }

    #[test]
    fn records_without_an_env_list_still_load() {
        let rec: Record = serde_json::from_str(r#"{"name":"radarr","values":{}}"#).unwrap();
        assert!(rec.env.is_empty());
        let round = serde_json::to_string(&rec).unwrap();
        assert!(
            !round.contains("\"env\""),
            "empty env stays out of the JSON"
        );
    }

    #[test]
    fn subdomain_validation() {
        assert!(validate_subdomain("").is_ok());
        assert!(validate_subdomain("media").is_ok());
        assert!(validate_subdomain("Bad").is_err());
    }

    #[test]
    fn preinstall_internet_only_for_dedicated_baseline_namespaces() {
        let pool = vec!["naslos-app-1".to_string(), "naslos-app-2".to_string()];
        let on = Exposure::default();
        assert!(preinstall_internet("naslos-app-1", &on, &pool));
        // Shared and privileged namespaces are namespace-scoped selectors.
        assert!(!preinstall_internet("naslos-apps", &on, &pool));
        assert!(!preinstall_internet("naslos-apps-priv", &on, &pool));
        // An Internet opt-out must not create the policy.
        let off = Exposure {
            internet: false,
            ..Exposure::default()
        };
        assert!(!preinstall_internet("naslos-app-1", &off, &pool));
    }

    #[test]
    fn orphan_internet_policy_sweep_ignores_unlabeled_and_active_policies() {
        let list = serde_json::json!({"items": [
            {"metadata": {"name": "old-internet", "labels": {
                MANAGED_BY_LABEL: MANAGED_BY_VALUE, APP_LABEL: "old"}}},
            {"metadata": {"name": "live-internet", "labels": {
                MANAGED_BY_LABEL: MANAGED_BY_VALUE, APP_LABEL: "live"}}},
            {"metadata": {"name": "manual-internet", "labels": {}}},
            {"metadata": {"name": "old-misnamed", "labels": {
                MANAGED_BY_LABEL: MANAGED_BY_VALUE, APP_LABEL: "old"}}}
        ]});
        let active = std::collections::HashSet::from(["live".to_string()]);
        assert_eq!(
            orphan_internet_policies(&list, &active),
            vec!["old-internet".to_string()]
        );
    }

    #[test]
    fn merge_values_deep_merges() {
        let base = serde_json::json!({ "a": 1, "nested": { "x": 1, "y": 2 } });
        let overlay = serde_json::json!({ "b": 2, "nested": { "y": 9, "z": 3 } });
        let merged = merge_values(&base, overlay.as_object());
        assert_eq!(merged["a"], 1);
        assert_eq!(merged["b"], 2);
        assert_eq!(merged["nested"]["x"], 1);
        assert_eq!(merged["nested"]["y"], 9);
        assert_eq!(merged["nested"]["z"], 3);
    }

    #[test]
    fn render_service_name_resolves_template() {
        assert_eq!(render_service_name("{{ .Release.Name }}", "nginx"), "nginx");
        assert_eq!(render_service_name("web", "nginx"), "web");
    }

    #[test]
    fn store_round_trips_and_sorts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("apps.json");
        let store = Store::new(&path.display().to_string());
        for name in ["zeta", "alpha"] {
            store
                .upsert(Record {
                    name: name.into(),
                    ..Default::default()
                })
                .unwrap();
        }
        let names: Vec<String> = store.list().into_iter().map(|r| r.name).collect();
        assert_eq!(names, vec!["alpha", "zeta"]);

        let reloaded = Store::new(&path.display().to_string());
        let mut reloaded = reloaded;
        reloaded.load().unwrap();
        assert_eq!(reloaded.list().len(), 2);
    }
}

#[cfg(test)]
mod exposure_patch_tests {
    use super::{exposure_for, Exposure, ExposurePatch};

    #[test]
    fn omitted_fields_keep_the_stored_value() {
        let current = Exposure {
            subdomain: "plex".into(),
            tls: true,
            auth: true,
            local_only: false,
            internet: true,
            service: "plex".into(),
            port: 32400,
            scheme: "http".into(),
            ..Exposure::default()
        };
        let patch: ExposurePatch = serde_json::from_str(r#"{"subdomain":"media"}"#).unwrap();
        let merged = patch.apply(&current);
        assert_eq!(merged.subdomain, "media");
        assert!(merged.tls, "TLS must not be disabled by an omitted field");
        assert!(merged.auth, "auth must not be disabled by an omitted field");
        assert!(merged.internet, "Internet must stay on by default");
        assert_eq!(merged.port, 32400);
    }

    #[test]
    fn internet_defaults_on_and_can_be_disabled() {
        assert!(Exposure::default().internet);
        let current = Exposure::default();
        let patch: ExposurePatch = serde_json::from_str(r#"{"internet":false}"#).unwrap();
        assert!(!patch.apply(&current).internet);
        let merged: Exposure = serde_json::from_str(r#"{"subdomain":"search"}"#).unwrap();
        assert!(
            merged.internet,
            "a record without the field keeps Internet on"
        );
    }

    #[test]
    fn install_patch_keeps_the_catalog_internet_default() {
        let app = crate::catalog::App {
            exposure: crate::catalog::ExposureDefaults {
                internet: false,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(!exposure_for(&app, None, "x").internet);
        // A partial install patch must not force the serde default (true).
        let patch: ExposurePatch = serde_json::from_str(r#"{"subdomain":"search"}"#).unwrap();
        assert!(!exposure_for(&app, Some(&patch), "x").internet);
        let patch: ExposurePatch = serde_json::from_str(r#"{"internet":true}"#).unwrap();
        assert!(exposure_for(&app, Some(&patch), "x").internet);
    }

    #[test]
    fn protected_paths_are_copied_and_patched_as_a_set() {
        let app = crate::catalog::App {
            exposure: crate::catalog::ExposureDefaults {
                protected_paths: vec!["/preferences".into(), "/stats".into()],
                ..Default::default()
            },
            ..Default::default()
        };
        let base = exposure_for(&app, None, "x");
        assert_eq!(base.protected_paths, vec!["/preferences", "/stats"]);

        let patch: ExposurePatch =
            serde_json::from_str(r#"{"protectedPaths":["/preferences"]}"#).unwrap();
        assert_eq!(
            patch.apply(&base).protected_paths,
            vec!["/preferences".to_string()]
        );

        // Omitted protectedPaths keeps the stored set.
        let patch: ExposurePatch = serde_json::from_str(r#"{"subdomain":"search"}"#).unwrap();
        assert_eq!(patch.apply(&base).protected_paths, base.protected_paths);
    }
}

#[cfg(test)]
mod value_schema_tests {
    use super::{
        clamp_pool_limit, load_pool_settings, pick_namespace, pool_storage_class,
        secret_value_keys, set_persistence_storage_class, validate_install_values,
        validate_pool_for_storage, validate_pool_limit,
    };
    use crate::catalog::App;
    use serde_json::json;

    fn app_with_schema() -> App {
        App {
            name: "demo".into(),
            schema: json!({
                "type": "object",
                "properties": { "port": { "type": "integer", "minimum": 1 } },
                "required": ["port"]
            }),
            ..Default::default()
        }
    }

    #[test]
    fn valid_values_pass() {
        let mut values = serde_json::Map::new();
        values.insert("port".into(), json!(8080));
        assert!(validate_install_values(&app_with_schema(), &values).is_ok());
    }

    #[test]
    fn missing_or_wrong_type_is_refused() {
        let empty = serde_json::Map::new();
        assert!(validate_install_values(&app_with_schema(), &empty).is_err());
        let mut bad = serde_json::Map::new();
        bad.insert("port".into(), json!("eighty"));
        let err = validate_install_values(&app_with_schema(), &bad).unwrap_err();
        assert!(err.contains("invalid values"), "{err}");
    }

    #[test]
    fn a_null_schema_is_unconstrained() {
        let app = App {
            name: "demo".into(),
            schema: serde_json::Value::Null,
            ..Default::default()
        };
        assert!(validate_install_values(&app, &serde_json::Map::new()).is_ok());
    }

    #[test]
    fn catalog_secret_fields_are_detected() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "password": {"type": "string", "format": "password"},
                "nested": {"type": "object", "properties": {"token": {"writeOnly": true}}},
                "user": {"type": "string"}
            }
        });
        let keys = secret_value_keys(&schema);
        assert!(keys.contains("password"));
        assert!(keys.contains("token"));
        assert!(!keys.contains("user"));
    }

    #[test]
    fn namespace_selection_skips_records_and_reservations() {
        let pool = vec![
            "naslos-apps-1".to_string(),
            "naslos-apps-2".to_string(),
            "naslos-apps-3".to_string(),
        ];
        let mut used = std::collections::HashSet::new();
        used.insert("naslos-apps-1".to_string());
        used.insert("naslos-apps-2".to_string());
        assert_eq!(
            pick_namespace(&pool, &used).as_deref(),
            Some("naslos-apps-3")
        );
        used.insert("naslos-apps-3".to_string());
        assert_eq!(pick_namespace(&pool, &used), None);
    }

    #[test]
    fn pool_limit_is_clamped_to_the_provisioned_pool() {
        assert_eq!(clamp_pool_limit(0, 20), 1);
        assert_eq!(clamp_pool_limit(7, 20), 7);
        assert_eq!(clamp_pool_limit(50, 20), 20);
    }

    #[test]
    fn pool_limit_validation_rejects_out_of_range_and_below_used() {
        assert!(validate_pool_limit(0, 20, 0).is_err());
        assert!(validate_pool_limit(21, 20, 0).is_err());
        assert!(validate_pool_limit(3, 20, 4).is_err());
        assert!(validate_pool_limit(4, 20, 4).is_ok());
        assert!(validate_pool_limit(20, 20, 4).is_ok());
        assert!(validate_pool_limit(5, 0, 0).is_err());
    }

    #[test]
    fn pool_settings_round_trip_and_tolerate_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app-pool.json");
        let path = path.to_str().unwrap();
        assert_eq!(load_pool_settings(path), None);
        crate::chartsrepo::write_atomic(path, br#"{"limit":17}"#, 0o600, 0o750).unwrap();
        assert_eq!(load_pool_settings(path), Some(17));
        std::fs::write(path, b"not json").unwrap();
        assert_eq!(load_pool_settings(path), None);
    }

    #[test]
    fn pool_storage_class_is_safe_and_case_aware() {
        let data = pool_storage_class("Data");
        let lower = pool_storage_class("data");
        assert!(data.starts_with("naslos-pool-data-"));
        assert!(lower.starts_with("naslos-pool-data-"));
        assert_ne!(data, lower);
        assert!(data.len() <= 63);
        assert!(data
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'));
        assert!(validate_pool_for_storage("Data").is_ok());
        assert!(validate_pool_for_storage("a/b").is_err());
        assert!(validate_pool_for_storage("../etc").is_err());
    }

    #[test]
    fn persistence_storage_class_covers_top_level_and_nested() {
        let mut values = serde_json::json!({
            "persistence": {
                "data": { "size": "2Gi", "storageClass": "" },
                "config": { "size": "1Gi" }
            },
            "postgres": {
                "persistence": { "storageClass": "" }
            },
            "other": { "storageClass": "keep-me" }
        });
        set_persistence_storage_class(&mut values, "naslos-pool-ssd");
        assert_eq!(
            values["persistence"]["data"]["storageClass"],
            "naslos-pool-ssd"
        );
        assert_eq!(
            values["persistence"]["config"]["storageClass"],
            "naslos-pool-ssd"
        );
        assert_eq!(
            values["postgres"]["persistence"]["storageClass"],
            "naslos-pool-ssd"
        );
        assert_eq!(values["other"]["storageClass"], "keep-me");
    }
}

#[cfg(test)]
mod dataset_layout_tests {
    use super::{
        chart_tree_mentions_datasets, claim_name, dataset_helm_values, normalize_dataset_names,
        strip_dataset_values,
    };
    use crate::datasets::{ManagedDataset, Registry};
    use serde_json::json;

    fn registry(names: &[&str]) -> Registry {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::new(&dir.path().join("d.json").display().to_string(), "");
        for name in names {
            registry
                .add(ManagedDataset {
                    name: (*name).into(),
                    host_path: format!("/var/mnt/tank/{name}"),
                    shared: false,
                    warning_acknowledged: false,
                    created_at: None,
                    attached_to: Vec::new(),
                })
                .unwrap();
        }
        std::mem::forget(dir);
        registry
    }

    #[test]
    fn selection_is_trimmed_deduped_and_order_preserving() {
        let selected = vec![
            " media ".to_string(),
            "downloads".to_string(),
            "media".to_string(),
        ];
        assert_eq!(
            normalize_dataset_names(&selected).unwrap(),
            vec!["media", "downloads"]
        );
        assert!(normalize_dataset_names(&["  ".to_string()]).is_err());
    }

    #[test]
    fn claim_names_use_the_platform_prefix() {
        assert_eq!(claim_name("sonarr", "media"), "naslos-ds-sonarr-media");
    }

    #[test]
    fn baseline_entries_carry_a_claim_per_dataset() {
        let registry = registry(&["media", "downloads"]);
        let values = dataset_helm_values(
            "sonarr",
            false,
            &["media".to_string(), "downloads".to_string()],
            &registry,
        )
        .unwrap();
        let entries = values["datasets"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["mountPath"], "/datasets/media");
        assert_eq!(entries[0]["claim"], "naslos-ds-sonarr-media");
        assert_eq!(entries[1]["claim"], "naslos-ds-sonarr-downloads");
        assert_eq!(values["datasetsClaim"], "naslos-ds-sonarr-media");
        assert_eq!(values["datasetsClaims"].as_array().unwrap().len(), 2);
        assert!(values.get("hostPath").is_none());
    }

    #[test]
    fn privileged_entries_carry_the_host_path_and_legacy_value() {
        let registry = registry(&["media", "downloads"]);
        let values = dataset_helm_values(
            "jackett",
            true,
            &["media".to_string(), "downloads".to_string()],
            &registry,
        )
        .unwrap();
        let entries = values["datasets"].as_array().unwrap();
        assert_eq!(entries[0]["hostPath"], "/var/mnt/tank/media");
        assert_eq!(entries[1]["mountPath"], "/datasets/downloads");
        assert_eq!(values["hostPath"], "/var/mnt/tank/media");
        assert!(values.get("datasetsClaim").is_none());
    }

    #[test]
    fn an_unregistered_dataset_is_refused() {
        let registry = registry(&["media"]);
        let err =
            dataset_helm_values("sonarr", false, &["missing".to_string()], &registry).unwrap_err();
        assert!(err.contains("not registered"), "{err}");
    }

    #[test]
    fn platform_keys_are_stripped_from_client_values() {
        let mut values = serde_json::Map::new();
        values.insert("port".into(), json!(8080));
        values.insert("datasets".into(), json!([{"name": "evil"}]));
        values.insert("datasetsClaim".into(), json!("someone-elses-claim"));
        values.insert("datasetsClaims".into(), json!(["x"]));
        strip_dataset_values(&mut values);
        assert_eq!(values["port"], 8080);
        assert!(values.get("datasets").is_none());
        assert!(values.get("datasetsClaim").is_none());
        assert!(values.get("datasetsClaims").is_none());
    }

    #[test]
    fn chart_support_scanner_reads_the_template_tree() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("templates")).unwrap();
        std::fs::write(
            dir.path().join("templates/app.yaml"),
            "volumeMounts:\n{{- range .Values.datasets }}\n",
        )
        .unwrap();
        assert!(chart_tree_mentions_datasets(dir.path()));

        let legacy = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(legacy.path().join("templates")).unwrap();
        std::fs::write(
            legacy.path().join("templates/app.yaml"),
            "claimName: {{ .Values.datasetsClaim }}\n",
        )
        .unwrap();
        assert!(!chart_tree_mentions_datasets(legacy.path()));
    }
}

#[cfg(test)]
mod value_coercion_tests {
    use super::coerce_schema_numbers;
    use serde_json::json;

    #[test]
    fn numeric_strings_are_coerced_for_integer_and_number_fields() {
        let schema = json!({
            "type": "object",
            "properties": {
                "puid": { "type": "integer" },
                "ratio": { "type": "number" },
                "name": { "type": "string" }
            }
        });
        let mut values = serde_json::Map::new();
        values.insert("puid".into(), json!("10001"));
        values.insert("ratio".into(), json!("1.5"));
        values.insert("name".into(), json!("42"));
        coerce_schema_numbers(&schema, &mut values);
        assert_eq!(values["puid"], json!(10001));
        assert_eq!(values["ratio"], json!(1.5));
        assert_eq!(values["name"], json!("42"));
    }

    #[test]
    fn unparseable_numeric_strings_stay_for_schema_validation() {
        let schema = json!({
            "type": "object",
            "properties": { "pgid": { "type": "integer" } }
        });
        let mut values = serde_json::Map::new();
        values.insert("pgid".into(), json!("not-a-number"));
        coerce_schema_numbers(&schema, &mut values);
        assert_eq!(values["pgid"], json!("not-a-number"));
    }
}

#[cfg(test)]
mod resource_quantity_tests {
    use super::{quantity_number, resource_totals, validate_resource_quantities};
    use serde_json::json;

    fn values(resources: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        let mut map = serde_json::Map::new();
        map.insert("resources".into(), resources);
        map
    }

    #[test]
    fn flat_quantities_are_accepted() {
        let map = values(json!({
            "requests": { "cpu": "100m", "memory": "256Mi" },
            "limits": { "cpu": "2", "memory": "2Gi" }
        }));
        assert!(validate_resource_quantities(&map).is_ok());
    }

    #[test]
    fn decimal_numbers_and_fractions_are_accepted() {
        let map = values(json!({
            "requests": { "cpu": 0.5, "memory": "1.5Gi" },
            "limits": { "cpu": "1500m", "memory": 1024 }
        }));
        assert!(validate_resource_quantities(&map).is_ok());
    }

    #[test]
    fn per_component_resources_are_accepted() {
        let map = values(json!({
            "affine": { "requests": { "cpu": "100m" }, "limits": { "memory": "1Gi" } },
            "postgres": { "limits": { "cpu": "2", "memory": "4Gi" } }
        }));
        assert!(validate_resource_quantities(&map).is_ok());
    }

    #[test]
    fn invalid_quantities_are_refused() {
        for bad in ["100mm", "2 GB", "-1", "lots", "1.2.3"] {
            let map = values(json!({ "limits": { "memory": bad } }));
            let err = validate_resource_quantities(&map).unwrap_err();
            assert!(err.contains("invalid quantity"), "{bad}: {err}");
        }
    }

    #[test]
    fn a_non_quantity_type_is_refused() {
        let map = values(json!({ "limits": { "cpu": true } }));
        let err = validate_resource_quantities(&map).unwrap_err();
        assert!(err.contains("quantity string or number"), "{err}");
    }

    #[test]
    fn unrelated_resource_keys_are_not_validated() {
        let map = values(json!({
            "requests": { "ephemeral-storage": "not-a-quantity" }
        }));
        assert!(validate_resource_quantities(&map).is_ok());
    }

    #[test]
    fn missing_resources_is_fine() {
        let map = serde_json::Map::new();
        assert!(validate_resource_quantities(&map).is_ok());
    }

    #[test]
    fn quantities_parse_decimal_and_binary_suffixes() {
        assert_eq!(quantity_number("100m"), Some(0.1));
        assert_eq!(quantity_number("0.5"), Some(0.5));
        assert_eq!(quantity_number("2"), Some(2.0));
        assert_eq!(quantity_number("1Gi"), Some(1024.0 * 1024.0 * 1024.0));
        assert_eq!(quantity_number("256Mi"), Some(256.0 * 1024.0 * 1024.0));
        assert_eq!(quantity_number("-1"), None);
        assert_eq!(quantity_number("100mm"), None);
    }

    #[test]
    fn totals_sum_flat_resources() {
        let map = values(json!({
            "requests": { "cpu": "100m", "memory": "256Mi" },
            "limits": { "cpu": "2", "memory": "2Gi" }
        }));
        let totals = resource_totals(&map);
        assert!((totals.requests_cpu - 0.1).abs() < 1e-9);
        assert_eq!(totals.requests_memory, 256.0 * 1024.0 * 1024.0);
        assert_eq!(totals.limits_cpu, 2.0);
        assert_eq!(totals.limits_memory, 2.0 * 1024.0 * 1024.0 * 1024.0);
    }

    #[test]
    fn totals_sum_every_component_group() {
        let map = values(json!({
            "app": { "requests": { "cpu": "250m" }, "limits": { "cpu": "2" } },
            "postgres": { "requests": { "cpu": 1 }, "limits": { "cpu": "4" } }
        }));
        let totals = resource_totals(&map);
        assert_eq!(totals.requests_cpu, 1.25);
        assert_eq!(totals.limits_cpu, 6.0);
        assert_eq!(totals.requests_memory, 0.0);
    }
}
