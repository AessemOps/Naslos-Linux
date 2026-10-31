//! Registry of host datasets an app may mount (D3/F1 phase 1).
//!
//! The platform no longer lets a chart reach `/var/mnt` blindly: an operator
//! registers a specific ZFS dataset (validated against the agent's mountpoint
//! list) and marks it private or shared. The install path will create a
//! per-app PV/PVC from this record; this module owns the records and their
//! safety rules.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Component, Path};
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagedDataset {
    pub name: String,
    #[serde(rename = "hostPath")]
    pub host_path: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub shared: bool,
    /// The operator acknowledged that a shared dataset is readable by every
    /// app it is attached to (SEC-2026-D3).
    #[serde(
        default,
        rename = "warningAcknowledged",
        skip_serializing_if = "is_false"
    )]
    pub warning_acknowledged: bool,
    #[serde(default, rename = "createdAt")]
    pub created_at: Option<DateTime<Utc>>,
    /// Apps currently attached to this dataset. A private dataset may only have
    /// one; a shared one may have several (SEC-2026-D3).
    #[serde(default, rename = "attachedTo", skip_serializing_if = "Vec::is_empty")]
    pub attached_to: Vec<String>,
}

fn is_false(v: &bool) -> bool {
    !*v
}

/// A dataset registry backed by one JSON file.
pub struct Registry {
    path: String,
    buddy_host_path: String,
    inner: Mutex<BTreeMap<String, ManagedDataset>>,
}

impl Registry {
    pub fn new(path: &str, buddy_host_path: &str) -> Self {
        let mut registry = Self {
            path: path.to_string(),
            buddy_host_path: buddy_host_path.to_string(),
            inner: Mutex::new(BTreeMap::new()),
        };
        if !path.is_empty() {
            if let Err(e) = registry.load() {
                tracing::warn!("could not load managed datasets: {e}");
            }
        }
        registry
    }

    fn load(&mut self) -> Result<(), String> {
        if self.path.is_empty() || !Path::new(&self.path).exists() {
            return Ok(());
        }
        let data = std::fs::read(&self.path).map_err(|e| e.to_string())?;
        let list: Vec<ManagedDataset> =
            serde_json::from_slice(&data).map_err(|e| format!("parsing {}: {e}", self.path))?;
        let mut inner = self.inner.lock().unwrap();
        for ds in list {
            inner.insert(ds.name.clone(), ds);
        }
        Ok(())
    }

    pub fn list(&self) -> Vec<ManagedDataset> {
        self.inner.lock().unwrap().values().cloned().collect()
    }

    pub fn get(&self, name: &str) -> Option<ManagedDataset> {
        self.inner.lock().unwrap().get(name).cloned()
    }

    pub fn add(&self, mut ds: ManagedDataset) -> Result<ManagedDataset, String> {
        // Attachments are server-owned; a client must not be able to spoof them.
        ds.attached_to.clear();
        ds.host_path = normalize_host_path(&ds.host_path)?;
        validate(&ds, &self.buddy_host_path)?;
        if ds.created_at.is_none() {
            ds.created_at = Some(Utc::now());
        }
        let mut inner = self.inner.lock().unwrap();
        if inner.contains_key(&ds.name) {
            return Err(format!("dataset {:?} is already registered", ds.name));
        }
        inner.insert(ds.name.clone(), ds.clone());
        self.save_locked(&inner)?;
        Ok(ds)
    }

    /// Reserve a dataset for an app. Private datasets accept one app; shared
    /// datasets accept many. The reservation is persisted before the PV/PVC is
    /// created so two concurrent installs cannot both claim a private dataset.
    pub fn attach(&self, name: &str, app: &str) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap();
        let Some(ds) = inner.get_mut(name) else {
            return Err(format!("dataset {name:?} is not registered"));
        };
        if ds.attached_to.iter().any(|a| a == app) {
            return Ok(());
        }
        if !ds.shared && !ds.attached_to.is_empty() {
            return Err(format!(
                "dataset {name:?} is private and already attached to {}; register it as shared to attach it again",
                ds.attached_to.join(", ")
            ));
        }
        ds.attached_to.push(app.to_string());
        self.save_locked(&inner)
    }

    pub fn detach(&self, name: &str, app: &str) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap();
        let Some(ds) = inner.get_mut(name) else {
            return Ok(());
        };
        ds.attached_to.retain(|a| a != app);
        self.save_locked(&inner)
    }

    pub fn remove(&self, name: &str) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap();
        if let Some(ds) = inner.get(name) {
            if !ds.attached_to.is_empty() {
                return Err(format!(
                    "dataset {name:?} is attached to {}; detach it first",
                    ds.attached_to.join(", ")
                ));
            }
        }
        if inner.remove(name).is_none() {
            return Err(format!("dataset {name:?} is not registered"));
        }
        self.save_locked(&inner)
    }

    fn save_locked(&self, inner: &BTreeMap<String, ManagedDataset>) -> Result<(), String> {
        if self.path.is_empty() {
            return Ok(());
        }
        let list: Vec<&ManagedDataset> = inner.values().collect();
        let json = serde_json::to_vec_pretty(&list).map_err(|e| e.to_string())?;
        crate::chartsrepo::write_atomic(&self.path, &json, 0o600, 0o750)
    }
}

/// Validate a registry record. `buddy_host_path` is the host path of the Buddy
/// receive store: no dataset may contain it or sit inside it.
pub fn validate(ds: &ManagedDataset, buddy_host_path: &str) -> Result<(), String> {
    if !crate::chartsrepo::is_dns1123_label(&ds.name) {
        return Err(format!(
            "dataset name {:?} must be a lowercase DNS-1123 label",
            ds.name
        ));
    }
    let host = normalize_host_path(&ds.host_path)?;
    if !host.starts_with("/var/mnt/") {
        return Err(format!(
            "host path {host:?} must be below /var/mnt (Naslos datasets root)"
        ));
    }
    if host == "/var/mnt" {
        return Err("host path must be a dataset, not the datasets root".to_string());
    }
    if !buddy_host_path.is_empty() {
        let buddy = buddy_host_path.trim_end_matches('/');
        if host == buddy
            || host.starts_with(&format!("{buddy}/"))
            || buddy.starts_with(&format!("{host}/"))
        {
            return Err(format!(
                "host path {host:?} overlaps the Buddy receive store at {buddy:?}"
            ));
        }
    }
    if ds.shared && !ds.warning_acknowledged {
        return Err(
            "a shared dataset must carry warningAcknowledged=true after the security warning"
                .to_string(),
        );
    }
    Ok(())
}

/// Reject `..`, `.` and non-normal components and return a clean absolute path
/// without a trailing slash.
pub fn normalize_host_path(raw: &str) -> Result<String, String> {
    if raw.split('/').any(|part| part == "." || part == "..") {
        return Err(format!(
            "host path {raw:?} must not contain '.' or '..' components"
        ));
    }
    let path = Path::new(raw);
    if !path.is_absolute() {
        return Err(format!("host path {raw:?} must be absolute"));
    }
    let mut clean = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir => clean.push("/"),
            Component::Normal(part) => clean.push(part),
            _ => {
                return Err(format!(
                    "host path {raw:?} must not contain '.', '..' or a prefix"
                ))
            }
        }
    }
    let out = clean.to_string_lossy().to_string();
    Ok(out.trim_end_matches('/').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ds(name: &str, host: &str) -> ManagedDataset {
        ManagedDataset {
            name: name.into(),
            host_path: host.into(),
            shared: false,
            warning_acknowledged: false,
            created_at: None,
            attached_to: Vec::new(),
        }
    }

    #[test]
    fn only_dataset_paths_below_var_mnt_are_accepted() {
        assert!(validate(&ds("media", "/var/mnt/tank/media"), "").is_ok());
        for bad in [
            "/var/mnt",
            "/var/lib/naslos",
            "/etc",
            "/var/mnt/../etc",
            "relative/path",
            "/var/mnt/tank/./media",
        ] {
            assert!(validate(&ds("media", bad), "").is_err(), "{bad}");
        }
    }

    #[test]
    fn the_buddy_store_cannot_be_exposed() {
        let buddy = "/var/mnt/test/naslos-buddy";
        assert!(validate(&ds("buddy", buddy), buddy).is_err());
        assert!(validate(&ds("parent", "/var/mnt/test"), buddy).is_err());
        assert!(validate(&ds("inside", "/var/mnt/test/naslos-buddy/x"), buddy).is_err());
        assert!(validate(&ds("media", "/var/mnt/test/media"), buddy).is_ok());
    }

    #[test]
    fn shared_requires_acknowledgement() {
        let mut record = ds("media", "/var/mnt/tank/media");
        record.shared = true;
        assert!(validate(&record, "").is_err());
        record.warning_acknowledged = true;
        assert!(validate(&record, "").is_ok());
    }

    #[test]
    fn a_private_dataset_accepts_one_app_and_a_shared_one_many() {
        let dir = tempfile::tempdir().unwrap();
        let store = Registry::new(&dir.path().join("d.json").display().to_string(), "");
        store.add(ds("media", "/var/mnt/tank/media")).unwrap();
        store.attach("media", "sonarr").unwrap();
        assert!(store.attach("media", "radarr").is_err());
        store.detach("media", "sonarr").unwrap();
        store.attach("media", "radarr").unwrap();

        let mut shared = ds("library", "/var/mnt/tank/library");
        shared.shared = true;
        shared.warning_acknowledged = true;
        store.add(shared).unwrap();
        store.attach("library", "sonarr").unwrap();
        store.attach("library", "radarr").unwrap();
        assert_eq!(store.get("library").unwrap().attached_to.len(), 2);
        assert!(store.remove("library").is_err());
    }

    #[test]
    fn the_registry_round_trips_and_rejects_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("datasets.json");
        let store = Registry::new(&path.display().to_string(), "");
        store.add(ds("media", "/var/mnt/tank/media")).unwrap();
        assert_eq!(store.list().len(), 1);
        let mut spoof = ds("spoof", "/var/mnt/tank/spoof");
        spoof.attached_to = vec!["attacker".into()];
        store.add(spoof).unwrap();
        assert!(store.get("spoof").unwrap().attached_to.is_empty());
        store.remove("spoof").unwrap();
        assert!(store.add(ds("media", "/var/mnt/tank/other")).is_err());
        let reloaded = Registry::new(&path.display().to_string(), "");
        assert_eq!(
            reloaded.get("media").unwrap().host_path,
            "/var/mnt/tank/media"
        );
        reloaded.remove("media").unwrap();
        assert!(reloaded.list().is_empty());
    }
}
