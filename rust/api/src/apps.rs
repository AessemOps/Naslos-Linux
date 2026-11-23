//! Installed-app records and lifecycle orchestration (port of
//! `api/internal/apps`).
//!
//! Records are persisted as JSON beside the other state files and carry the
//! exposure settings the routing layer renders from.

use chrono::{DateTime, Utc};
use futures::{stream, StreamExt};
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

/// Chart value keys that control platform-sensitive Kubernetes fields. Even a
/// catalog schema that lists one of them does not authorize a client to set
/// it: only the catalog/chart defaults may (SEC-2026-D7, S-1).
pub const PLATFORM_SENSITIVE_VALUE_KEYS: [&str; 21] = [
    "affinity",
    "args",
    "automountserviceaccounttoken",
    "command",
    "containers",
    "hostipc",
    "hostnetwork",
    "hostpath",
    "hostpid",
    "image",
    "imagepullpolicy",
    "initcontainers",
    "nodename",
    "podsecuritycontext",
    "privileged",
    "priorityclassname",
    "securitycontext",
    "serviceaccount",
    "serviceaccountname",
    "volumemounts",
    "volumes",
];

/// Key fragments that identify container/pod-spec controls regardless of the
/// chart's naming (`extraContainers`, `sidecarContainers`, `podSpec`, …), plus
/// fields that can execute code or change the runtime environment. Matching is
/// case-insensitive.
pub const PLATFORM_SENSITIVE_KEY_FRAGMENTS: [&str; 18] = [
    "automount",
    "container",
    "envfrom",
    "extraenv",
    "hostipc",
    "hostnetwork",
    "hostpath",
    "hostpid",
    "imagepull",
    "nodeselector",
    "podsecurity",
    "podspec",
    "priorityclass",
    "securitycontext",
    "serviceaccount",
    "sidecar",
    "tolerations",
    "volumemount",
];

/// Helm status reads are subprocess calls; run them concurrently so the
/// Installed list does not wait on one app at a time.
const LIST_STATUS_CONCURRENCY: usize = 8;

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
    /// JSON-pointer paths the catalog marked secret when this record was
    /// installed/last upgraded. Persisted so a response can still redact the
    /// right values when the catalog is unavailable or changed (R-020).
    /// Serialized as `secretPaths` on the stored record to avoid colliding with
    /// the API view's own `redactedKeys` under `#[serde(flatten)]`.
    #[serde(default, rename = "secretPaths", skip_serializing_if = "Vec::is_empty")]
    pub redacted_keys: Vec<String>,
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
    /// True when the values could not be redacted safely (no catalog entry and
    /// no persisted key list) and were omitted from the response entirely.
    #[serde(default, rename = "valuesOmitted", skip_serializing_if = "is_false")]
    pub values_omitted: bool,
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
    /// The running image's digest differs from the digest the registry serves
    /// for its tag (a new upstream image was released).
    #[serde(default, rename = "imageUpdateAvailable")]
    pub image_update_available: bool,
    /// Short digest the running pod pulled; empty when no image drift is known.
    #[serde(
        default,
        rename = "imageCurrent",
        skip_serializing_if = "String::is_empty"
    )]
    pub image_current: String,
    /// Short digest the registry currently serves for the tag.
    #[serde(
        default,
        rename = "imageLatest",
        skip_serializing_if = "String::is_empty"
    )]
    pub image_latest: String,
}

/// One app with a newer chart or a newer upstream image (the summary used by
/// the tab badge and the sidebar dot).
#[derive(Debug, Clone, serde::Serialize)]
pub struct UpdateSummary {
    pub name: String,
    #[serde(rename = "installedVersion")]
    pub installed_version: String,
    #[serde(rename = "latestVersion")]
    pub latest_version: String,
    /// The update is an upstream image release, not (only) a chart version.
    #[serde(default, rename = "imageUpdate")]
    pub image_update: bool,
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

/// Fill the catalog-derived, live-version and cached image-drift fields on a
/// view. `live_version` is Helm's release version when it could be read.
fn enrich_view(
    view: &mut View,
    live_version: Option<&str>,
    catalog_app: Option<&crate::catalog::App>,
    image_drift: Option<&crate::images::ImageDrift>,
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
    // Never advertise an image refresh when the catalog chart is *older* than
    // the installed one: the update action resolves the catalog winner and
    // would downgrade the release.
    let catalog_not_older = !update_available(&view.latest_version, &view.installed_version);
    view.image_update_available = !view.record.orphaned
        && image_drift.is_some()
        && catalog_app.is_some()
        && catalog_not_older;
    view.image_current = image_drift
        .map(|drift| crate::images::short_digest(&drift.current))
        .unwrap_or_default();
    view.image_latest = image_drift
        .map(|drift| crate::images::short_digest(&drift.latest))
        .unwrap_or_default();
}

/// The release an app's pod belongs to, by its chart label.
fn pod_instance(pod: &serde_json::Value) -> &str {
    pod.pointer("/metadata/labels/app.kubernetes.io~1instance")
        .and_then(|value| value.as_str())
        .unwrap_or("")
}

/// Whether a pod should be consulted for image drift.
///
/// Only a live pod runs the image it reports: a completed or failed pod from
/// an earlier ReplicaSet keeps its stale `imageID`, so counting it makes the
/// app advertise an image update that has already been applied. The same goes
/// for a pod that is terminating: during a rollout the old pod still runs the
/// previous digest.
fn pod_is_live_for_image_check(pod: &serde_json::Value) -> bool {
    pod.pointer("/metadata/deletionTimestamp").is_none()
        && pod
            .pointer("/status/phase")
            .and_then(|phase| phase.as_str())
            == Some("Running")
}

/// `(name, image, imageID)` for every container and init container that can
/// carry an app image. The image-refresh patch handles init containers too, so
/// drift detection scans them under the same live-pod rule (S-10).
fn pod_container_images(pod: &serde_json::Value) -> Vec<(&str, &str, &str)> {
    let mut statuses: Vec<&serde_json::Value> = Vec::new();
    for key in ["containerStatuses", "initContainerStatuses"] {
        if let Some(items) = pod
            .pointer(&format!("/status/{key}"))
            .and_then(|statuses| statuses.as_array())
        {
            statuses.extend(items.iter());
        }
    }
    let mut out = Vec::new();
    for key in ["containers", "initContainers"] {
        let Some(specs) = pod
            .pointer(&format!("/spec/{key}"))
            .and_then(|specs| specs.as_array())
        else {
            continue;
        };
        for spec in specs {
            let Some(image) = spec.get("image").and_then(|image| image.as_str()) else {
                continue;
            };
            let name = spec
                .get("name")
                .and_then(|name| name.as_str())
                .unwrap_or("");
            let image_id = statuses
                .iter()
                .find(|status| status.get("name").and_then(|n| n.as_str()).unwrap_or("") == name)
                .and_then(|status| status.get("imageID"))
                .and_then(|id| id.as_str())
                .unwrap_or("");
            out.push((name, image, image_id));
        }
    }
    out
}

/// Build the strategic-merge patch that makes the drifted container pull its
/// image again and roll: `Always` only on containers whose image is
/// `reference`, plus a pod-template annotation. Returns `None` when no
/// container matches, so unrelated sidecars and fixed-tag databases are left
/// untouched.
fn image_pull_patch(
    workload: &serde_json::Value,
    reference: &str,
    roll: &str,
) -> Option<serde_json::Value> {
    let mut containers = Vec::new();
    let mut init_containers = Vec::new();
    for (key, target) in [
        ("containers", &mut containers),
        ("initContainers", &mut init_containers),
    ] {
        if let Some(specs) = workload
            .pointer(&format!("/spec/template/spec/{key}"))
            .and_then(|specs| specs.as_array())
        {
            for spec in specs {
                let image = spec.get("image").and_then(|image| image.as_str());
                if image != Some(reference) {
                    continue;
                }
                if let Some(container) = spec.get("name").and_then(|name| name.as_str()) {
                    target.push(serde_json::json!({
                        "name": container,
                        "imagePullPolicy": "Always",
                    }));
                }
            }
        }
    }
    if containers.is_empty() && init_containers.is_empty() {
        return None;
    }
    let mut spec = serde_json::json!({});
    if !containers.is_empty() {
        spec["containers"] = serde_json::Value::Array(containers);
    }
    if !init_containers.is_empty() {
        spec["initContainers"] = serde_json::Value::Array(init_containers);
    }
    Some(serde_json::json!({
        "spec": {
            "template": {
                "metadata": {
                    "annotations": { "naslos.io/image-refresh": roll },
                },
                "spec": spec,
            }
        }
    }))
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

/// A chart copied for one install/upgrade job. Dropping it removes the
/// private directory; `path()` stays valid while the value is alive.
struct ResolvedChart {
    _snapshot: crate::chartsrepo::ChartSnapshot,
    version: String,
}

impl ResolvedChart {
    fn path(&self) -> &str {
        &self._snapshot.path
    }
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
    /// Registry tag→digest resolver, cached for `IMAGE_CHECK_TTL`.
    image_digests: crate::images::ImageDigestCache,
    /// Last upstream-image drift per app, refreshed on the catalog cycle.
    image_drift: Mutex<HashMap<String, crate::images::ImageDrift>>,
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

/// The first non-empty `persistence.*.storageClass` in a values map: the app
/// pool choice the record was installed with. Pool storage is platform-owned
/// and not part of the catalog schema, so the whitelist cannot be allowed to
/// drop it on a later upgrade.
pub fn find_persistence_storage_class(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::Object(persistence)) = map.get("persistence") {
                if let Some(serde_json::Value::String(class)) = persistence.get("storageClass") {
                    if !class.is_empty() {
                        return Some(class.clone());
                    }
                }
                for child in persistence.values() {
                    if let serde_json::Value::Object(child) = child {
                        if let Some(serde_json::Value::String(class)) = child.get("storageClass") {
                            if !class.is_empty() {
                                return Some(class.clone());
                            }
                        }
                    }
                }
            }
            map.values().find_map(find_persistence_storage_class)
        }
        serde_json::Value::Array(items) => items.iter().find_map(find_persistence_storage_class),
        _ => None,
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
            image_digests: crate::images::ImageDigestCache::from_env(),
            image_drift: Mutex::new(HashMap::new()),
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
        // Resolve the exact source/channel the operator confirmed. Without a
        // pin the current winner is used once, and its identity is recorded so
        // later upgrades cannot switch sources silently (R-051/R-065).
        let app = self.pinned_app(&catalog, &req.name, "", "", &req.pin)?;
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
        let chart = self.resolve_chart(&app, &req.pin.chart_version).await?;
        let chart_dir = chart.path().to_string();

        // Load the chart defaults so pool-backed installs can override every
        // persistence key, including keys the catalog schema does not expose.
        let chart_values = self.helm.show_values(&chart_dir).await?;
        // The API owns the dataset Helm values: never let a client point a
        // chart at an arbitrary claim or host path, and keep schema validation
        // from seeing platform-managed keys. Everything else the client sent
        // must be listed in the catalog schema and must not control a
        // platform-sensitive chart field (SEC-2026-D7, S-1).
        let mut client_values = req.values.clone().unwrap_or_default();
        strip_dataset_values(&mut client_values);
        sanitize_submitted_values(&app, &mut client_values);
        let base_values = merge_values(&chart_values, app.default_values.as_object());
        let mut values = merge_values(
            &serde_json::Value::Object(base_values.clone()),
            Some(&client_values),
        );
        // The form submits a complete `resources` object; replace it wholesale
        // so a cleared key does not come back from a chart default (UI-03).
        replace_resource_values(&mut values, &client_values);
        coerce_schema_numbers(&app.schema, &mut values);
        prune_null_resources(&mut values);
        validate_install_values(&app, &values)?;
        validate_resource_quantities(&values)?;
        let datasets = normalize_dataset_names(&req.datasets)?;
        // Resolve and validate the host before touching a namespace: a rejected
        // host must not have side effects on a namespace handed back by a
        // failed install.
        let mut exposure = exposure_for(&app, req.exposure.as_ref(), &req.name);
        // The catalog's default subdomain was never validated on the install
        // path; an invalid host must not reach routing or an app CNAME.
        validate_subdomain(&exposure.subdomain)?;
        validate_protected_paths(&exposure.protected_paths)?;
        let base_domain = if req.base_domain.is_empty() {
            self.base_domain.clone()
        } else {
            req.base_domain.clone()
        };
        // An existing record must not lose its route/CNAME to a second app
        // silently claiming the same host (S-8). The catalog-default subdomain
        // goes through the same check as an explicit one.
        self.ensure_host_available(&req.name, &exposure.subdomain, &base_domain)?;
        let (namespace, _reservation) = self.reserve_namespace(app.privileged)?;
        self.validate_resource_quota(&namespace, &values).await?;
        // A namespace handed back by a failed install can still hold that
        // attempt's release, claims and env Secret. Converge them before this
        // attempt creates anything (R-023).
        self.reconcile_namespace_orphans(&namespace, &req.name)
            .await;
        self.remove_stale_release(&req.name, app.privileged, &namespace)
            .await?;
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
        if let Err(e) = self.provision_pool_storage(&req.pool, &mut values).await {
            self.rollback_provision(&req.name, &provision).await;
            return Err(e);
        }

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
                    if env_outcome.wrote {
                        self.restore_env(&namespace, &req.name, env_outcome.previous.as_ref())
                            .await;
                    }
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
        // recomputed on every install/upgrade. Only the operator's override
        // diff is stored, never the chart defaults (S-2).
        for key in PLATFORM_ENV_KEYS {
            values.remove(key);
        }
        let stored_values = stored_override_values(
            &base_values,
            &values,
            client_values.contains_key("resources"),
        );
        let now = Utc::now();
        let rec = Record {
            name: req.name.clone(),
            source: app.source.clone(),
            channel: app.channel.clone(),
            chart_path: app.chart_path.clone(),
            chart_version: chart.version.clone(),
            redacted_keys: secret_value_pointers(&app.schema),
            values: stored_values,
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
        if let Err(e) = self
            .store
            .upsert_unique_host(rec.clone(), &self.base_domain)
        {
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
        self.clear_image_drift(&req.name);
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
        pin: &ChartPin,
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
        // Resolve the recorded source/channel (or the explicitly pinned one).
        // A bare name lookup here is what let a new user source silently take
        // over an installed app's upgrade (R-051/R-065).
        let app = self.pinned_app(&catalog, name, &rec.source, &rec.channel, pin)?;
        let chart = self.resolve_chart(&app, &pin.chart_version).await?;
        let chart_dir = chart.path().to_string();
        // The persisted values are only the operator's override diff. Re-read
        // the chart being upgraded to and merge its (possibly changed) defaults
        // underneath, so a chart bump actually reaches the release (S-2).
        let chart_values = self.helm.show_values(&chart_dir).await?;
        let base_values = merge_values(&chart_values, app.default_values.as_object());
        let secret_pointers = redaction_pointers(&rec, &app.schema);
        let pool_class =
            find_persistence_storage_class(&serde_json::Value::Object(rec.values.clone()));
        let mut stored_overrides = rec.values.clone();
        strip_dataset_values(&mut stored_overrides);
        sanitize_submitted_values(&app, &mut stored_overrides);
        stored_overrides.remove("hostPath");
        let mut client_values = values.cloned().unwrap_or_default();
        strip_dataset_values(&mut client_values);
        sanitize_submitted_values(&app, &mut client_values);
        let mut merged = upgrade_merged_values(&base_values, &stored_overrides, &client_values);

        // A redacted secret comes back as `null`; keep the stored value. This
        // walks the schema's JSON-pointer paths, so nested secrets keep their
        // stored value too (R-020).
        restore_redacted_values(
            &stored_overrides,
            Some(&base_values),
            &mut merged,
            &secret_pointers,
        );
        coerce_schema_numbers(&app.schema, &mut merged);
        // The drawer sends an explicit `null` for a cleared resource field
        // (UI-03); remove it instead of handing Helm a null quantity.
        prune_null_resources(&mut merged);
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
                let fallback = if app.privileged {
                    "hostPath"
                } else {
                    "datasetsClaim"
                };
                // Client-supplied host paths are stripped (S-1), so the chart
                // default is the only trusted source for the legacy fallback.
                match chart_values.get(fallback) {
                    Some(value) => {
                        merged.insert(fallback.to_string(), value.clone());
                    }
                    None => {
                        merged.remove(fallback);
                    }
                }
            }
        } else {
            for (key, value) in &provision.values {
                merged.insert(key.clone(), value.clone());
            }
        }
        // The pool StorageClass lives under `persistence`, which no catalog
        // schema lists; re-apply it after the dataset merge so the whitelist
        // cannot silently move new claims to the default provisioner.
        if let Some(class) = &pool_class {
            let mut as_value = serde_json::Value::Object(merged.clone());
            set_persistence_storage_class(&mut as_value, class);
            merged = as_value.as_object().cloned().unwrap_or(merged);
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
        // Persist only the override diff against the chart version now in use,
        // so the next chart bump's defaults keep flowing through (S-2).
        let keep_resources =
            stored_overrides.contains_key("resources") || client_values.contains_key("resources");
        rec.values = stored_override_values(&base_values, &merged, keep_resources);
        rec.datasets = desired;
        if let Some(meta) = env_outcome.meta {
            rec.env = meta;
        }
        if chart_supports_datasets {
            rec.datasets_layout = DATASETS_LAYOUT_V2.to_string();
        }
        rec.chart_version = chart.version.clone();
        rec.source = app.source.clone();
        rec.channel = app.channel.clone();
        rec.chart_path = app.chart_path.clone();
        if !secret_pointers.is_empty() {
            rec.redacted_keys = secret_pointers;
        }
        rec.updated_at = Some(Utc::now());
        rec.last_error = env_outcome.warning;
        // Charts pull floating tags with `IfNotPresent`; without this the
        // update button would run Helm successfully and keep the old image.
        // Only an app with a known drift is touched, and a failed rollout
        // keeps the drift (and the error) so the update is offered again.
        if let Some(drift) = self.image_drift_for(name) {
            report(progress, STAGE_FINALIZING, "Pulling the new app image");
            match self.force_image_pull(&rec, &drift).await {
                Ok(()) => self.clear_image_drift(name),
                Err(e) => {
                    let warning = format!("pulling the new app image: {e}");
                    rec.last_error = if rec.last_error.is_empty() {
                        warning
                    } else {
                        format!("{}; {warning}", rec.last_error)
                    };
                }
            }
        }
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
            let app = match self.catalog_app_for(&catalog, &rec) {
                Some(app) => app,
                None => {
                    tracing::warn!(
                        "app {:?}: dataset layout migration skipped: its catalog entry is gone",
                        rec.name
                    );
                    continue;
                }
            };
            let chart = match self.resolve_chart(&app, "").await {
                Ok(chart) => chart,
                Err(e) => {
                    tracing::warn!("app {:?}: dataset layout migration skipped: {e}", rec.name);
                    continue;
                }
            };
            if !chart_tree_mentions_datasets(std::path::Path::new(chart.path())) {
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
            if let Err(e) = self
                .upgrade(&rec.name, None, None, None, &ChartPin::default(), progress)
                .await
            {
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
        // Serialize with install/upgrade/uninstall and the dataset layout
        // migration: a read-modify-write of the exposure could otherwise
        // overwrite a concurrent upgrade's values (R-050).
        let lock = self.app_lock(name);
        let _guard = lock.lock().await;
        let rec = self.store.get(name)?;
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
        // Moving an app onto a host another app already serves would silently
        // steal its route/CNAME (S-8).
        self.ensure_host_available(name, &merged.subdomain, &base_domain)?;
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
        // Field-wise write under the store lock: only the exposure-related
        // fields this endpoint owns are replaced, so a concurrent value or
        // dataset write is never clobbered by a stale whole-record upsert.
        let updated = self
            .store
            .update_unique_host(name, &self.base_domain, |current| {
                current.exposure = merged.clone();
                current.base_domain = base_domain.clone();
                current.updated_at = Some(Utc::now());
                current.last_error.clear();
            })?;
        self.apply_route(&updated, &base_domain).await;
        self.reconcile_app_dns(&updated).await;
        self.view(name).await
    }

    /// A Helm error meaning the release is absent. Helm words a missing
    /// release identically in v3 and v4 (`Release not loaded: NAME: release:
    /// not found`); matching both fragments keeps the tolerance working across
    /// SDK upgrades.
    fn release_missing(err: &str) -> bool {
        err.contains("Release not loaded") || err.contains("release: not found")
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
        report(
            progress,
            STAGE_INSTALLING,
            &format!("Removing the {name} release"),
        );
        // An orphaned record is created from a release that exists (backfill
        // adopted it), so it must attempt the Helm removal too. A record whose
        // release is already gone must stay removable: treating that as fatal
        // stranded the record and 409-blocked reinstalling the app forever, so
        // `release_missing` is tolerated and the route, Secret, claims and the
        // record are still cleaned up below.
        match self.helm_for(&rec).uninstall(name).await {
            Ok(()) => {}
            Err(e) if Self::release_missing(&e) => {}
            Err(e) => return Err(e),
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
        self.clear_image_drift(name);
        self.store.delete(name)
    }

    /// All records with live status, resolved one `helm list` per target
    /// namespace. The Helm CLI is a subprocess and the API pod has a small CPU
    /// and memory limit, so one read per app is far more expensive than one
    /// read per namespace: shared namespaces collapse many apps into a single
    /// call, and the per-app `helm status` round trips are gone.
    pub async fn list(&self) -> Result<Vec<View>, String> {
        let catalog = (self.catalog)();
        let mut groups: HashMap<(bool, String), Vec<Record>> = HashMap::new();
        for rec in self.store.list() {
            let namespace = self.target_namespace(&rec);
            groups
                .entry((rec.privileged, namespace))
                .or_default()
                .push(rec);
        }
        // Namespace reads are subprocess calls: run them concurrently so the
        // Installed list is not gated by a serial call loop.
        let results = stream::iter(groups)
            .map(|((privileged, namespace), records)| {
                let catalog = catalog.clone();
                async move {
                    let mut err: Option<String> = None;
                    let releases: HashMap<String, helm::App> =
                        match self.helm_client(privileged, &namespace).list().await {
                            Ok(apps) => apps.into_iter().map(|a| (a.name.clone(), a)).collect(),
                            Err(e) => {
                                err = Some(e);
                                HashMap::new()
                            }
                        };
                    let mut views = Vec::with_capacity(records.len());
                    for rec in records {
                        let mut view = View {
                            namespace: self.display_namespace(&rec),
                            chart: rec.chart_path.clone(),
                            status: "missing".to_string(),
                            url: self.url_for(&rec),
                            record: rec.clone(),
                            redacted_keys: Vec::new(),
                            ..Default::default()
                        };
                        let (redacted, omitted) = self.redact_record_values(&mut view.record);
                        view.redacted_keys = redacted;
                        view.values_omitted = omitted;
                        // The list endpoint does not read the env Secret; keep
                        // the variable names for the detail endpoint only.
                        view.record.env.clear();
                        let mut live_version: Option<String> = None;
                        if let Some(rel) = releases.get(&rec.name) {
                            view.status = rel.status.clone();
                            if !rel.chart.is_empty() {
                                view.chart = rel.chart.clone();
                            }
                            if !rel.version.is_empty() {
                                live_version = Some(rel.version.clone());
                            }
                        }
                        let catalog_app =
                            catalog.as_ref().and_then(|c| self.catalog_app_for(c, &rec));
                        let drift = self.image_drift_for(&rec.name);
                        enrich_view(
                            &mut view,
                            live_version.as_deref(),
                            catalog_app.as_ref(),
                            drift.as_ref(),
                        );
                        views.push(view);
                    }
                    (views, err)
                }
            })
            .buffer_unordered(LIST_STATUS_CONCURRENCY)
            .collect::<Vec<_>>()
            .await;

        let mut first_err: Option<String> = None;
        let mut views: Vec<View> = Vec::new();
        for (mut group, err) in results {
            if first_err.is_none() {
                if let Some(e) = err.filter(|e| !e.is_empty()) {
                    first_err = Some(e);
                }
            }
            views.append(&mut group);
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

    /// The catalog entry an installed record belongs to: its own
    /// source/channel when recorded, otherwise the current winner for legacy
    /// records. Keeps update checks from following a newer source that merely
    /// won the name (R-051/R-065).
    fn catalog_app_for(&self, catalog: &Catalog, rec: &Record) -> Option<crate::catalog::App> {
        if rec.source.trim().is_empty() {
            catalog.get(&rec.name).ok()
        } else {
            catalog
                .get_from_source(&rec.name, rec.source.trim(), rec.channel.trim())
                .ok()
        }
    }

    /// Create orphaned records for releases that predate the refactor or
    /// whose install timed out before the record was written. Adopted records
    /// stay orphaned (their values cannot be reconstructed from `helm list`
    /// safely) but are visible and uninstallable instead of invisible.
    pub fn backfill(&self, platform_release: &str, releases: &[helm::App]) -> Result<(), String> {
        let catalog = (self.catalog)();
        for rel in releases {
            if rel.name == platform_release {
                continue;
            }
            if self.store.get(&rel.name).is_ok() {
                continue;
            }
            let catalog_app = catalog.as_ref().and_then(|c| c.get(&rel.name).ok());
            let rec = Record {
                name: rel.name.clone(),
                source: catalog_app
                    .as_ref()
                    .map(|app| app.source.clone())
                    .unwrap_or_default(),
                channel: catalog_app
                    .as_ref()
                    .map(|app| app.channel.clone())
                    .unwrap_or_default(),
                chart_path: catalog_app
                    .as_ref()
                    .map(|app| app.chart_path.clone())
                    .unwrap_or_default(),
                chart_version: if rel.version.is_empty() {
                    catalog_app
                        .as_ref()
                        .map(|app| app.version.clone())
                        .unwrap_or_default()
                } else {
                    rel.version.clone()
                },
                redacted_keys: catalog_app
                    .as_ref()
                    .map(|app| secret_value_pointers(&app.schema))
                    .unwrap_or_default(),
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

    /// Adopt releases that exist in an app namespace but have no record, at
    /// startup. A killed install used to leave a live release that nothing
    /// listed or could uninstall (R-022).
    pub async fn backfill_from_cluster(&self) -> Result<(), String> {
        let platform_release = std::env::var("PLATFORM_RELEASE").unwrap_or_default();
        let mut namespaces: Vec<(bool, String)> = self
            .namespace_pool
            .iter()
            .map(|namespace| (false, namespace.clone()))
            .collect();
        namespaces.extend(
            self.privileged_namespace_pool
                .iter()
                .map(|namespace| (true, namespace.clone())),
        );
        namespaces.push((false, self.helm.namespace().to_string()));
        if let Some(privileged) = &self.helm_priv {
            namespaces.push((true, privileged.namespace().to_string()));
        }
        if !self.priv_ns.is_empty() {
            namespaces.push((true, self.priv_ns.clone()));
        }
        namespaces.sort();
        namespaces.dedup();
        let mut errors = Vec::new();
        for (privileged, namespace) in namespaces {
            if namespace.is_empty() {
                continue;
            }
            match self.helm_client(privileged, &namespace).list().await {
                Ok(releases) => {
                    if let Err(e) = self.backfill(&platform_release, &releases) {
                        errors.push(format!("{namespace}: {e}"));
                    }
                }
                Err(e) => errors.push(format!("{namespace}: {e}")),
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
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
        // Adopt releases whose install died before it recorded anything, so
        // they show in the Installed list and can be uninstalled (R-022).
        if let Err(e) = self.backfill_from_cluster().await {
            tracing::warn!("app release backfill at startup: {e}");
        }
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

    /// Refuse a host already claimed by a different installed app. Two records
    /// on one host would race for the Traefik route and the app CNAME, and the
    /// last one to reconcile would silently take both over (S-8). The catalog
    /// default subdomain goes through the same check as an explicit one.
    fn ensure_host_available(
        &self,
        name: &str,
        subdomain: &str,
        base_domain: &str,
    ) -> Result<(), String> {
        let host = exposure_host(subdomain, base_domain, &self.base_domain);
        if host.is_empty() {
            return Ok(());
        }
        let records = self.store.list();
        if let Some(owner) = host_owner(records.iter(), name, &host, &self.base_domain) {
            return Err(host_conflict_message(&host, owner));
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

    /// Remove a release with this name left in a fresh namespace by a failed
    /// or timed-out earlier attempt. The record check already ran, so any
    /// release here is not managed by the platform (R-022).
    async fn remove_stale_release(
        &self,
        name: &str,
        privileged: bool,
        namespace: &str,
    ) -> Result<(), String> {
        let client = self.helm_client(privileged, namespace);
        let releases = match client.list().await {
            Ok(releases) => releases,
            Err(e) => {
                tracing::warn!("app {name}: listing releases in {namespace}: {e}");
                return Ok(());
            }
        };
        if !releases.iter().any(|release| release.name == name) {
            return Ok(());
        }
        match client.uninstall(name).await {
            Ok(()) => {}
            Err(e) if Self::release_missing(&e) => {}
            Err(e) => {
                return Err(format!(
                    "removing the stale {name:?} release left in {namespace}: {e}"
                ))
            }
        }
        if let Some(router) = &self.router {
            if let Err(e) = router.delete(namespace, name).await {
                tracing::warn!("app {name}: removing routing for a stale release: {e}");
            }
        }
        Ok(())
    }

    /// Delete platform-owned claims, PVs and env Secrets for apps that have no
    /// record in `namespace`, before the namespace is reused. Every object is
    /// matched by its platform label, so an operator-managed object without
    /// the labels is never touched (R-023).
    async fn reconcile_namespace_orphans(&self, namespace: &str, incoming: &str) {
        let Some(kube) = &self.kube else {
            return;
        };
        let mut keep: std::collections::HashSet<String> = self
            .store
            .list()
            .into_iter()
            .filter(|rec| self.target_namespace(rec) == namespace)
            .map(|rec| rec.name)
            .collect();
        keep.insert(incoming.to_string());

        if let Ok(list) = kube
            .list_json("pvc", namespace, Some("naslos.local/app"))
            .await
        {
            for (name, app) in orphaned_objects(&list, "naslos.local/app", &keep) {
                tracing::warn!("app namespace {namespace}: removing orphaned PVC {name} for {app}");
                let _ = kube.delete("pvc", &name, namespace).await;
                let _ = kube.delete("pv", &name, "").await;
            }
        }
        if let Ok(list) = kube
            .list_json(
                "secret",
                namespace,
                Some("app.kubernetes.io/managed-by=naslos"),
            )
            .await
        {
            for (name, app) in orphaned_objects(&list, "naslos.io/app", &keep) {
                tracing::warn!(
                    "app namespace {namespace}: removing orphaned Secret {name} for {app}"
                );
                let _ = kube.delete("secret", &name, namespace).await;
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

    /// Resolve the catalog entry an install/upgrade is allowed to use.
    ///
    /// An installed app always resolves from its recorded source/channel, so a
    /// user source that later wins the bare name cannot silently take over an
    /// upgrade. An explicit `pin.source` is a deliberate migration (R-051).
    fn pinned_app(
        &self,
        catalog: &Catalog,
        name: &str,
        rec_source: &str,
        rec_channel: &str,
        pin: &ChartPin,
    ) -> Result<crate::catalog::App, String> {
        let source = if pin.source.trim().is_empty() {
            rec_source.trim()
        } else {
            pin.source.trim()
        };
        if source.is_empty() {
            let winner = catalog.get(name)?;
            if !pin.channel.trim().is_empty() {
                return catalog.get_pinned(name, &winner.source, pin.channel.trim());
            }
            return Ok(winner);
        }
        let channel = if pin.channel.trim().is_empty() {
            rec_channel.trim()
        } else {
            pin.channel.trim()
        };
        catalog.get_from_source(name, source, channel)
    }

    /// Copy the resolved chart into a private per-job directory and verify the
    /// version the caller confirmed is the one Helm will actually install.
    async fn resolve_chart(
        &self,
        app: &crate::catalog::App,
        requested_version: &str,
    ) -> Result<ResolvedChart, String> {
        self.charts.ensure_fresh(&app.source, &app.channel).await?;
        let read = self.charts.source_read_lock(&app.source).await;
        let source_dir = self.charts.app_dir(&app.source, &app.channel, &app.name)?;
        crate::chartsrepo::assert_regular_tree(&source_dir)?;
        let snapshot = self
            .charts
            .snapshot_app(&app.source, &app.channel, &app.name)?;
        drop(read);
        crate::chartsrepo::assert_regular_tree(&snapshot.path)?;
        let actual = crate::chartsrepo::chart_version(&snapshot.path)
            .unwrap_or_else(|| app.version.trim().to_string());
        if !requested_version.trim().is_empty() && actual != requested_version.trim() {
            return Err(format!(
                "chart version changed: confirmed {:?}, resolved {:?} from {}/{}; reload the catalog and confirm again",
                requested_version.trim(),
                actual,
                app.source,
                app.channel
            ));
        }
        Ok(ResolvedChart {
            _snapshot: snapshot,
            version: actual,
        })
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
        let (redacted, omitted) = self.redact_record_values(&mut view.record);
        view.redacted_keys = redacted;
        view.values_omitted = omitted;
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
        let catalog_app = (self.catalog)().and_then(|c| self.catalog_app_for(&c, &rec));
        let drift = self.image_drift_for(name);
        enrich_view(
            &mut view,
            live_version.as_deref(),
            catalog_app.as_ref(),
            drift.as_ref(),
        );
        Ok(view)
    }

    fn image_drift_for(&self, name: &str) -> Option<crate::images::ImageDrift> {
        self.image_drift
            .lock()
            .ok()
            .and_then(|drift| drift.get(name).cloned())
    }

    /// Manual "Check now": forget cached registry lookups first, then rebuild
    /// the drift snapshot so a newly pushed tag is visible immediately.
    pub async fn refresh_image_updates_now(&self) {
        self.image_digests.clear();
        self.refresh_image_updates().await;
    }

    fn clear_image_drift(&self, name: &str) {
        if let Ok(mut drift) = self.image_drift.lock() {
            drift.remove(name);
        }
    }

    /// Re-resolve every installed app's container image tags and remember the
    /// apps whose running digest no longer matches the registry.
    ///
    /// Called from the background catalog refresher and the manual Refresh
    /// action, so the list and update-summary endpoints never make registry
    /// calls themselves. Pods are listed once per namespace (isolated
    /// namespaces are owned by one app; shared ones filter by the instance
    /// label) and registry lookups run a few apps at a time.
    pub async fn refresh_image_updates(&self) {
        let Some(kube) = &self.kube else {
            return;
        };
        let records: Vec<Record> = self
            .store
            .list()
            .into_iter()
            .filter(|rec| !rec.orphaned)
            .collect();
        if records.is_empty() {
            if let Ok(mut drift) = self.image_drift.lock() {
                drift.clear();
            }
            return;
        }

        let mut namespaces: Vec<String> = records
            .iter()
            .map(|rec| self.target_namespace(rec))
            .collect();
        namespaces.sort();
        namespaces.dedup();
        let mut pods_by_namespace: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
        for namespace in namespaces {
            match kube.list_json("pods", &namespace, None).await {
                Ok(list) => {
                    let items = list
                        .get("items")
                        .and_then(|items| items.as_array())
                        .cloned()
                        .unwrap_or_default();
                    pods_by_namespace.insert(namespace, items);
                }
                Err(e) => {
                    tracing::debug!("image check could not list pods in {namespace}: {e}");
                }
            }
        }

        let checks = records.into_iter().map(|rec| {
            let namespace = self.target_namespace(&rec);
            let isolated = self.is_isolated_namespace(&namespace);
            let pods = pods_by_namespace
                .get(&namespace)
                .cloned()
                .unwrap_or_default();
            let app = rec.name.clone();
            async move {
                let mut drift = None;
                'pods: for pod in pods {
                    if !pod_is_live_for_image_check(&pod) {
                        continue;
                    }
                    if !isolated && pod_instance(&pod) != app {
                        continue;
                    }
                    for (_, image, image_id) in pod_container_images(&pod) {
                        let Some(current) = crate::images::running_digest(image_id) else {
                            continue;
                        };
                        if let Some(found) = self.image_digests.check(image, &current).await {
                            drift = Some(found);
                            break 'pods;
                        }
                    }
                }
                (app, drift)
            }
        });
        let results: Vec<(String, Option<crate::images::ImageDrift>)> =
            futures::stream::iter(checks)
                .buffer_unordered(4)
                .collect()
                .await;

        let mut next: HashMap<String, crate::images::ImageDrift> = HashMap::new();
        for (name, drift) in results {
            if let Some(drift) = drift {
                next.insert(name, drift);
            }
        }
        if let Ok(mut drift) = self.image_drift.lock() {
            *drift = next;
        }
    }

    /// The instance-label selector for a record: none in a dedicated namespace
    /// (the whole namespace belongs to the app), the label in a shared one.
    fn release_selector(&self, rec: &Record) -> Option<String> {
        let namespace = self.target_namespace(rec);
        if self.is_isolated_namespace(&namespace) {
            None
        } else {
            Some(format!("app.kubernetes.io/instance={}", rec.name))
        }
    }

    /// Force the release's drifted container to pull again and roll.
    ///
    /// Charts set `IfNotPresent` on floating tags, so a Helm upgrade with
    /// unchanged values leaves the old cached image running. Patching only the
    /// container whose image actually drifted (plus a pod-template annotation)
    /// makes the update action fetch the newly released digest without
    /// changing unrelated sidecars or databases.
    async fn force_image_pull(
        &self,
        rec: &Record,
        drift: &crate::images::ImageDrift,
    ) -> Result<(), String> {
        let Some(kube) = &self.kube else {
            return Ok(());
        };
        let namespace = self.target_namespace(rec);
        let selector = self.release_selector(rec);
        let roll = Utc::now().to_rfc3339();
        let mut patched = false;
        for kind in ["deployments", "statefulsets"] {
            let list = kube
                .list_json(kind, &namespace, selector.as_deref())
                .await?;
            let Some(items) = list.get("items").and_then(|items| items.as_array()) else {
                continue;
            };
            for item in items {
                let Some(name) = item.pointer("/metadata/name").and_then(|n| n.as_str()) else {
                    continue;
                };
                let Some(patch) = image_pull_patch(item, &drift.reference, &roll) else {
                    continue;
                };
                kube.patch_strategic(kind, name, &namespace, &patch).await?;
                kube.rollout_status(&namespace, kind, name, std::time::Duration::from_secs(180))
                    .await?;
                patched = true;
            }
        }
        if !patched {
            return Err(format!(
                "no workload container matches the drifting image {}",
                drift.reference
            ));
        }
        Ok(())
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

    /// Update summary for the tab badge and sidebar dot (no Helm or registry
    /// calls): chart updates come from records + catalog, image updates from
    /// the drift snapshot refreshed on the catalog cycle.
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
                let app = self.catalog_app_for(&catalog, &rec)?;
                let chart_update = update_available(rec.chart_version.trim(), app.version.trim());
                let image_update = self.image_drift_for(&rec.name).is_some()
                    && !update_available(app.version.trim(), rec.chart_version.trim());
                if !chart_update && !image_update {
                    return None;
                }
                Some(UpdateSummary {
                    name: rec.name.clone(),
                    installed_version: rec.chart_version.trim().to_string(),
                    latest_version: app.version.trim().to_string(),
                    image_update,
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

    /// Mask secret values in a record for API responses.
    ///
    /// The catalog schema is walked as JSON pointers, so a nested secret
    /// (`postgresql.auth.password`, `smtp.credentials.token`) is masked at its
    /// own path instead of leaking because only its leaf name was compared
    /// with top-level keys. When the catalog has no entry, the persisted
    /// pointer list is used; without either, the whole value map is omitted
    /// (fail closed) rather than returned unredacted.
    pub fn redact_record_values(&self, record: &mut Record) -> (Vec<String>, bool) {
        let catalog_app = (self.catalog)().and_then(|c| self.catalog_app_for(&c, record));
        redact_record_with_catalog(record, catalog_app.as_ref())
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

/// The source/channel/chart version the operator confirmed. Empty fields fall
/// back to the installed record (upgrade) or the current catalog winner
/// (install); a non-empty version must match the chart actually resolved.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ChartPin {
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub channel: String,
    #[serde(default, rename = "chartVersion")]
    pub chart_version: String,
}

impl ChartPin {
    pub fn is_empty(&self) -> bool {
        self.source.is_empty() && self.channel.is_empty() && self.chart_version.is_empty()
    }
}

/// `Install` request input.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct InstallRequest {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub values: Option<serde_json::Map<String, serde_json::Value>>,
    /// Source/channel/version confirmed by the operator (R-051/R-065).
    #[serde(default)]
    pub pin: ChartPin,
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
/// `writeOnly: true`), at any nesting depth. Kept for the config-export
/// redactor, which removes whole named keys; API responses use the pointer
/// paths from [`secret_value_pointers`].
pub fn secret_value_keys(schema: &serde_json::Value) -> std::collections::HashSet<String> {
    secret_value_pointers(schema)
        .into_iter()
        .filter_map(|pointer| pointer.rsplit('/').next().map(str::to_string))
        .collect()
}

/// JSON-pointer paths to every property the schema marks as secret, at any
/// nesting depth. A top-level `password` becomes `/password`; a nested
/// `postgresql.auth.password` becomes `/postgresql/auth/password`.
pub fn secret_value_pointers(schema: &serde_json::Value) -> Vec<String> {
    fn walk(node: &serde_json::Value, pointer: &str, out: &mut Vec<String>) {
        match node {
            serde_json::Value::Object(map) => {
                if let Some(serde_json::Value::Object(props)) = map.get("properties") {
                    for (key, prop) in props {
                        let child = format!("{pointer}/{}", escape_pointer(key));
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
                            out.push(child.clone());
                        }
                        walk(prop, &child, out);
                    }
                }
                for (key, value) in map {
                    if key == "properties" {
                        continue;
                    }
                    walk(value, &format!("{pointer}/{}", escape_pointer(key)), out);
                }
            }
            serde_json::Value::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    walk(item, &format!("{pointer}/{index}"), out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(schema, "", &mut out);
    out.sort();
    out.dedup();
    out
}

fn escape_pointer(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

/// The UI has historically seen top-level keys (`password`); keep that shape
/// for them while nested paths stay full JSON pointers.
fn api_redaction_label(pointer: &str) -> String {
    match pointer.strip_prefix('/') {
        Some(key) if !key.contains('/') => key.replace("~1", "/").replace("~0", "~"),
        _ => pointer.to_string(),
    }
}

/// The union of the secret pointers the schema currently declares and the
/// pointers persisted on the record.
///
/// The schema can lose a pointer (a catalog change, or a schema-less catalog
/// entry) but the value is still stored: keeping the persisted pointers means
/// an older secret is never returned in clear just because the catalog stopped
/// describing it (S-3).
pub fn redaction_pointers(record: &Record, schema: &serde_json::Value) -> Vec<String> {
    let mut pointers = record.redacted_keys.clone();
    for pointer in secret_value_pointers(schema) {
        if !pointers.contains(&pointer) {
            pointers.push(pointer);
        }
    }
    pointers.sort();
    pointers.dedup();
    pointers
}

/// Redact one record using the given catalog entry and the record's persisted
/// pointer list. Returns the labels actually masked and whether the values had
/// to be omitted because no safe redaction was possible.
pub fn redact_record_with_catalog(
    record: &mut Record,
    catalog_app: Option<&crate::catalog::App>,
) -> (Vec<String>, bool) {
    let empty_schema = serde_json::Value::Null;
    let schema = catalog_app.map(|app| &app.schema).unwrap_or(&empty_schema);
    let pointers = redaction_pointers(record, schema);
    // Persist the list when there is anything to persist; a schema that no
    // longer declares a secret must not erase the record's knowledge of one.
    if !pointers.is_empty() {
        record.redacted_keys = pointers.clone();
    }
    if catalog_app.is_none() && pointers.is_empty() && !record.values.is_empty() {
        record.values.clear();
        return (Vec::new(), true);
    }
    let mut redacted = Vec::new();
    let mut root = serde_json::Value::Object(std::mem::take(&mut record.values));
    for pointer in pointers {
        let Some(slot) = root.pointer_mut(&pointer) else {
            continue;
        };
        if slot.is_null() {
            continue;
        }
        *slot = serde_json::Value::Null;
        redacted.push(api_redaction_label(&pointer));
    }
    record.values = root.as_object().cloned().unwrap_or_default();
    redacted.sort();
    redacted.dedup();
    (redacted, false)
}

/// Copy the stored value back into `merged` when the client echoed a redacted
/// nested secret as `null`, mirroring the top-level rule in `upgrade`.
fn restore_redacted_values(
    stored: &serde_json::Map<String, serde_json::Value>,
    defaults: Option<&serde_json::Map<String, serde_json::Value>>,
    merged: &mut serde_json::Map<String, serde_json::Value>,
    pointers: &[String],
) {
    let mut root = serde_json::Value::Object(std::mem::take(merged));
    let stored = serde_json::Value::Object(stored.clone());
    let defaults = defaults.map(|defaults| serde_json::Value::Object(defaults.clone()));
    for pointer in pointers {
        let incoming_null = root
            .pointer(pointer)
            .map(|value| value.is_null())
            .unwrap_or(false);
        if !incoming_null {
            continue;
        }
        // The stored override diff does not contain a value that came from the
        // chart/catalog defaults, but the UI still echoes the redacted field
        // back as `null`; fall back to the defaults so the secret round-trips
        // instead of being overwritten with null (S-3).
        let previous = stored.pointer(pointer).cloned().or_else(|| {
            defaults
                .as_ref()
                .and_then(|defaults| defaults.pointer(pointer).cloned())
        });
        let Some(previous) = previous else {
            continue;
        };
        if let Some(slot) = root.pointer_mut(pointer) {
            *slot = previous;
        }
    }
    *merged = root.as_object().cloned().unwrap_or_default();
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

/// `(name, app-label)` of list items whose `label` value is not in `keep`.
/// Only platform-labeled objects are candidates; anything without both labels
/// is operator-managed and ignored (R-023).
pub fn orphaned_objects(
    list: &serde_json::Value,
    label: &str,
    keep: &std::collections::HashSet<String>,
) -> Vec<(String, String)> {
    let label_pointer = format!("/metadata/labels/{}", label.replace('/', "~1"));
    let mut out = Vec::new();
    for item in list
        .get("items")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        let app = item.pointer(&label_pointer).and_then(|v| v.as_str());
        let name = item.pointer("/metadata/name").and_then(|v| v.as_str());
        let (Some(app), Some(name)) = (app, name) else {
            continue;
        };
        if keep.contains(app) {
            continue;
        }
        out.push((name.to_string(), app.to_string()));
    }
    out
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

/// Strip values a client may not submit: keys outside the catalog schema's
/// property tree and platform-sensitive keys (S-1). Stripped paths are logged
/// so an operator can tell why a submitted value was ignored.
fn sanitize_submitted_values(
    app: &crate::catalog::App,
    values: &mut serde_json::Map<String, serde_json::Value>,
) {
    let stripped = sanitize_client_values(&app.schema, values);
    if !stripped.is_empty() {
        tracing::warn!(
            "app {:?}: ignoring values the catalog schema does not authorize: {}",
            app.name,
            stripped.join(", ")
        );
    }
}

/// The property tree one schema node exposes, with local `$ref`s resolved and
/// `allOf`/`anyOf`/`oneOf` property maps merged. `additional` is `false` when
/// undeclared keys are denied, `true` when they are allowed, and an object
/// schema when they are delegated to an element schema.
#[derive(Default)]
struct SchemaShape {
    properties: serde_json::Map<String, serde_json::Value>,
    items: Option<serde_json::Value>,
    additional: Option<serde_json::Value>,
    patterns: bool,
    unconstrained: bool,
}

impl SchemaShape {
    fn collect(
        root: &serde_json::Value,
        schema: &serde_json::Map<String, serde_json::Value>,
        depth: usize,
        out: &mut Self,
    ) {
        if depth > 32 {
            // A self-referential `$ref` cannot be expanded further; treat the
            // node as unconstrained rather than dropping a legitimate field.
            out.unconstrained = true;
            return;
        }
        if let Some(reference) = schema.get("$ref").and_then(|r| r.as_str()) {
            if let Some(pointer) = reference.strip_prefix('#') {
                if let Some(resolved) = root.pointer(pointer).and_then(|r| r.as_object()) {
                    Self::collect(root, resolved, depth + 1, out);
                }
            }
        }
        let properties = schema.get("properties").and_then(|p| p.as_object());
        if let Some(properties) = properties {
            for (key, value) in properties {
                out.properties
                    .entry(key.clone())
                    .or_insert_with(|| value.clone());
            }
        }
        if out.items.is_none() {
            out.items = schema.get("items").cloned();
        }
        if schema
            .get("patternProperties")
            .and_then(|p| p.as_object())
            .is_some_and(|p| !p.is_empty())
        {
            out.patterns = true;
        }
        match schema.get("additionalProperties") {
            Some(serde_json::Value::Object(element)) => {
                out.additional = Some(serde_json::Value::Object(element.clone()));
            }
            Some(serde_json::Value::Bool(true)) => {
                if !matches!(out.additional, Some(serde_json::Value::Object(_))) {
                    out.additional = Some(serde_json::Value::Bool(true));
                }
            }
            Some(serde_json::Value::Bool(false)) => {
                if out.additional.is_none() {
                    out.additional = Some(serde_json::Value::Bool(false));
                }
            }
            Some(_) => {
                if out.additional.is_none() {
                    out.additional = Some(serde_json::Value::Bool(false));
                }
            }
            None => {
                let composed = schema.get("$ref").is_some()
                    || ["allOf", "anyOf", "oneOf"].iter().any(|key| {
                        schema
                            .get(*key)
                            .and_then(|b| b.as_array())
                            .is_some_and(|b| !b.is_empty())
                    });
                if properties.is_none() && !composed {
                    out.unconstrained = true;
                } else if out.additional.is_none() {
                    out.additional = Some(serde_json::Value::Bool(false));
                }
            }
        }
        for key in ["allOf", "anyOf", "oneOf"] {
            if let Some(branches) = schema.get(key).and_then(|b| b.as_array()) {
                for branch in branches {
                    if let Some(branch) = branch.as_object() {
                        Self::collect(root, branch, depth + 1, out);
                    }
                }
            }
        }
    }
}

/// Project `values` onto the catalog schema's property tree. The merged
/// install values also carry chart/catalog defaults (image settings, service
/// wiring) that the schema never lists; validating those against the hardened
/// whitelist schema would reject every chart. Only catalog-exposed keys are
/// validated. Local `$ref`s and composed (`allOf`/`anyOf`/`oneOf`) property
/// maps are resolved so defaults under a composed node are not dropped.
fn project_schema_values(
    root: &serde_json::Value,
    schema: Option<&serde_json::Map<String, serde_json::Value>>,
    values: &serde_json::Value,
) -> serde_json::Value {
    let mut shape = SchemaShape::default();
    match schema {
        Some(schema) => SchemaShape::collect(root, schema, 0, &mut shape),
        None => shape.unconstrained = true,
    }
    match values {
        serde_json::Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, value) in map {
                if let Some(child) = shape.properties.get(key) {
                    out.insert(
                        key.clone(),
                        project_schema_values(root, child.as_object(), value),
                    );
                    continue;
                }
                if let Some(element) = shape.additional.as_ref().and_then(|a| a.as_object()) {
                    out.insert(
                        key.clone(),
                        project_schema_values(root, Some(element), value),
                    );
                    continue;
                }
                if shape.patterns
                    || shape.unconstrained
                    || matches!(shape.additional, Some(serde_json::Value::Bool(true)))
                {
                    out.insert(key.clone(), value.clone());
                }
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(items) => match shape.items.as_ref().and_then(|i| i.as_object()) {
            Some(item_schema) => serde_json::Value::Array(
                items
                    .iter()
                    .map(|item| project_schema_values(root, Some(item_schema), item))
                    .collect(),
            ),
            None => values.clone(),
        },
        _ => values.clone(),
    }
}

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
    let instance = project_schema_values(
        &app.schema,
        app.schema.as_object(),
        &serde_json::Value::Object(values.clone()),
    );
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
/// Replace the platform-edited `resources` object instead of deep-merging it.
/// An omitted key is a cleared field; the drawer sends the complete object
/// built from its form. A `null` top-level value removes the object.
pub fn replace_resource_values(
    merged: &mut serde_json::Map<String, serde_json::Value>,
    incoming: &serde_json::Map<String, serde_json::Value>,
) {
    let Some(resources) = incoming.get("resources") else {
        return;
    };
    if resources.is_null() {
        merged.remove("resources");
    } else {
        merged.insert("resources".to_string(), resources.clone());
    }
}

/// Remove `null` resource entries so a cleared field is a removal, not a Helm
/// value. Handles the flat layout (`resources.requests.cpu`) and the
/// per-component layout (`resources.<component>.limits.cpu`) the catalog uses.
pub fn prune_null_resources(values: &mut serde_json::Map<String, serde_json::Value>) {
    fn prune_sections(entry: &mut serde_json::Map<String, serde_json::Value>) {
        for section in ["requests", "limits"] {
            let Some(quantities) = entry.get_mut(section).and_then(|v| v.as_object_mut()) else {
                continue;
            };
            quantities.retain(|_, value| !value.is_null());
            if quantities.is_empty() {
                entry.remove(section);
            }
        }
    }
    let Some(resources) = values.get_mut("resources").and_then(|v| v.as_object_mut()) else {
        return;
    };
    prune_sections(resources);
    let mut empty_groups = Vec::new();
    for (group, entry) in resources.iter_mut() {
        if let Some(entry) = entry.as_object_mut() {
            prune_sections(entry);
            if entry.is_empty() {
                empty_groups.push(group.clone());
            }
        }
    }
    for group in empty_groups {
        resources.remove(&group);
    }
}

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
                if raw.is_null() {
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

fn is_platform_sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    PLATFORM_SENSITIVE_VALUE_KEYS.contains(&key.as_str())
        || PLATFORM_SENSITIVE_KEY_FRAGMENTS
            .iter()
            .any(|fragment| key.contains(fragment))
        // Chart schemas name these differently (`startupCommand`,
        // `containerArgs`, `extraArgs`), so match the suffix too.
        || key.ends_with("command")
        || key.ends_with("args")
}

/// Enforce the catalog schema as a whitelist for client-submitted values.
///
/// Every key not declared in the schema's property tree is removed, together
/// with keys that control platform-sensitive chart fields (`image`,
/// `privileged`, `hostPath`, `serviceAccount`, …) even when a schema lists
/// them. It is the client's only path into the Helm release, so without this a
/// values blob could replace the app image or mount an arbitrary host path
/// (SEC-2026-D7, S-1). Returns the stripped paths for logging.
pub fn sanitize_client_values(
    schema: &serde_json::Value,
    values: &mut serde_json::Map<String, serde_json::Value>,
) -> Vec<String> {
    let mut stripped = Vec::new();
    let mut root = serde_json::Value::Object(std::mem::take(values));
    // `resources` is the drawer's platform-owned editor, not a catalog schema
    // field (no catalog declares it). Keep it outside the schema whitelist but
    // still apply the sensitive-key rule beneath it.
    let resources = root.as_object_mut().and_then(|map| map.remove("resources"));
    sanitize_value(schema, schema.as_object(), &mut root, "", &mut stripped);
    if let Some(mut resources) = resources {
        sanitize_value(
            &serde_json::Value::Null,
            None,
            &mut resources,
            "/resources",
            &mut stripped,
        );
        if let Some(map) = root.as_object_mut() {
            map.insert("resources".to_string(), resources);
        }
    }
    *values = root.as_object().cloned().unwrap_or_default();
    stripped.sort();
    stripped.dedup();
    stripped
}

fn sanitize_value(
    root: &serde_json::Value,
    schema: Option<&serde_json::Map<String, serde_json::Value>>,
    value: &mut serde_json::Value,
    path: &str,
    stripped: &mut Vec<String>,
) {
    let mut shape = SchemaShape::default();
    match schema {
        Some(schema) => SchemaShape::collect(root, schema, 0, &mut shape),
        None => shape.unconstrained = true,
    }
    match value {
        serde_json::Value::Object(map) => {
            let mut remove = Vec::new();
            for (key, child) in map.iter_mut() {
                let child_path = format!("{path}/{}", escape_pointer(key));
                if is_platform_sensitive_key(key) {
                    remove.push(key.clone());
                    stripped.push(child_path);
                    continue;
                }
                if let Some(child_schema) = shape.properties.get(key) {
                    sanitize_value(root, child_schema.as_object(), child, &child_path, stripped);
                    continue;
                }
                if let Some(element) = shape.additional.as_ref().and_then(|a| a.as_object()) {
                    sanitize_value(root, Some(element), child, &child_path, stripped);
                    continue;
                }
                if shape.patterns
                    || shape.unconstrained
                    || matches!(shape.additional, Some(serde_json::Value::Bool(true)))
                {
                    // No per-key schema: only the sensitive-key rule applies
                    // beneath this node.
                    sanitize_value(root, None, child, &child_path, stripped);
                    continue;
                }
                remove.push(key.clone());
                stripped.push(child_path);
            }
            for key in remove {
                map.remove(&key);
            }
        }
        serde_json::Value::Array(items) => {
            let item_schema = shape.items.as_ref().and_then(|i| i.as_object());
            for (index, item) in items.iter_mut().enumerate() {
                sanitize_value(
                    root,
                    item_schema,
                    item,
                    &format!("{path}/{index}"),
                    stripped,
                );
            }
        }
        _ => {}
    }
}

/// The persisted value set for a record: only what differs from the trusted
/// chart/catalog defaults (plus the platform-derived values that are not
/// recomputed on upgrade, such as the pool storage class). Chart defaults are
/// deliberately not stored, so a later chart bump applies its changed defaults
/// (including `image.tag`) on upgrade (S-2).
fn stored_override_values(
    base: &serde_json::Map<String, serde_json::Value>,
    values: &serde_json::Map<String, serde_json::Value>,
    keep_resources: bool,
) -> serde_json::Map<String, serde_json::Value> {
    let mut overrides = values_override_diff(base, values);
    // The resource editor submits a complete object where an omitted key means
    // "cleared", so it is stored (and applied on upgrade) as a replacement
    // rather than a diff whose cleared keys a chart default would refill.
    if keep_resources {
        let resources = values
            .get("resources")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        overrides.insert("resources".to_string(), resources);
    }
    // Platform-owned values are recomputed from the record on every upgrade.
    for key in PLATFORM_DATASET_KEYS {
        overrides.remove(key);
    }
    for key in PLATFORM_ENV_KEYS {
        overrides.remove(key);
    }
    overrides.remove("hostPath");
    overrides
}

fn values_override_diff(
    base: &serde_json::Map<String, serde_json::Value>,
    values: &serde_json::Map<String, serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut out = serde_json::Map::new();
    for (key, value) in values {
        match base.get(key) {
            Some(base_value) if base_value == value => continue,
            Some(serde_json::Value::Object(base_object)) => {
                if let serde_json::Value::Object(object) = value {
                    let nested = values_override_diff(base_object, object);
                    if !nested.is_empty() {
                        out.insert(key.clone(), serde_json::Value::Object(nested));
                    }
                } else {
                    out.insert(key.clone(), value.clone());
                }
            }
            _ => {
                out.insert(key.clone(), value.clone());
            }
        }
    }
    out
}

/// Merge the NEW chart defaults underneath the stored overrides and the
/// client's new values. The stored override set was captured against the chart
/// version that was installed, so this is what lets a chart bump's changed
/// defaults (`image.tag` included) reach an existing release (S-2).
fn upgrade_merged_values(
    base_values: &serde_json::Map<String, serde_json::Value>,
    stored_overrides: &serde_json::Map<String, serde_json::Value>,
    client_values: &serde_json::Map<String, serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut merged = base_values.clone();
    merged = merge_values(&serde_json::Value::Object(merged), Some(stored_overrides));
    replace_resource_values(&mut merged, stored_overrides);
    merged = merge_values(&serde_json::Value::Object(merged), Some(client_values));
    replace_resource_values(&mut merged, client_values);
    merged
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

/// The name of the record (other than `name`) that already claims `host`.
pub fn host_owner<'a>(
    records: impl IntoIterator<Item = &'a Record>,
    name: &str,
    host: &str,
    default_base: &str,
) -> Option<&'a str> {
    if host.is_empty() {
        return None;
    }
    records
        .into_iter()
        .find(|rec| {
            rec.name != name
                && exposure_host(&rec.exposure.subdomain, &rec.base_domain, default_base) == host
        })
        .map(|rec| rec.name.as_str())
}

fn host_conflict_message(host: &str, owner: &str) -> String {
    format!(
        "host {host:?} is already claimed by app {owner:?}; choose another subdomain or base domain"
    )
}

/// The fully-qualified host an exposure claims, or empty when it has no route.
/// Compared case-insensitively because DNS host names are.
pub fn exposure_host(subdomain: &str, base_domain: &str, default_base: &str) -> String {
    let subdomain = subdomain.trim();
    if subdomain.is_empty() {
        return String::new();
    }
    let base = if base_domain.trim().is_empty() {
        default_base.trim()
    } else {
        base_domain.trim()
    };
    if base.is_empty() {
        return String::new();
    }
    format!(
        "{}.{}",
        subdomain.to_ascii_lowercase(),
        base.to_ascii_lowercase()
    )
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
        // Clone-and-swap: the previous value is restored when the durable
        // write fails, so memory never runs ahead of disk (R-049a).
        let previous = guard.insert(rec.name.clone(), clone_record(&rec));
        if let Err(e) = self.save_locked(&guard) {
            match previous {
                Some(previous) => {
                    guard.insert(rec.name, previous);
                }
                None => {
                    guard.remove(&rec.name);
                }
            }
            return Err(e);
        }
        Ok(())
    }

    pub fn update(&self, name: &str, mutate: impl FnOnce(&mut Record)) -> Result<Record, String> {
        let mut guard = self.records.lock().unwrap();
        let previous = guard
            .get(name)
            .map(clone_record)
            .ok_or_else(|| format!("app {name:?} not found"))?;
        let Some(rec) = guard.get_mut(name) else {
            return Err(format!("app {name:?} not found"));
        };
        mutate(rec);
        let out = clone_record(rec);
        if let Err(e) = self.save_locked(&guard) {
            guard.insert(name.to_string(), previous);
            return Err(e);
        }
        Ok(out)
    }

    /// Insert a record with an atomic host-claim check.
    ///
    /// The comparison and the insertion happen under one lock over the record
    /// map, so two installs of different names (each holding only its own
    /// per-app lock) cannot both claim the same `subdomain + baseDomain`
    /// (S-8). A conflict leaves the store unchanged.
    pub fn upsert_unique_host(&self, rec: Record, default_base: &str) -> Result<(), String> {
        let mut guard = self.records.lock().unwrap();
        let host = exposure_host(&rec.exposure.subdomain, &rec.base_domain, default_base);
        if let Some(owner) = host_owner(guard.values(), &rec.name, &host, default_base) {
            return Err(host_conflict_message(&host, owner));
        }
        let previous = guard.insert(rec.name.clone(), clone_record(&rec));
        if let Err(e) = self.save_locked(&guard) {
            match previous {
                Some(previous) => {
                    guard.insert(rec.name, previous);
                }
                None => {
                    guard.remove(&rec.name);
                }
            }
            return Err(e);
        }
        Ok(())
    }

    /// Update a record with an atomic host-claim check. Used by the exposure
    /// endpoint, which can move an app onto a host another app already serves;
    /// the mutation is rolled back when the new host conflicts (S-8).
    pub fn update_unique_host(
        &self,
        name: &str,
        default_base: &str,
        mutate: impl FnOnce(&mut Record),
    ) -> Result<Record, String> {
        let mut guard = self.records.lock().unwrap();
        let previous = guard
            .get(name)
            .map(clone_record)
            .ok_or_else(|| format!("app {name:?} not found"))?;
        let Some(rec) = guard.get_mut(name) else {
            return Err(format!("app {name:?} not found"));
        };
        mutate(rec);
        let out = clone_record(rec);
        let host = exposure_host(&out.exposure.subdomain, &out.base_domain, default_base);
        let conflict = host_owner(guard.values(), name, &host, default_base)
            .map(|owner| host_conflict_message(&host, owner));
        if let Some(message) = conflict {
            guard.insert(name.to_string(), previous);
            return Err(message);
        }
        if let Err(e) = self.save_locked(&guard) {
            guard.insert(name.to_string(), previous);
            return Err(e);
        }
        Ok(out)
    }

    pub fn delete(&self, name: &str) -> Result<(), String> {
        let mut guard = self.records.lock().unwrap();
        let previous = guard
            .remove(name)
            .ok_or_else(|| format!("app {name:?} not found"))?;
        if let Err(e) = self.save_locked(&guard) {
            guard.insert(name.to_string(), previous);
            return Err(e);
        }
        Ok(())
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
    fn release_missing_matches_helm_wording() {
        // The exact strings observed from the helm SDK (v3 and v4 alike).
        assert!(Manager::release_missing(
            "helm uninstall: Error: uninstall: Release not loaded: actual-budget: release: not found"
        ));
        // Fragments stay matched if the SDK gains or drops a prefix.
        assert!(Manager::release_missing(
            "Release not loaded: x: release: not found"
        ));
        assert!(Manager::release_missing("release: not found"));
        // Anything else stays a real failure: the tolerance must not swallow
        // permission, timeout or config errors.
        assert!(!Manager::release_missing(
            "helm uninstall: context deadline exceeded"
        ));
        assert!(!Manager::release_missing("forbidden: secrets is forbidden"));
        assert!(!Manager::release_missing(""));
    }

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
        enrich_view(&mut view, Some("0.1.2"), Some(&app), None);
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
        enrich_view(&mut view, None, None, None);
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
        enrich_view(&mut view, None, Some(&app), None);
        assert!(!view.update_available);
    }

    #[test]
    fn enrich_view_reports_image_drift_separately_from_a_chart_update() {
        let app = crate::catalog::App {
            name: "radarr".to_string(),
            version: "0.1.6".to_string(),
            ..Default::default()
        };
        let drift = crate::images::ImageDrift {
            reference: "lscr.io/linuxserver/radarr:latest".to_string(),
            current: "sha256:51a9ac2a9eb47afc833541e5fd6c11b86eb9f810694c22fa2429e3078e9194b2"
                .to_string(),
            latest: "sha256:ff1b1f21a200e19f52017bd1544031143715b1ab262b823a9dcb916a71da0956"
                .to_string(),
        };
        let mut view = View {
            record: Record {
                name: "radarr".to_string(),
                chart_version: "0.1.6".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        enrich_view(&mut view, Some("0.1.6"), Some(&app), Some(&drift));
        assert!(!view.update_available, "the chart is current");
        assert!(view.image_update_available);
        assert_eq!(view.image_current, "51a9ac2a9eb4");
        assert_eq!(view.image_latest, "ff1b1f21a200");

        // A catalog chart older than the installed one must not offer an
        // "image update": the update action resolves the catalog winner and
        // would downgrade the release.
        let older = crate::catalog::App {
            name: "radarr".to_string(),
            version: "0.1.5".to_string(),
            ..Default::default()
        };
        enrich_view(&mut view, Some("0.1.6"), Some(&older), Some(&drift));
        assert!(!view.update_available);
        assert!(!view.image_update_available);

        // An orphan must never advertise an image update either.
        view.record.orphaned = true;
        enrich_view(&mut view, Some("0.1.6"), Some(&app), Some(&drift));
        assert!(!view.image_update_available);
    }

    #[test]
    fn image_pull_patch_targets_only_the_drifted_container() {
        let workload = serde_json::json!({
            "spec": { "template": { "spec": {
                "containers": [
                    { "name": "gluetun", "image": "qmcgaw/gluetun:latest" },
                    { "name": "prowlarr", "image": "lscr.io/linuxserver/prowlarr:latest" }
                ],
                "initContainers": [ { "name": "init", "image": "busybox:latest" } ]
            } } }
        });
        let patch =
            image_pull_patch(&workload, "lscr.io/linuxserver/prowlarr:latest", "now").unwrap();
        assert_eq!(
            patch["spec"]["template"]["metadata"]["annotations"]["naslos.io/image-refresh"],
            "now"
        );
        let containers = patch["spec"]["template"]["spec"]["containers"]
            .as_array()
            .unwrap();
        assert_eq!(containers.len(), 1);
        assert_eq!(containers[0]["name"], "prowlarr");
        assert_eq!(containers[0]["imagePullPolicy"], "Always");
        assert!(patch["spec"]["template"]["spec"]["initContainers"].is_null());

        assert!(image_pull_patch(&workload, "example/app:v2", "now").is_none());
        assert!(image_pull_patch(&serde_json::json!({}), "example/app:v2", "now").is_none());
    }

    #[test]
    fn only_live_pods_participate_in_image_drift() {
        let running = serde_json::json!({ "status": { "phase": "Running" } });
        assert!(pod_is_live_for_image_check(&running));

        // A completed pod from an older ReplicaSet keeps its stale imageID;
        // counting it reports an update that was already applied.
        for phase in ["Succeeded", "Failed", "Pending", "Unknown"] {
            let pod = serde_json::json!({ "status": { "phase": phase } });
            assert!(!pod_is_live_for_image_check(&pod), "{phase}");
        }
        assert!(!pod_is_live_for_image_check(&serde_json::json!({})));

        // A terminating pod still runs the previous digest during a rollout.
        let terminating = serde_json::json!({
            "metadata": { "deletionTimestamp": "2026-01-01T00:00:00Z" },
            "status": { "phase": "Running" }
        });
        assert!(!pod_is_live_for_image_check(&terminating));
    }

    #[test]
    fn init_containers_participate_in_image_drift() {
        // The refresh patch already handles init containers, so drift
        // detection must scan them too (S-10).
        let pod = serde_json::json!({
            "spec": {
                "containers": [{ "name": "app", "image": "example/app:1" }],
                "initContainers": [{ "name": "init", "image": "example/init:1" }]
            },
            "status": {
                "phase": "Running",
                "containerStatuses": [
                    { "name": "app", "imageID": "example/app@sha256:aaa" }
                ],
                "initContainerStatuses": [
                    { "name": "init", "imageID": "example/init@sha256:bbb" }
                ]
            }
        });
        let images = pod_container_images(&pod);
        assert_eq!(images.len(), 2);
        assert!(images.contains(&("app", "example/app:1", "example/app@sha256:aaa")));
        assert!(images.contains(&("init", "example/init:1", "example/init@sha256:bbb")));

        // An init container without a matching status is still scanned; the
        // live-pod rule itself is unchanged.
        let pod = serde_json::json!({
            "spec": { "initContainers": [{ "name": "init", "image": "example/init:2" }] },
            "status": { "phase": "Running" }
        });
        assert!(pod_is_live_for_image_check(&pod));
        assert_eq!(
            pod_container_images(&pod),
            vec![("init", "example/init:2", "")]
        );
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
        clamp_pool_limit, find_persistence_storage_class, load_pool_settings, pick_namespace,
        pool_storage_class, secret_value_keys, set_persistence_storage_class,
        validate_install_values, validate_pool_for_storage, validate_pool_limit,
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
    fn the_pool_storage_class_survives_the_whitelist() {
        let flat = json!({ "persistence": { "storageClass": "naslos-pool-data-abc" } });
        assert_eq!(
            find_persistence_storage_class(&flat).as_deref(),
            Some("naslos-pool-data-abc")
        );
        let nested = json!({
            "postgres": { "persistence": { "storageClass": "naslos-pool-ssd-def" } }
        });
        assert_eq!(
            find_persistence_storage_class(&nested).as_deref(),
            Some("naslos-pool-ssd-def")
        );
        assert_eq!(
            find_persistence_storage_class(&json!({ "persistence": { "size": "1Gi" } })),
            None
        );
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
mod values_whitelist_tests {
    use super::{
        merge_values, project_schema_values, replace_resource_values, restore_redacted_values,
        sanitize_client_values, stored_override_values, upgrade_merged_values,
        validate_install_values,
    };
    use crate::catalog::App;
    use serde_json::json;

    fn schema() -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "port": { "type": "integer" },
                "smtp": {
                    "type": "object",
                    "properties": { "host": { "type": "string" } }
                }
            }
        })
    }

    fn map(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        value.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn install_rejects_a_hostile_values_blob() {
        let mut client = map(json!({
            "port": 8080,
            "image": { "repository": "evil", "tag": "backdoor" },
            "privileged": true,
            "hostPath": "/etc",
            "serviceAccount": "naslos-admin",
            "podSecurityContext": { "runAsUser": 0 },
            "notInTheSchema": "injected"
        }));
        let stripped = sanitize_client_values(&schema(), &mut client);
        assert_eq!(client["port"], 8080);
        for key in [
            "image",
            "privileged",
            "hostPath",
            "serviceAccount",
            "podSecurityContext",
            "notInTheSchema",
        ] {
            assert!(client.get(key).is_none(), "{key} must be stripped");
        }
        for path in [
            "/image",
            "/privileged",
            "/hostPath",
            "/serviceAccount",
            "/podSecurityContext",
            "/notInTheSchema",
        ] {
            assert!(stripped.contains(&path.to_string()), "{path}: {stripped:?}");
        }
    }

    #[test]
    fn upgrade_rejects_a_hostile_values_blob_and_legacy_record() {
        // Both the request values and a legacy record that persisted chart
        // defaults (or injected keys) go through the same whitelist on update.
        let mut stored = map(json!({
            "port": 8080,
            "image": { "tag": "backdoor" },
            "privileged": true,
            "hostPath": "/etc"
        }));
        sanitize_client_values(&schema(), &mut stored);
        assert_eq!(stored["port"], 8080);
        assert!(stored.get("image").is_none());
        assert!(stored.get("privileged").is_none());
        assert!(stored.get("hostPath").is_none());

        let chart = map(json!({ "image": { "tag": "2.0" }, "port": 80 }));
        let merged = upgrade_merged_values(&chart, &stored, &map(json!({})));
        assert_eq!(merged["image"]["tag"], "2.0");
        assert_eq!(merged["port"], 8080);
    }

    #[test]
    fn nested_unknown_and_sensitive_keys_are_stripped() {
        let mut client = map(json!({
            "smtp": { "host": "mail.example", "image": "evil", "token": "leak" }
        }));
        let stripped = sanitize_client_values(&schema(), &mut client);
        assert_eq!(client["smtp"]["host"], "mail.example");
        assert!(client["smtp"].get("image").is_none());
        assert!(client["smtp"].get("token").is_none());
        assert!(stripped.contains(&"/smtp/image".to_string()));
        assert!(stripped.contains(&"/smtp/token".to_string()));
    }

    #[test]
    fn schema_listed_sensitive_keys_are_still_not_authorized() {
        let schema = json!({
            "type": "object",
            "properties": {
                "image": { "type": "object" },
                "hostPath": { "type": "string" },
                "allowed": { "type": "string" }
            }
        });
        let mut client = map(json!({
            "image": { "tag": "evil" },
            "hostPath": "/etc",
            "allowed": "yes"
        }));
        sanitize_client_values(&schema, &mut client);
        assert_eq!(client["allowed"], "yes");
        assert!(client.get("image").is_none());
        assert!(client.get("hostPath").is_none());
    }

    #[test]
    fn an_additional_properties_schema_authorizes_extra_keys() {
        let schema = json!({
            "type": "object",
            "additionalProperties": { "type": "string" }
        });
        let mut client = map(json!({ "anything": "kept", "image": "evil" }));
        sanitize_client_values(&schema, &mut client);
        assert_eq!(client["anything"], "kept");
        assert!(client.get("image").is_none(), "the denylist still applies");
    }

    #[test]
    fn the_resources_editor_is_authorized_outside_the_schema() {
        // No catalog declares `resources`; the drawer's resource editor must
        // survive the schema whitelist, while a sensitive key inside it does
        // not.
        let mut client = map(json!({
            "port": 8080,
            "resources": {
                "requests": { "cpu": "100m" },
                "image": "evil"
            }
        }));
        let stripped = sanitize_client_values(&schema(), &mut client);
        assert_eq!(client["resources"]["requests"]["cpu"], "100m");
        assert!(client["resources"].get("image").is_none());
        assert!(stripped.contains(&"/resources/image".to_string()));
        assert_eq!(client["port"], 8080);
    }

    #[test]
    fn schema_listed_container_controls_are_still_stripped() {
        let schema = json!({
            "type": "object",
            "properties": {
                "command": { "type": "array" },
                "args": { "type": "array" },
                "extraContainers": { "type": "array" },
                "sidecar": { "type": "object" },
                "tolerations": { "type": "array" },
                "nodeSelector": { "type": "object" },
                "priorityClassName": { "type": "string" },
                "containerArgs": { "type": "array" },
                "startupCommand": { "type": "string" },
                "allowed": { "type": "string" }
            }
        });
        let mut client = map(json!({
            "command": ["sh", "-c", "id"],
            "args": ["--evil"],
            "extraContainers": [{ "name": "sidecar", "image": "evil" }],
            "sidecar": { "name": "sidecar" },
            "tolerations": [{ "operator": "Exists" }],
            "nodeSelector": { "node-role": "control-plane" },
            "priorityClassName": "system-node-critical",
            "containerArgs": ["--evil"],
            "startupCommand": "id",
            "allowed": "yes"
        }));
        let stripped = sanitize_client_values(&schema, &mut client);
        assert_eq!(client["allowed"], "yes");
        for key in [
            "command",
            "args",
            "extraContainers",
            "sidecar",
            "tolerations",
            "nodeSelector",
            "priorityClassName",
            "containerArgs",
            "startupCommand",
        ] {
            assert!(client.get(key).is_none(), "{key} must be stripped");
        }
        assert!(stripped.contains(&"/command".to_string()));
        assert!(stripped.contains(&"/startupCommand".to_string()));
    }

    #[test]
    fn map_typed_additional_properties_keep_their_element_schema() {
        let app = App {
            name: "demo".into(),
            schema: json!({
                "type": "object",
                "properties": { "port": { "type": "integer" } },
                "additionalProperties": { "type": "string" }
            }),
            ..Default::default()
        };
        let mut client = map(json!({
            "port": 8080,
            "dynamic": "kept",
            "image": "evil"
        }));
        let stripped = sanitize_client_values(&app.schema, &mut client);
        assert_eq!(client["dynamic"], "kept");
        assert!(client.get("image").is_none());
        assert!(stripped.contains(&"/image".to_string()));

        let mut bad = map(json!({ "port": 8080, "dynamic": 42 }));
        assert!(
            validate_install_values(&app, &bad).is_err(),
            "the element schema must still be enforced"
        );
        bad.insert("dynamic".into(), json!("ok"));
        assert!(validate_install_values(&app, &bad).is_ok());
    }

    #[test]
    fn composed_schemas_keep_defaults_under_refs_and_all_of() {
        let app = App {
            name: "demo".into(),
            schema: json!({
                "type": "object",
                "properties": { "db": { "$ref": "#/$defs/db" } },
                "$defs": {
                    "db": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": { "password": { "type": "string" } },
                        "required": ["password"]
                    }
                },
                "allOf": [{
                    "properties": { "port": { "type": "integer" } },
                    "required": ["port"]
                }]
            }),
            ..Default::default()
        };
        let values = json!({
            "port": 8080,
            "db": { "password": "chart-default", "dbExtra": true },
            "chartExtra": { "tag": "1.0" }
        });
        let projected = project_schema_values(&app.schema, app.schema.as_object(), &values);
        assert_eq!(projected["port"], 8080);
        assert_eq!(projected["db"]["password"], "chart-default");
        assert!(
            projected["db"].get("dbExtra").is_none(),
            "the referenced schema's whitelist applies"
        );
        assert!(projected.get("chartExtra").is_none());
        assert!(
            validate_install_values(&app, values.as_object().unwrap()).is_ok(),
            "a composed schema must not fail on its own defaults"
        );
    }

    #[test]
    fn a_chart_default_secret_survives_a_redacted_round_trip() {
        let base = map(json!({ "password": "chart-default", "port": 80 }));
        // Install stores only the override diff: the chart default password is
        // not in the record.
        let stored = stored_override_values(&base, &base, false);
        assert!(stored.is_empty());

        // The UI echoes the redacted field back as null on reconfigure.
        let client = map(json!({ "password": null }));
        let mut merged = upgrade_merged_values(&base, &stored, &client);
        assert!(merged["password"].is_null());
        restore_redacted_values(
            &stored,
            Some(&base),
            &mut merged,
            &["/password".to_string()],
        );
        assert_eq!(
            merged["password"], "chart-default",
            "a defaults-supplied secret must not be overwritten with null"
        );
        assert_eq!(merged["port"], 80);
    }

    #[test]
    fn install_stores_only_the_override_diff() {
        let base = map(json!({
            "image": { "tag": "1.0" },
            "port": 80,
            "persistence": { "data": { "size": "2Gi" } }
        }));
        let client = map(json!({ "port": 8080 }));
        let values = merge_values(&serde_json::Value::Object(base.clone()), Some(&client));
        let stored = stored_override_values(&base, &values, false);
        assert_eq!(stored["port"], 8080);
        assert!(
            stored.get("image").is_none(),
            "chart defaults must not be persisted"
        );
        assert!(
            stored.get("persistence").is_none(),
            "unchanged chart defaults must not be persisted"
        );
    }

    #[test]
    fn upgrade_applies_a_changed_chart_default_image_tag() {
        let install_base = map(json!({ "image": { "tag": "1.0" }, "port": 80 }));
        let client = map(json!({ "port": 8080 }));
        let install_values = merge_values(
            &serde_json::Value::Object(install_base.clone()),
            Some(&client),
        );
        let stored = stored_override_values(&install_base, &install_values, false);

        // The chart bumps its default image tag between install and upgrade.
        let upgrade_chart = map(json!({ "image": { "tag": "2.0" }, "port": 80 }));
        let merged = upgrade_merged_values(&upgrade_chart, &stored, &map(json!({})));
        assert_eq!(merged["image"]["tag"], "2.0");
        assert_eq!(merged["port"], 8080);
    }

    #[test]
    fn the_resources_object_is_stored_and_reapplied_wholesale() {
        let base = map(json!({
            "image": { "tag": "1.0" },
            "resources": { "limits": { "memory": "1Gi" } }
        }));
        let client = map(json!({ "resources": {} }));
        let mut values = merge_values(&serde_json::Value::Object(base.clone()), Some(&client));
        replace_resource_values(&mut values, &client);
        let stored = stored_override_values(&base, &values, true);
        assert_eq!(stored["resources"], json!({}));
        assert!(stored.get("image").is_none());

        let merged = upgrade_merged_values(
            &map(json!({ "resources": { "limits": { "memory": "1Gi" } } })),
            &stored,
            &map(json!({})),
        );
        assert_eq!(
            merged["resources"],
            json!({}),
            "a cleared resource object must not be refilled from the new chart defaults"
        );
    }

    #[test]
    fn platform_owned_values_are_not_persisted() {
        let base = map(json!({ "port": 80 }));
        let values = map(json!({
            "port": 8080,
            "datasets": [{ "name": "media" }],
            "datasetsClaim": "naslos-ds-app-media",
            "datasetsClaims": ["naslos-ds-app-media"],
            "hostPath": "/var/mnt/tank/media",
            "naslosEnvChecksum": "abc"
        }));
        let stored = stored_override_values(&base, &values, false);
        assert_eq!(stored["port"], 8080);
        for key in [
            "datasets",
            "datasetsClaim",
            "datasetsClaims",
            "hostPath",
            "naslosEnvChecksum",
        ] {
            assert!(stored.get(key).is_none(), "{key} is platform-owned");
        }
    }
}

#[cfg(test)]
mod host_uniqueness_tests {
    use super::{exposure_host, host_owner, Exposure, Record, Store};

    fn record(name: &str, subdomain: &str, base_domain: &str) -> Record {
        Record {
            name: name.into(),
            exposure: Exposure {
                subdomain: subdomain.into(),
                ..Exposure::default()
            },
            base_domain: base_domain.into(),
            ..Record::default()
        }
    }

    #[test]
    fn exposure_host_resolves_the_default_base_and_normalizes_case() {
        assert_eq!(
            exposure_host("Media", "Example.COM", "apps.example"),
            "media.example.com"
        );
        assert_eq!(
            exposure_host("media", "", "apps.example"),
            "media.apps.example"
        );
        assert_eq!(exposure_host("", "example.com", "apps.example"), "");
        assert_eq!(
            exposure_host("media", "example.com", ""),
            "media.example.com"
        );
    }

    #[test]
    fn a_host_claimed_by_another_record_names_the_owner() {
        let records = [
            record("plex", "media", "example.com"),
            record("sonarr", "tv", ""),
        ];
        let host = exposure_host("media", "", "example.com");
        assert_eq!(
            host_owner(records.iter(), "sonarr", &host, "example.com"),
            Some("plex")
        );
        // The same app may re-save its own host.
        assert_eq!(
            host_owner(records.iter(), "plex", &host, "example.com"),
            None
        );
        // A different base domain is a different host.
        let other = exposure_host("media", "other.example", "example.com");
        assert_eq!(
            host_owner(records.iter(), "sonarr", &other, "example.com"),
            None
        );
        // A catalog-default subdomain resolves against the record's domain.
        let defaulted = exposure_host("tv", "", "example.com");
        assert_eq!(
            host_owner(records.iter(), "plex", &defaulted, "example.com"),
            Some("sonarr")
        );
    }

    #[test]
    fn records_without_a_route_never_conflict() {
        let records = [record("internal", "", "")];
        assert_eq!(host_owner(records.iter(), "plex", "", "example.com"), None);
    }

    #[test]
    fn concurrent_installs_cannot_claim_the_same_host() {
        // Two installs for different names hold different per-app locks, so
        // only the store-level check-and-insert can make this exclusive (S-8).
        let store = std::sync::Arc::new(Store::new(""));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = ["one", "two"]
            .into_iter()
            .map(|name| {
                let store = store.clone();
                let barrier = barrier.clone();
                let name = name.to_string();
                std::thread::spawn(move || {
                    barrier.wait();
                    store.upsert_unique_host(
                        Record {
                            name,
                            exposure: Exposure {
                                subdomain: "media".into(),
                                ..Exposure::default()
                            },
                            base_domain: "example.com".into(),
                            ..Record::default()
                        },
                        "example.com",
                    )
                })
            })
            .collect();
        let results: Vec<Result<(), String>> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert_eq!(
            results.iter().filter(|result| result.is_ok()).count(),
            1,
            "{results:?}"
        );
        let error = results
            .iter()
            .find_map(|result| result.as_ref().err())
            .expect("one install must lose the host claim");
        assert!(error.contains("already claimed by app"), "{error}");
        assert_eq!(store.list().len(), 1);
    }

    #[test]
    fn an_exposure_update_cannot_move_onto_another_apps_host() {
        let store = Store::new("");
        store
            .upsert(record("plex", "media", "example.com"))
            .unwrap();
        store.upsert(record("sonarr", "tv", "example.com")).unwrap();
        let error = store
            .update_unique_host("sonarr", "example.com", |current| {
                current.exposure.subdomain = "media".into();
            })
            .unwrap_err();
        assert!(error.contains("already claimed by app \"plex\""), "{error}");
        assert_eq!(
            store.get("sonarr").unwrap().exposure.subdomain,
            "tv",
            "a conflicting update must be rolled back"
        );
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
    use super::{
        prune_null_resources, quantity_number, replace_resource_values, resource_totals,
        validate_resource_quantities,
    };
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
    fn resources_replace_instead_of_deep_merging_so_omitted_keys_clear() {
        let mut stored = values(json!({
            "requests": { "cpu": "100m", "memory": "256Mi" },
            "limits": { "cpu": "1", "memory": "1Gi" }
        }));
        let incoming = json!({
            "resources": { "requests": { "cpu": "200m" } }
        });
        let incoming = incoming.as_object().unwrap();
        replace_resource_values(&mut stored, incoming);
        prune_null_resources(&mut stored);
        assert_eq!(stored["resources"]["requests"]["cpu"], json!("200m"));
        assert!(
            stored["resources"]["requests"].get("memory").is_none(),
            "an omitted memory request must be cleared"
        );
        assert!(
            stored["resources"].get("limits").is_none(),
            "the complete object replaces the stored resources"
        );
    }

    #[test]
    fn clearing_all_resources_replaces_the_object_with_an_empty_one() {
        let mut stored = values(json!({
            "requests": { "cpu": "100m" },
            "limits": { "memory": "1Gi" }
        }));
        let incoming = json!({ "resources": {} });
        replace_resource_values(&mut stored, incoming.as_object().unwrap());
        prune_null_resources(&mut stored);
        assert_eq!(stored["resources"], json!({}));
        assert!(stored["resources"].get("requests").is_none());
        assert!(stored["resources"].get("limits").is_none());

        let incoming = json!({ "resources": null });
        replace_resource_values(&mut stored, incoming.as_object().unwrap());
        assert!(stored.get("resources").is_none());
    }

    #[test]
    fn cleared_resource_fields_are_removed_and_null_is_tolerated() {
        let mut map = values(json!({
            "requests": { "cpu": null, "memory": "256Mi" },
            "limits": { "cpu": "2", "memory": null },
            "postgres": { "limits": { "cpu": null, "memory": "1Gi" } }
        }));
        prune_null_resources(&mut map);
        assert!(validate_resource_quantities(&map).is_ok());
        assert_eq!(map["resources"]["requests"]["memory"], json!("256Mi"));
        assert_eq!(map["resources"]["limits"]["cpu"], json!("2"));
        assert!(map["resources"]["requests"].get("cpu").is_none());
        assert!(map["resources"]["postgres"]["limits"].get("cpu").is_none());
        assert_eq!(
            map["resources"]["postgres"]["limits"]["memory"],
            json!("1Gi")
        );
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

#[cfg(test)]
mod lifecycle_hardening_tests {
    use super::{
        orphaned_objects, redact_record_with_catalog, restore_redacted_values, secret_value_keys,
        secret_value_pointers, Record, Store,
    };
    use crate::catalog::App;
    use serde_json::json;
    use std::collections::HashSet;

    fn nested_secret_app() -> App {
        App {
            name: "demo".into(),
            schema: json!({
                "type": "object",
                "properties": {
                    "password": { "type": "string", "format": "password" },
                    "postgresql": {
                        "type": "object",
                        "properties": {
                            "auth": {
                                "type": "object",
                                "properties": {
                                    "password": { "type": "string", "format": "password" }
                                }
                            }
                        }
                    },
                    "smtp": {
                        "type": "object",
                        "properties": {
                            "credentials": {
                                "type": "object",
                                "properties": {
                                    "token": { "type": "string", "writeOnly": true }
                                }
                            }
                        }
                    },
                    "user": { "type": "string" }
                }
            }),
            ..Default::default()
        }
    }

    fn nested_record() -> Record {
        Record {
            name: "demo".into(),
            values: json!({
                "password": "top",
                "postgresql": { "auth": { "password": "pg" } },
                "smtp": { "credentials": { "token": "smtp" } },
                "user": "admin"
            })
            .as_object()
            .cloned()
            .unwrap(),
            ..Default::default()
        }
    }

    #[test]
    fn schema_secrets_are_json_pointer_paths_at_every_depth() {
        let pointers = secret_value_pointers(&nested_secret_app().schema);
        assert_eq!(
            pointers,
            vec![
                "/password".to_string(),
                "/postgresql/auth/password".to_string(),
                "/smtp/credentials/token".to_string()
            ]
        );
        let keys = secret_value_keys(&nested_secret_app().schema);
        assert!(keys.contains("password"));
        assert!(keys.contains("token"));
        assert!(!keys.contains("user"));
    }

    #[test]
    fn nested_secrets_are_redacted_at_their_own_path() {
        let app = nested_secret_app();
        let mut record = nested_record();
        let (redacted, omitted) = redact_record_with_catalog(&mut record, Some(&app));
        assert!(!omitted);
        assert_eq!(record.values["password"], json!(null));
        assert_eq!(record.values["postgresql"]["auth"]["password"], json!(null));
        assert_eq!(record.values["smtp"]["credentials"]["token"], json!(null));
        assert_eq!(record.values["user"], json!("admin"));
        assert!(redacted.contains(&"password".to_string()));
        assert!(redacted.contains(&"/postgresql/auth/password".to_string()));
        assert!(redacted.contains(&"/smtp/credentials/token".to_string()));
        assert_eq!(
            record.redacted_keys,
            vec![
                "/password".to_string(),
                "/postgresql/auth/password".to_string(),
                "/smtp/credentials/token".to_string()
            ]
        );
    }

    #[test]
    fn a_missing_catalog_omits_values_instead_of_leaking_them() {
        let mut record = nested_record();
        let (redacted, omitted) = redact_record_with_catalog(&mut record, None);
        assert!(omitted);
        assert!(redacted.is_empty());
        assert!(record.values.is_empty());
    }

    #[test]
    fn persisted_pointer_list_redacts_without_a_catalog() {
        let mut record = nested_record();
        record.redacted_keys = vec![
            "/postgresql/auth/password".to_string(),
            "/smtp/credentials/token".to_string(),
        ];
        let (redacted, omitted) = redact_record_with_catalog(&mut record, None);
        assert!(!omitted);
        assert_eq!(record.values["postgresql"]["auth"]["password"], json!(null));
        assert_eq!(record.values["smtp"]["credentials"]["token"], json!(null));
        assert_eq!(record.values["user"], json!("admin"));
        assert_eq!(redacted.len(), 2);
    }

    #[test]
    fn persisted_pointers_still_redact_without_schema_secrets() {
        // A catalog entry exists but its schema declares no secret: the
        // persisted pointer list must not be ignored (S-3).
        let app = App {
            name: "demo".into(),
            schema: json!({
                "type": "object",
                "properties": { "user": { "type": "string" } }
            }),
            ..Default::default()
        };
        let mut record = nested_record();
        record.redacted_keys = vec!["/postgresql/auth/password".to_string()];
        let (redacted, omitted) = redact_record_with_catalog(&mut record, Some(&app));
        assert!(!omitted);
        assert_eq!(record.values["postgresql"]["auth"]["password"], json!(null));
        assert_eq!(
            record.values["password"],
            json!("top"),
            "only the persisted pointer is hidden"
        );
        assert!(redacted.contains(&"/postgresql/auth/password".to_string()));
        assert_eq!(
            record.redacted_keys,
            vec!["/postgresql/auth/password".to_string()],
            "a schema with no pointers does not erase the persisted list"
        );
    }

    #[test]
    fn schema_and_persisted_pointers_are_unioned() {
        let app = nested_secret_app();
        let mut record = nested_record();
        record.redacted_keys = vec!["/legacy/token".to_string()];
        let (redacted, _) = redact_record_with_catalog(&mut record, Some(&app));
        assert!(
            redacted.contains(&"password".to_string()),
            "a top-level secret keeps its short label: {redacted:?}"
        );
        assert!(record.redacted_keys.contains(&"/legacy/token".to_string()));
        assert!(record
            .redacted_keys
            .contains(&"/smtp/credentials/token".to_string()));
    }

    #[test]
    fn a_null_nested_secret_keeps_the_stored_value_on_upgrade() {
        let stored = nested_record().values;
        let mut merged = json!({
            "postgresql": { "auth": { "password": null } },
            "smtp": { "credentials": { "token": null } },
            "user": "operator"
        })
        .as_object()
        .cloned()
        .unwrap();
        restore_redacted_values(
            &stored,
            None,
            &mut merged,
            &[
                "/postgresql/auth/password".to_string(),
                "/smtp/credentials/token".to_string(),
            ],
        );
        assert_eq!(merged["postgresql"]["auth"]["password"], json!("pg"));
        assert_eq!(merged["smtp"]["credentials"]["token"], json!("smtp"));
        assert_eq!(merged["user"], json!("operator"));
    }

    #[test]
    fn a_failed_store_save_restores_memory() {
        let dir = tempfile::tempdir().unwrap();
        let parent_is_file = dir.path().join("not-a-directory");
        std::fs::write(&parent_is_file, b"x").unwrap();
        let path = parent_is_file.join("apps.json").display().to_string();
        let store = Store::new(&path);
        let mut rec = Record {
            name: "demo".into(),
            values: json!({ "password": "secret" })
                .as_object()
                .cloned()
                .unwrap(),
            ..Default::default()
        };
        assert!(store.upsert(rec.clone()).is_err());
        assert!(
            store.get("demo").is_err(),
            "memory must not run ahead of disk"
        );
        rec.values.clear();
        rec.last_error = "new".into();
        // A successful path then a failing update must also restore.
        let ok_dir = tempfile::tempdir().unwrap();
        let state_dir = ok_dir.path().join("state");
        std::fs::create_dir(&state_dir).unwrap();
        let ok = Store::new(&format!("{}/apps.json", state_dir.display()));
        ok.upsert(rec.clone()).unwrap();
        std::fs::remove_dir_all(&state_dir).unwrap();
        std::fs::write(&state_dir, b"blocker").unwrap();
        assert!(ok
            .update("demo", |r| r.last_error = "changed".into())
            .is_err());
        assert_eq!(ok.get("demo").unwrap().last_error, "new");
        assert!(ok.delete("demo").is_err());
        assert_eq!(ok.get("demo").unwrap().last_error, "new");
    }

    #[test]
    fn exposure_updates_do_not_clobber_unrelated_fields() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(&format!("{}/apps.json", dir.path().display()));
        let rec = Record {
            name: "demo".into(),
            values: json!({ "port": 8080 }).as_object().cloned().unwrap(),
            ..Default::default()
        };
        store.upsert(rec).unwrap();
        store
            .update("demo", |current| {
                current.exposure.subdomain = "demo".into();
                current.updated_at = Some(chrono::Utc::now());
            })
            .unwrap();
        let after = store.get("demo").unwrap();
        assert_eq!(after.values["port"], json!(8080));
        assert_eq!(after.exposure.subdomain, "demo");
    }

    #[test]
    fn orphan_reconciliation_only_returns_unkept_labeled_objects() {
        let list = json!({
            "items": [
                { "metadata": { "name": "naslos-ds-old-media", "labels": { "naslos.local/app": "old" } } },
                { "metadata": { "name": "naslos-ds-live-media", "labels": { "naslos.local/app": "live" } } },
                { "metadata": { "name": "operator-claim", "labels": {} } }
            ]
        });
        let keep: HashSet<String> = HashSet::from(["live".to_string()]);
        let orphans = orphaned_objects(&list, "naslos.local/app", &keep);
        assert_eq!(
            orphans,
            vec![("naslos-ds-old-media".to_string(), "old".to_string())]
        );
    }

    #[test]
    fn source_pin_is_recorded_on_the_record() {
        let rec: Record = serde_json::from_value(json!({
            "name": "demo",
            "source": "mine",
            "channel": "Prod",
            "chartVersion": "1.2.3",
            "secretPaths": ["/postgresql/auth/password"]
        }))
        .unwrap();
        assert_eq!(rec.source, "mine");
        assert_eq!(rec.redacted_keys, vec!["/postgresql/auth/password"]);
        let encoded = serde_json::to_value(&rec).unwrap();
        assert_eq!(encoded["source"], "mine");
        assert_eq!(encoded["secretPaths"][0], "/postgresql/auth/password");
    }
}
