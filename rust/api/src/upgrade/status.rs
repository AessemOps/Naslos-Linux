//! Operation status in the `naslos-system-operation` ConfigMap.
//!
//! The ConfigMap is the lock and the UI's progress source. It lives in etcd,
//! so it survives API restarts and the Talos reboot; a Job pod that dies with
//! the reboot simply writes the next state when it comes back.

use crate::system::OPERATION_CONFIGMAP;
use crate::upgrade::commands;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Step {
    pub name: String,
    pub status: String,
    pub message: String,
    #[serde(rename = "startedAt", default)]
    pub started_at: String,
    #[serde(rename = "finishedAt", default)]
    pub finished_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OperationStatus {
    pub id: String,
    pub op: String,
    pub from: String,
    pub to: String,
    pub phase: String,
    pub steps: Vec<Step>,
    pub message: String,
    #[serde(rename = "startedAt", default)]
    pub started_at: String,
    #[serde(rename = "finishedAt", default)]
    pub finished_at: String,
    pub result: String,
    /// How many times the Job pod has started while `rebooting` (Talos A/B
    /// detection: a second start still on the old version means rollback).
    #[serde(default)]
    pub attempts: u32,
}

pub struct Writer {
    namespace: String,
    state_dir: std::path::PathBuf,
    status: OperationStatus,
}

impl Writer {
    pub fn new(
        namespace: &str,
        state_dir: &str,
        id: &str,
        op: crate::upgrade::Operation,
        from: &str,
        to: &str,
    ) -> Self {
        Self {
            namespace: namespace.to_string(),
            state_dir: std::path::PathBuf::from(state_dir),
            status: OperationStatus {
                id: id.to_string(),
                op: match op {
                    crate::upgrade::Operation::Naslos => "naslos",
                    crate::upgrade::Operation::Talos => "talos",
                    crate::upgrade::Operation::Kubernetes => "kubernetes",
                }
                .into(),
                from: from.to_string(),
                to: to.to_string(),
                phase: "pending".into(),
                started_at: now(),
                ..Default::default()
            },
        }
    }

    pub fn current_phase(&self) -> &str {
        &self.status.phase
    }

    pub fn attempts(&self) -> u32 {
        self.status.attempts
    }

    /// Adopt the existing ConfigMap when it belongs to this operation (a Job
    /// retry after a reboot), otherwise start a fresh state.
    pub async fn load_or_init(&mut self) -> Option<String> {
        // The local marker is the retry source of truth: during the Talos
        // reboot the API server (and this pod) go away, and the marker lives
        // on the state PVC. The ConfigMap is the lock/UI source and is updated
        // on every save once the API server answers again.
        let mut adopted = self.read_marker();
        if adopted.is_none() {
            adopted = self.read().await;
        }
        if let Some(existing) = adopted {
            if existing.id == self.status.id {
                self.status = existing;
                if self.status.phase == "rebooting" {
                    self.status.attempts = self.status.attempts.saturating_add(1);
                }
            } else if crate::system::active_phase(&existing.phase) {
                return Some(format!(
                    "another system operation ({}) is {}",
                    existing.id, existing.phase
                ));
            }
        }
        self.save().await;
        None
    }

    fn marker_path(&self) -> std::path::PathBuf {
        self.state_dir
            .join("updates")
            .join(format!("operation-{}.json", self.status.id))
    }

    fn read_marker(&self) -> Option<OperationStatus> {
        let raw = std::fs::read(self.marker_path()).ok()?;
        serde_json::from_slice(&raw).ok()
    }

    async fn read(&self) -> Option<OperationStatus> {
        let mut args = self.kubectl_args();
        args.extend([
            "get".into(),
            "configmap".into(),
            OPERATION_CONFIGMAP.into(),
            "-n".into(),
            self.namespace.clone(),
            "-o".into(),
            "json".into(),
        ]);
        let out = commands::run(&commands::kubectl(), &args, Duration::from_secs(20))
            .await
            .ok()?;
        let value: serde_json::Value = serde_json::from_str(out.trim()).ok()?;
        let state = value.get("data")?.get("state")?.as_str()?;
        serde_json::from_str(state).ok()
    }

    async fn save(&self) {
        // Write the local marker first so a reboot/retry never loses the phase
        // and attempts, even if the API server is unreachable.
        if let Ok(raw) = serde_json::to_vec(&self.status) {
            let path = self.marker_path();
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, raw).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }

        let object = serde_json::json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": {
                "name": OPERATION_CONFIGMAP,
                "namespace": self.namespace,
                "labels": {"app.kubernetes.io/name": "naslos-system-op"},
            },
            "stringData": {"state": serde_json::to_string(&self.status).unwrap_or_default()},
        });
        let mut args = self.kubectl_args();
        args.extend([
            "apply".into(),
            "-f".into(),
            "-".into(),
            "--server-side".into(),
            "--field-manager".into(),
            "naslos-upgrade".into(),
        ]);
        let raw = serde_json::to_string(&object).unwrap_or_default();
        if let Err(e) =
            commands::run_stdin(&commands::kubectl(), &args, &raw, Duration::from_secs(30)).await
        {
            tracing::warn!("writing the operation status: {e}");
        }
    }

    fn kubectl_args(&self) -> Vec<String> {
        match commands::kubeconfig() {
            Some(kc) => vec!["--kubeconfig".into(), kc],
            None => Vec::new(),
        }
    }

    pub async fn step(&mut self, name: &str, status: &str, message: &str) {
        let index = match self.status.steps.iter().position(|s| s.name == name) {
            Some(index) => index,
            None => {
                self.status.steps.push(Step {
                    name: name.into(),
                    ..Default::default()
                });
                self.status.steps.len() - 1
            }
        };
        let timestamp = now();
        let phase = self.status.phase.clone();
        let step = &mut self.status.steps[index];
        step.status = status.into();
        step.message = message.into();
        if status == "running" {
            step.started_at = timestamp.clone();
        } else if status == "succeeded" || status == "failed" {
            step.finished_at = timestamp;
        }
        if status == "running" {
            self.status.phase = "running".into();
        } else {
            self.status.phase = phase;
        }
        self.status.message = message.into();
        self.save().await;
    }

    /// Record the versions the operation actually moves between (Talos and
    /// Kubernetes differ from the Naslos release pair the Job was created with).
    pub async fn set_versions(&mut self, from: &str, to: &str) {
        self.status.from = from.to_string();
        self.status.to = to.to_string();
        self.save().await;
    }

    pub async fn phase(&mut self, phase: &str, message: &str) {
        self.status.phase = phase.into();
        self.status.message = message.into();
        self.save().await;
    }

    pub async fn succeeded(&mut self, message: &str) {
        self.status.phase = "succeeded".into();
        self.status.result = "succeeded".into();
        self.status.message = message.into();
        self.status.finished_at = now();
        self.save().await;
    }

    pub async fn failed(&mut self, error: &str) {
        self.status.phase = "failed".into();
        self.status.result = "failed".into();
        self.status.message = error.into();
        self.status.finished_at = now();
        self.save().await;
    }
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_round_trips_and_the_marker_is_per_operation() {
        let mut writer = Writer::new(
            "naslos",
            "/var/lib/naslos",
            "123-abc",
            crate::upgrade::Operation::Naslos,
            "0.3.0",
            "0.3.1",
        );
        assert_eq!(writer.current_phase(), "pending");
        assert_eq!(writer.attempts(), 0);
        assert_eq!(
            writer.marker_path(),
            std::path::PathBuf::from("/var/lib/naslos/updates/operation-123-abc.json")
        );
        writer.status.phase = "rebooting".into();
        writer.status.attempts = 2;
        let raw = serde_json::to_string(&writer.status).unwrap();
        let back: OperationStatus = serde_json::from_str(&raw).unwrap();
        assert_eq!(back.phase, "rebooting");
        assert_eq!(back.attempts, 2);
        assert_eq!(back.op, "naslos");
        assert_eq!(back.from, "0.3.0");
        assert_eq!(back.to, "0.3.1");
    }
}
