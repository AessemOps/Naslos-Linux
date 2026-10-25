//! Pods and namespaces for the terminal's pickers (port of `server/pods.go`).

use crate::json;
use crate::state::AppState;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use std::collections::HashMap;
use std::sync::Arc;

const TERMINAL_LABEL: &str = "naslos-terminal";

/// A DNS-1123 subdomain/label, so a caller-supplied name cannot be interpolated
/// into an API path.
pub fn validate_kube_name(name: &str, what: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(format!("{what} is required"));
    }
    if name.len() > 253 {
        return Err(format!("{what} is too long"));
    }
    // DNS-1123 subdomain: lowercase alphanumerics, '-' and '.', each label
    // starting and ending alphanumeric. Rejecting a leading '-' matters: the
    // value becomes a bare kubectl positional, where a leading '-' would be
    // parsed as a flag (`--server=…`).
    if name.split('.').any(|label| {
        label.is_empty()
            || label.len() > 63
            || !label
                .chars()
                .next()
                .map(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                .unwrap_or(false)
            || !label
                .chars()
                .last()
                .map(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                .unwrap_or(false)
            || !label
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    }) {
        return Err(format!("invalid {what} {name:?}"));
    }
    Ok(())
}

pub fn default_namespace() -> String {
    std::env::var("NASLOS_NAMESPACE").unwrap_or_else(|_| "naslos".to_string())
}

#[allow(clippy::result_large_err)]
fn kube(state: &AppState) -> Result<Arc<crate::kube::Client>, Response> {
    state.kube.clone().ok_or_else(|| {
        json(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({ "error": "cannot reach the Kubernetes API: no client configured" }),
        )
    })
}

/// `GET /api/namespaces`
pub async fn namespaces_get(State(state): State<Arc<AppState>>) -> Response {
    let kube = match kube(&state) {
        Ok(k) => k,
        Err(resp) => return resp,
    };
    let list = match kube.list_cluster("namespaces").await {
        Ok(v) => v,
        Err(e) => {
            return json(
                StatusCode::BAD_GATEWAY,
                serde_json::json!({ "error": format!("listing namespaces: {e}") }),
            )
        }
    };
    let mut names: Vec<String> = list
        .get("items")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|i| {
                    i.get("metadata")
                        .and_then(|m| m.get("name"))
                        .and_then(|n| n.as_str())
                })
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    json(StatusCode::OK, serde_json::to_value(names).unwrap())
}

/// `GET /api/pods?namespace=`
pub async fn pods_get(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let namespace = query.get("namespace").cloned().unwrap_or_default();
    if !namespace.is_empty() {
        if let Err(e) = validate_kube_name(&namespace, "namespace") {
            return json(StatusCode::BAD_REQUEST, serde_json::json!({ "error": e }));
        }
    }
    let kube = match kube(&state) {
        Ok(k) => k,
        Err(resp) => return resp,
    };
    let list = match kube.list_pods(&namespace).await {
        Ok(v) => v,
        Err(e) => {
            return json(
                StatusCode::BAD_GATEWAY,
                serde_json::json!({ "error": format!("listing pods: {e}") }),
            )
        }
    };
    let mut pods = Vec::new();
    if let Some(items) = list.get("items").and_then(|v| v.as_array()) {
        for pod in items {
            let meta = pod.get("metadata").cloned().unwrap_or_default();
            let spec = pod.get("spec").cloned().unwrap_or_default();
            let status = pod.get("status").cloned().unwrap_or_default();
            let name = meta
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let ns = meta
                .get("namespace")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let containers: Vec<String> = spec
                .get("containers")
                .and_then(|v| v.as_array())
                .map(|cs| {
                    cs.iter()
                        .filter_map(|c| c.get("name").and_then(|n| n.as_str()))
                        .map(|s| s.to_string())
                        .collect()
                })
                .unwrap_or_default();
            let ready = status
                .get("conditions")
                .and_then(|v| v.as_array())
                .map(|cs| {
                    cs.iter().any(|c| {
                        c.get("type") == Some(&serde_json::json!("Ready"))
                            && c.get("status") == Some(&serde_json::json!("True"))
                    })
                })
                .unwrap_or(false);
            let terminal = meta
                .get("labels")
                .and_then(|l| l.get("app.kubernetes.io/name"))
                .and_then(|v| v.as_str())
                == Some(TERMINAL_LABEL);
            pods.push(serde_json::json!({
                "name": name,
                "namespace": ns,
                "containers": containers,
                "phase": status.get("phase").and_then(|v| v.as_str()).unwrap_or(""),
                "ready": ready,
                "terminal": terminal,
            }));
        }
    }
    pods.sort_by(|a, b| {
        let an = a["namespace"].as_str().unwrap_or("");
        let bn = b["namespace"].as_str().unwrap_or("");
        an.cmp(bn).then(
            a["name"]
                .as_str()
                .unwrap_or("")
                .cmp(b["name"].as_str().unwrap_or("")),
        )
    });
    json(StatusCode::OK, serde_json::Value::Array(pods))
}

#[cfg(test)]
mod tests {
    use super::validate_kube_name;

    #[test]
    fn accepts_dns1123_names() {
        for ok in ["naslos", "naslos-terminal", "a.b-c", "ns1"] {
            assert!(validate_kube_name(ok, "pod").is_ok(), "{ok}");
        }
    }

    #[test]
    fn rejects_flag_like_and_malformed_names() {
        for bad in [
            "--insecure-skip-tls-verify",
            "-n",
            ".hidden",
            "trailing-",
            "UPPER",
            "with space",
            "a..b",
            "under_score",
            "x".repeat(64).as_str(),
        ] {
            assert!(validate_kube_name(bad, "pod").is_err(), "{bad}");
        }
    }
}
