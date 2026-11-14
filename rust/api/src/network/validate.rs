//! Validation for the network change request (FR-NET-02).
//!
//! Every rule is a pure function over typed values; no user string ever reaches
//! a command line. The caller supplies the set of physical link names and the
//! parsed current machine config, so the same rules are exercised by unit tests
//! without a node.

use std::collections::BTreeSet;
use std::net::IpAddr;

use ipnet::IpNet;

/// The request body of `POST /api/network/changes`.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct ChangeRequest {
    #[serde(default)]
    pub link: String,
    #[serde(default)]
    pub addressing: Option<String>,
    #[serde(default)]
    pub addresses: Option<Vec<String>>,
    #[serde(default)]
    pub gateway: Option<String>,
    #[serde(default)]
    pub mtu: Option<u32>,
    #[serde(default, rename = "confirmLowMtu")]
    pub confirm_low_mtu: bool,
    #[serde(default)]
    pub nameservers: Option<ModeValues>,
    #[serde(default, rename = "searchDomains")]
    pub search_domains: Option<ModeValues>,
}

/// A `mode`-tagged list. `servers` is used for DNS nameservers and `domains`
/// for search domains.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct ModeValues {
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub servers: Vec<String>,
    #[serde(default)]
    pub domains: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Addressing {
    Dhcp,
    Static,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolverMode {
    Unchanged,
    Inherit,
    Explicit(Vec<IpAddr>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchMode {
    Unchanged,
    Inherit,
    Explicit(Vec<String>),
}

/// A validated change. `addresses` is empty for a DHCP change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub link: String,
    pub addressing: Option<Addressing>,
    pub addresses: Vec<IpNet>,
    pub gateway: Option<IpAddr>,
    pub mtu: Option<u32>,
    pub nameservers: ResolverMode,
    pub search_domains: SearchMode,
}

impl Change {
    /// Whether the request changes anything at all.
    pub fn is_empty(&self) -> bool {
        self.addressing.is_none()
            && self.addresses.is_empty()
            && self.gateway.is_none()
            && self.mtu.is_none()
            && self.nameservers == ResolverMode::Unchanged
            && self.search_domains == SearchMode::Unchanged
    }
}

/// Validate a request against the physical links and the current config.
pub fn validate(
    req: &ChangeRequest,
    physical_links: &[String],
    current_subnets: &[String],
) -> Result<Change, String> {
    let link = req.link.trim();
    if link.is_empty() {
        return Err("link is required".to_string());
    }
    if !physical_links.iter().any(|name| name == link) {
        return Err(format!("{link:?} is not an editable physical link"));
    }

    let addressing = match req.addressing.as_deref().map(str::trim) {
        None | Some("") => None,
        Some("dhcp") => Some(Addressing::Dhcp),
        Some("static") => Some(Addressing::Static),
        Some(other) => {
            return Err(format!(
                "addressing must be \"dhcp\" or \"static\", got {other:?}"
            ))
        }
    };

    let mut addresses = Vec::new();
    if let Some(raw) = &req.addresses {
        if addressing != Some(Addressing::Static) {
            return Err("addresses may only be set when addressing is \"static\"".to_string());
        }
        if raw.is_empty() || raw.len() > 4 {
            return Err("a static link needs 1-4 addresses".to_string());
        }
        let mut seen = BTreeSet::new();
        for entry in raw {
            let net = parse_static_address(entry)?;
            if !seen.insert(net.to_string()) {
                return Err(format!("duplicate address {net}"));
            }
            addresses.push(net);
        }
    } else if addressing == Some(Addressing::Static) {
        return Err("a static link needs at least one address".to_string());
    }

    let gateway = match req.gateway.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => {
            if addressing == Some(Addressing::Dhcp) {
                return Err("a DHCP link may not set a static gateway".to_string());
            }
            let ip: IpAddr = raw
                .parse()
                .map_err(|_| format!("gateway {raw:?} is not an IP address"))?;
            if !is_unicast(&ip) {
                return Err(format!("gateway {ip} is not a unicast address"));
            }
            if addresses.is_empty() {
                return Err("a gateway requires static addresses".to_string());
            }
            if !addresses.iter().any(|net| net.contains(&ip)) {
                return Err(format!("gateway {ip} is not inside any configured subnet"));
            }
            if addresses.iter().any(|net| net.addr() == ip) {
                return Err(format!("gateway {ip} is also a configured address"));
            }
            Some(ip)
        }
    };

    let mtu = req.mtu;
    if let Some(value) = mtu {
        if !(576..=9216).contains(&value) {
            return Err(format!("mtu {value} is outside 576-9216"));
        }
        if value < 1280 && !req.confirm_low_mtu {
            return Err("an MTU below 1280 requires confirmLowMtu".to_string());
        }
    }

    let nameservers = match &req.nameservers {
        None => ResolverMode::Unchanged,
        Some(values) => match values.mode.trim() {
            "inherit" => ResolverMode::Inherit,
            "explicit" => {
                if values.servers.is_empty() || values.servers.len() > 3 {
                    return Err("1-3 DNS nameservers are required".to_string());
                }
                let mut servers = Vec::new();
                for raw in &values.servers {
                    let ip: IpAddr = raw
                        .parse()
                        .map_err(|_| format!("nameserver {raw:?} is not an IP address"))?;
                    if !is_unicast(&ip) {
                        return Err(format!("nameserver {ip} is not a unicast address"));
                    }
                    if servers.contains(&ip) {
                        return Err(format!("duplicate nameserver {ip}"));
                    }
                    servers.push(ip);
                }
                ResolverMode::Explicit(servers)
            }
            other => {
                return Err(format!(
                    "nameservers.mode must be \"inherit\" or \"explicit\", got {other:?}"
                ))
            }
        },
    };

    let search_domains = match &req.search_domains {
        None => SearchMode::Unchanged,
        Some(values) => match values.mode.trim() {
            "inherit" => SearchMode::Inherit,
            "explicit" => {
                if values.domains.is_empty() || values.domains.len() > 6 {
                    return Err("1-6 search domains are required".to_string());
                }
                for domain in &values.domains {
                    if !crate::certs::is_dns1123_subdomain(&domain.to_ascii_lowercase()) {
                        return Err(format!("search domain {domain:?} is not a valid DNS name"));
                    }
                }
                SearchMode::Explicit(values.domains.clone())
            }
            other => {
                return Err(format!(
                    "searchDomains.mode must be \"inherit\" or \"explicit\", got {other:?}"
                ))
            }
        },
    };

    // A static address outside the kubelet's valid subnets is fine: the patch
    // builder unions the new subnet into KubeNodeConfig. The parameter stays in
    // the signature so the rule is explicit and testable.
    let _ = current_subnets;

    let change = Change {
        link: link.to_string(),
        addressing,
        addresses,
        gateway,
        mtu,
        nameservers,
        search_domains,
    };
    if change.is_empty() {
        return Err("no change requested".to_string());
    }
    Ok(change)
}

/// Parse and vet one static address in CIDR form.
pub fn parse_static_address(raw: &str) -> Result<IpNet, String> {
    let net: IpNet = raw
        .parse()
        .map_err(|_| format!("address {raw:?} is not in CIDR form"))?;
    let max = if net.addr().is_ipv4() { 32 } else { 128 };
    if net.prefix_len() == 0 || net.prefix_len() > max {
        return Err(format!(
            "address {raw:?} has an invalid prefix length (1-{max})"
        ));
    }
    let addr = net.addr();
    if addr.is_unspecified() {
        return Err(format!("address {raw:?} is unspecified"));
    }
    if addr.is_loopback() {
        return Err(format!("address {raw:?} is loopback"));
    }
    if addr.is_multicast() {
        return Err(format!("address {raw:?} is multicast"));
    }
    if is_link_local(&addr) {
        return Err(format!("address {raw:?} is link-local"));
    }
    Ok(net)
}

fn is_unicast(ip: &IpAddr) -> bool {
    !ip.is_unspecified() && !ip.is_loopback() && !ip.is_multicast() && !is_link_local(ip)
}

fn is_link_local(addr: &IpAddr) -> bool {
    match addr {
        IpAddr::V4(v4) => v4.is_link_local(),
        IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) == 0xfe80,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn links() -> Vec<String> {
        vec!["eth0".to_string(), "eth1".to_string()]
    }

    fn req() -> ChangeRequest {
        ChangeRequest {
            link: "eth0".into(),
            ..Default::default()
        }
    }

    #[test]
    fn static_address_and_gateway_are_accepted() {
        let mut r = req();
        r.addressing = Some("static".into());
        r.addresses = Some(vec!["10.0.0.10/24".into()]);
        r.gateway = Some("10.0.0.1".into());
        let change = validate(&r, &links(), &[]).unwrap();
        assert_eq!(change.addressing, Some(Addressing::Static));
        assert_eq!(change.addresses.len(), 1);
    }

    #[test]
    fn link_must_be_a_physical_link() {
        let mut r = req();
        r.link = "lo".into();
        r.mtu = Some(1500);
        assert!(validate(&r, &links(), &[]).is_err());
    }

    #[test]
    fn static_requires_an_address_and_rejects_bad_cidrs() {
        let mut r = req();
        r.addressing = Some("static".into());
        assert!(validate(&r, &links(), &[]).is_err());
        for bad in [
            "127.0.0.1/8",
            "169.254.1.1/16",
            "224.0.0.1/24",
            "0.0.0.0/8",
            "10.0.0.1/0",
            "10.0.0.1/33",
            "10.0.0.300/24",
        ] {
            let mut r = req();
            r.addressing = Some("static".into());
            r.addresses = Some(vec![bad.into()]);
            assert!(
                validate(&r, &links(), &[]).is_err(),
                "{bad} must be rejected"
            );
        }
    }

    #[test]
    fn duplicate_addresses_are_rejected() {
        let mut r = req();
        r.addressing = Some("static".into());
        r.addresses = Some(vec!["10.0.0.10/24".into(), "10.0.0.10/24".into()]);
        assert!(validate(&r, &links(), &[]).is_err());
    }

    #[test]
    fn gateway_must_be_inside_a_configured_subnet_and_not_an_address() {
        let mut r = req();
        r.addressing = Some("static".into());
        r.addresses = Some(vec!["10.0.0.10/24".into()]);
        r.gateway = Some("10.9.0.1".into());
        assert!(validate(&r, &links(), &[]).is_err());
        r.gateway = Some("10.0.0.10".into());
        assert!(validate(&r, &links(), &[]).is_err());
        r.gateway = Some("10.0.0.1".into());
        assert!(validate(&r, &links(), &[]).is_ok());
    }

    #[test]
    fn dhcp_rejects_a_static_address_or_gateway() {
        let mut r = req();
        r.addressing = Some("dhcp".into());
        r.addresses = Some(vec!["10.0.0.10/24".into()]);
        assert!(validate(&r, &links(), &[]).is_err());
        let mut r = req();
        r.addressing = Some("dhcp".into());
        r.gateway = Some("10.0.0.1".into());
        assert!(validate(&r, &links(), &[]).is_err());
    }

    #[test]
    fn mtu_range_and_low_mtu_confirmation() {
        let mut r = req();
        r.mtu = Some(575);
        assert!(validate(&r, &links(), &[]).is_err());
        r.mtu = Some(9217);
        assert!(validate(&r, &links(), &[]).is_err());
        r.mtu = Some(1200);
        assert!(validate(&r, &links(), &[]).is_err());
        r.confirm_low_mtu = true;
        assert!(validate(&r, &links(), &[]).is_ok());
    }

    #[test]
    fn dns_modes() {
        let mut r = req();
        r.nameservers = Some(ModeValues {
            mode: "explicit".into(),
            servers: vec!["1.1.1.1".into(), "8.8.8.8".into()],
            ..Default::default()
        });
        let change = validate(&r, &links(), &[]).unwrap();
        assert_eq!(
            change.nameservers,
            ResolverMode::Explicit(vec!["1.1.1.1".parse().unwrap(), "8.8.8.8".parse().unwrap()])
        );

        let mut r = req();
        r.nameservers = Some(ModeValues {
            mode: "explicit".into(),
            servers: vec![],
            ..Default::default()
        });
        assert!(validate(&r, &links(), &[]).is_err());

        let mut r = req();
        r.nameservers = Some(ModeValues {
            mode: "explicit".into(),
            servers: vec!["1.1.1.1".into(), "1.1.1.1".into()],
            ..Default::default()
        });
        assert!(validate(&r, &links(), &[]).is_err());

        let mut r = req();
        r.search_domains = Some(ModeValues {
            mode: "explicit".into(),
            domains: vec!["lan".into(), "example.internal".into()],
            ..Default::default()
        });
        assert!(validate(&r, &links(), &[]).is_ok());

        let mut r = req();
        r.search_domains = Some(ModeValues {
            mode: "explicit".into(),
            domains: vec!["bad domain".into()],
            ..Default::default()
        });
        assert!(validate(&r, &links(), &[]).is_err());
    }

    #[test]
    fn an_empty_change_is_rejected() {
        assert!(validate(&req(), &links(), &[]).is_err());
    }
}
