//! Read-only SMART health for the node's whole disks.
//!
//! The agent image bundles a static `smartctl` (see `agent/Dockerfile.rust`).
//! It is run directly, not through `chroot /host`: the binary lives in the agent
//! image while `/dev` is the host's device tree. `-n standby` keeps a sleeping
//! disk asleep, and every call is bounded so a wedged device cannot hold the
//! agent's other routes.

use super::devices::is_whole_disk;
use super::runner::join_host;
use super::types::ZfsDiskHealth;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const CACHE_TTL: Duration = Duration::from_secs(30);
const DEFAULT_TIMEOUT_SECS: u64 = 10;
const DEFAULT_MAX_DISKS: usize = 64;
const MAX_CONCURRENT_PROBES: usize = 4;

/// One disk's SMART view. Optional fields are omitted when the device does not
/// report them; `available:false` means smartctl could not read the device.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SmartInfo {
    pub available: bool,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub health: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "temperatureC"
    )]
    pub temperature_c: Option<i64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "powerOnHours"
    )]
    pub power_on_hours: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "wearPercent"
    )]
    pub wear_percent: Option<u8>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "reallocatedSectors"
    )]
    pub reallocated_sectors: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "pendingSectors"
    )]
    pub pending_sectors: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "uncorrectableSectors"
    )]
    pub uncorrectable_sectors: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "mediaErrors"
    )]
    pub media_errors: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "errorCount"
    )]
    pub error_count: Option<u64>,
    #[serde(default)]
    pub standby: bool,
    #[serde(default)]
    pub message: String,
}

impl SmartInfo {
    fn unavailable(message: impl Into<String>) -> Self {
        Self {
            available: false,
            message: message.into(),
            ..Default::default()
        }
    }
}

/// A whole disk with its SMART data and, when it belongs to a pool, its ZFS
/// vdev state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiskHealth {
    pub device: String,
    #[serde(default)]
    pub smart: SmartInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zfs: Option<ZfsDiskHealth>,
}

/// The agent's `/api/v1/disks/health` payload.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiskHealthReport {
    #[serde(rename = "smartctlAvailable")]
    pub smartctl_available: bool,
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub disks: Vec<DiskHealth>,
}

#[derive(Clone)]
struct CachedReport {
    at: Instant,
    report: DiskHealthReport,
}

/// Runs smartctl against the host's whole disks.
pub struct SmartClient {
    host_root: PathBuf,
    bin: PathBuf,
    timeout: Duration,
    max_disks: usize,
    cache: tokio::sync::Mutex<Option<CachedReport>>,
}

impl SmartClient {
    pub fn new(
        host_root: impl Into<PathBuf>,
        bin: impl Into<PathBuf>,
        timeout: Duration,
        max_disks: usize,
    ) -> Self {
        Self {
            host_root: host_root.into(),
            bin: bin.into(),
            timeout,
            max_disks: max_disks.max(1),
            cache: tokio::sync::Mutex::new(None),
        }
    }

    /// Build from `SMARTCTL_BIN`, `SMARTCTL_TIMEOUT_SECONDS` and
    /// `SMARTCTL_MAX_DISKS`. An empty `SMARTCTL_BIN` disables SMART but keeps
    /// the ZFS half of the endpoint working.
    pub fn from_env(host_root: impl Into<PathBuf>) -> Self {
        let bin = std::env::var("SMARTCTL_BIN").unwrap_or_else(|_| "/app/smartctl".to_string());
        let timeout = std::env::var("SMARTCTL_TIMEOUT_SECONDS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|n| *n > 0)
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(DEFAULT_TIMEOUT_SECS));
        let max_disks = std::env::var("SMARTCTL_MAX_DISKS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_MAX_DISKS);
        Self::new(host_root, bin, timeout, max_disks)
    }

    /// Scan every whole disk, reusing a report younger than 30 s.
    pub async fn scan(&self) -> DiskHealthReport {
        if let Some(cached) = self.cache.lock().await.as_ref() {
            if cached.at.elapsed() < CACHE_TTL {
                return cached.report.clone();
            }
        }
        let report = self.scan_uncached().await;
        *self.cache.lock().await = Some(CachedReport {
            at: Instant::now(),
            report: report.clone(),
        });
        report
    }

    async fn scan_uncached(&self) -> DiskHealthReport {
        if self.bin.as_os_str().is_empty() || !self.bin.exists() {
            return DiskHealthReport {
                smartctl_available: false,
                error: format!("smartctl is not available at {}", self.bin.display()),
                disks: Vec::new(),
            };
        }

        let devices = self.whole_disks();
        let probes = devices.into_iter().map(|device| async move {
            let smart = self.probe(&device).await;
            DiskHealth {
                device,
                smart,
                zfs: None,
            }
        });
        let mut disks: Vec<DiskHealth> = futures::stream::iter(probes)
            .buffer_unordered(MAX_CONCURRENT_PROBES)
            .collect()
            .await;
        disks.sort_by(|a, b| a.device.cmp(&b.device));

        DiskHealthReport {
            smartctl_available: true,
            error: String::new(),
            disks,
        }
    }

    /// Whole disks under the host's `/dev`, sorted and capped.
    fn whole_disks(&self) -> Vec<String> {
        let dev_dir = join_host(&self.host_root, "/dev");
        let Ok(entries) = std::fs::read_dir(dev_dir) else {
            return Vec::new();
        };
        let mut devices = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !is_whole_disk(&name) {
                continue;
            }
            // Real device nodes are block devices; the regular-file arm keeps
            // the hermetic test fixture usable without mknod.
            match entry.file_type() {
                Ok(ft) if std::os::unix::fs::FileTypeExt::is_block_device(&ft) || ft.is_file() => {}
                _ => continue,
            }
            devices.push(format!("/dev/{name}"));
        }
        devices.sort();
        devices.truncate(self.max_disks);
        devices
    }

    async fn probe(&self, device: &str) -> SmartInfo {
        let mut cmd = tokio::process::Command::new(&self.bin);
        cmd.arg("-j")
            .arg("-a")
            .arg("-n")
            .arg("standby")
            .arg(device)
            .kill_on_drop(true);
        match tokio::time::timeout(self.timeout, cmd.output()).await {
            Err(_) => SmartInfo::unavailable(format!(
                "smartctl timed out after {}s",
                self.timeout.as_secs()
            )),
            Ok(Err(e)) => SmartInfo::unavailable(format!("running smartctl: {e}")),
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let mut info = parse_smartctl(&stdout);
                let raw = if stdout.trim().is_empty() {
                    stderr.as_ref()
                } else {
                    stdout.as_ref()
                };
                let standby = raw.to_ascii_lowercase().contains("standby")
                    || raw.to_ascii_lowercase().contains("sleep");
                if standby && !info.available {
                    info.standby = true;
                    info.message = "device is in standby; SMART data was not read".to_string();
                } else if info.message.is_empty() && !info.available {
                    let detail = stderr.trim();
                    info.message = if detail.is_empty() {
                        "smartctl returned no SMART data".to_string()
                    } else {
                        detail.to_string()
                    };
                }
                info
            }
        }
    }
}

/// Parse one `smartctl -j -a` document. A failing disk is data, not a command
/// failure: smartctl's exit status is a bitmask, so it is deliberately ignored
/// in favour of the JSON.
pub fn parse_smartctl(stdout: &str) -> SmartInfo {
    let Ok(json) = serde_json::from_str::<Value>(stdout) else {
        return SmartInfo::unavailable("smartctl returned invalid JSON");
    };

    let messages = smartctl_messages(&json);
    let has_device = json.get("device").is_some();
    let mut info = SmartInfo {
        available: has_device,
        message: messages.join("; "),
        ..Default::default()
    };

    let support = json.get("smart_support");
    if let Some(support) = support {
        info.enabled = support
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if support.get("available").and_then(Value::as_bool) == Some(false) {
            info.available = false;
            info.enabled = false;
            info.health = "UNKNOWN".to_string();
            if info.message.is_empty() {
                info.message = "SMART support is unavailable on this device".to_string();
            }
            return info;
        }
    }

    if let Some(status) = json.get("smart_status") {
        info.health = match status.get("passed").and_then(Value::as_bool) {
            Some(true) => "PASSED".to_string(),
            Some(false) => "FAILED".to_string(),
            None => "UNKNOWN".to_string(),
        };
    } else {
        info.health = "UNKNOWN".to_string();
    }

    info.temperature_c = json
        .get("temperature")
        .and_then(|t| t.get("current"))
        .and_then(Value::as_i64)
        .or_else(|| {
            json.get("nvme_smart_health_information_log")
                .and_then(|n| n.get("temperature"))
                .and_then(Value::as_i64)
        })
        .or_else(|| ata_raw(&json, 194).and_then(|v| i64::try_from(v).ok()))
        .or_else(|| ata_raw(&json, 190).and_then(|v| i64::try_from(v).ok()));

    info.power_on_hours = json
        .get("power_on_time")
        .and_then(|t| t.get("hours"))
        .and_then(Value::as_u64);

    if let Some(mode) = json.get("power_mode").and_then(Value::as_str) {
        let mode = mode.to_ascii_uppercase();
        info.standby = mode.contains("STANDBY") || mode.contains("SLEEP");
    }
    if info.standby && info.message.is_empty() {
        info.message = "device is in standby; SMART data was not read".to_string();
    }

    if let Some(nvme) = json.get("nvme_smart_health_information_log") {
        // NVMe SMART is always available when the log is present; smartctl does
        // not always emit a smart_support block for it.
        info.enabled = true;
        info.wear_percent = nvme
            .get("percentage_used")
            .and_then(Value::as_u64)
            .and_then(|v| u8::try_from(v).ok());
        info.media_errors = nvme.get("media_errors").and_then(Value::as_u64);
        info.error_count = nvme.get("num_err_log_entries").and_then(Value::as_u64);
    }

    info.reallocated_sectors = ata_raw(&json, 5);
    info.pending_sectors = ata_raw(&json, 197);
    info.uncorrectable_sectors = ata_raw(&json, 198);
    info.error_count = info.error_count.or_else(|| {
        json.get("ata_smart_error_log")
            .and_then(|l| l.get("summary"))
            .and_then(|s| s.get("count"))
            .and_then(Value::as_u64)
    });

    // ATA wear attributes are normalised values that start at 100 and fall as
    // the device wears. Prefer 177 (wear leveling) then 233 (media wearout).
    info.wear_percent = info
        .wear_percent
        .or_else(|| ata_normalized(&json, 177).map(|v| 100u64.saturating_sub(v) as u8))
        .or_else(|| ata_normalized(&json, 233).map(|v| 100u64.saturating_sub(v) as u8));

    if !has_device {
        info.available = false;
        if info.message.is_empty() {
            info.message = "smartctl could not read the device".to_string();
        }
    }

    info
}

fn smartctl_messages(json: &Value) -> Vec<String> {
    json.get("smartctl")
        .and_then(|s| s.get("messages"))
        .and_then(Value::as_array)
        .map(|messages| {
            messages
                .iter()
                .filter(|m| {
                    !matches!(
                        m.get("severity").and_then(Value::as_str),
                        Some("information")
                    )
                })
                .filter_map(|m| m.get("string").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn ata_attribute(json: &Value, id: u64) -> Option<&Value> {
    json.get("ata_smart_attributes")?
        .get("table")?
        .as_array()?
        .iter()
        .find(|attr| attr.get("id").and_then(Value::as_u64) == Some(id))
}

fn ata_raw(json: &Value, id: u64) -> Option<u64> {
    let raw = ata_attribute(json, id)?.get("raw")?.get("value")?;
    raw.as_u64().or_else(|| raw.as_str()?.trim().parse().ok())
}

fn ata_normalized(json: &Value, id: u64) -> Option<u64> {
    ata_attribute(json, id)?.get("value")?.as_u64()
}

/// Whether a smartctl binary exists at `path` (used by the endpoint tests).
pub fn smartctl_available(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::{parse_smartctl, smartctl_available, SmartClient};
    use std::time::Duration;

    const ATA: &str = r#"{
      "smartctl": {"exit_status": 0, "messages": []},
      "device": {"name": "/dev/sda", "type": "ata"},
      "model_name": "WDC WD40EFRX",
      "serial_number": "WD-WCC7K0000000",
      "smart_support": {"available": true, "enabled": true},
      "smart_status": {"passed": true},
      "temperature": {"current": 34},
      "power_on_time": {"hours": 12345},
      "ata_smart_attributes": {"table": [
        {"id": 5, "name": "Reallocated_Sector_Ct", "value": 100, "raw": {"value": 0}},
        {"id": 177, "name": "Wear_Leveling_Count", "value": 92, "raw": {"value": 8}},
        {"id": 194, "name": "Temperature_Celsius", "value": 66, "raw": {"value": 34}},
        {"id": 197, "name": "Current_Pending_Sector", "value": 100, "raw": {"value": 0}},
        {"id": 198, "name": "Offline_Uncorrectable", "value": 100, "raw": {"value": 0}}
      ]},
      "ata_smart_error_log": {"summary": {"count": 2}}
    }"#;

    const NVME: &str = r#"{
      "smartctl": {"exit_status": 0},
      "device": {"name": "/dev/nvme0n1", "protocol": "NVMe"},
      "smart_support": {"available": true, "enabled": true},
      "smart_status": {"passed": true},
      "temperature": {"current": 41},
      "power_on_time": {"hours": 987},
      "nvme_smart_health_information_log": {
        "percentage_used": 4,
        "media_errors": 0,
        "num_err_log_entries": 7,
        "temperature": 41
      }
    }"#;

    #[test]
    fn parses_ata_health() {
        let info = parse_smartctl(ATA);
        assert!(info.available);
        assert!(info.enabled);
        assert_eq!(info.health, "PASSED");
        assert_eq!(info.temperature_c, Some(34));
        assert_eq!(info.power_on_hours, Some(12345));
        assert_eq!(info.wear_percent, Some(8));
        assert_eq!(info.reallocated_sectors, Some(0));
        assert_eq!(info.pending_sectors, Some(0));
        assert_eq!(info.uncorrectable_sectors, Some(0));
        assert_eq!(info.error_count, Some(2));
    }

    #[test]
    fn parses_nvme_health() {
        let info = parse_smartctl(NVME);
        assert!(info.available);
        assert_eq!(info.health, "PASSED");
        assert_eq!(info.temperature_c, Some(41));
        assert_eq!(info.wear_percent, Some(4));
        assert_eq!(info.media_errors, Some(0));
        assert_eq!(info.error_count, Some(7));
    }

    #[test]
    fn failed_disk_is_data_not_an_error() {
        let json = ATA.replace(r#""passed": true"#, r#""passed": false"#);
        let info = parse_smartctl(&json);
        assert!(info.available);
        assert_eq!(info.health, "FAILED");
    }

    #[test]
    fn standby_is_reported_without_waking() {
        let json = ATA.replace(
            r#""temperature": {"current": 34},"#,
            r#""power_mode": "STANDBY","#,
        );
        let info = parse_smartctl(&json);
        assert!(info.standby);
        assert!(info.message.contains("standby"));
    }

    #[test]
    fn unsupported_smart_is_unknown() {
        let json = ATA.replace(
            r#""smart_support": {"available": true, "enabled": true},"#,
            r#""smart_support": {"available": false, "enabled": false},"#,
        );
        let info = parse_smartctl(&json);
        assert!(!info.available);
        assert_eq!(info.health, "UNKNOWN");
        assert!(info.message.contains("unavailable"));
    }

    #[test]
    fn open_failure_is_unavailable() {
        let json = r#"{
          "smartctl": {
            "exit_status": 2,
            "messages": [{"string": "Smartctl open device: /dev/vda failed: Operation not supported", "severity": "error"}]
          }
        }"#;
        let info = parse_smartctl(json);
        assert!(!info.available);
        assert!(info.message.contains("not supported"));
    }

    #[test]
    fn invalid_json_is_unavailable() {
        let info = parse_smartctl("not json");
        assert!(!info.available);
    }

    #[test]
    fn enumerates_only_whole_disks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("dev")).unwrap();
        for name in ["sda", "sda1", "nvme0n1", "nvme0n1p1", "loop0", "sr0"] {
            std::fs::write(dir.path().join("dev").join(name), b"").unwrap();
        }
        let client = SmartClient::new(dir.path(), "/bin/true", Duration::from_secs(1), 10);
        assert_eq!(client.whole_disks(), vec!["/dev/nvme0n1", "/dev/sda"]);
    }

    #[test]
    fn missing_binary_is_unavailable() {
        assert!(!smartctl_available(std::path::Path::new(
            "/nonexistent/smartctl"
        )));
    }

    #[tokio::test]
    async fn probe_recognizes_a_standby_message_without_json() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("smartctl");
        std::fs::write(
            &script,
            "#!/bin/sh\necho 'Device is in STANDBY mode'\nexit 2\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let client = SmartClient::new(dir.path(), script, Duration::from_secs(5), 1);
        let info = client.probe("/dev/sdb").await;
        assert!(info.standby);
        assert!(!info.available);
        assert!(info.message.contains("standby"));
    }
}
