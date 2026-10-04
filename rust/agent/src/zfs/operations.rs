//! Pool listing, creation, import/export and health (port of
//! `zfs/operations.go`).

use super::devices::{args2, normalize_vdev_topology, validate_pool_name, vdev_label, vdev_minimum_disks};
use super::types::{ImportablePool, Pool, PoolConfig, PoolDevice, PoolHealth, PoolIOStats};
use super::{args, ZfsClient, ZFS_BIN, ZPOOL_BIN, WIPEFS_BIN};
use crate::invalid;
use crate::zfs::validation::{ZfsError, ZfsResult};

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
    /// All pools with their member disks.
    pub async fn pools(&self) -> ZfsResult<Vec<Pool>> {
        let out = match self
            .host_exec(
                ZPOOL_BIN,
                &args(&["list", "-H", "-o", "name,size,alloc,free,health"]),
            )
            .await
        {
            Ok(out) => out,
            Err(e) => {
                return Err(ZfsError::Other(format!("listing pools: {}", e.message)))
            }
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
            let mut pool = Pool {
                name: fields[0].to_string(),
                size: fields[1].to_string(),
                alloc: fields[2].to_string(),
                free: fields[3].to_string(),
                health: fields[4].to_string(),
                ..Default::default()
            };
            // Parse `zpool status` for member disks so the disk wizard can mark
            // disks already in a pool.
            if let Ok(disks) = self.pool_disks(&pool.name).await {
                pool.disks = if disks.is_empty() { None } else { Some(disks) };
            }
            pools.push(pool);
        }
        Ok(pools)
    }

    /// Member devices of a pool, parsed from `zpool status`.
    pub async fn pool_disks(&self, name: &str) -> ZfsResult<Vec<String>> {
        validate_pool_name(name)?;
        let out = match self.host_exec(ZPOOL_BIN, &args2("status", name)).await {
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
        validate_pool_name(name)?;
        match self.host_exec(ZPOOL_BIN, &args2("status", name)).await {
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
            return Err(ZfsError::Validation(
                "at least one disk is required".into(),
            ));
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

        // Refuse if the pool already exists.
        if self
            .host_exec(ZPOOL_BIN, &args2("list", &cfg.name))
            .await
            .is_ok()
        {
            return Err(invalid!("pool {:?} already exists", cfg.name));
        }

        let members = self.pool_members().await?;
        let disks = self.normalize_disk_set_for_host(&cfg.disks, &members)?;

        let mut cache = String::new();
        if !cfg.cache.is_empty() {
            let normalized = self
                .normalize_disk_set_for_host(&[cfg.cache.clone()], &members)?;
            cache = normalized[0].clone();
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
                if let Err(e) = self
                    .host_exec(WIPEFS_BIN, &args(&["--all", disk]))
                    .await
                {
                    return Err(ZfsError::Other(format!(
                        "wiping {}: {}",
                        disk, e.message
                    )));
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
            opts.insert(k.clone(), v.clone());
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
    pub async fn destroy_pool(&self, name: &str) -> ZfsResult<()> {
        validate_pool_name(name)?;
        match self
            .host_exec(ZPOOL_BIN, &args(&["destroy", "-f", name]))
            .await
        {
            Ok(_) => Ok(()),
            Err(e) => Err(ZfsError::Other(format!(
                "destroying pool: {}: {}",
                e.output, e.message
            ))),
        }
    }

    /// Pools that can be imported but are not active (`zpool import` dry run).
    pub async fn list_importable(&self) -> ZfsResult<Vec<ImportablePool>> {
        let out = match self.host_exec(ZPOOL_BIN, &args(&["import"])).await {
            Ok(out) => out,
            Err(e) => {
                return Err(ZfsError::Other(format!(
                    "listing importable pools: {}",
                    e.message
                )))
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
        match self
            .host_exec(ZPOOL_BIN, &args(&["export", name]))
            .await
        {
            Ok(_) => Ok(()),
            Err(e) => Err(ZfsError::Other(format!(
                "exporting pool: {}: {}",
                e.output, e.message
            ))),
        }
    }

    /// Structured health from `zpool status` plus `zpool iostat`.
    pub async fn pool_health(&self, name: &str) -> ZfsResult<PoolHealth> {
        validate_pool_name(name)?;
        let out = match self.host_exec(ZPOOL_BIN, &args2("status", name)).await {
            Ok(out) => out,
            Err(e) => {
                return Err(ZfsError::Other(format!(
                    "getting pool status: {}",
                    e.message
                )))
            }
        };

        let mut health = PoolHealth {
            name: name.to_string(),
            ..Default::default()
        };
        let mut in_config = false;
        let mut header_seen = false;

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
            let indent = line.len() - line.trim_start().len();
            if indent <= 1 {
                health.config.push(dev);
            } else if let Some(parent) = health.config.last_mut() {
                parent.devices.get_or_insert_with(Vec::new).push(dev);
            }
        }

        if let Ok(io_out) = self
            .host_exec(ZPOOL_BIN, &args(&["iostat", "-v", name, "1", "1"]))
            .await
        {
            health.io_stats = parse_io_stats(&io_out, name);
        }

        Ok(health)
    }
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
