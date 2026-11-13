//! Automatic CNAME records for exposed apps (FR-DNS-09).
//!
//! Only records the platform created are updated or deleted. A pre-existing
//! CNAME with the desired target is adopted; any other record at the name is a
//! conflict and is left untouched. Credentials come from the base domain's
//! cert-manager Secret (OVH ZoneDNS keys or a Cloudflare API token).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::apps::{AppDns, DnsState, Record};
use crate::certs::Domain;
use crate::ddns;
use crate::kube;

/// Supported providers, in the order the UI lists them.
pub const SUPPORTED_PROVIDERS: [&str; 2] = ["ovh", "cloudflare"];

/// Whether a domain's provider can create app records.
pub fn provider_supported(provider: &str) -> bool {
    SUPPORTED_PROVIDERS.contains(&provider)
}

/// One record at the app's name (all types, so conflicts are visible).
#[derive(Debug, Clone, Default, Serialize)]
pub struct ZoneRecord {
    pub id: String,
    #[serde(rename = "type")]
    pub record_type: String,
    pub target: String,
}

/// Provider-specific record operations.
#[async_trait::async_trait]
pub trait RecordApi: Send + Sync {
    /// Whether the account serves this zone.
    async fn zone_exists(&self, zone: &str) -> Result<bool, String>;
    /// Every record at `fqdn` (not filtered by type).
    async fn find(&self, zone: &str, fqdn: &str) -> Result<Vec<ZoneRecord>, String>;
    async fn create_cname(&self, zone: &str, fqdn: &str, target: &str) -> Result<String, String>;
    async fn update_cname(&self, zone: &str, id: &str, target: &str) -> Result<(), String>;
    async fn delete(&self, zone: &str, id: &str) -> Result<(), String>;
    /// Ask the provider to publish the zone (OVH); a no-op elsewhere.
    async fn refresh(&self, zone: &str) -> Result<(), String>;
}

fn read_body(body: &str) -> String {
    body.chars().take(400).collect()
}

fn normalize_target(target: &str) -> String {
    target.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// A CNAME record absolute for OVH (`fqdn.`).
fn absolute(target: &str) -> String {
    format!("{}.", target.trim().trim_end_matches('.'))
}

// ---- OVH ZoneDNS ---------------------------------------------------------

pub struct OvhRecords {
    client: reqwest::Client,
    base: String,
    application_key: String,
    application_secret: String,
    consumer_key: String,
}

impl OvhRecords {
    pub fn new(
        client: reqwest::Client,
        base: &str,
        application_key: &str,
        application_secret: &str,
        consumer_key: &str,
    ) -> Self {
        Self {
            client,
            base: base.trim_end_matches('/').to_string(),
            application_key: application_key.to_string(),
            application_secret: application_secret.to_string(),
            consumer_key: consumer_key.to_string(),
        }
    }

    async fn call(&self, method: &str, url: &str, body: &str) -> Result<(u16, String), String> {
        let ts = chrono::Utc::now().timestamp();
        let signature = ddns::drivers::ovh_signature(
            &self.application_secret,
            &self.consumer_key,
            method,
            url,
            body,
            ts,
        );
        let m = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
        let resp = self
            .client
            .request(m, url)
            .header("X-Ovh-Application", &self.application_key)
            .header("X-Ovh-Consumer", &self.consumer_key)
            .header("X-Ovh-Timestamp", ts.to_string())
            .header("X-Ovh-Signature", signature)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        Ok((status, read_body(&text)))
    }

    fn zone_url(&self, zone: &str) -> String {
        format!("{}/domain/zone/{}", self.base, urlencode(zone))
    }
}

use crate::ddns::drivers::record_id;

#[async_trait::async_trait]
impl RecordApi for OvhRecords {
    async fn zone_exists(&self, zone: &str) -> Result<bool, String> {
        let (status, body) = self.call("GET", &self.zone_url(zone), "").await?;
        match status {
            200 => Ok(true),
            404 => Ok(false),
            _ => Err(format!("OVH returned HTTP {status}: {}", body.trim())),
        }
    }

    async fn find(&self, zone: &str, fqdn: &str) -> Result<Vec<ZoneRecord>, String> {
        let label = ddns::zone_label(zone, fqdn);
        let list_url = format!(
            "{}/record?subDomain={}",
            self.zone_url(zone),
            urlencode(&label)
        );
        let (status, body) = self.call("GET", &list_url, "").await?;
        if status != 200 {
            return Err(format!("OVH returned HTTP {status}: {}", body.trim()));
        }
        let ids: Vec<serde_json::Value> =
            serde_json::from_str(&body).map_err(|e| format!("parsing the OVH record list: {e}"))?;
        let mut out = Vec::new();
        for id in ids.iter().filter_map(record_id) {
            let detail_url = format!("{}/record/{}", self.zone_url(zone), urlencode(&id));
            let (status, detail) = self.call("GET", &detail_url, "").await?;
            if status == 404 {
                continue;
            }
            if status != 200 {
                return Err(format!("OVH returned HTTP {status}: {}", detail.trim()));
            }
            let value: serde_json::Value =
                serde_json::from_str(&detail).map_err(|e| format!("parsing an OVH record: {e}"))?;
            out.push(ZoneRecord {
                id,
                record_type: value
                    .get("fieldType")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_uppercase(),
                target: value
                    .get("target")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string(),
            });
        }
        Ok(out)
    }

    async fn create_cname(&self, zone: &str, fqdn: &str, target: &str) -> Result<String, String> {
        let label = ddns::zone_label(zone, fqdn);
        let body = serde_json::json!({
            "fieldType": "CNAME",
            "subDomain": label,
            "target": absolute(target),
            "ttl": 0,
        })
        .to_string();
        let url = format!("{}/record", self.zone_url(zone));
        let (status, text) = self.call("POST", &url, &body).await?;
        if !(200..300).contains(&status) {
            return Err(format!("OVH returned HTTP {status}: {}", text.trim()));
        }
        let value: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| format!("parsing the OVH create response: {e}"))?;
        record_id(&value).ok_or_else(|| "OVH did not return a record id".to_string())
    }

    async fn update_cname(&self, zone: &str, id: &str, target: &str) -> Result<(), String> {
        let body = serde_json::json!({ "target": absolute(target), "ttl": 0 }).to_string();
        let url = format!("{}/record/{}", self.zone_url(zone), urlencode(id));
        let (status, text) = self.call("PUT", &url, &body).await?;
        if !(200..300).contains(&status) {
            return Err(format!("OVH returned HTTP {status}: {}", text.trim()));
        }
        Ok(())
    }

    async fn delete(&self, zone: &str, id: &str) -> Result<(), String> {
        let url = format!("{}/record/{}", self.zone_url(zone), urlencode(id));
        let (status, text) = self.call("DELETE", &url, "").await?;
        if status == 404 || (200..300).contains(&status) {
            Ok(())
        } else {
            Err(format!("OVH returned HTTP {status}: {}", text.trim()))
        }
    }

    async fn refresh(&self, zone: &str) -> Result<(), String> {
        let url = format!("{}/refresh", self.zone_url(zone));
        let (status, text) = self.call("POST", &url, "").await?;
        if !(200..300).contains(&status) {
            return Err(format!("OVH returned HTTP {status}: {}", text.trim()));
        }
        Ok(())
    }
}

/// The OVH API base for a provider `endpoint` value.
pub fn ovh_base(endpoint: &str) -> Result<String, String> {
    ddns::drivers::ovh_base(endpoint)
}

// ---- Cloudflare ----------------------------------------------------------

pub struct CloudflareRecords {
    client: reqwest::Client,
    base: String,
    token: String,
}

impl CloudflareRecords {
    pub fn new(client: reqwest::Client, base: &str, token: &str) -> Self {
        Self {
            client,
            base: base.trim_end_matches('/').to_string(),
            token: token.to_string(),
        }
    }

    async fn decode(&self, resp: reqwest::Response) -> Result<serde_json::Value, String> {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!(
                "Cloudflare returned HTTP {}: {}",
                status.as_u16(),
                text.trim()
            ));
        }
        let envelope: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| format!("parsing the Cloudflare response: {e}"))?;
        if envelope.get("success").and_then(|s| s.as_bool()) != Some(true) {
            return Err(format!(
                "Cloudflare API error: {}",
                envelope
                    .get("errors")
                    .map(|e| e.to_string())
                    .unwrap_or_default()
            ));
        }
        Ok(envelope
            .get("result")
            .cloned()
            .unwrap_or(serde_json::Value::Null))
    }

    async fn zone_id(&self, zone: &str) -> Result<Option<String>, String> {
        let url = format!("{}/zones?name={}", self.base, urlencode(zone));
        let resp = self
            .client
            .get(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let result = self.decode(resp).await?;
        Ok(result
            .as_array()
            .and_then(|zones| zones.first())
            .and_then(|zone| zone.get("id"))
            .and_then(|id| id.as_str())
            .map(str::to_string))
    }

    async fn zone_id_or_err(&self, zone: &str) -> Result<String, String> {
        self.zone_id(zone)
            .await?
            .ok_or_else(|| format!("Cloudflare has no zone {zone:?}"))
    }
}

#[async_trait::async_trait]
impl RecordApi for CloudflareRecords {
    async fn zone_exists(&self, zone: &str) -> Result<bool, String> {
        Ok(self.zone_id(zone).await?.is_some())
    }

    async fn find(&self, zone: &str, fqdn: &str) -> Result<Vec<ZoneRecord>, String> {
        let zone_id = self.zone_id_or_err(zone).await?;
        let url = format!(
            "{}/zones/{}/dns_records?name={}",
            self.base,
            urlencode(&zone_id),
            urlencode(fqdn)
        );
        let resp = self
            .client
            .get(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let result = self.decode(resp).await?;
        Ok(result
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|record| {
                Some(ZoneRecord {
                    id: record.get("id")?.as_str()?.to_string(),
                    record_type: record
                        .get("type")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .to_uppercase(),
                    target: record
                        .get("content")
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .to_string(),
                })
            })
            .collect())
    }

    async fn create_cname(&self, zone: &str, fqdn: &str, target: &str) -> Result<String, String> {
        let zone_id = self.zone_id_or_err(zone).await?;
        let url = format!("{}/zones/{}/dns_records", self.base, urlencode(&zone_id));
        let body = serde_json::json!({
            "type": "CNAME",
            "name": fqdn,
            "content": target.trim_end_matches('.'),
            "ttl": 1,
            "proxied": false,
        })
        .to_string();
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.token)
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let result = self.decode(resp).await?;
        result
            .get("id")
            .and_then(|id| id.as_str())
            .map(str::to_string)
            .ok_or_else(|| "Cloudflare did not return a record id".to_string())
    }

    async fn update_cname(&self, zone: &str, id: &str, target: &str) -> Result<(), String> {
        let zone_id = self.zone_id_or_err(zone).await?;
        let url = format!(
            "{}/zones/{}/dns_records/{}",
            self.base,
            urlencode(&zone_id),
            urlencode(id)
        );
        let body = serde_json::json!({
            "content": target.trim_end_matches('.'),
            "ttl": 1,
            "proxied": false,
        })
        .to_string();
        let resp = self
            .client
            .patch(&url)
            .bearer_auth(&self.token)
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        self.decode(resp).await.map(|_| ())
    }

    async fn delete(&self, zone: &str, id: &str) -> Result<(), String> {
        let zone_id = self.zone_id_or_err(zone).await?;
        let url = format!(
            "{}/zones/{}/dns_records/{}",
            self.base,
            urlencode(&zone_id),
            urlencode(id)
        );
        let resp = self
            .client
            .delete(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if resp.status().as_u16() == 404 {
            return Ok(());
        }
        self.decode(resp).await.map(|_| ())
    }

    async fn refresh(&self, _zone: &str) -> Result<(), String> {
        Ok(())
    }
}

/// Minimal RFC 3986 percent-encoding for query/path components.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// What to do with the records already present at the app's hostname.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    Keep(String),
    Update(String),
    Create,
    Conflict(String),
}

fn plan_action(
    existing: &[ZoneRecord],
    owned_id: Option<&str>,
    owned_same_name: bool,
    target: &str,
    fqdn: &str,
) -> Action {
    let cname = existing
        .iter()
        .find(|record| record.record_type.eq_ignore_ascii_case("CNAME"));
    let other = existing
        .iter()
        .find(|record| !record.record_type.eq_ignore_ascii_case("CNAME"));
    match cname {
        Some(record) if normalize_target(&record.target) == normalize_target(target) => {
            // Adopt a matching pre-existing record.
            Action::Keep(record.id.clone())
        }
        Some(record) if owned_id == Some(record.id.as_str()) || owned_same_name => {
            Action::Update(record.id.clone())
        }
        Some(_) => Action::Conflict(format!("a different CNAME record already exists at {fqdn}")),
        None if other.is_some() => {
            Action::Conflict(format!("another DNS record already exists at {fqdn}"))
        }
        None => Action::Create,
    }
}

/// The CNAME reconciler shared with the app manager.
pub struct DnsService {
    client: reqwest::Client,
    kube: Option<Arc<kube::Client>>,
    apps_namespace: String,
    domains: Arc<crate::certs::Store>,
    ddns: Option<Arc<ddns::Store>>,
    ovh_base: Option<String>,
    cloudflare_base: Option<String>,
    zones: Mutex<HashMap<String, String>>,
}

impl DnsService {
    pub fn new(
        kube: Option<Arc<kube::Client>>,
        apps_namespace: &str,
        domains: Arc<crate::certs::Store>,
        ddns: Option<Arc<ddns::Store>>,
    ) -> Self {
        Self {
            client: ddns::default_client(),
            kube,
            apps_namespace: apps_namespace.to_string(),
            domains,
            ddns,
            ovh_base: None,
            cloudflare_base: None,
            zones: Mutex::new(HashMap::new()),
        }
    }

    /// Override the provider API bases (tests).
    pub fn with_endpoints(mut self, ovh: Option<String>, cloudflare: Option<String>) -> Self {
        self.ovh_base = ovh;
        self.cloudflare_base = cloudflare;
        self
    }

    /// Build the provider client from the domain's cert-manager Secret.
    async fn api_for(&self, domain: &Domain) -> Result<Box<dyn RecordApi>, String> {
        if !provider_supported(&domain.dns_provider) {
            return Err(format!(
                "provider {:?} cannot create app DNS records",
                domain.dns_provider
            ));
        }
        let Some(kube) = &self.kube else {
            return Err("no Kubernetes client is available to read DNS credentials".to_string());
        };
        if domain.credentials_secret.is_empty() {
            return Err("the domain has no credentials Secret".to_string());
        }
        let secret = kube
            .get_json("secret", &domain.credentials_secret, &self.apps_namespace)
            .await?
            .ok_or_else(|| {
                format!(
                    "reading Secret {}/{}: not found",
                    self.apps_namespace, domain.credentials_secret
                )
            })?;
        let data = secret
            .get("data")
            .and_then(|data| data.as_object())
            .cloned()
            .unwrap_or_default();
        let value = |key: &str| -> String {
            data.get(key)
                .and_then(|v| v.as_str())
                .and_then(|encoded| {
                    use base64::Engine;
                    base64::engine::general_purpose::STANDARD
                        .decode(encoded)
                        .ok()
                })
                .map(|raw| String::from_utf8_lossy(&raw).trim().to_string())
                .unwrap_or_default()
        };
        match domain.dns_provider.as_str() {
            "ovh" => {
                let endpoint = domain
                    .provider_config
                    .get("endpoint")
                    .map(String::as_str)
                    .unwrap_or("");
                let base = match &self.ovh_base {
                    Some(base) => base.clone(),
                    None => ovh_base(endpoint)?,
                };
                let application_key = value("applicationKey");
                let application_secret = value("applicationSecret");
                let consumer_key = value("consumerKey");
                if application_key.is_empty()
                    || application_secret.is_empty()
                    || consumer_key.is_empty()
                {
                    return Err(
                        "OVH app records need applicationKey, applicationSecret and consumerKey"
                            .to_string(),
                    );
                }
                Ok(Box::new(OvhRecords::new(
                    self.client.clone(),
                    &base,
                    &application_key,
                    &application_secret,
                    &consumer_key,
                )))
            }
            "cloudflare" => {
                let token = value("api-token");
                if token.is_empty() {
                    return Err("Cloudflare app records need an api-token".to_string());
                }
                Ok(Box::new(CloudflareRecords::new(
                    self.client.clone(),
                    self.cloudflare_base
                        .as_deref()
                        .unwrap_or("https://api.cloudflare.com/client/v4"),
                    &token,
                )))
            }
            other => Err(format!("provider {other:?} cannot create app DNS records")),
        }
    }

    /// Walk the base domain upward until the provider recognises a zone.
    async fn zone_for(&self, api: &dyn RecordApi, base_domain: &str) -> Result<String, String> {
        if let Some(zone) = self.zones.lock().unwrap().get(base_domain) {
            return Ok(zone.clone());
        }
        let labels: Vec<&str> = base_domain.split('.').collect();
        for start in 0..labels.len() {
            let candidate = labels[start..].join(".");
            if candidate.is_empty() {
                continue;
            }
            if api.zone_exists(&candidate).await? {
                self.zones
                    .lock()
                    .unwrap()
                    .insert(base_domain.to_string(), candidate.clone());
                return Ok(candidate);
            }
        }
        Err(format!(
            "no DNS zone managed by the provider contains {base_domain}"
        ))
    }

    fn error_state(
        previous: Option<&DnsState>,
        fqdn: &str,
        target: &str,
        provider: &str,
        base_domain: &str,
        error: String,
    ) -> DnsState {
        let mut state = previous.cloned().unwrap_or_default();
        state.fqdn = fqdn.to_string();
        state.target = target.to_string();
        state.provider = provider.to_string();
        state.base_domain = base_domain.to_string();
        state.status = "error".to_string();
        state.error = error;
        state.updated_at = Some(chrono::Utc::now());
        state
    }

    async fn delete_state(&self, state: &DnsState) -> Result<(), String> {
        if !state.owned || state.record_id.is_empty() {
            return Ok(());
        }
        let base = if state.base_domain.is_empty() {
            return Err("the DNS state has no base domain".to_string());
        } else {
            state.base_domain.clone()
        };
        let domain = self.domains.get(&base)?;
        let api = self.api_for(&domain).await?;
        api.delete(&state.zone, &state.record_id).await?;
        api.refresh(&state.zone).await?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl AppDns for DnsService {
    async fn reconcile(&self, rec: &Record, base_domain: &str) -> Option<DnsState> {
        let base = if rec.base_domain.is_empty() {
            base_domain.to_string()
        } else {
            rec.base_domain.clone()
        };
        let subdomain = rec.exposure.subdomain.trim();
        let desired_fqdn = if subdomain.is_empty() || base.is_empty() {
            String::new()
        } else {
            format!("{subdomain}.{base}")
        };

        // Not desired: remove a record this platform owns.
        if desired_fqdn.is_empty() {
            if let Some(previous) = &rec.dns {
                if let Err(e) = self.delete_state(previous).await {
                    return Some(Self::error_state(
                        Some(previous),
                        &previous.fqdn,
                        &previous.target,
                        &previous.provider,
                        &base,
                        e,
                    ));
                }
            }
            return None;
        }

        let domain = match self.domains.get(&base) {
            Ok(domain) => domain,
            Err(_) => {
                if let Some(previous) = &rec.dns {
                    if let Err(e) = self.delete_state(previous).await {
                        return Some(Self::error_state(
                            Some(previous),
                            &desired_fqdn,
                            &previous.target,
                            &previous.provider,
                            &base,
                            e,
                        ));
                    }
                }
                return None;
            }
        };
        let entry_id = domain
            .app_records_ddns_entry
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty());
        let Some(entry_id) = entry_id else {
            if let Some(previous) = &rec.dns {
                if let Err(e) = self.delete_state(previous).await {
                    return Some(Self::error_state(
                        Some(previous),
                        &desired_fqdn,
                        &previous.target,
                        &domain.dns_provider,
                        &base,
                        e,
                    ));
                }
            }
            return None;
        };
        let entry = match self.ddns.as_ref().map(|store| store.get(entry_id)) {
            Some(Ok(entry)) => entry,
            Some(Err(e)) => {
                return Some(Self::error_state(
                    rec.dns.as_ref(),
                    &desired_fqdn,
                    "",
                    &domain.dns_provider,
                    &base,
                    e,
                ))
            }
            None => {
                return Some(Self::error_state(
                    rec.dns.as_ref(),
                    &desired_fqdn,
                    "",
                    &domain.dns_provider,
                    &base,
                    "dynamic DNS is not enabled".to_string(),
                ))
            }
        };
        let target = ddns::fqdn(&entry.zone, &entry.record);
        if target.is_empty() {
            return Some(Self::error_state(
                rec.dns.as_ref(),
                &desired_fqdn,
                "",
                &domain.dns_provider,
                &base,
                format!("DDNS entry {entry_id:?} has no target host"),
            ));
        }
        if normalize_target(&target) == normalize_target(&desired_fqdn) {
            return Some(Self::error_state(
                rec.dns.as_ref(),
                &desired_fqdn,
                &target,
                &domain.dns_provider,
                &base,
                "the app hostname would point at itself".to_string(),
            ));
        }

        let api = match self.api_for(&domain).await {
            Ok(api) => api,
            Err(e) => {
                return Some(Self::error_state(
                    rec.dns.as_ref(),
                    &desired_fqdn,
                    &target,
                    &domain.dns_provider,
                    &base,
                    e,
                ))
            }
        };
        let zone = match self.zone_for(api.as_ref(), &base).await {
            Ok(zone) => zone,
            Err(e) => {
                return Some(Self::error_state(
                    rec.dns.as_ref(),
                    &desired_fqdn,
                    &target,
                    &domain.dns_provider,
                    &base,
                    e,
                ))
            }
        };

        // A renamed app or a changed base domain leaves the old record behind.
        if let Some(previous) = &rec.dns {
            if previous.owned
                && !previous.record_id.is_empty()
                && (previous.fqdn != desired_fqdn
                    || previous.base_domain != base
                    || previous.provider != domain.dns_provider)
            {
                if let Err(e) = self.delete_state(previous).await {
                    return Some(Self::error_state(
                        Some(previous),
                        &desired_fqdn,
                        &target,
                        &domain.dns_provider,
                        &base,
                        e,
                    ));
                }
            }
        }

        let existing = match api.find(&zone, &desired_fqdn).await {
            Ok(records) => records,
            Err(e) => {
                return Some(Self::error_state(
                    rec.dns.as_ref(),
                    &desired_fqdn,
                    &target,
                    &domain.dns_provider,
                    &base,
                    e,
                ))
            }
        };
        let owned = rec.dns.as_ref().filter(|state| state.owned);
        let owned_id = owned
            .map(|state| state.record_id.clone())
            .filter(|id| !id.is_empty());
        let owned_same_name = owned
            .map(|state| state.fqdn == desired_fqdn)
            .unwrap_or(false);

        let write_error = |error: String| {
            Self::error_state(
                rec.dns.as_ref(),
                &desired_fqdn,
                &target,
                &domain.dns_provider,
                &base,
                error,
            )
        };

        let action = plan_action(
            &existing,
            owned_id.as_deref(),
            owned_same_name,
            &target,
            &desired_fqdn,
        );
        let (record_id, changed) = match action {
            Action::Keep(id) => (id, false),
            Action::Update(id) => {
                if let Err(e) = api.update_cname(&zone, &id, &target).await {
                    return Some(write_error(e));
                }
                (id, true)
            }
            Action::Conflict(error) => return Some(write_error(error)),
            Action::Create => match api.create_cname(&zone, &desired_fqdn, &target).await {
                Ok(id) => (id, true),
                Err(e) => return Some(write_error(e)),
            },
        };

        if changed {
            if let Err(e) = api.refresh(&zone).await {
                return Some(write_error(e));
            }
        }

        Some(DnsState {
            fqdn: desired_fqdn,
            target,
            provider: domain.dns_provider.clone(),
            zone,
            record_id,
            owned: true,
            status: "ok".to_string(),
            error: String::new(),
            updated_at: Some(chrono::Utc::now()),
            base_domain: base,
        })
    }

    async fn remove(&self, rec: &Record) -> Result<(), String> {
        match &rec.dns {
            Some(state) if state.owned => self.delete_state(state).await,
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::Exposure;

    #[test]
    fn only_ovh_and_cloudflare_support_app_records() {
        assert!(provider_supported("ovh"));
        assert!(provider_supported("cloudflare"));
        assert!(!provider_supported("rfc2136"));
        assert!(!provider_supported("passthrough"));
    }

    #[test]
    fn cname_targets_are_written_absolute_for_ovh() {
        assert_eq!(absolute("naslos.example.com"), "naslos.example.com.");
        assert_eq!(absolute("naslos.example.com."), "naslos.example.com.");
        assert_eq!(
            normalize_target("Naslos.Example.COM."),
            "naslos.example.com"
        );
    }

    fn record(subdomain: &str) -> Record {
        Record {
            name: "radarr".into(),
            exposure: Exposure {
                subdomain: subdomain.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn matching_records_are_adopted_and_conflicts_are_left_alone() {
        let cname = ZoneRecord {
            id: "9".into(),
            record_type: "CNAME".into(),
            target: "Naslos.Example.NET.".into(),
        };
        assert_eq!(
            plan_action(
                std::slice::from_ref(&cname),
                None,
                false,
                "naslos.example.net",
                "app.example.com"
            ),
            Action::Keep("9".into())
        );
        assert!(matches!(
            plan_action(
                std::slice::from_ref(&cname),
                None,
                false,
                "other.example.net",
                "app.example.com"
            ),
            Action::Conflict(_)
        ));
        assert_eq!(
            plan_action(
                std::slice::from_ref(&cname),
                Some("9"),
                true,
                "other.example.net",
                "app.example.com"
            ),
            Action::Update("9".into())
        );
        let a = ZoneRecord {
            id: "3".into(),
            record_type: "A".into(),
            target: "1.2.3.4".into(),
        };
        assert!(matches!(
            plan_action(&[a], None, false, "naslos.example.net", "app.example.com"),
            Action::Conflict(_)
        ));
        assert_eq!(
            plan_action(&[], None, false, "naslos.example.net", "app.example.com"),
            Action::Create
        );
    }

    type Captured = Arc<Mutex<Vec<(String, String)>>>;

    fn capture(captured: &Captured, method: &str, body: impl Into<String>) {
        captured
            .lock()
            .unwrap()
            .push((method.to_string(), body.into()));
    }

    fn bodies(captured: &Captured) -> Vec<String> {
        captured
            .lock()
            .unwrap()
            .iter()
            .map(|(_, body)| body.clone())
            .collect()
    }

    async fn spawn(router: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://{addr}")
    }

    fn ovh_mock(captured: Captured) -> axum::Router {
        use axum::{extract::Query, routing::get, Json, Router};
        use std::collections::HashMap;
        let list = {
            let captured = captured.clone();
            move |Query(query): Query<HashMap<String, String>>| {
                let captured = captured.clone();
                async move {
                    capture(
                        &captured,
                        "LIST",
                        query.get("subDomain").cloned().unwrap_or_default(),
                    );
                    Json(serde_json::json!([11]))
                }
            }
        };
        let detail = {
            let captured = captured.clone();
            move || {
                let captured = captured.clone();
                async move {
                    capture(&captured, "DETAIL", "");
                    Json(serde_json::json!({
                        "id": 11, "fieldType": "CNAME", "target": "old.example.net."
                    }))
                }
            }
        };
        let create = {
            let captured = captured.clone();
            move |Json(body): Json<serde_json::Value>| {
                let captured = captured.clone();
                async move {
                    capture(&captured, "CREATE", body.to_string());
                    Json(serde_json::json!({ "id": 12 }))
                }
            }
        };
        let update = {
            let captured = captured.clone();
            move |Json(body): Json<serde_json::Value>| {
                let captured = captured.clone();
                async move {
                    capture(&captured, "UPDATE", body.to_string());
                    Json(serde_json::json!({}))
                }
            }
        };
        let remove = {
            let captured = captured.clone();
            move || {
                let captured = captured.clone();
                async move {
                    capture(&captured, "DELETE", "");
                    Json(serde_json::json!({}))
                }
            }
        };
        let refresh = {
            let captured = captured.clone();
            move || {
                let captured = captured.clone();
                async move {
                    capture(&captured, "REFRESH", "");
                    Json(serde_json::json!({}))
                }
            }
        };
        Router::new()
            .route(
                "/domain/zone/example.com",
                get(|| async { Json(serde_json::json!({})) }),
            )
            .route("/domain/zone/example.com/record", get(list).post(create))
            .route(
                "/domain/zone/example.com/record/{id}",
                get(detail).put(update).delete(remove),
            )
            .route(
                "/domain/zone/example.com/refresh",
                axum::routing::post(refresh),
            )
    }

    #[tokio::test]
    async fn ovh_records_create_update_and_delete() {
        let captured: Captured = Arc::new(Mutex::new(Vec::new()));
        let base = spawn(ovh_mock(captured.clone())).await;
        let api = OvhRecords::new(ddns::default_client(), &base, "ak", "as", "ck");

        assert!(api.zone_exists("example.com").await.unwrap());
        assert!(!api.zone_exists("other.com").await.unwrap());

        let found = api.find("example.com", "radarr.example.com").await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].record_type, "CNAME");
        assert_eq!(found[0].target, "old.example.net.");

        let id = api
            .create_cname("example.com", "radarr.example.com", "naslos.example.net")
            .await
            .unwrap();
        assert_eq!(id, "12");
        api.update_cname("example.com", "11", "naslos.example.net")
            .await
            .unwrap();
        api.delete("example.com", "11").await.unwrap();
        api.refresh("example.com").await.unwrap();

        let bodies = bodies(&captured);
        assert!(bodies
            .iter()
            .any(|body| body.contains(r#""subDomain":"radarr""#)));
        assert!(bodies
            .iter()
            .any(|body| body.contains(r#""target":"naslos.example.net.""#)));
    }

    fn cloudflare_mock(captured: Captured) -> axum::Router {
        use axum::{extract::Query, routing::get, Json, Router};
        use std::collections::HashMap;
        let zones = move |Query(_query): Query<HashMap<String, String>>| async move {
            Json(serde_json::json!({
                "success": true, "result": [{ "id": "z1", "name": "example.com" }]
            }))
        };
        let list = {
            let captured = captured.clone();
            move |Query(query): Query<HashMap<String, String>>| {
                let captured = captured.clone();
                async move {
                    capture(
                        &captured,
                        "LIST",
                        query.get("name").cloned().unwrap_or_default(),
                    );
                    Json(serde_json::json!({
                        "success": true,
                        "result": [{ "id": "r1", "type": "CNAME", "content": "old.example.net" }]
                    }))
                }
            }
        };
        let create = {
            let captured = captured.clone();
            move |Json(body): Json<serde_json::Value>| {
                let captured = captured.clone();
                async move {
                    capture(&captured, "CREATE", body.to_string());
                    Json(serde_json::json!({ "success": true, "result": { "id": "r2" } }))
                }
            }
        };
        let update = {
            let captured = captured.clone();
            move |Json(body): Json<serde_json::Value>| {
                let captured = captured.clone();
                async move {
                    capture(&captured, "UPDATE", body.to_string());
                    Json(serde_json::json!({ "success": true, "result": {} }))
                }
            }
        };
        let remove = {
            let captured = captured.clone();
            move || {
                let captured = captured.clone();
                async move {
                    capture(&captured, "DELETE", "");
                    Json(serde_json::json!({ "success": true, "result": {} }))
                }
            }
        };
        Router::new()
            .route("/zones", get(zones))
            .route("/zones/z1/dns_records", get(list).post(create))
            .route(
                "/zones/z1/dns_records/r1",
                axum::routing::patch(update).delete(remove),
            )
    }

    #[tokio::test]
    async fn cloudflare_records_are_unproxied_and_updated() {
        let captured: Captured = Arc::new(Mutex::new(Vec::new()));
        let base = spawn(cloudflare_mock(captured.clone())).await;
        let api = CloudflareRecords::new(ddns::default_client(), &base, "token");

        assert!(api.zone_exists("example.com").await.unwrap());
        let found = api.find("example.com", "radarr.example.com").await.unwrap();
        assert_eq!(found[0].id, "r1");
        assert_eq!(found[0].target, "old.example.net");

        let id = api
            .create_cname("example.com", "radarr.example.com", "naslos.example.net")
            .await
            .unwrap();
        assert_eq!(id, "r2");
        api.update_cname("example.com", "r1", "naslos.example.net")
            .await
            .unwrap();
        api.delete("example.com", "r1").await.unwrap();

        let bodies = bodies(&captured);
        assert!(bodies
            .iter()
            .any(|body| body.contains(r#""content":"naslos.example.net""#)));
        assert!(bodies
            .iter()
            .any(|body| body.contains(r#""proxied":false"#)));
        assert!(bodies.iter().any(|body| body == "radarr.example.com"));
    }

    #[test]
    fn a_record_without_a_subdomain_is_not_desired() {
        let service = DnsService::new(
            None,
            "naslos-apps",
            Arc::new(crate::certs::Store::new("")),
            Some(Arc::new(ddns::Store::new(""))),
        );
        let state = futures::executor::block_on(service.reconcile(&record(""), "example.com"));
        assert!(state.is_none());
    }
}
