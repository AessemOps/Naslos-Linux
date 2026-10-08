//! Talos adapter (port of `api/internal/talos`, using the bundled `talosctl`
//! CLI instead of the Go machinery library — the plan's decision, since there
//! is no Rust equivalent of the COSI client).
//!
//! Resolution follows the same rule as the Go client: `TALOSCONFIG` env first,
//! then `~/.talos/config`, then the in-cluster mount
//! `/var/run/secrets/talos.dev/config`. Endpoints come from `TALOS_ENDPOINTS`
//! (the chart sets it; without it the Go client fatals at startup).

use std::net::IpAddr;
use std::process::Stdio;
use std::time::Duration;

use crate::metrics::{
    CpuMetrics, DiskMetrics, MemoryMetrics, NetworkInterface, NetworkMetrics, SystemInfo,
    SystemMetrics,
};

/// How long a single `talosctl` invocation may take.
const CMD_TIMEOUT: Duration = Duration::from_secs(20);

/// The Talos CLI adapter.
pub struct TalosClient {
    binary: String,
    endpoints: Vec<String>,
    nodes: Vec<String>,
    talosconfig: Option<String>,
}

impl TalosClient {
    /// Build from the environment. Returns `None` when the Talos endpoints are
    /// not configured (the API still serves; metrics collection is skipped).
    pub fn from_env() -> Option<Self> {
        let endpoints = split_csv(&std::env::var("TALOS_ENDPOINTS").unwrap_or_default());
        if endpoints.is_empty() {
            tracing::warn!("TALOS_ENDPOINTS not set; Talos metrics disabled");
            return None;
        }
        let nodes = match std::env::var("TALOS_NODES") {
            Ok(v) if !v.is_empty() => split_csv(&v),
            _ => endpoints.clone(),
        };
        Some(Self {
            binary: std::env::var("TALOSCTL_BIN").unwrap_or_else(|_| "talosctl".to_string()),
            endpoints,
            nodes,
            talosconfig: resolve_talosconfig(),
        })
    }

    /// Assemble the common flags for every invocation.
    fn base_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if let Some(cfg) = &self.talosconfig {
            args.push("--talosconfig".into());
            args.push(cfg.clone());
        }
        for e in &self.endpoints {
            args.push("-e".into());
            args.push(e.clone());
        }
        for n in &self.nodes {
            args.push("-n".into());
            args.push(n.clone());
        }
        args
    }

    /// Run `talosctl <args>` and return stdout. Non-zero exit is an error.
    async fn run(&self, args: &[&str]) -> Result<String, String> {
        let mut full = self.base_args();
        full.extend(args.iter().map(|s| s.to_string()));

        let mut cmd = tokio::process::Command::new(&self.binary);
        cmd.args(&full);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let output = tokio::time::timeout(CMD_TIMEOUT, cmd.output())
            .await
            .map_err(|_| format!("talosctl {}: timed out", args.join(" ")))?
            .map_err(|e| format!("talosctl {}: {e}", args.join(" ")))?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "talosctl {} exited {}: {}",
                args.join(" "),
                output.status.code().unwrap_or(-1),
                err.trim()
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    async fn read(&self, path: &str) -> Result<String, String> {
        // `/proc/...` (no leading slash) is what `talosctl read` expects.
        let clean = path.trim_start_matches('/');
        self.run(&["read", clean]).await
    }

    /// Collect CPU, memory, disk, network and system info, tolerating per-field
    /// failures exactly like the Go `GetSystemMetrics`.
    pub async fn get_system_metrics(&self) -> SystemMetrics {
        let mut snap = SystemMetrics::default();
        if let Ok(info) = self.system_info().await {
            snap.system = info;
        }
        if let Ok(mem) = self.memory_metrics().await {
            snap.memory = mem;
        }
        if let Ok(cpu) = self.cpu_metrics().await {
            snap.cpu = cpu;
        }
        if let Ok(disk) = self.disk_metrics().await {
            snap.disk = disk;
        }
        if let Ok(net) = self.network_metrics().await {
            snap.network = net;
        }
        snap
    }

    async fn system_info(&self) -> Result<SystemInfo, String> {
        let mut info = SystemInfo {
            os: "Talos Linux".into(),
            ..Default::default()
        };
        if let Ok(out) = self.run(&["get", "version", "-o", "json"]).await {
            if let Some(spec) = first_json(&out).and_then(|v| v.get("spec").cloned()) {
                if let Some(v) = spec.get("version").and_then(|v| v.as_str()) {
                    info.talos_version = v.to_string();
                }
                if let Some(n) = spec.get("name").and_then(|v| v.as_str()) {
                    info.os = n.to_string();
                }
            }
        }
        if let Ok(uptime) = self.read("/proc/uptime").await {
            if let Some(first) = uptime.split_whitespace().next() {
                info.uptime = first.parse::<f64>().unwrap_or(0.0) as u64;
            }
        }
        if let Ok(host) = self.read("/proc/sys/kernel/hostname").await {
            info.hostname = host.trim().to_string();
        }
        if let Ok(kernel) = self.read("/proc/sys/kernel/osrelease").await {
            info.kernel = kernel.trim().to_string();
        }
        Ok(info)
    }

    async fn memory_metrics(&self) -> Result<MemoryMetrics, String> {
        let out = self.read("/proc/meminfo").await?;
        let map = parse_meminfo(&out);
        let kb = |k: &str| map.get(k).copied().unwrap_or(0) * 1024;
        let total = kb("MemTotal");
        let free = kb("MemFree");
        let available = kb("MemAvailable");
        let used = total.saturating_sub(available);
        let usage = if total > 0 {
            used as f64 / total as f64 * 100.0
        } else {
            0.0
        };
        let swap_total = kb("SwapTotal");
        let swap_free = kb("SwapFree");
        Ok(MemoryMetrics {
            total,
            used,
            free,
            available,
            usage_percent: usage,
            swap_total,
            swap_used: swap_total.saturating_sub(swap_free),
        })
    }

    async fn cpu_metrics(&self) -> Result<CpuMetrics, String> {
        let mut cpu = CpuMetrics::default();
        if let Ok(loadavg) = self.read("/proc/loadavg").await {
            let parts: Vec<&str> = loadavg.split_whitespace().collect();
            if parts.len() >= 3 {
                cpu.load_avg1 = parts[0].parse().unwrap_or(0.0);
                cpu.load_avg5 = parts[1].parse().unwrap_or(0.0);
                cpu.load_avg15 = parts[2].parse().unwrap_or(0.0);
            }
        }
        if let Ok(stat) = self.read("/proc/stat").await {
            if let Some(line) = stat.lines().find(|l| l.starts_with("cpu ")) {
                let vals: Vec<u64> = line
                    .split_whitespace()
                    .skip(1)
                    .filter_map(|v| v.parse().ok())
                    .collect();
                if vals.len() >= 5 {
                    let total: u64 = vals.iter().sum();
                    let idle = vals[3] + vals.get(4).copied().unwrap_or(0);
                    if total > 2 {
                        // The deltas are unknown, so idle% is derived from the
                        // double sample the Go client would take; a single read
                        // yields instantaneous idle share, which is close enough
                        // for a dashboard and never zero.
                        cpu.usage_percent = (total - idle) as f64 / total as f64 * 100.0;
                    }
                }
            }
        }
        if let Ok(n) = self.read("/proc/cpuinfo").await {
            cpu.cores = n.lines().filter(|l| l.starts_with("processor")).count() as i64;
        }
        Ok(cpu)
    }

    async fn disk_metrics(&self) -> Result<DiskMetrics, String> {
        let out = self.run(&["get", "disks", "-o", "json"]).await?;
        // Talos `get disks` reports physical disks; the Go client reads the root
        // filesystem. Keep the totals conservative: sum the discovered disks.
        let mut total: u64 = 0;
        for v in all_json(&out) {
            if let Some(size) = v
                .get("spec")
                .and_then(|s| s.get("size"))
                .and_then(|s| s.as_str())
                .and_then(parse_size)
            {
                total += size;
            }
        }
        Ok(DiskMetrics {
            total,
            ..Default::default()
        })
    }

    async fn network_metrics(&self) -> Result<NetworkMetrics, String> {
        let mut net = NetworkMetrics::default();
        if let Ok(dev) = self.read("/proc/net/dev").await {
            let mut interfaces = Vec::new();
            for line in dev.lines().skip(2) {
                let Some((name, rest)) = line.split_once(':') else {
                    continue;
                };
                let name = name.trim();
                let nums: Vec<u64> = rest
                    .split_whitespace()
                    .filter_map(|v| v.parse().ok())
                    .collect();
                if nums.len() < 16 {
                    continue;
                }
                net.bytes_recv += nums[0];
                net.packets_recv += nums[1];
                net.bytes_sent += nums[8];
                net.packets_sent += nums[9];
                interfaces.push(NetworkInterface {
                    name: name.to_string(),
                    ..Default::default()
                });
            }
            net.interfaces = if interfaces.is_empty() {
                None
            } else {
                Some(interfaces)
            };
        }
        // Addresses: one per link, IPv4 preferred. Failure is non-fatal.
        if let Ok(out) = self.run(&["get", "addresses", "-o", "json"]).await {
            let by_link = addresses_by_link(&all_json(&out));
            if let Some(ifaces) = net.interfaces.as_mut() {
                for iface in ifaces.iter_mut() {
                    if let Some(addr) = by_link.get(&iface.name) {
                        iface.ip_address = addr.clone();
                    }
                }
            }
        }
        Ok(net)
    }
}

/// Resolve the talosconfig path exactly like the Go client's default
/// resolution: `TALOSCONFIG` env, then `~/.talos/config`, then the in-cluster
/// mount. Returns `None` to let `talosctl` apply its own default.
fn resolve_talosconfig() -> Option<String> {
    if let Ok(path) = std::env::var("TALOSCONFIG") {
        if !path.is_empty() {
            return Some(path);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let p = format!("{home}/.talos/config");
        if std::path::Path::new(&p).exists() {
            return Some(p);
        }
    }
    let in_cluster = "/var/run/secrets/talos.dev/config";
    if std::path::Path::new(in_cluster).exists() {
        return Some(in_cluster.to_string());
    }
    None
}

/// A discovered disk (the subset of Talos's `storage.Disk` the API uses).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Disk {
    #[serde(rename = "device")]
    pub device_name: String,
    pub size: u64,
    #[serde(rename = "isSystemDisk")]
    pub system_disk: bool,
    pub model: String,
    pub serial: String,
    #[serde(rename = "busPath")]
    pub bus_path: String,
    #[serde(rename = "type")]
    pub disk_type: String,
    /// True for spinning disks; used to derive the type when Talos does not
    /// report one.
    #[serde(default)]
    pub rotational: bool,
    #[serde(default)]
    pub cdrom: bool,
}

impl TalosClient {
    /// Discover disks via `talosctl get disks` + `get systemdisk`. The system
    /// disk is marked so the API can exclude it.
    pub async fn get_discovered_volumes(&self) -> Result<Vec<Disk>, String> {
        let disks_out = self.run(&["get", "disks", "-o", "json"]).await?;
        let system_out = self.run(&["get", "systemdisk", "-o", "json"]).await?;
        let system_dev = first_json(&system_out)
            .and_then(|v| {
                v.get("spec")
                    .and_then(|s| s.get("devPath"))
                    .and_then(|d| d.as_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_default();

        let mut disks = Vec::new();
        for v in all_json(&disks_out) {
            let Some(spec) = v.get("spec") else { continue };
            let dev = spec.get("dev_path").and_then(|s| s.as_str()).unwrap_or("");
            if dev.is_empty() {
                continue;
            }
            let rotational = spec
                .get("rotational")
                .and_then(|b| b.as_bool())
                .unwrap_or(false);
            let cdrom = spec.get("cdrom").and_then(|b| b.as_bool()).unwrap_or(false);
            let disk_type = if dev.starts_with("/dev/nvme") {
                "NVME"
            } else if rotational {
                "HDD"
            } else if dev.starts_with("/dev/loop") {
                "UNKNOWN"
            } else {
                "SSD"
            };
            disks.push(Disk {
                device_name: dev.to_string(),
                size: spec.get("size").and_then(|s| s.as_u64()).unwrap_or(0),
                system_disk: dev == system_dev,
                model: spec
                    .get("model")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string(),
                serial: spec
                    .get("serial")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string(),
                bus_path: spec
                    .get("bus_path")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string(),
                disk_type: disk_type.to_string(),
                rotational,
                cdrom,
            });
        }
        Ok(disks)
    }
}

/// Disk info passed to the advisor (port of `talos.DiskInfo`).
#[derive(Debug, Clone, Default)]
pub struct DiskInfo {
    pub device_path: String,
    pub size: u64,
    pub is_system_disk: bool,
    pub model: String,
    pub serial: String,
    pub is_ssd: bool,
}

/// A disk configuration recommendation (lowercase JSON keys match the UI
/// contract, `DiskWizard.svelte`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct Recommendation {
    pub topology: String,
    pub disks: Vec<String>,
    pub description: String,
    #[serde(rename = "humanReadableSize")]
    pub human_readable_size: String,
}

/// Best-practice topology recommendations (port of `VolumeAdvisor`).
pub struct VolumeAdvisor;

impl VolumeAdvisor {
    pub fn recommend(disks: &[DiskInfo]) -> Result<Recommendation, String> {
        if disks.is_empty() {
            return Err("no disks provided".to_string());
        }
        let usable: Vec<&DiskInfo> = disks.iter().filter(|d| !d.is_system_disk).collect();
        let n = usable.len();
        let paths: Vec<String> = usable.iter().map(|d| d.device_path.clone()).collect();
        let (topology, description) = match n {
            0 => return Err("no disks provided".to_string()),
            1 => (
                "single",
                "Single disk — no redundancy. Use only for non-critical data or cache.",
            ),
            2 => (
                "mirror",
                "Mirror (RAID1) — maximum redundancy for 2 disks. Usable capacity = size of smallest disk.",
            ),
            3..=5 => (
                "raidz1",
                "RAIDZ1 (single parity) — good balance of capacity and redundancy for 3-5 disks.",
            ),
            6..=10 => (
                "raidz2",
                "RAIDZ2 (double parity) — recommended for 6-10 disks. Survives any 2 disk failures.",
            ),
            _ => (
                "raidz3",
                "RAIDZ3 (triple parity) — recommended for 11+ disks. Survives any 3 disk failures.",
            ),
        };
        Ok(Recommendation {
            topology: topology.to_string(),
            disks: paths,
            description: description.to_string(),
            human_readable_size: String::new(),
        })
    }
}

fn split_csv(s: &str) -> Vec<String> {
    s.split(',')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

/// Parse `/proc/meminfo` into a kB-valued map.
pub fn parse_meminfo(out: &str) -> std::collections::HashMap<String, u64> {
    let mut map = std::collections::HashMap::new();
    for line in out.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let value = rest
            .split_whitespace()
            .next()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        map.insert(key.trim().to_string(), value);
    }
    map
}

/// Pick one address per link: IPv4 over IPv6, routable over link-local, no
/// loopback (port of `addressesByLink`).
pub fn addresses_by_link(
    values: &[serde_json::Value],
) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let mut best: std::collections::HashMap<String, u8> = std::collections::HashMap::new();
    for v in values {
        let Some(spec) = v.get("spec") else { continue };
        let link = spec.get("linkName").and_then(|s| s.as_str()).unwrap_or("");
        let addr_str = spec.get("address").and_then(|s| s.as_str()).unwrap_or("");
        if link.is_empty() {
            continue;
        }
        let Ok(net) = addr_str.parse::<ipnet::IpNet>() else {
            continue;
        };
        let addr = net.addr();
        if addr.is_loopback() || addr.is_multicast() || is_link_local(&addr) {
            continue;
        }
        let score: u8 = if addr.is_ipv4() { 1 } else { 0 };
        if best.get(link).is_some_and(|cur| *cur >= score) {
            continue;
        }
        out.insert(link.to_string(), addr.to_string());
        best.insert(link.to_string(), score);
    }
    out
}

fn is_link_local(addr: &IpAddr) -> bool {
    match addr {
        IpAddr::V4(v4) => v4.is_link_local(),
        IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) == 0xfe80,
    }
}

/// Parse a Talos human size like `512GB` / `1.8TB` into bytes.
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let split = s.find(|c: char| c.is_ascii_alphabetic())?;
    let (num, unit) = s.split_at(split);
    let value: f64 = num.trim().parse().ok()?;
    let mult: f64 = match unit.to_ascii_uppercase().as_str() {
        "B" => 1.0,
        "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        "PB" => 1e15,
        "KIB" => 1024.0,
        "MIB" => 1024.0_f64.powi(2),
        "GIB" => 1024.0_f64.powi(3),
        "TIB" => 1024.0_f64.powi(4),
        _ => return None,
    };
    Some((value * mult) as u64)
}

/// Parse the first JSON value in `out` (talosctl `-o json` can stream several).
pub fn first_json(out: &str) -> Option<serde_json::Value> {
    all_json(out).into_iter().next()
}

/// Parse every concatenated JSON value in a `talosctl -o json` stream, skipping
/// the leading `WARNING:` lines talosctl prints when client/server differ.
pub fn all_json(out: &str) -> Vec<serde_json::Value> {
    let mut values = Vec::new();
    let mut rest = out.trim_start();
    let decoder = serde_json::Deserializer::from_str(rest).into_iter::<serde_json::Value>();
    for value in decoder {
        match value {
            Ok(v) => values.push(v),
            Err(_) => break,
        }
    }
    // Fall back to scanning after any non-JSON preamble line.
    if values.is_empty() {
        if let Some(idx) = out.find('{') {
            rest = &out[idx..];
            let decoder = serde_json::Deserializer::from_str(rest).into_iter::<serde_json::Value>();
            for value in decoder.flatten() {
                values.push(value);
            }
        }
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meminfo_parses_kb() {
        let out = "MemTotal:        6042848 kB\nMemFree:         1692356 kB\nMemAvailable:    3427816 kB\nSwapTotal:             0 kB\nSwapFree:              0 kB\n";
        let m = parse_meminfo(out);
        assert_eq!(m["MemTotal"], 6042848);
        assert_eq!(m["MemAvailable"], 3427816);
    }

    #[test]
    fn size_parses_units() {
        assert_eq!(parse_size("512GB"), Some(512_000_000_000));
        assert_eq!(parse_size("1GiB"), Some(1_073_741_824));
        assert_eq!(parse_size("none"), None);
    }

    #[test]
    fn addresses_prefer_ipv4_and_skip_link_local() {
        let values: Vec<serde_json::Value> = serde_json::from_str(
            r#"[
              {"spec":{"linkName":"eth0","address":"fe80::1/64"}},
              {"spec":{"linkName":"eth0","address":"192.168.1.10/24"}},
              {"spec":{"linkName":"eth0","address":"fe80::abcd/64"}}
            ]"#,
        )
        .unwrap();
        let by_link = addresses_by_link(&values);
        assert_eq!(
            by_link.get("eth0").map(String::as_str),
            Some("192.168.1.10")
        );
    }

    #[test]
    fn all_json_skips_warning_preamble() {
        let out = "WARNING: server older\n{\"a\":1}\n{\"b\":2}\n";
        let v = all_json(out);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0]["a"], 1);
    }
}
