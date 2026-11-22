//! Shared transport policy for the LAN-capable alert channels (SMTP, syslog).
//!
//! These channels commonly point at a relay or collector on the operator's own
//! network, so an explicitly configured RFC1918 IPv4 target is allowed. The
//! guard stays strict about everything that is not a deliberate LAN choice:
//! loopback, link-local (including cloud metadata), CGNAT, multicast,
//! unspecified, documentation, benchmark, reserved and ULA addresses are
//! refused, IPv4-mapped forms included. A hostname is resolved at save time and
//! again at send time, and the send is pinned to the vetted address.
//!
//! TLS is always validated against the compiled-in Mozilla trust anchors. There
//! is deliberately no insecure/accept-any switch, so a private relay that wants
//! STARTTLS or implicit TLS must present a publicly trusted certificate (or the
//! operator selects `none` on an isolated network).

use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::{Arc, OnceLock};

/// Whether an address is refused for an SMTP/syslog target.
pub(crate) fn blocked_target(ip: &IpAddr) -> bool {
    match ip {
        // is_private() is exactly RFC1918 (10/8, 172.16/12, 192.168/16). It
        // excludes loopback and link-local, so those stay refused below.
        IpAddr::V4(v4) if v4.is_private() => false,
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                if v4.is_private() {
                    return false;
                }
            }
            crate::ddns::drivers::blocked_ip(ip)
        }
        _ => crate::ddns::drivers::blocked_ip(ip),
    }
}

fn refuse(ip: &IpAddr) -> Result<(), String> {
    if blocked_target(ip) {
        return Err(
            "refusing to send alerts to a loopback, link-local or otherwise non-routable address"
                .to_string(),
        );
    }
    Ok(())
}

/// Trim a configured host and reject anything that could smuggle a second
/// target or credentials into the transport (URL userinfo, path, whitespace,
/// control characters, or a host:port pair in the host field).
pub(crate) fn normalize_host(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    let host = trimmed
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(trimmed)
        .trim();
    if host.is_empty() {
        return Err("alert host is empty".to_string());
    }
    if host.chars().any(|c| {
        c.is_control()
            || c.is_whitespace()
            || matches!(c, '@' | '/' | '\\' | '<' | '>' | '"' | '[' | ']')
    }) {
        return Err("alert host contains an invalid character".to_string());
    }
    if host.contains(':') && host.parse::<IpAddr>().is_err() {
        return Err("alert host must not include a port".to_string());
    }
    Ok(host.to_string())
}

/// Validate a target at settings-save time. A DNS name must resolve now, and
/// every address it resolves to must pass the guard.
pub(crate) fn validate_target(host: &str, port: u16) -> Result<(), String> {
    if port == 0 {
        return Err("alert port must be between 1 and 65535".to_string());
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return refuse(&ip);
    }
    let addrs = (host, port)
        .to_socket_addrs()
        .map_err(|_| "cannot resolve the alert host".to_string())?;
    let mut any = false;
    for addr in addrs {
        any = true;
        refuse(&addr.ip())?;
    }
    if !any {
        return Err("the alert host did not resolve".to_string());
    }
    Ok(())
}

/// Resolve a target at send time and return only vetted addresses. The caller
/// connects to one of these directly, so DNS cannot be rebound between the
/// check and the connection.
pub(crate) async fn resolve_target(host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
    if port == 0 {
        return Err("alert port must be between 1 and 65535".to_string());
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        refuse(&ip)?;
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    let addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| "cannot resolve the alert host".to_string())?;
    let mut vetted = Vec::new();
    for addr in addrs {
        refuse(&addr.ip())?;
        vetted.push(addr);
    }
    if vetted.is_empty() {
        return Err("the alert host did not resolve".to_string());
    }
    Ok(vetted)
}

fn tls_config() -> Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let _ = rustls::crypto::ring::default_provider().install_default();
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            Arc::new(
                rustls::ClientConfig::builder()
                    .with_root_certificates(roots)
                    .with_no_client_auth(),
            )
        })
        .clone()
}

/// Wrap a connected TCP stream in validated TLS for `host`.
pub(crate) async fn tls_connect(
    tcp: tokio::net::TcpStream,
    host: &str,
) -> Result<tokio_rustls::client::TlsStream<tokio::net::TcpStream>, String> {
    let name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|_| "invalid alert host name".to_string())?;
    let connector = tokio_rustls::TlsConnector::from(tls_config());
    connector.connect(name, tcp).await.map_err(|_| {
        "the TLS handshake failed (the certificate could not be validated)".to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_rfc1918_targets_are_allowed() {
        for allowed in [
            "10.0.0.5",
            "10.255.255.254",
            "172.16.4.2",
            "172.31.255.254",
            "192.168.1.10",
            "192.168.255.255",
            "1.1.1.1",
            "8.8.8.8",
            "2606:4700:4700::1111",
            "::ffff:192.168.1.10",
        ] {
            assert!(
                !blocked_target(&allowed.parse().unwrap()),
                "{allowed} must be allowed"
            );
        }
    }

    #[test]
    fn loopback_link_local_metadata_and_special_targets_are_refused() {
        for blocked in [
            "127.0.0.1",
            "127.0.0.2",
            "169.254.169.254",
            "169.254.1.1",
            "0.0.0.0",
            "100.64.0.1",
            "198.18.0.1",
            "192.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
            "::1",
            "fe80::1",
            "fd00::1",
            "fc00::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
        ] {
            assert!(
                blocked_target(&blocked.parse().unwrap()),
                "{blocked} must be refused"
            );
        }
    }

    #[test]
    fn host_normalization_refuses_embedded_targets() {
        assert_eq!(normalize_host(" relay.lan ").unwrap(), "relay.lan");
        assert_eq!(normalize_host("[::1]").unwrap(), "::1");
        assert_eq!(normalize_host("10.0.0.5").unwrap(), "10.0.0.5");
        for bad in [
            "user:pass@relay.lan",
            "user@relay.lan",
            "relay.lan:587",
            "relay.lan/path",
            "relay.lan\\path",
            "relay lan",
            "relay\nlan",
            "[]",
        ] {
            assert!(normalize_host(bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn save_time_validation_uses_the_same_guard() {
        assert!(validate_target("192.168.1.10", 25).is_ok());
        assert!(validate_target("127.0.0.1", 25).is_err());
        assert!(validate_target("169.254.169.254", 25).is_err());
        assert!(validate_target("192.168.1.10", 0).is_err());
    }
}
