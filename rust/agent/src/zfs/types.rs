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
    /// `zfs get encryption`: `off` for an unencrypted dataset, else the cipher.
    #[serde(default)]
    pub encryption: String,
    /// `zfs get keystatus`: `none` (not encrypted), `unavailable` (locked) or
    /// `available` (unlocked). The agent normalises ZFS's `-` to `none`.
    #[serde(default)]
    pub keystatus: String,
    /// The dataset whose key this dataset uses (`zfs get encryptionroot`).
    #[serde(default, rename = "encryptionRoot")]
    pub encryption_root: String,
    /// ZFS's stable object GUID. The API binds a destroy (and the key Secret
    /// it deletes) to this value instead of the reusable dataset name.
    #[serde(default)]
    pub guid: String,
}

/// The encryption state of one dataset (`GET /api/v1/encryption/status/…`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EncryptionStatus {
    pub name: String,
    #[serde(default)]
    pub encryption: String,
    #[serde(default)]
    pub keystatus: String,
    #[serde(default, rename = "encryptionRoot")]
    pub encryption_root: String,
    #[serde(default)]
    pub mounted: bool,
}

impl EncryptionStatus {
    /// Whether the dataset (or an ancestor) is encrypted.
    pub fn is_encrypted(&self) -> bool {
        !self.encryption.is_empty() && self.encryption != "off" && self.encryption != "-"
    }
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

/// A ZFS zvol (block volume), as returned by `GET /api/v1/zvols`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Zvol {
    pub name: String,
    /// Human-readable `volsize`.
    pub volsize: String,
    /// Exact `volsize` bytes (`zfs list -p`); resize compares against this.
    #[serde(rename = "volsizeBytes")]
    pub volsize_bytes: i64,
    /// Exact `volblocksize` bytes (`zfs list -p` prints numeric properties as bytes).
    pub volblocksize: String,
    #[serde(default)]
    pub compression: String,
    pub used: String,
    pub avail: String,
    pub refer: String,
    /// `encryption` property (`off` for a plain volume).
    #[serde(default)]
    pub encryption: String,
    /// `encryptionroot` property; empty for a plain volume.
    #[serde(default, rename = "encryptionRoot")]
    pub encryption_root: String,
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

/// A disk's membership and error counters from `zpool status`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ZfsDiskHealth {
    pub pool: String,
    pub vdev: String,
    pub state: String,
    #[serde(rename = "readErrors")]
    pub read_errors: u64,
    #[serde(rename = "writeErrors")]
    pub write_errors: u64,
    #[serde(rename = "cksumErrors")]
    pub cksum_errors: u64,
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
    #[serde(default)]
    pub dataset: String,
    pub name: String,
    pub guid: String,
    pub created: String,
}

/// A snapshot and the storage schedule that owns it, from the
/// `user:naslos:schedule` property. An unset property is an empty string.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotSchedule {
    pub path: String,
    #[serde(default)]
    pub schedule: String,
}

/// One `zfs send` invocation.
#[derive(Debug, Clone, Default)]
pub struct SendStreamOptions {
    pub dataset: String,
    pub to: String,
    pub from: String,
    /// Send encrypted records as-is (`-w`); a backup wants this.
    pub raw: bool,
    /// Include descendant datasets (`-r`, without properties). Local
    /// replication uses it when the request asks for a recursive copy.
    pub recursive: bool,
}
