//! Per-app custom environment variables (FR-APP-26).
//!
//! Values live only in the Secret `naslos-env-<release>` in the app's
//! namespace; the record persists just the variable names and whether each one
//! is a secret. Charts that declare `capabilities: [env]` reference the Secret
//! with `envFrom` and roll on the `naslos.io/env-checksum` annotation.

use std::collections::HashMap;
use std::collections::{BTreeMap, BTreeSet};

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::kube;

/// The Secret holding one release's environment variables.
pub const SECRET_PREFIX: &str = "naslos-env-";
/// Platform-owned Helm value carrying the Secret checksum.
pub const CHECKSUM_VALUE: &str = "naslosEnvChecksum";
/// Chart capability that enables the feature.
pub const CAPABILITY: &str = "env";
pub const MAX_VARS: usize = 100;
pub const MAX_NAME_BYTES: usize = 128;
pub const MAX_VALUE_BYTES: usize = 32 * 1024;
pub const MAX_TOTAL_BYTES: usize = 512 * 1024;

/// One environment variable supplied by an operator.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvVar {
    pub name: String,
    /// `None` on an update keeps the stored value of a `secret` variable.
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub secret: bool,
}

/// The persisted, value-free description of an app's environment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvMeta {
    pub name: String,
    #[serde(default)]
    pub secret: bool,
}

impl EnvMeta {
    pub fn from_vars(vars: &[EnvVar]) -> Vec<EnvMeta> {
        vars.iter()
            .map(|v| EnvMeta {
                name: v.name.clone(),
                secret: v.secret,
            })
            .collect()
    }
}

/// The Secret name for a release.
pub fn secret_name(release: &str) -> String {
    format!("{SECRET_PREFIX}{release}")
}

/// `^[A-Za-z_][A-Za-z0-9_.-]*$`, at most 128 bytes.
pub fn valid_name(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_NAME_BYTES {
        return false;
    }
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// Validate the shape of a submitted environment list (values not required:
/// a `secret` update may omit them).
pub fn validate(vars: &[EnvVar]) -> Result<(), String> {
    if vars.len() > MAX_VARS {
        return Err(format!(
            "at most {MAX_VARS} environment variables are allowed"
        ));
    }
    let mut seen = BTreeSet::new();
    let mut total = 0usize;
    for var in vars {
        if !valid_name(&var.name) {
            return Err(format!(
                "invalid environment variable name {:?}; use letters, digits, '_', '.' and '-'",
                var.name
            ));
        }
        if !seen.insert(var.name.clone()) {
            return Err(format!("duplicate environment variable {:?}", var.name));
        }
        if var.name.starts_with("NASLOS_") {
            return Err(format!(
                "environment variable {:?} uses the reserved NASLOS_ prefix",
                var.name
            ));
        }
        if let Some(value) = &var.value {
            if value.len() > MAX_VALUE_BYTES {
                return Err(format!(
                    "environment variable {:?} is larger than {MAX_VALUE_BYTES} bytes",
                    var.name
                ));
            }
            total += value.len();
        }
        total += var.name.len();
    }
    if total >= MAX_TOTAL_BYTES {
        return Err(format!(
            "environment variables exceed {MAX_TOTAL_BYTES} bytes in total"
        ));
    }
    Ok(())
}

/// Resolve submitted variables to the values that go into the Secret.
///
/// `existing` is the current Secret data. A `secret` variable whose value is
/// `null` keeps its stored value; every other missing value is an error.
pub fn resolve(
    existing: &BTreeMap<String, String>,
    vars: &[EnvVar],
) -> Result<BTreeMap<String, String>, String> {
    validate(vars)?;
    let mut out = BTreeMap::new();
    let mut total = 0usize;
    for var in vars {
        let value = match (&var.value, var.secret) {
            (Some(value), _) => value.clone(),
            (None, true) => existing.get(&var.name).cloned().ok_or_else(|| {
                format!(
                    "environment variable {:?} has no stored value; enter one",
                    var.name
                )
            })?,
            (None, false) => {
                return Err(format!("environment variable {:?} needs a value", var.name))
            }
        };
        if value.len() > MAX_VALUE_BYTES {
            return Err(format!(
                "environment variable {:?} is larger than {MAX_VALUE_BYTES} bytes",
                var.name
            ));
        }
        total += var.name.len() + value.len();
        out.insert(var.name.clone(), value);
    }
    if total >= MAX_TOTAL_BYTES {
        return Err(format!(
            "environment variables exceed {MAX_TOTAL_BYTES} bytes in total"
        ));
    }
    Ok(out)
}

/// SHA-256 over the sorted `name=value` pairs, hex encoded.
pub fn checksum(data: &BTreeMap<String, String>) -> String {
    let mut hasher = Sha256::new();
    for (name, value) in data {
        hasher.update(name.as_bytes());
        hasher.update(b"=");
        hasher.update(value.as_bytes());
        hasher.update(b"\n");
    }
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Names the chart sets itself on the containers that consume the env Secret.
///
/// `rendered` is `helm template` output; `secret_name` is the release's env
/// Secret. Sidecars that do not reference the Secret are ignored.
pub fn chart_env_names(rendered: &str, secret_name: &str) -> Result<BTreeSet<String>, String> {
    let mut names = BTreeSet::new();
    for document in serde_yaml::Deserializer::from_str(rendered) {
        let value = serde_yaml::Value::deserialize(document)
            .map_err(|e| format!("parsing rendered chart: {e}"))?;
        let Some(doc) = value.as_mapping() else {
            continue;
        };
        let kind = doc.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        if !matches!(kind, "Deployment" | "StatefulSet" | "DaemonSet") {
            continue;
        }
        let containers = doc
            .get("spec")
            .and_then(|spec| spec.get("template"))
            .and_then(|template| template.get("spec"))
            .and_then(|spec| spec.get("containers"))
            .and_then(|containers| containers.as_sequence());
        for container in containers.into_iter().flatten() {
            let uses_secret = container
                .get("envFrom")
                .and_then(|env_from| env_from.as_sequence())
                .into_iter()
                .flatten()
                .any(|entry| {
                    entry
                        .get("secretRef")
                        .and_then(|reference| reference.get("name"))
                        .and_then(|name| name.as_str())
                        .map(|name| name == secret_name)
                        .unwrap_or(false)
                });
            if !uses_secret {
                continue;
            }
            for env in container
                .get("env")
                .and_then(|env| env.as_sequence())
                .into_iter()
                .flatten()
            {
                if let Some(name) = env.get("name").and_then(|name| name.as_str()) {
                    names.insert(name.to_string());
                }
            }
        }
    }
    Ok(names)
}

/// Read the env Secret's values; `Ok(None)` when it does not exist.
pub async fn read_secret(
    client: &kube::Client,
    namespace: &str,
    release: &str,
) -> Result<Option<BTreeMap<String, String>>, String> {
    let Some(object) = client
        .get_json("secret", &secret_name(release), namespace)
        .await?
    else {
        return Ok(None);
    };
    let data = object
        .get("data")
        .and_then(|data| data.as_object())
        .cloned()
        .unwrap_or_default();
    let engine = base64::engine::general_purpose::STANDARD;
    let mut out = BTreeMap::new();
    for (name, encoded) in data {
        let encoded = encoded.as_str().unwrap_or_default();
        let bytes = engine
            .decode(encoded)
            .map_err(|e| format!("decoding environment variable {name:?}: {e}"))?;
        let value = String::from_utf8(bytes)
            .map_err(|_| format!("environment variable {name:?} is not valid UTF-8"))?;
        out.insert(name, value);
    }
    Ok(Some(out))
}

/// Apply the env Secret with the platform labels.
pub async fn write_secret(
    client: &kube::Client,
    namespace: &str,
    release: &str,
    app: &str,
    data: &BTreeMap<String, String>,
) -> Result<(), String> {
    let string_data: HashMap<&String, &String> = data.iter().collect();
    let object = serde_json::json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": secret_name(release),
            "namespace": namespace,
            "labels": {
                "app.kubernetes.io/managed-by": "naslos",
                "naslos.io/app": app,
            },
        },
        "type": "Opaque",
        "stringData": string_data,
    });
    client.apply(&object).await
}

/// Delete the env Secret, ignoring a missing one.
pub async fn delete_secret(
    client: &kube::Client,
    namespace: &str,
    release: &str,
) -> Result<(), String> {
    client
        .delete("secret", &secret_name(release), namespace)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn var(name: &str, value: Option<&str>, secret: bool) -> EnvVar {
        EnvVar {
            name: name.into(),
            value: value.map(str::to_string),
            secret,
        }
    }

    #[test]
    fn names_follow_the_kubernetes_subset() {
        for name in ["FOO", "_FOO", "A1", "my.var", "my-var", "TZ"] {
            assert!(valid_name(name), "{name} should be valid");
        }
        for name in ["", "1FOO", "-FOO", "FOO=BAR", "FOO BAR", "fi\u{00f1}e"] {
            assert!(!valid_name(name), "{name} should be invalid");
        }
        assert!(!valid_name(&"A".repeat(MAX_NAME_BYTES + 1)));
    }

    #[test]
    fn validation_rejects_duplicates_and_reserved_prefixes() {
        let duplicate = vec![var("FOO", Some("a"), false), var("FOO", Some("b"), false)];
        assert!(validate(&duplicate).is_err());
        assert!(validate(&[var("NASLOS_SECRET", Some("x"), true)]).is_err());
        let mut many = Vec::new();
        for i in 0..=MAX_VARS {
            many.push(var(&format!("V{i}"), Some("x"), false));
        }
        assert!(validate(&many).is_err());
        assert!(validate(&[var("BIG", Some(&"x".repeat(MAX_VALUE_BYTES + 1)), false)]).is_err());
    }

    #[test]
    fn a_null_secret_keeps_the_stored_value() {
        let mut existing = BTreeMap::new();
        existing.insert("TOKEN".to_string(), "stored".to_string());
        let resolved = resolve(&existing, &[var("TOKEN", None, true)]).unwrap();
        assert_eq!(resolved.get("TOKEN").map(String::as_str), Some("stored"));
        assert!(resolve(&BTreeMap::new(), &[var("TOKEN", None, true)]).is_err());
        assert!(resolve(&existing, &[var("TOKEN", None, false)]).is_err());
    }

    #[test]
    fn omitted_variables_are_removed() {
        let mut existing = BTreeMap::new();
        existing.insert("OLD".to_string(), "1".to_string());
        let resolved = resolve(&existing, &[var("NEW", Some("2"), false)]).unwrap();
        assert_eq!(resolved.len(), 1);
        assert!(!resolved.contains_key("OLD"));
    }

    #[test]
    fn checksum_is_sorted_and_value_sensitive() {
        let mut a = BTreeMap::new();
        a.insert("B".to_string(), "2".to_string());
        a.insert("A".to_string(), "1".to_string());
        let mut b = BTreeMap::new();
        b.insert("A".to_string(), "1".to_string());
        b.insert("B".to_string(), "2".to_string());
        assert_eq!(checksum(&a), checksum(&b));
        b.insert("B".to_string(), "3".to_string());
        assert_ne!(checksum(&a), checksum(&b));
        assert_eq!(checksum(&a).len(), 64);
        assert_eq!(checksum(&BTreeMap::new()).len(), 64);
    }

    #[test]
    fn chart_names_only_come_from_containers_using_the_env_secret() {
        let rendered = r#"
apiVersion: apps/v1
kind: Deployment
metadata: { name: app }
spec:
  template:
    spec:
      initContainers:
        - name: migrate
          envFrom:
            - secretRef: { name: naslos-env-app }
          env:
            - { name: INIT_ONLY, value: x }
      containers:
        - name: app
          envFrom:
            - secretRef: { name: naslos-env-app, optional: true }
          env:
            - { name: TZ, value: UTC }
            - { name: PUID, value: "1000" }
        - name: sidecar
          env:
            - { name: SIDECAR_ONLY, value: x }
---
apiVersion: v1
kind: Service
metadata: { name: app }
"#;
        let names = chart_env_names(rendered, "naslos-env-app").unwrap();
        assert!(names.contains("TZ"));
        assert!(names.contains("PUID"));
        assert!(!names.contains("SIDECAR_ONLY"));
        assert!(!names.contains("INIT_ONLY"));
        assert!(chart_env_names(rendered, "naslos-env-other")
            .unwrap()
            .is_empty());
    }
}
