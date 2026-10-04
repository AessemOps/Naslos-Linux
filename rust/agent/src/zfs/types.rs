//! Data types shared by the ZFS operations. JSON field names and ordering
//! mirror the Go agent's struct tags exactly (the API and Playwright suite are
//! the conformance oracle).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A ZFS pool, as returned by `GET /api/v1/pools`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Pool {
    pub name: String,
    pub size: String,
    pub alloc: String,
    pub free: String,
    pub health: String,
    #[serde(default)]
    pub topology: String,
    /// `disks` has no `omitempty` in Go, so it is present as `null` when the
    /// member list could not be read.
    #[serde(default)]
    pub disks: Option<Vec<String>>,
    #[serde(default)]
    pub mountpoint: String,
}

/// A ZFS dataset.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Dataset {
    pub name: String,
    pub used: String,
    pub avail: String,
    pub refer: String,
    pub mountpoint: String,
    /// Exact `used` bytes (`zfs list -p`); used to catch a send that captured
    /// nothing (AV-8).
    #[serde(rename = "usedBytes")]
    pub used_bytes: i64,
    /// Whether the dataset is mounted in *this* process's mount namespace.
    pub mounted: bool,
}

/// `POST /api/v1/pools` request body.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PoolConfig {
    pub name: String,
    #[serde(default)]
    pub topology: String,
    #[serde(default)]
    pub disks: Vec<String>,
    #[serde(default)]
    pub cache: String,
    #[serde(default)]
    pub options: HashMap<String, String>,
}

/// A pool discovered by `zpool import` but not currently imported.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImportablePool {
    pub name: String,
    pub state: String,
    pub topology: String,
    /// No `omitempty` in Go: `null` when no disks were parsed.
    #[serde(default)]
    pub disks: Option<Vec<String>>,
}

/// Structured parse of `zpool status` for the health page.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PoolHealth {
    pub name: String,
    pub state: String,
    pub scan: String,
    pub errors: String,
    pub config: Vec<PoolDevice>,
    #[serde(rename = "ioStats")]
    pub io_stats: PoolIOStats,
}

/// A single device in the pool config tree.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PoolDevice {
    pub name: String,
    pub state: String,
    pub read: String,
    pub write: String,
    pub cksum: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub devices: Option<Vec<PoolDevice>>,
}

/// `zpool iostat` counters for a pool.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PoolIOStats {
    #[serde(rename = "readOps")]
    pub read_ops: String,
    #[serde(rename = "writeOps")]
    pub write_ops: String,
    #[serde(rename = "readBW")]
    pub read_bw: String,
    #[serde(rename = "writeBW")]
    pub write_bw: String,
}

/// A snapshot's identity: name plus GUID (the sender recognises its last send).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SnapshotInfo {
    pub name: String,
    pub guid: String,
    pub created: String,
}

/// One `zfs send` invocation.
#[derive(Debug, Clone, Default)]
pub struct SendStreamOptions {
    pub dataset: String,
    pub to: String,
    pub from: String,
    /// Send encrypted records as-is (`-w`); a backup wants this.
    pub raw: bool,
}
