//! Renders the dynamic Authelia SSO fragments (port of `api/internal/authelia`).
//!
//! Authelia reads its configuration only at startup and validates
//! `access_control` as one file, so a change also restarts the workload (the
//! cluster sync arrives in a later slice).

/// The ConfigMap keys the chart mounts at `/config-sso`.
pub const COOKIES_KEY: &str = "cookies.yml";
pub const RULES_KEY: &str = "rules.yml";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Cookie {
    domain: String,
    authelia_url: String,
    default_redirection_url: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Rule {
    domain: String,
    policy: String,
}

/// Render `cookies.yml` and `rules.yml` for the effective SSO list. The first
/// domain is the primary: its apex rules come from the static chart config, so
/// only non-primary apexes are emitted here, while the wildcard rule is emitted
/// for every domain.
pub fn fragments(domains: &[String]) -> Result<(String, String), String> {
    let cookies: Vec<Cookie> = domains
        .iter()
        .map(|d| Cookie {
            domain: d.clone(),
            authelia_url: format!("https://{d}/authelia/"),
            default_redirection_url: format!("https://{d}/"),
        })
        .collect();

    let primary = domains.first().cloned().unwrap_or_default();
    let mut rules: Vec<Rule> = Vec::with_capacity(domains.len() * 2);
    for d in domains {
        if *d != primary {
            rules.push(Rule {
                domain: d.clone(),
                policy: "one_factor".into(),
            });
        }
    }
    for d in domains {
        rules.push(Rule {
            domain: format!("*.{d}"),
            policy: "one_factor".into(),
        });
    }

    let cookies_yaml =
        serde_yaml::to_string(&cookies).map_err(|e| format!("encoding cookies.yml: {e}"))?;
    let rules_yaml =
        serde_yaml::to_string(&rules).map_err(|e| format!("encoding rules.yml: {e}"))?;
    Ok((cookies_yaml, rules_yaml))
}

/// Pushes the SSO fragments into the cluster.
#[async_trait::async_trait]
pub trait Syncer: Send + Sync {
    async fn sync(&self, domains: &[String]) -> Result<(), String>;
}

/// A syncer over the bundled `kubectl` client.
pub struct KubeSyncer {
    client: std::sync::Arc<crate::kube::Client>,
    namespace: String,
    config_map: String,
    pod: String,
}

impl KubeSyncer {
    pub fn new(client: std::sync::Arc<crate::kube::Client>, namespace: &str) -> Self {
        Self {
            client,
            namespace: namespace.to_string(),
            config_map: "naslos-authelia-sso".to_string(),
            pod: "naslos-authelia-0".to_string(),
        }
    }
}

#[async_trait::async_trait]
impl Syncer for KubeSyncer {
    async fn sync(&self, domains: &[String]) -> Result<(), String> {
        let (cookies, rules) =
            fragments(domains).map_err(|e| format!("rendering SSO fragments: {e}"))?;
        let current = self
            .client
            .get_json("configmap", &self.config_map, &self.namespace)
            .await?;
        let Some(current) = current else {
            // The chart seeds this ConfigMap; do not create it here.
            return Err(format!(
                "SSO ConfigMap {}/{} is missing; reinstall the chart to seed it",
                self.namespace, self.config_map
            ));
        };
        let data = current.get("data").cloned().unwrap_or_default();
        let existing_cookies = data.get(COOKIES_KEY).and_then(|v| v.as_str()).unwrap_or("");
        let existing_rules = data.get(RULES_KEY).and_then(|v| v.as_str()).unwrap_or("");
        if fragments_equal(existing_cookies, &cookies) && fragments_equal(existing_rules, &rules) {
            return Ok(());
        }
        let patch = serde_json::json!({ "data": { COOKIES_KEY: cookies, RULES_KEY: rules } });
        self.client
            .patch_merge("configmap", &self.config_map, &self.namespace, &patch)
            .await?;
        // Deleting the pod keeps the API's write scope to the ConfigMap.
        self.client.delete_pod(&self.pod, &self.namespace).await
    }
}

/// Whether the stored fragment is semantically the same as the desired one.
fn fragments_equal(existing: &str, desired: &str) -> bool {
    if existing.is_empty() {
        return false;
    }
    let got: Result<Vec<serde_yaml::Value>, _> = serde_yaml::from_str(existing);
    let want: Result<Vec<serde_yaml::Value>, _> = serde_yaml::from_str(desired);
    match (got, want) {
        (Ok(g), Ok(w)) => g == w,
        _ => false,
    }
}

#[cfg(test)]
mod reconciler_tests {
    use super::*;

    #[test]
    fn fragments_equal_ignores_quoting() {
        let a = "- domain: \"naslos.local\"\n  authelia_url: \"https://naslos.local/authelia/\"\n  default_redirection_url: \"https://naslos.local/\"\n";
        let b = "domain: naslos.local\nauthelia_url: https://naslos.local/authelia/\ndefault_redirection_url: https://naslos.local/\n";
        let desired = "- domain: naslos.local\n  authelia_url: https://naslos.local/authelia/\n  default_redirection_url: https://naslos.local/\n";
        assert!(fragments_equal(a, desired));
        // `b` is a mapping, not a sequence, so it does not match.
        assert!(!fragments_equal(b, desired));
        assert!(!fragments_equal("", desired));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_and_rule_shape() {
        let (cookies, rules) =
            fragments(&["naslos.local".into(), "media.example.com".into()]).unwrap();
        let parsed: Vec<serde_yaml::Value> = serde_yaml::from_str(&cookies).unwrap();
        assert_eq!(parsed.len(), 2);
        let first = &parsed[0];
        assert_eq!(first["domain"], "naslos.local");
        assert_eq!(first["authelia_url"], "https://naslos.local/authelia/");
        assert_eq!(first["default_redirection_url"], "https://naslos.local/");

        // The primary apex is not repeated; the non-primary apex and every
        // wildcard are present.
        assert!(!rules.contains("domain: naslos.local\n"));
        for want in [
            "domain: media.example.com\n  policy: one_factor",
            "domain: '*.naslos.local'\n  policy: one_factor",
            "domain: '*.media.example.com'\n  policy: one_factor",
        ] {
            assert!(rules.contains(want), "rules missing {want:?}:\n{rules}");
        }
    }

    #[test]
    fn primary_only() {
        let (cookies, rules) = fragments(&["naslos.local".into()]).unwrap();
        assert!(cookies.contains("domain: naslos.local"));
        assert!(!rules.contains("domain: naslos.local\n"));
        assert!(rules.contains("domain: '*.naslos.local'"));
    }

    #[test]
    fn zero_domains_is_empty_list() {
        let (cookies, rules) = fragments(&[]).unwrap();
        assert_eq!(cookies.trim(), "[]");
        assert_eq!(rules.trim(), "[]");
    }
}
