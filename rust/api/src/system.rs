//! Release facts, update checks and the upgrader Job contract (spec FR-UPD).
//!
//! The API never upgrades itself. It resolves the newest eligible release from
//! GitHub (or a mirror), verifies the pack, and creates a Job that runs
//! `naslos-upgrade` outside the API pod. Everything here is deterministic and
//! unit-tested so the Job spec, the stepping rule and the lock cannot drift.

use crate::semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const DEFAULT_SOURCE: &str = "https://api.github.com/repos/AessemOps/Naslos-Linux/releases";
pub const CHECK_FILE: &str = "check.json";
pub const OPERATION_CONFIGMAP: &str = "naslos-system-operation";
pub const OPERATION_PREFIX: &str = "naslos-system-op-";

/// What the installed pack pinned (rendered by the chart from `release.*`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReleaseFacts {
    pub profile: String,
    pub naslos_version: String,
    pub chart_version: String,
    pub talos_version: String,
    pub schematic_id: String,
    pub kubernetes_version: String,
}

impl ReleaseFacts {
    pub fn from_env() -> Self {
        let get = |key: &str| std::env::var(key).unwrap_or_default();
        Self {
            profile: get("NASLOS_RELEASE_PROFILE"),
            naslos_version: get("NASLOS_RELEASE_VERSION"),
            chart_version: get("NASLOS_RELEASE_CHART_VERSION"),
            talos_version: get("NASLOS_RELEASE_TALOS_VERSION"),
            schematic_id: get("NASLOS_RELEASE_SCHEMATIC_ID"),
            kubernetes_version: get("NASLOS_RELEASE_KUBERNETES_VERSION"),
        }
    }

    /// D2: only an installer-profile node may run an operation. A
    /// `make install-vm` deployment has no `release` block at all.
    pub fn upgradable(&self) -> bool {
        self.profile == "installer"
            && Version::parse(&self.naslos_version).is_some()
            && Version::parse(&self.talos_version).is_some()
            && Version::parse(&self.kubernetes_version).is_some()
    }
}

/// Chart-provided wiring for the upgrader Job.
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub release: ReleaseFacts,
    pub api_image: String,
    pub release_name: String,
    pub service_account: String,
    pub configmap: String,
    pub workload_namespace: String,
    pub talos_secret: String,
    pub state_pvc: String,
    pub backoff_limit: i64,
    pub ttl_seconds: i64,
}

impl Config {
    pub fn from_env() -> Self {
        let get = |key: &str| std::env::var(key).unwrap_or_default();
        let number = |key: &str, default: i64| {
            std::env::var(key)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(default)
        };
        Self {
            release: ReleaseFacts::from_env(),
            api_image: get("NASLOS_API_IMAGE"),
            release_name: {
                let name = get("PLATFORM_RELEASE");
                if name.is_empty() {
                    "naslos".to_string()
                } else {
                    name
                }
            },
            service_account: get("SYSTEM_UPGRADER_SERVICE_ACCOUNT"),
            configmap: get("SYSTEM_UPGRADER_CONFIGMAP"),
            workload_namespace: get("NASLOS_WORKLOAD_NAMESPACE"),
            talos_secret: get("SYSTEM_UPGRADER_TALOS_SECRET"),
            state_pvc: get("SYSTEM_UPGRADER_STATE_PVC"),
            backoff_limit: number("SYSTEM_UPGRADER_BACKOFF_LIMIT", 3),
            ttl_seconds: number("SYSTEM_UPGRADER_TTL_SECONDS", 3600),
        }
    }

    pub fn service_account(&self) -> &str {
        if self.service_account.is_empty() {
            "naslos-upgrader"
        } else {
            &self.service_account
        }
    }

    pub fn configmap(&self) -> &str {
        if self.configmap.is_empty() {
            OPERATION_CONFIGMAP
        } else {
            &self.configmap
        }
    }
}

/// One verified release the API may offer.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Candidate {
    pub version: String,
    pub notes_url: String,
    pub body: String,
    pub published_at: String,
    pub pack_url: String,
    pub pack_sha256: String,
    pub pack_path: String,
    pub talos_version: String,
    pub kubernetes_version: String,
    pub api_image: String,
}

impl Candidate {
    pub fn as_json(&self) -> serde_json::Value {
        serde_json::json!({
            "version": self.version,
            "notesUrl": self.notes_url,
            "body": self.body,
            "publishedAt": self.published_at,
            "talosVersion": self.talos_version,
            "kubernetesVersion": self.kubernetes_version,
            "apiImage": self.api_image,
        })
    }
}

/// The cached result of the last update check.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateCheck {
    pub checked_at: String,
    pub source: String,
    pub current: String,
    pub available: Option<Candidate>,
    pub blocked: String,
    pub error: String,
}

impl UpdateCheck {
    pub fn as_json(&self) -> serde_json::Value {
        serde_json::json!({
            "checkedAt": self.checked_at,
            "source": self.source,
            "current": self.current,
            "available": self.available.as_ref().map(Candidate::as_json),
            "blocked": self.blocked,
            "error": self.error,
        })
    }
}

/// Stepping rule (D5): each of Talos and Kubernetes may move at most one minor
/// per release. A larger jump is refused with a reason the UI can show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stepping {
    pub allowed: bool,
    pub reason: String,
}

pub fn stepping(
    current: &ReleaseFacts,
    candidate: &Candidate,
    running_talos: &str,
    running_kubernetes: &str,
) -> Stepping {
    let Some(candidate_version) = Version::parse(&candidate.version) else {
        return Stepping {
            allowed: false,
            reason: "the release version is not strict semver".into(),
        };
    };
    if let Some(current_version) = Version::parse(&current.naslos_version) {
        if candidate_version <= current_version {
            return Stepping {
                allowed: false,
                reason: format!(
                    "{} is not newer than {}",
                    candidate.version, current.naslos_version
                ),
            };
        }
    }
    for (name, target, running) in [
        ("Talos", &candidate.talos_version, running_talos),
        (
            "Kubernetes",
            &candidate.kubernetes_version,
            running_kubernetes,
        ),
    ] {
        if running.is_empty() {
            continue;
        }
        let (Some(target), Some(running)) = (Version::parse(target), Version::parse(running))
        else {
            continue;
        };
        let distance = target.minor_distance(&running);
        if distance < 0 {
            return Stepping {
                allowed: false,
                reason: format!("{name} {target} would be a downgrade from {running}"),
            };
        }
        if distance > 1 {
            return Stepping {
                allowed: false,
                reason: format!(
                    "{name} {target} is {distance} minors ahead of {running}; upgrade to the next release first"
                ),
            };
        }
    }
    Stepping {
        allowed: true,
        reason: String::new(),
    }
}

/// Release lookup + verified pack cache.
pub struct Checker {
    source: String,
    cache_dir: PathBuf,
    facts: ReleaseFacts,
    client: reqwest::Client,
    interval: Duration,
}

#[derive(Debug, Deserialize)]
struct ReleaseEntry {
    // GitHub returns null for these fields on some drafts/edge cases; a null
    // must not make the whole update check fail (the release is simply skipped
    // when tag_name is absent).
    #[serde(default)]
    tag_name: Option<String>,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    assets: Option<Vec<Asset>>,
}

#[derive(Debug, Deserialize)]
struct Asset {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    browser_download_url: Option<String>,
}

impl Checker {
    /// Build from env; `None` when `SYSTEM_UPDATES_ENABLED=false`.
    pub fn from_env() -> Option<Self> {
        let enabled = std::env::var("SYSTEM_UPDATES_ENABLED")
            .map(|v| v != "false")
            .unwrap_or(true);
        if !enabled {
            return None;
        }
        let facts = ReleaseFacts::from_env();
        // D2: only an installer-profile node is web-upgradable. Do not even
        // poll GitHub on a make-install-vm deployment.
        if facts.profile != "installer" {
            return None;
        }
        let source =
            std::env::var("SYSTEM_UPDATES_SOURCE").unwrap_or_else(|_| DEFAULT_SOURCE.to_string());
        let cache_dir = std::env::var("SYSTEM_UPDATES_CACHE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/var/lib/naslos/updates"));
        let interval = std::env::var("SYSTEM_UPDATES_INTERVAL_HOURS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|n| *n > 0)
            .map(|h| Duration::from_secs(h * 3600))
            .unwrap_or(Duration::from_secs(24 * 3600));
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .ok()?;
        Some(Self {
            source,
            cache_dir,
            facts,
            client,
            interval,
        })
    }

    pub fn interval(&self) -> Duration {
        self.interval
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn facts(&self) -> &ReleaseFacts {
        &self.facts
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    pub fn load_cached(&self) -> Option<UpdateCheck> {
        let raw = std::fs::read(self.cache_dir.join(CHECK_FILE)).ok()?;
        serde_json::from_slice(&raw).ok()
    }

    pub fn cached_candidate(&self, version: &str) -> Option<Candidate> {
        self.load_cached()
            .and_then(|c| c.available)
            .filter(|c| c.version == version)
    }

    /// Fetch releases, verify the newest pack whose system pins are within one
    /// minor of the installed release, and cache it.
    pub async fn check(&self) -> UpdateCheck {
        match self.check_inner().await {
            Ok(check) => {
                if let Err(e) = self.save(&check) {
                    tracing::warn!("saving the update check: {e}");
                }
                check
            }
            Err(e) => {
                let mut check = self.load_cached().unwrap_or_else(|| UpdateCheck {
                    source: self.source.clone(),
                    current: self.facts.naslos_version.clone(),
                    ..Default::default()
                });
                check.error = e;
                check.checked_at = now();
                check
            }
        }
    }

    async fn check_inner(&self) -> Result<UpdateCheck, String> {
        let releases: Vec<ReleaseEntry> = self.fetch_releases().await?;
        let mut newer: Vec<(Version, ReleaseEntry)> = releases
            .into_iter()
            .filter_map(|r| {
                let version = r.tag_name.as_deref().and_then(Version::parse_tag)?;
                Some((version, r))
            })
            .filter(|(v, _)| {
                Version::parse(&self.facts.naslos_version)
                    .map(|current| *v > current)
                    .unwrap_or(true)
            })
            .collect();
        newer.sort_by_key(|a| std::cmp::Reverse(a.0));

        let mut blocked = String::new();
        for (version, release) in newer.iter().take(8) {
            let Some((pack_url, pack_sha_url)) = pack_assets(release, version) else {
                blocked = format!("release {} has no install pack", version.as_tag());
                continue;
            };
            let pack_sha = match self.download_text(&pack_sha_url).await {
                Ok(text) => text
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_ascii_lowercase(),
                Err(e) => {
                    blocked = format!("release {}: {e}", version.as_tag());
                    continue;
                }
            };
            if pack_sha.len() != 64 || !pack_sha.bytes().all(|b| b.is_ascii_hexdigit()) {
                blocked = format!("release {} has an invalid pack sha256", version.as_tag());
                continue;
            }
            let bytes = match self.download_bytes(&pack_url).await {
                Ok(bytes) => bytes,
                Err(e) => {
                    blocked = format!("release {}: {e}", version.as_tag());
                    continue;
                }
            };
            let got = crate::pack::hex_sha256(&bytes);
            if got != pack_sha {
                blocked = format!(
                    "release {}: pack sha256 mismatch (got {got})",
                    version.as_tag()
                );
                continue;
            }
            let meta = match crate::pack::verify_pack(&bytes) {
                Ok(meta) => meta,
                Err(e) => {
                    blocked = format!("release {}: {e}", version.as_tag());
                    continue;
                }
            };
            if meta.naslos_version != version.to_string() {
                blocked = format!(
                    "release {} packs naslosVersion {}",
                    version.as_tag(),
                    meta.naslos_version
                );
                continue;
            }
            let candidate = Candidate {
                version: version.to_string(),
                notes_url: release.html_url.clone().unwrap_or_default(),
                body: release.body.clone().unwrap_or_default(),
                published_at: release.published_at.clone().unwrap_or_default(),
                pack_url,
                pack_sha256: pack_sha,
                pack_path: self
                    .cache_dir
                    .join(format!("pack-{version}.tar.gz"))
                    .display()
                    .to_string(),
                talos_version: meta.talos_version.clone(),
                kubernetes_version: meta.kubernetes_version.clone(),
                api_image: meta.api_image().unwrap_or_default().to_string(),
            };
            let decision = stepping(
                &self.facts,
                &candidate,
                &self.facts.talos_version,
                &self.facts.kubernetes_version,
            );
            if !decision.allowed {
                blocked = decision.reason;
                continue;
            }
            if let Err(e) = self.store_pack(&candidate, &bytes) {
                blocked = e;
                continue;
            }
            return Ok(UpdateCheck {
                checked_at: now(),
                source: self.source.clone(),
                current: self.facts.naslos_version.clone(),
                available: Some(candidate),
                blocked: String::new(),
                error: String::new(),
            });
        }

        Ok(UpdateCheck {
            checked_at: now(),
            source: self.source.clone(),
            current: self.facts.naslos_version.clone(),
            available: None,
            blocked: if blocked.is_empty() {
                "no newer release".into()
            } else {
                blocked
            },
            error: String::new(),
        })
    }

    async fn fetch_releases(&self) -> Result<Vec<ReleaseEntry>, String> {
        let url = releases_url(&self.source);
        let body = self.download_text(&url).await?;
        serde_json::from_str(&body).map_err(|e| format!("parsing releases from {url}: {e}"))
    }

    async fn download_text(&self, url: &str) -> Result<String, String> {
        let resp = self
            .client
            .get(url)
            .header("User-Agent", "naslos-api")
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(|e| format!("GET {url}: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("GET {url}: HTTP {}", resp.status()));
        }
        resp.text().await.map_err(|e| format!("reading {url}: {e}"))
    }

    async fn download_bytes(&self, url: &str) -> Result<Vec<u8>, String> {
        let resp = self
            .client
            .get(url)
            .header("User-Agent", "naslos-api")
            .send()
            .await
            .map_err(|e| format!("GET {url}: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("GET {url}: HTTP {}", resp.status()));
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| format!("reading {url}: {e}"))?;
        if bytes.len() > 512 * 1024 * 1024 {
            return Err(format!("GET {url}: pack is larger than 512 MiB"));
        }
        Ok(bytes.to_vec())
    }

    fn store_pack(&self, candidate: &Candidate, bytes: &[u8]) -> Result<(), String> {
        std::fs::create_dir_all(&self.cache_dir)
            .map_err(|e| format!("creating {}: {e}", self.cache_dir.display()))?;
        let path = Path::new(&candidate.pack_path);
        let tmp = path.with_extension("tar.gz.tmp");
        std::fs::write(&tmp, bytes).map_err(|e| format!("writing {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("renaming {}: {e}", tmp.display()))?;
        // Keep only the offered pack: the state PVC is small and an older
        // candidate is never run once a newer check has replaced it.
        if let Ok(entries) = std::fs::read_dir(&self.cache_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("pack-") && name.ends_with(".tar.gz") && entry.path() != path {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        Ok(())
    }

    fn save(&self, check: &UpdateCheck) -> Result<(), String> {
        std::fs::create_dir_all(&self.cache_dir)
            .map_err(|e| format!("creating {}: {e}", self.cache_dir.display()))?;
        let path = self.cache_dir.join(CHECK_FILE);
        let tmp = path.with_extension("json.tmp");
        let raw = serde_json::to_vec_pretty(check).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, raw).map_err(|e| format!("writing {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("renaming {}: {e}", tmp.display()))
    }
}

/// GitHub API vs static mirror (a drill can serve `releases.json`).
pub fn releases_url(source: &str) -> String {
    if source.ends_with(".json") {
        return source.to_string();
    }
    if source.contains("api.github.com") {
        return format!("{source}?per_page=100");
    }
    format!("{}/releases.json", source.trim_end_matches('/'))
}

fn pack_assets(release: &ReleaseEntry, version: &Version) -> Option<(String, String)> {
    let pack = format!("naslos-install-pack-{version}.tar.gz");
    let mut pack_url = None;
    let mut sha_url = None;
    for asset in release.assets.as_deref().unwrap_or_default() {
        let Some(name) = asset.name.as_deref() else {
            continue;
        };
        let Some(url) = asset.browser_download_url.as_deref() else {
            continue;
        };
        if url.is_empty() {
            continue;
        }
        if name == pack {
            pack_url = Some(url.to_string());
        } else if name == format!("{pack}.sha256") {
            sha_url = Some(url.to_string());
        }
    }
    Some((pack_url?, sha_url?))
}

/// Whether a ConfigMap phase is an active lock.
pub fn active_phase(phase: &str) -> bool {
    matches!(phase, "pending" | "running" | "rebooting")
}

/// Operation kinds the API accepts.
pub fn valid_op(op: &str) -> bool {
    matches!(op, "naslos" | "talos" | "kubernetes")
}

/// Build the upgrader Job object. Kept pure so the contract is unit-tested.
pub struct JobOptions<'a> {
    pub name: &'a str,
    pub namespace: &'a str,
    pub image: &'a str,
    pub op: &'a str,
    pub operation_id: &'a str,
    pub from: &'a str,
    pub version: &'a str,
    pub pack_url: &'a str,
    pub pack_sha256: &'a str,
    pub release_name: &'a str,
    pub service_account: &'a str,
    pub talos_secret: &'a str,
    pub state_pvc: &'a str,
    pub workload_namespace: &'a str,
    pub backoff_limit: i64,
    pub ttl_seconds: i64,
}

pub fn job_spec(o: &JobOptions<'_>) -> serde_json::Value {
    let mut volumes = vec![serde_json::json!({"name": "tmp", "emptyDir": {}})];
    let mut mounts = vec![serde_json::json!({"name": "tmp", "mountPath": "/tmp"})];
    if !o.talos_secret.is_empty() {
        volumes.push(serde_json::json!({
            "name": "talosconfig",
            "secret": {"secretName": o.talos_secret, "items": [{"key": "talosconfig", "path": "config"}]}
        }));
        mounts.push(serde_json::json!({
            "name": "talosconfig",
            "mountPath": "/var/run/secrets/talos.dev",
            "readOnly": true
        }));
    }
    if !o.state_pvc.is_empty() {
        volumes.push(serde_json::json!({
            "name": "state",
            "persistentVolumeClaim": {"claimName": o.state_pvc}
        }));
        mounts.push(serde_json::json!({"name": "state", "mountPath": "/var/lib/naslos"}));
    } else {
        volumes.push(serde_json::json!({"name": "state", "emptyDir": {}}));
        mounts.push(serde_json::json!({"name": "state", "mountPath": "/var/lib/naslos"}));
    }

    serde_json::json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": {
            "name": o.name,
            "namespace": o.namespace,
            "labels": {
                "app.kubernetes.io/name": "naslos-system-op",
                "app.kubernetes.io/component": "upgrader",
                "naslos.io/operation-id": o.operation_id,
                "naslos.io/op": o.op,
                "naslos.io/from": o.from,
                "naslos.io/to": o.version,
            }
        },
        "spec": {
            "backoffLimit": o.backoff_limit,
            "ttlSecondsAfterFinished": o.ttl_seconds,
            "template": {
                "metadata": {
                    "labels": {
                        "app.kubernetes.io/name": "naslos-system-op",
                        "naslos.io/operation-id": o.operation_id,
                        "naslos.io/op": o.op,
                    }
                },
                "spec": {
                    "serviceAccountName": o.service_account,
                    "restartPolicy": "Never",
                    "securityContext": {
                        "runAsNonRoot": true,
                        "runAsUser": 65532,
                        "runAsGroup": 65532,
                        "fsGroup": 65532,
                        "seccompProfile": {"type": "RuntimeDefault"}
                    },
                    "containers": [{
                        "name": "upgrade",
                        "image": o.image,
                        "imagePullPolicy": "IfNotPresent",
                        "command": ["naslos-upgrade"],
                        "args": [
                            "--op", o.op,
                            "--operation-id", o.operation_id,
                            "--from", o.from,
                            "--version", o.version,
                            "--pack-url", o.pack_url,
                            "--sha256", o.pack_sha256,
                            "--namespace", o.namespace,
                            "--release", o.release_name,
                        ],
                        "env": [
                            {"name": "NASLOS_NAMESPACE", "value": o.namespace},
                            {"name": "NASLOS_RELEASE", "value": o.release_name},
                            {"name": "NASLOS_WORKLOAD_NAMESPACE", "value": o.workload_namespace},
                            {"name": "NASLOS_STATE_DIR", "value": "/var/lib/naslos"},
                            {"name": "TALOSCONFIG", "value": "/var/run/secrets/talos.dev/config"},
                            {"name": "HOME", "value": "/tmp"},
                            {"name": "HELM_CACHE_HOME", "value": "/tmp/helm/cache"},
                            {"name": "HELM_CONFIG_HOME", "value": "/tmp/helm/config"},
                            {"name": "HELM_DATA_HOME", "value": "/tmp/helm/data"},
                        ],
                        "volumeMounts": mounts,
                        "securityContext": {
                            "allowPrivilegeEscalation": false,
                            "readOnlyRootFilesystem": true,
                            "capabilities": {"drop": ["ALL"]}
                        },
                        "resources": {
                            "requests": {"cpu": "50m", "memory": "128Mi"},
                            "limits": {"cpu": "1", "memory": "512Mi"}
                        }
                    }],
                    "volumes": volumes
                }
            }
        }
    })
}

/// A DNS-label-safe operation id: unix seconds + 8 hex characters.
pub fn new_operation_id() -> String {
    let mut random = [0u8; 4];
    let _ = getrandom::getrandom(&mut random);
    let suffix: String = random.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-{}", chrono::Utc::now().timestamp(), suffix)
}

pub fn operation_job_name(id: &str) -> String {
    format!("{OPERATION_PREFIX}{id}")
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The cached pack's sha256 for a candidate (the API pins the approved value).
pub fn approved_sha(candidate: &Candidate) -> String {
    candidate.pack_sha256.clone()
}

/// Talos installer image for a pack (used by the `talos` operation).
pub fn talos_installer_image(schematic: &str, version: &str) -> String {
    format!("factory.talos.dev/installer/{schematic}:{version}")
}

/// Pack component images as a map (for the post-upgrade digest check).
pub fn expected_images(meta: &crate::pack::PackMetadata) -> HashMap<String, String> {
    meta.images.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> ReleaseFacts {
        ReleaseFacts {
            profile: "installer".into(),
            naslos_version: "0.3.0".into(),
            chart_version: "0.3.0".into(),
            talos_version: "v1.14.1".into(),
            schematic_id: "abc".into(),
            kubernetes_version: "v1.37.0".into(),
        }
    }

    fn candidate() -> Candidate {
        Candidate {
            version: "0.3.1".into(),
            talos_version: "v1.14.2".into(),
            kubernetes_version: "v1.37.1".into(),
            api_image: "ghcr.io/aessemops/naslos-api@sha256:dead".into(),
            ..Default::default()
        }
    }

    #[test]
    fn profile_gate() {
        assert!(facts().upgradable());
        let mut vm = facts();
        vm.profile.clear();
        assert!(!vm.upgradable());
    }

    #[test]
    fn stepping_allows_one_minor_and_refuses_more() {
        let mut c = candidate();
        c.talos_version = "v1.15.0".into();
        c.kubernetes_version = "v1.38.0".into();
        assert!(stepping(&facts(), &c, "v1.14.1", "v1.37.0").allowed);

        c.talos_version = "v1.16.0".into();
        let s = stepping(&facts(), &c, "v1.14.1", "v1.37.0");
        assert!(!s.allowed);
        assert!(s.reason.contains("Talos"));
    }

    #[test]
    fn stepping_refuses_a_downgrade() {
        let mut c = candidate();
        c.kubernetes_version = "v1.36.0".into();
        let s = stepping(&facts(), &c, "v1.14.1", "v1.37.0");
        assert!(!s.allowed);
        assert!(s.reason.contains("downgrade"));
    }

    #[test]
    fn lock_phases() {
        assert!(active_phase("running"));
        assert!(active_phase("rebooting"));
        assert!(!active_phase("succeeded"));
        assert!(!active_phase("failed"));
    }

    #[test]
    fn job_spec_is_locked_down() {
        let spec = job_spec(&JobOptions {
            name: "naslos-system-op-1-ab",
            namespace: "naslos",
            image: "ghcr.io/aessemops/naslos-api@sha256:dead",
            op: "naslos",
            operation_id: "1-ab",
            from: "0.3.0",
            version: "0.3.1",
            pack_url: "https://example/pack.tar.gz",
            pack_sha256: "abc",
            release_name: "naslos",
            service_account: "naslos-upgrader",
            talos_secret: "naslos-talosconfig",
            state_pvc: "naslos-shares-config",
            workload_namespace: "naslos-privileged",
            backoff_limit: 3,
            ttl_seconds: 3600,
        });
        let pod = &spec["spec"]["template"]["spec"];
        assert_eq!(pod["serviceAccountName"], "naslos-upgrader");
        assert_eq!(pod["containers"][0]["command"][0], "naslos-upgrade");
        assert_eq!(
            pod["containers"][0]["image"],
            "ghcr.io/aessemops/naslos-api@sha256:dead"
        );
        assert_eq!(
            pod["containers"][0]["securityContext"]["readOnlyRootFilesystem"],
            true
        );
        assert_eq!(
            pod["containers"][0]["volumeMounts"][1]["name"],
            "talosconfig"
        );
        assert_eq!(pod["containers"][0]["volumeMounts"][2]["name"], "state");
        assert_eq!(spec["metadata"]["labels"]["naslos.io/operation-id"], "1-ab");
    }

    #[test]
    fn null_release_fields_do_not_break_the_check() {
        let json = r#"[
            {"tag_name": null, "html_url": null, "body": null, "published_at": null, "assets": null},
            {"tag_name": "v0.3.1", "html_url": "https://example/0.3.1", "body": null,
             "published_at": null, "assets": [
                {"name": "naslos-install-pack-0.3.1.tar.gz", "browser_download_url": "https://example/pack"},
                {"name": "naslos-install-pack-0.3.1.tar.gz.sha256", "browser_download_url": "https://example/sha"}
             ]}
        ]"#;
        let releases: Vec<ReleaseEntry> = serde_json::from_str(json).unwrap();
        let selected: Vec<_> = releases
            .into_iter()
            .filter_map(|r| {
                let version = r.tag_name.as_deref().and_then(Version::parse_tag)?;
                Some((version, r))
            })
            .collect();
        assert_eq!(selected.len(), 1);
        let (version, release) = &selected[0];
        assert_eq!(version.to_string(), "0.3.1");
        assert_eq!(release.html_url.as_deref(), Some("https://example/0.3.1"));
        let (pack, sha) = pack_assets(release, version).expect("pack assets");
        assert_eq!(pack, "https://example/pack");
        assert_eq!(sha, "https://example/sha");
    }

    #[test]
    fn releases_url_forms() {
        assert_eq!(
            releases_url("https://api.github.com/repos/A/B/releases"),
            "https://api.github.com/repos/A/B/releases?per_page=100"
        );
        assert_eq!(
            releases_url("http://mirror.local/naslos"),
            "http://mirror.local/naslos/releases.json"
        );
        assert_eq!(
            releases_url("http://mirror.local/releases.json"),
            "http://mirror.local/releases.json"
        );
    }

    #[test]
    fn operation_ids_are_dns_labels() {
        let id = new_operation_id();
        assert!(id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'));
        assert!(!id.ends_with('-'));
        assert_eq!(operation_job_name(&id), format!("naslos-system-op-{id}"));
    }
}
