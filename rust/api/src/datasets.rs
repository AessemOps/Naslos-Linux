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
    /// Whether the persisted registry was read and validated successfully. A
    /// missing file is a valid empty registry (fresh install); an unreadable
    /// or invalid file fails closed so consumers (notably the dataset destroy
    /// guard) never treat an unproven registry as "no attachments".
    loaded: bool,
    /// Why the registry failed to load, with recovery guidance.
    load_error: Option<String>,
}

impl Registry {
    pub fn new(path: &str, buddy_host_path: &str) -> Self {
        let mut registry = Self {
            path: path.to_string(),
            buddy_host_path: buddy_host_path.to_string(),
            inner: Mutex::new(BTreeMap::new()),
            // An empty path is an in-memory test registry, not a failed load.
            loaded: path.is_empty(),
            load_error: None,
        };
        if !path.is_empty() {
            if let Err(e) = registry.load() {
                tracing::warn!("could not load managed datasets: {e}");
            }
        }
        registry
    }

    /// Whether the persisted registry was read successfully.
    pub fn loaded(&self) -> bool {
        self.loaded
    }

    pub fn load_error(&self) -> Option<String> {
        self.load_error.clone()
    }

    /// Fail closed unless the persisted registry loaded: writing a fresh list
    /// over an unreadable file would drop every attachment record.
    pub fn require_loaded(&self) -> Result<(), String> {
        if self.loaded {
            Ok(())
        } else {
            Err(self
                .load_error
                .clone()
                .unwrap_or_else(|| format!("dataset registry {} is unavailable", self.path)))
        }
    }

    fn load(&mut self) -> Result<(), String> {
        self.loaded = false;
        self.load_error = None;
        if self.path.is_empty() || !Path::new(&self.path).exists() {
            self.loaded = true;
            return Ok(());
        }
        let data = match std::fs::read(&self.path) {
            Ok(data) => data,
            Err(e) => {
                let message = format!(
                    "dataset registry {} could not be read: {e}; refusing to overwrite it",
                    self.path
                );
                self.load_error = Some(message.clone());
                return Err(message);
            }
        };
        let list: Vec<ManagedDataset> = match serde_json::from_slice(&data) {
            Ok(list) => list,
            Err(e) => {
                let backup = match crate::shares::backup_corrupt_store(&self.path, &data) {
                    Ok(path) => format!(" (backup saved to {})", path.display()),
                    Err(backup_err) => format!(" ({backup_err})"),
                };
                let message = format!(
                    "dataset registry {} is corrupt: {e}{backup}; repair it or replace it with a valid registry before changing datasets",
                    self.path
                );
                self.load_error = Some(message.clone());
                return Err(message);
            }
        };
        // Never trust a record the API did not validate itself: a dropped or
        // forged field must fail the whole load, not silently unblock a
        // destroy. Build a candidate map and publish it only when every record
        // passes.
        let mut candidate = BTreeMap::new();
        for ds in list {
            if let Err(e) = validate(&ds, &self.buddy_host_path) {
                let message = format!(
                    "dataset registry {} contains an invalid record ({:?}): {e}; repair it or replace it with a valid registry before changing datasets",
                    self.path, ds.name
                );
                self.load_error = Some(message.clone());
                return Err(message);
            }
            candidate.insert(ds.name.clone(), ds);
        }
        *self.inner.lock().unwrap() = candidate;
        self.loaded = true;
        Ok(())
    }

    pub fn list(&self) -> Vec<ManagedDataset> {
        self.inner.lock().unwrap().values().cloned().collect()
    }

    pub fn get(&self, name: &str) -> Option<ManagedDataset> {
        self.inner.lock().unwrap().get(name).cloned()
    }

    /// Validate and normalize a record against this registry's Buddy path
    /// without writing it. Attachments are server-owned and cleared.
    pub fn check(&self, ds: &ManagedDataset) -> Result<ManagedDataset, String> {
        let mut ds = ds.clone();
        ds.attached_to.clear();
        ds.host_path = normalize_host_path(&ds.host_path)?;
        validate(&ds, &self.buddy_host_path)?;
        Ok(ds)
    }

    pub fn update_config(&self, ds: ManagedDataset) -> Result<ManagedDataset, String> {
        self.require_loaded()?;
        let ds = self.check(&ds)?;
        let mut inner = self.inner.lock().unwrap();
        let Some(existing) = inner.get(&ds.name) else {
            return Err(format!("dataset {:?} is not registered", ds.name));
        };
        if existing.host_path != ds.host_path && !existing.attached_to.is_empty() {
            return Err(format!(
                "dataset {:?} is attached to {}; detach it before changing its host path",
                ds.name,
                existing.attached_to.join(", ")
            ));
        }
        if existing.shared && !ds.shared && existing.attached_to.len() > 1 {
            return Err(format!(
                "dataset {:?} is shared by {}; detach them before making it private",
                ds.name,
                existing.attached_to.join(", ")
            ));
        }
        let mut candidate = inner.clone();
        let updated = candidate.get_mut(&ds.name).expect("checked above");
        updated.host_path = ds.host_path;
        updated.shared = ds.shared;
        updated.warning_acknowledged = ds.warning_acknowledged;
        let out = updated.clone();
        self.save_locked(&candidate)?;
        *inner = candidate;
        Ok(out)
    }

    pub fn add(&self, ds: ManagedDataset) -> Result<ManagedDataset, String> {
        self.require_loaded()?;
        let mut ds = self.check(&ds)?;
        if ds.created_at.is_none() {
            ds.created_at = Some(Utc::now());
        }
        let mut inner = self.inner.lock().unwrap();
        if inner.contains_key(&ds.name) {
            return Err(format!("dataset {:?} is already registered", ds.name));
        }
        let mut candidate = inner.clone();
        candidate.insert(ds.name.clone(), ds.clone());
        self.save_locked(&candidate)?;
        *inner = candidate;
        Ok(ds)
    }

    /// Reserve a dataset for an app. Private datasets accept one app; shared
    /// datasets accept many. The reservation is persisted before the PV/PVC is
    /// created so two concurrent installs cannot both claim a private dataset.
    pub fn attach(&self, name: &str, app: &str) -> Result<(), String> {
        self.require_loaded()?;
        let mut inner = self.inner.lock().unwrap();
        let Some(existing) = inner.get(name) else {
            return Err(format!("dataset {name:?} is not registered"));
        };
        if existing.attached_to.iter().any(|a| a == app) {
            return Ok(());
        }
        if !existing.shared && !existing.attached_to.is_empty() {
            return Err(format!(
                "dataset {name:?} is private and already attached to {}; register it as shared to attach it again",
                existing.attached_to.join(", ")
            ));
        }
        let mut candidate = inner.clone();
        candidate
            .get_mut(name)
            .expect("checked above")
            .attached_to
            .push(app.to_string());
        self.save_locked(&candidate)?;
        *inner = candidate;
        Ok(())
    }

    pub fn detach(&self, name: &str, app: &str) -> Result<(), String> {
        self.require_loaded()?;
        let mut inner = self.inner.lock().unwrap();
        if !inner.contains_key(name) {
            return Ok(());
        }
        let mut candidate = inner.clone();
        candidate
            .get_mut(name)
            .expect("checked above")
            .attached_to
            .retain(|a| a != app);
        self.save_locked(&candidate)?;
        *inner = candidate;
        Ok(())
    }

    pub fn remove(&self, name: &str) -> Result<(), String> {
        self.require_loaded()?;
        let mut inner = self.inner.lock().unwrap();
        if let Some(ds) = inner.get(name) {
            if !ds.attached_to.is_empty() {
                return Err(format!(
                    "dataset {name:?} is attached to {}; detach it first",
                    ds.attached_to.join(", ")
                ));
            }
        }
        let mut candidate = inner.clone();
        if candidate.remove(name).is_none() {
            return Err(format!("dataset {name:?} is not registered"));
        }
        self.save_locked(&candidate)?;
        *inner = candidate;
        Ok(())
    }

    fn save_locked(&self, inner: &BTreeMap<String, ManagedDataset>) -> Result<(), String> {
        self.require_loaded()?;
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
/// Whether a dataset path is, contains, or sits inside one of the Buddy store
/// paths. Registering a pool root must not expose the encrypted Buddy store
/// just because Buddy is disabled on this install (the data disk may still
/// contain one).
pub fn overlaps_buddy(host: &str, buddy_paths: &[String]) -> bool {
    let host = host.trim_end_matches('/');
    buddy_paths.iter().any(|buddy| {
        let buddy = buddy.trim_end_matches('/');
        !buddy.is_empty()
            && (host == buddy
                || host.starts_with(&format!("{buddy}/"))
                || buddy.starts_with(&format!("{host}/")))
    })
}

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
    fn a_registered_path_cannot_expose_the_buddy_store() {
        let buddy = vec!["/var/mnt/test/naslos-buddy".to_string()];
        assert!(overlaps_buddy("/var/mnt/test/naslos-buddy", &buddy));
        assert!(overlaps_buddy("/var/mnt/test", &buddy));
        assert!(overlaps_buddy("/var/mnt/test/naslos-buddy/chunks", &buddy));
        assert!(!overlaps_buddy("/var/mnt/test/drill", &buddy));
        assert!(!overlaps_buddy("/var/mnt/other", &buddy));
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
    fn update_config_preserves_attachments_and_refuses_a_mount_move() {
        let dir = tempfile::tempdir().unwrap();
        let store = Registry::new(&dir.path().join("d.json").display().to_string(), "");
        store.add(ds("media", "/var/mnt/tank/media")).unwrap();
        store.attach("media", "sonarr").unwrap();

        let mut moved = ds("media", "/var/mnt/tank/other");
        moved.shared = true;
        moved.warning_acknowledged = true;
        assert!(store.update_config(moved).is_err());

        let mut updated = ds("media", "/var/mnt/tank/media");
        updated.shared = true;
        updated.warning_acknowledged = true;
        let saved = store.update_config(updated).unwrap();
        assert!(saved.shared);
        assert!(saved.warning_acknowledged);
        assert_eq!(saved.attached_to, vec!["sonarr"]);
        assert!(store
            .update_config(ds("missing", "/var/mnt/tank/x"))
            .is_err());
    }

    #[test]
    fn a_shared_dataset_cannot_become_private_while_attached_twice() {
        let dir = tempfile::tempdir().unwrap();
        let store = Registry::new(&dir.path().join("d.json").display().to_string(), "");
        let mut shared = ds("library", "/var/mnt/tank/library");
        shared.shared = true;
        shared.warning_acknowledged = true;
        store.add(shared).unwrap();
        store.attach("library", "sonarr").unwrap();
        store.attach("library", "radarr").unwrap();

        let mut private = ds("library", "/var/mnt/tank/library");
        private.warning_acknowledged = false;
        assert!(store.update_config(private).is_err());
        assert!(store.get("library").unwrap().shared);
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
    #[test]
    fn a_corrupt_registry_is_backed_up_and_mutations_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("datasets.json");
        std::fs::write(&path, "{not json").unwrap();
        let store = Registry::new(&path.display().to_string(), "");
        assert!(!store.loaded());
        let error = store.require_loaded().unwrap_err();
        assert!(error.contains("corrupt"), "{error}");
        assert!(error.contains(".corrupt-"), "{error}");
        assert!(store.list().is_empty());

        assert!(store.add(ds("media", "/var/mnt/tank/media")).is_err());
        assert!(store.attach("media", "sonarr").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{not json");

        let backups = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().contains(".corrupt-"))
            .count();
        assert_eq!(backups, 1, "one corrupt backup");
    }

    #[test]
    fn an_invalid_record_fails_the_registry_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("datasets.json");
        // A record outside /var/mnt could never have been registered through
        // the API, so the whole file is untrustworthy.
        let bad = vec![ds("media", "/etc")];
        std::fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();
        let store = Registry::new(&path.display().to_string(), "");
        assert!(!store.loaded());
        let error = store.require_loaded().unwrap_err();
        assert!(error.contains("invalid record"), "{error}");
        assert!(store.list().is_empty(), "no partial load");
        assert!(store.add(ds("media", "/var/mnt/tank/media")).is_err());
    }

    #[test]
    fn a_failed_save_does_not_commit_the_mutation_to_memory() {
        let dir = tempfile::tempdir().unwrap();
        // The parent is a regular file, so the atomic temp-file creation
        // fails after the in-memory candidate was built.
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"not a directory").unwrap();
        let store = Registry::new(&blocker.join("datasets.json").display().to_string(), "");
        let err = store
            .add(ds("media", "/var/mnt/tank/media"))
            .expect_err("the save must fail");
        assert!(!err.is_empty());
        assert!(
            store.list().is_empty(),
            "a failed save must not leave a phantom record"
        );

        // Also seed a real store, then make the save fail and prove the old
        // value survives.
        let store_dir = tempfile::tempdir().unwrap();
        let path = store_dir.path().join("datasets.json");
        let store = Registry::new(&path.display().to_string(), "");
        store.add(ds("media", "/var/mnt/tank/media")).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        let err = store
            .attach("media", "sonarr")
            .expect_err("the save must fail");
        assert!(!err.is_empty());
        assert!(
            store.get("media").unwrap().attached_to.is_empty(),
            "a failed save must roll the attachment back"
        );
    }
}
