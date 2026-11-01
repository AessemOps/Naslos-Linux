//! Rebuild the installer's per-install overrides from the current release.
//!
//! The installer stores fully merged values in the release, including the old
//! image digests. Reusing them would silently keep the old images, so the
//! upgrader keeps only the keys the installer owns (`helm.Overrides` in
//! Naslos-Installer) and takes everything else from the new pack.
//!
//! Keep this list identical to `Naslos-Installer/internal/helm/helm.go`
//! `Overrides`; docs/installer-contract.md §7 lists both next to each other.

use serde_json::{json, Map, Value};

pub const KEYS: &[&str] = &[
    "domain",
    "sso.domains",
    "shares.discovery.name",
    "openldap.host",
    "api.talosConfigSecret",
    "networkPolicy.nodeCIDR",
    "networkPolicy.ingressPluginsCIDR",
    "networkPolicy.nfsClientCIDR",
];

/// Build the override object from a `helm get values` document. Missing keys
/// are omitted so the new pack's defaults apply; a missing `domain` is an error
/// because an upgrade without it would change the appliance's identity.
pub fn rebuild(current: &Value) -> Result<Value, String> {
    let domain = get(current, "domain")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if domain.trim().is_empty() {
        return Err("the current release has no domain; refusing to rebuild overrides".into());
    }

    let mut out = Map::new();
    out.insert("domain".into(), json!(domain));
    if let Some(v) = get(current, "sso.domains").filter(|v| v.is_array()) {
        out.insert("sso".into(), json!({ "domains": v }));
    }
    if let Some(v) = get(current, "shares.discovery.name").filter(|v| v.is_string()) {
        out.insert("shares".into(), json!({ "discovery": { "name": v } }));
    }
    if let Some(v) = get(current, "openldap.host").filter(|v| v.is_string()) {
        out.insert("openldap".into(), json!({ "host": v }));
    }
    if let Some(v) = get(current, "api.talosConfigSecret").filter(|v| v.is_string()) {
        out.insert("api".into(), json!({ "talosConfigSecret": v }));
    }
    let mut network = Map::new();
    for key in ["nodeCIDR", "ingressPluginsCIDR", "nfsClientCIDR"] {
        if let Some(v) = get(current, &format!("networkPolicy.{key}")).filter(|v| v.is_string()) {
            network.insert(key.into(), v.clone());
        }
    }
    if !network.is_empty() {
        out.insert("networkPolicy".into(), Value::Object(network));
    }
    Ok(Value::Object(out))
}

fn get<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn merged() -> Value {
        json!({
            "domain": "naslos.local",
            "sso": {"domains": ["naslos.local"]},
            "shares": {"discovery": {"name": "naslos"}, "smb": {"image": {"tag": "0.2.6"}}},
            "openldap": {"host": "ldap.naslos.local", "image": "ghcr.io/aessemops/naslos-openldap:0.2.6"},
            "api": {"talosConfigSecret": "naslos-talosconfig", "image": {"tag": "0.2.6"}},
            "networkPolicy": {
                "nodeCIDR": "192.168.1.0/24",
                "ingressPluginsCIDR": "192.168.1.0/24",
                "nfsClientCIDR": "192.168.1.0/24",
                "gitEgress": true
            },
            "apps": {"officialSource": {"commit": "old"}}
        })
    }

    #[test]
    fn keeps_only_the_installer_keys() {
        let out = rebuild(&merged()).unwrap();
        assert_eq!(out["domain"], "naslos.local");
        assert_eq!(out["sso"]["domains"][0], "naslos.local");
        assert_eq!(out["shares"]["discovery"]["name"], "naslos");
        assert_eq!(out["openldap"]["host"], "ldap.naslos.local");
        assert_eq!(out["api"]["talosConfigSecret"], "naslos-talosconfig");
        assert_eq!(out["networkPolicy"]["nodeCIDR"], "192.168.1.0/24");
        assert!(out.get("apps").is_none());
        assert!(out["shares"].get("smb").is_none());
        assert!(out["api"].get("image").is_none());
        assert!(out["networkPolicy"].get("gitEgress").is_none());
    }

    #[test]
    fn refuses_without_a_domain() {
        assert!(rebuild(&json!({})).is_err());
        assert!(rebuild(&json!({"domain": "  "})).is_err());
    }

    #[test]
    fn the_key_list_is_documented_and_stable() {
        assert_eq!(KEYS.len(), 8);
    }
}
