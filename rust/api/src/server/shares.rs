//! Share handlers (port of `api/internal/server/shares.go`).

use crate::agent::SharesConfigRequest;
use crate::json;
use crate::logsafe;
use crate::shares::{CreateShareRequest, NssGroup, UpdateShareRequest};
use crate::state::AppState;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use std::collections::HashMap;
use std::sync::Arc;

#[allow(clippy::result_large_err)]
fn agent(state: &AppState) -> Result<Arc<crate::agent::Client>, Response> {
    state.agent.clone().ok_or_else(|| {
        json(
            StatusCode::BAD_GATEWAY,
            serde_json::json!({ "error": "agent client not configured" }),
        )
    })
}

pub async fn shares_get(State(state): State<Arc<AppState>>) -> Response {
    let shares = state.shares.lock().unwrap().list();
    json(StatusCode::OK, serde_json::to_value(shares).unwrap())
}

pub async fn shares_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let req: CreateShareRequest = match super::zfs::read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    if let Err(e) = require_dataset_path(&state, &req.path).await {
        return bad_request(&e);
    }
    if let Err(e) = require_share_path_exist(&state, &req.path) {
        return bad_request(&e);
    }
    let created = match state.shares.lock().unwrap().create(&req) {
        Ok(s) => s,
        Err(e) => return bad_request(&e),
    };
    let _ = apply_shares_config(&state).await;
    json(StatusCode::CREATED, serde_json::to_value(created).unwrap())
}

pub async fn share_detail_get(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    match state.shares.lock().unwrap().get(&name) {
        Ok(s) => json(StatusCode::OK, serde_json::to_value(s).unwrap()),
        Err(e) => json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e })),
    }
}

pub async fn share_detail_put(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    body: Body,
) -> Response {
    let req: UpdateShareRequest = match super::zfs::read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    if let Some(path) = &req.path {
        if let Err(e) = require_dataset_path(&state, path).await {
            return bad_request(&e);
        }
        if let Err(e) = require_share_path_exist(&state, path) {
            return bad_request(&e);
        }
    }
    let updated = match state.shares.lock().unwrap().update(&name, &req) {
        Ok(s) => s,
        Err(e) => return bad_request(&e),
    };
    let _ = apply_shares_config(&state).await;
    json(StatusCode::OK, serde_json::to_value(updated).unwrap())
}

pub async fn share_detail_delete(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    if let Err(e) = state.shares.lock().unwrap().delete(&name) {
        return json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e }));
    }
    let _ = apply_shares_config(&state).await;
    json(
        StatusCode::OK,
        serde_json::json!({ "status": "share deleted", "name": name }),
    )
}

pub async fn share_paths_get(State(state): State<Arc<AppState>>) -> Response {
    match dataset_mountpoints(&state).await {
        Ok(paths) => {
            let base = state.shares.lock().unwrap().zfs_base().to_string();
            json(
                StatusCode::OK,
                serde_json::json!({ "base": base, "paths": paths }),
            )
        }
        Err(e) => json(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({ "error": format!("cannot list shareable datasets: {e}") }),
        ),
    }
}

pub async fn share_folders_get(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let path = query.get("path").cloned().unwrap_or_default();
    if let Err(e) = require_dataset_path(&state, &path).await {
        return bad_request(&e);
    }
    let client = match agent(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match client.list_share_folders(&path).await {
        Ok(folders) => {
            let base = state.shares.lock().unwrap().zfs_base().to_string();
            json(
                StatusCode::OK,
                serde_json::json!({ "base": base, "path": path, "folders": folders }),
            )
        }
        Err(e) => json(
            StatusCode::BAD_GATEWAY,
            serde_json::json!({ "error": e.to_string() }),
        ),
    }
}

pub async fn share_folders_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    #[derive(serde::Deserialize, Default)]
    struct Req {
        #[serde(default)]
        path: String,
        #[serde(default)]
        name: String,
    }
    let req: Req = match super::zfs::read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    if let Err(e) = require_dataset_path(&state, &req.path).await {
        return bad_request(&e);
    }
    let client = match agent(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match client.create_share_folder(&req.path, &req.name).await {
        Ok(created) => json(StatusCode::CREATED, serde_json::json!({ "path": created })),
        Err(e) => bad_request(&e.to_string()),
    }
}

pub async fn share_folders_delete(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let path = query.get("path").cloned().unwrap_or_default();
    if let Err(e) = require_dataset_path(&state, &path).await {
        return bad_request(&e);
    }
    let client = match agent(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match client.delete_share_folder(&path).await {
        Ok(()) => json(
            StatusCode::OK,
            serde_json::json!({ "status": "folder removed", "path": path }),
        ),
        Err(e) => bad_request(&e.to_string()),
    }
}

pub async fn shares_status_get(State(state): State<Arc<AppState>>) -> Response {
    let bundle = state.shares.lock().unwrap().render_config_bundle();
    let mut response = serde_json::json!({
        "revision": bundle.revision,
        "shareCount": bundle.share_count,
        "agent": serde_json::Value::Null,
        "agentError": "",
    });
    match agent(&state) {
        Ok(client) => match client.get_shares_status().await {
            Ok(status) => response["agent"] = serde_json::to_value(status).unwrap(),
            Err(e) => response["agentError"] = serde_json::Value::String(e.to_string()),
        },
        Err(_) => {
            response["agentError"] = serde_json::Value::String("agent client not configured".into())
        }
    }
    json(StatusCode::OK, response)
}

pub async fn shares_apply_post(State(state): State<Arc<AppState>>) -> Response {
    match apply_shares_config(&state).await {
        Ok(status) => json(StatusCode::OK, serde_json::to_value(status).unwrap()),
        Err(e) => json(StatusCode::BAD_GATEWAY, serde_json::json!({ "error": e })),
    }
}

pub async fn samba_config_get(State(state): State<Arc<AppState>>) -> Response {
    let conf = state.shares.lock().unwrap().generate_samba_config();
    text(conf)
}

pub async fn nfs_config_get(State(state): State<Arc<AppState>>) -> Response {
    let conf = state.shares.lock().unwrap().generate_ganesha_config();
    text(conf)
}

fn text(body: String) -> Response {
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "text/plain")],
        body,
    )
        .into_response()
}

use axum::response::IntoResponse;

fn bad_request(msg: &str) -> Response {
    json(StatusCode::BAD_REQUEST, serde_json::json!({ "error": msg }))
}

/// The mountpoints of all datasets on the node.
async fn dataset_mountpoints(state: &AppState) -> Result<Vec<String>, String> {
    let client = state
        .agent
        .clone()
        .ok_or_else(|| "agent client not configured".to_string())?;
    let datasets = client.list_datasets().await.map_err(|e| e.to_string())?;
    let mut paths: Vec<String> = datasets
        .into_iter()
        .filter(|d| {
            crate::shares::path_on_dataset(&d.mountpoint, std::slice::from_ref(&d.mountpoint)).1
        })
        .map(|d| d.mountpoint)
        .collect();
    paths.sort();
    Ok(paths)
}

/// Reject a share path that is not on a ZFS dataset.
async fn require_dataset_path(state: &AppState, path: &str) -> Result<(), String> {
    let client = state.agent.clone().ok_or_else(|| {
        format!("cannot verify that {path} is on a ZFS dataset (agent unavailable)")
    })?;
    let datasets = client.list_datasets().await.map_err(|e| {
        format!("cannot verify that {path} is on a ZFS dataset (agent unavailable): {e}")
    })?;
    let mountpoints: Vec<String> = datasets.into_iter().map(|d| d.mountpoint).collect();
    if crate::shares::path_on_dataset(path, &mountpoints).1 {
        return Ok(());
    }
    Err(format!(
        "{path} is not on a ZFS dataset, so its data would live on the node's ephemeral partition (no snapshots, no redundancy, lost on upgrade). Create a dataset under one of {} first",
        mountpoints.join(", ")
    ))
}

/// Confirm the share path exists as a folder (skipped when the datasets mount is absent).
fn require_share_path_exist(state: &AppState, path: &str) -> Result<(), String> {
    let base = state.shares.lock().unwrap().zfs_base().to_string();
    if std::fs::metadata(&base).is_err() {
        return Ok(());
    }
    state.shares.lock().unwrap().validate_path(path)
}

/// Convert LDAP groups into the extrausers form (members reduced to uids).
async fn ldap_nss_groups(state: &AppState) -> Vec<NssGroup> {
    let Some(identity) = state.identity.clone() else {
        return Vec::new();
    };
    let Ok(groups) = identity.list_groups().await else {
        tracing::warn!("could not read LDAP groups for share access lists");
        return Vec::new();
    };
    groups
        .into_iter()
        .map(|g| NssGroup {
            members: g
                .members
                .iter()
                .filter_map(|dn| {
                    let uid = extract_uid(dn);
                    if uid.is_empty() {
                        None
                    } else {
                        Some(uid)
                    }
                })
                .collect(),
            gid: crate::shares::group_gid(&g.cn),
            name: g.cn,
        })
        .collect()
}

/// Render the share configuration and push it to the agent.
pub async fn apply_shares_config(
    state: &AppState,
) -> Result<crate::agent::SharesConfigStatus, String> {
    let bundle = state.shares.lock().unwrap().render_config_bundle();

    // Resolve LDAP group membership BEFORE locking the account store: a
    // std::sync::MutexGuard must never be held across an await (the handler
    // future would stop being Send).
    let nss_groups = ldap_nss_groups(state).await;
    let (samba_users, nss_passwd, nss_group, nss_shadow) = {
        let store = state.samba_users.lock().unwrap();
        (
            store.render_smbpasswd(),
            store.render_passwd(),
            store.render_group(&nss_groups),
            store.render_shadow(),
        )
    };

    let client = state
        .agent
        .clone()
        .ok_or_else(|| "agent client not configured".to_string())?;
    let req = SharesConfigRequest {
        samba_conf: bundle.samba_conf,
        ganesha_conf: bundle.ganesha_conf,
        samba_users,
        nss_passwd,
        nss_group,
        nss_shadow,
        revision: bundle.revision.clone(),
        share_count: bundle.share_count,
    };
    match client.apply_shares_config(&req).await {
        Ok(status) => Ok(status),
        Err(e) => {
            tracing::warn!(
                "failed to apply shares config (revision {}): {e}",
                logsafe::field(&bundle.revision)
            );
            Err(e.to_string())
        }
    }
}

/// Reduce a member reference (DN or plain uid) to a bare uid.
pub fn extract_uid(dn: &str) -> String {
    let dn = dn.trim();
    if dn.is_empty() {
        return String::new();
    }
    if !dn.contains('=') {
        return dn.to_string();
    }
    let first = dn.split(',').next().unwrap_or("");
    let mut value = first.strip_prefix("uid=").unwrap_or(first).to_string();
    value = value
        .replace("\\2C", ",")
        .replace("\\3D", "=")
        .replace("\\2c", ",")
        .replace("\\3d", "=");
    if let Some(idx) = value.rfind("uid=") {
        value = value[idx + 4..].to_string();
    }
    value.trim().to_string()
}
