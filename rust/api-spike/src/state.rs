//! Shared state: env, auth config, and the metrics snapshot.

use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The dashboard metrics model, field-for-field with `api/internal/metrics`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct SystemMetrics {
    pub cpu: CpuMetrics,
    pub memory: MemoryMetrics,
    pub disk: DiskMetrics,
    pub network: NetworkMetrics,
    pub zfs: ZfsMetrics,
    pub system: SystemInfo,
    #[serde(rename = "updatedAt")]
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
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
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct DiskMetrics {
    pub total: u64,
    pub used: u64,
    pub free: u64,
    #[serde(rename = "usagePercent")]
    pub usage_percent: f64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct NetworkMetrics {
    #[serde(rename = "bytesSent")]
    pub bytes_sent: u64,
    #[serde(rename = "bytesRecv")]
    pub bytes_recv: u64,
    #[serde(rename = "packetsSent")]
    pub packets_sent: u64,
    #[serde(rename = "packetsRecv")]
    pub packets_recv: u64,
    pub interfaces: Vec<NetworkInterface>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct ZfsMetrics {
    pub pools: Vec<PoolMetrics>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct PoolMetrics {
    pub name: String,
    pub size: u64,
    pub alloc: u64,
    pub free: u64,
    #[serde(rename = "usagePercent")]
    pub usage_percent: f64,
    pub health: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct SystemInfo {
    pub hostname: String,
    pub uptime: u64,
    pub os: String,
    pub kernel: String,
    #[serde(rename = "talosVersion")]
    pub talos_version: String,
    #[serde(rename = "lastBoot")]
    pub last_boot: Option<chrono::DateTime<chrono::Utc>>,
}

pub struct AppState {
    pub proxy_secret: String,
    pub proxy_cidr: Cidr,
    pub metrics: Mutex<SystemMetrics>,
}

impl AppState {
    /// A state with an explicit secret and a permissive CIDR, for tests.
    pub fn for_test(proxy_secret: &str) -> Self {
        Self {
            proxy_secret: proxy_secret.to_string(),
            proxy_cidr: Cidr::parse("0.0.0.0/0"),
            metrics: Mutex::new(SystemMetrics::default()),
        }
    }

    pub fn from_env() -> Result<Self, String> {
        let proxy_secret = std::env::var("PROXY_SHARED_SECRET").unwrap_or_default();
        if proxy_secret.is_empty() {
            return Err(
                "PROXY_SHARED_SECRET is required; set it from the naslos-proxy Secret".into(),
            );
        }
        let cidr = std::env::var("TRAEFIK_CIDR").unwrap_or_else(|_| "10.0.0.0/8".to_string());
        Ok(Self {
            proxy_secret,
            proxy_cidr: Cidr::parse(&cidr),
            metrics: Mutex::new(SystemMetrics::default()),
        })
    }

    pub fn snapshot(&self) -> SystemMetrics {
        self.metrics.lock().unwrap().clone()
    }

    /// Go's `time.Time{}` zero value serializes as this RFC3339 string (not
    /// null), so an un-collected metrics manager must match it.
    pub const ZERO_TIME: &'static str = "0001-01-01T00:00:00Z";

    pub fn updated_at_json(&self) -> serde_json::Value {
        let m = self.metrics.lock().unwrap();
        match m.updated_at {
            Some(t) => serde_json::Value::String(t.to_rfc3339()),
            None => serde_json::Value::String(Self::ZERO_TIME.to_string()),
        }
    }

    pub fn update(&self, mut m: SystemMetrics) {
        m.updated_at = Some(chrono::Utc::now());
        *self.metrics.lock().unwrap() = m;
    }

    /// The dashboard projection (port of `Manager.GetDashboardData`).
    pub fn dashboard_data(&self) -> serde_json::Value {
        let m = self.metrics.lock().unwrap();
        let updated_at = match m.updated_at {
            Some(t) => serde_json::Value::String(t.to_rfc3339()),
            None => serde_json::Value::String(Self::ZERO_TIME.to_string()),
        };
        // Go serializes nil slices as null; keep empty vectors null too.
        let interfaces = if m.network.interfaces.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::to_value(&m.network.interfaces).unwrap()
        };
        let pools = if m.zfs.pools.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::to_value(&m.zfs.pools).unwrap()
        };
        serde_json::json!({
            "cpu": { "usage": m.cpu.usage_percent, "cores": m.cpu.cores },
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
            "zfs": { "poolCount": m.zfs.pools.len(), "pools": pools },
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
                "interfaces": interfaces,
            },
            "updatedAt": updated_at,
        })
    }

    /// Collect one metrics snapshot. The spike reads the fields a real port
    /// would get from the Talos client (hostname, kernel, load, memory); it is
    /// intentionally dependency-free for a memory measurement.
    pub fn collect_once(&self) {
        let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname")
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let mut info = SystemInfo {
            hostname,
            os: "Talos".to_string(),
            ..Default::default()
        };
        if let Ok(uptime) = std::fs::read_to_string("/proc/uptime") {
            if let Some(first) = uptime.split_whitespace().next() {
                info.uptime = first.parse::<f64>().unwrap_or(0.0) as u64;
            }
        }
        self.update(SystemMetrics {
            system: info,
            ..Default::default()
        });
    }

    /// Metrics collector loop (port of `startMetricsCollector`).
    pub async fn run_metrics_collector(self: Arc<Self>) {
        let interval = std::env::var("METRICS_INTERVAL_SECONDS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|n| *n > 0)
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(5));
        self.collect_once();
        let mut ticker = tokio::time::interval(interval);
        ticker.tick().await; // the immediate tick is already collected above
        loop {
            ticker.tick().await;
            self.collect_once();
        }
    }
}

/// Minimal CIDR match (IPv4/IPv6) mirroring the Go auth's `TRAEFIK_CIDR`.
pub struct Cidr {
    base: Option<(IpAddr, u8)>,
}

impl Cidr {
    pub fn parse(s: &str) -> Self {
        match s.trim().parse::<ipnet::IpNet>() {
            Ok(net) => Self {
                base: Some((net.addr(), net.prefix_len())),
            },
            Err(_) => Self { base: None },
        }
    }

    pub fn contains(&self, ip: &IpAddr) -> bool {
        let Some((base, len)) = self.base else {
            return true; // unparseable CIDR: fail open like a dev default
        };
        match (base, ip) {
            (IpAddr::V4(b), IpAddr::V4(x)) => {
                let net = ipnet::Ipv4Net::new(b, len).ok();
                net.map(|n| n.contains(x)).unwrap_or(true)
            }
            (IpAddr::V6(b), IpAddr::V6(x)) => {
                let net = ipnet::Ipv6Net::new(b, len).ok();
                net.map(|n| n.contains(x)).unwrap_or(true)
            }
            _ => false,
        }
    }
}
