//! Base-domain handlers (port of `api/internal/server/domains.go`).

use crate::certs::Domain;
use crate::json;
use crate::state::AppState;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use std::collections::HashMap;
use std::sync::Arc;

/// A domain create/update body: the domain fields plus the selected provider's
/// credential `fields`. `solver` is write-only in responses, so an update that
/// omits it must preserve the stored value; `solver_submitted` distinguishes
/// an omitted key from an explicit `null`/empty object.
#[derive(Default, serde::Deserialize)]
struct DomainRequest {
    #[serde(flatten)]
    domain: Domain,
    #[serde(default)]
    fields: Option<HashMap<String, String>>,
    #[serde(skip)]
    solver_submitted: bool,
}

/// Decode a domain request while recording whether `solver` was present.
#[allow(clippy::result_large_err)]
async fn read_domain_request(body: Body) -> Result<DomainRequest, Response> {
    let value: serde_json::Value = super::zfs::read_json(body).await?;
    let solver_submitted = value.get("solver").is_some();
    let mut req: DomainRequest = serde_json::from_value(value).map_err(|e| {
        json(
            StatusCode::BAD_REQUEST,
            serde_json::json!({ "error": e.to_string() }),
        )
    })?;
    req.solver_submitted = solver_submitted;
    Ok(req)
}

fn solver_is_explicitly_empty(solver: &serde_json::Value) -> bool {
    solver.is_null() || solver.as_object().map(|m| m.is_empty()).unwrap_or(false)
}

fn bad_request(msg: &str) -> Response {
    json(StatusCode::BAD_REQUEST, serde_json::json!({ "error": msg }))
}

/// The base domains an app exposure may use: the primary first, then every
/// registered domain, de-duplicated.
pub fn selectable_domains(state: &AppState) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut add = |d: String| {
        if d.is_empty() || !seen.insert(d.clone()) {
            return;
        }
        out.push(d);
    };
    add(state.base_domain.clone());
    for d in state.domains.list() {
        add(d.base_domain);
    }
    out
}

/// The domains Authelia protects: the primary plus the chart seed plus any
/// store domain promoted to SSO.
pub fn effective_sso_domains(state: &AppState) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut add = |d: String| {
        if d.is_empty() || !seen.insert(d.clone()) {
            return;
        }
        out.push(d);
    };
    add(state.base_domain.clone());
    for d in &state.sso_domains {
        add(d.clone());
    }
    for d in state.domains.list() {
        if d.sso {
            add(d.base_domain);
        }
    }
    out
}

/// Whether a non-empty base domain is one of the configured domains.
pub fn base_domain_selectable(state: &AppState, base_domain: &str) -> bool {
    selectable_domains(state).iter().any(|d| d == base_domain)
}

/// Installed apps whose exposure requires auth on the given base domain.
fn apps_using_auth(state: &AppState, base_domain: &str) -> Vec<String> {
    let Some(manager) = &state.app_manager else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for rec in manager.records() {
        if !rec.exposure.auth {
            continue;
        }
        let domain = if rec.base_domain.is_empty() {
            state.base_domain.clone()
        } else {
            rec.base_domain.clone()
        };
        if domain == base_domain {
            names.push(rec.name);
        }
    }
    names.sort();
    names
}

async fn sync_sso_state(state: &AppState) -> Result<(), String> {
    let Some(sso) = &state.sso else {
        return Ok(());
    };
    sso.sync(&effective_sso_domains(state)).await
}

/// The GET projection of a domain. The cert-manager `solver` and any
/// credential-bearing `providerConfig` values are never echoed; only their
/// presence is reported.
fn domain_view(domain: &Domain) -> serde_json::Value {
    let mut value = serde_json::to_value(domain).unwrap_or_default();
    let mut safe = serde_json::Map::new();
    let mut redacted: Vec<String> = Vec::new();
    for (key, val) in &domain.provider_config {
        if crate::server::config_export::is_sensitive_key(key)
            || crate::server::config_export::is_free_form_value_key(key)
            || crate::server::config_export::value_looks_sensitive(val)
        {
            redacted.push(key.clone());
        } else {
            safe.insert(key.clone(), serde_json::json!(val));
        }
    }
    redacted.sort();
    if let Some(obj) = value.as_object_mut() {
        obj.remove("solver");
        obj.insert(
            "solverConfigured".to_string(),
            serde_json::json!(domain.solver.is_some()),
        );
        if safe.is_empty() {
            obj.remove("providerConfig");
        } else {
            obj.insert(
                "providerConfig".to_string(),
                serde_json::Value::Object(safe),
            );
        }
        if !redacted.is_empty() {
            obj.insert(
                "redactedProviderConfigKeys".to_string(),
                serde_json::json!(redacted),
            );
        }
    }
    value
}

/// `GET|POST /api/domains`
pub async fn domains_get(State(state): State<Arc<AppState>>) -> Response {
    let domains: Vec<serde_json::Value> = state
        .domains
        .list()
        .iter()
        .map(|domain| {
            let mut value = domain_view(domain);
            value["appRecordsSupported"] =
                serde_json::json!(crate::dnsrecords::provider_supported(&domain.dns_provider));
            value
        })
        .collect();
    json(
        StatusCode::OK,
        serde_json::json!({
            "domains": domains,
            "certManager": state.certs.is_some(),
            "baseDomain": state.base_domain,
            "selectableDomains": selectable_domains(&state),
            "ssoDomains": effective_sso_domains(&state),
        }),
    )
}

pub async fn domains_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let mut req = match read_domain_request(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    req.domain.primary = false;
    upsert_domain(&state, req, StatusCode::CREATED).await
}

/// `GET|PUT|DELETE /api/domains/{name}`
pub async fn domain_detail_get(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    match state.domains.get(&name) {
        Ok(d) => {
            let mut value = domain_view(&d);
            value["appRecordsSupported"] =
                serde_json::json!(crate::dnsrecords::provider_supported(&d.dns_provider));
            json(StatusCode::OK, value)
        }
        Err(e) => json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e })),
    }
}

pub async fn domain_detail_put(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    body: Body,
) -> Response {
    let mut req = match read_domain_request(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    if req.domain.base_domain.is_empty() {
        req.domain.base_domain = name;
    }
    upsert_domain(&state, req, StatusCode::OK).await
}

pub async fn domain_detail_delete(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    let domain = match state.domains.get(&name) {
        Ok(d) => d,
        Err(e) => return json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e })),
    };
    // Remove owned app records before the domain (and its credentials) goes
    // away, so the provider API is still reachable.
    if let Some(manager) = &state.app_manager {
        manager.remove_dns_for_domain(&name).await;
    }
    if let Some(certs) = &state.certs {
        if let Err(e) = certs.delete(&domain).await {
            return json(StatusCode::BAD_GATEWAY, serde_json::json!({ "error": e }));
        }
    }
    if let Err(e) = state.domains.delete(&name) {
        return json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e }));
    }
    if domain.sso || name == state.base_domain {
        if let Err(e) = sync_sso_state(&state).await {
            tracing::warn!(
                "Authelia SSO sync after removing {} failed: {e}",
                crate::logsafe::field(&name)
            );
        }
    }
    json(
        StatusCode::OK,
        serde_json::json!({ "status": "domain removed", "name": name }),
    )
}

/// `POST /api/domains/{name}/sso`
pub async fn domain_sso_post(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    body: Body,
) -> Response {
    #[derive(Default, serde::Deserialize)]
    struct Req {
        #[serde(default)]
        enabled: bool,
    }
    let req: Req = match super::zfs::read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    if name == state.base_domain {
        return bad_request("the primary domain is always an SSO domain");
    }
    if let Err(e) = state.domains.get(&name) {
        return json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e }));
    }
    if !req.enabled {
        if state.sso_domains.iter().any(|d| d == &name) {
            return json(
                StatusCode::CONFLICT,
                serde_json::json!({ "error": "this domain is declared in the chart's SSO list (SSO_DOMAINS); remove it from values before demoting" }),
            );
        }
        let apps = apps_using_auth(&state, &name);
        if !apps.is_empty() {
            return json(
                StatusCode::CONFLICT,
                serde_json::json!({ "error": format!("cannot disable SSO: installed apps still require auth on {name}: {}", apps.join(", ")) }),
            );
        }
    }
    let domain = match state.domains.update(&name, |d| {
        d.sso = req.enabled;
        d.updated_at = Some(chrono::Utc::now());
    }) {
        Ok(d) => d,
        Err(e) => return json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e })),
    };
    if let Err(e) = sync_sso_state(&state).await {
        tracing::warn!("Authelia SSO sync failed: {e}");
    }
    json(
        StatusCode::OK,
        serde_json::json!({
            "domain": domain_view(&domain),
            "domains": state.domains.list().iter().map(domain_view).collect::<Vec<_>>(),
            "ssoDomains": effective_sso_domains(&state),
        }),
    )
}

/// `GET /api/domains/{name}/certificate`
pub async fn domain_certificate_get(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    let domain = match state.domains.get(&name) {
        Ok(d) => d,
        Err(e) => return json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e })),
    };
    let Some(certs) = &state.certs else {
        return json(
            StatusCode::OK,
            serde_json::json!({
                "status": "unavailable",
                "reason": "cert-manager is not installed",
                "secretName": domain.secret_name(),
            }),
        );
    };
    match certs.status(&domain).await {
        Ok(status) => json(StatusCode::OK, status),
        Err(e) => json(StatusCode::BAD_GATEWAY, serde_json::json!({ "error": e })),
    }
}

async fn upsert_domain(state: &Arc<AppState>, req: DomainRequest, status: StatusCode) -> Response {
    let DomainRequest {
        mut domain,
        fields,
        solver_submitted,
    } = req;
    if domain.environment.is_empty() {
        domain.environment = "staging".to_string();
    }
    if domain
        .app_records_ddns_entry
        .as_deref()
        .map(str::trim)
        .map(|id| id.is_empty())
        .unwrap_or(false)
    {
        domain.app_records_ddns_entry = None;
    }
    let existing = state.domains.get(&domain.base_domain).ok();
    // Preserve write-only fields that an update omitted. The UI never receives
    // the solver or credential values back, so omission must not erase them.
    if let Some(prev) = &existing {
        if domain.credentials_secret.is_empty() {
            domain.credentials_secret = prev.credentials_secret.clone();
        }
        if !solver_submitted {
            domain.solver = prev.solver.clone();
        }
        if fields.is_none() {
            domain.provider_config = prev.provider_config.clone();
        }
    }
    // An explicit empty solver clears it only for provider-rendered solvers;
    // passthrough still needs one and is rejected by `validate` below.
    if solver_submitted
        && domain
            .solver
            .as_ref()
            .map(solver_is_explicitly_empty)
            .unwrap_or(false)
    {
        domain.solver = None;
    }
    if let Err(e) = validate_app_records(state, &domain) {
        return bad_request(&e);
    }
    if let Some(fields) = &fields {
        if let Err(e) = apply_domain_fields(state, &mut domain, fields, existing.as_ref()).await {
            return bad_request(&e);
        }
    }
    if let Err(e) = domain.validate() {
        return bad_request(&e);
    }
    let now = chrono::Utc::now();
    if let Some(prev) = &existing {
        domain.created_at = prev.created_at;
        // The SSO flag is owned by POST /api/domains/{domain}/sso.
        domain.sso = prev.sso;
    } else {
        domain.created_at = Some(now);
    }
    domain.updated_at = Some(now);

    match &state.certs {
        Some(certs) => match certs.apply(&domain).await {
            Ok(()) => domain.last_error.clear(),
            Err(e) => domain.last_error = e,
        },
        None => domain.last_error = "cert-manager is not installed".to_string(),
    }
    if let Err(e) = state.domains.upsert(domain.clone()) {
        return json(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "error": e }),
        );
    }
    if let Some(manager) = &state.app_manager {
        manager.reconcile_dns_for_domain(&domain.base_domain).await;
    }
    let mut value = domain_view(&domain);
    value["appRecordsSupported"] =
        serde_json::json!(crate::dnsrecords::provider_supported(&domain.dns_provider));
    json(status, value)
}

/// Validate the opt-in link between a domain and a DDNS entry.
fn validate_app_records(state: &AppState, domain: &Domain) -> Result<(), String> {
    let Some(entry_id) = domain
        .app_records_ddns_entry
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    else {
        return Ok(());
    };
    if !crate::dnsrecords::provider_supported(&domain.dns_provider) {
        return Err(format!(
            "provider {:?} cannot create app DNS records; use ovh or cloudflare",
            domain.dns_provider
        ));
    }
    let Some(manager) = &state.ddns else {
        return Err("dynamic DNS is not enabled; app DNS records need a DDNS entry".to_string());
    };
    let entry = manager.store().get(entry_id)?;
    if entry.record.trim().starts_with("*.") {
        return Err("a wildcard DDNS entry cannot be an app-record target".to_string());
    }
    Ok(())
}

/// Split a submitted field map into the non-secret providerConfig and a
/// credential Secret.
async fn apply_domain_fields(
    state: &Arc<AppState>,
    domain: &mut Domain,
    fields: &HashMap<String, String>,
    existing: Option<&Domain>,
) -> Result<(), String> {
    let p = state
        .providers
        .get(&domain.dns_provider)
        .ok_or_else(|| format!("unsupported DNS provider {:?}", domain.dns_provider))?;
    if !p.supports_certificates() {
        return Err(format!(
            "provider {:?} does not support certificates",
            p.name
        ));
    }
    if p.is_passthrough() {
        return Ok(());
    }
    let create = existing.is_none();
    let existing_config = existing.map(|d| d.provider_config.clone());
    let resolved = p.resolve_fields("cert", fields, existing_config.as_ref(), &[], create)?;
    domain.provider_config = resolved.config.clone();

    let secret_name = if domain.credentials_secret.is_empty() {
        format!("naslos-domain-{}-creds", sanitize_name(&domain.base_domain))
    } else {
        crate::kube::validate_credentials_secret(&domain.credentials_secret)?;
        domain.credentials_secret.clone()
    };
    p.validate_solver(&secret_name, &resolved.config)?;
    if resolved.secret_values.is_empty() && resolved.removed_secret_keys.is_empty() {
        return Ok(());
    }
    let Some(kube) = &state.kube else {
        return Err("cannot reach the Kubernetes API to store credentials".to_string());
    };
    if !resolved.secret_values.is_empty() {
        kube.write_secret(&state.apps_namespace, &secret_name, &resolved.secret_values)
            .await
            .map_err(|e| format!("storing credentials: {e}"))?;
        domain.credentials_secret = secret_name.clone();
    }
    if !resolved.removed_secret_keys.is_empty() {
        crate::ddns::remove_secret_keys(
            kube,
            &state.apps_namespace,
            &secret_name,
            &resolved.removed_secret_keys,
        )
        .await
        .map_err(|e| format!("revoking credentials: {e}"))?;
    }
    Ok(())
}

/// Reduce a domain to a DNS-1123-safe component.
fn sanitize_name(name: &str) -> String {
    let mapped: String = name
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                c
            } else {
                '-'
            }
        })
        .collect();
    mapped.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_view_redacts_solver_and_credentials() {
        let domain = Domain {
            base_domain: "example.com".into(),
            solver: Some(serde_json::json!({ "webhook": { "token": "leak" } })),
            provider_config: HashMap::from([
                ("endpoint".to_string(), "ovh-eu".to_string()),
                ("applicationKey".to_string(), "legacy-key".to_string()),
            ]),
            ..Default::default()
        };
        let value = domain_view(&domain);
        let text = value.to_string();
        assert!(!text.contains("leak"), "{text}");
        assert!(!text.contains("legacy-key"), "{text}");
        assert_eq!(value["solverConfigured"], true);
        assert_eq!(value["providerConfig"]["endpoint"], "ovh-eu");
        assert!(value.get("solver").is_none(), "{text}");
    }

    #[test]
    fn sanitize_name_reduces_domains() {
        assert_eq!(sanitize_name("Example.COM"), "example-com");
        assert_eq!(sanitize_name("media.example.com"), "media-example-com");
    }
}
