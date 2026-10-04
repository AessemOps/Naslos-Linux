//! ZFS pool/dataset operations for the Naslos agent.
//!
//! All operations execute via `chroot /host` to manage pools on the Talos host.

pub mod backup;
pub mod dataset;
pub mod devices;
pub mod operations;
pub mod runner;
pub mod types;
pub mod utils;
pub mod validation;

pub use devices::validate_pool_name;

pub use runner::{join_host, RealRunner, Runner};
pub use types::{
    Dataset, ImportablePool, Pool, PoolConfig, PoolDevice, PoolHealth, PoolIOStats, SendStreamOptions,
    SnapshotInfo,
};
pub use validation::{ZfsError, ZfsResult};

use std::path::PathBuf;
use std::sync::Arc;

/// Absolute paths inside the Talos host.
pub const ZPOOL_BIN: &str = "/usr/local/sbin/zpool";
pub const ZFS_BIN: &str = "/usr/local/sbin/zfs";
pub const WIPEFS_BIN: &str = "/usr/bin/wipefs";

/// The ZFS client. `host_root` is where the Talos host filesystem is mounted
/// into the agent pod (`/host`), and also the chroot target the runner uses.
pub struct ZfsClient {
    runner: Arc<dyn Runner>,
    host_root: PathBuf,
}

impl ZfsClient {
    pub fn new(runner: Arc<dyn Runner>, host_root: impl Into<PathBuf>) -> Self {
        Self {
            runner,
            host_root: host_root.into(),
        }
    }

    /// Run a command inside the host namespace via chroot.
    pub(crate) async fn host_exec(
        &self,
        bin: &str,
        args: &[String],
    ) -> Result<String, runner::RunError> {
        self.runner.run(bin, args).await
    }

    /// `hostRoot + <in-host path>`.
    pub(crate) fn host_path(&self, in_host: &str) -> PathBuf {
        join_host(&self.host_root, in_host)
    }

    /// Whether a binary exists inside the host root (gates optional steps).
    pub fn host_bin_exists(&self, bin: &str) -> bool {
        self.host_path(bin).exists()
    }

    /// ZFS is usable when the host has both the `zpool` binary and a loaded
    /// module (`/dev/zfs`). The binary can exist without the module, which would
    /// fail later with a confusing error.
    pub fn is_zfs_available(&self) -> bool {
        self.host_path(ZPOOL_BIN).exists() && self.host_path("/dev/zfs").exists()
    }
}

/// Convenience: build a `Vec<String>` from string slices.
pub(crate) fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}
