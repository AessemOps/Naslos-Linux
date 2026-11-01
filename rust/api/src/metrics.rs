//! System metrics model and manager (port of `api/internal/metrics`).

use chrono::{DateTime, Utc};
use std::sync::Mutex;

/// Go's `time.Time{}` zero value serializes as this RFC3339 string.
pub const ZERO_TIME: &str = "0001-01-01T00:00:00Z";

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SystemMetrics {
    pub cpu: CpuMetrics,
    pub memory: MemoryMetrics,
    pub disk: DiskMetrics,
    pub network: NetworkMetrics,
    pub zfs: ZfsMetrics,
    pub system: SystemInfo,
    #[serde(rename = "updatedAt")]
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CpuMetrics {
    #[serde(rename = "usagePercent")]
    pub usage_percent: f64,
    pub cores: i64,
    #[serde(rename = "loadAvg1")]
    pub load_avg1: f64,
    #[serde(rename = "loadAvg5")]
    pub load_avg5: f64,
    #[serde(rename = "loadAvg15")]
    pub load_avg15: f64,
    #[serde(default)]
    pub model: String,
    #[serde(rename = "temperatureC")]
    pub temperature_c: Option<f64>,
    #[serde(default, rename = "perCore")]
    pub per_core: Vec<CpuCore>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CpuCore {
    pub core: i64,
    #[serde(rename = "usagePercent")]
    pub usage_percent: f64,
    #[serde(
        default,
        rename = "frequencyMhz",
        skip_serializing_if = "Option::is_none"
    )]
    pub frequency_mhz: Option<u64>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct MemoryMetrics {
    pub total: u64,
    pub used: u64,
    pub free: u64,
    pub available: u64,
    #[serde(rename = "usagePercent")]
    pub usage_percent: f64,
    #[serde(rename = "swapTotal")]
    pub swap_total: u64,
    #[serde(rename = "swapUsed")]
    pub swap_used: u64,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct DiskMetrics {
    pub total: u64,
    pub used: u64,
    pub free: u64,
    #[serde(rename = "usagePercent")]
    pub usage_percent: f64,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct NetworkMetrics {
    #[serde(rename = "bytesSent")]
    pub bytes_sent: u64,
    #[serde(rename = "bytesRecv")]
    pub bytes_recv: u64,
    #[serde(rename = "packetsSent")]
    pub packets_sent: u64,
    #[serde(rename = "packetsRecv")]
    pub packets_recv: u64,
    pub interfaces: Option<Vec<NetworkInterface>>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct NetworkInterface {
    pub name: String,
    #[serde(rename = "ipAddress")]
    pub ip_address: String,
    #[serde(rename = "macAddress")]
    pub mac_address: String,
    #[serde(rename = "bytesSent")]
    pub bytes_sent: u64,
    #[serde(rename = "bytesRecv")]
    pub bytes_recv: u64,
}

/// Parse a `zpool list` size (`10.5G`, `1.2T`, `512M`) into bytes. The agent's
/// pool list uses human-readable strings while the dashboard contract is
/// numeric (SEC-2026-H7).
pub fn parse_zfs_size(value: &str) -> Option<u64> {
    let value = value.trim();
    if value.is_empty() || value == "-" {
        return None;
    }
    let (number, multiplier) = match value.chars().last()? {
        'K' | 'k' => (&value[..value.len() - 1], 1u64 << 10),
        'M' | 'm' => (&value[..value.len() - 1], 1u64 << 20),
        'G' | 'g' => (&value[..value.len() - 1], 1u64 << 30),
        'T' | 't' => (&value[..value.len() - 1], 1u64 << 40),
        'P' | 'p' => (&value[..value.len() - 1], 1u64 << 50),
        _ => (value, 1),
    };
    let number: f64 = number.trim_end_matches('B').parse().ok()?;
    if !number.is_finite() || number < 0.0 {
        return None;
    }
    Some((number * multiplier as f64) as u64)
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ZfsMetrics {
    pub pools: Option<Vec<PoolMetrics>>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PoolMetrics {
    pub name: String,
    pub size: u64,
    pub alloc: u64,
    pub free: u64,
    #[serde(rename = "usagePercent")]
    pub usage_percent: f64,
    pub health: String,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SystemInfo {
    pub hostname: String,
    pub uptime: u64,
    pub os: String,
    pub kernel: String,
    #[serde(rename = "talosVersion")]
    pub talos_version: String,
    #[serde(rename = "lastBoot")]
    pub last_boot: Option<DateTime<Utc>>,
}

/// Manages the metrics snapshot.
#[derive(Default)]
pub struct Manager {
    metrics: Mutex<SystemMetrics>,
}

impl Manager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self) -> SystemMetrics {
        self.metrics.lock().unwrap().clone()
    }

    pub fn update(&self, mut m: SystemMetrics) {
        m.updated_at = Some(Utc::now());
        *self.metrics.lock().unwrap() = m;
    }

    /// The dashboard projection (port of `Manager.GetDashboardData`).
    pub fn dashboard_data(&self) -> serde_json::Value {
        let m = self.metrics.lock().unwrap();
        let updated_at = match m.updated_at {
            Some(t) => serde_json::Value::String(t.to_rfc3339()),
            None => serde_json::Value::String(ZERO_TIME.to_string()),
        };
        serde_json::json!({
            "cpu": {
                "usage": m.cpu.usage_percent,
                "cores": m.cpu.cores,
                "model": m.cpu.model.clone(),
                "temperatureC": m.cpu.temperature_c,
                "loadAvg1": m.cpu.load_avg1,
                "loadAvg5": m.cpu.load_avg5,
                "loadAvg15": m.cpu.load_avg15,
                "perCore": m.cpu.per_core.clone(),
            },
            "memory": {
                "usage": m.memory.usage_percent,
                "total": m.memory.total,
                "used": m.memory.used,
                "available": m.memory.available,
            },
            "disk": {
                "usage": m.disk.usage_percent,
                "total": m.disk.total,
                "used": m.disk.used,
                "free": m.disk.free,
            },
            "zfs": { "poolCount": m.zfs.pools.as_deref().unwrap_or(&[]).len(), "pools": m.zfs.pools },
            "system": {
                "hostname": m.system.hostname,
                "uptime": m.system.uptime,
                "os": m.system.os,
            },
            "network": {
                "bytesSent": m.network.bytes_sent,
                "bytesRecv": m.network.bytes_recv,
                "packetsSent": m.network.packets_sent,
                "packetsRecv": m.network.packets_recv,
                "interfaces": m.network.interfaces,
            },
            "updatedAt": updated_at,
        })
    }

    /// The full `/api/metrics` view with Go's zero-time when un-collected.
    pub fn full_data(&self) -> serde_json::Value {
        let data = self.get();
        let mut value = serde_json::to_value(data).unwrap();
        value["updatedAt"] = self.updated_at_json();
        value
    }

    pub fn updated_at_json(&self) -> serde_json::Value {
        let m = self.metrics.lock().unwrap();
        match m.updated_at {
            Some(t) => serde_json::Value::String(t.to_rfc3339()),
            None => serde_json::Value::String(ZERO_TIME.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_zfs_size, CpuCore, CpuMetrics, Manager, SystemMetrics};

    #[test]
    fn dashboard_cpu_carries_temperature_and_per_core() {
        let manager = Manager::new();
        manager.update(SystemMetrics {
            cpu: CpuMetrics {
                usage_percent: 12.5,
                cores: 2,
                model: "Test CPU".into(),
                temperature_c: Some(45.0),
                per_core: vec![
                    CpuCore {
                        core: 0,
                        usage_percent: 10.0,
                        frequency_mhz: Some(3600),
                    },
                    CpuCore {
                        core: 1,
                        usage_percent: 15.0,
                        frequency_mhz: None,
                    },
                ],
                ..Default::default()
            },
            ..Default::default()
        });
        let view = manager.dashboard_data();
        assert_eq!(view["cpu"]["model"], "Test CPU");
        assert_eq!(view["cpu"]["temperatureC"], 45.0);
        assert_eq!(view["cpu"]["perCore"].as_array().unwrap().len(), 2);
        assert_eq!(view["cpu"]["perCore"][0]["frequencyMhz"], 3600);
    }

    #[test]
    fn zpool_sizes_are_parsed() {
        assert_eq!(parse_zfs_size("10G"), Some(10 * (1 << 30)));
        assert_eq!(parse_zfs_size("512M"), Some(512 * (1 << 20)));
        assert_eq!(
            parse_zfs_size("1.5T"),
            Some((1.5 * (1u64 << 40) as f64) as u64)
        );
        assert_eq!(parse_zfs_size("123"), Some(123));
        assert_eq!(parse_zfs_size("-"), None);
    }
}
