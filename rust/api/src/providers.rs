//! Declarative DNS provider definitions (port of `api/internal/providers`).
//!
//! Loads embedded YAML defaults plus an optional override directory. A provider
//! describes its credential fields (driving the UI forms and deciding what is a
//! Secret), an optional cert-manager DNS-01 solver and an optional DDNS driver.

use include_dir::{include_dir, Dir};
use std::collections::HashMap;

/// The embedded built-in provider definitions.
static BUILTIN: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/src/providers/builtin");

/// The UI/config type of a credential field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldType {
    #[default]
    String,
    Bool,
    Enum,
}

/// One provider credential/config field.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Field {
    pub key: String,
    pub label: String,
    #[serde(rename = "type", default)]
    pub field_type: FieldType,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub default: String,
    #[serde(rename = "enum", default, skip_serializing_if = "Vec::is_empty")]
    pub enumeration: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub secret: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub secret_key: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_if: Option<ShowIf>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// A conditional-visibility rule for a field.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ShowIf {
    pub key: String,
    pub value: String,
}

impl Field {
    pub fn secret_key_or(&self) -> String {
        if self.secret_key.is_empty() {
            self.key.clone()
        } else {
            self.secret_key.clone()
        }
    }
}

/// The provider's cert-manager DNS-01 support.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CertManager {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solver: Option<serde_yaml::Value>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub passthrough: bool,
}

/// The provider's dynamic-DNS support.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Ddns {
    pub driver: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub defaults: HashMap<String, String>,
}

/// One declarative provider definition.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    pub name: String,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub icon: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub api_rights: Vec<String>,
    #[serde(default)]
    pub fields: Vec<Field>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert_manager: Option<CertManager>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ddns: Option<Ddns>,
}

/// The DDNS drivers compiled into the API.
fn known_drivers(driver: &str) -> bool {
    matches!(
        driver,
        "ovh" | "cloudflare" | "digitalocean" | "godaddy" | "porkbun" | "http"
    )
}

impl Provider {
    pub fn has_cert_manager(&self) -> bool {
        self.cert_manager
            .as_ref()
            .map(|c| c.solver.is_some())
            .unwrap_or(false)
    }

    pub fn has_ddns(&self) -> bool {
        self.ddns
            .as_ref()
            .map(|d| !d.driver.is_empty())
            .unwrap_or(false)
    }

    pub fn is_passthrough(&self) -> bool {
        self.cert_manager
            .as_ref()
            .map(|c| c.passthrough)
            .unwrap_or(false)
    }

    pub fn supports_certificates(&self) -> bool {
        self.has_cert_manager() || self.is_passthrough()
    }

    pub fn secret_fields(&self) -> Vec<Field> {
        self.fields_for_scope("")
            .into_iter()
            .filter(|f| f.secret)
            .collect()
    }

    pub fn config_fields(&self) -> Vec<Field> {
        self.fields_for_scope("")
            .into_iter()
            .filter(|f| !f.secret)
            .collect()
    }

    pub fn fields_for(&self, scope: &str) -> Vec<Field> {
        self.fields_for_scope(scope)
    }

    fn fields_for_scope(&self, scope: &str) -> Vec<Field> {
        self.fields
            .iter()
            .filter(|f| f.scope.is_empty() || scope.is_empty() || f.scope == scope)
            .cloned()
            .collect()
    }

    /// Return config with every missing field filled from its default.
    pub fn with_defaults(&self, config: &HashMap<String, String>) -> HashMap<String, String> {
        let mut out = config.clone();
        for f in &self.fields {
            if !out.contains_key(&f.key) && !f.default.is_empty() {
                out.insert(f.key.clone(), f.default.clone());
            }
        }
        out
    }

    /// Render the provider's cert-manager solver for a domain.
    pub fn solver(
        &self,
        secret_name: &str,
        config: &HashMap<String, String>,
    ) -> Result<serde_yaml::Value, String> {
        if !self.has_cert_manager() {
            return Err(format!(
                "provider {:?} does not define a cert-manager solver",
                self.name
            ));
        }
        let merged = self.with_defaults(config);
        let solver = self
            .cert_manager
            .as_ref()
            .and_then(|c| c.solver.as_ref())
            .ok_or_else(|| format!("provider {:?}: no solver", self.name))?;
        let rendered = render_value(solver, secret_name, &merged)
            .map_err(|e| format!("provider {:?}: {e}", self.name))?;
        Ok(rendered)
    }

    pub fn validate_solver(
        &self,
        secret_name: &str,
        config: &HashMap<String, String>,
    ) -> Result<(), String> {
        if !self.has_cert_manager() {
            return Ok(());
        }
        self.solver(secret_name, config).map(|_| ())
    }
}

/// Validate a single field value against its type.
pub fn validate_field(f: &Field, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Ok(());
    }
    match f.field_type {
        FieldType::Bool => {
            if value != "true" && value != "false" {
                return Err(format!("field {:?} must be true or false", f.key));
            }
        }
        FieldType::Enum => {
            if !f.enumeration.iter().any(|v| v == value) {
                return Err(format!(
                    "field {:?} must be one of {}",
                    f.key,
                    f.enumeration.join(", ")
                ));
            }
        }
        FieldType::String => {}
    }
    Ok(())
}

/// The result of splitting a provider's submitted fields.
#[derive(Debug, Clone, Default)]
pub struct FieldResolution {
    pub config: HashMap<String, String>,
    pub secret_values: HashMap<String, String>,
    pub credential_fields: Vec<String>,
}

impl Provider {
    /// Apply one submission to the provider's field model for the given scope.
    pub fn resolve_fields(
        &self,
        scope: &str,
        submitted: &HashMap<String, String>,
        existing_config: Option<&HashMap<String, String>>,
        existing_credential_fields: &[String],
        create: bool,
    ) -> Result<FieldResolution, String> {
        let mut out = FieldResolution {
            config: HashMap::new(),
            secret_values: HashMap::new(),
            credential_fields: existing_credential_fields.to_vec(),
        };

        for f in self.fields_for_scope(scope) {
            if f.secret {
                continue;
            }
            let value = if let Some(v) = submitted.get(&f.key) {
                Some(v.clone())
            } else if let Some(existing) = existing_config.and_then(|c| c.get(&f.key)) {
                if !existing.is_empty() {
                    Some(existing.clone())
                } else {
                    None
                }
            } else if !f.default.is_empty() {
                Some(f.default.clone())
            } else {
                None
            };
            if let Some(v) = value {
                out.config.insert(f.key.clone(), v);
            }
            if f.required
                && out
                    .config
                    .get(&f.key)
                    .map(|v| v.trim().is_empty())
                    .unwrap_or(true)
            {
                return Err(format!("field {:?} is required", f.key));
            }
            if let Some(v) = out.config.get(&f.key) {
                validate_field(&f, v)?;
            }
        }

        for f in self.fields_for_scope(scope) {
            if !f.secret {
                continue;
            }
            if let Some(v) = submitted.get(&f.key) {
                if !v.trim().is_empty() {
                    out.secret_values.insert(f.secret_key_or(), v.clone());
                    if !out.credential_fields.contains(&f.key) {
                        out.credential_fields.push(f.key.clone());
                    }
                    continue;
                }
            }
            if create && f.required {
                return Err(format!("field {:?} is required", f.key));
            }
            if let Some(v) = submitted.get(&f.key) {
                validate_field(&f, v)?;
            }
        }
        Ok(out)
    }
}

/// Render `${cred.<key>}` and `${secret}` placeholders in a solver value.
fn render_value(
    value: &serde_yaml::Value,
    secret_name: &str,
    config: &HashMap<String, String>,
) -> Result<serde_yaml::Value, String> {
    match value {
        serde_yaml::Value::String(s) => Ok(serde_yaml::Value::String(render_string(
            s,
            secret_name,
            config,
        )?)),
        serde_yaml::Value::Mapping(m) => {
            let mut out = serde_yaml::Mapping::new();
            for (k, v) in m {
                out.insert(k.clone(), render_value(v, secret_name, config)?);
            }
            Ok(serde_yaml::Value::Mapping(out))
        }
        serde_yaml::Value::Sequence(seq) => {
            let mut out = Vec::with_capacity(seq.len());
            for v in seq {
                out.push(render_value(v, secret_name, config)?);
            }
            Ok(serde_yaml::Value::Sequence(out))
        }
        other => Ok(other.clone()),
    }
}

fn render_string(
    s: &str,
    secret_name: &str,
    config: &HashMap<String, String>,
) -> Result<String, String> {
    // Replace ${cred.<key>} first.
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if s[i..].starts_with("${cred.") {
            if let Some(end) = s[i..].find('}') {
                let key = &s[i + "${cred.".len()..i + end];
                match config.get(key) {
                    Some(v) => out.push_str(v),
                    None => return Err(format!("missing config value {key:?}")),
                }
                i += end + 1;
                continue;
            }
        }
        if s[i..].starts_with("${secret}") {
            if secret_name.is_empty() {
                return Err(
                    "solver references ${secret} but no credentials Secret is set".to_string(),
                );
            }
            out.push_str(secret_name);
            i += "${secret}".len();
            continue;
        }
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    Ok(out)
}

/// A loaded set of provider definitions plus non-fatal load errors.
#[derive(Default)]
pub struct Registry {
    providers: HashMap<String, Provider>,
    errs: Vec<String>,
}

impl Registry {
    /// Read the embedded built-ins and then the optional override directory.
    pub fn load(override_dir: &str) -> Self {
        let mut r = Registry::default();

        let mut files: Vec<&include_dir::File> = BUILTIN.files().collect();
        files.sort_by(|a, b| a.path().cmp(b.path()));
        for file in files {
            match file.contents_utf8() {
                Some(data) => r.add(data, &file.path().display().to_string()),
                None => r
                    .errs
                    .push(format!("built-in {}: not UTF-8", file.path().display())),
            }
        }

        if !override_dir.trim().is_empty() {
            match std::fs::read_dir(override_dir) {
                Ok(entries) => {
                    let mut names: Vec<String> = entries
                        .flatten()
                        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .filter(|n| {
                            let lower = n.to_lowercase();
                            lower.ends_with(".yaml") || lower.ends_with(".yml")
                        })
                        .collect();
                    names.sort();
                    for name in names {
                        let full = std::path::Path::new(override_dir).join(&name);
                        match std::fs::read_to_string(&full) {
                            Ok(data) => r.add(&data, &full.display().to_string()),
                            Err(e) => r.errs.push(format!("provider {name}: {e}")),
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => r
                    .errs
                    .push(format!("reading providers dir {override_dir}: {e}")),
            }
        }
        r
    }

    fn add(&mut self, data: &str, source: &str) {
        match parse(data) {
            Ok(p) => {
                self.providers.insert(p.name.clone(), p);
            }
            Err(e) => self.errs.push(format!("{source}: {e}")),
        }
    }

    pub fn get(&self, name: &str) -> Option<&Provider> {
        self.providers.get(name)
    }

    pub fn list(&self) -> Vec<&Provider> {
        let mut out: Vec<&Provider> = self.providers.values().collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    pub fn errors(&self) -> Vec<String> {
        self.errs.clone()
    }
}

/// Decode and validate one provider document.
pub fn parse(data: &str) -> Result<Provider, String> {
    let mut p: Provider =
        serde_yaml::from_str(data).map_err(|e| format!("parsing provider YAML: {e}"))?;
    validate(&p)?;
    if p.display_name.is_empty() {
        p.display_name = p.name.clone();
    }
    Ok(p)
}

fn is_dns1123_label(name: &str) -> bool {
    if name.is_empty() || name.len() > 63 {
        return false;
    }
    let b = name.as_bytes();
    if !b[0].is_ascii_lowercase() && !b[0].is_ascii_digit() {
        return false;
    }
    if !b[b.len() - 1].is_ascii_lowercase() && !b[b.len() - 1].is_ascii_digit() {
        return false;
    }
    b.iter()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

fn validate(p: &Provider) -> Result<(), String> {
    if !is_dns1123_label(&p.name) {
        return Err(format!(
            "provider name {:?} is not a DNS-1123 label",
            p.name
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for f in &p.fields {
        if f.key.trim().is_empty() {
            return Err(format!("provider {:?}: a field has no key", p.name));
        }
        if !seen.insert(f.key.clone()) {
            return Err(format!(
                "provider {:?}: duplicate field key {:?}",
                p.name, f.key
            ));
        }
        if !matches!(f.scope.as_str(), "" | "cert" | "ddns") {
            return Err(format!(
                "provider {:?} field {:?}: unknown scope {:?}",
                p.name, f.key, f.scope
            ));
        }
        if let Some(si) = &f.show_if {
            if si.key.trim().is_empty() {
                return Err(format!(
                    "provider {:?} field {:?}: showIf needs a key",
                    p.name, f.key
                ));
            }
        }
        match f.field_type {
            FieldType::String => {}
            FieldType::Bool => {
                if !f.default.is_empty() && f.default != "true" && f.default != "false" {
                    return Err(format!(
                        "provider {:?} field {:?}: bool default must be true or false",
                        p.name, f.key
                    ));
                }
            }
            FieldType::Enum => {
                if f.enumeration.is_empty() {
                    return Err(format!(
                        "provider {:?} field {:?}: enum field has no values",
                        p.name, f.key
                    ));
                }
                if !f.default.is_empty() && !f.enumeration.contains(&f.default) {
                    return Err(format!(
                        "provider {:?} field {:?}: default {:?} is not in the enum",
                        p.name, f.key, f.default
                    ));
                }
            }
        }
    }
    if let Some(d) = &p.ddns {
        if !d.driver.is_empty() && !known_drivers(&d.driver) {
            return Err(format!(
                "provider {:?}: unknown DDNS driver {:?}",
                p.name, d.driver
            ));
        }
    }
    for (i, right) in p.api_rights.iter().enumerate() {
        if right.trim().is_empty() {
            return Err(format!("provider {:?}: apiRights[{i}] is empty", p.name));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_all_builtins() {
        let r = Registry::load("");
        assert!(r.errors().is_empty(), "errors: {:?}", r.errors());
        // The embedded set has 17 providers.
        assert_eq!(
            r.list().len(),
            17,
            "providers: {:?}",
            r.list().iter().map(|p| &p.name).collect::<Vec<_>>()
        );
        assert!(r.get("ovh").is_some());
        assert!(r.get("passthrough").is_some());
    }

    #[test]
    fn ovh_solver_renders_secret_and_cred() {
        let r = Registry::load("");
        let ovh = r.get("ovh").unwrap();
        assert!(ovh.has_cert_manager());
        let config = HashMap::from([
            ("endpoint".to_string(), "ovh-eu".to_string()),
            ("applicationKey".to_string(), "k".to_string()),
        ]);
        let solver = ovh.solver("ovh-creds", &config).unwrap();
        let s = serde_yaml::to_string(&solver).unwrap();
        assert!(
            s.contains("ovh-creds"),
            "solver should reference the secret: {s}"
        );
    }

    #[test]
    fn passthrough_supports_certificates_without_a_solver() {
        let r = Registry::load("");
        let p = r.get("passthrough").unwrap();
        assert!(p.is_passthrough());
        assert!(p.supports_certificates());
        assert!(!p.has_cert_manager());
    }

    #[test]
    fn resolve_fields_splits_secret_and_config() {
        let r = Registry::load("");
        let cloudflare = r.get("cloudflare").unwrap();
        let submitted = HashMap::from([("apiToken".to_string(), "tok".to_string())]);
        let res = cloudflare
            .resolve_fields("cert", &submitted, None, &[], true)
            .unwrap();
        assert_eq!(
            res.secret_values.get("api-token").map(String::as_str),
            Some("tok")
        );
        assert!(res.credential_fields.contains(&"apiToken".to_string()));
    }

    #[test]
    fn invalid_provider_yaml_is_recorded_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("bad.yaml"), "name: Bad_Name\n").unwrap();
        let r = Registry::load(&dir.path().display().to_string());
        assert!(!r.errors().is_empty());
        assert!(r.get("ovh").is_some(), "built-ins still load");
    }

    #[test]
    fn validate_field_types() {
        let f = Field {
            key: "flag".into(),
            field_type: FieldType::Bool,
            ..Default::default()
        };
        assert!(validate_field(&f, "true").is_ok());
        assert!(validate_field(&f, "yes").is_err());
        let e = Field {
            key: "mode".into(),
            field_type: FieldType::Enum,
            enumeration: vec!["a".into(), "b".into()],
            ..Default::default()
        };
        assert!(validate_field(&e, "a").is_ok());
        assert!(validate_field(&e, "c").is_err());
    }
}
