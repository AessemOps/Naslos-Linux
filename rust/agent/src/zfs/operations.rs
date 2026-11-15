//! Pool listing, creation, import/export and health (port of
//! `zfs/operations.go`).

use super::devices::{
    args2, canonical_dev, is_whole_disk, normalize_vdev_topology, validate_pool_name, vdev_label,
    vdev_minimum_disks, whole_disk_path,
};
use super::types::{
    ImportablePool, Pool, PoolConfig, PoolDevice, PoolHealth, PoolIOStats, ZfsDiskHealth,
};
use super::{args, ZfsClient, WIPEFS_BIN, ZFS_BIN, ZPOOL_BIN};
use crate::invalid;
use crate::zfs::validation::{ZfsError, ZfsResult};
use std::collections::HashMap;
use std::path::Path;

/// ZFS best-practice dataset options for Talos.
pub fn default_options() -> std::collections::HashMap<String, String> {
    [
        ("xattr", "sa"),
        ("compression", "zstd"),
        ("acltype", "posixacl"),
        ("atime", "off"),
        ("dnodesize", "auto"),
        ("relatime", "on"),
        ("recordsize", "128K"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

impl ZfsClient {
    /// All pools with their member disks. A pool whose status cannot be read
    /// is still listed, without its disks (display path only — use
    /// `pools_strict` before acting on the result).
    pub async fn pools(&self) -> ZfsResult<Vec<Pool>> {
        let mut pools = self.pool_rows().await?;
        for pool in &mut pools {
            // Parse `zpool status` for member disks so the disk wizard can mark
            // disks already in a pool.
            if let Ok(disks) = self.pool_disks(&pool.name).await {
                pool.disks = if disks.is_empty() { None } else { Some(disks) };
            }
        }
        Ok(pools)
    }

    /// Every pool with its members; fails if any `zpool status` fails.
    pub async fn pools_strict(&self) -> ZfsResult<Vec<Pool>> {
        let mut pools = self.pool_rows().await?;
        for pool in &mut pools {
            let disks = self.pool_disks(&pool.name).await?;
            pool.disks = if disks.is_empty() { None } else { Some(disks) };
        }
        Ok(pools)
    }

    /// `zpool list` rows, without per-pool status.
    async fn pool_rows(&self) -> ZfsResult<Vec<Pool>> {
        let out = match self
            .host_exec(
                ZPOOL_BIN,
                &args(&["list", "-H", "-o", "name,size,alloc,free,health"]),
            )
            .await
        {
            Ok(out) => out,
            Err(e) => return Err(ZfsError::Other(format!("listing pools: {}", e.message))),
        };

        let mut pools = Vec::new();
        for line in out.trim().split('\n') {
            if line.is_empty() {
                continue;
            }
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() < 5 {
                continue;
            }
            let pool = Pool {
                name: fields[0].to_string(),
                size: fields[1].to_string(),
                alloc: fields[2].to_string(),
                free: fields[3].to_string(),
                health: fields[4].to_string(),
                ..Default::default()
            };
            pools.push(pool);
        }
        Ok(pools)
    }

    /// Member devices of a pool, parsed from `zpool status -L`.
    ///
    /// `-L` prints the kernel device name (`sdc2`) rather than the stored
    /// partuuid, so a pool imported by partuuid still yields usable `/dev/…`
    /// paths instead of bare GUIDs (which the parser below discards).
    pub async fn pool_disks(&self, name: &str) -> ZfsResult<Vec<String>> {
        validate_pool_name(name)?;
        let out = match self
            .host_exec(ZPOOL_BIN, &args(&["status", "-L", name]))
            .await
        {
            Ok(out) => out,
            Err(e) => return Err(ZfsError::Other(e.message)),
        };

        let mut disks = Vec::new();
        let mut in_config = false;
        let mut header_seen = false;
        for line in out.split('\n') {
            let trimmed = line.trim();
            if trimmed == "config:" {
                in_config = true;
                continue;
            }
            if !in_config {
                continue;
            }
            if trimmed.is_empty() {
                continue;
            }
            if !header_seen {
                header_seen = true;
                continue;
            }
            let fields: Vec<&str> = trimmed.split_whitespace().collect();
            if fields.len() < 2 {
                continue;
            }
            let mut dev = fields[0].to_string();
            if dev == name || dev.contains('-') || dev.contains(':') {
                continue;
            }
            if !dev.starts_with("/dev/") {
                dev = format!("/dev/{dev}");
            }
            disks.push(dev);
        }
        Ok(disks)
    }

    /// Detailed `zpool status` output.
    pub async fn pool_status(&self, name: &str) -> ZfsResult<String> {
        self.pool_status_with(name, false).await
    }

    /// `zpool status -L`: the config rows name the kernel device (`sdc2`)
    /// instead of the stored GUID/path. Disk membership needs this, because a
    /// pool imported by GUID would otherwise list opaque ids that cannot be
    /// matched to a whole disk.
    async fn pool_status_kernel(&self, name: &str) -> ZfsResult<String> {
        self.pool_status_with(name, true).await
    }

    async fn pool_status_with(&self, name: &str, kernel_names: bool) -> ZfsResult<String> {
        validate_pool_name(name)?;
        let argv = if kernel_names {
            args(&["status", "-L", name])
        } else {
            args2("status", name)
        };
        match self.host_exec(ZPOOL_BIN, &argv).await {
            Ok(out) => Ok(out),
            Err(e) => Err(ZfsError::Other(format!("pool status: {}", e.message))),
        }
    }

    /// Create a pool with best-practice options.
    pub async fn create_pool(&self, cfg: &PoolConfig) -> ZfsResult<()> {
        validate_pool_name(&cfg.name)?;
        let topology = normalize_vdev_topology(&cfg.topology)?;
        super::dataset::validate_dataset_options(&cfg.options)?;
        if cfg.disks.is_empty() {
            return Err(ZfsError::Validation("at least one disk is required".into()));
        }
        let min = vdev_minimum_disks(&topology);
        if cfg.disks.len() < min {
            return Err(invalid!(
                "creating {} needs at least {} disks, got {}",
                vdev_label(&topology),
                min,
                cfg.disks.len()
            ));
        }

        // Serialize the whole check → wipe → create sequence. Without this the
        // existence check, the ownership snapshot and the wipe race each other.
        let _guard = self.mutation_lock.lock().await;

        // Refuse if the pool already exists.
        if self
            .host_exec(ZPOOL_BIN, &args2("list", &cfg.name))
            .await
            .is_ok()
        {
            return Err(invalid!("pool {:?} already exists", cfg.name));
        }

        // Fail closed: if a pool's status cannot be read we do not know whether
        // its disks are free, so we must not wipe anything.
        let members = self.pool_members_strict().await?;
        let importable = self.importable_members().await?;
        let disks = self.normalize_disk_set_for_host(&cfg.disks, &members)?;
        for dev in &disks {
            if let Some(owner) = importable.get(dev) {
                return Err(invalid!(
                    "disk {} belongs to exported pool {:?}; import or destroy that pool first",
                    dev,
                    owner
                ));
            }
        }

        let mut cache = String::new();
        if !cfg.cache.is_empty() {
            let normalized =
                self.normalize_disk_set_for_host(std::slice::from_ref(&cfg.cache), &members)?;
            cache = normalized[0].clone();
            if let Some(owner) = importable.get(&cache) {
                return Err(invalid!(
                    "cache disk {} belongs to exported pool {:?}; import or destroy that pool first",
                    cache,
                    owner
                ));
            }
            if disks.contains(&cache) {
                return Err(invalid!(
                    "disk {} cannot be both a data disk and the cache device",
                    cache
                ));
            }
        }

        // Wipe signatures when wipefs exists; Talos's ZFS extension may not ship it.
        if self.host_bin_exists(WIPEFS_BIN) {
            for disk in &disks {
                if let Err(e) = self.host_exec(WIPEFS_BIN, &args(&["--all", disk])).await {
                    return Err(ZfsError::Other(format!("wiping {}: {}", disk, e.message)));
                }
            }
        }

        let mut argv: Vec<String> = args(&["create", "-f", "-o", "ashift=12"]);
        argv.push(cfg.name.clone());
        match topology.as_str() {
            "mirror" => {
                argv.push("mirror".into());
                argv.extend(disks.clone());
            }
            "raidz1" | "raidz" | "raidz2" | "raidz3" => {
                argv.push(topology.clone());
                argv.extend(disks.clone());
            }
            _ => {
                argv.extend(disks.clone());
            }
        }

        if let Err(e) = self.host_exec(ZPOOL_BIN, &argv).await {
            // `zpool create` may "fail" with a mount error on Talos even though
            // the pool was created; verify rather than trusting the exit code.
            if self
                .host_exec(ZPOOL_BIN, &args2("list", &cfg.name))
                .await
                .is_err()
            {
                return Err(ZfsError::Other(format!(
                    "creating pool: {}: {}",
                    e.output, e.message
                )));
            }
        }

        // Writable mountpoint (Talos root is read-only).
        let mountpoint = format!("/var/mnt/{}", cfg.name);
        let mp_arg = format!("mountpoint={mountpoint}");
        if let Err(e) = self
            .host_exec(ZFS_BIN, &args(&["set", &mp_arg, &cfg.name]))
            .await
        {
            return Err(ZfsError::Other(format!(
                "setting mountpoint: {}",
                e.message
            )));
        }

        // Apply dataset options (defaults merged with the request), sorted so the
        // command sequence is deterministic.
        let mut opts = default_options();
        for (k, v) in &cfg.options {
            // Validate and emit the same trimmed value; validation trims, so
            // passing the raw string made zfs reject `" 100G "`.
            opts.insert(k.clone(), v.trim().to_string());
        }
        let mut keys: Vec<&String> = opts.keys().collect();
        keys.sort();
        for k in keys {
            if k == "mountpoint" {
                continue;
            }
            let set_arg = format!("{}={}", k, opts[k]);
            if let Err(e) = self
                .host_exec(ZFS_BIN, &args(&["set", &set_arg, &cfg.name]))
                .await
            {
                return Err(ZfsError::Other(format!("setting {}: {}", k, e.message)));
            }
        }

        // Disable SELinux contexts (required for Talos).
        if let Err(e) = self
            .host_exec(
                ZFS_BIN,
                &args(&[
                    "set",
                    "context=none",
                    "fscontext=none",
                    "defcontext=none",
                    "rootcontext=none",
                    &cfg.name,
                ]),
            )
            .await
        {
            return Err(ZfsError::Other(format!(
                "disabling SELinux on pool: {}",
                e.message
            )));
        }

        if !cache.is_empty() {
            if let Err(e) = self
                .host_exec(ZPOOL_BIN, &args(&["add", &cfg.name, "cache", &cache]))
                .await
            {
                return Err(ZfsError::Other(format!(
                    "adding cache device {}: {}",
                    cache, e.message
                )));
            }
        }

        Ok(())
    }

    /// Destroy a pool.
    ///
    /// A pool whose datasets are mounted or held open fails with `pool is
    /// busy`; the caller must stop its consumers first. Do not force-unmount
    /// datasets here: that takes shares offline and is not atomic if the
    /// destroy still fails.
    pub async fn destroy_pool(&self, name: &str, force: bool) -> ZfsResult<()> {
        validate_pool_name(name)?;
        let mut argv = vec!["destroy"];
        if force {
            argv.push("-f");
        }
        argv.push(name);
        match self.host_exec(ZPOOL_BIN, &args(&argv)).await {
            Ok(_) => Ok(()),
            Err(e) => Err(ZfsError::Other(format!(
                "destroying pool: {}: {}",
                e.output.trim(),
                e.message
            ))),
        }
    }

    /// Pools that can be imported but are not active (`zpool import` dry run).
    pub async fn list_importable(&self) -> ZfsResult<Vec<ImportablePool>> {
        let out = match self.host_exec(ZPOOL_BIN, &args(&["import"])).await {
            Ok(out) => out,
            Err(e) => {
                // `zpool import` exits non-zero with "no pools available to
                // import" on a clean node; that is not a failure.
                let combined = format!("{} {}", e.output, e.message);
                if combined.to_ascii_lowercase().contains("no pools available") {
                    return Ok(Vec::new());
                }
                return Err(ZfsError::Other(format!(
                    "listing importable pools: {}",
                    combined.trim()
                )));
            }
        };

        let mut pools: Vec<ImportablePool> = Vec::new();
        let mut cur: Option<ImportablePool> = None;
        let mut in_config = false;
        let mut header_seen = false;

        for line in out.split('\n') {
            let trimmed = line.trim();

            if let Some(rest) = trimmed.strip_prefix("pool:") {
                if let Some(c) = cur.take() {
                    pools.push(c);
                }
                cur = Some(ImportablePool {
                    name: rest.trim().to_string(),
                    ..Default::default()
                });
                in_config = false;
                header_seen = false;
                continue;
            }
            let Some(c) = cur.as_mut() else { continue };

            if !in_config {
                if let Some(idx) = trimmed.find(':') {
                    if idx > 0 {
                        let key = trimmed[..idx].trim();
                        let val = trimmed[idx + 1..].trim();
                        if key == "state" {
                            c.state = val.to_string();
                        }
                    }
                }
                if trimmed == "config:" {
                    in_config = true;
                    header_seen = false;
                }
                continue;
            }

            // Inside config: skip empty lines and the header, then parse devices.
            if trimmed.is_empty() {
                if header_seen {
                    in_config = false;
                }
                continue;
            }
            if !header_seen {
                header_seen = true;
                continue;
            }
            let fields: Vec<&str> = trimmed.split_whitespace().collect();
            if fields.len() < 2 {
                continue;
            }
            let mut dev = fields[0].to_string();
            let indent = line.len() - line.trim_start().len();

            if indent <= 1 {
                if dev.starts_with("mirror") {
                    c.topology = "mirror".into();
                } else if dev.starts_with("raidz3") {
                    c.topology = "raidz3".into();
                } else if dev.starts_with("raidz2") {
                    c.topology = "raidz2".into();
                } else if dev.starts_with("raidz") {
                    c.topology = "raidz1".into();
                } else if dev != c.name {
                    c.topology = "single".into();
                }
            } else if !dev.contains('-') && !dev.starts_with(&c.name) {
                if !dev.starts_with("/dev/") {
                    dev = format!("/dev/{dev}");
                }
                c.disks.get_or_insert_with(Vec::new).push(dev);
            }
        }
        if let Some(c) = cur {
            pools.push(c);
        }
        Ok(pools)
    }

    /// Import an existing pool, or every available pool when `name` is empty.
    pub async fn import_pool(&self, name: &str) -> ZfsResult<()> {
        let argv: Vec<String> = if name.is_empty() {
            args(&["import", "-f"])
        } else {
            validate_pool_name(name)?;
            args(&["import", "-f", name])
        };
        match self.host_exec(ZPOOL_BIN, &argv).await {
            Ok(_) => Ok(()),
            Err(e) => Err(ZfsError::Other(format!(
                "importing pool: {}: {}",
                e.output, e.message
            ))),
        }
    }

    /// Export a pool.
    pub async fn export_pool(&self, name: &str) -> ZfsResult<()> {
        validate_pool_name(name)?;
        match self.host_exec(ZPOOL_BIN, &args(&["export", name])).await {
            Ok(_) => Ok(()),
            Err(e) => Err(ZfsError::Other(format!(
                "exporting pool: {}: {}",
                e.output, e.message
            ))),
        }
    }

    /// Structured health from `zpool status` plus `zpool iostat`.
    pub async fn pool_health(&self, name: &str) -> ZfsResult<PoolHealth> {
        let out = self.pool_status(name).await?;

        let mut health = parse_pool_health(name, &out);

        if let Ok(io_out) = self
            .host_exec(ZPOOL_BIN, &args(&["iostat", "-v", name, "1", "1"]))
            .await
        {
            health.io_stats = parse_io_stats(&io_out, name);
        }

        Ok(health)
    }

    /// Per-disk ZFS membership and error counters, keyed by canonical
    /// `/dev/<node>`. Best effort: a pool whose status cannot be read is
    /// skipped, because the SMART half of the health page must still work.
    pub async fn disk_health(&self) -> HashMap<String, ZfsDiskHealth> {
        let pools = match self.pool_rows().await {
            Ok(pools) => pools,
            Err(e) => {
                tracing::warn!("disk health: listing pools: {e}");
                return HashMap::new();
            }
        };

        let mut map = HashMap::new();
        for pool in pools {
            let Ok(out) = self.pool_status_kernel(&pool.name).await else {
                continue;
            };
            // Group rows are needed to label a cache/log member, but they must
            // not appear in the pool page's `PoolHealth.config`.
            let health = parse_pool_health_with(&pool.name, &out, true);
            for dev in &health.config {
                self.collect_disk_health(&pool.name, dev, None, &mut map);
            }
        }
        map
    }

    fn collect_disk_health(
        &self,
        pool: &str,
        dev: &PoolDevice,
        parent: Option<&str>,
        map: &mut HashMap<String, ZfsDiskHealth>,
    ) {
        if let Some(children) = &dev.devices {
            // The first level is the pool row itself; its children are the
            // real vdevs (mirror-0, raidz1-0, …). Do not label every leaf with
            // the pool name.
            let child_parent = if dev.name == pool {
                parent
            } else {
                Some(dev.name.as_str())
            };
            for child in children {
                self.collect_disk_health(pool, child, child_parent, map);
            }
            return;
        }

        let Some(device) = canonical_zfs_device(&self.host_root, &dev.name) else {
            return;
        };
        // A pool member is normally a partition (`sdc2`, `nvme0n1p1`), but the
        // health page lists whole disks. Attribute the member to the disk that
        // carries the ZFS label instead of dropping it.
        let device = whole_disk_path(&device).unwrap_or(device);
        let member = ZfsDiskHealth {
            pool: pool.to_string(),
            vdev: parent.unwrap_or(&dev.name).to_string(),
            state: dev.state.clone(),
            read_errors: parse_count(&dev.read),
            write_errors: parse_count(&dev.write),
            cksum_errors: parse_count(&dev.cksum),
        };
        match map.get_mut(&device) {
            Some(existing) => merge_disk_health(existing, member),
            None => {
                map.insert(device, member);
            }
        }
    }
}

/// Merge two members that live on the same physical disk (for example a data
/// partition plus a log or cache partition). The worse state wins, the error
/// counters add up, and the vdev labels are kept so neither member disappears.
fn merge_disk_health(existing: &mut ZfsDiskHealth, incoming: ZfsDiskHealth) {
    if state_severity(&incoming.state) > state_severity(&existing.state) {
        existing.pool = incoming.pool.clone();
        existing.state = incoming.state.clone();
        existing.vdev = merge_labels(&incoming.vdev, &existing.vdev);
    } else {
        existing.vdev = merge_labels(&existing.vdev, &incoming.vdev);
    }
    existing.read_errors = existing.read_errors.saturating_add(incoming.read_errors);
    existing.write_errors = existing.write_errors.saturating_add(incoming.write_errors);
    existing.cksum_errors = existing.cksum_errors.saturating_add(incoming.cksum_errors);
}

fn merge_labels(first: &str, second: &str) -> String {
    let mut labels: Vec<&str> = first.split(", ").collect();
    if !labels.contains(&second) {
        labels.push(second);
    }
    labels.join(", ")
}

/// Higher is worse. Unknown/empty states rank below ONLINE so a real state
/// always wins a merge.
fn state_severity(state: &str) -> u8 {
    match state.to_ascii_uppercase().as_str() {
        "FAULTED" | "REMOVED" => 6,
        "UNAVAIL" => 5,
        "OFFLINE" => 4,
        "DEGRADED" => 3,
        "ONLINE" => 2,
        _ => 1,
    }
}

/// Parse the fields of `zpool status` (everything before the I/O stats).
pub fn parse_pool_health(name: &str, out: &str) -> PoolHealth {
    parse_pool_health_with(name, out, false)
}

/// Like [`parse_pool_health`], but optionally keeps the non-leaf group rows
/// (`cache`, `logs`, `spares`, `special`, `dedup`) so disk membership can
/// attribute a member to its group instead of the pool row.
fn parse_pool_health_with(name: &str, out: &str, keep_groups: bool) -> PoolHealth {
    let mut health = PoolHealth {
        name: name.to_string(),
        ..Default::default()
    };
    let mut in_config = false;
    let mut header_seen = false;
    let mut entries: Vec<(usize, PoolDevice)> = Vec::new();

    for line in out.split('\n') {
        let trimmed = line.trim();

        if !in_config {
            if let Some(idx) = trimmed.find(':') {
                if idx > 0 {
                    let key = trimmed[..idx].trim();
                    let val = trimmed[idx + 1..].trim();
                    match key {
                        "pool" => health.name = val.to_string(),
                        "state" => health.state = val.to_string(),
                        "scan" => health.scan = val.to_string(),
                        "errors" => health.errors = val.to_string(),
                        _ => {}
                    }
                }
            }
            if trimmed == "config:" {
                in_config = true;
                header_seen = false;
            }
            continue;
        }

        if trimmed.is_empty() {
            if header_seen {
                in_config = false;
            }
            continue;
        }
        if !header_seen {
            header_seen = true;
            continue;
        }
        let fields: Vec<&str> = trimmed.split_whitespace().collect();
        if fields.len() < 5 {
            // Group rows (`cache`, `logs`, `spares`, `special`, `dedup`) have no
            // counters. Disk membership keeps them so its members are
            // attributed to the group vdev; the pool page does not.
            if keep_groups && fields.len() == 1 && is_vdev_group(fields[0]) {
                entries.push((
                    line.len() - line.trim_start().len(),
                    PoolDevice {
                        name: fields[0].to_string(),
                        ..Default::default()
                    },
                ));
            }
            continue;
        }
        let dev = PoolDevice {
            name: fields[0].to_string(),
            state: fields[1].to_string(),
            read: fields[2].to_string(),
            write: fields[3].to_string(),
            cksum: fields[4].to_string(),
            devices: None,
        };
        entries.push((line.len() - line.trim_start().len(), dev));
    }

    let mut index = 0;
    health.config = build_device_tree(&entries, &mut index, 0);
    health
}

/// Rebuild the indented `zpool status` config rows into the device tree. The
/// previous one-level parser attached every deeper row to the pool row, which
/// made a mirror/raidz leaf impossible to attribute to its vdev.
fn build_device_tree(
    entries: &[(usize, PoolDevice)],
    index: &mut usize,
    parent_indent: usize,
) -> Vec<PoolDevice> {
    let mut devices = Vec::new();
    while *index < entries.len() {
        let (indent, entry) = &entries[*index];
        if *indent <= parent_indent {
            break;
        }
        let mut dev = entry.clone();
        *index += 1;
        let children = build_device_tree(entries, index, *indent);
        dev.devices = if children.is_empty() {
            None
        } else {
            Some(children)
        };
        devices.push(dev);
    }
    devices
}

fn canonical_zfs_device(host_root: &Path, name: &str) -> Option<String> {
    let candidate = if name.starts_with("/dev/") {
        name.to_string()
    } else if is_whole_disk(name) || whole_disk_path(&format!("/dev/{name}")).is_some() {
        // Whole disks and partitions (`sdc`, `sdc2`); GUIDs and vdev labels
        // (`mirror-0`, `cache`) are rejected.
        format!("/dev/{name}")
    } else {
        return None;
    };
    canonical_dev(host_root, &candidate).ok().or_else(|| {
        let bare = candidate.trim_start_matches("/dev/");
        if is_whole_disk(bare) || whole_disk_path(&candidate).is_some() {
            Some(candidate)
        } else {
            None
        }
    })
}

fn parse_count(value: &str) -> u64 {
    value.parse().unwrap_or(0)
}

/// Non-leaf `zpool status` config rows that group spare/log/special vdevs.
fn is_vdev_group(name: &str) -> bool {
    matches!(
        name,
        "cache" | "logs" | "spares" | "spare" | "special" | "dedup"
    )
}

/// Extract the pool's I/O counters from `zpool iostat -v`.
pub fn parse_io_stats(out: &str, pool_name: &str) -> PoolIOStats {
    let mut stats = PoolIOStats::default();
    for line in out.split('\n') {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() >= 7 && fields[0] == pool_name {
            stats.read_ops = fields[3].to_string();
            stats.write_ops = fields[4].to_string();
            stats.read_bw = fields[5].to_string();
            stats.write_bw = fields[6].to_string();
            break;
        }
    }
    stats
}
