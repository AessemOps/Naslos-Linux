//! A small Kubernetes client over the bundled `kubectl` CLI (port of the
//! client-go dynamic/typed calls the reconcilers need).
//!
//! Consistent with the plan's bundled-CLI decision for helm/talosctl: rather
//! than pull in kube-rs, drive `kubectl` with `-o json` and reuse the
//! in-cluster kubeconfig the helm adapter builds.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

/// A `kubectl`-backed Kubernetes client.
pub struct Client {
    binary: String,
    kubeconfig: Option<PathBuf>,
    /// The default namespace for namespaced resources.
    pub namespace: String,
}

impl Client {
    pub fn new(namespace: &str) -> Self {
        Self {
            binary: std::env::var("KUBECTL_BIN").unwrap_or_else(|_| "kubectl".to_string()),
            kubeconfig: crate::helm::resolve_kubeconfig(),
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

    async fn run(&self, args: &[String], stdin: Option<String>) -> Result<String, String> {
        let mut full = self.base_args();
        full.extend(args.iter().cloned());

        let mut cmd = tokio::process::Command::new(&self.binary);
        cmd.args(&full);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        if stdin.is_some() {
            cmd.stdin(Stdio::piped());
        }

        let mut child = cmd.spawn().map_err(|e| format!("running kubectl: {e}"))?;
        if let Some(data) = stdin {
            use tokio::io::AsyncWriteExt;
            if let Some(mut si) = child.stdin.take() {
                si.write_all(data.as_bytes())
                    .await
                    .map_err(|e| format!("writing to kubectl stdin: {e}"))?;
                let _ = si.shutdown().await;
            }
        }

        let output = tokio::time::timeout(Duration::from_secs(60), child.wait_with_output())
            .await
            .map_err(|_| "kubectl: timed out".to_string())?
            .map_err(|e| format!("kubectl: {e}"))?;

        if !output.status.success() {
            return Err(format!(
                "kubectl {}: {}",
                args.first().cloned().unwrap_or_default(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Server-side apply one object (JSON) with the naslos field manager.
    pub async fn apply(&self, object: &serde_json::Value) -> Result<(), String> {
        let data = serde_json::to_string(object).map_err(|e| e.to_string())?;
        self.run(
            &[
                "apply".into(),
                "-f".into(),
                "-".into(),
                "--server-side".into(),
                "--field-manager".into(),
                "naslos-api".into(),
            ],
            Some(data),
        )
        .await
        .map(|_| ())
    }

    /// Delete a resource, ignoring a missing one.
    pub async fn delete(&self, kind: &str, name: &str, namespace: &str) -> Result<(), String> {
        self.run(
            &[
                "delete".into(),
                kind.into(),
                name.into(),
                "-n".into(),
                namespace.into(),
                "--ignore-not-found".into(),
            ],
            None,
        )
        .await
        .map(|_| ())
    }

    /// Fetch a resource as JSON; `Ok(None)` when it does not exist.
    pub async fn get_json(
        &self,
        kind: &str,
        name: &str,
        namespace: &str,
    ) -> Result<Option<serde_json::Value>, String> {
        match self
            .run(
                &[
                    "get".into(),
                    kind.into(),
                    name.into(),
                    "-n".into(),
                    namespace.into(),
                    "-o".into(),
                    "json".into(),
                ],
                None,
            )
            .await
        {
            Ok(out) => Ok(serde_json::from_str(out.trim()).ok()),
            Err(e) if e.contains("NotFound") || e.contains("not found") => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// List resources as JSON (a `kubectl get ... -o json` items array).
    pub async fn list_json(
        &self,
        kind: &str,
        namespace: &str,
        selector: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        let mut args = vec![
            "get".to_string(),
            kind.to_string(),
            "-n".to_string(),
            namespace.to_string(),
            "-o".to_string(),
            "json".to_string(),
        ];
        if let Some(sel) = selector {
            args.push("-l".into());
            args.push(sel.into());
        }
        let out = self.run(&args, None).await?;
        serde_json::from_str(out.trim()).map_err(|e| format!("parsing kubectl output: {e}"))
    }

    /// Delete a pod so its controller recreates it (Authelia restart).
    pub async fn delete_pod(&self, name: &str, namespace: &str) -> Result<(), String> {
        self.delete("pod", name, namespace).await
    }

    /// Merge-patch a resource (used for the SSO ConfigMap's `data` keys, so the
    /// chart's ownership of other fields is untouched).
    pub async fn patch_merge(
        &self,
        kind: &str,
        name: &str,
        namespace: &str,
        patch: &serde_json::Value,
    ) -> Result<(), String> {
        let data = serde_json::to_string(patch).map_err(|e| e.to_string())?;
        self.run(
            &[
                "patch".into(),
                kind.into(),
                name.into(),
                "-n".into(),
                namespace.into(),
                "--type".into(),
                "merge".into(),
                "-p".into(),
                data,
            ],
            None,
        )
        .await
        .map(|_| ())
    }

    /// Write a Secret's string data (server-side apply).
    pub async fn write_secret(
        &self,
        namespace: &str,
        name: &str,
        data: &std::collections::HashMap<String, String>,
    ) -> Result<(), String> {
        let object = serde_json::json!({
            "apiVersion": "v1",
            "kind": "Secret",
            "metadata": { "name": name, "namespace": namespace },
            "type": "Opaque",
            "stringData": data,
        });
        self.apply(&object).await
    }
}

/// Whether the cert-manager Certificate CRD is present (best-effort probe).
pub async fn cert_manager_available(client: &Client) -> bool {
    client
        .run(
            &[
                "get".into(),
                "crds".into(),
                "certificates.cert-manager.io".into(),
                "-o".into(),
                "name".into(),
            ],
            None,
        )
        .await
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_args_include_kubeconfig_when_present() {
        let c = Client {
            binary: "kubectl".into(),
            kubeconfig: Some(PathBuf::from("/tmp/kc")),
            namespace: "naslos".into(),
        };
        assert_eq!(
            c.base_args(),
            vec!["--kubeconfig".to_string(), "/tmp/kc".to_string()]
        );
    }

    #[test]
    fn base_args_empty_without_kubeconfig() {
        let c = Client {
            binary: "kubectl".into(),
            kubeconfig: None,
            namespace: "naslos".into(),
        };
        assert!(c.base_args().is_empty());
    }
}
