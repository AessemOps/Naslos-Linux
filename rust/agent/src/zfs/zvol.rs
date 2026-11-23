//! ZFS zvol (block volume) creation, listing, resizing and destruction.
//!
//! The zvol property surface is deliberately small: only `volblocksize` and
//! `compression` are accepted, and every value is validated before it becomes
//! an argv element (`zfs create -V …` / `zfs set volsize=…`). Dataset-only
//! options are refused here and zvol-only options are refused by the dataset
//! validator, so a request cannot hop between the two APIs.

use super::dataset::{
    human_bytes, is_dataset_option_key, matches_size, valid_compression, validate_dataset_path,
    validate_new_dataset_path,
};
use super::devices::validate_pool_name;
use super::runner::join_host;
use super::{ZfsClient, ZFS_BIN};
use crate::zfs::validation::{ZfsError, ZfsResult};
use crate::zfs::Zvol;
use crate::{conflict, invalid};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Smallest block size OpenZFS accepts for a zvol.
pub const MIN_VOLBLOCKSIZE: u64 = 512;
/// Largest block size OpenZFS accepts for a zvol.
pub const MAX_VOLBLOCKSIZE: u64 = 16 * 1024 * 1024;

/// Parse a validated ZFS size (`10G`, `1.5T`, `4096`) into bytes.
pub fn parse_size_bytes(raw: &str) -> Option<u64> {
    let s = raw.trim();
    if !matches_size(s) {
        return None;
    }
    let (number, unit) = match s.as_bytes().last() {
        Some(b'K') => (&s[..s.len() - 1], 1024u64),
        Some(b'M') => (&s[..s.len() - 1], 1024u64.pow(2)),
        Some(b'G') => (&s[..s.len() - 1], 1024u64.pow(3)),
        Some(b'T') => (&s[..s.len() - 1], 1024u64.pow(4)),
        Some(b'P') => (&s[..s.len() - 1], 1024u64.pow(5)),
        Some(b'E') => (&s[..s.len() - 1], 1024u64.pow(6)),
        _ => (s, 1),
    };
    let value: f64 = number.parse().ok()?;
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    let bytes = value * unit as f64;
    if bytes >= u64::MAX as f64 {
        return None;
    }
    // Do not round: `0.1` is not a byte and `1023.5` is not a power of two.
    // Fractional byte counts are refused rather than silently rounded.
    if bytes < 1.0 || bytes.fract() != 0.0 {
        return None;
    }
    Some(bytes as u64)
}

/// Validate a zvol size (`volsize` on create, the target size on resize).
pub fn validate_volsize(raw: &str) -> ZfsResult<u64> {
    let value = raw.trim();
    if value.is_empty() {
        return Err(invalid!("zvol size is required (for example 10G)"));
    }
    parse_size_bytes(value)
        .ok_or_else(|| invalid!("invalid zvol size {:?}: use a size such as 10G", value))
}

/// Validate `volblocksize`: a power of two from 512 bytes to 16 MiB.
pub fn validate_volblocksize(raw: &str) -> ZfsResult<u64> {
    let value = raw.trim();
    let bytes = parse_size_bytes(value).ok_or_else(|| {
        invalid!(
            "invalid volblocksize {:?}: use a power of two from 512 to 16M",
            value
        )
    })?;
    if !(MIN_VOLBLOCKSIZE..=MAX_VOLBLOCKSIZE).contains(&bytes) || !bytes.is_power_of_two() {
        return Err(invalid!(
            "invalid volblocksize {:?}: use a power of two from 512 to 16M",
            value
        ));
    }
    Ok(bytes)
}

/// Validate the properties a zvol is created with.
pub fn validate_zvol_options(options: &HashMap<String, String>) -> ZfsResult<()> {
    for (key, raw) in options {
        let value = raw.trim();
        match key.as_str() {
            "volblocksize" => {
                validate_volblocksize(value)?;
            }
            "compression" => {
                if !valid_compression(value) {
                    return Err(invalid!("unsupported compression {:?}", value));
                }
            }
            "volsize" => {
                return Err(invalid!(
                    "set the size with the size field, not a volsize option"
                ))
            }
            k if is_dataset_option_key(k) => {
                return Err(invalid!(
                    "{k} is a dataset option; it cannot be set on a zvol"
                ))
            }
            _ => return Err(invalid!("unsupported zvol option {:?}", key)),
        }
    }
    Ok(())
}

impl ZfsClient {
    /// Every zvol on the node.
    pub async fn all_zvols(&self) -> ZfsResult<Vec<Zvol>> {
        self.zvol_rows(None).await
    }

    /// zvols in one pool.
    pub async fn zvols(&self, pool: &str) -> ZfsResult<Vec<Zvol>> {
        validate_pool_name(pool)?;
        self.zvol_rows(Some(pool)).await
    }

    async fn zvol_rows(&self, pool: Option<&str>) -> ZfsResult<Vec<Zvol>> {
        let mut argv: Vec<String> = super::dataset::strings(&[
            "list",
            "-H",
            "-p",
            "-o",
            "name,volsize,volblocksize,used,avail,refer,compression",
            "-t",
            "volume",
            "-r",
        ]);
        if let Some(pool) = pool {
            argv.push(pool.to_string());
        }

        let out = match self.host_exec(ZFS_BIN, &argv).await {
            Ok(out) => out,
            Err(e) => return Err(ZfsError::Other(format!("listing zvols: {}", e.message))),
        };

        let mut zvols = Vec::new();
        for line in out.trim().split('\n') {
            if line.is_empty() {
                continue;
            }
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() < 7 {
                continue;
            }
            let volsize_bytes: i64 = fields[1].parse().unwrap_or(0);
            let used_bytes: i64 = fields[3].parse().unwrap_or(0);
            let avail_bytes: i64 = fields[4].parse().unwrap_or(0);
            let refer_bytes: i64 = fields[5].parse().unwrap_or(0);
            zvols.push(Zvol {
                name: fields[0].to_string(),
                volsize: human_bytes(volsize_bytes),
                volsize_bytes,
                volblocksize: fields[2].to_string(),
                compression: fields[6].to_string(),
                used: human_bytes(used_bytes),
                avail: human_bytes(avail_bytes),
                refer: human_bytes(refer_bytes),
            });
        }
        Ok(zvols)
    }

    /// Create a zvol: `zfs create -p -V <validated size> [-o …] <name>`.
    pub async fn create_zvol(
        &self,
        name: &str,
        size: &str,
        options: &HashMap<String, String>,
    ) -> ZfsResult<()> {
        validate_new_dataset_path(name)?;
        validate_volsize(size)?;
        validate_zvol_options(options)?;
        let _guard = self.lock_dataset_mutation(name).await;

        let mut argv: Vec<String> = vec![
            "create".into(),
            "-p".into(),
            "-V".into(),
            size.trim().to_string(),
        ];
        let mut keys: Vec<&String> = options.keys().collect();
        keys.sort();
        for k in keys {
            argv.push("-o".into());
            argv.push(format!("{}={}", k, options[k].trim()));
        }
        argv.push(name.to_string());

        match self.host_exec(ZFS_BIN, &argv).await {
            Ok(_) => Ok(()),
            Err(e) => Err(ZfsError::Other(format!(
                "creating zvol: {}: {}",
                e.output.trim(),
                e.message
            ))),
        }
    }

    /// Resize a zvol: `zfs set volsize=<validated size> <name>`.
    ///
    /// Growth only. A shrink can corrupt the filesystem inside the volume and
    /// is refused. A consumer the agent can see (mounted device, sysfs holder,
    /// open host file descriptor) also refuses the resize, because an online
    /// size change cannot rescan that consumer for it.
    pub async fn resize_zvol(&self, name: &str, size: &str) -> ZfsResult<()> {
        validate_dataset_path(name)?;
        let wanted = validate_volsize(size)?;
        let _guard = self.lock_dataset_mutation(name).await;
        self.require_zvol(name).await?;
        if let Some(reason) = zvol_in_use(&self.host_root, name) {
            return Err(conflict!(
                "zvol {name} is in use ({reason}); stop the consumer before resizing it"
            ));
        }

        let current = match self
            .host_exec(
                ZFS_BIN,
                &super::dataset::strings(&["list", "-H", "-p", "-o", "volsize", name]),
            )
            .await
        {
            Ok(out) => out
                .trim()
                .parse::<u64>()
                .map_err(|_| invalid!("cannot read the current size of zvol {name}"))?,
            Err(e) => return Err(invalid!("{name} is not a zvol: {}", e.output.trim())),
        };
        if wanted < current {
            return Err(invalid!(
                "shrinking a zvol is not supported ({current} -> {wanted} bytes); it can corrupt the filesystem in the volume"
            ));
        }
        if wanted == current {
            return Ok(());
        }

        match self
            .host_exec(
                ZFS_BIN,
                &super::dataset::strings(&["set", &format!("volsize={}", size.trim()), name]),
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(e) if is_zvol_busy(&e.output) => {
                Err(conflict!("zvol {name} is in use: {}", e.output.trim()))
            }
            Err(e) => Err(ZfsError::Other(format!(
                "resizing zvol: {}: {}",
                e.output.trim(),
                e.message
            ))),
        }
    }

    /// Destroy a zvol. Never destroys a filesystem/snapshot of the same name
    /// and never retries a busy destroy (unlike the dataset stale-mount
    /// ladder): a busy zvol has a live consumer, which must be stopped first.
    pub async fn destroy_zvol(&self, name: &str) -> ZfsResult<()> {
        validate_dataset_path(name)?;
        let _guard = self.lock_dataset_mutation(name).await;
        self.require_zvol(name).await?;
        if let Some(reason) = zvol_in_use(&self.host_root, name) {
            return Err(conflict!(
                "zvol {name} is in use ({reason}); stop the consumer before deleting it"
            ));
        }

        match self
            .host_exec(ZFS_BIN, &super::dataset::strings(&["destroy", name]))
            .await
        {
            Ok(_) => Ok(()),
            Err(e) if is_zvol_busy(&e.output) => {
                Err(conflict!("zvol {name} is in use: {}", e.output.trim()))
            }
            Err(e) => Err(ZfsError::Other(format!(
                "destroying zvol: {}: {}",
                e.output.trim(),
                e.message
            ))),
        }
    }

    /// Refuse to operate on a path that is not a zvol, so `destroy`/`set`
    /// cannot touch a filesystem dataset with the same name.
    async fn require_zvol(&self, name: &str) -> ZfsResult<()> {
        let out = match self
            .host_exec(
                ZFS_BIN,
                &super::dataset::strings(&["list", "-H", "-o", "type", name]),
            )
            .await
        {
            Ok(out) => out,
            Err(e) => return Err(invalid!("{name} is not a zvol: {}", e.output.trim())),
        };
        if out.trim() != "volume" {
            return Err(invalid!("{name} is not a zvol"));
        }
        Ok(())
    }
}

fn is_zvol_busy(output: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    lower.contains("volume is busy")
        || lower.contains("dataset is busy")
        || lower.contains("is in use")
}

/// Best-effort detection of a zvol consumer visible from the agent.
///
/// ZFS itself is the final authority for a busy destroy, but a resize needs a
/// pre-check: `zfs set volsize` succeeds while a consumer holds the device.
/// The agent has no host PID namespace, so it looks at:
///
/// 1. `/dev/zvol[/dsk]/<name>` resolution and the host/agent mount tables,
/// 2. sysfs holders (device-mapper, md) for the resulting `zdN` device,
/// 3. host or agent `/proc/<pid>/fd` links, when that proc tree is visible.
///
/// Anything not visible here (for example an iSCSI daemon with a private
/// device handle) is still refused by ZFS on destroy and is tracked as the
/// `[OPEN]` consumer-registration requirement in `docs/iscsi.md`.
pub fn zvol_in_use(host_root: &Path, name: &str) -> Option<String> {
    let candidates = [format!("/dev/zvol/{name}"), format!("/dev/zvol/dsk/{name}")];
    let mut devices: Vec<String> = Vec::new();
    let mut paths: Vec<String> = Vec::new();
    for candidate in &candidates {
        paths.push(candidate.clone());
        if let Ok(resolved) = std::fs::canonicalize(join_host(host_root, candidate)) {
            if let Ok(relative) = resolved.strip_prefix(host_root) {
                paths.push(format!("/{}", relative.to_string_lossy()));
            }
            if let Some(base) = resolved.file_name().and_then(|f| f.to_str()) {
                if !devices.iter().any(|d| d == base) {
                    devices.push(base.to_string());
                }
            }
        }
    }
    // A mounted or opened partition is a separate node (`zdNpM`) under the
    // zvol's sysfs block directory; the whole-disk name alone misses it.
    let mut inspected = devices.clone();
    let mut holder_dirs: Vec<PathBuf> = devices
        .iter()
        .map(|device| host_root.join("sys/block").join(device).join("holders"))
        .collect();
    for device in &devices {
        let block = host_root.join("sys/block").join(device);
        if let Ok(entries) = std::fs::read_dir(block) {
            for entry in entries.flatten() {
                let partition = entry.file_name().to_string_lossy().to_string();
                if partition.len() > device.len() && partition.starts_with(device.as_str()) {
                    paths.push(format!("/dev/{partition}"));
                    // A dm/md/LUKS holder of a partition lives beside the
                    // partition node (`zdN/zdNpM/holders`), not under the
                    // whole-disk `zdN/holders` directory.
                    holder_dirs.push(entry.path().join("holders"));
                    inspected.push(partition);
                }
            }
        }
    }
    for device in &devices {
        paths.push(format!("/dev/{device}"));
    }

    for mounts in [host_root.join("proc/mounts"), PathBuf::from("/proc/mounts")] {
        let Ok(contents) = std::fs::read_to_string(mounts) else {
            continue;
        };
        for line in contents.lines() {
            let mut fields = line.split_whitespace();
            let Some(device) = fields.next() else {
                continue;
            };
            let mountpoint = fields.next().unwrap_or("an unknown path");
            if paths.iter().any(|p| p == device) {
                return Some(format!("mounted at {mountpoint}"));
            }
        }
    }

    for holders in holder_dirs {
        if let Ok(entries) = std::fs::read_dir(holders) {
            if let Some(entry) = entries.flatten().next() {
                return Some(format!("held by {}", entry.file_name().to_string_lossy()));
            }
        }
    }

    for proc_root in [host_root.join("proc"), PathBuf::from("/proc")] {
        if let Some(pid) = open_by_process(&proc_root, &inspected) {
            return Some(format!("open by process {pid}"));
        }
    }
    None
}

fn open_by_process(proc_root: &Path, devices: &[String]) -> Option<String> {
    let entries = std::fs::read_dir(proc_root).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        else {
            continue;
        };
        let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            let Ok(target) = std::fs::read_link(fd.path()) else {
                continue;
            };
            let target = target.to_string_lossy();
            if devices.iter().any(|d| target.ends_with(&format!("/{d}"))) {
                return Some(pid.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sizes_with_binary_units() {
        assert_eq!(parse_size_bytes("512"), Some(512));
        assert_eq!(parse_size_bytes("10G"), Some(10 * 1024u64.pow(3)));
        assert_eq!(
            parse_size_bytes("1.5T"),
            Some((1.5 * 1024f64.powi(4)) as u64)
        );
        assert_eq!(parse_size_bytes("0"), None);
        assert_eq!(parse_size_bytes("10g"), None);
        assert_eq!(parse_size_bytes("10GB"), None);
    }

    #[test]
    fn volblocksize_is_a_power_of_two_in_range() {
        for ok in ["512", "4K", "16K", "128K", "1M", "16M"] {
            assert!(validate_volblocksize(ok).is_ok(), "{ok}");
        }
        for bad in ["", "256", "3M", "17M", "32M", "1G", "lots"] {
            assert!(validate_volblocksize(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn zvol_options_split_dataset_and_zvol_surfaces() {
        assert!(validate_zvol_options(&HashMap::from([
            ("compression".into(), "zstd".into()),
            ("volblocksize".into(), "16K".into()),
        ]))
        .is_ok());

        for dataset_only in ["quota", "recordsize", "atime", "copies", "readonly"] {
            let err = validate_zvol_options(&HashMap::from([(
                dataset_only.to_string(),
                "on".to_string(),
            )]))
            .expect_err(dataset_only);
            assert!(err.to_string().contains("dataset option"), "{err}");
        }

        let err = validate_zvol_options(&HashMap::from([("volsize".into(), "10G".into())]))
            .expect_err("volsize option");
        assert!(err.to_string().contains("size field"), "{err}");

        let err = validate_zvol_options(&HashMap::from([("mountpoint".into(), "/".into())]))
            .expect_err("unknown option");
        assert!(err.to_string().contains("unsupported zvol option"), "{err}");
    }
}
