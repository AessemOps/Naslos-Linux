//! The read-only network inventory behind `GET /api/network` (FR-NET-01).
//!
//! Talos status resources are parsed tolerantly: the field names of
//! `LinkStatus`/`RouteStatus`/`ResolverStatus` have moved between Talos
//! releases, and an unknown extra field must never make the page unusable.

use std::collections::BTreeMap;

use crate::talos::TalosClient;

use super::changes::ChangeView;
use super::config::MachineConfig;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inventory {
    pub hostname: HostnameView,
    pub resolvers: ResolversView,
    pub primary_link: String,
    pub links: Vec<LinkView>,
    pub appliance: ApplianceView,
    pub pending: Option<ChangeView>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct HostnameView {
    pub value: String,
    pub source: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolversView {
    pub inherit: bool,
    pub nameservers: Vec<ResolverView>,
    pub search_domains: Vec<String>,
    pub search_inherit: bool,
    pub host_dns: HostDnsView,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ResolverView {
    pub address: String,
    pub protocol: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostDnsView {
    pub enabled: bool,
    pub forward_kube_dns_to_host: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LinkView {
    pub name: String,
    pub kind: String,
    pub up: bool,
    pub mac: String,
    pub mtu: u32,
    #[serde(rename = "speedMbps")]
    pub speed_mbps: Option<u64>,
    pub duplex: Option<String>,
    pub addresses: Vec<AddressView>,
    pub gateway: Option<String>,
    pub config: LinkConfigView,
    pub features: FeaturesView,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AddressView {
    pub address: String,
    pub source: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LinkConfigView {
    pub addressing: String,
    pub addresses: Vec<String>,
    pub routes: Vec<RouteView>,
    pub mtu: Option<u32>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RouteView {
    pub destination: Option<String>,
    pub gateway: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FeaturesView {
    pub wol: Vec<String>,
    pub ring_rx: Option<u32>,
    pub ring_tx: Option<u32>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ApplianceView {
    pub domain: String,
    #[serde(rename = "lanCidr")]
    pub lan_cidr: String,
}

/// Collect the live inventory. A missing speed/duplex or ethernet-status read
/// is normal and degrades to `null`/empty, never to an error.
pub async fn collect(
    talos: &TalosClient,
    appliance: ApplianceView,
    pending: Option<ChangeView>,
) -> Result<Inventory, String> {
    let raw_config = talos.machine_config().await?;
    let cfg = MachineConfig::parse(&raw_config)?;

    let link_values = talos.links().await.unwrap_or_default();
    let address_values = talos.addresses().await.unwrap_or_default();
    let route_values = talos.routes().await.unwrap_or_default();
    let resolver_values = talos.resolvers().await.unwrap_or_default();
    let ethernet_values = talos.ethernet_status().await.unwrap_or_default();
    let running_hostname = talos.hostname().await;

    let addresses_by_link = group_addresses(&address_values);
    let default_gateway = default_gateway(&route_values);
    let physical = physical_links(&link_values);
    let primary_link = primary_link(&physical, &addresses_by_link, &cfg);

    let mut links = Vec::new();
    for link in &physical {
        let ethernet = ethernet_for(&ethernet_values, &link.name);
        let features = FeaturesView {
            wol: ethernet
                .and_then(|spec| spec.get("wakeOnLan").or_else(|| spec.get("wol")))
                .map(|v| match v {
                    serde_json::Value::Array(items) => items
                        .iter()
                        .filter_map(|v| v.as_str())
                        .map(str::to_string)
                        .collect(),
                    serde_json::Value::Bool(true) => vec!["magic".to_string()],
                    _ => Vec::new(),
                })
                .unwrap_or_default(),
            ring_rx: ethernet
                .and_then(|spec| spec.get("rings"))
                .and_then(|rings| rings.get("rx"))
                .and_then(|v| v.as_u64())
                .and_then(|v| u32::try_from(v).ok()),
            ring_tx: ethernet
                .and_then(|spec| spec.get("rings"))
                .and_then(|rings| rings.get("tx"))
                .and_then(|v| v.as_u64())
                .and_then(|v| u32::try_from(v).ok()),
        };
        let config = cfg.links.get(&link.name).cloned().unwrap_or_default();
        let addressing = if cfg.dhcp4.contains(&link.name) || cfg.dhcp6.contains(&link.name) {
            "dhcp"
        } else if !config.addresses.is_empty() {
            "static"
        } else {
            "dhcp"
        };
        let mut link_addresses = addresses_by_link
            .get(&link.name)
            .cloned()
            .unwrap_or_default();
        for address in &mut link_addresses {
            if address.source == "link" || address.source == "loopback" {
                continue;
            }
            address.source = if config.addresses.iter().any(|a| a == &address.address) {
                "static".to_string()
            } else if cfg.dhcp4.contains(&link.name) {
                "dhcp4".to_string()
            } else if cfg.dhcp6.contains(&link.name) {
                "dhcp6".to_string()
            } else {
                address.source.clone()
            };
        }
        link_addresses.sort_by_key(|a| (a.source != "static", a.address.clone()));
        links.push(LinkView {
            name: link.name.clone(),
            kind: link.kind.clone(),
            up: link.up,
            mac: link.mac.clone(),
            mtu: link.mtu,
            speed_mbps: talos.link_speed(&link.name).await,
            duplex: talos.link_duplex(&link.name).await,
            addresses: link_addresses,
            gateway: if link.name == primary_link {
                default_gateway.clone()
            } else {
                config.routes.first().map(|route| route.gateway.clone())
            },
            config: LinkConfigView {
                addressing: addressing.to_string(),
                addresses: config.addresses.clone(),
                routes: config
                    .routes
                    .iter()
                    .map(|route| RouteView {
                        destination: route.destination.clone(),
                        gateway: route.gateway.clone(),
                    })
                    .collect(),
                mtu: config.mtu,
            },
            features,
        });
    }

    let hostname = hostname_view(&cfg, running_hostname);
    let resolvers = resolvers_view(&cfg, &resolver_values);

    Ok(Inventory {
        hostname,
        resolvers,
        primary_link,
        links,
        appliance,
        pending,
    })
}

/// Names of the editable physical links (wire links only; the loopback, VLAN,
/// bond and bridge documents are out of scope).
pub fn physical_link_names(values: &[serde_json::Value]) -> Vec<String> {
    physical_links(values)
        .into_iter()
        .map(|link| link.name)
        .collect()
}

#[derive(Debug, Clone)]
struct LinkRow {
    name: String,
    kind: String,
    up: bool,
    mac: String,
    mtu: u32,
}

fn physical_links(values: &[serde_json::Value]) -> Vec<LinkRow> {
    let mut links = Vec::new();
    for value in values {
        let Some(spec) = value.get("spec") else {
            continue;
        };
        let name = text(spec, &["name", "linkName"]);
        if name.is_empty() {
            continue;
        }
        let kind = text(spec, &["kind", "type"]);
        let kind = if kind.is_empty() {
            infer_kind(&name)
        } else {
            kind
        };
        if kind != "ether" {
            continue;
        }
        links.push(LinkRow {
            name: name.clone(),
            kind,
            up: spec.get("up").and_then(|v| v.as_bool()).unwrap_or(true),
            mac: text(spec, &["hardwareAddr", "mac", "permanentAddr"]),
            mtu: spec
                .get("mtu")
                .and_then(|v| v.as_u64())
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(0),
        });
    }
    links.sort_by(|a, b| a.name.cmp(&b.name));
    links
}

fn infer_kind(name: &str) -> String {
    if name == "lo" || name.starts_with("lo") {
        "loopback".to_string()
    } else if name.starts_with("vlan") || name.contains('.') {
        "vlan".to_string()
    } else if name.starts_with("bond") {
        "bond".to_string()
    } else if name.starts_with("br") {
        "bridge".to_string()
    } else {
        "ether".to_string()
    }
}

fn group_addresses(values: &[serde_json::Value]) -> BTreeMap<String, Vec<AddressView>> {
    let mut out: BTreeMap<String, Vec<AddressView>> = BTreeMap::new();
    for value in values {
        let Some(spec) = value.get("spec") else {
            continue;
        };
        let link = text(spec, &["linkName", "name"]);
        let address = text(spec, &["address", "addr"]);
        if link.is_empty() || address.is_empty() {
            continue;
        }
        let source = match address.parse::<ipnet::IpNet>() {
            Ok(net) if net.addr().is_loopback() => "loopback",
            Ok(net) if is_link_local(&net.addr()) => "link",
            _ => "other",
        };
        out.entry(link).or_default().push(AddressView {
            address,
            source: source.to_string(),
        });
    }
    out
}

fn default_gateway(values: &[serde_json::Value]) -> Option<String> {
    for value in values {
        let Some(spec) = value.get("spec") else {
            continue;
        };
        let destination = text(spec, &["destination", "dst"]);
        if destination.is_empty()
            || destination == "0.0.0.0/0"
            || destination == "::/0"
            || destination == "default"
        {
            let gateway = text(spec, &["gateway", "via"]);
            if !gateway.is_empty() {
                return Some(gateway);
            }
        }
    }
    None
}

fn primary_link(
    physical: &[LinkRow],
    addresses: &BTreeMap<String, Vec<AddressView>>,
    cfg: &MachineConfig,
) -> String {
    for link in physical {
        if let Some(rows) = addresses.get(&link.name) {
            if rows.iter().any(|row| {
                row.address
                    .parse::<ipnet::IpNet>()
                    .map(|net| net.addr().is_ipv4() && row.source != "link")
                    .unwrap_or(false)
            }) {
                return link.name.clone();
            }
        }
    }
    cfg.links
        .keys()
        .next()
        .cloned()
        .unwrap_or_else(|| physical.first().map(|l| l.name.clone()).unwrap_or_default())
}

fn hostname_view(cfg: &MachineConfig, running: Option<String>) -> HostnameView {
    let source = cfg
        .hostname
        .as_ref()
        .map(|h| h.source.clone())
        .unwrap_or_else(|| "auto-stable".to_string());
    let value = cfg
        .hostname
        .as_ref()
        .filter(|h| !h.value.is_empty())
        .map(|h| h.value.clone())
        .or(running)
        .unwrap_or_default();
    HostnameView { value, source }
}

fn resolvers_view(cfg: &MachineConfig, status: &[serde_json::Value]) -> ResolversView {
    let mut live: Vec<String> = Vec::new();
    let mut live_search: Vec<String> = Vec::new();
    for value in status {
        let Some(spec) = value.get("spec") else {
            continue;
        };
        for key in ["resolvers", "nameservers", "dnsServers"] {
            if let Some(items) = spec.get(key).and_then(|v| v.as_array()) {
                for item in items {
                    let address = match item {
                        serde_json::Value::String(s) => s.clone(),
                        other => text(other, &["address", "addr"]),
                    };
                    if !address.is_empty() {
                        push_unique(&mut live, address);
                    }
                }
            }
        }
        if let Some(items) = spec.get("searchDomains").and_then(|v| v.as_array()) {
            for item in items.iter().filter_map(|v| v.as_str()) {
                push_unique(&mut live_search, item.to_string());
            }
        }
    }

    let (inherit, nameservers) = match &cfg.resolver.nameservers {
        Some(list) => (
            false,
            list.iter()
                .map(|address| ResolverView {
                    address: address.clone(),
                    protocol: "Do53".to_string(),
                })
                .collect(),
        ),
        None => (
            true,
            live.into_iter()
                .map(|address| ResolverView {
                    address,
                    protocol: "Do53".to_string(),
                })
                .collect(),
        ),
    };
    let (search_inherit, search_domains) = match &cfg.resolver.search_domains {
        Some(list) => (false, list.clone()),
        None => (true, live_search),
    };
    ResolversView {
        inherit,
        nameservers,
        search_domains,
        search_inherit,
        host_dns: HostDnsView {
            enabled: cfg.resolver.host_dns_enabled.unwrap_or(false),
            forward_kube_dns_to_host: cfg.resolver.forward_kube_dns_to_host.unwrap_or(false),
        },
    }
}

fn ethernet_for<'a>(values: &'a [serde_json::Value], link: &str) -> Option<&'a serde_json::Value> {
    values.iter().find_map(|value| {
        let spec = value.get("spec")?;
        let name = text(spec, &["name", "linkName"]);
        (name == link).then_some(spec)
    })
}

/// The node's `InternalIP` from `kubectl get nodes -o json`. IPv4 preferred.
pub fn node_internal_ip(nodes: &serde_json::Value) -> Option<String> {
    let items = nodes.get("items")?.as_array()?;
    let mut ipv6 = None;
    for node in items {
        let addresses = node.get("status")?.get("addresses")?.as_array()?;
        for address in addresses {
            if address.get("type").and_then(|v| v.as_str()) != Some("InternalIP") {
                continue;
            }
            let Some(value) = address.get("address").and_then(|v| v.as_str()) else {
                continue;
            };
            if value
                .parse::<std::net::IpAddr>()
                .map(|ip| ip.is_ipv4())
                .unwrap_or(false)
            {
                return Some(value.to_string());
            }
            ipv6.get_or_insert_with(|| value.to_string());
        }
    }
    ipv6
}

/// The best routable address across all links (IPv4 preferred).
pub fn primary_address(values: &[serde_json::Value]) -> Option<String> {
    let by_link = crate::talos::addresses_by_link(values);
    let mut entries: Vec<(String, String)> = by_link.into_iter().collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let v4 = entries.iter().find(|(_, address)| {
        address
            .parse::<std::net::IpAddr>()
            .map(|ip| ip.is_ipv4())
            .unwrap_or(false)
    });
    v4.or_else(|| entries.first())
        .map(|(_, address)| address.clone())
}

fn text(spec: &serde_json::Value, keys: &[&str]) -> String {
    for key in keys {
        if let Some(value) = spec.get(*key).and_then(|v| v.as_str()) {
            if !value.is_empty() {
                return value.to_string();
            }
        }
    }
    String::new()
}

fn is_link_local(addr: &std::net::IpAddr) -> bool {
    match addr {
        std::net::IpAddr::V4(v4) => v4.is_link_local(),
        std::net::IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) == 0xfe80,
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

    #[test]
    fn only_ether_links_are_editable() {
        let values: Vec<serde_json::Value> = serde_json::from_str(
            r#"[
              {"spec":{"name":"lo","kind":"loopback"}},
              {"spec":{"name":"eth0","kind":"ether"}},
              {"spec":{"name":"vlan10","kind":"vlan"}}
            ]"#,
        )
        .unwrap();
        assert_eq!(physical_link_names(&values), vec!["eth0".to_string()]);
    }

    #[test]
    fn default_gateway_prefers_a_default_route() {
        let values: Vec<serde_json::Value> = serde_json::from_str(
            r#"[
              {"spec":{"destination":"10.0.0.0/24","gateway":"10.0.0.1"}},
              {"spec":{"destination":"0.0.0.0/0","gateway":"10.0.0.254"}}
            ]"#,
        )
        .unwrap();
        assert_eq!(default_gateway(&values).as_deref(), Some("10.0.0.254"));
    }

    #[test]
    fn internal_ip_prefers_ipv4() {
        let nodes: serde_json::Value = serde_json::from_str(
            r#"{"items":[{"status":{"addresses":[
              {"type":"InternalIP","address":"fe80::1"},
              {"type":"InternalIP","address":"10.0.0.10"}
            ]}}]}"#,
        )
        .unwrap();
        assert_eq!(node_internal_ip(&nodes).as_deref(), Some("10.0.0.10"));
    }

    #[test]
    fn serializes_the_ui_contract_keys() {
        let inventory = Inventory {
            hostname: HostnameView {
                value: "node".into(),
                source: "auto-stable".into(),
            },
            resolvers: ResolversView {
                inherit: true,
                nameservers: Vec::new(),
                search_domains: vec!["lan".into()],
                search_inherit: true,
                host_dns: HostDnsView {
                    enabled: true,
                    forward_kube_dns_to_host: false,
                },
            },
            primary_link: "eth0".into(),
            links: vec![LinkView {
                name: "eth0".into(),
                kind: "ether".into(),
                up: true,
                mac: String::new(),
                mtu: 1500,
                speed_mbps: Some(2500),
                duplex: Some("full".into()),
                addresses: Vec::new(),
                gateway: None,
                config: LinkConfigView {
                    addressing: "dhcp".into(),
                    addresses: Vec::new(),
                    routes: Vec::new(),
                    mtu: None,
                },
                features: FeaturesView {
                    wol: Vec::new(),
                    ring_rx: None,
                    ring_tx: None,
                },
            }],
            appliance: ApplianceView {
                domain: "example.test".into(),
                lan_cidr: "10.0.0.0/24".into(),
            },
            pending: None,
        };
        let value = serde_json::to_value(inventory).unwrap();
        assert_eq!(value["primaryLink"], "eth0");
        assert_eq!(value["resolvers"]["searchDomains"][0], "lan");
        assert_eq!(value["resolvers"]["searchInherit"], true);
        assert_eq!(value["resolvers"]["hostDns"]["forwardKubeDnsToHost"], false);
        assert_eq!(value["links"][0]["speedMbps"], 2500);
        assert_eq!(value["appliance"]["lanCidr"], "10.0.0.0/24");
    }

    #[test]
    fn hostname_falls_back_to_the_running_value() {
        let cfg = MachineConfig::default();
        let view = hostname_view(&cfg, Some("node-1".into()));
        assert_eq!(view.value, "node-1");
        assert_eq!(view.source, "auto-stable");
    }
}
