//! A typed view of the Talos machine configuration for the host network
//! settings page (FR-NET).
//!
//! Talos v1.14 emits the per-link documents (`LinkConfig`, `DHCPv4Config`,
//! `DHCPv6Config`, `ResolverConfig`, `HostnameConfig`, `KubeNodeConfig`) and
//! still accepts the legacy `machine.network.interfaces` list and
//! `machine.certSANs`. The parser understands both so the API can migrate a
//! legacy entry on the first edit instead of leaving two competing sources of
//! truth.

use serde::Deserialize as _;
use std::collections::{BTreeMap, BTreeSet};

/// A route as stored in a `LinkConfig` document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteDoc {
    pub destination: Option<String>,
    pub gateway: String,
}

/// The effective addressing for one physical link.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkConfigDoc {
    pub addresses: Vec<String>,
    pub routes: Vec<RouteDoc>,
    pub mtu: Option<u32>,
    pub up: Option<bool>,
}

/// The resolver document. `None` means "not set" (inherit from DHCP).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolverDoc {
    pub nameservers: Option<Vec<String>>,
    pub search_domains: Option<Vec<String>>,
    pub host_dns_enabled: Option<bool>,
    pub forward_kube_dns_to_host: Option<bool>,
}

impl ResolverDoc {
    pub fn is_unset(&self) -> bool {
        self.nameservers.is_none()
            && self.search_domains.is_none()
            && self.host_dns_enabled.is_none()
            && self.forward_kube_dns_to_host.is_none()
    }
}

/// Where the node hostname comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostnameDoc {
    pub value: String,
    pub source: String,
}

/// A parsed machine configuration plus the raw multi-document YAML (the raw
/// form is the revert snapshot, so it is never reconstructed from this view).
#[derive(Debug, Clone, Default)]
pub struct MachineConfig {
    pub raw: String,
    pub links: BTreeMap<String, LinkConfigDoc>,
    pub dhcp4: BTreeSet<String>,
    pub dhcp6: BTreeSet<String>,
    pub resolver: ResolverDoc,
    pub hostname: Option<HostnameDoc>,
    pub valid_subnets: Vec<String>,
    pub cert_sans: Vec<String>,
    /// The legacy `machine.network.interfaces` entries, verbatim.
    pub legacy_interfaces: Vec<serde_yaml::Value>,
}

impl MachineConfig {
    /// Parse the multi-document YAML returned by
    /// `talosctl get machineconfig -o yaml`.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let mut cfg = Self {
            raw: raw.to_string(),
            ..Default::default()
        };
        for doc in serde_yaml::Deserializer::from_str(raw) {
            let value = serde_yaml::Value::deserialize(doc)
                .map_err(|e| format!("parsing the machine config: {e}"))?;
            cfg.ingest(&value);
        }
        cfg.apply_legacy_links();
        Ok(cfg)
    }

    fn ingest(&mut self, doc: &serde_yaml::Value) {
        // A legacy v1alpha1 doc is a single mapping with a `machine` key.
        if let Some(machine) = doc.get("machine") {
            self.ingest_legacy_machine(machine);
        }
        let Some(kind) = doc.get("kind").and_then(|v| v.as_str()) else {
            return;
        };
        match kind {
            "LinkConfig" => self.ingest_link(doc),
            "DHCPv4Config" => {
                if let Some(name) = doc.get("name").and_then(|v| v.as_str()) {
                    self.dhcp4.insert(name.to_string());
                }
            }
            "DHCPv6Config" => {
                if let Some(name) = doc.get("name").and_then(|v| v.as_str()) {
                    self.dhcp6.insert(name.to_string());
                }
            }
            "ResolverConfig" => self.ingest_resolver(doc),
            "HostnameConfig" => self.ingest_hostname(doc),
            "KubeNodeConfig" => {
                if let Some(subnets) = doc
                    .get("nodeIP")
                    .and_then(|v| v.get("validSubnets"))
                    .and_then(|v| v.as_sequence())
                {
                    for net in subnets.iter().filter_map(|v| v.as_str()) {
                        push_unique(&mut self.valid_subnets, net.to_string());
                    }
                }
            }
            _ => {}
        }
    }

    fn ingest_legacy_machine(&mut self, machine: &serde_yaml::Value) {
        if let Some(sans) = machine.get("certSANs").and_then(|v| v.as_sequence()) {
            for san in sans.iter().filter_map(|v| v.as_str()) {
                push_unique(&mut self.cert_sans, san.to_string());
            }
        }
        if let Some(network) = machine.get("network") {
            if let Some(interfaces) = network.get("interfaces").and_then(|v| v.as_sequence()) {
                for entry in interfaces {
                    self.legacy_interfaces.push(entry.clone());
                }
            }
            if let Some(hostname) = network.get("hostname").and_then(|v| v.as_str()) {
                if !hostname.is_empty() {
                    self.hostname = Some(HostnameDoc {
                        value: hostname.to_string(),
                        source: "static".to_string(),
                    });
                }
            }
            if let Some(nameservers) = network.get("nameservers").and_then(|v| v.as_sequence()) {
                self.resolver.nameservers = Some(
                    nameservers
                        .iter()
                        .filter_map(|v| v.as_str())
                        .map(str::to_string)
                        .collect(),
                );
            }
            if let Some(domains) = network.get("searchDomains").and_then(|v| v.as_sequence()) {
                self.resolver.search_domains = Some(
                    domains
                        .iter()
                        .filter_map(|v| v.as_str())
                        .map(str::to_string)
                        .collect(),
                );
            }
        }
    }

    fn ingest_link(&mut self, doc: &serde_yaml::Value) {
        let Some(name) = doc.get("name").and_then(|v| v.as_str()) else {
            return;
        };
        let entry = self.links.entry(name.to_string()).or_default();
        if let Some(addresses) = doc.get("addresses").and_then(|v| v.as_sequence()) {
            entry.addresses = addresses
                .iter()
                .filter_map(|v| match v {
                    serde_yaml::Value::String(s) => Some(s.clone()),
                    other => other
                        .get("address")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                })
                .collect();
        }
        if let Some(routes) = doc.get("routes").and_then(|v| v.as_sequence()) {
            entry.routes = routes
                .iter()
                .filter_map(|v| {
                    let gateway = v.get("gateway").and_then(|g| g.as_str())?.to_string();
                    Some(RouteDoc {
                        destination: v
                            .get("destination")
                            .and_then(|d| d.as_str())
                            .map(str::to_string),
                        gateway,
                    })
                })
                .collect();
        }
        if let Some(mtu) = doc.get("mtu").and_then(|v| v.as_u64()) {
            entry.mtu = u32::try_from(mtu).ok();
        }
        if let Some(up) = doc.get("up").and_then(|v| v.as_bool()) {
            entry.up = Some(up);
        }
    }

    fn ingest_resolver(&mut self, doc: &serde_yaml::Value) {
        if let Some(nameservers) = doc.get("nameservers").and_then(|v| v.as_sequence()) {
            self.resolver.nameservers = Some(
                nameservers
                    .iter()
                    .filter_map(|v| match v {
                        serde_yaml::Value::String(s) => Some(s.clone()),
                        other => other
                            .get("address")
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                    })
                    .collect(),
            );
        }
        if let Some(domains) = doc.get("searchDomains").and_then(|v| v.as_sequence()) {
            self.resolver.search_domains = Some(
                domains
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(str::to_string)
                    .collect(),
            );
        }
        if let Some(host_dns) = doc.get("hostDNS") {
            self.resolver.host_dns_enabled = host_dns.get("enabled").and_then(|v| v.as_bool());
            self.resolver.forward_kube_dns_to_host = host_dns
                .get("forwardKubeDNSToHost")
                .and_then(|v| v.as_bool());
        }
    }

    fn ingest_hostname(&mut self, doc: &serde_yaml::Value) {
        if let Some(value) = doc.get("hostname").and_then(|v| v.as_str()) {
            if !value.is_empty() {
                self.hostname = Some(HostnameDoc {
                    value: value.to_string(),
                    source: "static".to_string(),
                });
            }
            return;
        }
        if let Some(auto) = doc.get("auto").and_then(|v| v.as_str()) {
            // `auto: stable` is the appliance default; the running hostname is
            // read from /proc and patched in by the inventory builder.
            self.hostname = Some(HostnameDoc {
                value: String::new(),
                source: format!("auto-{auto}"),
            });
        }
    }

    /// Fold legacy `machine.network.interfaces` entries into `links` when the
    /// document form does not already describe that link. The raw entries stay
    /// in `legacy_interfaces` so the patch builder can delete them.
    fn apply_legacy_links(&mut self) {
        for entry in &self.legacy_interfaces {
            let Some(name) = entry.get("interface").and_then(|v| v.as_str()) else {
                continue;
            };
            let dhcp = entry.get("dhcp").and_then(|v| v.as_bool()).unwrap_or(false);
            if dhcp {
                self.dhcp4.insert(name.to_string());
            }
            let link = self.links.entry(name.to_string()).or_default();
            if let Some(addresses) = entry.get("addresses").and_then(|v| v.as_sequence()) {
                for address in addresses.iter().filter_map(|v| v.as_str()) {
                    push_unique(&mut link.addresses, address.to_string());
                }
            }
            if let Some(routes) = entry.get("routes").and_then(|v| v.as_sequence()) {
                for route in routes {
                    if let Some(gateway) = route.get("gateway").and_then(|v| v.as_str()) {
                        if !link.routes.iter().any(|r| r.gateway == gateway) {
                            link.routes.push(RouteDoc {
                                destination: route
                                    .get("network")
                                    .or_else(|| route.get("destination"))
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string),
                                gateway: gateway.to_string(),
                            });
                        }
                    }
                }
            }
            if let Some(mtu) = entry.get("mtu").and_then(|v| v.as_u64()) {
                if link.mtu.is_none() {
                    link.mtu = u32::try_from(mtu).ok();
                }
            }
        }
    }

    /// The legacy entry for `link`, if the stored config still carries one.
    pub fn legacy_entry(&self, link: &str) -> Option<&serde_yaml::Value> {
        self.legacy_interfaces
            .iter()
            .find(|entry| entry.get("interface").and_then(|v| v.as_str()) == Some(link))
    }
}

fn push_unique(list: &mut Vec<String>, value: String) {
    if !list.iter().any(|v| v == &value) {
        list.push(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC_FORM: &str = r#"
apiVersion: v1alpha1
kind: LinkConfig
name: eth0
addresses:
  - address: 10.0.0.10/24
routes:
  - gateway: 10.0.0.1
mtu: 1500
---
apiVersion: v1alpha1
kind: ResolverConfig
nameservers:
  - address: 1.1.1.1
    protocol: Do53
searchDomains:
  - lan
hostDNS:
  enabled: true
  forwardKubeDNSToHost: false
---
apiVersion: v1alpha1
kind: HostnameConfig
auto: stable
---
apiVersion: v1alpha1
kind: KubeNodeConfig
nodeIP:
  validSubnets:
    - 10.0.0.0/24
machine:
  certSANs:
    - 10.0.0.99
"#;

    #[test]
    fn parses_document_form() {
        let cfg = MachineConfig::parse(DOC_FORM).unwrap();
        assert_eq!(cfg.links["eth0"].addresses, vec!["10.0.0.10/24"]);
        assert_eq!(cfg.links["eth0"].routes[0].gateway, "10.0.0.1");
        assert_eq!(cfg.links["eth0"].mtu, Some(1500));
        assert_eq!(cfg.resolver.nameservers, Some(vec!["1.1.1.1".to_string()]));
        assert_eq!(cfg.resolver.search_domains, Some(vec!["lan".to_string()]));
        assert_eq!(cfg.resolver.host_dns_enabled, Some(true));
        assert_eq!(cfg.resolver.forward_kube_dns_to_host, Some(false));
        assert_eq!(cfg.hostname.as_ref().unwrap().source, "auto-stable");
        assert_eq!(cfg.valid_subnets, vec!["10.0.0.0/24"]);
        assert_eq!(cfg.cert_sans, vec!["10.0.0.99"]);
    }

    #[test]
    fn parses_legacy_interface_list() {
        let raw = r#"
machine:
  network:
    hostname: node-1
    interfaces:
      - interface: eth1
        dhcp: false
        addresses:
          - 10.1.0.5/24
        routes:
          - gateway: 10.1.0.1
        mtu: 9000
"#;
        let cfg = MachineConfig::parse(raw).unwrap();
        assert!(cfg.dhcp4.is_empty());
        assert_eq!(cfg.links["eth1"].addresses, vec!["10.1.0.5/24"]);
        assert_eq!(cfg.links["eth1"].mtu, Some(9000));
        assert!(cfg.legacy_entry("eth1").is_some());
        assert_eq!(cfg.hostname.unwrap().value, "node-1");
    }

    #[test]
    fn legacy_dhcp_entry_marks_the_link_as_dhcp() {
        let raw = r#"
machine:
  network:
    interfaces:
      - interface: eth0
        dhcp: true
"#;
        let cfg = MachineConfig::parse(raw).unwrap();
        assert!(cfg.dhcp4.contains("eth0"));
    }
}
