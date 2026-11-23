//! Base domains and their cert-manager ACME DNS-01 certificates (port of
//! `api/internal/certs`). The Issuer/Certificate CRs are rendered here; the
//! cluster apply (dynamic client) arrives in a later slice.

use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use crate::providers::Registry;

/// The process-wide provider registry (set once at startup).
static REGISTRY: OnceLock<Arc<Registry>> = OnceLock::new();

/// Replace the provider registry used by certificates.
pub fn configure_registry(r: Arc<Registry>) {
    let _ = REGISTRY.set(r);
}

fn registry() -> Arc<Registry> {
    REGISTRY
        .get_or_init(|| Arc::new(Registry::load("")))
        .clone()
}

/// A DNS-01 challenge provider name (a value in the registry).
pub type Provider = String;

/// A base domain with an optional ACME certificate.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Domain {
    #[serde(rename = "baseDomain")]
    pub base_domain: String,
    #[serde(rename = "dnsProvider", default)]
    pub dns_provider: Provider,
    #[serde(
        rename = "credentialsSecret",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub credentials_secret: String,
    #[serde(
        rename = "providerConfig",
        default,
        skip_serializing_if = "HashMap::is_empty"
    )]
    pub provider_config: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solver: Option<serde_json::Value>,
    #[serde(
        rename = "acmeEmail",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub acme_email: String,
    #[serde(default)]
    pub environment: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub primary: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub sso: bool,
    /// DDNS entry whose hostname app CNAMEs point at. `None` disables app
    /// records for this domain (FR-DNS-09).
    #[serde(
        rename = "appRecordsDdnsEntry",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub app_records_ddns_entry: Option<String>,
    #[serde(rename = "createdAt", default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(rename = "updatedAt", default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
    #[serde(
        rename = "lastError",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub last_error: String,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// A DNS name / subdomain (RFC 1123).
pub fn is_dns1123_subdomain(s: &str) -> bool {
    if s.is_empty() || s.len() > 253 {
        return false;
    }
    s.split('.').all(|label| {
        if label.is_empty() || label.len() > 63 {
            return false;
        }
        let b = label.as_bytes();
        if !b[0].is_ascii_lowercase() && !b[0].is_ascii_digit() {
            return false;
        }
        if !b[b.len() - 1].is_ascii_lowercase() && !b[b.len() - 1].is_ascii_digit() {
            return false;
        }
        b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
    })
}

impl Domain {
    /// Validate the domain record.
    pub fn validate(&self) -> Result<(), String> {
        if !is_dns1123_subdomain(&self.base_domain) {
            return Err(format!(
                "base domain {:?} is not a valid DNS name",
                self.base_domain
            ));
        }
        let reg = registry();
        let p = reg
            .get(&self.dns_provider)
            .ok_or_else(|| format!("unsupported DNS provider {:?}", self.dns_provider))?;
        if p.has_cert_manager() {
            if self.credentials_secret.is_empty() {
                return Err(format!("{} requires a credentials secret", p.name));
            }
        } else if p.is_passthrough() {
            if self.solver.is_none() {
                return Err("passthrough requires a solver".to_string());
            }
        } else {
            return Err(format!(
                "provider {:?} does not support certificates",
                p.name
            ));
        }
        match self.environment.as_str() {
            "" | "staging" | "production" => {}
            _ => return Err("environment must be staging or production".to_string()),
        }
        Ok(())
    }

    /// The TLS secret the wildcard Certificate writes into.
    pub fn secret_name(&self) -> String {
        let name: String = self
            .base_domain
            .chars()
            .map(|c| {
                if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        format!("naslos-{name}-tls")
    }

    /// The Issuer name for the domain.
    pub fn issuer_name(&self) -> String {
        let name: String = self
            .base_domain
            .chars()
            .map(|c| {
                if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        format!("naslos-acme-{name}")
    }

    /// The effective environment (defaulting to staging).
    pub fn effective_environment(&self) -> &str {
        if self.environment == "production" {
            "production"
        } else {
            "staging"
        }
    }
}

/// Render the Issuer and wildcard Certificate for a domain.
pub fn spec(d: &Domain, namespace: &str) -> Result<(serde_json::Value, serde_json::Value), String> {
    d.validate()?;
    let server = if d.effective_environment() == "production" {
        "https://acme-v02.api.letsencrypt.org/directory"
    } else {
        "https://acme-staging-v02.api.letsencrypt.org/directory"
    };
    let solver = solver_for(d)?;

    let issuer = serde_json::json!({
        "apiVersion": "cert-manager.io/v1",
        "kind": "Issuer",
        "metadata": { "name": d.issuer_name(), "namespace": namespace },
        "spec": {
            "acme": {
                "server": server,
                "email": d.acme_email,
                "privateKeySecretRef": { "name": format!("{}-account", d.issuer_name()) },
                "solvers": [ { "dns01": solver } ],
            }
        }
    });
    let certificate = serde_json::json!({
        "apiVersion": "cert-manager.io/v1",
        "kind": "Certificate",
        "metadata": { "name": d.secret_name(), "namespace": namespace },
        "spec": {
            "secretName": d.secret_name(),
            "issuerRef": { "name": d.issuer_name(), "kind": "Issuer", "group": "cert-manager.io" },
            "dnsNames": [ d.base_domain, format!("*.{}", d.base_domain) ],
        }
    });
    Ok((issuer, certificate))
}

fn solver_for(d: &Domain) -> Result<serde_json::Value, String> {
    let reg = registry();
    let p = reg
        .get(&d.dns_provider)
        .ok_or_else(|| format!("unsupported DNS provider {:?}", d.dns_provider))?;
    if p.is_passthrough() {
        return d
            .solver
            .clone()
            .ok_or_else(|| "passthrough requires a solver".to_string());
    }
    if !p.has_cert_manager() {
        return Err(format!(
            "provider {:?} does not support certificates",
            p.name
        ));
    }
    let solver = p.solver(&d.credentials_secret, &d.provider_config)?;
    // Convert the YAML solver into a JSON value.
    let json = serde_json::to_value(&solver).map_err(|e| e.to_string())?;
    Ok(json)
}

/// Persists domain records.
pub struct Store {
    path: String,
    domains: std::sync::Mutex<HashMap<String, Domain>>,
    /// Whether the persisted domain list was read successfully. A missing or
    /// empty file is a valid empty store (fresh install); an unparsable file
    /// fails closed so the next mutation cannot overwrite the only copy.
    loaded: std::sync::atomic::AtomicBool,
    /// Why the store failed to load, with recovery guidance. Present exactly
    /// when `loaded` is false.
    load_error: std::sync::Mutex<Option<String>>,
}

impl Store {
    pub fn new(path: &str) -> Self {
        Self {
            path: path.to_string(),
            domains: std::sync::Mutex::new(HashMap::new()),
            // An empty path is an in-memory test store, not a failed load.
            loaded: std::sync::atomic::AtomicBool::new(path.is_empty()),
            load_error: std::sync::Mutex::new(None),
        }
    }

    pub fn from_env() -> Self {
        Self::new(
            &std::env::var("DOMAINS_CONFIG")
                .unwrap_or_else(|_| "/var/lib/naslos/domains.json".to_string()),
        )
    }

    /// Whether the persisted domain list was read successfully.
    pub fn loaded(&self) -> bool {
        self.loaded.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn load_error(&self) -> Option<String> {
        self.load_error.lock().unwrap().clone()
    }

    /// Fail closed unless the persisted store was read successfully: writing a
    /// fresh list over an unreadable file would discard the only copy.
    pub fn require_loaded(&self) -> Result<(), String> {
        if self.loaded() {
            Ok(())
        } else {
            Err(self
                .load_error()
                .unwrap_or_else(|| format!("domain store {} is unavailable", self.path)))
        }
    }

    pub fn load(&self) -> Result<(), String> {
        self.loaded
            .store(false, std::sync::atomic::Ordering::SeqCst);
        *self.load_error.lock().unwrap() = None;
        if self.path.is_empty() {
            self.loaded.store(true, std::sync::atomic::Ordering::SeqCst);
            return Ok(());
        }
        let data = match std::fs::read(&self.path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.loaded.store(true, std::sync::atomic::Ordering::SeqCst);
                return Ok(());
            }
            Err(e) => {
                let message = format!(
                    "domain store {} could not be read: {e}; refusing to overwrite it",
                    self.path
                );
                *self.load_error.lock().unwrap() = Some(message.clone());
                return Err(message);
            }
        };
        let list: Vec<Domain> = match serde_json::from_slice(&data) {
            Ok(list) => list,
            Err(e) => {
                let backup = match crate::shares::backup_corrupt_store(&self.path, &data) {
                    Ok(path) => format!(" (backup saved to {})", path.display()),
                    Err(backup_err) => format!(" ({backup_err})"),
                };
                let message = format!(
                    "domain store {} is corrupt: {e}{backup}; repair it or replace it with a valid store before changing domains",
                    self.path
                );
                *self.load_error.lock().unwrap() = Some(message.clone());
                return Err(message);
            }
        };
        let mut guard = self.domains.lock().unwrap();
        guard.clear();
        for d in list {
            if !d.base_domain.is_empty() {
                guard.insert(d.base_domain.clone(), d);
            }
        }
        self.loaded.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    pub fn list(&self) -> Vec<Domain> {
        let guard = self.domains.lock().unwrap();
        let mut out: Vec<Domain> = guard.values().cloned().collect();
        out.sort_by(|a, b| a.base_domain.cmp(&b.base_domain));
        out
    }

    pub fn get(&self, name: &str) -> Result<Domain, String> {
        self.domains
            .lock()
            .unwrap()
            .get(name)
            .cloned()
            .ok_or_else(|| format!("domain {name:?} not found"))
    }

    pub fn upsert(&self, d: Domain) -> Result<(), String> {
        self.require_loaded()?;
        let mut guard = self.domains.lock().unwrap();
        guard.insert(d.base_domain.clone(), d);
        self.save_locked(&guard)
    }

    pub fn delete(&self, name: &str) -> Result<(), String> {
        self.require_loaded()?;
        let mut guard = self.domains.lock().unwrap();
        if guard.remove(name).is_none() {
            return Err(format!("domain {name:?} not found"));
        }
        self.save_locked(&guard)
    }

    pub fn update(&self, name: &str, mutate: impl FnOnce(&mut Domain)) -> Result<Domain, String> {
        self.require_loaded()?;
        let mut guard = self.domains.lock().unwrap();
        let d = guard
            .get_mut(name)
            .ok_or_else(|| format!("domain {name:?} not found"))?;
        mutate(d);
        let out = d.clone();
        self.save_locked(&guard)?;
        Ok(out)
    }

    fn save_locked(&self, guard: &HashMap<String, Domain>) -> Result<(), String> {
        self.require_loaded()?;
        if self.path.is_empty() {
            return Ok(());
        }
        let mut list: Vec<&Domain> = guard.values().collect();
        list.sort_by(|a, b| a.base_domain.cmp(&b.base_domain));
        let mut data =
            serde_json::to_vec_pretty(&list).map_err(|e| format!("encoding domains: {e}"))?;
        data.push(b'\n');
        crate::chartsrepo::write_atomic(&self.path, &data, 0o600, 0o750)
    }
}

/// Applies a domain's certificate CRs to the cluster.
#[async_trait::async_trait]
pub trait Reconciler: Send + Sync {
    async fn apply(&self, d: &Domain) -> Result<(), String>;
    async fn delete(&self, d: &Domain) -> Result<(), String>;
    async fn status(&self, d: &Domain) -> Result<serde_json::Value, String>;
}

/// A reconciler over the bundled `kubectl` client.
pub struct KubeReconciler {
    client: Arc<crate::kube::Client>,
    namespace: String,
}

impl KubeReconciler {
    pub fn new(client: Arc<crate::kube::Client>, namespace: &str) -> Self {
        Self {
            client,
            namespace: namespace.to_string(),
        }
    }
}

#[async_trait::async_trait]
impl Reconciler for KubeReconciler {
    async fn apply(&self, d: &Domain) -> Result<(), String> {
        let (issuer, certificate) = spec(d, &self.namespace)?;
        self.client.apply(&issuer).await?;
        self.client.apply(&certificate).await
    }

    async fn delete(&self, d: &Domain) -> Result<(), String> {
        self.client
            .delete("certificate", &d.secret_name(), &self.namespace)
            .await?;
        self.client
            .delete("issuer", &d.issuer_name(), &self.namespace)
            .await
    }

    async fn status(&self, d: &Domain) -> Result<serde_json::Value, String> {
        let obj = self
            .client
            .get_json("certificate", &d.secret_name(), &self.namespace)
            .await?;
        let Some(obj) = obj else {
            return Ok(serde_json::json!({ "status": "pending" }));
        };
        let conditions = obj
            .get("status")
            .and_then(|s| s.get("conditions"))
            .cloned()
            .unwrap_or(serde_json::Value::Array(vec![]));
        let mut state = "pending";
        if let Some(arr) = conditions.as_array() {
            for c in arr {
                if c.get("type") == Some(&serde_json::json!("Ready")) {
                    state = if c.get("status") == Some(&serde_json::json!("True")) {
                        "ready"
                    } else {
                        "not-ready"
                    };
                }
            }
        }
        Ok(serde_json::json!({
            "status": state,
            "conditions": conditions,
            "secretName": d.secret_name(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cloudflare(base: &str) -> Domain {
        Domain {
            base_domain: base.into(),
            dns_provider: "cloudflare".into(),
            credentials_secret: "c".into(),
            ..Default::default()
        }
    }

    #[test]
    fn spec_cloudflare_production() {
        let mut d = cloudflare("example.com");
        d.acme_email = "admin@example.com".into();
        d.environment = "production".into();
        let (issuer, cert) = spec(&d, "naslos-apps").unwrap();
        assert_eq!(issuer["kind"], "Issuer");
        assert_eq!(cert["kind"], "Certificate");
        assert_eq!(
            issuer["spec"]["acme"]["server"],
            "https://acme-v02.api.letsencrypt.org/directory"
        );
        assert_eq!(cert["spec"]["dnsNames"][0], "example.com");
        assert_eq!(cert["spec"]["dnsNames"][1], "*.example.com");
    }

    #[test]
    fn spec_defaults_to_staging() {
        let (issuer, _) = spec(&cloudflare("example.com"), "ns").unwrap();
        assert_eq!(
            issuer["spec"]["acme"]["server"],
            "https://acme-staging-v02.api.letsencrypt.org/directory"
        );
    }

    #[test]
    fn domain_validation() {
        assert!(cloudflare("example.com").validate().is_ok());
        assert!(Domain {
            base_domain: "not a domain".into(),
            dns_provider: "cloudflare".into(),
            credentials_secret: "c".into(),
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(Domain {
            base_domain: "example.com".into(),
            dns_provider: "cloudflare".into(),
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(Domain {
            base_domain: "example.com".into(),
            dns_provider: "passthrough".into(),
            ..Default::default()
        }
        .validate()
        .is_err());
        let mut bad_env = cloudflare("example.com");
        bad_env.environment = "prod".into();
        assert!(bad_env.validate().is_err());
    }

    #[test]
    fn store_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("domains.json");
        let store = Store::new(&path.display().to_string());
        store.load().unwrap();
        let mut d = cloudflare("example.com");
        d.environment = "production".into();
        store.upsert(d).unwrap();

        let reloaded = Store::new(&path.display().to_string());
        reloaded.load().unwrap();
        let got = reloaded.get("example.com").unwrap();
        assert_eq!(got.environment, "production");
        assert_eq!(got.secret_name(), "naslos-example-com-tls");
        assert_eq!(got.issuer_name(), "naslos-acme-example-com");
    }

    #[test]
    fn a_corrupt_store_is_backed_up_and_mutations_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("domains.json");
        std::fs::write(&path, "{not json").unwrap();

        let store = Store::new(&path.display().to_string());
        let error = store.load().unwrap_err();
        assert!(error.contains("corrupt"), "{error}");
        assert!(error.contains(".corrupt-"), "{error}");
        assert!(!store.loaded());
        assert!(store.require_loaded().is_err());

        // The corrupt file stays in place and is never replaced by a write.
        assert!(store.upsert(cloudflare("example.com")).is_err());
        assert!(store.delete("example.com").is_err());
        assert!(store
            .update("example.com", |d| d.environment = "production".into())
            .is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{not json");

        let backups = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().contains(".corrupt-"))
            .count();
        assert_eq!(backups, 1, "one corrupt backup");
    }

    #[test]
    fn a_missing_store_is_a_valid_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(&dir.path().join("domains.json").display().to_string());
        store.load().unwrap();
        assert!(store.loaded());
        store.upsert(cloudflare("example.com")).unwrap();
        assert_eq!(store.list().len(), 1);
    }
}
