//! Day-2 host network settings (FR-NET).
//!
//! Talos remains the single source of truth: this module reads the machine
//! configuration and the live link status, validates an operator change, and
//! turns it into a guarded `talosctl patch machineconfig` sequence. The API
//! persists only the in-flight change record and the previous config snapshot
//! needed to revert.

pub mod changes;
pub mod config;
pub mod inventory;
pub mod patch;
pub mod validate;
pub mod watchdog;

use std::sync::Arc;

use async_trait::async_trait;

use crate::kube::Client as KubeClient;
use crate::talos::TalosClient;

pub use changes::{ChangeRecord, ChangeView, Manager, StartError};

/// The narrow Talos surface the change state machine needs. Implemented by the
/// real CLI adapter and by a fake in tests.
#[async_trait]
pub trait Node: Send + Sync {
    /// The running machine configuration (multi-document YAML).
    async fn machine_config(&self) -> Result<String, String>;
    /// The machine configuration as seen from one address (verification).
    async fn probe_config(&self, endpoint: Option<&str>) -> Result<String, String>;
    /// Names of the editable physical links.
    async fn physical_links(&self) -> Result<Vec<String>, String>;
    /// The node's current primary routable IP, if discoverable.
    async fn primary_ip(&self) -> Option<String>;
    /// Apply a machine-config patch; `endpoint` overrides the configured one.
    async fn patch(&self, endpoint: Option<&str>, patch: &str, mode: &str) -> Result<(), String>;
    /// Apply a full machine configuration (the saved revert snapshot).
    async fn replace(&self, endpoint: Option<&str>, config: &str, mode: &str)
        -> Result<(), String>;
    /// Restart the CNI so it re-detects the interface MTU.
    async fn restart_cilium(&self) -> Result<(), String>;
}

/// The production backend: the Talos CLI adapter plus the kubectl client.
pub struct TalosNode {
    client: Arc<TalosClient>,
    kube: Option<Arc<KubeClient>>,
}

impl TalosNode {
    pub fn new(client: Arc<TalosClient>, kube: Option<Arc<KubeClient>>) -> Self {
        Self { client, kube }
    }
}

#[async_trait]
impl Node for TalosNode {
    async fn machine_config(&self) -> Result<String, String> {
        self.client.machine_config().await
    }

    async fn probe_config(&self, endpoint: Option<&str>) -> Result<String, String> {
        match endpoint {
            Some(endpoint) => self.client.machine_config_at(endpoint).await,
            None => self.client.machine_config().await,
        }
    }

    async fn physical_links(&self) -> Result<Vec<String>, String> {
        let values = self.client.links().await?;
        Ok(inventory::physical_link_names(&values))
    }

    async fn primary_ip(&self) -> Option<String> {
        if let Some(kube) = &self.kube {
            if let Ok(nodes) = kube.list_cluster("nodes").await {
                if let Some(ip) = inventory::node_internal_ip(&nodes) {
                    return Some(ip);
                }
            }
        }
        let values = self.client.addresses().await.ok()?;
        inventory::primary_address(&values)
    }

    async fn patch(&self, endpoint: Option<&str>, patch: &str, mode: &str) -> Result<(), String> {
        match endpoint {
            Some(endpoint) => self.client.patch_config_at(endpoint, patch, mode).await,
            None => self.client.patch_config(patch, mode).await,
        }
    }

    async fn replace(
        &self,
        endpoint: Option<&str>,
        config: &str,
        mode: &str,
    ) -> Result<(), String> {
        match endpoint {
            Some(endpoint) => self.client.replace_config_at(endpoint, config, mode).await,
            None => self.client.replace_config(config, mode).await,
        }
    }

    async fn restart_cilium(&self) -> Result<(), String> {
        let kube = self
            .kube
            .as_ref()
            .ok_or("the Kubernetes client is unavailable")?;
        kube.rollout_restart("kube-system", "daemonset", "cilium")
            .await
    }
}

/// Backend used when no Talos endpoint is configured: every operation reports a
/// clear 503 instead of panicking (`AppState::for_test`, Talos-disabled installs).
pub struct UnavailableNode;

#[async_trait]
impl Node for UnavailableNode {
    async fn machine_config(&self) -> Result<String, String> {
        Err("Talos is not configured on this instance".into())
    }
    async fn probe_config(&self, _endpoint: Option<&str>) -> Result<String, String> {
        Err("Talos is not configured on this instance".into())
    }
    async fn physical_links(&self) -> Result<Vec<String>, String> {
        Err("Talos is not configured on this instance".into())
    }
    async fn primary_ip(&self) -> Option<String> {
        None
    }
    async fn patch(&self, _: Option<&str>, _: &str, _: &str) -> Result<(), String> {
        Err("Talos is not configured on this instance".into())
    }
    async fn replace(&self, _: Option<&str>, _: &str, _: &str) -> Result<(), String> {
        Err("Talos is not configured on this instance".into())
    }
    async fn restart_cilium(&self) -> Result<(), String> {
        Err("Talos is not configured on this instance".into())
    }
}
