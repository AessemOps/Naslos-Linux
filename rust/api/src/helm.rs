//! Helm lifecycle management via the bundled `helm` CLI (port of
//! `api/internal/helm`).
//!
//! The Go implementation used the Helm SDK; there is no Rust equivalent, so the
//! plan's decision is to bundle the `helm` binary and drive it with `-o json`.
//! The SDK resolved its Kubernetes config in-cluster, which the CLI does not do
//! automatically, so `Client` builds a kubeconfig from the pod's service
//! account when none is provided.

use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

/// Matches the Go client's `5 * time.Minute`.
const WAIT_TIMEOUT: &str = "5m0s";
/// How long a read-only Helm call may run.
const READ_TIMEOUT: Duration = Duration::from_secs(120);
/// A write can wait for its rollout and then roll back with another full wait
/// (`--rollback-on-failure`), so the outer guard must exceed two Helm waits.
/// The previous 360 s guard killed the process in the middle of Helm's own
/// rollback and left a pending release behind (R-022).
const WRITE_TIMEOUT: Duration = Duration::from_secs(20 * 60);
/// How long a detached cleanup waits before retrying a timed-out release.
const CLEANUP_DELAY: Duration = Duration::from_secs(30);

/// Write flags shared by install and upgrade.
///
/// Helm v4 applies release objects server-side. The image-refresh pass also
/// patches `imagePullPolicy` on the drifted container with `kubectl`, which
/// makes `kubectl-patch` a second field manager on the Deployment; without
/// `--force-conflicts` the next upgrade (and its automatic rollback) fails with
/// `Apply failed with 1 conflict: ... imagePullPolicy` and leaves the release
/// `failed`. Helm owns the app's objects, so it resolves the conflict.
/// `--rollback-on-failure` is the Helm v4 name for the v3 `--atomic`; the old
/// flag still works but prints a deprecation warning that reads like the
/// failure in the app job log.
fn write_flags(reset_values: bool) -> Vec<String> {
    let mut flags = vec![
        "--wait".to_string(),
        "--timeout".to_string(),
        WAIT_TIMEOUT.to_string(),
        "--rollback-on-failure".to_string(),
        "--force-conflicts".to_string(),
    ];
    if reset_values {
        flags.push("--reset-values".to_string());
    }
    flags
}

/// An installed application.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct App {
    pub name: String,
    pub namespace: String,
    pub chart: String,
    pub version: String,
    #[serde(default)]
    pub values: serde_json::Value,
    pub status: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub ports: Vec<i64>,
    #[serde(default)]
    pub url: String,
}

/// Helm CLI adapter, scoped to one namespace.
#[derive(Clone)]
pub struct Client {
    namespace: String,
    binary: String,
    kubeconfig: Option<PathBuf>,
}

impl Client {
    pub fn new(namespace: &str) -> Self {
        Self {
            namespace: namespace.to_string(),
            binary: std::env::var("HELM_BIN").unwrap_or_else(|_| "helm".to_string()),
            kubeconfig: resolve_kubeconfig(),
        }
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// A client with the same binary/kubeconfig but a different target
    /// namespace (per-app isolation, D3/F1).
    pub fn for_namespace(&self, namespace: &str) -> Self {
        Self {
            binary: self.binary.clone(),
            kubeconfig: self.kubeconfig.clone(),
            namespace: namespace.to_string(),
        }
    }

    fn base_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if let Some(kc) = &self.kubeconfig {
            args.push("--kubeconfig".into());
            args.push(kc.display().to_string());
        }
        args
    }

    async fn run(&self, args: &[String]) -> Result<String, String> {
        self.run_with_timeout(args, READ_TIMEOUT).await
    }

    /// Run a mutating Helm command with a guard that outlives its `--wait`
    /// plus the automatic rollback's own wait.
    async fn run_write(&self, args: &[String]) -> Result<String, String> {
        self.run_with_timeout(args, WRITE_TIMEOUT).await
    }

    async fn run_with_timeout(&self, args: &[String], timeout: Duration) -> Result<String, String> {
        let mut full = self.base_args();
        full.extend(args.iter().cloned());

        let mut cmd = tokio::process::Command::new(&self.binary);
        crate::proc::scrub(&mut cmd);
        cmd.kill_on_drop(true);
        cmd.args(&full);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        let output = match tokio::time::timeout(timeout, cmd.output()).await {
            Ok(output) => output.map_err(|e| format!("running helm: {e}"))?,
            Err(_) => {
                let command = args.first().cloned().unwrap_or_default();
                // The process is killed on drop, but Helm may have already
                // created the release (or started a rollback). Schedule a
                // detached cleanup so a timed-out write cannot leave a
                // `pending-*` release that blocks the next attempt.
                self.spawn_detached_cleanup(args);
                return Err(format!("helm {command}: timed out"));
            }
        };

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "helm {}: {}",
                args.first().cloned().unwrap_or_default(),
                err.trim()
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Best-effort cleanup after the outer guard killed a write. This runs on
    /// its own task and never blocks the request path.
    fn spawn_detached_cleanup(&self, args: &[String]) {
        let Some(name) = args.get(1).cloned() else {
            return;
        };
        let Some(command) = args.first().cloned() else {
            return;
        };
        let client = self.clone();
        if command == "install" {
            tokio::spawn(async move {
                tokio::time::sleep(CLEANUP_DELAY).await;
                if let Err(e) = client
                    .run_with_timeout(
                        &[
                            "uninstall".into(),
                            name.clone(),
                            "-n".into(),
                            client.namespace.clone(),
                        ],
                        Duration::from_secs(120),
                    )
                    .await
                {
                    tracing::warn!("helm: detached cleanup of {name} failed: {e}");
                } else {
                    tracing::warn!("helm: detached cleanup removed the timed-out release {name}");
                }
            });
        } else if command == "upgrade" {
            tokio::spawn(async move {
                tokio::time::sleep(CLEANUP_DELAY).await;
                if let Err(e) = client
                    .run_with_timeout(
                        &[
                            "rollback".into(),
                            name.clone(),
                            "0".into(),
                            "-n".into(),
                            client.namespace.clone(),
                            "--wait".into(),
                            "--timeout".into(),
                            WAIT_TIMEOUT.into(),
                        ],
                        Duration::from_secs(10 * 60),
                    )
                    .await
                {
                    tracing::warn!("helm: detached rollback of {name} failed: {e}");
                }
            });
        }
    }

    /// Write values to a temp file and return `-f <path>` args.
    fn values_args(
        values: &serde_json::Value,
    ) -> Result<(Vec<String>, Option<tempfile::NamedTempFile>), String> {
        if values.is_null() || values.as_object().map(|o| o.is_empty()).unwrap_or(false) {
            return Ok((Vec::new(), None));
        }
        let yaml =
            serde_json::to_string_pretty(values).map_err(|e| format!("marshaling values: {e}"))?;
        let mut file = tempfile::Builder::new()
            .prefix("naslos-values-")
            .suffix(".json")
            .tempfile()
            .map_err(|e| format!("creating values file: {e}"))?;
        use std::io::Write;
        file.as_file_mut()
            .write_all(yaml.as_bytes())
            .map_err(|e| format!("writing values file: {e}"))?;
        let path = file.path().display().to_string();
        Ok((vec!["-f".into(), path], Some(file)))
    }

    /// The chart's own `values.yaml` defaults (used to discover persistence
    /// keys before a pool-backed install).
    pub async fn show_values(&self, chart_dir: &str) -> Result<serde_json::Value, String> {
        let out = self
            .run(&["show".into(), "values".into(), chart_dir.to_string()])
            .await?;
        serde_yaml::from_str(&out).map_err(|e| format!("parsing chart values: {e}"))
    }

    /// Render a chart with the values an install/upgrade would use. Used to
    /// discover the names the chart itself sets on the containers that consume
    /// the platform env Secret (FR-APP-26).
    pub async fn template_dir(
        &self,
        name: &str,
        chart_dir: &str,
        values: &serde_json::Value,
    ) -> Result<String, String> {
        let (mut args, _guard) = Self::values_args(values)?;
        let mut full = vec![
            "template".to_string(),
            name.to_string(),
            chart_dir.to_string(),
            "-n".into(),
            self.namespace.clone(),
        ];
        full.append(&mut args);
        self.run(&full).await
    }

    /// Install a chart from a local directory (`--wait`, 5m timeout).
    pub async fn install_dir(
        &self,
        name: &str,
        chart_dir: &str,
        values: &serde_json::Value,
    ) -> Result<App, String> {
        let (mut args, _guard) = Self::values_args(values)?;
        let mut full = vec![
            "install".to_string(),
            name.to_string(),
            chart_dir.to_string(),
            "-n".into(),
            self.namespace.clone(),
        ];
        full.extend(write_flags(false));
        full.append(&mut args);
        self.run_write(&full).await?;
        self.get(name).await
    }

    /// Upgrade a release from a local chart directory. `--reset-values` mirrors
    /// the Go `ResetValues=true` (the UI reconfigure round-trips exact values).
    pub async fn upgrade_dir(
        &self,
        name: &str,
        chart_dir: &str,
        values: &serde_json::Value,
    ) -> Result<App, String> {
        let (mut args, _guard) = Self::values_args(values)?;
        let mut full = vec![
            "upgrade".to_string(),
            name.to_string(),
            chart_dir.to_string(),
            "-n".into(),
            self.namespace.clone(),
        ];
        full.extend(write_flags(true));
        full.append(&mut args);
        self.run_write(&full).await?;
        self.get(name).await
    }

    /// Uninstall a release (`--wait`, 5m timeout).
    pub async fn uninstall(&self, name: &str) -> Result<(), String> {
        self.run_write(&[
            "uninstall".into(),
            name.into(),
            "-n".into(),
            self.namespace.clone(),
            "--wait".into(),
            "--timeout".into(),
            WAIT_TIMEOUT.into(),
        ])
        .await
        .map(|_| ())
    }

    /// List every release in the namespace.
    ///
    /// Helm v4 lists releases in any status by default; the v3 `--all` flag no
    /// longer exists (it errors with "unknown flag: --all").
    pub async fn list(&self) -> Result<Vec<App>, String> {
        let out = self
            .run(&[
                "list".into(),
                "-n".into(),
                self.namespace.clone(),
                "-o".into(),
                "json".into(),
            ])
            .await?;
        let entries: Vec<serde_json::Value> = serde_json::from_str(out.trim()).unwrap_or_default();
        Ok(entries.iter().map(app_from_list_entry).collect())
    }

    /// Details of a specific release.
    pub async fn get(&self, name: &str) -> Result<App, String> {
        let status_out = self
            .run(&[
                "status".into(),
                name.into(),
                "-n".into(),
                self.namespace.clone(),
                "-o".into(),
                "json".into(),
            ])
            .await?;
        let values_out = self
            .run(&[
                "get".into(),
                "values".into(),
                name.into(),
                "-n".into(),
                self.namespace.clone(),
                "-o".into(),
                "json".into(),
            ])
            .await
            .unwrap_or_else(|_| "null".to_string());

        let status_json: serde_json::Value = serde_json::from_str(status_out.trim())
            .map_err(|e| format!("parsing helm status: {e}"))?;
        let values_json: serde_json::Value =
            serde_json::from_str(values_out.trim()).unwrap_or(serde_json::Value::Null);
        Ok(app_from_status(&status_json, values_json, &self.namespace))
    }

    /// Roll back a release to a revision.
    pub async fn rollback(&self, name: &str, revision: i64) -> Result<(), String> {
        self.run_write(&[
            "rollback".into(),
            name.into(),
            revision.to_string(),
            "-n".into(),
            self.namespace.clone(),
            "--wait".into(),
            "--timeout".into(),
            WAIT_TIMEOUT.into(),
        ])
        .await
        .map(|_| ())
    }
}

/// Map a Helm release status to the API's human-readable status.
pub fn release_status(status: &str) -> &str {
    match status.to_lowercase().as_str() {
        "deployed" => "running",
        "failed" => "failed",
        "pending-install" | "pending-upgrade" | "pending-rollback" => "pending",
        "uninstalled" | "uninstalling" => "stopped",
        "superseded" => "superseded",
        _ => status,
    }
}

/// Split a `helm list` chart string into (name, version).
///
/// The chart part is `"<name>-<version>"`, but both may contain hyphens:
/// `my-app-1.2.3` is chart `my-app`, while `nginx-1.2.3-rc.1+build.5` is chart
/// `nginx` with a prerelease/build suffix. Splitting on the last hyphen (the
/// old behavior) reported `1.2.3-rc.1` as the chart name and `rc.1` as the
/// version. Scan hyphens from the right and accept the first suffix that is a
/// complete semver, including prerelease/build identifiers (R-066).
pub fn split_chart(chart: &str) -> (String, String) {
    let chart = chart.trim();
    if chart.is_empty() {
        return (String::new(), String::new());
    }
    for (index, _) in chart.match_indices('-').rev() {
        let candidate = &chart[index + 1..];
        if is_semver_with_suffix(candidate) {
            return (chart[..index].to_string(), candidate.to_string());
        }
    }
    (chart.to_string(), String::new())
}

/// `X.Y.Z` with optional `-prerelease` and `+build` suffixes. Helm chart
/// versions are semver 2.0, so the strict release-only `crate::semver` parser
/// is not enough here.
fn is_semver_with_suffix(value: &str) -> bool {
    let (core_and_pre, build) = match value.split_once('+') {
        Some((left, right)) => (left, Some(right)),
        None => (value, None),
    };
    if let Some(build) = build {
        if !valid_semver_identifiers(build) {
            return false;
        }
    }
    let (core, prerelease) = match core_and_pre.split_once('-') {
        Some((left, right)) => (left, Some(right)),
        None => (core_and_pre, None),
    };
    if let Some(prerelease) = prerelease {
        if !valid_semver_identifiers(prerelease) {
            return false;
        }
    }
    let mut parts = core.split('.');
    let valid_number = |part: Option<&str>| {
        part.is_some_and(|part| {
            !part.is_empty()
                && part.bytes().all(|b| b.is_ascii_digit())
                && (part.len() == 1 || !part.starts_with('0'))
        })
    };
    valid_number(parts.next())
        && valid_number(parts.next())
        && valid_number(parts.next())
        && parts.next().is_none()
}

fn valid_semver_identifiers(value: &str) -> bool {
    !value.is_empty()
        && value.split('.').all(|part| {
            !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

/// Parse `helm list`'s `updated` field ("2026-10-04 12:00:00.000000 +0000 UTC").
pub fn parse_helm_time(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    // Keep the leading "YYYY-MM-DD HH:MM:SS" and treat it as UTC.
    let head: String = s.chars().take(19).collect();
    let iso = head.replacen(' ', "T", 1);
    chrono::NaiveDateTime::parse_from_str(&iso, "%Y-%m-%dT%H:%M:%S")
        .ok()
        .map(|naive| naive.and_utc())
}

fn app_from_list_entry(entry: &serde_json::Value) -> App {
    let chart_str = entry.get("chart").and_then(|v| v.as_str()).unwrap_or("");
    let (chart, version) = split_chart(chart_str);
    App {
        name: entry
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        namespace: entry
            .get("namespace")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        chart,
        version,
        status: release_status(entry.get("status").and_then(|v| v.as_str()).unwrap_or(""))
            .to_string(),
        updated_at: entry
            .get("updated")
            .and_then(|v| v.as_str())
            .and_then(parse_helm_time),
        ..Default::default()
    }
}

fn app_from_status(
    status: &serde_json::Value,
    values: serde_json::Value,
    fallback_ns: &str,
) -> App {
    let chart = status.get("chart").and_then(|v| v.as_str()).unwrap_or("");
    let (chart_name, version) = split_chart(chart);
    let ns = status
        .get("namespace")
        .and_then(|v| v.as_str())
        .unwrap_or(fallback_ns)
        .to_string();
    App {
        name: status
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        namespace: ns,
        chart: chart_name,
        version,
        status: release_status(
            status
                .get("info")
                .and_then(|i| i.get("status"))
                .and_then(|v| v.as_str())
                .unwrap_or(""),
        )
        .to_string(),
        updated_at: status
            .get("info")
            .and_then(|i| i.get("last_deployed"))
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc)),
        values,
        ..Default::default()
    }
}

/// Resolve a kubeconfig for the helm CLI: `HELM_KUBECONFIG` (development),
/// else the reduced `naslos-helm` service account, else the API's own account.
///
/// Charts must NOT be applied with the API's identity: that account can create
/// Traefik routes and cert-manager objects, so a malicious chart could become
/// same-origin with the UI (SEC-2026-D1/D4). The chart grants `naslos-helm` only
/// workload resources.
pub fn resolve_kubeconfig() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("HELM_KUBECONFIG") {
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    if let (Ok(token), Ok(ca)) = (
        std::env::var("HELM_SA_TOKEN_FILE"),
        std::env::var("HELM_SA_CA_FILE"),
    ) {
        if !token.is_empty() && !ca.is_empty() {
            return helm_sa_kubeconfig(&token, &ca);
        }
    }
    in_cluster_kubeconfig()
}

/// The API's own kubeconfig (routing, exec, certs): never the Helm account.
pub fn resolve_api_kubeconfig() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("HELM_KUBECONFIG") {
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    in_cluster_kubeconfig()
}

fn helm_sa_kubeconfig(token_file: &str, ca_file: &str) -> Option<PathBuf> {
    let host = std::env::var("KUBERNETES_SERVICE_HOST").ok()?;
    let port = std::env::var("KUBERNETES_SERVICE_PORT").unwrap_or_else(|_| "443".to_string());
    let namespace = std::env::var("APPS_NAMESPACE").unwrap_or_else(|_| "naslos-apps".to_string());
    let yaml = build_sa_kubeconfig(&host, &port, &namespace, token_file, ca_file);
    let path = std::env::temp_dir().join(format!(
        "naslos-helm-sa-kubeconfig-{}.yaml",
        std::process::id()
    ));
    crate::chartsrepo::write_atomic(&path.display().to_string(), yaml.as_bytes(), 0o600, 0o700)
        .ok()?;
    Some(path)
}

fn build_sa_kubeconfig(
    host: &str,
    port: &str,
    namespace: &str,
    token_file: &str,
    ca_file: &str,
) -> String {
    format!(
        "apiVersion: v1\nkind: Config\nclusters:\n- name: naslos-helm\n  cluster:\n    server: https://{host}:{port}\n    certificate-authority: {ca_file}\ncontexts:\n- name: naslos-helm\n  context:\n    cluster: naslos-helm\n    user: naslos-helm\n    namespace: {namespace}\ncurrent-context: naslos-helm\nusers:\n- name: naslos-helm\n  user:\n    tokenFile: {token_file}\n"
    )
}

/// Build a kubeconfig from the pod's service account, if running in-cluster.
fn in_cluster_kubeconfig() -> Option<PathBuf> {
    let host = std::env::var("KUBERNETES_SERVICE_HOST").ok()?;
    let port = std::env::var("KUBERNETES_SERVICE_PORT").unwrap_or_else(|_| "443".to_string());
    let sa = Path::new("/var/run/secrets/kubernetes.io/serviceaccount");
    let token = sa.join("token");
    let ca = sa.join("ca.crt");
    if !token.exists() {
        return None;
    }
    let namespace = std::fs::read_to_string(sa.join("namespace"))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "default".to_string());

    let yaml = format!(
        "apiVersion: v1\nkind: Config\nclusters:\n- name: in-cluster\n  cluster:\n    server: https://{host}:{port}\n    certificate-authority: {ca}\ncontexts:\n- name: in-cluster\n  context:\n    cluster: in-cluster\n    user: in-cluster\n    namespace: {namespace}\ncurrent-context: in-cluster\nusers:\n- name: in-cluster\n  user:\n    tokenFile: {token}\n",
        ca = ca.display(),
        token = token.display(),
    );

    // Per-process path and 0600: the file points at the SA token (SEC-2026-B11).
    let path = std::env::temp_dir().join(format!(
        "naslos-helm-kubeconfig-{}.yaml",
        std::process::id()
    ));
    crate::chartsrepo::write_atomic(&path.display().to_string(), yaml.as_bytes(), 0o600, 0o700)
        .ok()?;
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_flags_force_conflicts_and_avoid_the_deprecated_atomic_flag() {
        let install = write_flags(false);
        assert!(install.contains(&"--force-conflicts".to_string()));
        assert!(install.contains(&"--rollback-on-failure".to_string()));
        assert!(!install.contains(&"--atomic".to_string()));
        assert!(!install.contains(&"--reset-values".to_string()));

        let upgrade = write_flags(true);
        assert!(upgrade.contains(&"--force-conflicts".to_string()));
        assert!(upgrade.contains(&"--rollback-on-failure".to_string()));
        assert!(upgrade.contains(&"--reset-values".to_string()));
        assert!(!upgrade.contains(&"--atomic".to_string()));
    }

    #[test]
    fn the_helm_sa_kubeconfig_uses_the_mounted_token() {
        let yaml = build_sa_kubeconfig(
            "10.96.0.1",
            "443",
            "naslos-apps",
            "/var/run/secrets/naslos-helm/token",
            "/var/run/secrets/naslos-helm/ca.crt",
        );
        assert!(yaml.contains("tokenFile: /var/run/secrets/naslos-helm/token"));
        assert!(yaml.contains("namespace: naslos-apps"));
        assert!(yaml.contains("server: https://10.96.0.1:443"));
    }

    #[test]
    fn release_status_mapping() {
        assert_eq!(release_status("deployed"), "running");
        assert_eq!(release_status("pending-upgrade"), "pending");
        assert_eq!(release_status("uninstalled"), "stopped");
        assert_eq!(release_status("weird"), "weird");
    }

    #[test]
    fn split_chart_name_and_version() {
        assert_eq!(
            split_chart("nginx-15.0.0"),
            ("nginx".to_string(), "15.0.0".to_string())
        );
        assert_eq!(
            split_chart("my-app-1.2.3"),
            ("my-app".to_string(), "1.2.3".to_string())
        );
        assert_eq!(
            split_chart("standalone"),
            ("standalone".to_string(), String::new())
        );
        assert_eq!(
            split_chart("nginx-1.2.3-rc.1"),
            ("nginx".to_string(), "1.2.3-rc.1".to_string())
        );
        assert_eq!(
            split_chart("my-app-1.2.3-rc.1+build.5"),
            ("my-app".to_string(), "1.2.3-rc.1+build.5".to_string())
        );
        assert_eq!(
            split_chart("nginx-1.2.3+build.5"),
            ("nginx".to_string(), "1.2.3+build.5".to_string())
        );
        assert_eq!(
            split_chart("suffix-only-1.2"),
            ("suffix-only-1.2".to_string(), String::new())
        );
        assert_eq!(
            split_chart("not-a-version-x.y.z"),
            ("not-a-version-x.y.z".to_string(), String::new())
        );
    }

    #[test]
    fn parse_helm_time_from_list() {
        let t = parse_helm_time("2026-10-04 12:34:56.000000 +0000 UTC").unwrap();
        assert_eq!(t.to_rfc3339(), "2026-10-04T12:34:56+00:00");
        assert!(parse_helm_time("").is_none());
    }

    #[test]
    fn list_entry_maps_to_app() {
        let entry: serde_json::Value = serde_json::json!({
            "name": "nginx", "namespace": "naslos-apps", "chart": "nginx-15.0.0",
            "status": "deployed", "updated": "2026-10-04 12:34:56.000000 +0000 UTC"
        });
        let app = app_from_list_entry(&entry);
        assert_eq!(app.name, "nginx");
        assert_eq!(app.chart, "nginx");
        assert_eq!(app.version, "15.0.0");
        assert_eq!(app.status, "running");
    }

    #[test]
    fn values_args_skip_null_and_empty_objects() {
        let (args, file) = Client::values_args(&serde_json::Value::Null).unwrap();
        assert!(args.is_empty());
        assert!(file.is_none());

        let (args, file) = Client::values_args(&serde_json::json!({})).unwrap();
        assert!(args.is_empty());
        assert!(file.is_none());
    }

    #[test]
    fn values_args_write_a_temp_json_file() {
        let values = serde_json::json!({"replicaCount": 2, "image": {"tag": "x"}});
        let (args, file) = Client::values_args(&values).unwrap();
        let file = file.expect("a values file");
        assert_eq!(args[0], "-f");
        assert_eq!(args[1], file.path().display().to_string());
        assert!(std::path::Path::new(&args[1]).exists());
        let contents = std::fs::read_to_string(&args[1]).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();
        assert_eq!(parsed, values);
    }

    #[test]
    fn status_maps_to_app_with_values_and_timestamp() {
        let status = serde_json::json!({
            "name": "nginx",
            "namespace": "naslos-apps",
            "chart": "nginx-15.0.0",
            "info": {"status": "deployed", "last_deployed": "2026-10-04T12:34:56Z"}
        });
        let values = serde_json::json!({"replicaCount": 2});
        let app = app_from_status(&status, values.clone(), "fallback");
        assert_eq!(app.name, "nginx");
        assert_eq!(app.namespace, "naslos-apps");
        assert_eq!(app.chart, "nginx");
        assert_eq!(app.version, "15.0.0");
        assert_eq!(app.status, "running");
        assert_eq!(
            app.updated_at.unwrap().to_rfc3339(),
            "2026-10-04T12:34:56+00:00"
        );
        assert_eq!(app.values, values);
    }

    #[test]
    fn status_without_namespace_falls_back_and_missing_fields_stay_empty() {
        let app = app_from_status(&serde_json::json!({}), serde_json::json!({}), "naslos-apps");
        assert_eq!(app.namespace, "naslos-apps");
        assert!(app.name.is_empty());
        assert!(app.chart.is_empty());
        assert!(app.version.is_empty());
        assert_eq!(app.status, release_status(""));
        assert!(app.updated_at.is_none());
        assert_eq!(app.values, serde_json::json!({}));
    }
}
