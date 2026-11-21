//! The host-network change state machine (FR-NET-02/03).
//!
//! A change is applied with `talosctl patch machineconfig --mode=try`, checked
//! at the target address, confirmed with a real certSANs update, and kept in an
//! `awaiting` window for five minutes. The record (including the previous full
//! machine config) is persisted so an API restart either continues or reverts
//! instead of blocking every future change behind a dead `pending` entry.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};

use super::config::MachineConfig;
use super::patch::{self, VerifyNeed};
use super::validate::{validate, ChangeRequest};
use super::Node;

/// How long the apply/verify phase may take before the change is abandoned.
/// Talos's own `--mode=try` revert window is 60 s, so verification must finish
/// inside it with room to spare.
const VERIFY_WINDOW: i64 = 45;
/// How long the record stays `awaiting` after confirmation (operator revert
/// window). After this the change is `applied`.
const AWAIT_WINDOW: i64 = 300;
const MODE_LIVE: &str = "no-reboot";
const MODE_TRY: &str = "try";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Applying,
    Verifying,
    Awaiting,
    Applied,
    Reverted,
    Failed,
}

impl Phase {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Applied | Self::Reverted | Self::Failed)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Applying => "applying",
            Self::Verifying => "verifying",
            Self::Awaiting => "awaiting",
            Self::Applied => "applied",
            Self::Reverted => "reverted",
            Self::Failed => "failed",
        }
    }
}

/// The persisted change record.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeRecord {
    pub id: String,
    pub phase: Phase,
    pub link: String,
    /// The address to verify the change at (the new static address).
    pub target: Option<String>,
    pub old_primary_ip: Option<String>,
    pub new_primary_ip: Option<String>,
    pub pre_patch: String,
    pub patch: String,
    pub confirm_patch: String,
    pub verify: VerifyNeed,
    pub needs_cilium_restart: bool,
    pub previous_config: String,
    pub previous_cert_sans: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deadline: DateTime<Utc>,
    pub error: Option<String>,
}

/// What the UI sees for `GET /api/network/changes/{id}`.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeView {
    pub id: String,
    pub phase: &'static str,
    pub link: String,
    pub target: Option<String>,
    pub deadline: DateTime<Utc>,
    pub error: Option<String>,
}

impl ChangeRecord {
    pub fn view(&self) -> ChangeView {
        ChangeView {
            id: self.id.clone(),
            phase: self.phase.as_str(),
            link: self.link.clone(),
            target: self.target.clone(),
            deadline: self.deadline,
            error: self.error.clone(),
        }
    }
}

/// A start error mapped to an HTTP status by the handler.
#[derive(Debug, Clone)]
pub struct StartError {
    pub status: u16,
    pub message: String,
}

impl StartError {
    fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: 409,
            message: message.into(),
        }
    }
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            status: 400,
            message: message.into(),
        }
    }
    fn unavailable(message: impl Into<String>) -> Self {
        Self {
            status: 503,
            message: message.into(),
        }
    }
    fn backend(message: impl Into<String>) -> Self {
        Self {
            status: 502,
            message: message.into(),
        }
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: 404,
            message: message.into(),
        }
    }
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Stored {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending: Option<ChangeRecord>,
}

/// Owns the pending change record and advances it.
pub struct Manager {
    path: String,
    state: Mutex<Option<ChangeRecord>>,
    node: std::sync::Arc<dyn Node>,
    /// Serializes the watchdog and an operator revert so a revert always wins
    /// over an in-flight confirmation.
    running: tokio::sync::Mutex<()>,
    cancel: AtomicBool,
}

impl Manager {
    /// Build from a state path and a node backend; loads any saved record.
    pub fn new(path: &str, node: std::sync::Arc<dyn Node>) -> std::sync::Arc<Self> {
        let manager = Self {
            path: path.to_string(),
            state: Mutex::new(None),
            node,
            running: tokio::sync::Mutex::new(()),
            cancel: AtomicBool::new(false),
        };
        manager.load();
        std::sync::Arc::new(manager)
    }

    /// Build from the process environment and start the watchdog.
    pub fn from_env(
        talos: Option<std::sync::Arc<crate::talos::TalosClient>>,
        kube: Option<std::sync::Arc<crate::kube::Client>>,
    ) -> std::sync::Arc<Self> {
        let path = std::env::var("NETWORK_STATE")
            .unwrap_or_else(|_| "/var/lib/naslos/network.json".to_string());
        let node: std::sync::Arc<dyn Node> = match talos {
            Some(client) => std::sync::Arc::new(super::TalosNode::new(client, kube)),
            None => std::sync::Arc::new(super::UnavailableNode),
        };
        let manager = Self::new(&path, node);
        super::watchdog::spawn(manager.clone());
        manager
    }

    fn load(&self) {
        if self.path.is_empty() {
            return;
        }
        let Ok(data) = std::fs::read(&self.path) else {
            return;
        };
        match serde_json::from_slice::<Stored>(&data) {
            Ok(stored) => *self.state.lock().unwrap() = stored.pending,
            Err(e) => tracing::warn!("could not parse the network change store: {e}"),
        }
    }

    fn persist(&self, record: &Option<ChangeRecord>) {
        if self.path.is_empty() {
            return;
        }
        let stored = Stored {
            pending: record.clone(),
        };
        let data = match serde_json::to_vec_pretty(&stored) {
            Ok(data) => data,
            Err(e) => {
                tracing::warn!("could not encode the network change store: {e}");
                return;
            }
        };
        if let Err(e) = crate::chartsrepo::write_atomic(&self.path, &data, 0o600, 0o750) {
            tracing::warn!("could not save the network change store: {e}");
        }
    }

    fn snapshot(&self) -> Option<ChangeRecord> {
        self.state.lock().unwrap().clone()
    }

    fn pending(&self) -> Option<ChangeRecord> {
        self.snapshot().filter(|record| !record.phase.is_terminal())
    }

    /// The pending change (non-terminal), for `GET /api/network`.
    pub fn pending_view(&self) -> Option<ChangeView> {
        self.pending().map(|record| record.view())
    }

    /// The record with `id`, terminal or not.
    pub fn detail(&self, id: &str) -> Option<ChangeView> {
        self.snapshot()
            .filter(|record| record.id == id)
            .map(|record| record.view())
    }

    /// Validate and start a change. Returns once the record is persisted; the
    /// watchdog performs the actual apply.
    pub async fn start(&self, request: ChangeRequest) -> Result<ChangeRecord, StartError> {
        if self.pending().is_some() {
            return Err(StartError::conflict(
                "a network change is already pending; wait for it to finish or revert it",
            ));
        }
        let raw = self
            .node
            .machine_config()
            .await
            .map_err(|e| StartError::unavailable(format!("reading the machine config: {e}")))?;
        let cfg = MachineConfig::parse(&raw).map_err(StartError::backend)?;
        let physical = self
            .node
            .physical_links()
            .await
            .map_err(|e| StartError::unavailable(format!("listing the network links: {e}")))?;
        let primary = self.node.primary_ip().await;
        let change =
            validate(&request, &physical, &cfg.valid_subnets).map_err(StartError::invalid)?;
        let built = patch::build(&change, &cfg, primary.as_deref()).map_err(StartError::invalid)?;

        let now = Utc::now();
        let record = ChangeRecord {
            id: new_id(),
            phase: Phase::Applying,
            link: change.link.clone(),
            target: built.target_ip.clone(),
            old_primary_ip: built.old_primary_ip.clone(),
            new_primary_ip: built.target_ip.clone(),
            pre_patch: built.pre_patch,
            patch: built.patch,
            confirm_patch: built.confirm_patch,
            verify: VerifyNeed::from_change(&change),
            needs_cilium_restart: built.needs_cilium_restart,
            previous_config: cfg.raw.clone(),
            previous_cert_sans: cfg.cert_sans.clone(),
            created_at: now,
            updated_at: now,
            deadline: now + chrono::Duration::seconds(VERIFY_WINDOW),
            error: None,
        };
        {
            let mut guard = self.state.lock().unwrap();
            // A concurrent start may have won after the check above; the
            // winner stays authoritative and the loser gets a 409.
            if guard
                .as_ref()
                .is_some_and(|current| !current.phase.is_terminal())
            {
                return Err(StartError::conflict(
                    "a network change is already pending; wait for it to finish or revert it",
                ));
            }
            *guard = Some(record.clone());
            self.persist(&guard);
        }
        Ok(record)
    }

    /// Apply a saved previous config and mark the change reverted.
    pub async fn revert(&self, id: &str) -> Result<ChangeRecord, StartError> {
        let record = self
            .snapshot()
            .ok_or_else(|| StartError::not_found("no network change"))?;
        if record.id != id {
            return Err(StartError::not_found("unknown network change"));
        }
        if record.phase.is_terminal() {
            return Err(StartError::conflict(
                "the network change has already finished",
            ));
        }
        // Ask an in-flight verification to stop before it confirms.
        self.cancel.store(true, Ordering::SeqCst);
        let _guard = self.running.lock().await;
        self.cancel.store(false, Ordering::SeqCst);
        let record = self
            .snapshot()
            .ok_or_else(|| StartError::not_found("no network change"))?;
        if record.id != id {
            return Err(StartError::not_found("unknown network change"));
        }
        if record.phase.is_terminal() {
            return Ok(record);
        }
        self.revert_record(&record, "reverted by the operator")
            .await
            .map_err(StartError::backend)
    }

    /// Advance the pending record one step. Called by the watchdog loop and by
    /// unit tests; there is no timer inside this module.
    pub async fn tick(&self) {
        let _guard = self.running.lock().await;
        let Some(record) = self.pending() else {
            return;
        };
        let now = Utc::now();
        match record.phase {
            Phase::Applying => self.apply(&record, now).await,
            Phase::Verifying => self.verify(&record, now).await,
            Phase::Awaiting if now >= record.deadline => {
                self.transition(&record.id, Phase::Applied, now, None).await;
            }
            _ => {}
        }
    }

    /// The endpoint the node currently answers on: the Kubernetes InternalIP
    /// when it is discoverable (it follows an address change), otherwise the
    /// configured Talos endpoint. Bounded because kubectl has a 60 s timeout.
    async fn current_endpoint(&self) -> Option<String> {
        tokio::time::timeout(Duration::from_secs(5), self.node.primary_ip())
            .await
            .ok()
            .flatten()
    }

    async fn apply(&self, record: &ChangeRecord, now: DateTime<Utc>) {
        if now >= record.deadline {
            // The API restarted before the apply could finish. The certSANs
            // pre-patch may already have landed, so restore the snapshot
            // rather than leaving it behind.
            self.abort(
                record,
                "the change could not be started in time".to_string(),
            )
            .await;
            return;
        }
        let endpoint = self.current_endpoint().await;
        if let Err(e) = self
            .node
            .patch(endpoint.as_deref(), &record.pre_patch, MODE_LIVE)
            .await
        {
            self.abort(record, format!("applying the certificate update: {e}"))
                .await;
            return;
        }
        if let Err(e) = self
            .node
            .patch(endpoint.as_deref(), &record.patch, MODE_TRY)
            .await
        {
            self.abort(record, format!("applying the network change: {e}"))
                .await;
            return;
        }
        self.transition(
            &record.id,
            Phase::Verifying,
            now + chrono::Duration::seconds(VERIFY_WINDOW),
            None,
        )
        .await;
    }

    async fn verify(&self, record: &ChangeRecord, now: DateTime<Utc>) {
        if self.cancel.load(Ordering::SeqCst) {
            return;
        }
        if now >= record.deadline {
            self.revert_record(
                record,
                "the new configuration did not answer within the guard window",
            )
            .await
            .ok();
            return;
        }
        // A DHCP change has no predicted target: ask Kubernetes for the node's
        // current InternalIP (it follows the new lease) before falling back to
        // the configured endpoint.
        let mut endpoints = self.probe_endpoints(record);
        if record.target.is_none() {
            if let Some(ip) = self.current_endpoint().await {
                if !endpoints.iter().flatten().any(|endpoint| endpoint == &ip) {
                    endpoints.insert(0, Some(ip));
                }
            }
        }
        let mut verified_at: Option<Option<String>> = None;
        for endpoint in endpoints {
            if self.cancel.load(Ordering::SeqCst) {
                return;
            }
            if let Ok(raw) = self.node.probe_config(endpoint.as_deref()).await {
                if let Ok(cfg) = MachineConfig::parse(&raw) {
                    if record.verify.matches(&cfg) {
                        verified_at = Some(endpoint);
                        break;
                    }
                }
            }
            // A slow probe must not overshoot the try-mode window silently.
            if Utc::now() >= record.deadline {
                break;
            }
        }
        let Some(endpoint) = verified_at else {
            return;
        };
        if let Err(e) = self
            .node
            .patch(endpoint.as_deref(), &record.confirm_patch, MODE_LIVE)
            .await
        {
            self.abort(record, format!("confirming the change: {e}"))
                .await;
            return;
        }
        if record.needs_cilium_restart {
            if let Err(e) = self.node.restart_cilium().await {
                self.set_error(&record.id, format!("restarting Cilium: {e}"))
                    .await;
            }
        }
        self.transition(
            &record.id,
            Phase::Awaiting,
            Utc::now() + chrono::Duration::seconds(AWAIT_WINDOW),
            None,
        )
        .await;
    }

    /// Endpoints to probe, preferred first: the new address, then the
    /// configured Talos endpoint(s).
    fn probe_endpoints(&self, record: &ChangeRecord) -> Vec<Option<String>> {
        match &record.target {
            Some(target) => vec![Some(target.clone()), None],
            None => vec![None],
        }
    }

    /// Revert/abort endpoints: the current InternalIP first (the configured
    /// endpoint may be stale after an earlier address change), then the target,
    /// then the configured endpoint.
    async fn restore_endpoints(&self, record: &ChangeRecord) -> Vec<Option<String>> {
        let mut out = Vec::new();
        if let Some(ip) = self.current_endpoint().await {
            out.push(Some(ip));
        }
        for endpoint in self.probe_endpoints(record) {
            if !out.contains(&endpoint) {
                out.push(endpoint);
            }
        }
        out
    }

    async fn revert_record(
        &self,
        record: &ChangeRecord,
        reason: &str,
    ) -> Result<ChangeRecord, String> {
        let mut last_error = String::new();
        for endpoint in self.restore_endpoints(record).await {
            match self
                .node
                .replace(endpoint.as_deref(), &record.previous_config, MODE_LIVE)
                .await
            {
                Ok(()) => {
                    return Ok(self
                        .transition(
                            &record.id,
                            Phase::Reverted,
                            Utc::now(),
                            Some(reason.to_string()),
                        )
                        .await)
                }
                Err(e) => last_error = e,
            }
        }
        self.transition(
            &record.id,
            Phase::Failed,
            Utc::now(),
            Some(format!("{reason}; reverting failed: {last_error}")),
        )
        .await;
        Err(last_error)
    }

    /// Mark a change failed and best-effort restore the previous config.
    async fn abort(&self, record: &ChangeRecord, message: String) {
        for endpoint in self.restore_endpoints(record).await {
            if self
                .node
                .replace(endpoint.as_deref(), &record.previous_config, MODE_LIVE)
                .await
                .is_ok()
            {
                break;
            }
        }
        self.transition(&record.id, Phase::Failed, Utc::now(), Some(message))
            .await;
    }

    async fn set_error(&self, id: &str, message: String) {
        let mut guard = self.state.lock().unwrap();
        if let Some(record) = guard.as_mut() {
            if record.id == id {
                record.error = Some(message);
                record.updated_at = Utc::now();
            }
        }
        self.persist(&guard);
    }

    async fn transition(
        &self,
        id: &str,
        phase: Phase,
        deadline: DateTime<Utc>,
        error: Option<String>,
    ) -> ChangeRecord {
        let mut guard = self.state.lock().unwrap();
        let updated = if let Some(record) = guard.as_mut() {
            if record.id == id {
                record.phase = phase;
                record.deadline = deadline;
                record.updated_at = Utc::now();
                if phase == Phase::Applied {
                    record.error = None;
                } else if error.is_some() {
                    record.error = error;
                }
                record.clone()
            } else {
                record.clone()
            }
        } else {
            ChangeRecord {
                id: id.to_string(),
                phase,
                link: String::new(),
                target: None,
                old_primary_ip: None,
                new_primary_ip: None,
                pre_patch: String::new(),
                patch: String::new(),
                confirm_patch: String::new(),
                verify: VerifyNeed {
                    link: String::new(),
                    addressing: None,
                    addresses: Vec::new(),
                    mtu: None,
                    nameservers: None,
                    nameservers_inherit: false,
                    search_domains: None,
                    search_inherit: false,
                },
                needs_cilium_restart: false,
                previous_config: String::new(),
                previous_cert_sans: Vec::new(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
                deadline,
                error,
            }
        };
        self.persist(&guard);
        updated
    }
}

/// The watchdog cadence. Kept in one place so a test can document it.
pub const TICK_INTERVAL: Duration = Duration::from_secs(2);

#[cfg(test)]
impl Manager {
    /// Force the recorded deadline so the state machine can be exercised
    /// without waiting out the real guard windows.
    fn force_deadline(&self, id: &str, deadline: DateTime<Utc>) {
        let mut guard = self.state.lock().unwrap();
        if let Some(record) = guard.as_mut() {
            if record.id == id {
                record.deadline = deadline;
            }
        }
        self.persist(&guard);
    }
}

fn new_id() -> String {
    let mut bytes = [0u8; 8];
    if getrandom::fill(&mut bytes).is_ok() {
        return bytes.iter().map(|b| format!("{b:02x}")).collect();
    }
    format!("{:x}", Utc::now().timestamp_nanos_opt().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::Node;
    use async_trait::async_trait;
    use std::sync::atomic::AtomicUsize;
    use std::sync::{Arc, Mutex as StdMutex};

    const DHCP_CONFIG: &str = r#"
apiVersion: v1alpha1
kind: LinkConfig
name: eth0
mtu: 1500
---
apiVersion: v1alpha1
kind: DHCPv4Config
name: eth0
---
apiVersion: v1alpha1
kind: ResolverConfig
hostDNS:
  enabled: true
  forwardKubeDNSToHost: false
---
machine:
  certSANs:
    - 10.0.0.99
"#;

    const STATIC_CONFIG: &str = r#"
apiVersion: v1alpha1
kind: LinkConfig
name: eth0
addresses:
  - address: 10.0.0.50/24
routes:
  - gateway: 10.0.0.1
mtu: 1500
---
apiVersion: v1alpha1
kind: ResolverConfig
hostDNS:
  enabled: true
  forwardKubeDNSToHost: false
---
machine:
  certSANs:
    - 10.0.0.50
"#;

    #[derive(Default)]
    struct FakeNode {
        config: StdMutex<String>,
        try_config: StdMutex<Option<String>>,
        patches: StdMutex<Vec<(Option<String>, String, String)>>,
        replaced: StdMutex<Vec<String>>,
        probes: AtomicUsize,
        fail_probe: AtomicBool,
        cilium_restarts: AtomicUsize,
    }

    impl FakeNode {
        fn new() -> Self {
            Self {
                config: StdMutex::new(DHCP_CONFIG.to_string()),
                ..Default::default()
            }
        }
    }

    #[async_trait]
    impl Node for FakeNode {
        async fn machine_config(&self) -> Result<String, String> {
            Ok(self.config.lock().unwrap().clone())
        }
        async fn probe_config(&self, _endpoint: Option<&str>) -> Result<String, String> {
            self.probes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.fail_probe.load(std::sync::atomic::Ordering::SeqCst) {
                return Err("unreachable".into());
            }
            Ok(self.config.lock().unwrap().clone())
        }
        async fn physical_links(&self) -> Result<Vec<String>, String> {
            Ok(vec!["eth0".to_string()])
        }
        async fn primary_ip(&self) -> Option<String> {
            Some("10.0.0.20".to_string())
        }
        async fn patch(
            &self,
            endpoint: Option<&str>,
            patch: &str,
            mode: &str,
        ) -> Result<(), String> {
            self.patches.lock().unwrap().push((
                endpoint.map(str::to_string),
                patch.to_string(),
                mode.to_string(),
            ));
            if mode == "try" {
                if let Some(next) = self.try_config.lock().unwrap().take() {
                    *self.config.lock().unwrap() = next;
                }
            }
            Ok(())
        }
        async fn replace(
            &self,
            _endpoint: Option<&str>,
            config: &str,
            _mode: &str,
        ) -> Result<(), String> {
            self.replaced.lock().unwrap().push(config.to_string());
            *self.config.lock().unwrap() = config.to_string();
            Ok(())
        }
        async fn restart_cilium(&self) -> Result<(), String> {
            self.cilium_restarts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    fn static_request() -> ChangeRequest {
        ChangeRequest {
            link: "eth0".into(),
            addressing: Some("static".into()),
            addresses: Some(vec!["10.0.0.50/24".into()]),
            gateway: Some("10.0.0.1".into()),
            ..Default::default()
        }
    }

    fn mtu_request() -> ChangeRequest {
        ChangeRequest {
            link: "eth0".into(),
            mtu: Some(1400),
            ..Default::default()
        }
    }

    fn manager(node: Arc<FakeNode>) -> Arc<Manager> {
        Manager::new("", node)
    }

    #[tokio::test]
    async fn a_static_change_applies_verifies_and_confirms() {
        let node = Arc::new(FakeNode::new());
        node.try_config
            .lock()
            .unwrap()
            .replace(STATIC_CONFIG.to_string());
        let manager = manager(node.clone());
        let record = manager.start(static_request()).await.unwrap();
        assert_eq!(record.phase, Phase::Applying);

        manager.tick().await;
        let view = manager.detail(&record.id).unwrap();
        assert_eq!(view.phase, "verifying");
        assert_eq!(node.patches.lock().unwrap().len(), 2);
        assert_eq!(node.patches.lock().unwrap()[1].2, "try");

        manager.tick().await;
        let view = manager.detail(&record.id).unwrap();
        assert_eq!(view.phase, "awaiting");
        assert_eq!(node.patches.lock().unwrap().len(), 3);
        assert_eq!(node.patches.lock().unwrap()[2].2, "no-reboot");

        manager.force_deadline(&record.id, Utc::now() - chrono::Duration::seconds(1));
        manager.tick().await;
        assert_eq!(manager.detail(&record.id).unwrap().phase, "applied");
    }

    #[tokio::test]
    async fn an_unreachable_target_reverts_at_the_deadline() {
        let node = Arc::new(FakeNode::new());
        node.try_config
            .lock()
            .unwrap()
            .replace(STATIC_CONFIG.to_string());
        node.fail_probe
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let manager = manager(node.clone());
        let record = manager.start(static_request()).await.unwrap();
        manager.tick().await;
        manager.force_deadline(&record.id, Utc::now() - chrono::Duration::seconds(1));
        manager.tick().await;
        assert_eq!(manager.detail(&record.id).unwrap().phase, "reverted");
        assert_eq!(node.replaced.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_operator_revert_beats_a_confirmation() {
        let node = Arc::new(FakeNode::new());
        node.try_config
            .lock()
            .unwrap()
            .replace(STATIC_CONFIG.to_string());
        let manager = manager(node.clone());
        let record = manager.start(static_request()).await.unwrap();
        manager.tick().await;
        let updated = manager.revert(&record.id).await.unwrap();
        assert_eq!(updated.phase, Phase::Reverted);
        assert_eq!(node.replaced.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_second_change_is_refused_while_one_is_pending() {
        let node = Arc::new(FakeNode::new());
        let manager = manager(node);
        manager.start(static_request()).await.unwrap();
        let error = manager.start(static_request()).await.unwrap_err();
        assert_eq!(error.status, 409);
    }

    #[tokio::test]
    async fn a_pending_record_resumes_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("network.json");
        let path = path.display().to_string();
        let node = Arc::new(FakeNode::new());
        node.try_config
            .lock()
            .unwrap()
            .replace(STATIC_CONFIG.to_string());

        let first = Manager::new(&path, node.clone());
        let record = first.start(static_request()).await.unwrap();
        first.tick().await;
        drop(first);

        let resumed = Manager::new(&path, node.clone());
        assert!(resumed.pending_view().is_some());
        // The guard window passed while the API was down: revert, do not stay
        // stuck behind a dead pending record.
        resumed.force_deadline(&record.id, Utc::now() - chrono::Duration::seconds(1));
        resumed.tick().await;
        assert_eq!(resumed.detail(&record.id).unwrap().phase, "reverted");
        assert!(resumed.pending_view().is_none());
        // A new change is accepted afterwards.
        resumed.start(mtu_request()).await.unwrap();
    }

    #[tokio::test]
    async fn an_mtu_change_restarts_cilium_after_confirmation() {
        let node = Arc::new(FakeNode::new());
        let manager = manager(node.clone());
        let record = manager.start(mtu_request()).await.unwrap();
        manager.tick().await;
        // The fake applies the try patch; make the config reflect the MTU so
        // verification succeeds.
        *node.config.lock().unwrap() =
            DHCP_CONFIG
                .to_string()
                .replacen("mtu: 1500", "mtu: 1400", 1);
        manager.tick().await;
        assert_eq!(manager.detail(&record.id).unwrap().phase, "awaiting");
        assert_eq!(
            node.cilium_restarts
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
    }
}
