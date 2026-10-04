//! Dataset listing, validation, creation and destruction (port of
//! `zfs/dataset.go`).

use super::devices::validate_pool_name;
use super::{ZfsClient, ZFS_BIN};
use crate::invalid;
use crate::zfs::validation::{ZfsError, ZfsResult};
use crate::zfs::Dataset;

/// `humanBytes` from the Go agent: powers of 1024, one decimal below 10.
pub fn human_bytes(n: i64) -> String {
    const UNIT: i64 = 1024;
    if n < UNIT {
        return format!("{n}B");
    }
    let units = ['K', 'M', 'G', 'T', 'P', 'E'];
    let mut value = n as f64;
    let mut i: i32 = -1;
    while value >= UNIT as f64 && i < units.len() as i32 - 1 {
        value /= UNIT as f64;
        i += 1;
    }
    let unit = units[i.max(0) as usize];
    if value < 10.0 {
        format!("{value:.1}{unit}")
    } else {
        format!("{value:.0}{unit}")
    }
}

/// ZFS-safe dataset component charset: `^[A-Za-z0-9][A-Za-z0-9._-]*$`.
fn is_valid_dataset_component(part: &str) -> bool {
    let mut chars = part.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// `^[0-9]+(\.[0-9]+)?[KMGTPE]?$`.
fn matches_size(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    let digits = |b: u8| b.is_ascii_digit();
    if i >= bytes.len() || !digits(bytes[i]) {
        return false;
    }
    while i < bytes.len() && digits(bytes[i]) {
        i += 1;
    }
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        if i >= bytes.len() || !digits(bytes[i]) {
            return false;
        }
        while i < bytes.len() && digits(bytes[i]) {
            i += 1;
        }
    }
    if i < bytes.len() {
        if !matches!(bytes[i], b'K' | b'M' | b'G' | b'T' | b'P' | b'E') {
            return false;
        }
        i += 1;
    }
    i == bytes.len()
}

fn valid_compression(v: &str) -> bool {
    matches!(
        v,
        "on" | "off" | "lz4" | "zstd" | "zstd-fast" | "gzip" | "gzip-1" | "gzip-9" | "lzjb" | "zle"
    )
}

fn is_dataset_option_key(k: &str) -> bool {
    matches!(
        k,
        "compression" | "quota" | "recordsize" | "atime" | "copies" | "readonly"
    )
}

/// Validate a dataset name relative to its pool.
pub fn validate_dataset_name(name: &str) -> ZfsResult<()> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(ZfsError::Validation("dataset name is required".into()));
    }
    if trimmed != name {
        return Err(ZfsError::Validation(
            "dataset name must not start or end with whitespace".into(),
        ));
    }
    if trimmed.len() > 200 {
        return Err(ZfsError::Validation(
            "dataset name must be 200 characters or fewer".into(),
        ));
    }
    if trimmed.starts_with('/') {
        return Err(ZfsError::Validation(
            "dataset name must be relative to its pool, not a path".into(),
        ));
    }
    if trimmed.contains('@') {
        return Err(ZfsError::Validation(
            "dataset name must not contain '@' (that is a snapshot)".into(),
        ));
    }
    for part in trimmed.split('/') {
        if !is_valid_dataset_component(part) {
            return Err(invalid!(
                "invalid dataset name {:?}: each part must start with a letter or digit and contain only letters, digits, '.', '_' or '-'",
                trimmed
            ));
        }
    }
    Ok(())
}

/// Validate the properties a dataset is created with.
pub fn validate_dataset_options(
    options: &std::collections::HashMap<String, String>,
) -> ZfsResult<()> {
    for (key, raw) in options {
        let value = raw.trim();
        if !is_dataset_option_key(key) {
            return Err(invalid!("unsupported dataset option {:?}", key));
        }
        match key.as_str() {
            "compression" => {
                if !valid_compression(value) {
                    return Err(invalid!("unsupported compression {:?}", value));
                }
            }
            "quota" => {
                if !value.is_empty() && value != "none" && !matches_size(value) {
                    return Err(invalid!(
                        "invalid quota {:?}: use a size such as 500G, or none to remove it",
                        value
                    ));
                }
            }
            "recordsize" => {
                if !matches_size(value) {
                    return Err(invalid!("invalid recordsize {:?}", value));
                }
            }
            "atime" | "readonly" => {
                if value != "on" && value != "off" {
                    return Err(invalid!("{} must be on or off", key));
                }
            }
            "copies" => {
                if value != "1" && value != "2" && value != "3" {
                    return Err(ZfsError::Validation(
                        "copies must be 1, 2 or 3".into(),
                    ));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Validate a full dataset path: an existing-looking pool plus one or more
/// components.
pub fn validate_dataset_path(name: &str) -> ZfsResult<()> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(ZfsError::Validation("dataset name is required".into()));
    }
    if trimmed.starts_with('/') {
        return Err(invalid!(
            "dataset {:?} must be relative to its pool, not an absolute path",
            name
        ));
    }
    let parts: Vec<&str> = trimmed.split('/').collect();
    if parts.len() < 2 {
        return Err(invalid!("dataset {:?} must be <pool>/<name>", name));
    }
    validate_pool_name(parts[0])?;
    validate_dataset_name(&parts[1..].join("/"))
}

impl ZfsClient {
    /// Every dataset on the node, with exact and human sizes.
    pub async fn all_datasets(&self) -> ZfsResult<Vec<Dataset>> {
        let out = match self
            .host_exec(
                ZFS_BIN,
                &strings(&[
                    "list",
                    "-H",
                    "-p",
                    "-o",
                    "name,used,avail,refer,mountpoint,mounted",
                    "-t",
                    "filesystem",
                    "-r",
                ]),
            )
            .await
        {
            Ok(out) => out,
            Err(e) => {
                return Err(ZfsError::Other(format!(
                    "listing datasets: {}",
                    e.message
                )))
            }
        };

        let mut datasets = Vec::new();
        for line in out.trim().split('\n') {
            if line.is_empty() {
                continue;
            }
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() < 5 {
                continue;
            }
            let used_bytes: i64 = fields[1].parse().unwrap_or(0);
            let avail_bytes: i64 = fields[2].parse().unwrap_or(0);
            let refer_bytes: i64 = fields[3].parse().unwrap_or(0);
            datasets.push(Dataset {
                name: fields[0].to_string(),
                used: human_bytes(used_bytes),
                used_bytes,
                avail: human_bytes(avail_bytes),
                refer: human_bytes(refer_bytes),
                mountpoint: fields[4].to_string(),
                mounted: fields.len() > 5 && fields[5] == "yes",
            });
        }
        Ok(datasets)
    }

    /// Datasets in a pool (human-readable sizes as `zfs list` prints them).
    pub async fn datasets(&self, pool: &str) -> ZfsResult<Vec<Dataset>> {
        validate_pool_name(pool)?;
        let out = match self
            .host_exec(
                ZFS_BIN,
                &strings(&[
                    "list",
                    "-H",
                    "-o",
                    "name,used,avail,refer,mountpoint,mounted",
                    "-r",
                    pool,
                ]),
            )
            .await
        {
            Ok(out) => out,
            Err(e) => {
                return Err(ZfsError::Other(format!(
                    "listing datasets: {}",
                    e.message
                )))
            }
        };

        let mut datasets = Vec::new();
        for line in out.trim().split('\n') {
            if line.is_empty() {
                continue;
            }
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() < 5 {
                continue;
            }
            datasets.push(Dataset {
                name: fields[0].to_string(),
                used: fields[1].to_string(),
                avail: fields[2].to_string(),
                refer: fields[3].to_string(),
                mountpoint: fields[4].to_string(),
                used_bytes: 0,
                mounted: fields.len() > 5 && fields[5] == "yes",
            });
        }
        Ok(datasets)
    }

    /// Create a dataset (`-p` so nested names create their parents).
    pub async fn create_dataset(
        &self,
        name: &str,
        options: &std::collections::HashMap<String, String>,
    ) -> ZfsResult<()> {
        validate_dataset_path(name)?;
        validate_dataset_options(options)?;

        let mut argv: Vec<String> = vec!["create".into(), "-p".into()];
        let mut keys: Vec<&String> = options.keys().collect();
        keys.sort();
        for k in keys {
            argv.push("-o".into());
            argv.push(format!("{}={}", k, options[k]));
        }
        argv.push(name.to_string());

        match self.host_exec(ZFS_BIN, &argv).await {
            Ok(_) => Ok(()),
            Err(e) => Err(ZfsError::Other(format!(
                "creating dataset: {}: {}",
                e.output.trim(),
                e.message
            ))),
        }
    }

    /// Destroy a dataset, recovering from the AV-8 stale-mount "busy" case.
    pub async fn destroy_dataset(&self, name: &str, recursive: bool) -> ZfsResult<()> {
        validate_dataset_path(name)?;

        let mut argv: Vec<String> = vec!["destroy".into()];
        if recursive {
            argv.push("-r".into());
        }
        argv.push(name.to_string());

        let mut result = self.host_exec(ZFS_BIN, &argv).await;
        let err = match result {
            Ok(_) => return Ok(()),
            Err(ref e) => e.clone(),
        };
        if !is_busy_error(&err.output) {
            return Err(ZfsError::Other(format!(
                "destroying dataset: {}: {}",
                err.output.trim(),
                err.message
            )));
        }

        // Rung 1: `zfs unmount -f` releases a mount a dead namespace still
        // tracks.
        tracing::warn!("dataset {} is busy; forcing an unmount and retrying", name);
        let unmount = self
            .host_exec(ZFS_BIN, &strings(&["unmount", "-f", name]))
            .await;
        match unmount {
            Ok(_) => {
                result = self.host_exec(ZFS_BIN, &argv).await;
                if result.is_ok() {
                    return Ok(());
                }
            }
            Err(unmount_err) => {
                if !unmount_err
                    .output
                    .to_ascii_lowercase()
                    .contains("not currently mounted")
                {
                    return Err(invalid!(
                        "destroying dataset: it is busy ({}) and the forced unmount also failed: {}: {}",
                        err.output.trim(),
                        unmount_err.output.trim(),
                        unmount_err.message
                    ));
                }
            }
        }

        // Rung 2: clear the recorded mountpoint and retry once more.
        tracing::warn!(
            "dataset {} is still busy with no live mount; clearing its mountpoint and retrying",
            name
        );
        let prop = self
            .host_exec(
                ZFS_BIN,
                &strings(&["set", "mountpoint=none", "canmount=off", name]),
            )
            .await;
        if let Err(prop_err) = prop {
            return Err(invalid!(
                "destroying dataset: it is busy with no live mount ({}) and clearing the mountpoint also failed: {}: {}",
                err.output.trim(),
                prop_err.output.trim(),
                prop_err.message
            ));
        }

        match self.host_exec(ZFS_BIN, &argv).await {
            Ok(_) => Ok(()),
            Err(retry_err) => {
                if is_busy_error(&retry_err.output) {
                    Err(invalid!(
                        "destroying dataset: it is still busy with mounted=no, no snapshots, children or holds ({}). This is the openzfs leaked-reference case that needs a pool export/import (the agent does not export, as that would take the shares offline)",
                        retry_err.output.trim()
                    ))
                } else {
                    Err(ZfsError::Other(format!(
                        "destroying dataset after clearing its mountpoint: {}: {}",
                        retry_err.output.trim(),
                        retry_err.message
                    )))
                }
            }
        }
    }

    /// Create a snapshot.
    pub async fn snapshot(&self, dataset: &str, snap_name: &str) -> ZfsResult<()> {
        validate_dataset_path(dataset)?;
        super::backup::validate_snapshot_name(snap_name)?;
        let name = format!("{dataset}@{snap_name}");
        match self
            .host_exec(ZFS_BIN, &strings(&["snapshot", &name]))
            .await
        {
            Ok(_) => Ok(()),
            Err(e) => Err(ZfsError::Other(format!(
                "creating snapshot: {}: {}",
                e.output, e.message
            ))),
        }
    }

    /// List a dataset's snapshots (names without the `dataset@` prefix).
    pub async fn snapshots(&self, dataset: &str) -> ZfsResult<Vec<String>> {
        validate_dataset_path(dataset)?;
        let out = match self
            .host_exec(
                ZFS_BIN,
                &strings(&["list", "-H", "-t", "snapshot", "-o", "name", "-r", dataset]),
            )
            .await
        {
            Ok(out) => out,
            Err(e) => {
                return Err(ZfsError::Other(format!("listing snapshots: {}", e.message)))
            }
        };

        let prefix = format!("{dataset}@");
        let mut snaps = Vec::new();
        for line in out.trim().split('\n') {
            if !line.is_empty() {
                snaps.push(line.strip_prefix(&prefix).unwrap_or(line).to_string());
            }
        }
        Ok(snaps)
    }
}

/// The stale-mount "dataset is busy" signature that a forced unmount can clear.
pub fn is_busy_error(out: &str) -> bool {
    let lower = out.to_ascii_lowercase();
    lower.contains("dataset is busy") || (lower.contains("cannot destroy") && lower.contains("busy"))
}

/// Build `Vec<String>` from `&str` items.
pub(crate) fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}
