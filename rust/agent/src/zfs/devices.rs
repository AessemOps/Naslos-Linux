//! Pool-name/topology/disk validation and vdev attachment (port of
//! `zfs/devices.go`).

use super::{join_host, ZfsClient, ZPOOL_BIN};
use crate::invalid;
use crate::zfs::validation::{ZfsError, ZfsResult};
use std::collections::HashSet;

/// ZFS-safe pool charset: must start alphanumerically.
pub fn is_valid_pool_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
}

/// Reject a pool name ZFS would refuse or that could be confused with a path.
pub fn validate_pool_name(name: &str) -> ZfsResult<()> {
    if name.trim().is_empty() {
        return Err(ZfsError::Validation("pool name is required".into()));
    }
    if !is_valid_pool_name(name) {
        return Err(invalid!(
            "invalid pool name {:?}: must start with a letter or digit and contain only letters, digits, '.', '_', ':' or '-'",
            name
        ));
    }
    Ok(())
}

fn is_vdev_topology(t: &str) -> bool {
    matches!(
        t,
        "" | "single" | "stripe" | "mirror" | "raidz" | "raidz1" | "raidz2" | "raidz3"
    )
}

fn normalize_topology(topology: &str) -> &str {
    if topology == "stripe" || topology.is_empty() {
        "single"
    } else {
        topology
    }
}

/// Lowercase/trim a topology and reject unknown ones; `""`/`stripe` become
/// `single`.
pub fn normalize_vdev_topology(topology: &str) -> ZfsResult<String> {
    let norm = topology.trim().to_ascii_lowercase();
    if !is_vdev_topology(&norm) {
        return Err(invalid!(
            "unsupported topology {:?} (supported: single, mirror, raidz1, raidz2, raidz3)",
            topology
        ));
    }
    Ok(normalize_topology(&norm).to_string())
}

/// ZFS's own minimum disk counts per topology.
pub fn vdev_minimum_disks(topology: &str) -> usize {
    match topology {
        "mirror" | "raidz" | "raidz1" => 2,
        "raidz2" => 3,
        "raidz3" => 4,
        _ => 1,
    }
}

/// Topology name for user-facing messages.
pub fn vdev_label(topology: &str) -> &'static str {
    match topology {
        "mirror" => "a mirror",
        "raidz" | "raidz1" => "raidz1",
        "raidz2" => "raidz2",
        "raidz3" => "raidz3",
        _ => "a single-disk vdev",
    }
}

/// Require an absolute `/dev` path whose device exists on the node.
fn normalize_disk_path(host_root: &std::path::Path, disk: &str) -> ZfsResult<String> {
    let dev = disk.trim();
    if dev.is_empty() {
        return Err(ZfsError::Validation("disk path is required".into()));
    }
    if !dev.starts_with("/dev/") {
        return Err(invalid!(
            "disk {:?} must be an absolute path under /dev",
            disk
        ));
    }
    if dev.contains("..") || dev.contains('\n') || dev.contains('\r') || dev.contains('\t') {
        return Err(invalid!("invalid disk path {:?}", disk));
    }
    if !join_host(host_root, dev).exists() {
        return Err(invalid!("disk {} not found on the node", dev));
    }
    Ok(dev.to_string())
}

/// `isWholeDisk`: sda/vdb/nvme0n1 are disks; partitions and loop/sr/zram are not.
pub fn is_whole_disk(name: &str) -> bool {
    if name.starts_with("nvme") {
        // nvme0n1 is a disk; nvme0n1p1 is a partition.
        return !name.contains('p');
    }
    if name.starts_with("sd") || name.starts_with("vd") || name.starts_with("hd") {
        // sda/vdb are disks; sda1/vdb12 are partitions.
        let rest = &name[1..];
        return !rest.chars().any(|c| c.is_ascii_digit());
    }
    false
}

impl ZfsClient {
    /// Map every disk currently in a pool to that pool's name.
    pub async fn pool_members(&self) -> ZfsResult<std::collections::HashMap<String, String>> {
        let pools = self.pools().await?;
        let mut members = std::collections::HashMap::new();
        for pool in pools {
            if let Some(disks) = pool.disks {
                for disk in disks {
                    members.insert(disk, pool.name.clone());
                }
            }
        }
        Ok(members)
    }

    /// Whole block devices on the node that belong to no pool, sorted.
    pub async fn free_disks(&self) -> ZfsResult<Vec<String>> {
        let members = self.pool_members().await?;

        let dev_dir = self.host_path("/dev");
        let entries = std::fs::read_dir(&dev_dir)
            .map_err(|e| ZfsError::Other(format!("reading /dev: {e}")))?;

        let mut free = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !is_whole_disk(&name) {
                continue;
            }
            let dev = format!("/dev/{name}");
            if members.contains_key(&dev) {
                continue;
            }
            match entry.file_type() {
                Ok(ft) if ft.is_file() => {}
                _ => continue,
            }
            free.push(dev);
        }
        free.sort();
        Ok(free)
    }

    /// Validate a set of disks for a destructive operation.
    fn normalize_disk_set(
        &self,
        disks: &[String],
        members: &std::collections::HashMap<String, String>,
    ) -> ZfsResult<Vec<String>> {
        let mut clean = Vec::with_capacity(disks.len());
        let mut seen: HashSet<String> = HashSet::new();
        for disk in disks {
            let dev = normalize_disk_path(&self.host_root, disk)?;
            if seen.contains(&dev) {
                return Err(invalid!("disk {} was given more than once", dev));
            }
            seen.insert(dev.clone());
            if let Some(owner) = members.get(&dev) {
                return Err(invalid!("disk {} already belongs to pool {:?}", dev, owner));
            }
            clean.push(dev);
        }
        Ok(clean)
    }

    /// Attach one vdev (a group of disks in a topology) to an existing pool.
    pub async fn add_vdev(
        &self,
        pool: &str,
        topology: &str,
        disks: &[String],
        force: bool,
    ) -> ZfsResult<()> {
        validate_pool_name(pool)?;
        let norm = normalize_vdev_topology(topology)?;
        if disks.is_empty() {
            return Err(ZfsError::Validation(
                "at least one disk is required".into(),
            ));
        }
        let min = vdev_minimum_disks(&norm);
        if disks.len() < min {
            return Err(invalid!(
                "adding {} needs at least {} disks, got {}",
                vdev_label(&norm),
                min,
                disks.len()
            ));
        }

        // The pool has to exist first.
        if self.host_exec(ZPOOL_BIN, &args2("list", pool)).await.is_err() {
            return Err(invalid!("pool {:?} not found", pool));
        }

        let members = self.pool_members().await?;
        let clean = self.normalize_disk_set(disks, &members)?;

        let mut argv: Vec<String> = vec!["add".into()];
        if force {
            argv.push("-f".into());
        }
        argv.push(pool.to_string());
        if norm != "single" {
            argv.push(norm.clone());
        }
        argv.extend(clean);

        match self.host_exec(ZPOOL_BIN, &argv).await {
            Ok(_) => Ok(()),
            Err(e) => Err(ZfsError::Other(format!(
                "adding {} to pool {}: {}: {}",
                vdev_label(&norm),
                pool,
                e.output.trim(),
                e.message
            ))),
        }
    }

    /// Public wrapper used by `CreatePool` for cache-device validation.
    pub(crate) fn normalize_disk_set_for_host(
        &self,
        disks: &[String],
        members: &std::collections::HashMap<String, String>,
    ) -> ZfsResult<Vec<String>> {
        self.normalize_disk_set(disks, members)
    }
}

/// `[a, b]` as `Vec<String>`.
pub(crate) fn args2(a: &str, b: &str) -> Vec<String> {
    vec![a.to_string(), b.to_string()]
}
