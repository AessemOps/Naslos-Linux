//! Folder management confined to the datasets mount (port of
//! `shares/folders.go`).
//!
//! Creation resolves the nearest existing ancestor, requires it to stay inside
//! the canonicalised datasets root, and walks down with `openat(O_NOFOLLOW)`
//! so a symlink swapped in under a writable share can never redirect a create
//! outside `/var/mnt`. The new folder is made writable by the share service
//! account (`10001:10000`, mode `02770`) or inherits the parent ownership when
//! the agent is not privileged enough to chown (R-041).

use super::SharesClient;
use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;

/// In-host mount root of the ZFS datasets shares are served from.
pub const DATASETS_BASE: &str = "/var/mnt";

/// ZFS's magic snapshot-browsing directory.
const ZFS_SNAPSHOTS_DIR: &str = ".zfs";

/// Owner/group the share service containers run as.
const SHARE_UID: u32 = 10001;
const SHARE_GID: u32 = 10000;
/// Group-writable with setgid so every service account sharing the group can
/// write and new files keep the dataset's group.
const SHARE_DIR_MODE: u32 = 0o2770;

/// `filepath.Clean` for an absolute path (resolves `.`/`..`, collapses `//`).
fn clean_abs_path(s: &str) -> String {
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
    format!("/{}", out.join("/"))
}

fn open_dir_nofollow(path: &Path) -> Result<OwnedFd, String> {
    let c = CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| format!("path {} contains NUL", path.display()))?;
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(format!(
            "opening {}: {}",
            path.display(),
            io::Error::last_os_error()
        ));
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn open_child_dir(parent: &OwnedFd, name: &str) -> Result<OwnedFd, io::Error> {
    let c = CString::new(name).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn mkdir_child(parent: &OwnedFd, name: &str, mode: u32) -> Result<(), io::Error> {
    let c = CString::new(name).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    if unsafe { libc::mkdirat(parent.as_raw_fd(), c.as_ptr(), mode as libc::mode_t) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Give the folder the share account ownership and mode, or inherit the
/// parent's ownership when the agent may not chown to 10001:10000.
fn set_share_owner(folder: &OwnedFd, parent: &OwnedFd) -> Result<(), String> {
    if unsafe { libc::fchown(folder.as_raw_fd(), SHARE_UID, SHARE_GID) } != 0 {
        let err = io::Error::last_os_error();
        let code = err.raw_os_error();
        if code == Some(libc::EPERM) || code == Some(libc::EINVAL) {
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            if unsafe { libc::fstat(parent.as_raw_fd(), &mut st) } != 0 {
                return Err(format!(
                    "stat parent folder: {}",
                    io::Error::last_os_error()
                ));
            }
            if unsafe { libc::fchown(folder.as_raw_fd(), st.st_uid, st.st_gid) } != 0 {
                return Err(format!(
                    "inheriting parent ownership: {}",
                    io::Error::last_os_error()
                ));
            }
        } else {
            return Err(format!("chown {SHARE_UID}:{SHARE_GID}: {err}"));
        }
    }
    if unsafe { libc::fchmod(folder.as_raw_fd(), SHARE_DIR_MODE as libc::mode_t) } != 0 {
        return Err(format!(
            "chmod {SHARE_DIR_MODE:o}: {}",
            io::Error::last_os_error()
        ));
    }
    Ok(())
}

impl SharesClient {
    /// Immediate subdirectories of an in-host directory, sorted.
    pub fn list_folders(&self, dir_path: &str) -> Result<Vec<String>, String> {
        let clean = self.clean_folder_path(dir_path)?;
        let entries = std::fs::read_dir(self.host_path(&clean)).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!("folder does not exist: {clean}")
            } else {
                format!("reading {clean}: {e}")
            }
        })?;

        let mut folders = Vec::new();
        for entry in entries.flatten() {
            // Do not follow symlinks to directories elsewhere.
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let name = entry.file_name().to_string_lossy().into_owned();
            if !is_dir || name == ZFS_SNAPSHOTS_DIR {
                continue;
            }
            folders.push(name);
        }
        folders.sort();
        Ok(folders)
    }

    /// Create one folder inside `parent`; the parent must already exist.
    pub fn create_folder(&self, parent: &str, name: &str) -> Result<String, String> {
        let clean_parent = self.clean_folder_path(parent)?;
        validate_folder_name(name)?;

        let host_base = self.host_path(DATASETS_BASE);
        let base = std::fs::canonicalize(&host_base).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!("datasets root does not exist: {DATASETS_BASE}")
            } else {
                format!("resolving {DATASETS_BASE}: {e}")
            }
        })?;
        let mut dir = open_dir_nofollow(&base)?;

        let relative = clean_parent.strip_prefix(DATASETS_BASE).unwrap_or_default();
        for part in relative.split('/').filter(|p| !p.is_empty()) {
            match open_child_dir(&dir, part) {
                Ok(fd) => dir = fd,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    return Err(format!("folder does not exist: {clean_parent}"));
                }
                Err(e)
                    if e.raw_os_error() == Some(libc::ELOOP)
                        || e.raw_os_error() == Some(libc::ENOTDIR) =>
                {
                    return Err(format!(
                        "{clean_parent} is not a real folder (a symlink is not allowed): {e}"
                    ));
                }
                Err(e) => return Err(format!("opening {part} below {DATASETS_BASE}: {e}")),
            }
        }

        let full = format!("{clean_parent}/{name}");
        match mkdir_child(&dir, name, SHARE_DIR_MODE) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                return Err(format!("folder already exists: {full}"));
            }
            Err(e) => return Err(format!("creating {full}: {e}")),
        }
        let folder =
            open_child_dir(&dir, name).map_err(|e| format!("opening {full} after create: {e}"))?;
        set_share_owner(&folder, &dir)
            .map_err(|e| format!("setting ownership/mode on {full}: {e}"))?;

        Ok(full)
    }

    /// Remove an EMPTY directory; a non-empty one is deliberately refused.
    pub fn delete_folder(&self, dir_path: &str) -> Result<(), String> {
        let clean = self.clean_folder_path(dir_path)?;
        if clean == DATASETS_BASE {
            return Err("the datasets root cannot be removed".to_string());
        }

        let target = self.host_path(&clean);
        let meta = std::fs::symlink_metadata(&target).map_err(|e| format!("stat {clean}: {e}"))?;
        if meta.file_type().is_symlink() {
            return Err(format!("refusing to remove a symlink: {clean}"));
        }
        if !meta.is_dir() {
            return Err(format!("{clean} is not a folder"));
        }
        let entries = std::fs::read_dir(&target).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!("folder does not exist: {clean}")
            } else {
                format!("reading {clean}: {e}")
            }
        })?;
        if entries.count() > 0 {
            return Err(format!("folder {clean} is not empty"));
        }

        std::fs::remove_dir(&target).map_err(|e| format!("removing {clean}: {e}"))
    }

    /// Canonicalise an in-host folder path and require it inside `DATASETS_BASE`.
    pub fn clean_folder_path(&self, path: &str) -> Result<String, String> {
        let trimmed = path.trim();
        if trimmed.is_empty() {
            return Err("folder path is required".to_string());
        }

        // Reject non-absolute input BEFORE cleaning: clean_abs_path always
        // yields a leading '/', so the check below could never fail on its own.
        if !trimmed.starts_with('/') {
            return Err(format!("folder path must be absolute: {trimmed}"));
        }

        let clean = clean_abs_path(trimmed);
        if clean != DATASETS_BASE && !clean.starts_with(&format!("{DATASETS_BASE}/")) {
            return Err(format!("folder path must be inside {DATASETS_BASE}"));
        }

        // Resolve the nearest existing ancestor and re-check containment. The
        // old code canonicalised only the full path, so a non-existent leaf
        // under a symlinked parent skipped the check entirely.
        let host_base = self.host_path(DATASETS_BASE);
        let Ok(resolved_base) = std::fs::canonicalize(&host_base) else {
            return Ok(clean);
        };
        let mut probe = clean.clone();
        loop {
            match std::fs::canonicalize(self.host_path(&probe)) {
                Ok(resolved) => {
                    if resolved != resolved_base && !resolved.starts_with(&resolved_base) {
                        return Err(format!("folder path must resolve inside {DATASETS_BASE}"));
                    }
                    return Ok(clean);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    let Some(idx) = probe.rfind('/') else {
                        return Err(format!("folder path must be inside {DATASETS_BASE}"));
                    };
                    if idx == 0 {
                        return Err(format!("folder path must be inside {DATASETS_BASE}"));
                    }
                    probe.truncate(idx);
                    if probe.len() < DATASETS_BASE.len() {
                        return Err(format!("folder path must be inside {DATASETS_BASE}"));
                    }
                }
                Err(e) => return Err(format!("resolving {probe}: {e}")),
            }
        }
    }
}

/// Reject names that are empty, traversals, separators or control characters.
pub fn validate_folder_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("folder name is required".to_string());
    }
    if name.trim() != name {
        return Err("folder name must not start or end with whitespace".to_string());
    }
    if name.len() > 255 {
        return Err("folder name must be 255 characters or fewer".to_string());
    }
    if name == "." || name == ".." {
        return Err(format!("folder name must not be {name:?}"));
    }
    if name.contains('/') || name.contains('\\') {
        return Err("folder name must not contain a path separator".to_string());
    }
    if name.contains('\n') || name.contains('\r') || name.contains('\t') || name.contains('\0') {
        return Err("folder name must not contain control characters".to_string());
    }
    if name == ZFS_SNAPSHOTS_DIR {
        return Err(format!(
            "{ZFS_SNAPSHOTS_DIR} is reserved by ZFS for snapshots"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;

    fn client() -> (tempfile::TempDir, SharesClient) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("var/mnt/tank")).unwrap();
        let client = SharesClient::new(dir.path());
        (dir, client)
    }

    #[test]
    fn create_folder_sets_share_ownership_and_mode() {
        let (dir, client) = client();
        let parent = dir.path().join("var/mnt/tank");
        let created = client.create_folder("/var/mnt/tank", "media").unwrap();
        assert_eq!(created, "/var/mnt/tank/media");

        let meta = std::fs::metadata(parent.join("media")).unwrap();
        assert_eq!(meta.permissions().mode() & 0o7777, SHARE_DIR_MODE);
        if unsafe { libc::geteuid() } == 0 {
            assert_eq!(meta.uid(), SHARE_UID);
            assert_eq!(meta.gid(), SHARE_GID);
        } else {
            let parent_meta = std::fs::metadata(&parent).unwrap();
            assert_eq!(meta.uid(), parent_meta.uid());
            assert_eq!(meta.gid(), parent_meta.gid());
        }
    }

    #[cfg(unix)]
    #[test]
    fn create_folder_refuses_a_symlinked_parent() {
        use std::os::unix::fs::symlink;

        let (dir, client) = client();
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        symlink(&outside, dir.path().join("var/mnt/tank/link")).unwrap();

        let err = client
            .create_folder("/var/mnt/tank/link", "escape")
            .unwrap_err();
        assert!(
            err.contains("symlink") || err.contains("inside"),
            "symlinked parent must be refused: {err}"
        );
        assert!(!outside.join("escape").exists());
    }

    #[cfg(unix)]
    #[test]
    fn clean_folder_path_rejects_a_symlinked_ancestor() {
        use std::os::unix::fs::symlink;

        let (dir, client) = client();
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(outside.join("sub")).unwrap();
        symlink(&outside, dir.path().join("var/mnt/tank/link")).unwrap();

        assert!(client.clean_folder_path("/var/mnt/tank/link").is_err());
        assert!(client
            .clean_folder_path("/var/mnt/tank/link/not-created-yet")
            .is_err());
        assert!(client.clean_folder_path("/var/mnt/tank/media").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn delete_folder_refuses_a_symlink() {
        use std::os::unix::fs::symlink;

        let (dir, client) = client();
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        symlink(&outside, dir.path().join("var/mnt/tank/link")).unwrap();
        assert!(client.delete_folder("/var/mnt/tank/link").is_err());
        assert!(outside.exists());
    }
}
