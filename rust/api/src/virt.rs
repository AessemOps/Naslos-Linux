//! The VM records and the desired-state renderer (FR-VMT).
//!
//! The API is the brain (the shares pattern): it validates every record,
//! persists `vms.json` on the state PVC, renders the whole desired config and
//! pushes it to vmd, which reconciles QEMU processes against it. The model and
//! validation are shared with vmd via the `naslos-vmd` crate, so both sides
//! enforce exactly the same rules (SEC-1: no unvalidated string ever reaches a
//! QEMU command line).

use naslos_vmd::model::{DesiredConfig, DesiredState, VmSpec};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::sync::Mutex;

/// The events the VM watcher can raise (notifications.rs picks them up by
/// name; the owner enables them on the Notifications page).
pub const EVENT_VM_FAILED: &str = "vm_failed";
pub const EVENT_VM_EXITED: &str = "vm_exited_unexpected";

/// A VM record merged with its live state, as served by `/api/virt/vms`.
#[derive(Debug, Clone, Serialize)]
pub struct VmView {
    #[serde(flatten)]
    pub spec: VmSpec,
    /// Live fields (never persisted): "running", "stopped" or "unknown"
    /// (vmd unreachable).
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(rename = "cpuPercent", skip_serializing_if = "Option::is_none")]
    pub cpu_percent: Option<f64>,
    #[serde(rename = "memUsedMiB", skip_serializing_if = "Option::is_none")]
    pub mem_used_mib: Option<u32>,
    // The console credentials (vncPort + the per-boot VNC password) never
    // appear in list or detail payloads: only the /api/ws/vm/{name}/vnc
    // preflight returns them, and the page fetches it on demand.
    #[serde(rename = "unexpectedExit", skip_serializing_if = "std::ops::Not::not")]
    pub unexpected_exit: bool,
    #[serde(rename = "lastError", skip_serializing_if = "String::is_empty")]
    pub last_error: String,
}

/// The VM records, persisted to `VMS_CONFIG` (vms.json on the state PVC).
pub struct Store {
    path: String,
    inner: Mutex<Vec<VmSpec>>,
}

impl Store {
    pub fn new(path: &str) -> Self {
        let store = Self {
            path: path.to_string(),
            inner: Mutex::new(Vec::new()),
        };
        if !path.is_empty() {
            if let Err(e) = store.load() {
                tracing::warn!("could not load the VM records: {e}");
            }
        }
        store
    }

    pub fn from_env() -> Self {
        Self::new(
            &std::env::var("VMS_CONFIG").unwrap_or_else(|_| "/var/lib/naslos/vms.json".to_string()),
        )
    }

    fn load(&self) -> Result<(), String> {
        if self.path.is_empty() || !std::path::Path::new(&self.path).exists() {
            return Ok(());
        }
        let data = std::fs::read(&self.path).map_err(|e| e.to_string())?;
        let list: Vec<VmSpec> =
            serde_json::from_slice(&data).map_err(|e| format!("parsing {}: {e}", self.path))?;
        let mut inner = self.inner.lock().unwrap();
        inner.clear();
        for spec in list {
            // Never trust the on-disk file: re-validate everything
            // (SEC-2026-B12 pattern).
            naslos_vmd::model::validate_vm(&spec)?;
            inner.push(spec);
        }
        Ok(())
    }

    fn save_locked(&self, inner: &[VmSpec]) -> Result<(), String> {
        if self.path.is_empty() {
            return Ok(());
        }
        let json = serde_json::to_vec_pretty(inner).map_err(|e| e.to_string())?;
        crate::chartsrepo::write_atomic(&self.path, &json, 0o600, 0o750)
    }

    pub fn list(&self) -> Vec<VmSpec> {
        self.inner.lock().unwrap().clone()
    }

    pub fn get(&self, name: &str) -> Option<VmSpec> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .find(|vm| vm.name == name)
            .cloned()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.inner.lock().unwrap().iter().any(|vm| vm.name == name)
    }

    /// Insert a new record (validated, unique).
    pub fn create(&self, spec: VmSpec) -> Result<VmSpec, String> {
        naslos_vmd::model::validate_vm(&spec)?;
        {
            let mut inner = self.inner.lock().unwrap();
            if inner.iter().any(|vm| vm.name == spec.name) {
                return Err(format!("a VM named {:?} already exists", spec.name));
            }
            // A brand-new VM starts in the desired state its create form
            // chose; the field is validated like everything else.
            inner.push(spec.clone());
            self.save_locked(&inner)?;
        }
        Ok(spec)
    }

    /// Replace one record's definition (the name is immutable).
    pub fn update(&self, name: &str, spec: VmSpec) -> Result<VmSpec, String> {
        if spec.name != name {
            return Err("the VM name cannot be changed".to_string());
        }
        naslos_vmd::model::validate_vm(&spec)?;
        let mut inner = self.inner.lock().unwrap();
        let Some(existing) = inner.iter_mut().find(|vm| vm.name == name) else {
            return Err(format!("VM {name:?} does not exist"));
        };
        *existing = spec.clone();
        self.save_locked(&inner)?;
        Ok(spec)
    }

    /// Remove one record (without touching the dataset; the caller does that).
    pub fn remove(&self, name: &str) -> Result<VmSpec, String> {
        let mut inner = self.inner.lock().unwrap();
        let index = inner
            .iter()
            .position(|vm| vm.name == name)
            .ok_or_else(|| format!("VM {name:?} does not exist"))?;
        let removed = inner.remove(index);
        self.save_locked(&inner)?;
        Ok(removed)
    }

    /// Set one VM's desired runtime state (start/stop actions).
    pub fn set_desired(&self, name: &str, desired: DesiredState) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap();
        let Some(vm) = inner.iter_mut().find(|vm| vm.name == name) else {
            return Err(format!("VM {name:?} does not exist"));
        };
        vm.desired = desired;
        self.save_locked(&inner)?;
        Ok(())
    }

    /// Render the desired config to push. `boot` marks autostart VMs as
    /// desired-running (the API startup push: pod restart = VM restart for
    /// autostart-marked VMs, per docs/virtual-machines.md). A plain push
    /// keeps each record's persisted desired state.
    pub fn render_config(&self, boot: bool) -> DesiredConfig {
        let mut vms = self.list();
        if boot {
            for vm in &mut vms {
                if vm.autostart {
                    vm.desired = DesiredState::Running;
                }
            }
        }
        let revision = config_revision(&vms);
        DesiredConfig { revision, vms }
    }
}

/// The revision gates the push: a canonical hash of the config the API is
/// about to hand over, so a stale or interleaved push is detectable.
fn config_revision(vms: &[VmSpec]) -> String {
    let canonical = serde_json::to_vec(vms).unwrap_or_default();
    let digest = Sha256::digest(&canonical);
    format!("{digest:x}")
}

/// Merge the records with vmd's live report. `live` is `None` when vmd is
/// unreachable: the records still list, with state "unknown".
pub fn merge_views(records: &[VmSpec], live: Option<&[crate::vmd::VmLive]>) -> Vec<VmView> {
    let mut views = Vec::new();
    for spec in records {
        let entry = live.and_then(|list| list.iter().find(|l| l.name == spec.name));
        let mut view = VmView {
            spec: spec.clone(),
            state: "unknown".into(),
            pid: None,
            cpu_percent: None,
            mem_used_mib: None,
            unexpected_exit: false,
            last_error: String::new(),
        };
        if let Some(l) = entry {
            view.state = l.state.clone();
            if l.state == "running" {
                view.pid = l.pid;
                view.cpu_percent = Some(l.cpu_percent);
                view.mem_used_mib = Some(l.mem_used_mib);
            }
            view.unexpected_exit = l.unexpected_exit;
            view.last_error = l.last_error.clone();
        }
        views.push(view);
    }
    views
}

/// The dataset of one VM under its pool: `<pool>/vms/<name>`.
pub fn vm_dataset(pool: &str, name: &str) -> String {
    format!("{pool}/vms/{name}")
}

/// The image library dataset of one pool: `<pool>/vms/isos`.
pub fn isos_dataset(pool: &str) -> String {
    format!("{pool}/vms/isos")
}

#[cfg(test)]
mod tests {
    use super::*;
    use naslos_vmd::model::{
        CloudInitSpec, DiskBus, DiskFormat, DiskSpec, Firmware, NetMode, NetSpec, PortForward,
    };

    fn spec(name: &str) -> VmSpec {
        VmSpec {
            name: name.into(),
            cpu: 2,
            mem_mib: 2048,
            autostart: false,
            firmware: Firmware::Uefi,
            desired: DesiredState::Stopped,
            disk: DiskSpec {
                size_gb: 40,
                format: DiskFormat::Qcow2,
                bus: DiskBus::Virtio,
                source_image: None,
            },
            net: NetSpec {
                mode: NetMode::Nat,
                forwards: vec![PortForward {
                    host: 8081,
                    dst: 80,
                }],
                nic: None,
            },
            cloud_init: Some(CloudInitSpec {
                hostname: name.into(),
                user: "admin".into(),
                ssh_keys: vec!["ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExample t@e".into()],
            }),
            iso: Some("isos/debian-13-netinst.iso".into()),
            pool: "tank".into(),
        }
    }

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vms.json");
        let store = Store::new(&path.display().to_string());
        (dir, store)
    }

    #[test]
    fn the_store_round_trips_and_revalidates() {
        let (_dir, store) = store();
        store.create(spec("alpha")).unwrap();
        store.create(spec("beta")).unwrap();
        assert!(store.create(spec("alpha")).is_err());
        assert_eq!(store.list().len(), 2);

        // A name that would not validate never lands in the file.
        let mut bad = spec("UPPER");
        assert!(store.create(bad.clone()).is_err());
        bad.name = "gamma".into();
        assert!(store.create(bad).is_ok());
    }

    #[test]
    fn updates_keep_the_name_immutable() {
        let (_dir, store) = store();
        store.create(spec("alpha")).unwrap();
        let mut renamed = spec("renamed");
        assert!(store.update("alpha", renamed.clone()).is_err());
        renamed.name = "alpha".into();
        renamed.cpu = 4;
        let updated = store.update("alpha", renamed).unwrap();
        assert_eq!(updated.cpu, 4);
        assert_eq!(store.get("alpha").unwrap().cpu, 4);
    }

    #[test]
    fn boot_render_marks_autostart_vms_running() {
        let (_dir, store) = store();
        let mut auto = spec("auto");
        auto.autostart = true;
        store.create(auto).unwrap();
        store.create(spec("manual")).unwrap();

        let plain = store.render_config(false);
        assert_eq!(
            plain.vms.iter().find(|v| v.name == "auto").unwrap().desired,
            DesiredState::Stopped
        );
        let boot = store.render_config(true);
        assert_eq!(
            boot.vms.iter().find(|v| v.name == "auto").unwrap().desired,
            DesiredState::Running
        );
        assert_eq!(
            boot.vms
                .iter()
                .find(|v| v.name == "manual")
                .unwrap()
                .desired,
            DesiredState::Stopped
        );
    }

    #[test]
    fn revisions_change_with_the_config() {
        let (_dir, store) = store();
        store.create(spec("alpha")).unwrap();
        let first = store.render_config(false);
        let unchanged = store.render_config(false);
        assert_eq!(first.revision, unchanged.revision);
        let mut edited = spec("alpha");
        edited.cpu = 8;
        store.update("alpha", edited).unwrap();
        assert_ne!(first.revision, store.render_config(false).revision);
    }

    #[test]
    fn views_merge_live_state_and_survive_a_dead_vmd() {
        let records = vec![spec("alpha")];
        let views = merge_views(&records, None);
        assert_eq!(views[0].state, "unknown");

        let live = vec![crate::vmd::VmLive {
            name: "alpha".into(),
            state: "running".into(),
            pid: Some(42),
            vnc_port: Some(5901),
            vnc_password: "pw123456".into(),
            cpu_percent: 12.5,
            mem_used_mib: 1024,
            ..Default::default()
        }];
        let views = merge_views(&records, Some(&live));
        assert_eq!(views[0].state, "running");
        assert_eq!(views[0].cpu_percent, Some(12.5));
        // The console credentials stay out of list/detail payloads; the
        // serialized view must not carry them at all.
        let encoded = serde_json::to_string(&views[0]).unwrap();
        assert!(!encoded.contains("vncPassword"));
        assert!(!encoded.contains("vncPort"));
    }

    #[test]
    fn dataset_names_are_scoped_under_the_pool() {
        assert_eq!(vm_dataset("tank", "alpha"), "tank/vms/alpha");
        assert_eq!(isos_dataset("tank"), "tank/vms/isos");
    }
}
