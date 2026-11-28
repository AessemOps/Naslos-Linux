//! Cloud endpoint parsing and connect-time address policy.
//!
//! An S3 endpoint is the one place a user can point the API at an arbitrary
//! host, so the guard runs where the connection is made, not only when the
//! destination is saved: a custom resolver vets every address the name resolves
//! to and reqwest connects only to those addresses (DNS rebinding is closed).
//! Redirects are disabled and no proxy is used, so a vetted host cannot bounce
//! the request somewhere else.
//!
//! The policy reuses [`crate::ddns::drivers::blocked_ip`] for loopback,
//! link-local (cloud metadata), CGNAT, multicast, ULA and IPv4-mapped forms.
//! RFC1918 addresses are allowed only when the operator lists the exact
//! `cidr:port` pair (`CLOUD_PRIVATE_CIDRS`, fed by
//! `networkPolicy.cloudPrivateEndpoints`), and the pod/service CIDRs plus the
//! node IP stay blocked even if a broader allowlist would cover them.
//! Public endpoints must use HTTPS on port 443.

use std::net::{IpAddr, SocketAddr};

use ipnet::IpNet;

/// One operator-allowlisted private endpoint (`cidr:port`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivateEndpoint {
    pub net: IpNet,
    pub port: u16,
}

impl PrivateEndpoint {
    pub fn contains(&self, ip: &IpAddr) -> bool {
        self.net.contains(ip)
    }
}

/// The address policy for cloud connections.
#[derive(Debug, Clone, Default)]
pub struct NetConfig {
    /// RFC1918 destinations the operator explicitly allowed, port-scoped.
    pub private: Vec<PrivateEndpoint>,
    /// Addresses that are refused even when a private allowlist would cover
    /// them (the pod and service CIDRs).
    pub blocked: Vec<IpNet>,
    /// This node's own address (`NASLOS_NODE_IP`), always refused.
    pub node_ip: Option<IpAddr>,
}

impl NetConfig {
    pub fn from_env() -> Self {
        Self {
            private: parse_private_cidrs(&std::env::var("CLOUD_PRIVATE_CIDRS").unwrap_or_default())
                .unwrap_or_else(|e| {
                    tracing::warn!("ignoring CLOUD_PRIVATE_CIDRS: {e}");
                    Vec::new()
                }),
            blocked: parse_cidr_list(&std::env::var("CLOUD_BLOCKED_CIDRS").unwrap_or_default())
                .unwrap_or_else(|e| {
                    tracing::warn!("ignoring CLOUD_BLOCKED_CIDRS: {e}");
                    Vec::new()
                }),
            node_ip: std::env::var("NASLOS_NODE_IP")
                .ok()
                .and_then(|v| v.trim().parse().ok()),
        }
    }

    /// Whether the endpoint's port may be reached at this address.
    pub fn address_allowed(&self, ip: &IpAddr, port: u16, require_private: bool) -> bool {
        let ip = canonical(*ip);
        if self.blocked.iter().any(|net| net.contains(&ip)) {
            return false;
        }
        if self.node_ip.is_some_and(|node| canonical(node) == ip) {
            return false;
        }
        let private = is_rfc1918(&ip);
        if private {
            return self
                .private
                .iter()
                .any(|entry| entry.port == port && entry.contains(&ip));
        }
        if require_private {
            // Plaintext HTTP is only for an operator-allowlisted private
            // endpoint. A public host never gets it, even with the checkbox.
            return false;
        }
        if crate::ddns::drivers::blocked_ip(&ip) {
            return false;
        }
        port == 443
    }
}

/// IPv4-mapped IPv6 addresses are judged as IPv4.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        other => other,
    }
}

fn is_rfc1918(ip: &IpAddr) -> bool {
    matches!(ip, IpAddr::V4(v4) if v4.is_private())
}

/// Parse `cidr:port,cidr:port`. IPv4 only: the chart's policies and the
/// documented recipes are IPv4, and an IPv6 CIDR's colons would be ambiguous.
pub fn parse_private_cidrs(raw: &str) -> Result<Vec<PrivateEndpoint>, String> {
    let mut out = Vec::new();
    for entry in raw.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (cidr, port) = entry
            .rsplit_once(':')
            .ok_or_else(|| format!("private endpoint {entry:?} must be cidr:port"))?;
        let net: IpNet = cidr
            .parse()
            .map_err(|e| format!("invalid private CIDR {cidr:?}: {e}"))?;
        if net.addr().is_ipv6() {
            return Err(format!(
                "private endpoint {entry:?}: only IPv4 CIDRs are supported"
            ));
        }
        let port: u16 = port
            .parse()
            .ok()
            .filter(|p| *p > 0)
            .ok_or_else(|| format!("private endpoint {entry:?} has an invalid port"))?;
        out.push(PrivateEndpoint { net, port });
    }
    Ok(out)
}

/// Parse a comma-separated CIDR list.
pub fn parse_cidr_list(raw: &str) -> Result<Vec<IpNet>, String> {
    let mut out = Vec::new();
    for entry in raw.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let net: IpNet = entry
            .parse()
            .map_err(|e| format!("invalid CIDR {entry:?}: {e}"))?;
        out.push(net);
    }
    Ok(out)
}

/// A validated cloud endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// Scheme origin (`https://host[:port]`), no trailing slash or path.
    pub url: String,
    pub host: String,
    pub port: u16,
    pub plaintext: bool,
}

/// Parse and validate an S3 endpoint URL. Literal addresses are checked now;
/// a hostname is checked when it resolves (connect time).
pub fn parse_endpoint(raw: &str, allow_insecure_http: bool) -> Result<Endpoint, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("endpoint is required".to_string());
    }
    let url = reqwest::Url::parse(trimmed).map_err(|e| format!("invalid endpoint URL: {e}"))?;
    let scheme = url.scheme();
    if scheme != "https" && scheme != "http" {
        return Err(
            "endpoint must start with https:// (or http:// for an allowlisted private host)"
                .to_string(),
        );
    }
    let plaintext = scheme == "http";
    if plaintext && !allow_insecure_http {
        return Err(
            "refusing a plaintext http endpoint: acknowledge allowInsecureHttp for an allowlisted private host"
                .to_string(),
        );
    }
    if url.username() != "" || url.password().is_some() {
        return Err("endpoint must not embed credentials".to_string());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("endpoint must not carry a query or fragment".to_string());
    }
    let path = url.path();
    if !path.is_empty() && path != "/" {
        return Err(
            "endpoint must be an origin such as https://s3.example.com, without a path".to_string(),
        );
    }
    let host = url
        .host_str()
        .ok_or_else(|| "endpoint has no host".to_string())?
        .to_string();
    if host.is_empty() {
        return Err("endpoint has no host".to_string());
    }
    if let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
        // A literal address never reaches the resolver, so the address-only
        // rules are checked here (metadata/loopback/CGNAT/ULA and public
        // http). The private allowlist is still enforced at connect time by the
        // resolver, which keeps this parser deterministic.
        let port = url
            .port_or_known_default()
            .ok_or_else(|| "endpoint has no port".to_string())?;
        let ip = canonical(ip);
        if !is_rfc1918(&ip) {
            if crate::ddns::drivers::blocked_ip(&ip) {
                return Err(
                    "endpoint address is blocked (metadata, loopback or a non-routable range)"
                        .to_string(),
                );
            }
            if plaintext {
                return Err("a public endpoint must use https; plaintext http is only for an allowlisted private endpoint".to_string());
            }
            if port != 443 {
                return Err("a public endpoint must use https on port 443".to_string());
            }
        }
    } else {
        crate::alert_transport::normalize_host(&host)
            .map_err(|e| format!("invalid endpoint host: {e}"))?;
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| "endpoint has no port".to_string())?;
    Ok(Endpoint {
        url: url.origin().ascii_serialization(),
        host,
        port,
        plaintext,
    })
}

/// A connect-time resolver that only returns addresses the policy allows.
#[derive(Debug, Clone)]
pub struct CloudDns {
    pub port: u16,
    pub require_private: bool,
    pub config: NetConfig,
}

impl reqwest::dns::Resolve for CloudDns {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        let this = self.clone();
        Box::pin(async move {
            let addrs = tokio::net::lookup_host((host.as_str(), this.port))
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
            let mut allowed: Vec<SocketAddr> = Vec::new();
            for addr in addrs {
                if this
                    .config
                    .address_allowed(&addr.ip(), this.port, this.require_private)
                {
                    allowed.push(addr);
                }
            }
            if allowed.is_empty() {
                return Err(
                    "the endpoint resolves to a blocked address or port (private endpoints must be listed in cloudPrivateEndpoints)"
                        .into(),
                );
            }
            Ok(Box::new(allowed.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// Build the per-destination HTTP client: rustls with the compiled-in webpki
/// roots (no invalid-certificate escape hatch), no redirects, no proxy, and the
/// connect-time address policy.
pub fn http_client(endpoint: &Endpoint, config: &NetConfig) -> Result<reqwest::Client, String> {
    let dns = CloudDns {
        port: endpoint.port,
        require_private: endpoint.plaintext,
        config: config.clone(),
    };
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .https_only(!endpoint.plaintext)
        .connect_timeout(std::time::Duration::from_secs(15))
        .pool_idle_timeout(std::time::Duration::from_secs(90))
        .dns_resolver(std::sync::Arc::new(dns))
        .build()
        .map_err(|e| format!("building the endpoint HTTP client: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> NetConfig {
        NetConfig {
            private: parse_private_cidrs("192.168.1.0/24:9000,10.10.0.0/16:9443").unwrap(),
            blocked: parse_cidr_list("10.244.0.0/16,10.96.0.0/12").unwrap(),
            node_ip: Some("192.168.1.38".parse().unwrap()),
        }
    }

    fn allowed(cfg: &NetConfig, ip: &str, port: u16, require_private: bool) -> bool {
        cfg.address_allowed(&ip.parse().unwrap(), port, require_private)
    }

    #[test]
    fn public_https_on_443_is_allowed_and_other_ports_are_not() {
        let cfg = cfg();
        assert!(allowed(&cfg, "8.8.8.8", 443, false));
        assert!(!allowed(&cfg, "8.8.8.8", 8443, false));
    }

    #[test]
    fn metadata_loopback_cgnat_and_ula_are_always_refused() {
        let cfg = cfg();
        for ip in [
            "169.254.169.254",
            "::ffff:169.254.169.254",
            "127.0.0.1",
            "::1",
            "100.64.0.1",
            "fd00::1",
            "fe80::1",
        ] {
            assert!(!allowed(&cfg, ip, 9000, true), "{ip} must be blocked");
            assert!(!allowed(&cfg, ip, 443, false), "{ip} must be blocked");
        }
    }

    #[test]
    fn private_addresses_need_an_exact_cidr_and_port_match() {
        let cfg = cfg();
        assert!(allowed(&cfg, "192.168.1.50", 9000, true));
        assert!(allowed(&cfg, "192.168.1.50", 9000, false));
        assert!(!allowed(&cfg, "192.168.1.50", 9001, true));
        assert!(!allowed(&cfg, "192.168.2.50", 9000, true));
        // A private address is never implicitly public.
        assert!(!allowed(&cfg, "192.168.1.50", 443, false));
    }

    #[test]
    fn the_node_ip_and_cluster_cidrs_win_over_a_broad_allowlist() {
        let broad = NetConfig {
            private: parse_private_cidrs("10.0.0.0/8:9000").unwrap(),
            blocked: parse_cidr_list("10.244.0.0/16,10.96.0.0/12").unwrap(),
            node_ip: Some("10.1.2.3".parse().unwrap()),
        };
        assert!(allowed(&broad, "10.1.2.4", 9000, true));
        assert!(!allowed(&broad, "10.1.2.3", 9000, true));
        assert!(!allowed(&broad, "10.96.0.1", 9000, true));
        assert!(!allowed(&broad, "10.244.0.5", 9000, true));
    }

    #[test]
    fn public_hosts_can_never_use_plaintext_http() {
        let cfg = cfg();
        assert!(!allowed(&cfg, "8.8.8.8", 80, true));
        assert!(!allowed(&cfg, "8.8.8.8", 80, false));
    }

    #[test]
    fn endpoint_parsing_rejects_credentials_paths_and_bad_schemes() {
        assert!(parse_endpoint("https://s3.us-west-004.backblazeb2.com", false).is_ok());
        assert!(parse_endpoint("ftp://example.test", false).is_err());
        assert!(parse_endpoint("https://user:pw@example.test", false).is_err());
        assert!(parse_endpoint("https://example.test/prefix", false).is_err());
        assert!(parse_endpoint("https://example.test?x=1", false).is_err());
        assert!(parse_endpoint("http://example.test", false).is_err());
        assert!(parse_endpoint("http://192.168.1.50:9000", true).is_ok());
        assert!(parse_endpoint("http://8.8.8.8", true).is_err());
        assert!(parse_endpoint("https://169.254.169.254", false).is_err());
    }

    #[test]
    fn the_origin_drops_a_trailing_slash() {
        let endpoint = parse_endpoint("https://s3.example.test/", false).unwrap();
        assert_eq!(endpoint.url, "https://s3.example.test");
        assert_eq!(endpoint.port, 443);
        assert!(!endpoint.plaintext);
    }

    #[test]
    fn cidr_parsing_is_strict() {
        assert!(parse_private_cidrs("192.168.1.0/24:9000").is_ok());
        assert!(parse_private_cidrs("192.168.1.0/24").is_err());
        assert!(parse_private_cidrs("192.168.1.0/24:0").is_err());
        assert!(parse_private_cidrs("fd00::/8:9000").is_err());
        assert!(parse_cidr_list("10.0.0.0/8,10.96.0.0/12").is_ok());
        assert!(parse_cidr_list("nope").is_err());
    }
}
