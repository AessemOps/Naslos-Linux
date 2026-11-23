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

/// Redact credential-bearing values from a runtime error before it is stored
/// or echoed. A failed cert-manager apply can quote the rendered Issuer, which
/// for a passthrough solver is the operator-supplied (credential-bearing)
/// solver object; provider config values that the GET projection already
/// treats as sensitive can leak the same way.
fn redact_error(domain: &Domain, text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let mut needles: Vec<String> = Vec::new();
    if let Some(solver) = &domain.solver {
        collect_solver_strings(solver, &mut needles);
    }
    for (key, value) in &domain.provider_config {
        if crate::server::config_export::is_sensitive_key(key)
            || crate::server::config_export::is_free_form_value_key(key)
            || crate::server::config_export::value_looks_sensitive(value)
        {
            needles.push(value.clone());
        }
    }
    // Longest first, so a needle embedded in a longer one is replaced whole.
    needles.sort_by_key(|needle| std::cmp::Reverse(needle.len()));
    let mut out = text.to_string();
    for needle in needles {
        // Very short values would corrupt ordinary words in the message.
        if needle.trim().len() >= 4 {
            out = out.replace(&needle, "[redacted]");
        }
    }
    out
}

fn collect_solver_strings(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(items) => {
            for item in items {
                collect_solver_strings(item, out);
            }
        }
        serde_json::Value::Object(map) => {
            for item in map.values() {
                collect_solver_strings(item, out);
            }
        }
        _ => {}
    }
}

/// The GET projection of a domain. The cert-manager `solver` and any
/// credential-bearing `providerConfig` values are never echoed; only their
/// presence is reported. `lastError` is sanitized against the stored solver
/// and provider values, because an apply error can quote them.
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
        if !domain.last_error.is_empty() {
            obj.insert(
                "lastError".to_string(),
                serde_json::json!(redact_error(domain, &domain.last_error)),
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

/// 503 while the domain store did not load: mutations must fail closed
/// BEFORE any cluster side effect (credential Secrets, Issuers,
/// Certificates, DNS records), so a corrupt registry cannot leave partial
/// cluster state behind.
fn domain_store_unavailable(state: &AppState) -> Option<Response> {
    state.domains.require_loaded().err().map(|error| {
        json(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({ "error": error }),
        )
    })
}

pub async fn domains_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    if let Some(response) = domain_store_unavailable(&state) {
        return response;
    }
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
    if let Some(response) = domain_store_unavailable(&state) {
        return response;
    }
    let mut req = match read_domain_request(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    // The path names the record; a body that renames the domain would create
    // or overwrite a different record than the URL and audit target claim.
    if !req.domain.base_domain.is_empty() && req.domain.base_domain != name {
        return bad_request("baseDomain in the request body must match the domain in the path");
    }
    req.domain.base_domain = name;
    upsert_domain(&state, req, StatusCode::OK).await
}

pub async fn domain_detail_delete(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    if let Some(response) = domain_store_unavailable(&state) {
        return response;
    }
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
    if let Some(response) = domain_store_unavailable(&state) {
        return response;
    }
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
    // Reject a malformed name before touching provider fields or a Secret, and
    // keep the DNS error ahead of the provider-field errors.
    if !crate::certs::is_dns1123_subdomain(&domain.base_domain) {
        return bad_request(&format!(
            "base domain {:?} is not a valid DNS name",
            domain.base_domain
        ));
    }
    let provider_changed = existing
        .as_ref()
        .map(|prev| prev.dns_provider != domain.dns_provider)
        .unwrap_or(false);
    // A provider change swaps the credential model. Demand the new provider's
    // fields unless the target renders no provider-supplied credentials:
    // passthrough uses the submitted raw solver, and a provider with no secret
    // credential fields (rfc2136) renders from config/defaults plus the stored
    // Secret name. Everything else would point the new solver at the old
    // provider's Secret keys.
    let no_fields = fields.as_ref().map(|f| f.is_empty()).unwrap_or(true);
    if provider_changed {
        let solver_ready = solver_submitted
            && domain
                .solver
                .as_ref()
                .map(|solver| !solver_is_explicitly_empty(solver))
                .unwrap_or(false);
        if let Some(provider) = state.providers.get(&domain.dns_provider) {
            if provider.is_passthrough() {
                // The raw solver is the whole credential model: never fall back
                // to a stale solver from the previous provider.
                if !solver_ready {
                    return bad_request(
                        "changing dnsProvider to passthrough requires submitting a solver",
                    );
                }
            } else if no_fields && provider_change_requires_fields(provider) {
                return bad_request(
                    "changing dnsProvider requires submitting the new provider's fields",
                );
            }
        }
    }
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
    // A create may name a credentials Secret the operator already provisioned
    // out of band (the documented MCP `domain_create` path: credentialsSecret
    // plus providerConfig, no fields). In that case the Secret name and the
    // submitted config are validated, but the provider's fields are not
    // required because the resolver cannot write the Secret it cannot see.
    let supplied_credentials_only =
        existing.is_none() && no_fields && !domain.credentials_secret.is_empty();
    // Resolve the provider fields on every write, not only when `fields` was
    // submitted: an omitted key preserves the stored config, but a create or a
    // provider change must still satisfy the provider's required secret fields
    // and the effective solver must be re-validated (I-3).
    let submitted_fields = fields.unwrap_or_default();
    let enforce_credentials =
        !supplied_credentials_only && (existing.is_none() || provider_changed);
    if let Err(e) = apply_domain_fields(
        state,
        &mut domain,
        &submitted_fields,
        existing.as_ref(),
        enforce_credentials,
        provider_changed,
        supplied_credentials_only,
    )
    .await
    {
        return bad_request(&e);
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
            Err(e) => domain.last_error = redact_error(&domain, &e),
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

/// Whether a provider's solver renders credentials from its fields. Such a
/// provider needs a `fields` submission (or an out-of-band Secret) when it is
/// the target of a provider change.
fn provider_change_requires_fields(provider: &crate::providers::Provider) -> bool {
    provider.fields_for("cert").iter().any(|field| field.secret)
}

/// Split a submitted field map into the non-secret providerConfig and a
/// credential Secret. The resolver runs on every write for cert-manager
/// providers; `create_or_changed` enforces the provider's required secret
/// fields when there is no stored credential model to fall back on.
#[allow(clippy::too_many_arguments)]
async fn apply_domain_fields(
    state: &Arc<AppState>,
    domain: &mut Domain,
    fields: &HashMap<String, String>,
    existing: Option<&Domain>,
    create_or_changed: bool,
    provider_changed: bool,
    supplied_credentials_only: bool,
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
        if provider_changed {
            // The raw solver replaces any provider-rendered credential config.
            domain.provider_config.clear();
        }
        return Ok(());
    }
    // An operator-supplied Secret name must stay inside the Naslos convention
    // (SEC-2026-D12). Validate it before resolving fields or writing a Secret,
    // including the omission path where no resolver used to run at all.
    if !domain.credentials_secret.is_empty() {
        crate::kube::validate_credentials_secret(&domain.credentials_secret)?;
    }
    // Never inherit the previous provider's config after a provider switch: its
    // keys mean something else to the new provider. A create that only names an
    // out-of-band Secret keeps and validates the submitted providerConfig.
    let existing_config = if provider_changed {
        None
    } else if let Some(prev) = existing {
        Some(prev.provider_config.clone())
    } else if supplied_credentials_only {
        Some(domain.provider_config.clone())
    } else {
        None
    };
    let resolved = p.resolve_fields(
        "cert",
        fields,
        existing_config.as_ref(),
        &[],
        create_or_changed,
    )?;
    domain.provider_config = resolved.config.clone();

    let secret_name = if domain.credentials_secret.is_empty() {
        format!("naslos-domain-{}-creds", sanitize_name(&domain.base_domain))
    } else {
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

    #[test]
    fn domain_view_redacts_solver_values_from_last_error() {
        const CANARY: &str = "CANARY-SOLVER-TOKEN-4f9a";
        let domain = Domain {
            base_domain: "example.com".into(),
            solver: Some(serde_json::json!({
                "webhook": { "token": CANARY, "groupName": "example.com" }
            })),
            provider_config: HashMap::from([("apiKey".to_string(), CANARY.to_string())]),
            last_error: format!(
                "applying the Issuer failed: admission rejected {{\"token\":\"{CANARY}\"}} for {CANARY}"
            ),
            ..Default::default()
        };
        let value = domain_view(&domain);
        let text = value.to_string();
        assert!(!text.contains(CANARY), "{text}");
        assert!(
            value["lastError"]
                .as_str()
                .unwrap()
                .contains("applying the Issuer failed"),
            "{text}"
        );
        assert_eq!(redact_error(&domain, ""), "");
    }

    #[test]
    fn redaction_keeps_an_error_without_solver_values() {
        let domain = Domain {
            base_domain: "example.com".into(),
            solver: Some(serde_json::json!({"webhook": {"token": "abcd-secret"}})),
            ..Default::default()
        };
        assert_eq!(
            redact_error(&domain, "the cluster refused the Issuer"),
            "the cluster refused the Issuer"
        );
    }

    /// A reconciler that echoes the solver in its error, like a Kubernetes
    /// admission rejection that quotes the rendered Issuer.
    struct EchoSolverReconciler;

    #[async_trait::async_trait]
    impl crate::certs::Reconciler for EchoSolverReconciler {
        async fn apply(&self, d: &Domain) -> Result<(), String> {
            let solver = d
                .solver
                .as_ref()
                .map(serde_json::Value::to_string)
                .unwrap_or_default();
            Err(format!("the cluster rejected the Issuer: {solver}"))
        }

        async fn delete(&self, _d: &Domain) -> Result<(), String> {
            Ok(())
        }

        async fn status(&self, _d: &Domain) -> Result<serde_json::Value, String> {
            Ok(serde_json::json!({}))
        }
    }

    #[tokio::test]
    async fn apply_errors_are_redacted_before_they_are_stored() {
        const CANARY: &str = "CANARY-APPLY-TOKEN-77aa";
        let mut state = AppState::for_test("test-secret");
        state.certs = Some(Arc::new(EchoSolverReconciler));
        let state = Arc::new(state);
        let req = DomainRequest {
            domain: Domain {
                base_domain: "example.test".into(),
                dns_provider: "passthrough".into(),
                solver: Some(serde_json::json!({"webhook": {"token": CANARY}})),
                environment: "staging".into(),
                ..Default::default()
            },
            fields: None,
            solver_submitted: true,
        };
        let response = upsert_domain(&state, req, StatusCode::CREATED).await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let stored = state.domains.get("example.test").unwrap();
        assert!(
            !stored.last_error.contains(CANARY),
            "stored lastError leaked the solver: {}",
            stored.last_error
        );
        assert!(stored.last_error.contains("rejected the Issuer"));
    }
}
