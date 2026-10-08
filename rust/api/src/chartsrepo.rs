//! Git-based Helm chart repositories (port of `api/internal/chartsrepo`).
//!
//! A source is a git repository laid out as `<root>/naslos-repo.yaml` (optional
//! channel mapping) and `<root>/apps/<name>/{Chart.yaml,naslos-app.yaml}`. The
//! cache holds one working tree per (source, channel).
//!
//! The Go code used go-git; this port drives the `git` CLI through a
//! `GitBackend` trait (the plan's bundled-CLI approach, consistent with
//! helm/talosctl), so the repository logic is testable without a real remote.

use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The channel offered when a source declares no mapping.
pub const DEFAULT_CHANNEL: &str = "Prod";
/// Directory holding one chart per app.
const APPS_DIR: &str = "apps";
/// How long a cached clone is considered fresh.
pub const DEFAULT_TTL: Duration = Duration::from_secs(15 * 60);
/// Guards against a pathological repository.
const MAX_REPO_BYTES: u64 = 256 << 20;
const MAX_REPO_FILES: usize = 20000;

/// How a source authenticates to its git remote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthType {
    #[default]
    Public,
    Token,
    Ssh,
}

/// The standard channel -> branch mapping.
pub fn default_channels() -> HashMap<String, String> {
    [
        ("Prod", "Prod"),
        ("Develop", "Develop"),
        ("Experimental", "Experimental"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

/// A configured chart repository.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Source {
    pub name: String,
    #[serde(
        default,
        rename = "displayName",
        skip_serializing_if = "String::is_empty"
    )]
    pub display_name: String,
    pub url: String,
    #[serde(default)]
    pub auth: AuthType,
    #[serde(
        default,
        rename = "credentialsSecret",
        skip_serializing_if = "String::is_empty"
    )]
    pub credentials_secret: String,
    #[serde(
        default,
        rename = "credentialsNamespace",
        skip_serializing_if = "String::is_empty"
    )]
    pub credentials_namespace: String,
    #[serde(default, rename = "tokenKey", skip_serializing_if = "String::is_empty")]
    pub token_key: String,
    #[serde(
        default,
        rename = "sshKeyKey",
        skip_serializing_if = "String::is_empty"
    )]
    pub ssh_key_key: String,
    #[serde(default)]
    pub channels: HashMap<String, String>,
    #[serde(default)]
    pub official: bool,
    #[serde(default, rename = "addedAt", skip_serializing_if = "Option::is_none")]
    pub added_at: Option<DateTime<Utc>>,
    #[serde(default, rename = "lastSync", skip_serializing_if = "Option::is_none")]
    pub last_sync: Option<DateTime<Utc>>,
    #[serde(
        default,
        rename = "lastError",
        skip_serializing_if = "String::is_empty"
    )]
    pub last_error: String,
}

impl Source {
    pub fn channels_for(&self) -> HashMap<String, String> {
        if !self.channels.is_empty() {
            let filtered: HashMap<String, String> = self
                .channels
                .iter()
                .filter(|(k, v)| !k.trim().is_empty() && !v.trim().is_empty())
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            if !filtered.is_empty() {
                return filtered;
            }
        }
        default_channels()
    }

    pub fn channel_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.channels_for().into_keys().collect();
        names.sort();
        names
    }

    pub fn token_key_or(&self) -> String {
        if self.token_key.is_empty() {
            "token".to_string()
        } else {
            self.token_key.clone()
        }
    }

    pub fn ssh_key_key_or(&self) -> String {
        if self.ssh_key_key.is_empty() {
            "ssh-private-key".to_string()
        } else {
            self.ssh_key_key.clone()
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_name(&self.name)?;
        if self.url.trim().is_empty() {
            return Err(format!("source {:?}: url is required", self.name));
        }
        match self.auth {
            AuthType::Public => {}
            AuthType::Token | AuthType::Ssh => {
                if self.credentials_secret.is_empty() {
                    return Err(format!(
                        "source {:?}: auth {:?} requires credentialsSecret",
                        self.name, self.auth
                    ));
                }
            }
        }
        Ok(())
    }

    fn normalize(&mut self) -> Result<(), String> {
        if self.channels.is_empty() {
            self.channels = default_channels();
        }
        if self.added_at.is_none() {
            self.added_at = Some(Utc::now());
        }
        self.validate()
    }
}

/// A lowercase RFC 1123 label (DNS-1123).
pub fn is_dns1123_label(name: &str) -> bool {
    if name.is_empty() || name.len() > 63 {
        return false;
    }
    let bytes = name.as_bytes();
    if !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit() {
        return false;
    }
    if !bytes[bytes.len() - 1].is_ascii_lowercase() && !bytes[bytes.len() - 1].is_ascii_digit() {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("source name is required".to_string());
    }
    if name.len() > 63 {
        return Err(format!(
            "source name {name:?} must be 63 characters or fewer"
        ));
    }
    if !is_dns1123_label(name) {
        return Err(format!(
            "source name {name:?} must be a lowercase DNS-1123 label"
        ));
    }
    Ok(())
}

/// Persists the configured sources as JSON.
pub struct Store {
    path: String,
    sources: Mutex<HashMap<String, Source>>,
}

impl Store {
    pub fn new(path: &str) -> Self {
        Self {
            path: path.to_string(),
            sources: Mutex::new(HashMap::new()),
        }
    }

    pub fn from_env() -> Self {
        let path = std::env::var("SOURCES_CONFIG")
            .unwrap_or_else(|_| "/var/lib/naslos/sources.json".to_string());
        Self::new(&path)
    }

    pub fn load(&self) -> Result<(), String> {
        if self.path.is_empty() {
            return Ok(());
        }
        let data = match std::fs::read(&self.path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(format!("reading sources file: {e}")),
        };
        let list: Vec<Source> =
            serde_json::from_slice(&data).map_err(|e| format!("parsing sources file: {e}"))?;
        let mut guard = self.sources.lock().unwrap();
        for mut src in list {
            if src.name.is_empty() {
                continue;
            }
            if src.channels.is_empty() {
                src.channels = default_channels();
            }
            guard.insert(src.name.clone(), src);
        }
        Ok(())
    }

    fn save_locked(&self, guard: &HashMap<String, Source>) -> Result<(), String> {
        if self.path.is_empty() {
            return Ok(());
        }
        let mut list: Vec<&Source> = guard.values().collect();
        list.sort_by(|a, b| a.name.cmp(&b.name));
        let mut data =
            serde_json::to_vec_pretty(&list).map_err(|e| format!("encoding sources: {e}"))?;
        data.push(b'\n');
        write_atomic(&self.path, &data, 0o600, 0o750)
    }

    pub fn list(&self) -> Vec<Source> {
        let guard = self.sources.lock().unwrap();
        let mut list: Vec<Source> = guard.values().cloned().collect();
        list.sort_by(|a, b| a.name.cmp(&b.name));
        list
    }

    pub fn get(&self, name: &str) -> Result<Source, String> {
        self.sources
            .lock()
            .unwrap()
            .get(name)
            .cloned()
            .ok_or_else(|| format!("source {name:?} not found"))
    }

    pub fn upsert(&self, mut src: Source) -> Result<(), String> {
        src.normalize()?;
        let mut guard = self.sources.lock().unwrap();
        guard.insert(src.name.clone(), src);
        self.save_locked(&guard)
    }

    pub fn update(&self, name: &str, mutate: impl FnOnce(&mut Source)) -> Result<Source, String> {
        let mut guard = self.sources.lock().unwrap();
        let src = guard
            .get_mut(name)
            .ok_or_else(|| format!("source {name:?} not found"))?;
        mutate(src);
        let out = src.clone();
        self.save_locked(&guard)?;
        Ok(out)
    }

    pub fn delete(&self, name: &str) -> Result<(), String> {
        let mut guard = self.sources.lock().unwrap();
        if guard.remove(name).is_none() {
            return Err(format!("source {name:?} not found"));
        }
        self.save_locked(&guard)
    }
}

/// Resolved credentials for one source.
#[derive(Debug, Clone, Default)]
pub struct Credentials {
    pub token: String,
    pub ssh_key: Vec<u8>,
}

/// Resolves a source's credentials.
pub trait CredentialsProvider: Send + Sync {
    fn resolve(&self, src: &Source) -> Result<Credentials, String>;
}

/// A fixed-credentials provider (tests / single-source use).
pub struct StaticCredentialsProvider {
    pub creds: Credentials,
}

impl CredentialsProvider for StaticCredentialsProvider {
    fn resolve(&self, _src: &Source) -> Result<Credentials, String> {
        Ok(self.creds.clone())
    }
}

/// The auth a git operation needs.
#[derive(Debug, Clone, Default)]
pub struct GitAuth {
    pub token: String,
    pub ssh_key: Vec<u8>,
}

/// Clones or pulls a source/channel working tree. Abstracted so the repository
/// logic is testable without a real remote.
#[async_trait::async_trait]
pub trait GitBackend: Send + Sync {
    async fn sync(&self, url: &str, branch: &str, dir: &str, auth: &GitAuth) -> Result<(), String>;
}

/// Drives the bundled `git` CLI.
pub struct GitCliBackend {
    binary: String,
}

impl GitCliBackend {
    pub fn new() -> Self {
        Self {
            binary: std::env::var("GIT_BIN").unwrap_or_else(|_| "git".to_string()),
        }
    }

    async fn run(&self, args: &[String], env: &[(String, String)]) -> Result<(), String> {
        let mut cmd = tokio::process::Command::new(&self.binary);
        cmd.args(args);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let output = tokio::time::timeout(Duration::from_secs(300), cmd.output())
            .await
            .map_err(|_| {
                format!(
                    "git {}: timed out",
                    args.first().cloned().unwrap_or_default()
                )
            })?
            .map_err(|e| format!("running git: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "git {}: {}",
                args.first().cloned().unwrap_or_default(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(())
    }
}

impl Default for GitCliBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl GitBackend for GitCliBackend {
    async fn sync(&self, url: &str, branch: &str, dir: &str, auth: &GitAuth) -> Result<(), String> {
        // HTTPS token: embed as basic auth in the URL (go-git used
        // x-access-token). SSH: point GIT_SSH_COMMAND at a temp key.
        let (clone_url, env) = if !auth.token.is_empty() {
            (inject_token(url, &auth.token), Vec::new())
        } else if !auth.ssh_key.is_empty() {
            let key_path = write_ssh_key(&auth.ssh_key)?;
            (
                url.to_string(),
                vec![(
                    "GIT_SSH_COMMAND".to_string(),
                    format!(
                        "ssh -i {} -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null",
                        key_path.display()
                    ),
                )],
            )
        } else {
            (url.to_string(), Vec::new())
        };

        if !Path::new(dir).join(".git").exists() {
            if let Some(parent) = Path::new(dir).parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("creating cache directory: {e}"))?;
            }
            self.run(
                &[
                    "clone".into(),
                    "--branch".into(),
                    branch.into(),
                    "--single-branch".into(),
                    "--depth".into(),
                    "1".into(),
                    "--no-tags".into(),
                    clone_url,
                    dir.into(),
                ],
                &env,
            )
            .await
        } else {
            self.run(
                &[
                    "-C".into(),
                    dir.into(),
                    "fetch".into(),
                    "--depth".into(),
                    "1".into(),
                    "origin".into(),
                    branch.into(),
                ],
                &env,
            )
            .await?;
            self.run(
                &[
                    "-C".into(),
                    dir.into(),
                    "reset".into(),
                    "--hard".into(),
                    "FETCH_HEAD".into(),
                ],
                &env,
            )
            .await
        }
    }
}

/// Embed an HTTPS token as basic auth (x-access-token).
fn inject_token(url: &str, token: &str) -> String {
    if let Some(rest) = url.strip_prefix("https://") {
        format!("https://x-access-token:{token}@{rest}")
    } else {
        url.to_string()
    }
}

fn write_ssh_key(key: &[u8]) -> Result<PathBuf, String> {
    let path = std::env::temp_dir().join("naslos-charts-deploy-key");
    std::fs::write(&path, key).map_err(|e| format!("writing deploy key: {e}"))?;
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))
        .map_err(|e| format!("setting deploy key mode: {e}"))?;
    Ok(path)
}

/// Owns the source store and the local clone cache.
pub struct Manager {
    store: Arc<Store>,
    cache_dir: String,
    ttl: Duration,
    creds: Option<Arc<dyn CredentialsProvider>>,
    git: Arc<dyn GitBackend>,
    refresh_mu: tokio::sync::Mutex<()>,
}

impl Manager {
    pub fn new(
        store: Arc<Store>,
        cache_dir: &str,
        ttl: Duration,
        creds: Option<Arc<dyn CredentialsProvider>>,
        git: Arc<dyn GitBackend>,
    ) -> Self {
        Self {
            store,
            cache_dir: cache_dir.to_string(),
            ttl: if ttl.is_zero() { DEFAULT_TTL } else { ttl },
            creds,
            git,
            refresh_mu: tokio::sync::Mutex::new(()),
        }
    }

    pub fn sources(&self) -> Vec<Source> {
        self.store.list()
    }

    pub fn get_source(&self, name: &str) -> Result<Source, String> {
        self.store.get(name)
    }

    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    pub fn add_source(&self, mut src: Source) -> Result<Source, String> {
        src.normalize()?;
        self.store.upsert(src.clone())?;
        self.store.get(&src.name)
    }

    pub fn delete_source(&self, name: &str) -> Result<(), String> {
        self.store.delete(name)?;
        let _ = std::fs::remove_dir_all(Path::new(&self.cache_dir).join(name));
        Ok(())
    }

    /// The working tree for a source/channel, validating both names.
    pub fn dir(&self, source_name: &str, channel: &str) -> Result<String, String> {
        validate_name(source_name)?;
        let channel = if channel.trim().is_empty() {
            DEFAULT_CHANNEL
        } else {
            channel.trim()
        };
        validate_name(&channel.to_lowercase())
            .map_err(|_| format!("invalid channel {channel:?}"))?;
        Ok(Path::new(&self.cache_dir)
            .join(source_name)
            .join(channel.to_lowercase())
            .display()
            .to_string())
    }

    /// Refresh one channel of a source.
    pub async fn refresh(&self, name: &str, channel: &str) -> Result<(), String> {
        let src = self.store.get(name)?;
        self.refresh_source(&src, channel).await
    }

    /// Refresh every source/channel.
    pub async fn refresh_all(&self) -> Result<(), String> {
        let mut errs = Vec::new();
        for src in self.store.list() {
            for channel in src.channel_names() {
                if let Err(e) = self.refresh_source(&src, &channel).await {
                    errs.push(e);
                }
            }
        }
        if errs.is_empty() {
            Ok(())
        } else {
            Err(errs.join("; "))
        }
    }

    async fn refresh_source(&self, src: &Source, channel: &str) -> Result<(), String> {
        let branch = branch_for(src, channel)?;
        let dir = self.dir(&src.name, channel)?;

        let _guard = self.refresh_mu.lock().await;

        let auth = self.auth_for(src)?;
        if let Err(e) = self.git.sync(&src.url, &branch, &dir, &auth).await {
            self.record_sync(&src.name, Some(&e));
            return Err(format!("refreshing {}/{channel}: {e}", src.name));
        }
        if let Err(e) = stat_repo(&dir) {
            let _ = std::fs::remove_dir_all(&dir);
            self.record_sync(&src.name, Some(&e));
            return Err(format!("refreshing {}/{channel}: {e}", src.name));
        }
        self.record_sync(&src.name, None);
        Ok(())
    }

    fn auth_for(&self, src: &Source) -> Result<GitAuth, String> {
        if src.auth == AuthType::Public {
            return Ok(GitAuth::default());
        }
        let Some(creds) = &self.creds else {
            return Err(format!(
                "source {:?} uses {:?} auth but no credential provider is configured",
                src.name, src.auth
            ));
        };
        let resolved = creds.resolve(src)?;
        match src.auth {
            AuthType::Token => {
                if resolved.token.trim().is_empty() {
                    return Err(format!("source {:?}: empty token", src.name));
                }
                Ok(GitAuth {
                    token: resolved.token,
                    ssh_key: Vec::new(),
                })
            }
            AuthType::Ssh => {
                if resolved.ssh_key.is_empty() {
                    return Err(format!("source {:?}: empty SSH key", src.name));
                }
                Ok(GitAuth {
                    token: String::new(),
                    ssh_key: resolved.ssh_key,
                })
            }
            AuthType::Public => Ok(GitAuth::default()),
        }
    }

    fn record_sync(&self, name: &str, sync_err: Option<&String>) {
        let _ = self.store.update(name, |src| {
            src.last_error = sync_err.cloned().unwrap_or_default();
            if sync_err.is_none() {
                src.last_sync = Some(Utc::now());
            }
        });
    }

    /// Return a source whose cache is fresh, refreshing when the TTL expired;
    /// fall back to the stale clone when the remote is unreachable.
    pub async fn ensure_fresh(&self, name: &str, channel: &str) -> Result<Source, String> {
        let src = self.store.get(name)?;
        self.dir(name, channel)?;
        branch_for(&src, channel)?;

        let cached = self.has_clone(name, channel);
        let fresh = cached
            && src
                .last_sync
                .map(|t| (Utc::now() - t).num_seconds() < self.ttl.as_secs() as i64)
                .unwrap_or(false);
        if fresh {
            return Ok(src);
        }
        if let Err(e) = self.refresh_source(&src, channel).await {
            if cached {
                return self.store.get(name);
            }
            return Err(e);
        }
        self.store.get(name)
    }

    fn has_clone(&self, name: &str, channel: &str) -> bool {
        let Ok(dir) = self.dir(name, channel) else {
            return false;
        };
        Path::new(&dir).join(".git").is_dir()
    }

    /// The chart directories under `apps/` for a cached source/channel.
    pub fn app_names(&self, name: &str, channel: &str) -> Result<Vec<String>, String> {
        let dir = self.dir(name, channel)?;
        let apps_root = Path::new(&dir).join(APPS_DIR);
        let entries = match std::fs::read_dir(&apps_root) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(format!("listing apps: {e}")),
        };
        let mut names = Vec::new();
        for entry in entries.flatten() {
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let n = entry.file_name().to_string_lossy().into_owned();
            if validate_name(&n).is_ok() {
                names.push(n);
            }
        }
        Ok(names)
    }

    /// Resolve one app's directory, refusing any path that escapes the clone.
    pub fn app_dir(&self, name: &str, channel: &str, app: &str) -> Result<String, String> {
        validate_name(app).map_err(|_| format!("invalid app name {app:?}"))?;
        let dir = self.dir(name, channel)?;
        safe_join(&dir, &format!("{APPS_DIR}/{app}"))
    }
}

fn branch_for(src: &Source, channel: &str) -> Result<String, String> {
    src.channels_for()
        .get(channel)
        .cloned()
        .ok_or_else(|| format!("source {:?} does not offer channel {channel:?}", src.name))
}

/// Join `rel` onto `root` and verify the resolved path stays inside `root`.
pub fn safe_join(root: &str, rel: &str) -> Result<String, String> {
    if rel.is_empty() || Path::new(rel).is_absolute() {
        return Err(format!("invalid relative path {rel:?}"));
    }
    let clean = clean_rel(rel);
    if clean == ".." || clean.starts_with("../") {
        return Err(format!("path {rel:?} escapes the repository"));
    }
    let full = Path::new(root).join(&clean);

    let root_resolved =
        std::fs::canonicalize(root).map_err(|e| format!("resolving repository root: {e}"))?;
    let resolved = std::fs::canonicalize(&full).map_err(|e| format!("resolving {rel:?}: {e}"))?;
    if resolved != root_resolved && !resolved.starts_with(&root_resolved) {
        return Err(format!("path {rel:?} resolves outside the repository"));
    }
    Ok(resolved.display().to_string())
}

/// `filepath.Clean` for a relative path.
fn clean_rel(s: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for part in s.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            p => out.push(p),
        }
    }
    out.join("/")
}

/// Enforce the repository size and file-count guard, skipping `.git`.
pub fn stat_repo(dir: &str) -> Result<(), String> {
    let mut files = 0usize;
    let mut bytes = 0u64;
    let mut stack = vec![PathBuf::from(dir)];
    while let Some(current) = stack.pop() {
        let entries = std::fs::read_dir(&current).map_err(|e| e.to_string())?;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let file_type = entry.file_type().map_err(|e| e.to_string())?;
            if file_type.is_dir() {
                if name == ".git" {
                    continue;
                }
                stack.push(entry.path());
                continue;
            }
            files += 1;
            bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
            if files > MAX_REPO_FILES {
                return Err(format!("repository has more than {MAX_REPO_FILES} files"));
            }
            if bytes > MAX_REPO_BYTES {
                return Err(format!(
                    "repository is larger than {} MiB",
                    MAX_REPO_BYTES >> 20
                ));
            }
        }
    }
    Ok(())
}

/// Atomic write with dir + file modes.
pub(crate) fn write_atomic(
    path: &str,
    data: &[u8],
    file_mode: u32,
    dir_mode: u32,
) -> Result<(), String> {
    let target = Path::new(path);
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    if !dir.as_os_str().is_empty() && dir != Path::new(".") {
        std::fs::create_dir_all(dir).map_err(|e| format!("creating directory: {e}"))?;
        let _ =
            std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(dir_mode));
    }
    let tmp = tempfile::Builder::new()
        .prefix(".tmp-")
        .tempfile_in(dir)
        .map_err(|e| format!("creating temp file: {e}"))?;
    {
        use std::io::Write;
        let mut f = tmp.as_file().try_clone().map_err(|e| e.to_string())?;
        f.write_all(data).map_err(|e| format!("writing: {e}"))?;
        f.sync_all().map_err(|e| format!("syncing: {e}"))?;
    }
    let tmp_path = tmp.into_temp_path();
    std::fs::set_permissions(
        &tmp_path,
        std::os::unix::fs::PermissionsExt::from_mode(file_mode),
    )
    .map_err(|e| format!("setting mode: {e}"))?;
    std::fs::rename(&tmp_path, target).map_err(|e| format!("replacing: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(name: &str) -> Source {
        Source {
            name: name.into(),
            url: "https://example.com/repo.git".into(),
            auth: AuthType::Public,
            channels: HashMap::new(),
            ..Default::default()
        }
    }

    #[test]
    fn dns1123_validation() {
        assert!(validate_name("naslos-official").is_ok());
        assert!(validate_name("Bad_Name").is_err());
        assert!(validate_name("").is_err());
        assert!(validate_name("-lead").is_err());
    }

    #[test]
    fn channels_default_and_custom() {
        let s = source("official");
        assert_eq!(s.channel_names(), vec!["Develop", "Experimental", "Prod"]);
        let mut custom = source("only-main");
        custom.channels = HashMap::from([("Prod".to_string(), "main".to_string())]);
        assert_eq!(custom.channel_names(), vec!["Prod"]);
        assert_eq!(branch_for(&custom, "Prod").unwrap(), "main");
        assert!(branch_for(&custom, "Develop").is_err());
    }

    #[test]
    fn source_validate_requires_creds_for_token() {
        let mut s = source("priv");
        s.auth = AuthType::Token;
        assert!(s.validate().is_err());
        s.credentials_secret = "gh-pat".into();
        assert!(s.validate().is_ok());
    }

    #[test]
    fn safe_join_rejects_traversal() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("apps/ok")).unwrap();
        assert!(safe_join(&dir.path().display().to_string(), "apps/ok").is_ok());
        assert!(safe_join(&dir.path().display().to_string(), "../etc").is_err());
        assert!(safe_join(&dir.path().display().to_string(), "/etc").is_err());
    }

    #[test]
    fn dir_uses_lowercased_channel() {
        let store = Arc::new(Store::new(""));
        let m = Manager::new(
            store,
            "/cache",
            DEFAULT_TTL,
            None,
            Arc::new(GitCliBackend::new()),
        );
        assert_eq!(m.dir("official", "Prod").unwrap(), "/cache/official/prod");
        assert!(m.dir("Bad Name", "Prod").is_err());
    }

    #[test]
    fn app_names_skips_non_dirs_and_bad_names() {
        let dir = tempfile::tempdir().unwrap();
        let apps = dir.path().join("official/prod/apps");
        std::fs::create_dir_all(apps.join("nginx")).unwrap();
        std::fs::create_dir_all(apps.join("Bad_Name")).unwrap();
        std::fs::write(apps.join("file.txt"), b"x").unwrap();
        let store = Arc::new(Store::new(""));
        let m = Manager::new(
            store,
            &dir.path().display().to_string(),
            DEFAULT_TTL,
            None,
            Arc::new(GitCliBackend::new()),
        );
        assert_eq!(m.app_names("official", "Prod").unwrap(), vec!["nginx"]);
    }
}
