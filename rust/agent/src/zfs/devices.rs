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

/// Resolve a device path to its canonical `/dev/<node>` name.
///
/// This is the guard against `/dev//sdb`, `/dev/disk/by-id/...` and
/// `/dev/mapper/...` aliases: ownership is tracked by the real device node, so
/// an alias can no longer hide a disk that already belongs to a pool.
pub(crate) fn canonical_dev(host_root: &std::path::Path, dev: &str) -> ZfsResult<String> {
    let host = join_host(host_root, dev);
    let resolved =
        std::fs::canonicalize(&host).map_err(|_| invalid!("disk {} not found on the node", dev))?;
    let dev_dir = std::fs::canonicalize(join_host(host_root, "/dev"))
        .map_err(|e| ZfsError::Other(format!("resolving /dev: {e}")))?;
    if !resolved.starts_with(&dev_dir) || resolved == dev_dir {
        return Err(invalid!(
            "disk {} does not resolve to a device under /dev",
            dev
        ));
    }
    let name = resolved
        .file_name()
        .ok_or_else(|| invalid!("disk {} has no device name", dev))?;
    Ok(format!("/dev/{}", name.to_string_lossy()))
}

/// Require an absolute `/dev` path whose device exists on the node, and return
/// its canonical device name.
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
    let canonical = canonical_dev(host_root, dev)?;
    if !is_whole_disk(canonical.trim_start_matches("/dev/")) {
        return Err(invalid!("disk {} is a partition, not a whole disk", dev));
    }
    Ok(canonical)
}

/// `isWholeDisk`: sda/vdb/nvme0n1 are disks; partitions and loop/sr/zram are not.
pub fn is_whole_disk(name: &str) -> bool {
    if name.starts_with("nvme") {
        // nvme0n1 is a disk; nvme0n1p1 is a partition.
        return !name.contains('p');
    }
    if name.starts_with("mmcblk") {
        // eMMC boot/RPMB areas are block devices but not wipeable disks.
        if name.contains("boot") || name.contains("rpmb") {
            return false;
        }
        return !name.contains('p');
    }
    if name.starts_with("sd") || name.starts_with("vd") || name.starts_with("hd") {
        // sda/vdb are disks; sda1/vdb12 are partitions.
        let rest = &name[1..];
        return !rest.chars().any(|c| c.is_ascii_digit());
    }
    false
}

/// Map a partition device (`/dev/sdc2`, `/dev/nvme0n1p1`) to its whole disk.
///
/// Pool membership is recorded for the partition that carries the ZFS label,
/// but the disk wizard selects whole disks. Without this mapping a pool whose
/// vdev is `/dev/sdc2` would not reserve `/dev/sdc`, and a create could wipe
/// the pool's label.
pub(crate) fn whole_disk_path(dev: &str) -> Option<String> {
    let name = dev.strip_prefix("/dev/")?;
    if name.starts_with("nvme") || name.starts_with("mmcblk") {
        let pos = name.rfind('p')?;
        let base = &name[..pos];
        if base.chars().last().is_some_and(|c| c.is_ascii_digit()) {
            return Some(format!("/dev/{base}"));
        }
        return None;
    }
    if name.starts_with("sd") || name.starts_with("vd") || name.starts_with("hd") {
        let base = name.trim_end_matches(|c: char| c.is_ascii_digit());
        if base.len() < name.len() && base.len() > 2 {
            return Some(format!("/dev/{base}"));
        }
    }
    None
}

/// Whether `candidate` is `disk` itself or one of its partitions.
///
/// The naming convention is the fallback for a node whose sysfs tree is not
/// readable: `sda` owns `sda1`, `nvme0n1` owns `nvme0n1p1`.
fn is_partition_of(disk: &str, candidate: &str) -> bool {
    if candidate == disk {
        return true;
    }
    let rest = candidate.strip_prefix(disk).unwrap_or_default();
    if disk.starts_with("nvme") || disk.starts_with("mmcblk") {
        return rest
            .strip_prefix('p')
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
    }
    !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
}

/// The disk itself plus its partitions, from `/sys/class/block/<disk>/*`.
///
/// The authoritative set is the kernel's sysfs tree; a partition listed there
/// is checked even when its device node name does not follow the usual
/// convention. The naming fallback keeps the guard useful when sysfs is not
/// mounted into the agent.
fn disk_and_partitions(host_root: &std::path::Path, disk: &str) -> Vec<String> {
    let mut devices = vec![disk.to_string()];
    for dir in ["sys/class/block", "sys/block"] {
        let Ok(entries) = std::fs::read_dir(host_root.join(dir).join(disk)) else {
            continue;
        };
        for entry in entries.flatten() {
            let candidate = entry.file_name().to_string_lossy().into_owned();
            if is_partition_of(disk, &candidate) && !devices.contains(&candidate) {
                devices.push(candidate);
            }
        }
        break;
    }
    devices
}

/// The host's mount table. Prefer the host root's own `/proc/mounts`; the
/// agent's view is only a fallback for a node where `/host/proc` is not
/// mounted. An existing but empty host table is taken at face value.
fn host_mounts(host_root: &std::path::Path) -> Vec<(String, String)> {
    let host_table = host_root.join("proc/mounts");
    let tables = if host_table.exists() {
        vec![host_table]
    } else {
        vec![std::path::PathBuf::from("/proc/mounts")]
    };
    let mut mounts = Vec::new();
    for table in tables {
        let Ok(contents) = std::fs::read_to_string(&table) else {
            continue;
        };
        for line in contents.lines() {
            let mut fields = line.split_whitespace();
            let Some(source) = fields.next() else {
                continue;
            };
            let mountpoint = fields.next().unwrap_or("");
            mounts.push((source.to_string(), mountpoint.to_string()));
        }
        break;
    }
    mounts
}

/// Whether a mount source names `disk` or one of its partitions. Aliases
/// (`/dev/disk/by-id/…`, `/dev/mapper/…`) are resolved inside the host root
/// so they cannot hide a mounted system partition.
fn source_is_disk(host_root: &std::path::Path, source: &str, disk: &str) -> bool {
    let Some(name) = source.strip_prefix("/dev/") else {
        // UUID=/PARTUUID=/overlay/tmpfs: no device path to compare.
        return false;
    };
    if name.is_empty() {
        return false;
    }
    let base = name.rsplit('/').next().unwrap_or(name);
    if is_partition_of(disk, base) {
        return true;
    }
    match std::fs::canonicalize(join_host(host_root, source)) {
        Ok(resolved) => resolved
            .file_name()
            .and_then(|f| f.to_str())
            .is_some_and(|base| is_partition_of(disk, base)),
        Err(_) => false,
    }
}

/// Why a whole disk must not be handed to ZFS, or `None` when nothing
/// visible uses it.
///
/// The API excludes the Talos system disk when it lists candidates, but the
/// agent is the last line of defense: a bug or a direct API call could
/// otherwise `wipefs --all` the disk the node boots from. A disk is refused
/// when the host's mount table shows any of its partitions (or the disk
/// itself) mounted, when a partition backs the host root, or when a
/// device-mapper/md holder claims one of its partitions.
pub fn disk_in_use(host_root: &std::path::Path, disk: &str) -> Option<String> {
    let Some(name) = disk.strip_prefix("/dev/") else {
        return Some("it is not a /dev block device".to_string());
    };
    if name.is_empty() || name.contains('/') {
        return Some("it is not a plain /dev block device name".to_string());
    }
    let devices = disk_and_partitions(host_root, name);
    for (source, mountpoint) in host_mounts(host_root) {
        if devices
            .iter()
            .any(|device| source_is_disk(host_root, &source, device))
        {
            if mountpoint == "/" {
                return Some(
                    "it backs the host root filesystem (/) or one of its system partitions"
                        .to_string(),
                );
            }
            return Some(format!("it is mounted at {mountpoint}"));
        }
    }

    // LVM/LUKS/md can layer a holder over a partition. A holder means a live
    // consumer even when the mount source no longer names the partition.
    for device in &devices {
        for dir in ["sys/class/block", "sys/block"] {
            let Ok(entries) = std::fs::read_dir(host_root.join(dir).join(device).join("holders"))
            else {
                continue;
            };
            if let Some(holder) = entries.flatten().next() {
                return Some(format!(
                    "it is held by the {} device-mapper/md device",
                    holder.file_name().to_string_lossy()
                ));
            }
        }
    }
    None
}

impl ZfsClient {
    /// Map every disk currently in a pool to that pool's name.
    ///
    /// Member names are canonicalized so an alias on either side
    /// (`zpool status` reporting `by-id`, the request using `/dev/sdb`) still
    /// matches.
    pub async fn pool_members(&self) -> ZfsResult<std::collections::HashMap<String, String>> {
        self.collect_members(self.pools().await?).await
    }

    /// Like `pool_members`, but fails if any pool's `zpool status` could not be
    /// read. Destructive operations must use this: a suspended/UNAVAIL pool is
    /// exactly the state in which `zpool status` fails, and its disks are
    /// obviously still in use.
    pub async fn pool_members_strict(
        &self,
    ) -> ZfsResult<std::collections::HashMap<String, String>> {
        let pools = self.pools_strict().await?;
        self.collect_members(pools).await
    }

    async fn collect_members(
        &self,
        pools: Vec<super::types::Pool>,
    ) -> ZfsResult<std::collections::HashMap<String, String>> {
        let mut members = std::collections::HashMap::new();
        for pool in pools {
            if let Some(disks) = pool.disks {
                for disk in disks {
                    let key = canonical_dev(&self.host_root, &disk).unwrap_or(disk);
                    if let Some(whole) = whole_disk_path(&key) {
                        members.entry(whole).or_insert_with(|| pool.name.clone());
                    }
                    members.insert(key, pool.name.clone());
                }
            }
        }
        Ok(members)
    }

    /// Disks that belong to an *exported* pool (`zpool import` dry run).
    pub async fn importable_members(&self) -> ZfsResult<std::collections::HashMap<String, String>> {
        let pools = self.list_importable().await?;
        let mut members = std::collections::HashMap::new();
        for pool in pools {
            for disk in pool.disks.unwrap_or_default() {
                let key = canonical_dev(&self.host_root, &disk).unwrap_or(disk);
                if let Some(whole) = whole_disk_path(&key) {
                    members.entry(whole).or_insert_with(|| pool.name.clone());
                }
                members.insert(key, pool.name.clone());
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
            // Real disk nodes are block devices; `is_file()` alone made
            // free_disks always return an empty list (SEC-2026-H20). Regular
            // files are accepted too so the hermetic test fixture can create
            // /dev/sd* without mknod (a real /dev has no such regular files).
            match entry.file_type() {
                Ok(ft) if std::os::unix::fs::FileTypeExt::is_block_device(&ft) || ft.is_file() => {}
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
            return Err(ZfsError::Validation("at least one disk is required".into()));
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
        if self
            .host_exec(ZPOOL_BIN, &args2("list", pool))
            .await
            .is_err()
        {
            return Err(invalid!("pool {:?} not found", pool));
        }

        let _guard = self.mutation_lock.lock().await;
        let members = self.pool_members_strict().await?;
        let importable = self.importable_members().await?;
        let clean = self.normalize_disk_set(disks, &members)?;
        for dev in &clean {
            if let Some(owner) = importable.get(dev) {
                return Err(invalid!(
                    "disk {} belongs to exported pool {:?}; import or destroy that pool first",
                    dev,
                    owner
                ));
            }
        }
        // The agent refuses a mounted/system disk even when the API's
        // candidate list is wrong (defense in depth; `zpool add` writes
        // labels just like create).
        for dev in &clean {
            if let Some(reason) = disk_in_use(&self.host_root, dev) {
                return Err(crate::conflict!(
                    "refusing to add {dev} to pool {pool}: {reason}"
                ));
            }
        }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_disk_mapping_reserves_partition_owners() {
        assert_eq!(whole_disk_path("/dev/sdc2").as_deref(), Some("/dev/sdc"));
        assert_eq!(
            whole_disk_path("/dev/nvme0n1p1").as_deref(),
            Some("/dev/nvme0n1")
        );
        assert_eq!(
            whole_disk_path("/dev/mmcblk0p1").as_deref(),
            Some("/dev/mmcblk0")
        );
        assert_eq!(whole_disk_path("/dev/mmcblk0boot0"), None);
        assert_eq!(whole_disk_path("/dev/mmcblk0rpmb"), None);
        assert_eq!(whole_disk_path("/dev/sdc"), None);
        assert_eq!(whole_disk_path("/dev/zd32"), None);
    }

    #[test]
    fn emmc_boot_and_rpmb_are_not_whole_disks() {
        assert!(!is_whole_disk("mmcblk0boot0"));
        assert!(!is_whole_disk("mmcblk0boot1"));
        assert!(!is_whole_disk("mmcblk0rpmb"));
        assert!(is_whole_disk("mmcblk0"));
        assert!(!is_whole_disk("mmcblk0p1"));
    }
    #[test]
    fn whole_disk_paths_match_shared_fixture() {
        for line in include_str!("../../../testdata/zfs_device_paths.txt").lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (input, expected) = line
                .split_once('|')
                .expect("fixture line is input|expected");
            let want = match expected.trim() {
                "" => None,
                value => Some(value.to_string()),
            };
            assert_eq!(whole_disk_path(input.trim()), want, "for {input}");
        }
    }
}
