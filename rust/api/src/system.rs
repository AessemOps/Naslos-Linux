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
/// Where the API caches the verified pack inside the state volume. The
/// upgrader Job mounts the same claim at `/var/lib/naslos`, so this path is
/// also the Job's `--pack-file` (C-1c, see `job_spec`).
pub const STATE_MOUNT: &str = "/var/lib/naslos";
/// Largest release metadata/text asset (releases JSON, sha256, signature).
pub const MAX_TEXT_BYTES: usize = 8 * 1024 * 1024;

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
    /// Detached signature asset, when the release publishes one (R-063).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pack_sig_url: String,
    /// Whether the pack signature verified against the configured key.
    #[serde(default)]
    pub pack_attested: bool,
    pub pack_path: String,
    /// Cached detached signature beside `pack_path`, when the release has one.
    /// Verifying it locally is what keeps the Job off the network (C-1c).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pack_sig_path: String,
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

/// Whether the running Talos/Kubernetes still lags the installed release pins.
pub fn system_steps_pending(release: &ReleaseFacts, talos: &str, kubernetes: &str) -> bool {
    let differs = |running: &str, pin: &str| {
        !running.is_empty() && Version::parse(running) != Version::parse(pin)
    };
    differs(talos, &release.talos_version) || differs(kubernetes, &release.kubernetes_version)
}

/// Stepping decision for the candidate the UI will act on. A candidate equal to
/// the installed release is the "finish the pending system steps" case, which
/// is allowed while Talos or Kubernetes still lag the release pins.
pub fn candidate_stepping(
    release: &ReleaseFacts,
    candidate: &Candidate,
    talos: &str,
    kubernetes: &str,
) -> Stepping {
    if Version::parse(&candidate.version) == Version::parse(&release.naslos_version) {
        pending_stepping(candidate, talos, kubernetes)
    } else {
        stepping(release, candidate, talos, kubernetes)
    }
}

fn pending_stepping(candidate: &Candidate, talos: &str, kubernetes: &str) -> Stepping {
    for (name, target, running) in [
        ("Talos", &candidate.talos_version, talos),
        ("Kubernetes", &candidate.kubernetes_version, kubernetes),
    ] {
        if running.is_empty() {
            continue;
        }
        let (Some(target), Some(running)) = (Version::parse(target), Version::parse(running))
        else {
            continue;
        };
        if target == running {
            continue;
        }
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
    /// Ed25519 public key for pack signatures; empty means "no signature
    /// verification configured" and the sha256 from the release is the
    /// trust anchor (documented residual, R-063).
    pack_pubkey: String,
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
        // I-7: every redirect hop is re-validated against `pack_url_allowed`,
        // not just the first URL the release lists.
        let client = pack_client(Duration::from_secs(300), &source).ok()?;
        let pack_pubkey = std::env::var("SYSTEM_UPDATES_PACK_PUBKEY")
            .unwrap_or_default()
            .trim()
            .to_string();
        Some(Self {
            source,
            cache_dir,
            facts,
            client,
            interval,
            pack_pubkey,
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

    pub fn pack_pubkey(&self) -> &str {
        &self.pack_pubkey
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

    /// Fetch releases, verify the newest eligible pack, and cache it.
    ///
    /// When the node already runs the newest release but its Talos or
    /// Kubernetes version still lags that release's pins, the installed
    /// release is offered as the candidate so the UI can finish the pending
    /// ordered steps (Naslos → Talos → Kubernetes). Without this a node that
    /// upgraded Naslos first would have no way to complete the system steps.
    pub async fn check(&self, talos: &str, kubernetes: &str) -> UpdateCheck {
        match self.check_inner(talos, kubernetes).await {
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

    async fn check_inner(&self, talos: &str, kubernetes: &str) -> Result<UpdateCheck, String> {
        let releases: Vec<ReleaseEntry> = self.fetch_releases().await?;
        let current = Version::parse(&self.facts.naslos_version);

        let mut newer: Vec<(Version, ReleaseEntry)> = Vec::new();
        let mut installed: Option<(Version, ReleaseEntry)> = None;
        for release in releases {
            let Some(version) = release.tag_name.as_deref().and_then(Version::parse_tag) else {
                continue;
            };
            if current.map(|c| version > c).unwrap_or(true) {
                newer.push((version, release));
            } else if current == Some(version) {
                installed = Some((version, release));
            }
        }
        newer.sort_by_key(|a| std::cmp::Reverse(a.0));
        newer.truncate(8);
        if system_steps_pending(&self.facts, talos, kubernetes) {
            if let Some(entry) = installed {
                newer.push(entry);
            }
        }

        let mut blocked = String::new();
        for (version, release) in &newer {
            let candidate = match self.fetch_candidate(version, release).await {
                Ok(candidate) => candidate,
                Err(e) => {
                    blocked = e;
                    continue;
                }
            };
            let decision = candidate_stepping(&self.facts, &candidate, talos, kubernetes);
            if !decision.allowed {
                blocked = decision.reason;
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

    /// Download, verify and build the candidate for one release. The verified
    /// pack is written to the cache only by the caller.
    async fn fetch_candidate(
        &self,
        version: &Version,
        release: &ReleaseEntry,
    ) -> Result<Candidate, String> {
        let (pack_url, pack_sha_url, pack_sig_url) = pack_assets(release, version)
            .ok_or_else(|| format!("release {} has no install pack", version.as_tag()))?;
        pack_url_allowed(&pack_url, &self.source)
            .map_err(|e| format!("release {}: {e}", version.as_tag()))?;
        let pack_sha = self
            .download_text(&pack_sha_url)
            .await
            .map(|text| {
                text.split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_ascii_lowercase()
            })
            .map_err(|e| format!("release {}: {e}", version.as_tag()))?;
        if pack_sha.len() != 64 || !pack_sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!(
                "release {} has an invalid pack sha256",
                version.as_tag()
            ));
        }
        let bytes = self
            .download_bytes(&pack_url)
            .await
            .map_err(|e| format!("release {}: {e}", version.as_tag()))?;
        let got = crate::pack::hex_sha256(&bytes);
        if got != pack_sha {
            return Err(format!(
                "release {}: pack sha256 mismatch (got {got})",
                version.as_tag()
            ));
        }
        // R-063: verify a detached signature whenever the release publishes
        // one, and fail closed if a key is configured but the release is
        // unsigned. The sha256 fetched from the same origin is not an
        // independent trust anchor on its own.
        let mut pack_attested = false;
        let mut signature: Option<String> = None;
        if let Some(sig_url) = pack_sig_url.as_deref() {
            if self.pack_pubkey.is_empty() {
                return Err(format!(
                    "release {} publishes a pack signature but SYSTEM_UPDATES_PACK_PUBKEY is not configured",
                    version.as_tag()
                ));
            }
            pack_url_allowed(sig_url, &self.source)
                .map_err(|e| format!("release {}: {e}", version.as_tag()))?;
            let text = self
                .download_text(sig_url)
                .await
                .map_err(|e| format!("release {}: {e}", version.as_tag()))?;
            crate::upgrade::verify_pack_signature(&bytes, &text, &self.pack_pubkey)
                .map_err(|e| format!("release {}: {e}", version.as_tag()))?;
            pack_attested = true;
            signature = Some(text);
        } else if !self.pack_pubkey.is_empty() {
            return Err(format!(
                "release {} has no pack signature but SYSTEM_UPDATES_PACK_PUBKEY is configured",
                version.as_tag()
            ));
        }
        let meta = crate::pack::verify_pack(&bytes)
            .map_err(|e| format!("release {}: {e}", version.as_tag()))?;
        if meta.naslos_version != version.to_string() {
            return Err(format!(
                "release {} packs naslosVersion {}",
                version.as_tag(),
                meta.naslos_version
            ));
        }
        let pack_path = self
            .cache_dir
            .join(format!("pack-{version}.tar.gz"))
            .display()
            .to_string();
        // The Job verifies the cached signature offline; keep the exact bytes
        // the API just checked (C-1c).
        let pack_sig_path = if signature.is_some() {
            format!("{pack_path}.sig")
        } else {
            String::new()
        };
        let candidate = Candidate {
            version: version.to_string(),
            notes_url: release.html_url.clone().unwrap_or_default(),
            body: release.body.clone().unwrap_or_default(),
            published_at: release.published_at.clone().unwrap_or_default(),
            pack_url,
            pack_sha256: pack_sha,
            pack_sig_url: pack_sig_url.unwrap_or_default(),
            pack_attested,
            pack_path,
            pack_sig_path,
            talos_version: meta.talos_version.clone(),
            kubernetes_version: meta.kubernetes_version.clone(),
            api_image: meta.api_image().unwrap_or_default().to_string(),
        };
        self.store_pack(&candidate, &bytes, signature.as_deref())?;
        Ok(candidate)
    }

    async fn fetch_releases(&self) -> Result<Vec<ReleaseEntry>, String> {
        let url = releases_url(&self.source);
        let body = self.download_text(&url).await?;
        serde_json::from_str(&body).map_err(|e| format!("parsing releases from {url}: {e}"))
    }

    async fn download_text(&self, url: &str) -> Result<String, String> {
        let bytes = download_capped(
            &self.client,
            url,
            "release metadata",
            MAX_TEXT_BYTES,
            "naslos-api",
        )
        .await?;
        String::from_utf8(bytes).map_err(|_| format!("GET {url}: body is not UTF-8"))
    }

    async fn download_bytes(&self, url: &str) -> Result<Vec<u8>, String> {
        download_capped(
            &self.client,
            url,
            "pack",
            crate::upgrade::MAX_PACK_BYTES,
            "naslos-api",
        )
        .await
    }

    fn store_pack(
        &self,
        candidate: &Candidate,
        bytes: &[u8],
        signature: Option<&str>,
    ) -> Result<(), String> {
        std::fs::create_dir_all(&self.cache_dir)
            .map_err(|e| format!("creating {}: {e}", self.cache_dir.display()))?;
        let path = Path::new(&candidate.pack_path);
        let tmp = path.with_extension("tar.gz.tmp");
        std::fs::write(&tmp, bytes).map_err(|e| format!("writing {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("renaming {}: {e}", tmp.display()))?;
        let sig_path = PathBuf::from(format!("{}.sig", path.display()));
        match signature {
            Some(text) => {
                let tmp = sig_path.with_extension("sig.tmp");
                std::fs::write(&tmp, text.as_bytes())
                    .map_err(|e| format!("writing {}: {e}", tmp.display()))?;
                std::fs::rename(&tmp, &sig_path)
                    .map_err(|e| format!("renaming {}: {e}", tmp.display()))?;
            }
            None => {
                let _ = std::fs::remove_file(&sig_path);
            }
        }
        // Keep only the offered pack: the state PVC is small and an older
        // candidate is never run once a newer check has replaced it.
        if let Ok(entries) = std::fs::read_dir(&self.cache_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                let is_pack = name.starts_with("pack-")
                    && (name.ends_with(".tar.gz") || name.ends_with(".tar.gz.sig"));
                if is_pack && entry.path() != path && entry.path() != sig_path {
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

fn pack_assets(
    release: &ReleaseEntry,
    version: &Version,
) -> Option<(String, String, Option<String>)> {
    let pack = format!("naslos-install-pack-{version}.tar.gz");
    let mut pack_url = None;
    let mut sha_url = None;
    let mut sig_url = None;
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
        } else if name == format!("{pack}.sig") {
            sig_url = Some(url.to_string());
        }
    }
    Some((pack_url?, sha_url?, sig_url))
}

/// The hosts a pack download may start from (R-063): the configured update
/// source or a GitHub release/CDN host. The check is https-only; GitHub's
/// redirect to the CDN is authenticated by TLS and the sha256 is re-checked
/// inside the Job.
pub fn pack_url_allowed(url: &str, source: &str) -> Result<(), String> {
    let uri: reqwest::Url = url
        .parse()
        .map_err(|e| format!("pack URL {url:?} is not a URL: {e}"))?;
    if uri.scheme() != "https" {
        return Err(format!("pack URL {url:?} must use https"));
    }
    let host = uri
        .host_str()
        .ok_or_else(|| format!("pack URL {url:?} has no host"))?;
    let source_host = reqwest::Url::parse(source)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string));
    const GITHUB_HOSTS: &[&str] = &[
        "github.com",
        "objects.githubusercontent.com",
        // GitHub redirects release assets here; still a GitHub-controlled
        // https host and the bytes are re-checked against the approved sha256.
        "release-assets.githubusercontent.com",
        "codeload.github.com",
        "api.github.com",
    ];
    if GITHUB_HOSTS
        .iter()
        .any(|allowed| host.eq_ignore_ascii_case(allowed))
        || source_host
            .as_deref()
            .is_some_and(|source| source.eq_ignore_ascii_case(host))
    {
        Ok(())
    } else {
        Err(format!(
            "pack URL host {host:?} is not the update source or a GitHub asset host"
        ))
    }
}

/// Redirect policy for release and pack downloads (I-7). The API used to follow
/// up to ten redirects and validate only the URL the release listed, so a
/// misconfigured or hostile mirror could bounce the download anywhere. Each hop
/// is now re-checked against `pack_url_allowed`; a hop that fails is a hard
/// error, not a follow.
pub fn pack_redirect_policy(source: &str) -> reqwest::redirect::Policy {
    let source = source.to_string();
    reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= 5 {
            return attempt.error("too many redirects while fetching the pack");
        }
        match pack_url_allowed(attempt.url().as_str(), &source) {
            Ok(()) => attempt.follow(),
            Err(e) => attempt.error(e),
        }
    })
}

/// Build the HTTP client used by the update poller and the upgrader Job with
/// the hop-validating redirect policy.
pub fn pack_client(timeout: Duration, source: &str) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(timeout)
        .redirect(pack_redirect_policy(source))
        .build()
        .map_err(|e| format!("building the download client: {e}"))
}

/// Download with a hard byte cap and a Content-Length pre-check, so a hostile
/// or broken asset cannot exhaust memory (I-7; the updater Job uses the same
/// helper as the API). `what` names the asset in errors.
pub async fn download_capped(
    client: &reqwest::Client,
    url: &str,
    what: &str,
    cap: usize,
    user_agent: &str,
) -> Result<Vec<u8>, String> {
    let resp = client
        .get(url)
        .header("User-Agent", user_agent)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("GET {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("GET {url}: HTTP {}", resp.status()));
    }
    if let Some(length) = resp.content_length() {
        if length > cap as u64 {
            return Err(format!(
                "GET {url}: {what} is {length} bytes, larger than the {cap} byte cap"
            ));
        }
    }
    let mut out = Vec::new();
    let mut resp = resp;
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| format!("reading the {what}: {e}"))?
    {
        if out.len().saturating_add(chunk.len()) > cap {
            return Err(format!("GET {url}: {what} exceeds the {cap} byte cap"));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
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
    pub pack_sig_url: &'a str,
    pub pack_pubkey: &'a str,
    pub release_name: &'a str,
    pub service_account: &'a str,
    pub talos_secret: &'a str,
    /// Comma-separated node addresses (`talosctl --nodes`).
    pub talos_nodes: &'a str,
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
        mounts.push(serde_json::json!({"name": "state", "mountPath": STATE_MOUNT}));
    } else {
        volumes.push(serde_json::json!({"name": "state", "emptyDir": {}}));
        mounts.push(serde_json::json!({"name": "state", "mountPath": STATE_MOUNT}));
    }
    // C-1c: the API downloads, verifies and caches the pack on the shared state
    // volume during the update check, and this Job mounts that claim at
    // `STATE_MOUNT`. Point the operation at the cached file (and its detached
    // signature) so the upgrade itself needs no egress under the namespace
    // default-deny; `--pack-url`/`--pack-sig-url` stay as the fallback path,
    // which the `naslos-system-op-egress` policy covers on 443.
    let pack_file = if o.state_pvc.is_empty() {
        String::new()
    } else {
        format!("{STATE_MOUNT}/updates/pack-{}.tar.gz", o.version)
    };
    let pack_sig_file = if pack_file.is_empty() {
        String::new()
    } else {
        format!("{pack_file}.sig")
    };
    let mut args = vec![
        serde_json::json!("--op"),
        serde_json::json!(o.op),
        serde_json::json!("--operation-id"),
        serde_json::json!(o.operation_id),
        serde_json::json!("--from"),
        serde_json::json!(o.from),
        serde_json::json!("--version"),
        serde_json::json!(o.version),
        serde_json::json!("--pack-url"),
        serde_json::json!(o.pack_url),
        serde_json::json!("--sha256"),
        serde_json::json!(o.pack_sha256),
        serde_json::json!("--pack-sig-url"),
        serde_json::json!(o.pack_sig_url),
        serde_json::json!("--pack-pubkey"),
        serde_json::json!(o.pack_pubkey),
        serde_json::json!("--namespace"),
        serde_json::json!(o.namespace),
        serde_json::json!("--release"),
        serde_json::json!(o.release_name),
    ];
    if !pack_file.is_empty() {
        args.push(serde_json::json!("--pack-file"));
        args.push(serde_json::json!(pack_file));
        args.push(serde_json::json!("--pack-sig-file"));
        args.push(serde_json::json!(pack_sig_file));
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
                        "args": args,
                        "env": [
                            {"name": "NASLOS_NAMESPACE", "value": o.namespace},
                            {"name": "NASLOS_RELEASE", "value": o.release_name},
                            {"name": "NASLOS_WORKLOAD_NAMESPACE", "value": o.workload_namespace},
                            {"name": "NASLOS_STATE_DIR", "value": STATE_MOUNT},
                            {"name": "TALOSCONFIG", "value": "/var/run/secrets/talos.dev/config"},
                            {"name": "TALOS_NODES", "value": o.talos_nodes},
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
    let _ = getrandom::fill(&mut random);
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
    fn installed_release_is_offered_while_system_pins_lag() {
        let f = facts();
        assert!(system_steps_pending(&f, "v1.14.0", "v1.37.0"));
        assert!(!system_steps_pending(&f, "v1.14.1", "v1.37.0"));
        assert!(!system_steps_pending(&f, "", ""));
    }

    #[test]
    fn installed_release_candidate_allows_pending_talos_step() {
        let f = facts();
        let installed = Candidate {
            version: f.naslos_version.clone(),
            talos_version: f.talos_version.clone(),
            kubernetes_version: f.kubernetes_version.clone(),
            ..Default::default()
        };
        let decision = candidate_stepping(&f, &installed, "v1.14.0", "v1.37.0");
        assert!(decision.allowed, "{}", decision.reason);
    }

    #[test]
    fn installed_release_candidate_refuses_a_pin_downgrade() {
        let f = facts();
        let installed = Candidate {
            version: f.naslos_version.clone(),
            talos_version: "v1.13.0".into(),
            kubernetes_version: f.kubernetes_version.clone(),
            ..Default::default()
        };
        let decision = candidate_stepping(&f, &installed, "v1.14.1", "v1.37.0");
        assert!(!decision.allowed);
        assert!(decision.reason.contains("downgrade"));
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
            pack_sig_url: "",
            pack_pubkey: "",
            release_name: "naslos",
            service_account: "naslos-upgrader",
            talos_secret: "naslos-talosconfig",
            talos_nodes: "192.168.1.117",
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
        let args = pod["containers"][0]["args"]
            .as_array()
            .expect("container args");
        let args: Vec<&str> = args.iter().filter_map(|a| a.as_str()).collect();
        for (flag, value) in [
            ("--pack-file", "/var/lib/naslos/updates/pack-0.3.1.tar.gz"),
            (
                "--pack-sig-file",
                "/var/lib/naslos/updates/pack-0.3.1.tar.gz.sig",
            ),
        ] {
            let index = args
                .iter()
                .position(|arg| *arg == flag)
                .unwrap_or_else(|| panic!("job spec must emit {flag}"));
            assert_eq!(args[index + 1], value);
        }
        let env = pod["containers"][0]["env"]
            .as_array()
            .expect("container env");
        let talos_nodes = env
            .iter()
            .find(|entry| entry["name"] == "TALOS_NODES")
            .expect("TALOS_NODES env");
        assert_eq!(talos_nodes["value"], "192.168.1.117");
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
        let (pack, sha, sig) = pack_assets(release, version).expect("pack assets");
        assert_eq!(pack, "https://example/pack");
        assert_eq!(sha, "https://example/sha");
        assert_eq!(sig, None);
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

    fn chart_policy_source() -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../charts/naslos/templates/system-upgrade.yaml");
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
    }

    /// The VAP carries its arg allowlist as a CEL `a.matches('<regex>')` literal.
    fn policy_arg_regex(source: &str) -> &str {
        let marker = "a.matches('";
        let start = source
            .find(marker)
            .unwrap_or_else(|| panic!("the upgrader policy has no arg allowlist"))
            + marker.len();
        let rest = &source[start..];
        let end = rest
            .find('\'')
            .unwrap_or_else(|| panic!("the upgrader arg allowlist is not a quoted literal"));
        &rest[..end]
    }

    /// Evaluate the policy's `^--(a|b|…)(=|$)` allowlist for one argument, the
    /// way CEL `matches` does for a flag (a non-flag value never reaches it).
    fn policy_allows(regex: &str, arg: &str) -> bool {
        let body = regex
            .strip_prefix("^--(")
            .unwrap_or_else(|| panic!("unexpected upgrader arg allowlist shape: {regex:?}"));
        let body = body
            .strip_suffix(")(=|$)")
            .unwrap_or_else(|| panic!("unexpected upgrader arg allowlist shape: {regex:?}"));
        match arg.strip_prefix("--") {
            Some(flag) => body
                .split('|')
                .any(|allowed| flag == allowed || flag.starts_with(&format!("{allowed}="))),
            None => false,
        }
    }

    /// C-1a: the chart's admission policy must admit every flag `job_spec`
    /// emits. The two drifted once (`--pack-sig-url`/`--pack-pubkey` were
    /// emitted but not allowed), which denied every web upgrade at admission.
    #[test]
    fn every_job_argument_is_admitted_by_the_chart_policy() {
        let source = chart_policy_source();
        let regex = policy_arg_regex(&source).to_string();
        let mut emitted: Vec<String> = Vec::new();
        for (state_pvc, pack_sig_url, pack_pubkey, talos_secret) in [
            ("naslos-shares-config", "", "", "naslos-talosconfig"),
            (
                "naslos-shares-config",
                "https://github.com/o/r/pack.sig",
                "abc",
                "",
            ),
            ("", "", "", "naslos-talosconfig"),
            ("", "https://github.com/o/r/pack.sig", "abc", ""),
        ] {
            let spec = job_spec(&JobOptions {
                name: "naslos-system-op-1-ab",
                namespace: "naslos",
                image: "ghcr.io/aessemops/naslos-api@sha256:dead",
                op: "naslos",
                operation_id: "1-ab",
                from: "0.3.0",
                version: "0.3.1",
                pack_url: "https://github.com/o/r/pack.tar.gz",
                pack_sha256: "abc",
                pack_sig_url,
                pack_pubkey,
                release_name: "naslos",
                service_account: "naslos-upgrader",
                talos_secret,
                talos_nodes: "10.0.0.1",
                state_pvc,
                workload_namespace: "naslos-privileged",
                backoff_limit: 3,
                ttl_seconds: 3600,
            });
            for arg in spec["spec"]["template"]["spec"]["containers"][0]["args"]
                .as_array()
                .expect("container args")
            {
                let arg = arg.as_str().expect("string arg");
                if arg.starts_with("--") {
                    emitted.push(arg.to_string());
                }
            }
        }
        for flag in [
            "--pack-url",
            "--pack-sig-url",
            "--pack-pubkey",
            "--pack-file",
            "--pack-sig-file",
        ] {
            assert!(
                emitted.iter().any(|arg| arg == flag),
                "job_spec no longer emits {flag}; update this test"
            );
        }
        for arg in &emitted {
            assert!(
                policy_allows(&regex, arg),
                "the chart admission policy denies job_spec argument {arg:?}"
            );
        }
        // Negative control: the allowlist is a flag set, not a prefix match.
        for denied in ["--privileged", "--pack-url-evil", "--command"] {
            assert!(!policy_allows(&regex, denied), "{denied} must stay denied");
        }
    }
}
