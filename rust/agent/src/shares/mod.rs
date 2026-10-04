//! Applies Naslos share service configuration on the Talos host.
//!
//! The API owns share definitions and renders the config files; this is the
//! privileged half that writes them into the host layout. The serving
//! containers watch the files and reload themselves.

pub mod config;
pub mod folders;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Where the Talos host filesystem is mounted into the agent pod.
pub const DEFAULT_HOST_ROOT: &str = "/host";

/// Config dir holding rendered files and samba's mutable state.
pub const CONFIG_DIR: &str = "/var/lib/naslos/shares";
pub const SAMBA_CONF_PATH: &str = "/var/lib/naslos/shares/smb.conf";
pub const GANESHA_CONF_PATH: &str = "/var/lib/naslos/shares/ganesha.conf";
pub const REVISION_PATH: &str = "/var/lib/naslos/shares/revision";
/// Rendered smbpasswd-format account file.
pub const SMB_USERS_PATH: &str = "/var/lib/naslos/shares/smbusers";
/// extrausers-format files (mounted at /var/lib/extrausers).
pub const NSS_DIR: &str = "/var/lib/naslos/shares/extrausers";

/// The rendered configuration pushed by the API.
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub samba_conf: String,
    pub ganesha_conf: String,
    pub samba_users: String,
    pub nss_passwd: String,
    pub nss_group: String,
    pub nss_shadow: String,
    pub revision: String,
    #[allow(dead_code)]
    pub share_count: i32,
}

/// Outcome of an apply, or the current on-host state.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Status {
    pub applied: bool,
    pub revision: String,
    #[serde(rename = "sambaConfPath")]
    pub samba_conf_path: String,
    #[serde(rename = "ganeshaConfPath")]
    pub ganesha_conf_path: String,
    #[serde(rename = "smbShareCount")]
    pub smb_share_count: i32,
    #[serde(rename = "nfsExportCount")]
    pub nfs_export_count: i32,
    pub messages: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Applies share configuration on the host.
pub struct SharesClient {
    host_root: PathBuf,
}

impl SharesClient {
    pub fn new(host_root: impl Into<PathBuf>) -> Self {
        Self {
            host_root: host_root.into(),
        }
    }

    /// `HostPath` for an in-host path.
    pub(crate) fn host_path(&self, in_host: &str) -> PathBuf {
        self.host_root.join(in_host.trim_start_matches('/'))
    }

    pub fn host_root(&self) -> &Path {
        &self.host_root
    }
}

/// Whether the host filesystem is reachable at `host_root`.
pub fn is_available(host_root: &Path) -> bool {
    host_root.is_dir()
}

/// Atomically write content to an in-host path (temp file + rename).
pub(crate) fn write_atomic(host_file_path: &Path, content: &str, mode: u32) -> Result<(), String> {
    let parent = host_file_path
        .parent()
        .ok_or_else(|| format!("no parent directory for {}", host_file_path.display()))?;
    std::fs::create_dir_all(parent).map_err(|e| format!("creating {}: {e}", parent.display()))?;

    let mut tmp = tempfile::Builder::new()
        .prefix(".tmp-")
        .tempfile_in(parent)
        .map_err(|e| format!("creating temp file for {}: {e}", host_file_path.display()))?;

    use std::io::Write;
    {
        tmp.as_file_mut()
            .write_all(content.as_bytes())
            .map_err(|e| format!("writing {}: {e}", host_file_path.display()))?;
        tmp.as_file()
            .sync_all()
            .map_err(|e| format!("syncing {}: {e}", host_file_path.display()))?;
    }

    let tmp_path = tmp.into_temp_path();
    std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(mode))
        .map_err(|e| format!("chmod {}: {e}", host_file_path.display()))?;
    std::fs::rename(&tmp_path, host_file_path)
        .map_err(|e| format!("replacing {}: {e}", host_file_path.display()))?;
    Ok(())
}

pub(crate) fn read_host_file(host_file_path: &Path) -> String {
    std::fs::read_to_string(host_file_path).unwrap_or_default()
}
