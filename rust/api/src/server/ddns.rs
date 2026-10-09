//! Dynamic-DNS handlers (port of `api/internal/server/ddns.go`).

use crate::ddns::{self, Entry};
use crate::json;
use crate::state::AppState;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use std::collections::HashMap;
use std::sync::Arc;

fn bad_request(msg: &str) -> Response {
    json(StatusCode::BAD_REQUEST, serde_json::json!({ "error": msg }))
}

/// A DNS-1123 label.
fn is_dns_label(s: &str) -> bool {
    if s.is_empty() || s.len() > 63 {
        return false;
    }
    let b = s.as_bytes();
    if !b[0].is_ascii_lowercase() && !b[0].is_ascii_digit() {
        return false;
    }
    if !b[b.len() - 1].is_ascii_lowercase() && !b[b.len() - 1].is_ascii_digit() {
        return false;
    }
    b.iter()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// A DNS name / subdomain.
fn is_dns_subdomain(s: &str) -> bool {
    !s.is_empty() && s.len() <= 253 && s.split('.').all(is_dns_label)
}

fn validate_record_label(record: &str) -> Result<(), String> {
    if record.is_empty() || record == "@" {
        return Ok(());
    }
    let stripped = record.strip_prefix("*.").unwrap_or(record);
    if !is_dns_label(stripped) {
        return Err(format!("record {record:?} is not a valid DNS label"));
    }
    Ok(())
}

/// `GET|POST /api/ddns`
pub async fn ddns_get(State(state): State<Arc<AppState>>) -> Response {
    match &state.ddns {
        Some(manager) => json(
            StatusCode::OK,
            serde_json::json!({
                "entries": manager.store().list(),
                "enabled": true,
                "intervalSeconds": manager.interval().as_secs(),
            }),
        ),
        None => json(
            StatusCode::OK,
            serde_json::json!({ "entries": [], "enabled": false }),
        ),
    }
}

#[derive(Default, serde::Deserialize)]
struct DdnsRequest {
    #[serde(default)]
    provider: String,
    #[serde(default)]
    zone: String,
    #[serde(default)]
    record: String,
    #[serde(default, rename = "recordType")]
    record_type: String,
    #[serde(default)]
    ttl: i64,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    fields: HashMap<String, String>,
}

pub async fn ddns_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let req: DdnsRequest = match super::zfs::read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    upsert(&state, None, req).await
}

/// `GET|PUT|DELETE /api/ddns/{id}`
pub async fn ddns_detail_get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let Some(manager) = &state.ddns else {
        return json(
            StatusCode::NOT_FOUND,
            serde_json::json!({ "error": "ddns entry not found" }),
        );
    };
    match manager.store().get(&id) {
        Ok(e) => json(StatusCode::OK, serde_json::to_value(e).unwrap()),
        Err(e) => json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e })),
    }
}

pub async fn ddns_detail_put(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Body,
) -> Response {
    let Some(manager) = &state.ddns else {
        return json(
            StatusCode::NOT_FOUND,
            serde_json::json!({ "error": "ddns entry not found" }),
        );
    };
    let existing = match manager.store().get(&id) {
        Ok(e) => e,
        Err(e) => return json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e })),
    };
    let req: DdnsRequest = match super::zfs::read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    upsert(&state, Some(existing), req).await
}

pub async fn ddns_detail_delete(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let Some(manager) = &state.ddns else {
        return json(
            StatusCode::NOT_FOUND,
            serde_json::json!({ "error": "ddns entry not found" }),
        );
    };
    let entry = match manager.store().get(&id) {
        Ok(e) => e,
        Err(e) => return json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e })),
    };
    if let Err(e) = manager.store().delete(&id) {
        return json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e }));
    }
    if let (Some(kube), false) = (&state.kube, entry.credentials_secret.is_empty()) {
        if let Err(e) =
            ddns::delete_secret(kube, &state.apps_namespace, &entry.credentials_secret).await
        {
            tracing::warn!(
                "could not delete DDNS Secret {}: {e}",
                entry.credentials_secret
            );
        }
    }
    json(
        StatusCode::OK,
        serde_json::json!({ "status": "ddns entry removed", "id": id }),
    )
}

/// `POST /api/ddns/{id}/run`
pub async fn ddns_run_post(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let Some(manager) = &state.ddns else {
        return json(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({ "error": "dynamic DNS is not enabled" }),
        );
    };
    if let Err(e) = manager.store().get(&id) {
        return json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e }));
    }
    match manager.run(&id, true).await {
        Ok(()) => {
            let entry = manager.store().get(&id).ok();
            json(StatusCode::OK, serde_json::to_value(entry).unwrap())
        }
        Err(e) => {
            let entry = manager.store().get(&id).ok();
            json(
                StatusCode::BAD_GATEWAY,
                serde_json::json!({ "error": e, "entry": entry }),
            )
        }
    }
}

async fn upsert(state: &Arc<AppState>, existing: Option<Entry>, req: DdnsRequest) -> Response {
    let Some(manager) = &state.ddns else {
        return json(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({ "error": "dynamic DNS is not enabled" }),
        );
    };

    let provider_name = if !req.provider.trim().is_empty() {
        req.provider.trim().to_string()
    } else if let Some(e) = &existing {
        e.provider.clone()
    } else {
        String::new()
    };
    let p = match state.providers.get(&provider_name) {
        Some(p) => p,
        None => return bad_request(&format!("unknown provider {provider_name:?}")),
    };
    if !p.has_ddns() {
        return bad_request(&format!(
            "provider {provider_name:?} does not support dynamic DNS"
        ));
    }

    let zone = if !req.zone.trim().is_empty() {
        req.zone.trim().to_string()
    } else if let Some(e) = &existing {
        e.zone.clone()
    } else {
        String::new()
    };
    if !is_dns_subdomain(&zone) {
        return bad_request(&format!("zone {zone:?} is not a valid DNS name"));
    }
    let record = if !req.record.trim().is_empty() {
        req.record.trim().to_string()
    } else if let Some(e) = &existing {
        e.record.clone()
    } else {
        String::new()
    };
    if let Err(e) = validate_record_label(&record) {
        return bad_request(&e);
    }
    let record_type = if !req.record_type.trim().is_empty() {
        req.record_type.trim().to_uppercase()
    } else if let Some(e) = &existing {
        e.record_type.clone()
    } else {
        String::new()
    };
    if record_type != "A" && record_type != "AAAA" {
        return bad_request("recordType must be A or AAAA");
    }
    let ttl = if req.ttl == 0 {
        existing.as_ref().map(|e| e.ttl).unwrap_or(0)
    } else {
        req.ttl
    };
    if ttl != 0 && !(60..=86400).contains(&ttl) {
        return bad_request("ttl must be 0 (provider default) or between 60 and 86400");
    }

    let existing_config = existing.as_ref().map(|e| e.provider_config.clone());
    let existing_fields = existing
        .as_ref()
        .map(|e| e.credential_fields.clone())
        .unwrap_or_default();
    let resolved = match p.resolve_fields(
        "ddns",
        &req.fields,
        existing_config.as_ref(),
        &existing_fields,
        existing.is_none(),
    ) {
        Ok(r) => r,
        Err(e) => return bad_request(&e),
    };

    let now = chrono::Utc::now();
    let mut entry = if let Some(e) = &existing {
        let mut e = e.clone();
        e.provider = provider_name;
        e.zone = zone;
        e.record = record;
        e.record_type = record_type;
        e.ttl = ttl;
        e.updated_at = Some(now);
        e.last_status.clear();
        e.last_error.clear();
        e
    } else {
        Entry {
            id: ddns::new_id(),
            provider: provider_name,
            zone,
            record,
            record_type,
            ttl,
            enabled: true,
            created_at: Some(now),
            updated_at: Some(now),
            ..Default::default()
        }
    };
    if let Some(enabled) = req.enabled {
        entry.enabled = enabled;
    }
    entry.provider_config = resolved.config;
    entry.credential_fields = resolved.credential_fields;

    if !resolved.secret_values.is_empty() {
        let secret_name = if entry.credentials_secret.is_empty() {
            entry.secret_name()
        } else {
            entry.credentials_secret.clone()
        };
        let Some(kube) = &state.kube else {
            return json(
                StatusCode::SERVICE_UNAVAILABLE,
                serde_json::json!({ "error": "cannot reach the Kubernetes API to store credentials" }),
            );
        };
        if let Err(e) = ddns::write_secret(
            kube,
            &state.apps_namespace,
            &secret_name,
            &resolved.secret_values,
        )
        .await
        {
            return json(
                StatusCode::BAD_GATEWAY,
                serde_json::json!({ "error": format!("storing credentials: {e}") }),
            );
        }
        entry.credentials_secret = secret_name;
    }
    if !p.secret_fields().is_empty() && entry.credentials_secret.is_empty() {
        for f in p.secret_fields() {
            if f.required {
                return bad_request(&format!("provider {:?} requires credentials", p.name));
            }
        }
    }

    if let Err(e) = manager.store().put(entry.clone()) {
        return json(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "error": e }),
        );
    }
    let status = if existing.is_some() {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    json(status, serde_json::to_value(entry).unwrap())
}

/// Split a comma-separated public-IP source list, falling back to a legacy
/// single source.
pub fn ddns_sources(list: &str, fallback: &str) -> Vec<String> {
    let out: Vec<String> = list
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if !out.is_empty() {
        return out;
    }
    let fallback = fallback.trim();
    if !fallback.is_empty() {
        return vec![fallback.to_string()];
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_label_validation() {
        assert!(validate_record_label("home").is_ok());
        assert!(validate_record_label("@").is_ok());
        assert!(validate_record_label("").is_ok());
        assert!(validate_record_label("*.home").is_ok());
        assert!(validate_record_label("bad label").is_err());
    }

    #[test]
    fn sources_split() {
        assert_eq!(ddns_sources("a,b , c", ""), vec!["a", "b", "c"]);
        assert_eq!(ddns_sources("", "x"), vec!["x"]);
    }
}
