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
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub commit: String,
    #[serde(default)]
    pub official: bool,
    #[serde(default, rename = "addedAt", skip_serializing_if = "Option::is_none")]
    pub added_at: Option<DateTime<Utc>>,
    #[serde(default, rename = "lastSync", skip_serializing_if = "Option::is_none")]
    pub last_sync: Option<DateTime<Utc>>,
    /// Exact-channel freshness. A source-level `lastSync` made every channel
    /// look fresh after one channel refreshed, so a new branch could serve a
    /// stale (or missing) clone until its own TTL expired.
    #[serde(
        default,
        rename = "channelSync",
        skip_serializing_if = "HashMap::is_empty"
    )]
    pub channel_sync: HashMap<String, DateTime<Utc>>,
    #[serde(
        default,
        rename = "lastError",
        skip_serializing_if = "String::is_empty"
    )]
    pub last_error: String,
}

impl Source {
    /// A copy safe for API responses: `lastError` may echo a git URL with the
    /// token embedded by the transport, so URL credentials are always masked.
    pub fn sanitized(&self) -> Source {
        let mut source = self.clone();
        source.last_error = redact_source_credentials(&source.last_error);
        source.url = redact_source_credentials(&source.url);
        source
    }

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
        validate_source_url(&self.url).map_err(|e| format!("source {:?}: {e}", self.name))?;
        if !self.commit.is_empty() && !is_full_commit(&self.commit) {
            return Err(format!(
                "source {:?}: commit must be a full 40-character hex SHA",
                self.name
            ));
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

/// Only well-formed HTTPS/SSH/git URLs may reach `git clone`.
///
/// A leading '-' (option injection), an `ext::`/`file://` transport (local
/// command execution / local file reads) or control characters are refused.
pub fn validate_source_url(url: &str) -> Result<(), String> {
    let url = url.trim();
    if url.starts_with('-') {
        return Err("url must not start with '-'".to_string());
    }
    if url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("url must not contain whitespace or control characters".to_string());
    }
    let lower = url.to_ascii_lowercase();
    // `git://` is unauthenticated and unencrypted: an on-path attacker could
    // serve an arbitrary chart to an install that runs with cluster identity.
    let allowed = lower.starts_with("https://")
        || lower.starts_with("ssh://")
        || (lower.starts_with("git@") && lower.contains(':'));
    if !allowed {
        return Err("url must use https://, ssh:// or git@host:path".to_string());
    }
    // Userinfo in an HTTPS URL is a plain-text credential: it is persisted in
    // sources.json and returned by GET /api/sources. Tokens belong in
    // `credentialsSecret`. SSH keeps the `git@` user (not a secret) but a
    // password before the host is refused.
    if let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    {
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
        if authority.contains('@') {
            return Err("url must not embed credentials; use credentialsSecret".to_string());
        }
    } else if let Some(rest) = url.strip_prefix("ssh://") {
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
        if let Some((userinfo, _host)) = authority.rsplit_once('@') {
            if userinfo.contains(':') {
                return Err("url must not embed a password".to_string());
            }
        }
    }
    Ok(())
}

/// Mask `scheme://user:pass@host` credentials that a git error may echo.
pub fn redact_source_credentials(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(scheme_end) = rest.find("://") {
        let (before, after_scheme) = rest.split_at(scheme_end + 3);
        out.push_str(before);
        let authority_end = after_scheme
            .find(['/', ' ', '\n', '\t', '\''])
            .unwrap_or(after_scheme.len());
        let authority = &after_scheme[..authority_end];
        let https = before.to_ascii_lowercase().ends_with("https://");
        if let Some(at) = authority.rfind('@') {
            let userinfo = &authority[..at];
            // `ssh://git@host` carries a non-secret user name; any password
            // there and every HTTPS userinfo are credentials.
            if https || userinfo.contains(':') {
                out.push_str("[redacted]@");
            } else {
                out.push_str(userinfo);
                out.push('@');
            }
            out.push_str(&authority[at + 1..]);
        } else {
            out.push_str(authority);
        }
        rest = &after_scheme[authority_end..];
    }
    out.push_str(rest);
    out
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
            // The name becomes a directory under the cache and is later passed
            // to remove_dir_all; never trust it from the on-disk file
            // (SEC-2026-B12).
            validate_name(&src.name)?;
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
    async fn sync(
        &self,
        url: &str,
        branch: &str,
        commit: &str,
        dir: &str,
        auth: &GitAuth,
    ) -> Result<(), String>;
}

fn is_full_commit(value: &str) -> bool {
    value.len() == 40 && value.chars().all(|c| c.is_ascii_hexdigit())
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
        self.run_stdout(args, env).await.map(|_| ())
    }

    /// Global git options that must precede every subcommand.
    ///
    /// A chart is installed from this tree by the Helm loader, which follows
    /// symlinks. Checking links out as plain files (containing the target path)
    /// removes the primitive that let a chart read files outside the clone;
    /// `assert_regular_tree` is the second layer.
    fn argv(args: &[String]) -> Vec<String> {
        let mut full = vec!["-c".to_string(), "core.symlinks=false".to_string()];
        full.extend(args.iter().cloned());
        full
    }

    async fn run_stdout(
        &self,
        args: &[String],
        env: &[(String, String)],
    ) -> Result<String, String> {
        let mut cmd = tokio::process::Command::new(&self.binary);
        crate::proc::scrub(&mut cmd);
        cmd.kill_on_drop(true);
        cmd.args(Self::argv(args));
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
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

impl Default for GitCliBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl GitBackend for GitCliBackend {
    async fn sync(
        &self,
        url: &str,
        branch: &str,
        commit: &str,
        dir: &str,
        auth: &GitAuth,
    ) -> Result<(), String> {
        // HTTPS token: embed as basic auth in the URL (go-git used
        // x-access-token). SSH: point GIT_SSH_COMMAND at a temp key.
        validate_source_url(url)?;
        let mut key_file = None;
        let (clone_url, env) = if !auth.token.is_empty() {
            (inject_token(url, &auth.token), Vec::new())
        } else if !auth.ssh_key.is_empty() {
            // Host-key verification is mandatory: without a pinned known_hosts
            // an on-path attacker could serve an arbitrary chart (and the API
            // installs charts with its own, Talos-bearing identity).
            let known_hosts = std::env::var("GIT_SSH_KNOWN_HOSTS")
                .unwrap_or_else(|_| "/var/lib/naslos/known_hosts".to_string());
            if !Path::new(&known_hosts).is_file() {
                return Err(format!(
                    "SSH chart sources require a pinned host key at {known_hosts} (set GIT_SSH_KNOWN_HOSTS); refusing to clone without host verification"
                ));
            }
            let file = write_ssh_key(&auth.ssh_key)?;
            let key_path = file.path().to_path_buf();
            key_file = Some(file);
            (
                url.to_string(),
                vec![(
                    "GIT_SSH_COMMAND".to_string(),
                    format!(
                        "ssh -i {} -o StrictHostKeyChecking=yes -o UserKnownHostsFile={}",
                        shell_quote(&key_path.display().to_string()),
                        shell_quote(&known_hosts)
                    ),
                )],
            )
        } else {
            (url.to_string(), Vec::new())
        };

        let result = async {
            let fresh = !Path::new(dir).join(".git").exists();
            if fresh {
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
                        "--".into(),
                        clone_url,
                        dir.into(),
                    ],
                    &env,
                )
                .await?;
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
            }

            if !commit.is_empty() {
                // Commit pinning (SEC-2026-D6): check out exactly the pinned
                // commit and fail closed if it is not what we ended up with.
                let present = self
                    .run(
                        &[
                            "-C".into(),
                            dir.into(),
                            "cat-file".into(),
                            "-e".into(),
                            format!("{commit}^{{commit}}"),
                        ],
                        &env,
                    )
                    .await
                    .is_ok();
                if !present {
                    self.run(
                        &[
                            "-C".into(),
                            dir.into(),
                            "fetch".into(),
                            "--depth".into(),
                            "1".into(),
                            "origin".into(),
                            commit.into(),
                        ],
                        &env,
                    )
                    .await?;
                }
                self.run(
                    &[
                        "-C".into(),
                        dir.into(),
                        "reset".into(),
                        "--hard".into(),
                        commit.into(),
                    ],
                    &env,
                )
                .await?;
                let head = self
                    .run_stdout(
                        &["-C".into(), dir.into(), "rev-parse".into(), "HEAD".into()],
                        &env,
                    )
                    .await?;
                if !head.trim().eq_ignore_ascii_case(commit) {
                    return Err(format!(
                        "the checked-out commit {} does not match the pinned {commit}",
                        head.trim()
                    ));
                }
            } else if !fresh {
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
                .await?;
            }
            Ok(())
        }
        .await;
        drop(key_file);
        result.map_err(|e| {
            if auth.token.is_empty() {
                e
            } else {
                e.replace(&auth.token, "***")
            }
        })
    }
}

/// Refuse a chart tree that contains anything Helm would follow outside the
/// clone: symlinks, hard links, devices, fifos, or an oversized/very large tree.
///
/// Confirmed against Helm v4.3.0: a symlinked chart file is read through, so
/// `{{ .Files.Get "leak.txt" }}` renders `/var/lib/naslos/buddy-identity.json`
/// into a manifest. Git already checks links out as plain files
/// (`core.symlinks=false`); this also covers trees produced any other way.
pub fn assert_regular_tree(root: &str) -> Result<(), String> {
    const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;
    const MAX_ENTRIES: usize = 5000;

    let root = std::fs::canonicalize(root)
        .map_err(|e| format!("resolving chart directory {root:?}: {e}"))?;
    if !root.is_dir() {
        return Err(format!("chart path {root:?} is not a directory"));
    }

    fn walk(dir: &std::path::Path, entries: &mut usize) -> Result<(), String> {
        let read = std::fs::read_dir(dir)
            .map_err(|e| format!("reading chart directory {}: {e}", dir.display()))?;
        for entry in read {
            let entry = entry.map_err(|e| format!("reading chart directory: {e}"))?;
            *entries += 1;
            if *entries > MAX_ENTRIES {
                return Err(format!(
                    "chart has more than {MAX_ENTRIES} entries; refusing to install it"
                ));
            }
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|e| format!("stat {}: {e}", path.display()))?;
            if file_type.is_symlink() {
                return Err(format!(
                    "chart contains a symlink ({}); charts may only contain regular files and directories",
                    path.display()
                ));
            }
            if file_type.is_dir() {
                walk(&path, entries)?;
            } else if file_type.is_file() {
                let meta = entry
                    .metadata()
                    .map_err(|e| format!("stat {}: {e}", path.display()))?;
                if meta.len() > MAX_FILE_BYTES {
                    return Err(format!(
                        "chart file {} is larger than {MAX_FILE_BYTES} bytes; refusing to install it",
                        path.display()
                    ));
                }
            } else {
                return Err(format!(
                    "chart contains a non-regular entry ({}); refusing to install it",
                    path.display()
                ));
            }
        }
        Ok(())
    }

    let mut entries = 0;
    walk(&root, &mut entries)
}

/// Quote a path for `GIT_SSH_COMMAND`, which git evaluates with a shell.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Embed an HTTPS token as basic auth (x-access-token).
fn inject_token(url: &str, token: &str) -> String {
    if let Some(rest) = url.strip_prefix("https://") {
        format!("https://x-access-token:{token}@{rest}")
    } else {
        url.to_string()
    }
}

fn write_ssh_key(key: &[u8]) -> Result<tempfile::NamedTempFile, String> {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let mut file =
        tempfile::NamedTempFile::new().map_err(|e| format!("creating the deploy-key file: {e}"))?;
    file.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("setting deploy key mode: {e}"))?;
    file.write_all(key)
        .map_err(|e| format!("writing deploy key: {e}"))?;
    Ok(file)
}

/// Owns the source store and the local clone cache.
pub struct Manager {
    store: Arc<Store>,
    cache_dir: String,
    ttl: Duration,
    creds: Option<Arc<dyn CredentialsProvider>>,
    git: Arc<dyn GitBackend>,
    /// One reader/writer lock per source. A refresh mutates the working tree;
    /// an install holds the read side while it copies the chart, so the copy
    /// can never observe a half-applied `git checkout` (R-051).
    source_locks: Mutex<HashMap<String, Arc<tokio::sync::RwLock<()>>>>,
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
            source_locks: Mutex::new(HashMap::new()),
        }
    }

    pub fn sources(&self) -> Vec<Source> {
        self.store.list().iter().map(Source::sanitized).collect()
    }

    /// The cache freshness window (the `CHARTS_TTL` value). The background
    /// catalog refresher sleeps for this long between full refreshes.
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    pub fn get_source(&self, name: &str) -> Result<Source, String> {
        self.store.get(name).map(|source| source.sanitized())
    }

    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    pub fn add_source(&self, mut src: Source) -> Result<Source, String> {
        src.normalize()?;
        if !src.official {
            if src.name == "official" {
                return Err(
                    "source name \"official\" is reserved for the seeded official source"
                        .to_string(),
                );
            }
            if let Ok(existing) = self.store.get(&src.name) {
                if existing.official {
                    return Err(format!(
                        "source name {:?} belongs to the official source; pick another name",
                        src.name
                    ));
                }
            }
        }
        self.store.upsert(src.clone())?;
        self.store.get(&src.name).map(|source| source.sanitized())
    }

    /// The read guard an install holds while it snapshots a chart.
    pub async fn source_read_lock(&self, name: &str) -> tokio::sync::OwnedRwLockReadGuard<()> {
        self.source_lock(name).read_owned().await
    }

    fn source_lock(&self, name: &str) -> Arc<tokio::sync::RwLock<()>> {
        let mut locks = self.source_locks.lock().unwrap();
        locks
            .entry(name.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::RwLock::new(())))
            .clone()
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

        let lock = self.source_lock(&src.name);
        let _guard = lock.write().await;

        let auth = self.auth_for(src)?;
        if let Err(e) = self
            .git
            .sync(&src.url, &branch, &src.commit, &dir, &auth)
            .await
        {
            self.record_sync(&src.name, channel, Some(&e));
            return Err(format!("refreshing {}/{channel}: {e}", src.name));
        }
        if let Err(e) = stat_repo(&dir) {
            let _ = std::fs::remove_dir_all(&dir);
            self.record_sync(&src.name, channel, Some(&e));
            return Err(format!("refreshing {}/{channel}: {e}", src.name));
        }
        self.record_sync(&src.name, channel, None);
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

    fn record_sync(&self, name: &str, channel: &str, sync_err: Option<&String>) {
        let now = Utc::now();
        let _ = self.store.update(name, |src| {
            src.last_error =
                redact_source_credentials(sync_err.map(String::as_str).unwrap_or_default());
            if sync_err.is_none() {
                src.last_sync = Some(now);
                src.channel_sync.insert(channel.to_string(), now);
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
                .channel_sync
                .get(channel)
                .map(|t| (Utc::now() - *t).num_seconds() < self.ttl.as_secs() as i64)
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

    /// Copy a resolved chart into a private temporary directory owned by one
    /// install/upgrade job. Helm reads the chart repeatedly; without the copy a
    /// concurrent refresh could rewrite it mid-read (R-051).
    pub fn snapshot_app(
        &self,
        name: &str,
        channel: &str,
        app: &str,
    ) -> Result<ChartSnapshot, String> {
        let source = self.app_dir(name, channel, app)?;
        let dir = tempfile::Builder::new()
            .prefix("naslos-chart-")
            .tempdir()
            .map_err(|e| format!("creating a private chart directory: {e}"))?;
        copy_tree(Path::new(&source), dir.path())?;
        let path = dir.path().display().to_string();
        Ok(ChartSnapshot { _dir: dir, path })
    }
}

/// A chart copy scoped to one job; dropping it removes the private directory.
pub struct ChartSnapshot {
    _dir: tempfile::TempDir,
    pub path: String,
}

/// Read the chart's `Chart.yaml` version (the version Helm will install).
pub fn chart_version(dir: &str) -> Option<String> {
    #[derive(Default, serde::Deserialize)]
    struct ChartYaml {
        #[serde(default)]
        version: String,
    }
    let data = std::fs::read(Path::new(dir).join("Chart.yaml")).ok()?;
    let meta: ChartYaml = serde_yaml::from_slice(&data).ok()?;
    Some(meta.version.trim().to_string()).filter(|version| !version.is_empty())
}

fn copy_tree(source: &Path, target: &Path) -> Result<(), String> {
    for entry in
        std::fs::read_dir(source).map_err(|e| format!("reading chart {}: {e}", source.display()))?
    {
        let entry = entry.map_err(|e| format!("reading chart {}: {e}", source.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| format!("stat {}: {e}", path.display()))?;
        if file_type.is_symlink() {
            return Err(format!(
                "chart contains a symlink ({}); refusing to copy it",
                path.display()
            ));
        }
        let destination = target.join(entry.file_name());
        if file_type.is_dir() {
            std::fs::create_dir_all(&destination)
                .map_err(|e| format!("creating {}: {e}", destination.display()))?;
            copy_tree(&path, &destination)?;
        } else if file_type.is_file() {
            std::fs::copy(&path, &destination)
                .map_err(|e| format!("copying {}: {e}", path.display()))?;
        } else {
            return Err(format!(
                "chart contains a non-regular file ({}); refusing to copy it",
                path.display()
            ));
        }
    }
    Ok(())
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
    fn unsafe_source_names_are_refused_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sources.json");
        std::fs::write(
            &path,
            r#"[{"name":"../escape","url":"https://example.com/r.git"}]"#,
        )
        .unwrap();
        let store = Store::new(&path.display().to_string());
        assert!(store.load().is_err());
    }

    #[test]
    fn git_commands_disable_symlinks() {
        let argv = GitCliBackend::argv(&["clone".to_string(), "--".to_string(), "u".to_string()]);
        assert_eq!(argv[0], "-c");
        assert_eq!(argv[1], "core.symlinks=false");
        assert_eq!(argv[2], "clone");
    }

    fn chart_tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("templates")).unwrap();
        std::fs::write(
            dir.path().join("Chart.yaml"),
            "apiVersion: v2\nname: x\nversion: 0.1.0\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("templates/cm.yaml"), "kind: ConfigMap\n").unwrap();
        dir
    }

    #[test]
    fn a_regular_chart_tree_is_accepted() {
        let dir = chart_tree();
        assert!(assert_regular_tree(dir.path().to_str().unwrap()).is_ok());
    }

    #[test]
    fn a_symlinked_chart_file_is_refused() {
        let dir = chart_tree();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc/hostname", dir.path().join("leak.txt")).unwrap();
        let err = assert_regular_tree(dir.path().to_str().unwrap())
            .expect_err("a symlink must be refused");
        assert!(err.contains("symlink"), "{err}");
    }

    #[test]
    fn an_oversized_chart_file_is_refused() {
        let dir = chart_tree();
        let big = std::fs::File::create(dir.path().join("big.bin")).unwrap();
        big.set_len(6 * 1024 * 1024).unwrap();
        let err = assert_regular_tree(dir.path().to_str().unwrap())
            .expect_err("a 6 MiB chart file must be refused");
        assert!(err.contains("larger than"), "{err}");
    }

    #[test]
    fn a_pinned_commit_must_be_a_full_sha() {
        let mut src = Source {
            name: "naslos".into(),
            url: "https://example.com/r.git".into(),
            ..Default::default()
        };
        assert!(src.validate().is_ok());
        src.commit = "781f192".into();
        assert!(src.validate().is_err());
        src.commit = "781f192de6a54db23f9fc098397d38a0b37d9fc0".into();
        assert!(src.validate().is_ok());
    }

    #[test]
    fn source_urls_are_scheme_restricted() {
        for ok in [
            "https://github.com/o/r.git",
            "ssh://git@example.com/o/r.git",
            "git@example.com:o/r.git",
        ] {
            assert!(validate_source_url(ok).is_ok(), "{ok}");
        }
        for bad in [
            "ext::sh -c touch /tmp/pwned",
            "file:///etc/passwd",
            "git://example.com/r.git",
            "-oProxyCommand=id",
            "https://example.com/r git",
            "ftp://example.com/r",
        ] {
            assert!(validate_source_url(bad).is_err(), "{bad}");
        }
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
    fn source_urls_reject_embedded_credentials() {
        for bad in [
            "https://user:secret@example.com/r.git",
            "https://token@example.com/r.git",
            "ssh://git:secret@example.com/r.git",
        ] {
            assert!(validate_source_url(bad).is_err(), "{bad}");
        }
        for ok in ["ssh://git@example.com/r.git", "git@example.com:o/r.git"] {
            assert!(validate_source_url(ok).is_ok(), "{ok}");
        }
        assert_eq!(
            redact_source_credentials(
                "fatal: https://user:token@example.com/r.git not found; ssh://git@example.com/r.git ok"
            ),
            "fatal: https://[redacted]@example.com/r.git not found; ssh://git@example.com/r.git ok"
        );
        let mut legacy = source("legacy");
        legacy.url = "https://user:token@example.com/r.git".into();
        assert_eq!(
            legacy.sanitized().url,
            "https://[redacted]@example.com/r.git"
        );
    }

    #[test]
    fn the_official_source_name_cannot_be_taken_over() {
        let store = Arc::new(Store::new(""));
        let manager = Manager::new(
            store.clone(),
            "/cache",
            DEFAULT_TTL,
            None,
            Arc::new(GitCliBackend::new()),
        );
        let mut seeded = source("official");
        seeded.official = true;
        manager.add_source(seeded).unwrap();
        let mut impostor = source("official");
        impostor.url = "https://evil.example/r.git".into();
        assert!(manager.add_source(impostor).is_err());

        let reserved = source("mine");
        let store2 = Arc::new(Store::new(""));
        let manager2 = Manager::new(
            store2,
            "/cache",
            DEFAULT_TTL,
            None,
            Arc::new(GitCliBackend::new()),
        );
        manager2.add_source(reserved.clone()).unwrap();
        let mut named_official = source("official");
        named_official.official = false;
        assert!(manager2.add_source(named_official).is_err());
    }

    #[test]
    fn a_chart_snapshot_is_a_private_copy() {
        let dir = tempfile::tempdir().unwrap();
        let app_dir = dir.path().join("official/prod/apps/nginx");
        std::fs::create_dir_all(app_dir.join("templates")).unwrap();
        std::fs::write(
            app_dir.join("Chart.yaml"),
            "apiVersion: v2\nname: nginx\nversion: 1.2.3\n",
        )
        .unwrap();
        std::fs::write(app_dir.join("templates/cm.yaml"), "kind: ConfigMap\n").unwrap();
        let store = Arc::new(Store::new(""));
        let manager = Manager::new(
            store,
            &dir.path().display().to_string(),
            DEFAULT_TTL,
            None,
            Arc::new(GitCliBackend::new()),
        );
        let snapshot = manager.snapshot_app("official", "Prod", "nginx").unwrap();
        assert_ne!(
            snapshot.path,
            app_dir.display().to_string(),
            "the install must not read the shared clone"
        );
        assert!(std::path::Path::new(&snapshot.path)
            .join("Chart.yaml")
            .is_file());
        assert_eq!(chart_version(&snapshot.path).as_deref(), Some("1.2.3"));
        let snapshot_path = snapshot.path.clone();
        drop(snapshot);
        assert!(!std::path::Path::new(&snapshot_path).exists());
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

    #[test]
    fn stat_repo_requires_a_readable_directory() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        assert!(stat_repo(&missing.display().to_string()).is_err());

        let file = dir.path().join("not-a-dir");
        std::fs::write(&file, b"x").unwrap();
        assert!(stat_repo(&file.display().to_string()).is_err());
    }

    #[test]
    fn stat_repo_accepts_empty_and_nested_trees() {
        let empty = tempfile::tempdir().unwrap();
        assert!(stat_repo(&empty.path().display().to_string()).is_ok());

        let nested = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(nested.path().join("apps/nginx/templates")).unwrap();
        std::fs::write(
            nested.path().join("apps/nginx/Chart.yaml"),
            "apiVersion: v2\nname: nginx\nversion: 1.0.0\n",
        )
        .unwrap();
        std::fs::write(
            nested.path().join("apps/nginx/templates/cm.yaml"),
            "kind: ConfigMap\n",
        )
        .unwrap();
        assert!(stat_repo(&nested.path().display().to_string()).is_ok());
    }

    #[test]
    fn stat_repo_refuses_an_oversized_tree_but_skips_git() {
        let dir = tempfile::tempdir().unwrap();
        let big = std::fs::File::create(dir.path().join("big.bin")).unwrap();
        big.set_len(MAX_REPO_BYTES + 1).unwrap();
        let err = stat_repo(&dir.path().display().to_string())
            .expect_err("an oversized clone must be refused");
        assert!(err.contains("larger than"), "{err}");

        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::rename(dir.path().join("big.bin"), dir.path().join(".git/big.bin")).unwrap();
        assert!(stat_repo(&dir.path().display().to_string()).is_ok());
    }
}
