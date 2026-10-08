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

    fn base_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if let Some(kc) = &self.kubeconfig {
            args.push("--kubeconfig".into());
            args.push(kc.display().to_string());
        }
        args
    }

    async fn run(&self, args: &[String]) -> Result<String, String> {
        let mut full = self.base_args();
        full.extend(args.iter().cloned());

        let mut cmd = tokio::process::Command::new(&self.binary);
        cmd.args(&full);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        let output = tokio::time::timeout(Duration::from_secs(360), cmd.output())
            .await
            .map_err(|_| {
                format!(
                    "helm {}: timed out",
                    args.first().cloned().unwrap_or_default()
                )
            })?
            .map_err(|e| format!("running helm: {e}"))?;

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
            "--wait".into(),
            "--timeout".into(),
            WAIT_TIMEOUT.into(),
        ];
        full.append(&mut args);
        self.run(&full).await?;
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
            "--wait".into(),
            "--timeout".into(),
            WAIT_TIMEOUT.into(),
            "--reset-values".into(),
        ];
        full.append(&mut args);
        self.run(&full).await?;
        self.get(name).await
    }

    /// Uninstall a release (`--wait`, 5m timeout).
    pub async fn uninstall(&self, name: &str) -> Result<(), String> {
        self.run(&[
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
    pub async fn list(&self) -> Result<Vec<App>, String> {
        let out = self
            .run(&[
                "list".into(),
                "-n".into(),
                self.namespace.clone(),
                "-o".into(),
                "json".into(),
                "--all".into(),
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
        self.run(&[
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

/// Split a `helm list` chart string ("nginx-15.0.0") into (name, version).
pub fn split_chart(chart: &str) -> (String, String) {
    match chart.rfind('-') {
        Some(i) => (chart[..i].to_string(), chart[i + 1..].to_string()),
        None => (chart.to_string(), String::new()),
    }
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

/// Resolve a kubeconfig for the helm CLI: `HELM_KUBECONFIG`, else a generated
/// in-cluster one, else `None` (ambient `KUBECONFIG`/`~/.kube/config`).
fn resolve_kubeconfig() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("HELM_KUBECONFIG") {
        if !path.is_empty() {
            return Some(PathBuf::from(path));
        }
    }
    in_cluster_kubeconfig()
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

    let path = std::env::temp_dir().join("naslos-helm-kubeconfig.yaml");
    std::fs::write(&path, yaml).ok()?;
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
