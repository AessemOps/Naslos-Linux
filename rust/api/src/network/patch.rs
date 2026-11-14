//! Machine-config patch construction (FR-NET-03).
//!
//! Patches are built from validated, typed values with `serde_json`/`serde_yaml`
//! — never by interpolating operator input into a template — and each Talos
//! document appears at most once per patch (Talos merges a single patch
//! document into its matching config document, so a second one would be
//! dropped).

use std::net::IpAddr;

use ipnet::IpNet;

use super::config::MachineConfig;
use super::validate::{Addressing, Change, ResolverMode, SearchMode};

/// A temporary certSAN used only when a change cannot otherwise produce a
/// distinct confirming update. It is dropped by the confirm patch and never
/// resolves.
const CONFIRM_SENTINEL: &str = "naslos-confirm.invalid";

/// The three patches (and metadata) one change needs.
#[derive(Debug, Clone)]
pub struct BuiltPatch {
    /// certSANs rewrite applied before the target patch: allows TLS at the new
    /// address while the target patch is being tried.
    pub pre_patch: String,
    /// The addressing/MTU/resolver documents, applied with `--mode=try`.
    pub patch: String,
    /// The certSANs rewrite that confirms the try-mode update. It must differ
    /// from `pre_patch`, otherwise Talos has no config update to observe.
    pub confirm_patch: String,
    pub target_ip: Option<String>,
    pub old_primary_ip: Option<String>,
    pub needs_cilium_restart: bool,
}

/// Build the patch set for one validated change.
pub fn build(
    change: &Change,
    cfg: &MachineConfig,
    old_primary_ip: Option<&str>,
) -> Result<BuiltPatch, String> {
    let mut docs: Vec<serde_json::Value> = Vec::new();

    if let Some(addressing) = change.addressing {
        match addressing {
            Addressing::Static => {
                let mut link = json_doc("LinkConfig", &change.link);
                link["addresses"] = serde_json::json!(change
                    .addresses
                    .iter()
                    .map(|net| serde_json::json!({ "address": net.to_string() }))
                    .collect::<Vec<_>>());
                if let Some(gateway) = change.gateway {
                    link["routes"] = serde_json::json!([{ "gateway": gateway.to_string() }]);
                }
                if let Some(mtu) = change.mtu {
                    link["mtu"] = serde_json::json!(mtu);
                }
                docs.push(link);
                if cfg.dhcp4.contains(&change.link) {
                    docs.push(delete_doc("DHCPv4Config", &change.link));
                }
                if cfg.dhcp6.contains(&change.link) {
                    docs.push(delete_doc("DHCPv6Config", &change.link));
                }
                docs.push(kube_node_doc(cfg, &change.addresses));
            }
            Addressing::Dhcp => {
                docs.push(json_doc("DHCPv4Config", &change.link));
                let mut link = json_doc("LinkConfig", &change.link);
                link["addresses"] = serde_json::json!({ "$patch": "delete" });
                link["routes"] = serde_json::json!({ "$patch": "delete" });
                if let Some(mtu) = change.mtu {
                    link["mtu"] = serde_json::json!(mtu);
                }
                docs.push(link);
            }
        }
        // The bootstrap patches used the legacy block; remove the matching
        // entry in the same patch so the two sources cannot disagree.
        if let Some(legacy) = legacy_replacement(cfg, &change.link) {
            docs.push(legacy);
        }
    } else if let Some(mtu) = change.mtu {
        let mut link = json_doc("LinkConfig", &change.link);
        link["mtu"] = serde_json::json!(mtu);
        docs.push(link);
        if let Some(legacy) = legacy_replacement(cfg, &change.link) {
            docs.push(legacy);
        }
    }

    if change.nameservers != ResolverMode::Unchanged
        || change.search_domains != SearchMode::Unchanged
    {
        let mut resolver = json_doc("ResolverConfig", "");
        // hostDNS is deliberately absent: it is set by bootstrap and dropping
        // it would re-break CoreDNS under Cilium.
        resolver
            .as_object_mut()
            .expect("a resolver document is an object")
            .remove("name");
        match &change.nameservers {
            ResolverMode::Unchanged => {}
            ResolverMode::Inherit => {
                resolver["nameservers"] = serde_json::json!({ "$patch": "delete" });
            }
            ResolverMode::Explicit(servers) => {
                resolver["nameservers"] = serde_json::json!(servers
                    .iter()
                    .map(|ip| serde_json::json!({ "address": ip.to_string(), "protocol": "Do53" }))
                    .collect::<Vec<_>>());
            }
        }
        match &change.search_domains {
            SearchMode::Unchanged => {}
            SearchMode::Inherit => {
                resolver["searchDomains"] = serde_json::json!({ "$patch": "delete" });
            }
            SearchMode::Explicit(domains) => {
                resolver["searchDomains"] = serde_json::json!(domains);
            }
        }
        docs.push(resolver);
    }

    if docs.is_empty() {
        return Err("no patch documents were produced".to_string());
    }

    let patch = join_docs(&docs)?;

    let new_primary = change
        .addresses
        .iter()
        .find(|net| net.addr().is_ipv4())
        .or_else(|| change.addresses.first())
        .map(|net| net.addr().to_string());
    let old_primary = old_primary_ip.map(str::to_string);

    let (pre_patch, confirm_patch) = cert_san_patches(
        &cfg.cert_sans,
        old_primary.as_deref(),
        new_primary.as_deref(),
    )?;

    let current_mtu = cfg.links.get(&change.link).and_then(|link| link.mtu);
    let needs_cilium_restart = change.mtu.is_some() && (change.mtu != current_mtu);

    Ok(BuiltPatch {
        pre_patch,
        patch,
        confirm_patch,
        target_ip: new_primary.clone(),
        old_primary_ip: old_primary,
        needs_cilium_restart,
    })
}

/// The serialisable subset of a change the verification phase needs. Stored in
/// the change record so verification works after an API restart.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VerifyNeed {
    pub link: String,
    /// `dhcp` / `static`, or `None` when addresing was not part of the change.
    pub addressing: Option<String>,
    pub addresses: Vec<String>,
    pub mtu: Option<u32>,
    pub nameservers: Option<Vec<String>>,
    pub nameservers_inherit: bool,
    pub search_domains: Option<Vec<String>>,
    pub search_inherit: bool,
}

impl VerifyNeed {
    pub fn from_change(change: &Change) -> Self {
        let (nameservers, nameservers_inherit) = match &change.nameservers {
            ResolverMode::Unchanged => (None, false),
            ResolverMode::Inherit => (None, true),
            ResolverMode::Explicit(list) => {
                (Some(list.iter().map(|ip| ip.to_string()).collect()), false)
            }
        };
        let (search_domains, search_inherit) = match &change.search_domains {
            SearchMode::Unchanged => (None, false),
            SearchMode::Inherit => (None, true),
            SearchMode::Explicit(list) => (Some(list.clone()), false),
        };
        Self {
            link: change.link.clone(),
            addressing: change
                .addressing
                .map(|a| match a {
                    Addressing::Dhcp => "dhcp",
                    Addressing::Static => "static",
                })
                .map(str::to_string),
            addresses: change.addresses.iter().map(|net| net.to_string()).collect(),
            mtu: change.mtu,
            nameservers,
            nameservers_inherit,
            search_domains,
            search_inherit,
        }
    }

    /// Whether the config read back from the node matches the requested change.
    pub fn matches(&self, cfg: &MachineConfig) -> bool {
        match self.addressing.as_deref() {
            None => {}
            Some("static") => {
                let Some(link) = cfg.links.get(&self.link) else {
                    return false;
                };
                let have: Vec<IpNet> = link
                    .addresses
                    .iter()
                    .filter_map(|raw| raw.parse().ok())
                    .collect();
                let want: Vec<IpNet> = self
                    .addresses
                    .iter()
                    .filter_map(|raw| raw.parse().ok())
                    .collect();
                if !want.iter().all(|net| have.contains(net)) {
                    return false;
                }
            }
            Some("dhcp") => {
                if !cfg.dhcp4.contains(&self.link) {
                    return false;
                }
                if cfg
                    .links
                    .get(&self.link)
                    .is_some_and(|link| !link.addresses.is_empty())
                {
                    return false;
                }
            }
            Some(_) => return false,
        }
        if let Some(mtu) = self.mtu {
            if cfg.links.get(&self.link).and_then(|link| link.mtu) != Some(mtu) {
                return false;
            }
        }
        if self.nameservers_inherit && cfg.resolver.nameservers.is_some() {
            return false;
        }
        if let Some(want) = &self.nameservers {
            let have: Vec<String> = cfg
                .resolver
                .nameservers
                .clone()
                .unwrap_or_default()
                .iter()
                .filter_map(|raw| raw.parse::<IpAddr>().ok().map(|ip| ip.to_string()))
                .collect();
            if have != *want {
                return false;
            }
        }
        if self.search_inherit && cfg.resolver.search_domains.is_some() {
            return false;
        }
        if let Some(want) = &self.search_domains {
            if cfg.resolver.search_domains.as_ref() != Some(want) {
                return false;
            }
        }
        true
    }
}

/// Whether the config read back from the node matches the requested change.
/// Used by the verification phase before confirming a try-mode update.
pub fn verify(change: &Change, cfg: &MachineConfig) -> bool {
    if let Some(addressing) = change.addressing {
        match addressing {
            Addressing::Static => {
                let Some(link) = cfg.links.get(&change.link) else {
                    return false;
                };
                let have: Vec<IpNet> = link
                    .addresses
                    .iter()
                    .filter_map(|raw| raw.parse().ok())
                    .collect();
                if !change.addresses.iter().all(|want| have.contains(want)) {
                    return false;
                }
            }
            Addressing::Dhcp => {
                if !cfg.dhcp4.contains(&change.link) {
                    return false;
                }
                if cfg
                    .links
                    .get(&change.link)
                    .is_some_and(|link| !link.addresses.is_empty())
                {
                    return false;
                }
            }
        }
    }
    if let Some(mtu) = change.mtu {
        if cfg.links.get(&change.link).and_then(|link| link.mtu) != Some(mtu) {
            return false;
        }
    }
    match &change.nameservers {
        ResolverMode::Unchanged => {}
        ResolverMode::Inherit => {
            if cfg.resolver.nameservers.is_some() {
                return false;
            }
        }
        ResolverMode::Explicit(want) => {
            let have: Vec<IpAddr> = cfg
                .resolver
                .nameservers
                .as_deref()
                .unwrap_or_default()
                .iter()
                .filter_map(|raw| raw.parse().ok())
                .collect();
            if have != *want {
                return false;
            }
        }
    }
    match &change.search_domains {
        SearchMode::Unchanged => {}
        SearchMode::Inherit => {
            if cfg.resolver.search_domains.is_some() {
                return false;
            }
        }
        SearchMode::Explicit(want) => {
            if cfg.resolver.search_domains.as_deref() != Some(want.as_slice()) {
                return false;
            }
        }
    }
    true
}

fn json_doc(kind: &str, name: &str) -> serde_json::Value {
    let mut doc = serde_json::json!({ "apiVersion": "v1alpha1", "kind": kind });
    if !name.is_empty() {
        doc["name"] = serde_json::json!(name);
    }
    doc
}

fn delete_doc(kind: &str, name: &str) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "v1alpha1",
        "kind": kind,
        "name": name,
        "$patch": "delete",
    })
}

fn kube_node_doc(cfg: &MachineConfig, addresses: &[IpNet]) -> serde_json::Value {
    let mut subnets = cfg.valid_subnets.clone();
    for net in addresses {
        let subnet = net.trunc().to_string();
        if !subnets.iter().any(|s| s == &subnet) {
            subnets.push(subnet);
        }
    }
    serde_json::json!({
        "apiVersion": "v1alpha1",
        "kind": "KubeNodeConfig",
        "nodeIP": { "validSubnets": subnets },
    })
}

fn legacy_replacement(cfg: &MachineConfig, link: &str) -> Option<serde_json::Value> {
    cfg.legacy_entry(link)?;
    let mut remaining = Vec::new();
    for entry in &cfg.legacy_interfaces {
        if entry.get("interface").and_then(|v| v.as_str()) == Some(link) {
            continue;
        }
        remaining.push(serde_json::to_value(entry).ok()?);
    }
    let interfaces = if remaining.is_empty() {
        serde_json::json!({ "$patch": "delete" })
    } else {
        serde_json::json!(remaining)
    };
    Some(serde_json::json!({
        "machine": { "network": { "interfaces": interfaces } }
    }))
}

fn join_docs(docs: &[serde_json::Value]) -> Result<String, String> {
    let mut out = String::new();
    for doc in docs {
        if !out.is_empty() {
            out.push_str("---\n");
        }
        let yaml = serde_yaml::to_string(doc)
            .map_err(|e| format!("encoding a machine-config patch: {e}"))?;
        out.push_str(&yaml);
    }
    Ok(out)
}

/// Build the certSANs union and shrink patches. `confirm` must always differ
/// from `pre`, because Talos only cancels a `--mode=try` revert on a real
/// configuration update.
fn cert_san_patches(
    existing: &[String],
    old: Option<&str>,
    new: Option<&str>,
) -> Result<(String, String), String> {
    let mut pre = existing.to_vec();
    let mut confirm = existing.to_vec();
    if let Some(old) = old {
        push_unique(&mut pre, old);
    }
    if let Some(new) = new {
        push_unique(&mut pre, new);
        if Some(new) != old {
            push_unique(&mut confirm, new);
        }
    }
    if pre == confirm {
        push_unique(&mut pre, CONFIRM_SENTINEL);
    }
    let pre = serde_json::json!({ "machine": { "certSANs": pre } }).to_string();
    let confirm = serde_json::json!({ "machine": { "certSANs": confirm } }).to_string();
    Ok((pre, confirm))
}

fn push_unique(list: &mut Vec<String>, value: &str) {
    if !list.iter().any(|v| v == value) {
        list.push(value.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::config::MachineConfig;

    const CONFIG: &str = r#"
apiVersion: v1alpha1
kind: LinkConfig
name: eth0
mtu: 1500
---
apiVersion: v1alpha1
kind: DHCPv4Config
name: eth0
---
apiVersion: v1alpha1
kind: KubeNodeConfig
nodeIP:
  validSubnets:
    - 10.0.0.0/24
"#;

    fn cfg() -> MachineConfig {
        MachineConfig::parse(CONFIG).unwrap()
    }

    fn static_change() -> Change {
        Change {
            link: "eth0".into(),
            addressing: Some(Addressing::Static),
            addresses: vec!["10.0.0.10/24".parse().unwrap()],
            gateway: Some("10.0.0.1".parse().unwrap()),
            mtu: None,
            nameservers: ResolverMode::Unchanged,
            search_domains: SearchMode::Unchanged,
        }
    }

    #[test]
    fn static_patch_adds_link_routes_and_removes_dhcp() {
        let built = build(&static_change(), &cfg(), Some("10.0.0.20")).unwrap();
        assert!(built.patch.contains("kind: LinkConfig"));
        assert!(built.patch.contains("address: 10.0.0.10/24"));
        assert!(built.patch.contains("gateway: 10.0.0.1"));
        assert!(built.patch.contains("kind: DHCPv4Config"));
        assert!(built.patch.contains("$patch: delete"));
        assert_eq!(built.target_ip.as_deref(), Some("10.0.0.10"));
        assert!(built.pre_patch.contains("10.0.0.20"));
        assert!(built.pre_patch.contains("10.0.0.10"));
        assert_ne!(built.pre_patch, built.confirm_patch);
    }

    #[test]
    fn static_patch_always_carries_the_kube_node_subnet() {
        let built = build(&static_change(), &cfg(), None).unwrap();
        assert!(built.patch.contains("kind: KubeNodeConfig"));
        assert!(built.patch.contains("10.0.0.0/24"));
    }

    #[test]
    fn dhcp_patch_adds_dhcp_and_clears_static_fields() {
        let change = Change {
            link: "eth0".into(),
            addressing: Some(Addressing::Dhcp),
            addresses: vec![],
            gateway: None,
            mtu: None,
            nameservers: ResolverMode::Unchanged,
            search_domains: SearchMode::Unchanged,
        };
        let built = build(&change, &cfg(), None).unwrap();
        assert!(built.patch.contains("kind: DHCPv4Config"));
        assert!(!built
            .patch
            .contains("$patch: delete\n    name: DHCPv4Config"));
        assert!(built.patch.contains("addresses:"));
        assert_eq!(built.target_ip, None);
    }

    #[test]
    fn mtu_only_patch_touches_only_the_link() {
        let change = Change {
            link: "eth0".into(),
            addressing: None,
            addresses: vec![],
            gateway: None,
            mtu: Some(1400),
            nameservers: ResolverMode::Unchanged,
            search_domains: SearchMode::Unchanged,
        };
        let built = build(&change, &cfg(), None).unwrap();
        assert!(built.patch.contains("mtu: 1400"));
        assert!(!built.patch.contains("addresses"));
        assert!(built.needs_cilium_restart);
    }

    #[test]
    fn resolver_patch_preserves_host_dns() {
        let change = Change {
            link: "eth0".into(),
            addressing: None,
            addresses: vec![],
            gateway: None,
            mtu: None,
            nameservers: ResolverMode::Explicit(vec!["1.1.1.1".parse().unwrap()]),
            search_domains: SearchMode::Inherit,
        };
        let built = build(&change, &cfg(), None).unwrap();
        assert!(built.patch.contains("kind: ResolverConfig"));
        assert!(built.patch.contains("address: 1.1.1.1"));
        assert!(built.patch.contains("searchDomains:"));
        assert!(!built.patch.contains("hostDNS"));
    }

    #[test]
    fn legacy_entry_is_removed_in_the_same_patch() {
        let raw = r#"
machine:
  network:
    interfaces:
      - interface: eth0
        dhcp: true
      - interface: eth1
        dhcp: true
"#;
        let cfg = MachineConfig::parse(raw).unwrap();
        let built = build(&static_change(), &cfg, None).unwrap();
        assert!(built.patch.contains("machine:"));
        assert!(built.patch.contains("interfaces:"));
        assert!(built.patch.contains("eth1"));
        // eth0 is replaced by the new documents, so the legacy entry is gone.
        assert!(!built.patch.contains("interface: eth0"));
    }

    #[test]
    fn confirming_patch_always_differs_from_the_pre_patch() {
        // No address change: pre adds the old primary, confirm drops it.
        let change = Change {
            link: "eth0".into(),
            addressing: None,
            addresses: vec![],
            gateway: None,
            mtu: Some(1400),
            nameservers: ResolverMode::Unchanged,
            search_domains: SearchMode::Unchanged,
        };
        let built = build(&change, &cfg(), Some("10.0.0.20")).unwrap();
        assert_ne!(built.pre_patch, built.confirm_patch);
        assert!(!built.confirm_patch.contains("10.0.0.20"));

        // The old IP is already an explicit certSAN: the sentinel makes the
        // confirming update real.
        let mut cfg = cfg();
        cfg.cert_sans = vec!["10.0.0.20".into()];
        let built = build(&change, &cfg, Some("10.0.0.20")).unwrap();
        assert_ne!(built.pre_patch, built.confirm_patch);
        assert!(built.pre_patch.contains(CONFIRM_SENTINEL));
    }

    #[test]
    fn verify_matches_the_target_config() {
        let change = static_change();
        let applied = MachineConfig::parse(
            r#"
apiVersion: v1alpha1
kind: LinkConfig
name: eth0
addresses:
  - address: 10.0.0.10/24
routes:
  - gateway: 10.0.0.1
"#,
        )
        .unwrap();
        assert!(verify(&change, &applied));
        assert!(!verify(&change, &cfg()));
    }
}
