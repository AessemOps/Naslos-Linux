//! Folder management confined to the datasets mount (port of
//! `shares/folders.go`).

use super::SharesClient;

/// In-host mount root of the ZFS datasets shares are served from.
pub const DATASETS_BASE: &str = "/var/mnt";

/// ZFS's magic snapshot-browsing directory.
const ZFS_SNAPSHOTS_DIR: &str = ".zfs";

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

        let parent_path = self.host_path(&clean_parent);
        let info = std::fs::metadata(&parent_path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!("folder does not exist: {clean_parent}")
            } else {
                format!("stat {clean_parent}: {e}")
            }
        })?;
        if !info.is_dir() {
            return Err(format!("{clean_parent} is not a folder"));
        }

        let full = format!("{clean_parent}/{name}");
        std::fs::create_dir(self.host_path(&full)).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                format!("folder already exists: {full}")
            } else {
                format!("creating {full}: {e}")
            }
        })?;
        Ok(full)
    }

    /// Remove an EMPTY directory; a non-empty one is deliberately refused.
    pub fn delete_folder(&self, dir_path: &str) -> Result<(), String> {
        let clean = self.clean_folder_path(dir_path)?;
        if clean == DATASETS_BASE {
            return Err("the datasets root cannot be removed".to_string());
        }

        let target = self.host_path(&clean);
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

        // Resolve symlinks on the host side and re-check containment.
        let resolved_base = match std::fs::canonicalize(self.host_path(DATASETS_BASE)) {
            Ok(p) => p,
            Err(_) => return Ok(clean),
        };
        match std::fs::canonicalize(self.host_path(&clean)) {
            Ok(resolved_target) => {
                if resolved_target.strip_prefix(&resolved_base).is_err() {
                    return Err(format!("folder path must resolve inside {DATASETS_BASE}"));
                }
                Ok(clean)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(clean),
            Err(e) => Err(format!("resolving {clean}: {e}")),
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
