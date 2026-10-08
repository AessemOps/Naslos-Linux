//! SMB/NFS share management (port of `api/internal/shares`).
//!
//! The API owns the share definitions and renders `smb.conf`/`ganesha.conf`; the
//! privileged agent writes them onto the Talos host. The renderers interpolate
//! share fields verbatim, so `validate_share_fields` is what stops a field from
//! injecting a new config directive (NAS-007).

pub mod render;

use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::Path;

/// A share protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    #[default]
    Smb,
    Nfs,
    Afp,
}

impl Protocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Protocol::Smb => "smb",
            Protocol::Nfs => "nfs",
            Protocol::Afp => "afp",
        }
    }
}

/// A single share definition (field-for-field with the Go `Share`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Share {
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub protocol: Protocol,
    #[serde(default)]
    pub description: String,
    #[serde(default, rename = "readOnly")]
    pub read_only: bool,
    #[serde(default)]
    pub browseable: bool,
    #[serde(default, rename = "allowedHosts")]
    pub allowed_hosts: Vec<String>,
    #[serde(default, rename = "validUsers")]
    pub valid_users: Vec<String>,
    #[serde(default, rename = "validGroups")]
    pub valid_groups: Vec<String>,
    #[serde(default, rename = "timeMachine")]
    pub time_machine: bool,
    #[serde(default, rename = "noRootSquash")]
    pub no_root_squash: bool,
    #[serde(default, rename = "createdAt")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub enabled: bool,
}

/// `POST /api/shares` request.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct CreateShareRequest {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub protocol: Protocol,
    #[serde(default)]
    pub description: String,
    #[serde(default, rename = "readOnly")]
    pub read_only: bool,
    #[serde(default)]
    pub browseable: Option<bool>,
    #[serde(default, rename = "allowedHosts")]
    pub allowed_hosts: Vec<String>,
    #[serde(default, rename = "validUsers")]
    pub valid_users: Vec<String>,
    #[serde(default, rename = "validGroups")]
    pub valid_groups: Vec<String>,
    #[serde(default, rename = "timeMachine")]
    pub time_machine: bool,
    #[serde(default, rename = "noRootSquash")]
    pub no_root_squash: bool,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// `PUT /api/shares/{name}` request. Option fields: present means change.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct UpdateShareRequest {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "readOnly")]
    pub read_only: Option<bool>,
    #[serde(default)]
    pub browseable: Option<bool>,
    #[serde(default, rename = "allowedHosts")]
    pub allowed_hosts: Option<Vec<String>>,
    #[serde(default, rename = "validUsers")]
    pub valid_users: Option<Vec<String>>,
    #[serde(default, rename = "validGroups")]
    pub valid_groups: Option<Vec<String>>,
    #[serde(default, rename = "timeMachine")]
    pub time_machine: Option<bool>,
    #[serde(default, rename = "noRootSquash")]
    pub no_root_squash: Option<bool>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// The rendered service configuration for the current share state.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ConfigBundle {
    #[serde(rename = "sambaConf")]
    pub samba_conf: String,
    #[serde(rename = "ganeshaConf")]
    pub ganesha_conf: String,
    #[serde(default, rename = "sambaUsers")]
    pub samba_users: String,
    #[serde(default, rename = "nssPasswd")]
    pub nss_passwd: String,
    #[serde(default, rename = "nssGroup")]
    pub nss_group: String,
    #[serde(default, rename = "nssShadow")]
    pub nss_shadow: String,
    pub revision: String,
    #[serde(rename = "shareCount")]
    pub share_count: i32,
}

/// Manages share definitions.
pub struct Manager {
    shares: HashMap<String, Share>,
    config_path: String,
    zfs_base: String,
}

/// The directory tree shares may live under (overridable for development).
pub fn default_zfs_base() -> String {
    std::env::var("SHARES_ZFS_BASE").unwrap_or_else(|_| "/var/mnt".to_string())
}

/// The default share config path (overridable by `SHARES_CONFIG`).
pub fn default_config_path() -> String {
    std::env::var("SHARES_CONFIG").unwrap_or_else(|_| "/var/lib/naslos/shares.json".to_string())
}

impl Manager {
    pub fn new(config_path: &str, zfs_base: &str) -> Self {
        let base = if zfs_base.is_empty() {
            default_zfs_base()
        } else {
            zfs_base.to_string()
        };
        let mut m = Self {
            shares: HashMap::new(),
            config_path: config_path.to_string(),
            zfs_base: base,
        };
        m.load();
        m
    }

    pub fn from_env() -> Self {
        Self::new(&default_config_path(), &default_zfs_base())
    }

    pub fn zfs_base(&self) -> &str {
        &self.zfs_base
    }

    fn load(&mut self) {
        if self.config_path.is_empty() {
            return;
        }
        let Ok(data) = std::fs::read(&self.config_path) else {
            return;
        };
        let Ok(list) = serde_json::from_slice::<Vec<Share>>(&data) else {
            return;
        };
        for mut s in list {
            if s.name.is_empty() {
                continue;
            }
            s.allowed_hosts = normalize_list(&s.allowed_hosts);
            s.valid_users = normalize_list(&s.valid_users);
            s.valid_groups = normalize_list(&s.valid_groups);
            if validate_share_fields(&s).is_err() {
                continue;
            }
            self.shares.insert(s.name.clone(), s);
        }
    }

    /// Persist shares atomically (temp file + rename), 0600.
    fn save(&self) -> Result<(), String> {
        if self.config_path.is_empty() {
            return Ok(());
        }
        let list = self.list();
        let mut data =
            serde_json::to_vec_pretty(&list).map_err(|e| format!("marshaling shares: {e}"))?;
        data.push(b'\n');

        let path = Path::new(&self.config_path);
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        if !dir.as_os_str().is_empty() && dir != Path::new(".") {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("creating shares config directory: {e}"))?;
        }
        let tmp = tempfile::Builder::new()
            .prefix(".shares-")
            .suffix(".tmp")
            .tempfile_in(dir)
            .map_err(|e| format!("creating temp shares config: {e}"))?;
        {
            use std::io::Write;
            let mut f = tmp
                .as_file()
                .try_clone()
                .map_err(|e| format!("writing shares config: {e}"))?;
            f.write_all(&data)
                .map_err(|e| format!("writing shares config: {e}"))?;
            f.sync_all()
                .map_err(|e| format!("syncing shares config: {e}"))?;
        }
        let tmp_path = tmp.into_temp_path();
        std::fs::set_permissions(
            &tmp_path,
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
        .map_err(|e| format!("setting shares config mode: {e}"))?;
        std::fs::rename(&tmp_path, &self.config_path)
            .map_err(|e| format!("replacing shares config: {e}"))?;
        Ok(())
    }

    /// All shares, ordered by name.
    pub fn list(&self) -> Vec<Share> {
        let mut out: Vec<Share> = self.shares.values().cloned().collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    pub fn get(&self, name: &str) -> Result<Share, String> {
        self.shares
            .get(name)
            .cloned()
            .ok_or_else(|| format!("share {name:?} not found"))
    }

    pub fn create(&mut self, req: &CreateShareRequest) -> Result<Share, String> {
        validate_share_name(&req.name)?;
        let clean_path = self.normalize_path(&req.path)?;
        if req.protocol != Protocol::Smb && req.protocol != Protocol::Nfs {
            return Err(format!(
                "unsupported protocol {:?}: supported protocols are {:?} and {:?}",
                req.protocol.as_str(),
                Protocol::Smb.as_str(),
                Protocol::Nfs.as_str()
            ));
        }
        if self.shares.contains_key(&req.name) {
            return Err(format!("share {:?} already exists", req.name));
        }

        let share = Share {
            name: req.name.clone(),
            path: clean_path,
            protocol: req.protocol,
            description: req.description.clone(),
            read_only: req.read_only,
            browseable: req.browseable.unwrap_or(true),
            allowed_hosts: normalize_list(&req.allowed_hosts),
            valid_users: normalize_list(&req.valid_users),
            valid_groups: normalize_list(&req.valid_groups),
            time_machine: req.time_machine,
            no_root_squash: req.no_root_squash,
            created_at: Some(Utc::now()),
            enabled: req.enabled.unwrap_or(true),
        };
        validate_share_fields(&share)?;
        self.shares.insert(share.name.clone(), share.clone());
        if let Err(e) = self.save() {
            self.shares.remove(&share.name);
            return Err(e);
        }
        Ok(share)
    }

    pub fn update(&mut self, name: &str, req: &UpdateShareRequest) -> Result<Share, String> {
        let mut share = self.get(name)?;
        let previous = share.clone();

        if let Some(path) = &req.path {
            share.path = self.normalize_path(path)?;
        }
        if let Some(d) = &req.description {
            share.description = d.clone();
        }
        if let Some(v) = req.read_only {
            share.read_only = v;
        }
        if let Some(v) = req.browseable {
            share.browseable = v;
        }
        if let Some(v) = &req.allowed_hosts {
            share.allowed_hosts = normalize_list(v);
        }
        if let Some(v) = &req.valid_users {
            share.valid_users = normalize_list(v);
        }
        if let Some(v) = &req.valid_groups {
            share.valid_groups = normalize_list(v);
        }
        if let Some(v) = req.time_machine {
            share.time_machine = v;
        }
        if let Some(v) = req.no_root_squash {
            share.no_root_squash = v;
        }
        if let Some(v) = req.enabled {
            share.enabled = v;
        }

        validate_share_fields(&share)?;
        self.shares.insert(name.to_string(), share.clone());
        if let Err(e) = self.save() {
            self.shares.insert(name.to_string(), previous);
            return Err(e);
        }
        Ok(share)
    }

    pub fn delete(&mut self, name: &str) -> Result<(), String> {
        self.get(name)?;
        self.shares.remove(name);
        self.save()
    }

    /// Validate that `path` is inside the ZFS base and is a directory.
    pub fn validate_path(&self, path: &str) -> Result<(), String> {
        let clean = self.normalize_path(path)?;
        let meta = std::fs::metadata(&clean).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!("path does not exist: {clean}")
            } else {
                format!("stat path: {e}")
            }
        })?;
        if !meta.is_dir() {
            return Err(format!("path is not a directory: {clean}"));
        }
        Ok(())
    }

    /// Canonicalise a share path and require it strictly inside the ZFS base.
    fn normalize_path(&self, path: &str) -> Result<String, String> {
        if path.trim().is_empty() {
            return Err("share path is required".to_string());
        }
        let clean = clean_path(path);
        let base = clean_path(&self.zfs_base);
        if clean == base || !clean.starts_with(&format!("{base}/")) {
            return Err(format!("share path must be a directory inside {base}"));
        }
        Ok(clean)
    }

    /// The rendered config bundle for the current share state.
    pub fn render_config_bundle(&self) -> ConfigBundle {
        let enabled = self.shares.values().filter(|s| s.enabled).count() as i32;
        ConfigBundle {
            samba_conf: self.generate_samba_config(),
            ganesha_conf: self.generate_ganesha_config(),
            revision: self.revision(),
            share_count: enabled,
            ..Default::default()
        }
    }

    /// A stable content hash of the enabled share set.
    fn revision(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        for s in self.list() {
            h.update(
                format!(
                    "{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}\n",
                    s.name,
                    s.path,
                    s.protocol.as_str(),
                    s.enabled,
                    s.read_only,
                    s.browseable,
                    s.time_machine,
                    s.no_root_squash,
                    s.allowed_hosts.join(","),
                    s.valid_users.join(",")
                )
                .as_bytes(),
            );
        }
        let digest = h.finalize();
        digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
    }
}

/// Whether `path` lives on one of the given dataset mountpoints.
pub fn path_on_dataset(path: &str, mountpoints: &[String]) -> (String, bool) {
    let clean = clean_path(path);
    if !clean.starts_with('/') {
        return (String::new(), false);
    }
    let mut best = String::new();
    for mp in mountpoints {
        let mp = clean_path(mp.trim());
        if mp.is_empty() || mp == "." || mp == "/" || !mp.starts_with('/') {
            continue;
        }
        if (clean == mp || clean.starts_with(&format!("{mp}/"))) && mp.len() > best.len() {
            best = mp;
        }
    }
    let found = !best.is_empty();
    (best, found)
}

/// `filepath.Clean` for absolute/unix paths.
pub fn clean_path(s: &str) -> String {
    if s.is_empty() {
        return ".".to_string();
    }
    let rooted = s.starts_with('/');
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
    let joined = out.join("/");
    if rooted {
        format!("/{joined}")
    } else if joined.is_empty() {
        ".".to_string()
    } else {
        joined
    }
}

/// Reject names that are empty, contain separators/control chars, or would be
/// ambiguous as a config section / export key.
pub fn validate_share_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("share name is required".to_string());
    }
    if name.len() > 80 {
        return Err("share name must be 80 characters or fewer".to_string());
    }
    if name.chars().any(|c| {
        matches!(
            c,
            '/' | '\\'
                | '['
                | ']'
                | '"'
                | '\''
                | ':'
                | '*'
                | '?'
                | '<'
                | '>'
                | '='
                | '+'
                | ';'
                | ','
        )
    }) {
        return Err(
            "share name must not contain any of / \\ [ ] \" ' : * ? < > = + ; ,".to_string(),
        );
    }
    if name.contains(['\n', '\r', '\0', '\t']) {
        return Err("share name must not contain control characters".to_string());
    }
    if name.trim() != name {
        return Err("share name must not start or end with whitespace".to_string());
    }
    Ok(())
}

/// Reject characters that would start a new directive/statement (NAS-007).
pub fn validate_share_fields(share: &Share) -> Result<(), String> {
    validate_no_control_chars("description", &share.description)?;
    validate_no_control_chars("path", &share.path)?;
    for (kind, entries) in [
        ("allowed host", &share.allowed_hosts),
        ("valid user", &share.valid_users),
        ("valid group", &share.valid_groups),
    ] {
        for entry in entries {
            validate_no_control_chars(kind, entry)?;
            if entry.contains(';') || entry.contains('"') {
                return Err(format!("{kind} {entry:?} must not contain ';' or '\"'"));
            }
        }
    }
    Ok(())
}

fn validate_no_control_chars(kind: &str, value: &str) -> Result<(), String> {
    if value.contains(['\n', '\r', '\0', '\t']) {
        return Err(format!(
            "{kind} must not contain control characters (newline, tab or NUL)"
        ));
    }
    Ok(())
}

/// Trim entries, drop empties and de-duplicate while preserving order.
pub fn normalize_list(input: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(input.len());
    let mut seen = std::collections::HashSet::new();
    for v in input {
        let v = v.trim();
        if v.is_empty() || !seen.insert(v.to_string()) {
            continue;
        }
        out.push(v.to_string());
    }
    out
}

// Re-export the renderers for convenience.
pub use render::{access_list, effective_clients, sanitize_netbios_name};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_name_validation() {
        assert!(validate_share_name("Media").is_ok());
        assert!(validate_share_name("").is_err());
        assert!(validate_share_name("a/b").is_err());
        assert!(validate_share_name("a\nb").is_err());
        assert!(validate_share_name(" padded ").is_err());
    }

    #[test]
    fn path_on_dataset_prefers_most_specific() {
        let mps = vec![
            "/var/mnt/tank".to_string(),
            "/var/mnt/tank/media".to_string(),
        ];
        assert_eq!(
            path_on_dataset("/var/mnt/tank/media/x", &mps),
            ("/var/mnt/tank/media".to_string(), true)
        );
        assert!(!path_on_dataset("/var/mnt/other", &mps).1);
    }

    #[test]
    fn normalize_list_dedupes() {
        let out = normalize_list(&[" a ".into(), "b".into(), "a".into(), "".into()]);
        assert_eq!(out, vec!["a", "b"]);
    }

    #[test]
    fn clean_path_resolves_traversal() {
        assert_eq!(
            clean_path("/var/mnt/tank/../tank/media"),
            "/var/mnt/tank/media"
        );
        assert_eq!(clean_path("/var/mnt/tank/"), "/var/mnt/tank");
    }
}
