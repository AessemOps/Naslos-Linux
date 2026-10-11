//! DDNS drivers (port of `ddns/{drivers,http,cloudflare,ovh,providers_multi}.go`).
//! The DigitalOcean, GoDaddy and Porkbun drivers are ported from
//! qdm12/ddns-updater (MIT); see CREDITS.md.

use std::collections::HashMap;

use super::{config_value, fqdn, normalize_ttl, secret_value, truncate, zone_label};

/// One DDNS update for a driver.
#[derive(Debug, Clone, Default)]
pub struct UpdateRequest {
    pub zone: String,
    pub record: String,
    pub record_type: String,
    pub ip: String,
    pub ttl: i64,
    /// The entry's non-secret provider config merged with driver defaults.
    pub config: HashMap<String, String>,
    /// The resolved secret fields (never persisted).
    pub secret: HashMap<String, String>,
}

/// Applies one DDNS update.
#[async_trait::async_trait]
pub trait Driver: Send + Sync {
    async fn update(&self, r: &UpdateRequest) -> Result<(), String>;
}

/// Build the driver for a provider's `ddns.driver`.
pub fn default_driver(driver: &str, client: reqwest::Client) -> Result<Box<dyn Driver>, String> {
    Ok(match driver {
        "ovh" => Box::new(OvhDriver::new(client)),
        "cloudflare" => Box::new(CloudflareDriver::new(client)),
        "digitalocean" => Box::new(DigitalOceanDriver::new(client)),
        "godaddy" => Box::new(GoDaddyDriver::new(client)),
        "porkbun" => Box::new(PorkbunDriver::new(client)),
        "http" => Box::new(HttpDriver::new(client)),
        other => return Err(format!("unknown DDNS driver {other:?}")),
    })
}

async fn read_body(resp: reqwest::Response) -> String {
    resp.text().await.unwrap_or_default()
}

fn status_ok(status: reqwest::StatusCode, want: u16) -> bool {
    status.as_u16() == want
}

// ---- Cloudflare ----------------------------------------------------------

pub struct CloudflareDriver {
    client: reqwest::Client,
    base_url: Option<String>,
}

impl CloudflareDriver {
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            base_url: None,
        }
    }

    fn base(&self) -> &str {
        self.base_url
            .as_deref()
            .unwrap_or("https://api.cloudflare.com/client/v4")
    }

    async fn decode(
        &self,
        resp: reqwest::Response,
        want_result: bool,
    ) -> Result<serde_json::Value, String> {
        let status = resp.status();
        let text = read_body(resp).await;
        if !status.is_success() {
            return Err(format!(
                "Cloudflare returned HTTP {}: {}",
                status.as_u16(),
                text.trim()
            ));
        }
        let env: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| format!("parsing the Cloudflare response: {e}"))?;
        if env.get("success").and_then(|v| v.as_bool()) != Some(true) {
            return Err(format!(
                "Cloudflare API error: {}",
                env.get("errors").map(|e| e.to_string()).unwrap_or_default()
            ));
        }
        if want_result {
            Ok(env
                .get("result")
                .cloned()
                .unwrap_or(serde_json::Value::Null))
        } else {
            Ok(serde_json::Value::Null)
        }
    }
}

#[async_trait::async_trait]
impl Driver for CloudflareDriver {
    async fn update(&self, r: &UpdateRequest) -> Result<(), String> {
        let token = secret_value(&r.secret, "apiToken");
        if token.is_empty() {
            return Err("Cloudflare requires an apiToken".to_string());
        }
        let name = fqdn(&r.zone, &r.record);

        let zone_url = format!("{}/zones?name={}", self.base(), urlencode(&r.zone));
        let resp = self
            .client
            .get(&zone_url)
            .bearer_auth(&token)
            .header("Content-Type", "application/json")
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let zones = self.decode(resp, true).await?;
        let zone_id = zones
            .as_array()
            .and_then(|a| a.first())
            .and_then(|z| z.get("id"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("Cloudflare has no zone {:?}", r.zone))?
            .to_string();

        let list_url = format!(
            "{}/zones/{}/dns_records?type={}&name={}",
            self.base(),
            urlencode(&zone_id),
            urlencode(&r.record_type),
            urlencode(&name)
        );
        let resp = self
            .client
            .get(&list_url)
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let records = self.decode(resp, true).await?;

        let mut ttl = normalize_ttl(r.ttl);
        if ttl == 0 {
            ttl = 1;
        }
        let payload = serde_json::json!({
            "type": r.record_type,
            "name": name,
            "content": r.ip,
            "ttl": ttl,
            "proxied": config_value(&r.config, "proxied").eq_ignore_ascii_case("true"),
        });

        let record_id = records
            .as_array()
            .and_then(|a| a.first())
            .and_then(|rec| rec.get("id"))
            .and_then(|v| v.as_str());
        let (method, target) = match record_id {
            Some(id) => (
                reqwest::Method::PUT,
                format!(
                    "{}/zones/{}/dns_records/{}",
                    self.base(),
                    urlencode(&zone_id),
                    urlencode(id)
                ),
            ),
            None => (
                reqwest::Method::POST,
                format!("{}/zones/{}/dns_records", self.base(), urlencode(&zone_id)),
            ),
        };
        let resp = self
            .client
            .request(method, &target)
            .bearer_auth(&token)
            .header("Content-Type", "application/json")
            .body(payload.to_string())
            .send()
            .await
            .map_err(|e| e.to_string())?;
        self.decode(resp, false).await.map(|_| ())
    }
}

// ---- OVH -----------------------------------------------------------------

pub struct OvhDriver {
    client: reqwest::Client,
    base_url: Option<String>,
    dynhost_base: Option<String>,
}

impl OvhDriver {
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            base_url: None,
            dynhost_base: None,
        }
    }

    async fn update_dynhost(&self, r: &UpdateRequest) -> Result<(), String> {
        let username = config_value(&r.config, "username");
        let password = secret_value(&r.secret, "password");
        if username.is_empty() || password.is_empty() {
            return Err(
                "OVH DynHost mode requires a username and password (or set mode: api)".to_string(),
            );
        }
        let base = self
            .dynhost_base
            .as_deref()
            .unwrap_or("https://www.ovh.com");
        let target = format!(
            "{}/nic/update?system=dyndns&hostname={}&myip={}",
            base.trim_end_matches('/'),
            urlencode(&fqdn(&r.zone, &r.record)),
            urlencode(&r.ip)
        );
        let resp = self
            .client
            .get(&target)
            .basic_auth(&username, Some(&password))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        let body = read_body(resp).await;
        if !status_ok(status, 200) {
            return Err(format!(
                "OVH DynHost returned HTTP {}: {}",
                status.as_u16(),
                truncate(&body)
            ));
        }
        let text = body.trim().to_lowercase();
        if text.starts_with("good") || text.starts_with("nochg") {
            Ok(())
        } else if text.starts_with("nohost") || text.starts_with("notfqdn") {
            Err("OVH DynHost: hostname does not exist".to_string())
        } else if text.starts_with("badrequest") {
            Err("OVH DynHost: bad request".to_string())
        } else if text.starts_with("badauth") {
            Err("OVH DynHost: authentication failed".to_string())
        } else {
            Err(format!(
                "OVH DynHost: unexpected response {:?}",
                truncate(&body)
            ))
        }
    }

    async fn do_signed(
        &self,
        method: &str,
        url: &str,
        app_key: &str,
        app_secret: &str,
        consumer_key: &str,
        body: &str,
    ) -> Result<reqwest::Response, String> {
        let ts = chrono::Utc::now().timestamp();
        let signature = ovh_signature(app_secret, consumer_key, method, url, body, ts);
        let m = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
        self.client
            .request(m, url)
            .header("X-Ovh-Application", app_key)
            .header("X-Ovh-Consumer", consumer_key)
            .header("X-Ovh-Timestamp", ts.to_string())
            .header("X-Ovh-Signature", signature)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| e.to_string())
    }

    async fn update_zonedns(&self, r: &UpdateRequest) -> Result<(), String> {
        let endpoint = config_value(&r.config, "endpoint");
        let base = match &self.base_url {
            Some(b) => b.clone(),
            None => ovh_base(&endpoint)?,
        };
        let mut app_key = secret_value(&r.secret, "applicationKey");
        if app_key.is_empty() {
            app_key = config_value(&r.config, "applicationKey");
        }
        let app_secret = secret_value(&r.secret, "applicationSecret");
        let consumer_key = secret_value(&r.secret, "consumerKey");
        if app_key.is_empty() || app_secret.is_empty() || consumer_key.is_empty() {
            return Err(
                "OVH requires applicationKey, applicationSecret and consumerKey".to_string(),
            );
        }
        let field_type = &r.record_type;
        let label = zone_label(&r.zone, &r.record);
        let zone = urlencode(&r.zone);

        let list_url = format!(
            "{base}/domain/zone/{zone}/record?fieldType={}&subDomain={}",
            urlencode(field_type),
            urlencode(&label)
        );
        let resp = self
            .do_signed("GET", &list_url, &app_key, &app_secret, &consumer_key, "")
            .await?;
        let status = resp.status();
        let text = read_body(resp).await;
        if !status_ok(status, 200) {
            return Err(format!(
                "OVH returned HTTP {}: {}",
                status.as_u16(),
                text.trim()
            ));
        }
        let records: Vec<serde_json::Value> =
            serde_json::from_str(&text).map_err(|e| format!("parsing the OVH record list: {e}"))?;

        let ttl = normalize_ttl(r.ttl);
        if let Some(first) = records.first() {
            let id = first.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
            let update_url = format!("{base}/domain/zone/{zone}/record/{id}");
            let body = serde_json::json!({ "target": r.ip, "ttl": ttl }).to_string();
            let resp = self
                .do_signed(
                    "PUT",
                    &update_url,
                    &app_key,
                    &app_secret,
                    &consumer_key,
                    &body,
                )
                .await?;
            let status = resp.status();
            if !status_ok(status, 200) {
                let text = read_body(resp).await;
                return Err(format!(
                    "OVH returned HTTP {}: {}",
                    status.as_u16(),
                    text.trim()
                ));
            }
        } else {
            let create_url = format!("{base}/domain/zone/{zone}/record");
            let body = serde_json::json!({
                "target": r.ip, "ttl": ttl, "fieldType": field_type, "subDomain": label
            })
            .to_string();
            let resp = self
                .do_signed(
                    "POST",
                    &create_url,
                    &app_key,
                    &app_secret,
                    &consumer_key,
                    &body,
                )
                .await?;
            let status = resp.status();
            if !status_ok(status, 200) {
                let text = read_body(resp).await;
                return Err(format!(
                    "OVH returned HTTP {}: {}",
                    status.as_u16(),
                    text.trim()
                ));
            }
        }

        let refresh_url = format!("{base}/domain/zone/{zone}/refresh");
        let resp = self
            .do_signed(
                "POST",
                &refresh_url,
                &app_key,
                &app_secret,
                &consumer_key,
                "",
            )
            .await?;
        let status = resp.status();
        if !status_ok(status, 200) {
            let text = read_body(resp).await;
            return Err(format!(
                "OVH returned HTTP {}: {}",
                status.as_u16(),
                text.trim()
            ));
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl Driver for OvhDriver {
    async fn update(&self, r: &UpdateRequest) -> Result<(), String> {
        if config_value(&r.config, "mode").eq_ignore_ascii_case("api") {
            self.update_zonedns(r).await
        } else {
            self.update_dynhost(r).await
        }
    }
}

fn ovh_base(endpoint: &str) -> Result<String, String> {
    match endpoint {
        "" | "ovh-eu" => Ok("https://eu.api.ovh.com/1.0".to_string()),
        "ovh-ca" => Ok("https://ca.api.ovh.com/1.0".to_string()),
        "ovh-us" => Ok("https://api.us.ovhcloud.com/1.0".to_string()),
        other if other.contains('.') => Ok(format!("https://{other}/1.0")),
        other => Err(format!("unknown OVH endpoint {other:?}")),
    }
}

/// The OVH API signing scheme (`$1$` + SHA1 hex).
pub fn ovh_signature(
    app_secret: &str,
    consumer_key: &str,
    method: &str,
    url: &str,
    body: &str,
    ts: i64,
) -> String {
    use sha1::{Digest, Sha1};
    let raw = format!("{app_secret}+{consumer_key}+{method}+{url}+{body}+{ts}");
    let mut h = Sha1::new();
    h.update(raw.as_bytes());
    format!(
        "$1${}",
        h.finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

// ---- DigitalOcean --------------------------------------------------------

pub struct DigitalOceanDriver {
    client: reqwest::Client,
    base_url: Option<String>,
}

impl DigitalOceanDriver {
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            base_url: None,
        }
    }
    fn base(&self) -> &str {
        self.base_url
            .as_deref()
            .unwrap_or("https://api.digitalocean.com/v2")
    }
}

#[async_trait::async_trait]
impl Driver for DigitalOceanDriver {
    async fn update(&self, r: &UpdateRequest) -> Result<(), String> {
        let token = secret_value(&r.secret, "token");
        if token.is_empty() {
            return Err("DigitalOcean requires a token".to_string());
        }
        let name = fqdn(&r.zone, &r.record);
        let list_url = format!(
            "{}/domains/{}/records?name={}&type={}",
            self.base(),
            r.zone,
            urlencode(&name),
            urlencode(&r.record_type)
        );
        let resp = self
            .client
            .get(&list_url)
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let text = read_body(resp).await;
        let listing: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| format!("parsing the DigitalOcean record list: {e}"))?;
        let id = listing
            .get("domain_records")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|rec| rec.get("id"))
            .and_then(|v| v.as_i64())
            .ok_or_else(|| format!("DigitalOcean has no {} record for {name}", r.record_type))?;
        let mut owner = zone_label(&r.zone, &r.record);
        if owner.is_empty() {
            owner = "@".to_string();
        }
        let body = serde_json::json!({ "type": r.record_type, "name": owner, "data": r.ip });
        let target = format!("{}/domains/{}/records/{id}", self.base(), r.zone);
        let resp = self
            .client
            .put(&target)
            .bearer_auth(&token)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        if !status_ok(status, 200) {
            let text = read_body(resp).await;
            return Err(format!(
                "DigitalOcean returned HTTP {}: {}",
                status.as_u16(),
                truncate(&text)
            ));
        }
        Ok(())
    }
}

// ---- GoDaddy -------------------------------------------------------------

pub struct GoDaddyDriver {
    client: reqwest::Client,
    base_url: Option<String>,
}

impl GoDaddyDriver {
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            base_url: None,
        }
    }
    fn base(&self) -> &str {
        self.base_url
            .as_deref()
            .unwrap_or("https://api.godaddy.com/v1")
    }
}

#[async_trait::async_trait]
impl Driver for GoDaddyDriver {
    async fn update(&self, r: &UpdateRequest) -> Result<(), String> {
        let key = config_value(&r.config, "apiKey");
        let secret = secret_value(&r.secret, "apiSecret");
        if key.is_empty() || secret.is_empty() {
            return Err("GoDaddy requires apiKey and apiSecret".to_string());
        }
        let mut owner = zone_label(&r.zone, &r.record);
        if owner.is_empty() {
            owner = "@".to_string();
        }
        let body = serde_json::json!([{ "data": r.ip }]);
        let target = format!(
            "{}/domains/{}/records/{}/{}",
            self.base(),
            r.zone,
            r.record_type,
            owner
        );
        let resp = self
            .client
            .put(&target)
            .header("Authorization", format!("sso-key {key}:{secret}"))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        if !status_ok(status, 200) {
            let text = read_body(resp).await;
            return Err(format!(
                "GoDaddy returned HTTP {}: {}",
                status.as_u16(),
                truncate(&text)
            ));
        }
        Ok(())
    }
}

// ---- Porkbun -------------------------------------------------------------

pub struct PorkbunDriver {
    client: reqwest::Client,
    base_url: Option<String>,
}

impl PorkbunDriver {
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            base_url: None,
        }
    }
    fn base(&self) -> &str {
        self.base_url
            .as_deref()
            .unwrap_or("https://api.porkbun.com/api/json/v3")
    }

    async fn post(
        &self,
        target: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let resp = self
            .client
            .post(target)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .body(payload.to_string())
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        let text = read_body(resp).await;
        if !status_ok(status, 200) {
            return Err(format!(
                "Porkbun returned HTTP {}: {}",
                status.as_u16(),
                truncate(&text)
            ));
        }
        let decoded: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| format!("parsing the Porkbun response: {e}"))?;
        let status_field = decoded.get("status").and_then(|v| v.as_str()).unwrap_or("");
        if !status_field.eq_ignore_ascii_case("SUCCESS") {
            return Err(format!("Porkbun error: {}", truncate(&text)));
        }
        Ok(decoded)
    }
}

#[async_trait::async_trait]
impl Driver for PorkbunDriver {
    async fn update(&self, r: &UpdateRequest) -> Result<(), String> {
        let key = config_value(&r.config, "apiKey");
        let secret = secret_value(&r.secret, "secretApiKey");
        if key.is_empty() || secret.is_empty() {
            return Err("Porkbun requires apiKey and secretApiKey".to_string());
        }
        let owner = zone_label(&r.zone, &r.record);
        let auth = |extra: serde_json::Value| -> serde_json::Value {
            let mut out = serde_json::json!({ "apikey": key, "secretapikey": secret });
            if let (Some(o), Some(e)) = (out.as_object_mut(), extra.as_object()) {
                for (k, v) in e {
                    o.insert(k.clone(), v.clone());
                }
            }
            out
        };
        let ttl = if r.ttl <= 0 { 0 } else { r.ttl };

        let list_target = format!(
            "{}/dns/retrieveByNameType/{}/{}/{}",
            self.base(),
            r.zone,
            r.record_type,
            owner
        );
        let decoded = self
            .post(&list_target, auth(serde_json::Value::Null))
            .await?;
        let ids = porkbun_record_ids(decoded.get("records"));

        let mut fields = serde_json::json!({
            "content": r.ip,
            "type": r.record_type,
            "ttl": ttl.to_string(),
        });
        if !owner.is_empty() {
            if let Some(o) = fields.as_object_mut() {
                o.insert("name".to_string(), serde_json::json!(owner));
            }
        }
        if ids.is_empty() {
            self.post(
                &format!("{}/dns/create/{}", self.base(), r.zone),
                auth(fields),
            )
            .await
            .map_err(|e| format!("creating the Porkbun record: {e}"))?;
            return Ok(());
        }
        for id in ids {
            let target = format!("{}/dns/edit/{}/{}", self.base(), r.zone, id);
            self.post(&target, auth(fields.clone()))
                .await
                .map_err(|e| format!("editing Porkbun record {id}: {e}"))?;
        }
        Ok(())
    }
}

fn porkbun_record_ids(raw: Option<&serde_json::Value>) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(arr) = raw.and_then(|v| v.as_array()) {
        for item in arr {
            if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
                if !id.is_empty() {
                    out.push(id.to_string());
                }
            }
        }
    }
    out
}

// ---- Generic HTTP --------------------------------------------------------

pub struct HttpDriver {
    client: reqwest::Client,
    base_url: Option<String>,
}

impl HttpDriver {
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            base_url: None,
        }
    }
}

#[async_trait::async_trait]
impl Driver for HttpDriver {
    async fn update(&self, r: &UpdateRequest) -> Result<(), String> {
        let update_url = config_value(&r.config, "updateUrl");
        if update_url.is_empty() {
            return Err("the generic HTTP driver needs updateUrl".to_string());
        }
        let mut rendered_url = render_template(&update_url, r)?;
        if let Some(base) = &self.base_url {
            if rendered_url.starts_with('/') {
                rendered_url = format!("{}{}", base.trim_end_matches('/'), rendered_url);
            }
        }
        let target = guard_url(&rendered_url)?;
        default_guard(&target).map_err(|e| format!("refusing to call {target}: {e}"))?;

        let method = {
            let m = config_value(&r.config, "method").to_uppercase();
            if m.is_empty() {
                "GET".to_string()
            } else {
                m
            }
        };
        let reqwest_method = match method.as_str() {
            "GET" => reqwest::Method::GET,
            "POST" => reqwest::Method::POST,
            "PUT" => reqwest::Method::PUT,
            "PATCH" => reqwest::Method::PATCH,
            other => return Err(format!("unsupported method {other:?}")),
        };

        let body_template = config_value(&r.config, "body");
        let body = if body_template.is_empty() {
            String::new()
        } else {
            render_template(&body_template, r)?
        };

        let mut rb = self.client.request(reqwest_method, target.clone());
        if !body.is_empty() {
            let content_type = {
                let ct = config_value(&r.config, "contentType");
                if ct.is_empty() {
                    "application/json".to_string()
                } else {
                    ct
                }
            };
            rb = rb.header("Content-Type", content_type).body(body);
        }
        rb = apply_auth(rb, r)?;

        let resp = rb.send().await.map_err(|e| e.to_string())?;
        let status = resp.status();
        let want = parse_success_status(&r.config)?;
        let text = read_body(resp).await;
        if !want.contains(&status.as_u16()) {
            return Err(format!(
                "update returned HTTP {} (want {want:?}): {}",
                status.as_u16(),
                truncate(&text)
            ));
        }
        if status.as_u16() == 204 {
            return Ok(());
        }
        let markers = split_list(&config_value(&r.config, "errorAny"));
        for marker in &markers {
            if text.contains(marker) {
                return Err(format!(
                    "update rejected by the provider: {}",
                    truncate(&text)
                ));
            }
        }
        let success_markers = split_list(&config_value(&r.config, "successAny"));
        if !success_markers.is_empty() && !success_markers.iter().any(|m| text.contains(m)) {
            return Err(format!(
                "update response did not contain any of {success_markers:?}: {}",
                truncate(&text)
            ));
        }
        let contains = config_value(&r.config, "successContains");
        if !contains.is_empty() && !text.contains(&contains) {
            return Err(format!("update response did not contain {contains:?}"));
        }
        Ok(())
    }
}

fn parse_success_status(config: &HashMap<String, String>) -> Result<Vec<u16>, String> {
    let raw = config_value(config, "successStatus");
    if raw.is_empty() {
        return Ok(vec![200]);
    }
    let mut out = Vec::new();
    for part in raw.split(',') {
        let code: u16 = part
            .trim()
            .parse()
            .map_err(|_| format!("successStatus {raw:?} is not a number"))?;
        out.push(code);
    }
    Ok(out)
}

fn split_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

fn apply_auth(
    rb: reqwest::RequestBuilder,
    r: &UpdateRequest,
) -> Result<reqwest::RequestBuilder, String> {
    match config_value(&r.config, "authType").as_str() {
        "" | "none" => Ok(rb),
        "basic" => Ok(rb.basic_auth(
            config_value(&r.config, "username"),
            Some(secret_value(&r.secret, "password")),
        )),
        "bearer" => {
            let token = secret_value(&r.secret, "token");
            if token.is_empty() {
                return Err("bearer auth needs a token".to_string());
            }
            Ok(rb.bearer_auth(token))
        }
        "header" => {
            let name = {
                let n = config_value(&r.config, "headerName");
                if n.is_empty() {
                    "Authorization".to_string()
                } else {
                    n
                }
            };
            let value = secret_value(&r.secret, "headerValue");
            if value.is_empty() {
                return Err("header auth needs headerValue".to_string());
            }
            Ok(rb.header(name, value))
        }
        "query" => {
            let name = {
                let n = config_value(&r.config, "headerName");
                if n.is_empty() {
                    "token".to_string()
                } else {
                    n
                }
            };
            let value = secret_value(&r.secret, "token");
            if value.is_empty() {
                return Err("query auth needs a token".to_string());
            }
            Ok(rb.query(&[(name, value)]))
        }
        other => Err(format!("unknown authType {other:?}")),
    }
}

/// Render the generic driver's `{{...}}` template. Supports `{{fqdn .record
/// .zone}}`, `{{label .record .zone}}`, `{{.zone}}`/`.record`/`.type`/`.ip`/
/// `.ttl`, `{{.secret.KEY}}` and `{{.config.KEY}}`. A missing key is an error
/// (Go's `missingkey=error`).
fn render_template(tmpl: &str, r: &UpdateRequest) -> Result<String, String> {
    let mut out = String::with_capacity(tmpl.len());
    let mut rest = tmpl;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after
            .find("}}")
            .ok_or_else(|| "unterminated template expression".to_string())?;
        let expr = after[..end].trim();
        out.push_str(&eval_expr(expr, r)?);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

fn eval_expr(expr: &str, r: &UpdateRequest) -> Result<String, String> {
    match expr {
        "fqdn .record .zone" => Ok(fqdn(&r.zone, &r.record)),
        "label .record .zone" => Ok(zone_label(&r.zone, &r.record)),
        ".zone" => Ok(r.zone.clone()),
        ".record" => Ok(r.record.clone()),
        ".type" => Ok(r.record_type.clone()),
        ".ip" => Ok(r.ip.clone()),
        ".ttl" => Ok(r.ttl.to_string()),
        e if e.starts_with(".secret.") => {
            let key = &e[".secret.".len()..];
            r.secret
                .get(key)
                .cloned()
                .ok_or_else(|| format!("missing secret value {key:?}"))
        }
        e if e.starts_with(".config.") => {
            let key = &e[".config.".len()..];
            r.config
                .get(key)
                .cloned()
                .ok_or_else(|| format!("missing config value {key:?}"))
        }
        other => Err(format!("unsupported template expression {{{{{other}}}}}")),
    }
}

fn guard_url(raw: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(raw).map_err(|e| format!("invalid URL {raw:?}: {e}"))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(format!("unsupported URL scheme {:?}", url.scheme()));
    }
    if url.host_str().is_none() {
        return Err(format!("URL {raw:?} has no host"));
    }
    Ok(url)
}

/// Reject targets that resolve to a non-routable address (a basic SSRF guard).
fn default_guard(url: &reqwest::Url) -> Result<(), String> {
    let host = url.host_str().ok_or("URL has no host")?;
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return if blocked_ip(&ip) {
            Err(format!("target {host} is not a public address"))
        } else {
            Ok(())
        };
    }
    use std::net::ToSocketAddrs;
    let addrs = (host, 0u16)
        .to_socket_addrs()
        .map_err(|e| format!("cannot resolve {host:?}: {e}"))?;
    let mut any = false;
    for addr in addrs {
        any = true;
        if blocked_ip(&addr.ip()) {
            return Err(format!(
                "target {host} resolves to a non-public address {}",
                addr.ip()
            ));
        }
    }
    if !any {
        return Err(format!("{host:?} did not resolve"));
    }
    Ok(())
}

fn blocked_ip(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.is_unspecified()
        }
        std::net::IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_multicast()
                || v6.is_unspecified()
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ovh_signature_shape() {
        let sig = ovh_signature(
            "secret",
            "consumer",
            "GET",
            "https://eu.api.ovh.com/1.0/x",
            "",
            1700000000,
        );
        assert!(sig.starts_with("$1$"));
        assert_eq!(sig.len(), 3 + 40);
    }

    #[test]
    fn template_render_fqdn_and_secret() {
        let r = UpdateRequest {
            zone: "example.com".into(),
            record: "home".into(),
            record_type: "A".into(),
            ip: "1.2.3.4".into(),
            secret: HashMap::from([("token".to_string(), "abc".to_string())]),
            ..Default::default()
        };
        assert_eq!(
            render_template("https://dns.example/{{fqdn .record .zone}}?ip={{.ip}}", &r).unwrap(),
            "https://dns.example/home.example.com?ip=1.2.3.4"
        );
        assert_eq!(render_template("{{.secret.token}}", &r).unwrap(), "abc");
        assert!(render_template("{{.secret.missing}}", &r).is_err());
    }

    #[test]
    fn guard_blocks_private_targets() {
        let url = reqwest::Url::parse("http://127.0.0.1/update").unwrap();
        assert!(default_guard(&url).is_err());
        let url = reqwest::Url::parse("http://10.0.0.1/x").unwrap();
        assert!(default_guard(&url).is_err());
    }
}
