//! Disk discovery and topology recommendation (port of
//! `api/internal/server/disks.go`).

use crate::agent::{SmartInfo, ZfsDiskHealth};
use crate::json;
use crate::state::AppState;
use crate::talos::{DiskInfo, VolumeAdvisor};
use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// Keep only disks usable for ZFS: drop loop devices, optical drives and the
/// system disk.
fn is_real_disk(d: &crate::talos::Disk) -> bool {
    if d.system_disk || d.cdrom {
        return false;
    }
    matches!(d.disk_type.as_str(), "SSD" | "HDD" | "NVME" | "SD")
}

/// `GET /api/disks` — discovered disks, annotated with the pool that owns them.
pub async fn disks_get(State(state): State<Arc<AppState>>) -> Response {
    let Some(talos) = state.talos.clone() else {
        return json(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "error": "Talos client not configured" }),
        );
    };
    let disks = match talos.get_discovered_volumes().await {
        Ok(d) => d,
        Err(e) => {
            return json(
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({ "error": e }),
            )
        }
    };

    // Best effort for display; the destructive endpoints re-check and fail
    // closed, so an error here only means the badges are missing.
    let signatures = talos.get_discovered_signatures().await.unwrap_or_default();

    let mut in_pool: HashMap<String, String> = HashMap::new();
    if let Some(client) = state.agent.clone() {
        if let Ok(pools) = client.list_pools().await {
            for p in pools {
                for d in p.disks {
                    in_pool.insert(d, p.name.clone());
                }
            }
        }
    }

    let mut result = Vec::new();
    for d in disks.iter().filter(|d| is_real_disk(d)) {
        let mut info = serde_json::json!({
            "device": d.device_name,
            "size": d.size,
            "isSystemDisk": d.system_disk,
            "model": d.model,
            "serial": d.serial,
            "busPath": d.bus_path,
            "type": d.disk_type,
            "signatures": signatures.get(&d.device_name).cloned().unwrap_or_default(),
        });
        if let Some(pool) = in_pool.get(&d.device_name) {
            info["inPool"] = serde_json::json!(pool);
        }
        result.push(info);
    }
    json(StatusCode::OK, serde_json::Value::Array(result))
}

#[derive(Default, Deserialize)]
struct RecommendBody {
    #[serde(default)]
    disks: Vec<String>,
}

/// `POST /api/disks/recommend` — a topology recommendation for the selection.
pub async fn disks_recommend(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let req: RecommendBody = match super::zfs::read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let Some(talos) = state.talos.clone() else {
        return json(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "error": "Talos client not configured" }),
        );
    };
    let disks = match talos.get_discovered_volumes().await {
        Ok(d) => d,
        Err(e) => {
            return json(
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({ "error": e }),
            )
        }
    };

    // Restrict to the user's selection when it names real disks; otherwise fall
    // back to all discovered disks.
    let selected: std::collections::HashSet<&String> = req.disks.iter().collect();
    let filtered: Vec<&crate::talos::Disk> = if selected.is_empty() {
        Vec::new()
    } else {
        disks
            .iter()
            .filter(|d| selected.contains(&d.device_name))
            .collect()
    };
    let candidates: Vec<&crate::talos::Disk> = if filtered.is_empty() {
        disks.iter().collect()
    } else {
        filtered
    };
    let real: Vec<&crate::talos::Disk> = candidates
        .iter()
        .copied()
        .filter(|d| is_real_disk(d))
        .collect();
    let candidates: Vec<&crate::talos::Disk> = if real.is_empty() { candidates } else { real };

    let infos: Vec<DiskInfo> = candidates
        .iter()
        .map(|d| DiskInfo {
            device_path: d.device_name.clone(),
            size: d.size,
            is_system_disk: d.system_disk,
            model: d.model.clone(),
            serial: d.serial.clone(),
            is_ssd: d.disk_type == "SSD" || d.disk_type == "NVME",
        })
        .collect();

    match VolumeAdvisor::recommend(&infos) {
        Ok(rec) => json(StatusCode::OK, serde_json::to_value(rec).unwrap()),
        Err(e) => json(StatusCode::BAD_REQUEST, serde_json::json!({ "error": e })),
    }
}

#[derive(Serialize)]
struct DiskHealthEntry {
    device: String,
    size: u64,
    #[serde(rename = "isSystemDisk")]
    is_system_disk: bool,
    model: String,
    serial: String,
    #[serde(rename = "busPath")]
    bus_path: String,
    #[serde(rename = "type")]
    disk_type: String,
    signatures: Vec<String>,
    #[serde(rename = "inPool", skip_serializing_if = "Option::is_none")]
    in_pool: Option<String>,
    smart: SmartInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    zfs: Option<ZfsDiskHealth>,
}

#[derive(Serialize)]
struct DiskHealthResponse {
    #[serde(rename = "smartctlAvailable")]
    smartctl_available: bool,
    error: String,
    #[serde(rename = "temperatureWarningC")]
    temperature_warning_c: u64,
    #[serde(rename = "wearWarningPercent")]
    wear_warning_percent: u64,
    disks: Vec<DiskHealthEntry>,
}

fn threshold_from_env(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default)
}

fn unavailable_smart(message: &str) -> SmartInfo {
    SmartInfo {
        available: false,
        message: message.to_string(),
        ..Default::default()
    }
}

/// `GET /api/disks/health` — the Talos disk inventory merged with the agent's
/// SMART and ZFS data. Both halves are best-effort: a missing Talos client or
/// an older agent leaves the other half visible instead of failing the page.
pub async fn disks_health_get(State(state): State<Arc<AppState>>) -> Response {
    let mut errors: Vec<String> = Vec::new();

    let mut signatures = HashMap::new();
    let talos_disks = match state.talos.as_ref() {
        Some(talos) => match talos.get_discovered_volumes().await {
            Ok(disks) => {
                signatures = talos.get_discovered_signatures().await.unwrap_or_default();
                disks
            }
            Err(e) => {
                errors.push(format!("Talos disk inventory: {e}"));
                Vec::new()
            }
        },
        None => {
            errors.push("Talos client not configured".to_string());
            Vec::new()
        }
    };

    let report = match state.agent.as_ref() {
        Some(agent) => match agent.disk_health().await {
            Ok(report) => report,
            Err(e) => {
                errors.push(e.message.clone());
                crate::agent::DiskHealthReport {
                    smartctl_available: false,
                    error: e.message,
                    disks: Vec::new(),
                }
            }
        },
        None => {
            errors.push("Naslos agent not configured".to_string());
            crate::agent::DiskHealthReport {
                smartctl_available: false,
                error: "Naslos agent not configured".to_string(),
                disks: Vec::new(),
            }
        }
    };

    let mut entries: std::collections::BTreeMap<String, DiskHealthEntry> = talos_disks
        .into_iter()
        .filter(|d| !d.cdrom && !d.device_name.starts_with("/dev/loop"))
        .map(|d| {
            let entry = DiskHealthEntry {
                device: d.device_name.clone(),
                size: d.size,
                is_system_disk: d.system_disk,
                model: d.model,
                serial: d.serial,
                bus_path: d.bus_path,
                disk_type: d.disk_type,
                signatures: signatures.get(&d.device_name).cloned().unwrap_or_default(),
                in_pool: None,
                smart: unavailable_smart("SMART data was not reported by the agent"),
                zfs: None,
            };
            (d.device_name, entry)
        })
        .collect();

    for disk in report.disks {
        let entry = entries
            .entry(disk.device.clone())
            .or_insert_with(|| DiskHealthEntry {
                device: disk.device.clone(),
                size: 0,
                is_system_disk: false,
                model: String::new(),
                serial: String::new(),
                bus_path: String::new(),
                disk_type: String::new(),
                signatures: Vec::new(),
                in_pool: None,
                smart: unavailable_smart("SMART data was not reported by the agent"),
                zfs: None,
            });
        entry.in_pool = disk.zfs.as_ref().map(|z| z.pool.clone());
        entry.smart = disk.smart;
        entry.zfs = disk.zfs;
    }

    let mut disks: Vec<DiskHealthEntry> = entries.into_values().collect();
    disks.sort_by(|a, b| a.device.cmp(&b.device));

    // Keep the agent's per-scan reason (e.g. the bundled smartctl is missing)
    // visible to the page even when the agent itself answered 200.
    if !report.error.is_empty() && !errors.iter().any(|e| e == &report.error) {
        errors.push(report.error.clone());
    }

    json(
        StatusCode::OK,
        serde_json::to_value(DiskHealthResponse {
            smartctl_available: report.smartctl_available,
            error: errors.join("; "),
            temperature_warning_c: threshold_from_env("DISK_HEALTH_TEMPERATURE_C", 55),
            wear_warning_percent: threshold_from_env("DISK_HEALTH_WEAR_PERCENT", 80),
            disks,
        })
        .unwrap_or_else(|_| serde_json::json!({ "smartctlAvailable": false, "error": "serialization failed", "disks": [] })),
    )
}
