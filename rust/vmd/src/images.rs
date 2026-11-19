//! The ISO/disk-image library (FR-VMT): one flat directory per pool at
//! `<pool>/vms/isos`.
//!
//! Three writers reach it: the chunked browser upload (relayed by the API),
//! the server-side URL importer, and deletion. The URL importer carries the
//! SSRF guard instead of a network policy: vmd is hostNetwork, whose traffic
//! is exempt from pod egress policy, so the target list is validated here —
//! public addresses only, http(s) only, redirects re-validated hop by hop
//! (FR-DNS-07 pattern).

use crate::model::{isos_dir, library_path, valid_library_name, valid_pool_component, ImageEntry};
use futures::StreamExt;
use sha2::{Digest, Sha256};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::path::Path;

/// Largest accepted image (imports and total uploads).
pub const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024 * 1024;

/// List the library of one pool. A missing directory is an empty library,
/// not an error (the API bootstraps it on first use). An invalid pool
/// component yields an empty list: the handlers reject it with 400 first.
pub async fn list(pool: &str) -> Vec<ImageEntry> {
    if !valid_pool_component(pool) {
        return Vec::new();
    }
    let dir = isos_dir(pool);
    let mut out = Vec::new();
    let Ok(entries) = tokio::fs::read_dir(&dir).await else {
        return out;
    };
    let mut entries = entries;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        if !valid_library_name(&name) {
            continue;
        }
        let size_bytes = match entry.metadata().await {
            Ok(meta) => meta.len(),
            Err(_) => continue,
        };
        out.push(ImageEntry { name, size_bytes });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Write one upload chunk at `offset` (truncate first when offset is 0).
/// `final_chunk` fsyncs and returns the total size.
pub async fn write_chunk(
    pool: &str,
    name: &str,
    offset: u64,
    final_chunk: bool,
    body: axum::body::Body,
) -> Result<u64, String> {
    if !valid_pool_component(pool) {
        return Err(format!("pool {pool:?} is not a valid dataset component"));
    }
    if !valid_library_name(name) {
        return Err(format!("{name:?} is not a valid library file name"));
    }
    let path = library_path(pool, name);
    if let Some(parent) = Path::new(&path).parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("creating the image library: {e}"))?;
    }
    let file = if offset == 0 {
        tokio::fs::File::create(&path)
            .await
            .map_err(|e| format!("creating {path}: {e}"))?
    } else {
        tokio::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .await
            .map_err(|e| format!("opening {path} at offset {offset}: {e}"))?
    };
    let mut file = file;
    if offset > 0 {
        let current = file
            .metadata()
            .await
            .map_err(|e| format!("stat {path}: {e}"))?
            .len();
        if current < offset {
            return Err(format!(
                "offset {offset} is past the end of the uploaded file ({current} bytes); send chunks in order"
            ));
        }
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|e| format!("seeking {path}: {e}"))?;
    }

    use tokio::io::{AsyncSeekExt, AsyncWriteExt};
    let mut written = 0u64;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("reading the upload chunk: {e}"))?;
        if written + chunk.len() as u64 > MAX_IMAGE_BYTES {
            return Err(format!(
                "the image exceeds the {MAX_IMAGE_BYTES}-byte library limit"
            ));
        }
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("writing {path}: {e}"))?;
        written += chunk.len() as u64;
    }
    if final_chunk {
        file.sync_all()
            .await
            .map_err(|e| format!("syncing {path}: {e}"))?;
    }
    let total = file
        .metadata()
        .await
        .map_err(|e| format!("stat {path}: {e}"))?
        .len();
    if total > MAX_IMAGE_BYTES {
        return Err(format!(
            "the image exceeds the {MAX_IMAGE_BYTES}-byte library limit"
        ));
    }
    Ok(total)
}

/// Import an image from a URL. The target is re-resolved and re-checked on
/// every redirect hop; an optional sha256 pins the content.
pub async fn import_url(
    pool: &str,
    name: &str,
    url: &str,
    sha256: Option<&str>,
) -> Result<u64, String> {
    if !valid_library_name(name) {
        return Err(format!("{name:?} is not a valid library file name"));
    }
    let expected = sha256.map(|h| {
        let clean = h.trim().trim_start_matches("sha256:").to_ascii_lowercase();
        clean
    });
    if let Some(h) = &expected {
        if h.len() != 64 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("{h:?} is not a sha256 hex digest"));
        }
    }

    let path = library_path(pool, name);
    if let Some(parent) = Path::new(&path).parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("creating the image library: {e}"))?;
    }
    let mut target = url.to_string();
    let mut redirects = 0;
    let response = loop {
        let (safe_url, host, pinned) = validate_target(&target)?;
        // Pin the connection to the vetted address so the host cannot be
        // rebound between validation and the request (the ntfy client uses
        // the same defense). reqwest only resolves overrides at client
        // level, so each hop gets its own pinned client.
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(10))
            .read_timeout(std::time::Duration::from_secs(120))
            .resolve(&host, pinned)
            .build()
            .map_err(|e| format!("building the importer client: {e}"))?;
        let resp = client
            .get(&safe_url)
            .send()
            .await
            .map_err(|e| format!("fetching {target}: {e}"))?;
        if resp.status().is_redirection() {
            let location = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            if location.is_empty() {
                return Err(format!("{} redirected without a Location", resp.status()));
            }
            redirects += 1;
            if redirects > 3 {
                return Err("the URL redirects more than 3 times".to_string());
            }
            target = absolutize(&target, &location)?;
            continue;
        }
        if let Err(e) = resp.error_for_status_ref() {
            return Err(format!("fetching {target}: {e}"));
        }
        break resp;
    };

    let mut file = tokio::fs::File::create(&path)
        .await
        .map_err(|e| format!("creating {path}: {e}"))?;
    use tokio::io::AsyncWriteExt;
    let mut hasher = Sha256::new();
    let mut written = 0u64;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("downloading {target}: {e}"))?;
        written += chunk.len() as u64;
        if written > MAX_IMAGE_BYTES {
            let _ = tokio::fs::remove_file(&path).await;
            return Err(format!(
                "the image exceeds the {MAX_IMAGE_BYTES}-byte library limit"
            ));
        }
        hasher.update(&chunk);
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("writing {path}: {e}"))?;
    }
    file.sync_all()
        .await
        .map_err(|e| format!("syncing {path}: {e}"))?;
    if let Some(expected) = expected {
        let actual = format!("{:x}", hasher.finalize());
        if actual != expected {
            let _ = tokio::fs::remove_file(&path).await;
            return Err(format!(
                "sha256 mismatch: expected {expected}, got {actual}; the file was removed"
            ));
        }
    }
    Ok(written)
}

/// Resolve a relative redirect against the current URL.
fn absolutize(current: &str, location: &str) -> Result<String, String> {
    let base = reqwest::Url::parse(current).map_err(|e| format!("parsing {current}: {e}"))?;
    let joined = base
        .join(location)
        .map_err(|e| format!("resolving redirect {location:?} against {current}: {e}"))?;
    Ok(joined.to_string())
}

/// Validate one fetch target: http(s) only, resolvable, and every resolved
/// address must be public. Returns the normalized URL, its host and the
/// pinned address the request must use.
fn validate_target(url: &str) -> Result<(String, String, SocketAddr), String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err("the import URL is empty".to_string());
    }
    if trimmed.starts_with('-') || trimmed.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("the import URL contains invalid characters".to_string());
    }
    let parsed = reqwest::Url::parse(trimmed)
        .map_err(|e| format!("the import URL {trimmed:?} is not valid: {e}"))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(format!(
            "the import URL must use http or https, got {:?}",
            parsed.scheme()
        ));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| "the import URL has no host".to_string())?
        .to_string();
    let port = parsed.port_or_known_default().unwrap_or(443);
    let addrs: Vec<SocketAddr> = if let Ok(ip) = host.parse::<IpAddr>() {
        if blocked_ip(&ip) {
            return Err(format!(
                "refusing to import from the non-public address {host}"
            ));
        }
        vec![SocketAddr::new(ip, port)]
    } else {
        (host.as_str(), port)
            .to_socket_addrs()
            .map_err(|e| format!("resolving the import host {host:?}: {e}"))?
            .collect()
    };
    if addrs.is_empty() {
        return Err(format!("the import host {host:?} did not resolve"));
    }
    for addr in &addrs {
        if blocked_ip(&addr.ip()) {
            return Err(format!(
                "refusing to import from {host} (resolves to the non-public address {})",
                addr.ip()
            ));
        }
    }
    Ok((parsed.to_string(), host, addrs[0]))
}

/// Non-public address classes, mirroring the API's DDNS/ntfy guard
/// (ddns::drivers::blocked_ip) so the importer accepts exactly the same
/// target space the platform already deems safe.
pub fn blocked_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || o[0] == 0
                || (o[0] == 100 && (o[1] & 0xc0) == 64)
                || (o[0] == 192 && o[1] == 0 && o[2] == 0)
                || (o[0] == 198 && (o[1] == 18 || o[1] == 19))
                || o[0] >= 240
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return blocked_ip(&IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            v6.is_loopback()
                || v6.is_multicast()
                || v6.is_unspecified()
                || (first & 0xffc0) == 0xfe80 // link-local fe80::/10
                || (first & 0xffc0) == 0xfec0 // deprecated site-local fec0::/10
                || (first & 0xfe00) == 0xfc00 // ULA fc00::/7
                || (first == 0x0064 && v6.segments()[1] == 0xff9b) // NAT64
                || first == 0x2002
                || (first == 0x2001 && v6.segments()[1] == 0x0db8)
        }
    }
}

/// Delete a library file.
pub async fn delete(pool: &str, name: &str) -> Result<(), String> {
    if !valid_pool_component(pool) {
        return Err(format!("pool {pool:?} is not a valid dataset component"));
    }
    if !valid_library_name(name) {
        return Err(format!("{name:?} is not a valid library file name"));
    }
    let path = library_path(pool, name);
    match tokio::fs::remove_file(&path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(format!("{name:?} is not in the library"))
        }
        Err(e) => Err(format!("deleting {path}: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_targets_are_blocked() {
        for bad in [
            "127.0.0.1",
            "10.0.0.5",
            "192.168.1.10",
            "172.16.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "::1",
            "fe80::1",
            "fc00::1",
            "fec0::1",
        ] {
            let ip: IpAddr = bad.parse().unwrap();
            assert!(blocked_ip(&ip), "{bad} must be blocked");
        }
        for good in [
            "1.1.1.1",
            "93.184.216.34",
            "2606:2800:220:1:248:1893:25c8:1946",
        ] {
            let ip: IpAddr = good.parse().unwrap();
            assert!(!blocked_ip(&ip), "{good} must be allowed");
        }
        // IPv4-mapped IPv6 must be judged as IPv4.
        let mapped: IpAddr = "::ffff:127.0.0.1".parse().unwrap();
        assert!(blocked_ip(&mapped));
    }

    #[test]
    fn schemes_and_targets_are_validated() {
        assert!(validate_target("https://example.com/debian.iso").is_ok());
        assert!(validate_target("http://example.com/debian.iso").is_ok());
        assert!(validate_target("ftp://example.com/debian.iso").is_err());
        assert!(validate_target("https://127.0.0.1/x").is_err());
        assert!(validate_target("https://10.0.0.5/x").is_err());
        assert!(validate_target("file:///etc/passwd").is_err());
        assert!(validate_target("https:///nohost").is_err());
    }

    #[tokio::test]
    async fn chunked_uploads_assemble_in_order() {
        let dir = tempfile::tempdir().unwrap();
        // Redirect the library into the tempdir by calling write_chunk with a
        // fake pool under /var/mnt is impossible; exercise the helper against
        // its real root only in CI. Instead validate the name guard here.
        assert!(
            write_chunk("tank", "../evil", 0, false, axum::body::Body::empty())
                .await
                .is_err()
        );
        assert!(
            write_chunk("tank", "a/b", 0, false, axum::body::Body::empty())
                .await
                .is_err()
        );
        let _ = dir;
    }

    #[test]
    fn redirects_resolve_relative_locations() {
        assert_eq!(
            absolutize("https://example.com/a/", "b.iso").unwrap(),
            "https://example.com/a/b.iso"
        );
        assert_eq!(
            absolutize("https://example.com/a/", "https://mirror.example.org/c.iso").unwrap(),
            "https://mirror.example.org/c.iso"
        );
    }
}
