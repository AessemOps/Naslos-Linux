//! Disk discovery and topology recommendation (port of
//! `api/internal/server/disks.go`).

use crate::json;
use crate::state::AppState;
use crate::talos::{DiskInfo, VolumeAdvisor};
use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use serde::Deserialize;
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
