//! User and group handlers (port of `api/internal/server/users*.go`).

use crate::json;
use crate::shares::PosixIdentity;
use crate::state::AppState;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use std::sync::Arc;

/// 503 when the identity/LDAP client is not configured.
fn identity_unavailable(state: &AppState) -> Option<Response> {
    if state.identity.is_none() {
        return Some(json(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({ "error": "Identity/LDAP is not configured (check LDAP_HOST, LDAP_BIND_PASS, and that OpenLDAP is running)" }),
        ));
    }
    None
}

#[allow(clippy::result_large_err)]
fn identity(state: &AppState) -> Result<Arc<crate::identity::Client>, Response> {
    state.identity.clone().ok_or_else(|| {
        json(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({ "error": "Identity/LDAP is not configured" }),
        )
    })
}

fn bad_request(msg: &str) -> Response {
    json(StatusCode::BAD_REQUEST, serde_json::json!({ "error": msg }))
}

// ---- Users ---------------------------------------------------------------

pub async fn users_get(State(state): State<Arc<AppState>>) -> Response {
    if let Some(resp) = identity_unavailable(&state) {
        return resp;
    }
    let client = match identity(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match client.list_people().await {
        Ok(people) => json(StatusCode::OK, serde_json::to_value(people).unwrap()),
        Err(e) => json(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "error": e }),
        ),
    }
}

#[derive(Default, serde::Deserialize)]
struct CreateUserBody {
    #[serde(default)]
    uid: String,
    #[serde(default, rename = "displayName")]
    display_name: String,
    #[serde(default)]
    email: String,
    #[serde(default, rename = "firstName")]
    first_name: String,
    #[serde(default, rename = "lastName")]
    last_name: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    groups: Vec<String>,
}

pub async fn users_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    if let Some(resp) = identity_unavailable(&state) {
        return resp;
    }
    let req: CreateUserBody = match super::zfs::read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    if req.uid.is_empty() || req.last_name.is_empty() {
        return bad_request("uid and lastName are required");
    }
    if req.password.is_empty() {
        return bad_request("password is required");
    }
    let client = match identity(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let person = match client
        .create_person(
            &req.uid,
            &req.display_name,
            &req.email,
            &req.first_name,
            &req.last_name,
        )
        .await
    {
        Ok(p) => p,
        Err(e) => return bad_request(&e),
    };
    match client.set_password(&req.uid, &req.password).await {
        Ok(nt_hash) => {
            if let Err(e) = sync_smb_password(&state, &req.uid, &nt_hash).await {
                return json(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    serde_json::json!({ "error": e }),
                );
            }
        }
        Err(e) => {
            return json(
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({ "error": e }),
            )
        }
    }
    for group in &req.groups {
        if let Err(e) = client.add_member(group, &req.uid).await {
            return json(
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({ "error": e }),
            );
        }
    }
    if let Err(e) = super::shares::apply_shares_config(&state).await {
        tracing::warn!("user created but pushing the share access mirror failed: {e}");
    }
    json(StatusCode::CREATED, serde_json::to_value(person).unwrap())
}

pub async fn user_detail_get(
    State(state): State<Arc<AppState>>,
    Path(uid): Path<String>,
) -> Response {
    if let Some(resp) = identity_unavailable(&state) {
        return resp;
    }
    let client = match identity(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match client.get_person(&uid).await {
        Ok(p) => json(StatusCode::OK, serde_json::to_value(p).unwrap()),
        Err(e) => json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e })),
    }
}

#[derive(Default, serde::Deserialize)]
struct UpdateUserBody {
    #[serde(default, rename = "displayName")]
    display_name: String,
    #[serde(default)]
    email: String,
    #[serde(default, rename = "firstName")]
    first_name: String,
    #[serde(default, rename = "lastName")]
    last_name: String,
    #[serde(default)]
    groups: Option<Vec<String>>,
}

pub async fn user_detail_put(
    State(state): State<Arc<AppState>>,
    Path(uid): Path<String>,
    body: Body,
) -> Response {
    if let Some(resp) = identity_unavailable(&state) {
        return resp;
    }
    let req: UpdateUserBody = match super::zfs::read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let client = match identity(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    if let Err(e) = client
        .update_person(
            &uid,
            &req.display_name,
            &req.email,
            &req.first_name,
            &req.last_name,
        )
        .await
    {
        return bad_request(&e);
    }

    if let Some(groups) = &req.groups {
        let person = match client.get_person(&uid).await {
            Ok(p) => p,
            Err(e) => return json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e })),
        };
        let mut desired = Vec::with_capacity(groups.len());
        for group in groups {
            match crate::identity::normalize_group_name(group) {
                Ok(normalized) => desired.push(normalized),
                Err(e) => return bad_request(&e),
            }
        }
        let (add, remove) = membership_delta(&person.groups, &desired, normalize_group);
        if !add.is_empty() || !remove.is_empty() {
            let membership_err = apply_membership_changes(
                &client,
                &MembershipTarget::UserGroups { uid: uid.clone() },
                &add,
                &remove,
            )
            .await;
            if let Err(e) = super::shares::apply_shares_config(&state).await {
                tracing::warn!("user updated but pushing the share access mirror failed: {e}");
            }
            if let Err(e) = membership_err {
                return json(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    serde_json::json!({ "error": e }),
                );
            }
        }
    }
    json(
        StatusCode::OK,
        serde_json::json!({ "status": "user updated" }),
    )
}

pub async fn user_detail_delete(
    State(state): State<Arc<AppState>>,
    Path(uid): Path<String>,
) -> Response {
    if let Some(resp) = identity_unavailable(&state) {
        return resp;
    }
    let client = match identity(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    if let Err(e) = client.delete_person(&uid).await {
        return bad_request(&e);
    }
    let _ = remove_smb_user(&state, &uid).await;
    json(
        StatusCode::OK,
        serde_json::json!({ "status": "user deleted" }),
    )
}

#[derive(Default, serde::Deserialize)]
struct PasswordBody {
    #[serde(default)]
    password: String,
}

pub async fn user_password_post(
    State(state): State<Arc<AppState>>,
    Path(uid): Path<String>,
    body: Body,
) -> Response {
    if let Some(resp) = identity_unavailable(&state) {
        return resp;
    }
    let req: PasswordBody = match super::zfs::read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    if req.password.is_empty() {
        return bad_request("password is required");
    }
    let client = match identity(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let nt_hash = match client.set_password(&uid, &req.password).await {
        Ok(h) => h,
        Err(e) => {
            return json(
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({ "error": e }),
            )
        }
    };
    if let Err(e) = sync_smb_password(&state, &uid, &nt_hash).await {
        return json(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "error": e }),
        );
    }
    json(
        StatusCode::OK,
        serde_json::json!({ "status": "password updated" }),
    )
}

pub async fn user_enable_post(
    State(state): State<Arc<AppState>>,
    Path((uid, action)): Path<(String, String)>,
) -> Response {
    if let Some(resp) = identity_unavailable(&state) {
        return resp;
    }
    let client = match identity(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let enabled = match action.as_str() {
        "enable" => true,
        "disable" => false,
        _ => return bad_request("invalid action"),
    };
    let result = if enabled {
        client.enable_person(&uid).await
    } else {
        client.disable_person(&uid).await
    };
    if let Err(e) = result {
        return json(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "error": e }),
        );
    }
    if let Err(e) = set_smb_user_enabled(&state, &uid, enabled).await {
        tracing::warn!(
            "could not mirror SMB account state for {}: {e}",
            crate::logsafe::field(&uid)
        );
    }
    let status = if enabled {
        "user enabled"
    } else {
        "user disabled"
    };
    json(StatusCode::OK, serde_json::json!({ "status": status }))
}

// ---- Groups --------------------------------------------------------------

pub async fn groups_get(State(state): State<Arc<AppState>>) -> Response {
    if let Some(resp) = identity_unavailable(&state) {
        return resp;
    }
    let client = match identity(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match client.list_groups().await {
        Ok(groups) => json(StatusCode::OK, serde_json::to_value(groups).unwrap()),
        Err(e) => json(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "error": e }),
        ),
    }
}

#[derive(Default, serde::Deserialize)]
struct CreateGroupBody {
    #[serde(default)]
    cn: String,
    #[serde(default)]
    description: String,
}

pub async fn groups_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    if let Some(resp) = identity_unavailable(&state) {
        return resp;
    }
    let req: CreateGroupBody = match super::zfs::read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let client = match identity(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match client.create_group(&req.cn, &req.description).await {
        Ok(g) => json(StatusCode::CREATED, serde_json::to_value(g).unwrap()),
        Err(e) => bad_request(&e),
    }
}

pub async fn group_detail_get(
    State(state): State<Arc<AppState>>,
    Path(cn): Path<String>,
) -> Response {
    if let Some(resp) = identity_unavailable(&state) {
        return resp;
    }
    let client = match identity(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match client.get_group(&cn).await {
        Ok(g) => json(StatusCode::OK, serde_json::to_value(g).unwrap()),
        Err(e) => json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e })),
    }
}

#[derive(Default, serde::Deserialize)]
struct UpdateGroupBody {
    #[serde(default)]
    members: Vec<String>,
}

pub async fn group_detail_put(
    State(state): State<Arc<AppState>>,
    Path(cn): Path<String>,
    body: Body,
) -> Response {
    if let Some(resp) = identity_unavailable(&state) {
        return resp;
    }
    let req: UpdateGroupBody = match super::zfs::read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let client = match identity(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let group = match client.get_group(&cn).await {
        Ok(g) => g,
        Err(e) => return json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e })),
    };
    let (add, remove) = membership_delta(&group.members, &req.members, extract_uid);
    if let Err(e) = apply_membership_changes(
        &client,
        &MembershipTarget::GroupMembers { cn: cn.clone() },
        &add,
        &remove,
    )
    .await
    {
        tracing::warn!(
            "group {} updated with errors: {e}",
            crate::logsafe::field(&cn)
        );
    }
    if let Err(e) = super::shares::apply_shares_config(&state).await {
        tracing::warn!("group membership changed but pushing it to the node failed: {e}");
    }
    json(
        StatusCode::OK,
        serde_json::json!({ "status": "group updated" }),
    )
}

pub async fn group_detail_delete(
    State(state): State<Arc<AppState>>,
    Path(cn): Path<String>,
) -> Response {
    if let Some(resp) = identity_unavailable(&state) {
        return resp;
    }
    let client = match identity(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    if let Err(e) = client.delete_group(&cn).await {
        return bad_request(&e);
    }
    if let Err(e) = super::shares::apply_shares_config(&state).await {
        tracing::warn!("group deleted but pushing it to the node failed: {e}");
    }
    json(
        StatusCode::OK,
        serde_json::json!({ "status": "group deleted" }),
    )
}

// ---- Helpers -------------------------------------------------------------

/// The entries to add/remove so that `current` becomes `desired`.
pub fn membership_delta(
    current: &[String],
    desired: &[String],
    normalize: impl Fn(&str) -> String,
) -> (Vec<String>, Vec<String>) {
    let mut present = std::collections::HashSet::new();
    for v in current {
        let v = normalize(v);
        if !v.is_empty() {
            present.insert(v);
        }
    }
    let mut wanted = std::collections::HashSet::new();
    for v in desired {
        let v = normalize(v);
        if !v.is_empty() {
            wanted.insert(v);
        }
    }

    let mut add = Vec::new();
    for v in desired {
        let v = normalize(v);
        if v.is_empty() || !present.insert(v.clone()) {
            continue;
        }
        add.push(v);
    }
    let mut remove = Vec::new();
    for v in current {
        let v = normalize(v);
        if v.is_empty() || !wanted.insert(v.clone()) {
            continue;
        }
        remove.push(v);
    }
    (add, remove)
}

/// Canonicalise an LDAP short group name (lowercase, trim, allowlist).
pub fn normalize_group(g: &str) -> String {
    match crate::identity::normalize_group_name(g) {
        Ok(n) => n,
        Err(_) => g.trim().to_lowercase(),
    }
}

use super::shares::extract_uid;

/// Apply every add then every remove, returning the first error.
async fn apply_membership_changes(
    client: &crate::identity::Client,
    target: &MembershipTarget,
    add: &[String],
    remove: &[String],
) -> Result<(), String> {
    let mut first_err: Option<String> = None;
    for item in add {
        let result = match target {
            MembershipTarget::UserGroups { uid } => client.add_member(item, uid).await,
            MembershipTarget::GroupMembers { cn } => client.add_member(cn, item).await,
        };
        if let Err(e) = result {
            if first_err.is_none() {
                first_err = Some(e);
            }
        }
    }
    for item in remove {
        let result = match target {
            MembershipTarget::UserGroups { uid } => client.remove_member(item, uid).await,
            MembershipTarget::GroupMembers { cn } => client.remove_member(cn, item).await,
        };
        if let Err(e) = result {
            if first_err.is_none() {
                first_err = Some(e);
            }
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Which side of a membership change an item is: the user's groups, or a
/// group's members.
pub enum MembershipTarget {
    UserGroups { uid: String },
    GroupMembers { cn: String },
}

/// Record the NT hash, persist it, and push the account file to the node.
async fn sync_smb_password(state: &AppState, uid: &str, nt_hash: &str) -> Result<(), String> {
    if uid.is_empty() || nt_hash.is_empty() {
        return Err("uid and NT hash are required to sync an SMB account".to_string());
    }
    let (uid_number, gid_number) = if let Some(identity) = state.identity.clone() {
        match identity.get_posix_ids(uid).await {
            Ok((u, g)) => (u, g),
            Err(e) => {
                tracing::warn!(
                    "could not read POSIX ids for {}: {e}",
                    crate::logsafe::field(uid)
                );
                (0, 0)
            }
        }
    } else {
        (0, 0)
    };
    state.samba_users.lock().unwrap().upsert(
        &PosixIdentity {
            uid: uid.to_string(),
            uid_num: uid_number,
            gid_num: gid_number,
            gecos: uid.to_string(),
            home_dir: String::new(),
            shell: String::new(),
        },
        nt_hash,
    )?;
    if let Err(e) = super::shares::apply_shares_config(state).await {
        return Err(format!(
            "SMB account recorded but pushing it to the node failed: {e}"
        ));
    }
    Ok(())
}

async fn remove_smb_user(state: &AppState, uid: &str) -> Result<(), String> {
    state.samba_users.lock().unwrap().remove(uid)?;
    if let Err(e) = super::shares::apply_shares_config(state).await {
        return Err(format!(
            "SMB account removed but pushing it to the node failed: {e}"
        ));
    }
    Ok(())
}

async fn set_smb_user_enabled(state: &AppState, uid: &str, enabled: bool) -> Result<(), String> {
    state
        .samba_users
        .lock()
        .unwrap()
        .set_enabled(uid, enabled)?;
    if let Err(e) = super::shares::apply_shares_config(state).await {
        return Err(format!(
            "SMB account state recorded but pushing it to the node failed: {e}"
        ));
    }
    Ok(())
}
