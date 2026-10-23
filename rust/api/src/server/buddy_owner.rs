//! Owner-facing buddy handlers (port of `server/buddy.go` and the identity part
//! of `server/buddy_send.go`).

use crate::buddy::keys::{load_identity, new_identity};
use crate::buddy::Peer;
use crate::json;
use crate::state::AppState;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use std::collections::HashMap;
use std::sync::Arc;

fn unavailable() -> Response {
    json(
        StatusCode::SERVICE_UNAVAILABLE,
        serde_json::json!({ "error": "Buddy Backup is not configured on this instance" }),
    )
}

fn bad_request(msg: &str) -> Response {
    json(StatusCode::BAD_REQUEST, serde_json::json!({ "error": msg }))
}

/// `GET /api/buddy/status`
pub async fn status(State(state): State<Arc<AppState>>) -> Response {
    let Some(receiver) = state.buddy.as_ref() else {
        return unavailable();
    };
    let free = match receiver.store.free_space() {
        Ok(v) => v,
        Err(e) => {
            return json(
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({ "error": format!("cannot read free space: {e}") }),
            )
        }
    };
    let used = match receiver.store.usage("") {
        Ok(v) => v,
        Err(e) => {
            return json(
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({ "error": format!("cannot read usage: {e}") }),
            )
        }
    };
    let backups = receiver.store.summary("").unwrap_or_default();
    json(
        StatusCode::OK,
        serde_json::json!({
            "enabled": true,
            "name": receiver.name,
            "version": receiver.version,
            "receivePath": receiver.store.root(),
            "freeBytes": free,
            "usedBytes": used,
            "enrollmentOpen": !receiver.enroll_token.is_empty(),
            "peers": receiver.peers.list(),
            "backups": backups,
        }),
    )
}

/// `GET|POST|DELETE /api/buddy/peers`
pub async fn peers_get(State(state): State<Arc<AppState>>) -> Response {
    let Some(receiver) = state.buddy.as_ref() else {
        return unavailable();
    };
    json(
        StatusCode::OK,
        serde_json::json!({ "peers": receiver.peers.list() }),
    )
}

#[derive(Default, serde::Deserialize)]
struct PeerRequest {
    #[serde(default)]
    name: String,
    #[serde(default, rename = "publicKey")]
    public_key: String,
    #[serde(default, rename = "allowedSources")]
    allowed_sources: Option<Vec<String>>,
    #[serde(default, rename = "quotaBytes")]
    quota_bytes: i64,
    #[serde(default)]
    enabled: Option<bool>,
}

pub async fn peers_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let Some(receiver) = state.buddy.as_ref() else {
        return unavailable();
    };
    let req: PeerRequest = match super::zfs::read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    if req.name.trim().is_empty() || req.public_key.trim().is_empty() {
        return bad_request("name and publicKey are required");
    }
    let existing = receiver.peers.get(&req.name);
    let mut peer = Peer {
        name: req.name.clone(),
        public_key: req.public_key.clone(),
        allowed_sources: req.allowed_sources.clone().unwrap_or_default(),
        quota_bytes: req.quota_bytes,
        enabled: true,
        ..Default::default()
    };
    if let Some(prev) = &existing {
        peer.created_at = prev.created_at;
        if req.allowed_sources.is_none() {
            peer.allowed_sources = prev.allowed_sources.clone();
        }
        if req.quota_bytes == 0 {
            peer.quota_bytes = prev.quota_bytes;
        }
    }
    if let Some(enabled) = req.enabled {
        peer.enabled = enabled;
    }
    match receiver.peers.add(peer.clone()) {
        Ok(()) => json(
            StatusCode::OK,
            serde_json::json!({ "peer": receiver.peers.get(&req.name).unwrap_or(peer) }),
        ),
        Err(e) => bad_request(&e),
    }
}

pub async fn peers_delete(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(receiver) = state.buddy.as_ref() else {
        return unavailable();
    };
    let name = query.get("name").cloned().unwrap_or_default();
    if name.is_empty() {
        return bad_request("name is required");
    }
    match receiver.peers.remove(&name) {
        Ok(()) => json(
            StatusCode::OK,
            serde_json::json!({
                "status": "revoked", "name": name,
                "note": "chunks already received stay until they are pruned: revoking a key stops new pushes, it is not a delete of existing backups",
            }),
        ),
        Err(e) => json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e })),
    }
}

/// `BUDDY_IDENTITY`, else the default path beside the other state files.
fn identity_path() -> String {
    std::env::var("BUDDY_IDENTITY")
        .unwrap_or_else(|_| "/var/lib/naslos/buddy-identity.json".to_string())
}

/// `GET|POST /api/buddy/identity`
pub async fn identity_get() -> Response {
    let path = identity_path();
    match load_identity(&path) {
        Ok(id) => {
            let fingerprint = match id.fingerprint() {
                Ok(f) => f,
                Err(e) => {
                    return json(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        serde_json::json!({ "error": e }),
                    )
                }
            };
            json(
                StatusCode::OK,
                serde_json::json!({
                    "exists": true, "name": id.name, "publicKey": id.public_key,
                    "fingerprint": fingerprint, "path": path, "createdAt": id.created_at,
                }),
            )
        }
        Err(_) if !std::path::Path::new(&path).exists() => json(
            StatusCode::OK,
            serde_json::json!({
                "exists": false, "path": path,
                "help": "create one with POST /api/buddy/identity; back the file up, losing it means losing the ability to read your backups",
            }),
        ),
        Err(e) => json(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "error": e }),
        ),
    }
}

pub async fn identity_post(body: Body) -> Response {
    let path = identity_path();
    // An empty body is allowed (create with defaults), like the Go io.EOF case.
    let req: IdentityRequest = super::zfs::read_json(body).await.unwrap_or_default();
    if std::path::Path::new(&path).exists() && !req.replace {
        return json(
            StatusCode::CONFLICT,
            serde_json::json!({
                "error": format!("an identity already exists at {path}: it is the only key that can read this instance's backups, so replacing it loses them. Send {{\"replace\": true}} if that is really what you want.")
            }),
        );
    }
    let mut name = req.name.trim().to_lowercase();
    if name.is_empty() {
        name = std::env::var("HOSTNAME")
            .or_else(|_| std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_string()))
            .unwrap_or_default()
            .to_lowercase();
    }
    if name.is_empty() {
        name = "naslos".to_string();
    }
    let identity = match new_identity(&name) {
        Ok(i) => i,
        Err(e) => {
            return json(
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({ "error": e }),
            )
        }
    };
    if let Err(e) = identity.save(&path) {
        return json(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "error": format!("saving the identity: {e}") }),
        );
    }
    let fingerprint = identity.fingerprint().unwrap_or_default();
    tracing::info!(
        "buddy: created a backup identity for {:?} ({fingerprint}) at {path}",
        identity.name
    );
    json(
        StatusCode::CREATED,
        serde_json::json!({
            "exists": true, "name": identity.name, "publicKey": identity.public_key,
            "fingerprint": fingerprint, "path": path,
            "warning": "back this file up somewhere your buddies cannot reach: it holds the only key that can read your backups",
        }),
    )
}

#[derive(Default, serde::Deserialize)]
struct IdentityRequest {
    #[serde(default)]
    name: String,
    #[serde(default)]
    replace: bool,
}
