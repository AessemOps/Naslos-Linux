//! Upstream image release detection for installed catalog apps.
//!
//! The official charts carry floating tags (`latest`, `stable`, `release`) with
//! `imagePullPolicy: IfNotPresent`. A new upstream image therefore neither
//! changes the chart version nor gets pulled by a plain Helm upgrade. This
//! module resolves the digest a registry currently serves for an image tag and
//! compares it with the digest the running pod pulled, so the Installed tab can
//! offer an image update.
//!
//! Only anonymous pulls are attempted: the official catalog is public. A
//! registry that demands credentials returns an error, which callers treat as
//! "no update" rather than a false positive. Pinned references
//! (`repo@sha256:…`) are deliberately skipped — they only change through a
//! chart release.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

/// Manifest media types a registry may return for a tag.
const MANIFEST_ACCEPT: &str = "application/vnd.oci.image.index.v1+json, \
application/vnd.docker.distribution.manifest.list.v2+json, \
application/vnd.oci.image.manifest.v1+json, \
application/vnd.docker.distribution.manifest.v2+json";
const DOCKER_HUB: &str = "docker.io";
const DOCKER_HUB_REGISTRY: &str = "registry-1.docker.io";
const TARGET_OS: &str = "linux";
const TARGET_ARCH: &str = "amd64";
/// A failed lookup is cached briefly so a registry outage cannot make every
/// catalog cycle hammer it.
const ERROR_TTL: Duration = Duration::from_secs(60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Manifests are tiny; refuse an oversized body rather than buffering it.
const MAX_MANIFEST_BYTES: usize = 2 * 1024 * 1024;

/// A parsed image reference. `reference` is a tag or a digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRef {
    /// Registry host without scheme (`docker.io`, `ghcr.io`, `lscr.io:443`).
    pub registry: String,
    /// Repository path (`linuxserver/radarr`, `library/alpine`).
    pub repository: String,
    /// Tag or digest.
    pub reference: String,
    /// The reference carries an explicit digest (`repo@sha256:…`).
    pub pinned: bool,
}

impl ImageRef {
    fn cache_key(&self) -> String {
        format!("{}/{}:{}", self.registry, self.repository, self.reference)
    }

    /// The host used for Registry v2 requests.
    fn registry_host(&self) -> &str {
        match self.registry.as_str() {
            DOCKER_HUB | "index.docker.io" => DOCKER_HUB_REGISTRY,
            other => other,
        }
    }
}

/// Parse an image reference as it appears in a pod spec or container status.
///
/// Returns `None` for an empty or malformed reference, which keeps the caller
/// from making a request for something that is not a registry image.
pub fn parse_image_ref(input: &str) -> Option<ImageRef> {
    let input = input.trim();
    let input = input
        .strip_prefix("docker-pullable://")
        .or_else(|| input.strip_prefix("docker://"))
        .unwrap_or(input);
    // A pod status may carry `repo:tag@sha256:…`; keep the tag for display but
    // treat the reference as pinned.
    let (name, digest) = match input.split_once('@') {
        Some((name, digest)) => (name, Some(digest)),
        None => (input, None),
    };
    if name.is_empty() {
        return None;
    }

    let (raw_registry, rest) = match name.split_once('/') {
        Some((first, rest))
            if first.contains('.') || first.contains(':') || first == "localhost" =>
        {
            (first.to_string(), rest)
        }
        _ => (DOCKER_HUB.to_string(), name),
    };
    // Docker Hub is addressable through several host names; normalize them so
    // the official-image `library/` prefix below is applied consistently.
    let registry = match raw_registry.as_str() {
        DOCKER_HUB | "index.docker.io" | "registry-1.docker.io" => DOCKER_HUB.to_string(),
        _ => raw_registry,
    };
    if !valid_registry(&registry) {
        return None;
    }

    let (repository, tag) = match rest.rsplit_once(':') {
        Some((repository, tag)) if !tag.contains('/') => (repository, Some(tag)),
        _ => (rest, None),
    };
    let mut repository = repository.to_string();
    if registry == DOCKER_HUB && !repository.contains('/') {
        // Docker Hub's canonical name for an official image.
        repository = format!("library/{repository}");
    }
    if !valid_repository(&repository) {
        return None;
    }

    let (reference, pinned) = match digest {
        Some(digest) => {
            if !valid_digest(digest) {
                return None;
            }
            (digest.to_string(), true)
        }
        None => {
            let tag = tag.unwrap_or("latest");
            if !valid_tag(tag) {
                return None;
            }
            (tag.to_string(), false)
        }
    };

    Some(ImageRef {
        registry,
        repository,
        reference,
        pinned,
    })
}

fn valid_registry(registry: &str) -> bool {
    if registry.is_empty() || registry.len() > 253 {
        return false;
    }
    let (host, port) = match registry.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() => (host, Some(port)),
        _ => (registry, None),
    };
    if host.is_empty()
        || host.starts_with('.')
        || host.ends_with('.')
        || !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
    {
        return false;
    }
    match port {
        Some(port) => port.chars().all(|c| c.is_ascii_digit()),
        None => true,
    }
}

fn valid_repository(repository: &str) -> bool {
    !repository.is_empty()
        && repository.len() <= 255
        && repository.split('/').all(|part| {
            !part.is_empty()
                && !part.starts_with('.')
                && !part.ends_with('.')
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        })
}

fn valid_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 128
        && !tag.starts_with('.')
        && !tag.starts_with('-')
        && tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn valid_digest(digest: &str) -> bool {
    match digest.split_once(':') {
        Some((algorithm, hex)) => {
            !algorithm.is_empty()
                && algorithm.chars().all(|c| c.is_ascii_alphanumeric())
                && hex.len() >= 32
                && hex.chars().all(|c| c.is_ascii_hexdigit())
        }
        None => false,
    }
}

/// The digest from a pod's `containerStatuses[].imageID`
/// (`docker-pullable://repo@sha256:…`).
pub fn running_digest(image_id: &str) -> Option<String> {
    let (_, digest) = image_id.rsplit_once('@')?;
    let digest = digest.trim();
    if valid_digest(digest) {
        Some(digest.to_string())
    } else {
        None
    }
}

/// A tag resolved to the digests a runtime may have recorded. For a
/// multi-arch tag `digest` is the index digest and `platform` is the
/// `linux/amd64` child: containerd reports one or the other in `imageID`
/// depending on the runtime/version, so both count as current.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedDigest {
    pub digest: String,
    pub platform: Option<String>,
}

impl ResolvedDigest {
    fn matches(&self, running: &str) -> bool {
        self.digest == running || self.platform.as_deref() == Some(running)
    }
}

/// An app container whose registry digest moved away from the running one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageDrift {
    /// The image reference as it appears in the pod spec.
    pub reference: String,
    /// Digest the running pod pulled (`sha256:…`).
    pub current: String,
    /// Top-level digest the registry currently serves for the tag.
    pub latest: String,
}

/// A short digest for display: the first 12 hex characters.
pub fn short_digest(digest: &str) -> String {
    let hex = digest.split_once(':').map(|(_, hex)| hex).unwrap_or(digest);
    hex.chars().take(12).collect()
}

/// Resolves tags to digests, caching responses for `ttl`.
pub struct ImageDigestCache {
    ttl: Duration,
    client: reqwest::Client,
    entries: Mutex<HashMap<String, CacheEntry>>,
}

struct CacheEntry {
    expires: Instant,
    result: Result<ResolvedDigest, String>,
}

impl ImageDigestCache {
    pub fn new(ttl: Duration) -> Self {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            // A registry must not redirect the digest lookup somewhere else.
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("naslos-api/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default();
        Self {
            ttl,
            client,
            entries: Mutex::new(HashMap::new()),
        }
    }

    pub fn from_env() -> Self {
        let ttl = std::env::var("IMAGE_CHECK_TTL")
            .ok()
            .and_then(|value| crate::state::parse_go_duration(&value))
            .unwrap_or_else(|| Duration::from_secs(60 * 60));
        Self::new(ttl)
    }

    /// Drop every cached lookup so the next check asks the registry again.
    /// The manual "Check now" uses this; the background cycle keeps the cache.
    pub fn clear(&self) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.clear();
        }
    }

    /// Compare the running digest of `image` with the digest the registry
    /// serves now. `None` means "no update or cannot tell".
    pub async fn check(&self, image: &str, running: &str) -> Option<ImageDrift> {
        let reference = parse_image_ref(image)?;
        if reference.pinned {
            return None;
        }
        let latest = match self.resolve(&reference).await {
            Ok(digest) => digest,
            Err(e) => {
                tracing::debug!(
                    image = %reference.cache_key(),
                    "image digest lookup failed: {e}"
                );
                return None;
            }
        };
        if latest.matches(running) {
            return None;
        }
        Some(ImageDrift {
            reference: image.trim().to_string(),
            current: running.to_string(),
            latest: latest.digest,
        })
    }

    /// Resolve a reference's current digest(s), using the cache.
    pub async fn resolve(&self, reference: &ImageRef) -> Result<ResolvedDigest, String> {
        let key = reference.cache_key();
        let now = Instant::now();
        if let Ok(entries) = self.entries.lock() {
            if let Some(entry) = entries.get(&key) {
                if entry.expires > now {
                    return entry.result.clone();
                }
            }
        }
        let result = resolve_digest(&self.client, reference).await;
        let ttl = if result.is_ok() { self.ttl } else { ERROR_TTL };
        if let Ok(mut entries) = self.entries.lock() {
            entries.insert(
                key,
                CacheEntry {
                    expires: Instant::now() + ttl,
                    result: result.clone(),
                },
            );
        }
        result
    }
}

/// Resolve the registry digests for one reference. For a multi-arch tag the
/// top-level index digest and the `linux/amd64` child are both returned so the
/// caller can match whichever digest the runtime recorded.
pub async fn resolve_digest(
    client: &reqwest::Client,
    reference: &ImageRef,
) -> Result<ResolvedDigest, String> {
    let url = format!(
        "https://{}/v2/{}/manifests/{}",
        reference.registry_host(),
        reference.repository,
        reference.reference
    );
    let mut response = client
        .get(&url)
        .header(reqwest::header::ACCEPT, MANIFEST_ACCEPT)
        .send()
        .await
        .map_err(|e| format!("registry request failed: {e}"))?;

    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        let challenge = response
            .headers()
            .get(reqwest::header::WWW_AUTHENTICATE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let token = fetch_token(client, challenge.as_deref()).await?;
        response = client
            .get(&url)
            .header(reqwest::header::ACCEPT, MANIFEST_ACCEPT)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| format!("registry request failed: {e}"))?;
    }

    if !response.status().is_success() {
        return Err(format!(
            "registry returned HTTP {} for {}",
            response.status(),
            reference.cache_key()
        ));
    }

    let headers = response.headers().clone();
    if response
        .content_length()
        .is_some_and(|len| len > MAX_MANIFEST_BYTES as u64)
    {
        return Err(format!(
            "registry manifest for {} is too large",
            reference.cache_key()
        ));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("reading the registry manifest: {e}"))?
    {
        if body.len() + chunk.len() > MAX_MANIFEST_BYTES {
            return Err(format!(
                "registry manifest for {} is too large",
                reference.cache_key()
            ));
        }
        body.extend_from_slice(&chunk);
    }

    let digest = headers
        .get("docker-content-digest")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| valid_digest(value))
        .map(str::to_string)
        .unwrap_or_else(|| format!("sha256:{:x}", Sha256::digest(&body)));

    let is_index = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|value| value.get("manifests").map(|manifests| manifests.is_array()))
        .unwrap_or(false);
    let platform = is_index.then(|| platform_manifest_digest(&body)).flatten();
    Ok(ResolvedDigest { digest, platform })
}

fn platform_manifest_digest(body: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let manifests = value.get("manifests")?.as_array()?;
    let matching = |os: &str, arch: &str| {
        manifests.iter().find_map(|manifest| {
            let platform = manifest.get("platform")?;
            if platform.get("os")?.as_str()? == os
                && platform.get("architecture")?.as_str()? == arch
            {
                Some(manifest.get("digest")?.as_str()?.to_string())
            } else {
                None
            }
        })
    };
    matching(TARGET_OS, TARGET_ARCH).filter(|digest| valid_digest(digest))
}

/// Fetch an anonymous bearer token for a `WWW-Authenticate: Bearer …` challenge.
async fn fetch_token(client: &reqwest::Client, challenge: Option<&str>) -> Result<String, String> {
    let challenge = challenge.ok_or("registry did not explain its authentication challenge")?;
    if !challenge
        .get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("bearer"))
    {
        return Err(format!("unsupported registry authentication: {challenge}"));
    }
    let realm = challenge_param(challenge, "realm")
        .ok_or("registry authentication challenge has no realm")?;
    let realm = validate_realm(&realm)?;
    let mut query: Vec<(&str, String)> = Vec::new();
    if let Some(service) = challenge_param(challenge, "service") {
        query.push(("service", service));
    }
    if let Some(scope) = challenge_param(challenge, "scope") {
        query.push(("scope", scope));
    }
    let response = client
        .get(realm)
        .query(&query)
        .send()
        .await
        .map_err(|e| format!("registry token request failed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "registry token endpoint returned HTTP {}",
            response.status()
        ));
    }
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("parsing the registry token: {e}"))?;
    body.get("token")
        .or_else(|| body.get("access_token"))
        .and_then(|token| token.as_str())
        .map(str::to_string)
        .ok_or_else(|| "registry token response carried no token".to_string())
}

/// Validate a registry auth realm before the API fetches it. The realm is
/// attacker-controlled by whichever registry the chart names, so only a public
/// HTTPS host is accepted: no IP literals, localhost, single-label or
/// `.local`/`.internal` names, and no embedded credentials. Redirects are
/// disabled on the client as well.
fn validate_realm(realm: &str) -> Result<reqwest::Url, String> {
    let url =
        reqwest::Url::parse(realm).map_err(|e| format!("invalid registry auth realm: {e}"))?;
    if url.scheme() != "https" {
        return Err(format!("registry auth realm is not https: {realm}"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("registry auth realm must not carry credentials".to_string());
    }
    let host = url
        .host_str()
        .ok_or_else(|| format!("registry auth realm has no host: {realm}"))?;
    let host = host.to_ascii_lowercase();
    let bare_host = host.trim_start_matches('[').trim_end_matches(']');
    if bare_host.parse::<std::net::IpAddr>().is_ok()
        || host == "localhost"
        || !host.contains('.')
        || host.ends_with(".local")
        || host.ends_with(".internal")
        || host.ends_with(".localhost")
        || host.ends_with(".svc")
    {
        return Err(format!("registry auth realm host is not public: {host}"));
    }
    Ok(url)
}

/// Read one quoted `key="value"` parameter from an auth challenge.
fn challenge_param(challenge: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=\"");
    let start = challenge.find(&needle)? + needle.len();
    let rest = &challenge[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_registry_references() {
        let docker = parse_image_ref("lscr.io/linuxserver/radarr:latest").unwrap();
        assert_eq!(docker.registry, "lscr.io");
        assert_eq!(docker.repository, "linuxserver/radarr");
        assert_eq!(docker.reference, "latest");
        assert!(!docker.pinned);

        let hub = parse_image_ref("searxng/searxng").unwrap();
        assert_eq!(hub.registry, "docker.io");
        assert_eq!(hub.repository, "searxng/searxng");
        assert_eq!(hub.reference, "latest");

        let official = parse_image_ref("docker.io/alpine:3.20").unwrap();
        assert_eq!(official.repository, "library/alpine");
        assert_eq!(official.reference, "3.20");

        let ghcr = parse_image_ref("ghcr.io/advplyr/audiobookshelf:latest").unwrap();
        assert_eq!(ghcr.registry, "ghcr.io");
        assert_eq!(ghcr.registry_host(), "ghcr.io");

        let hub_host = parse_image_ref("index.docker.io/library/redis:7").unwrap();
        assert_eq!(hub_host.registry, DOCKER_HUB);
        assert_eq!(hub_host.registry_host(), DOCKER_HUB_REGISTRY);
        let alias_official = parse_image_ref("index.docker.io/alpine:3.20").unwrap();
        assert_eq!(alias_official.repository, "library/alpine");

        let pinned = parse_image_ref(
            "lscr.io/linuxserver/radarr:latest@sha256:ff1b1f21a200e19f52017bd1544031143715b1ab262b823a9dcb916a71da0956",
        )
        .unwrap();
        assert!(pinned.pinned);
        assert_eq!(
            pinned.reference,
            "sha256:ff1b1f21a200e19f52017bd1544031143715b1ab262b823a9dcb916a71da0956"
        );
    }

    #[test]
    fn rejects_malformed_references() {
        assert!(parse_image_ref("").is_none());
        assert!(parse_image_ref("has space:latest").is_none());
        assert!(parse_image_ref("repo:").is_none());
        assert!(parse_image_ref("repo@sha256:abc").is_none());
        assert!(parse_image_ref("evil.example/../repo:tag").is_none());
        assert!(parse_image_ref("registry.example:bad/repo:tag").is_none());
    }

    #[test]
    fn reads_running_digest_from_status_image_id() {
        let id = "docker-pullable://lscr.io/linuxserver/radarr@sha256:7add5ffe7933e6b46aa3a26bed8a0c342c9a45ec331d9a8cf145e46b04b24f36";
        assert_eq!(
            running_digest(id).unwrap(),
            "sha256:7add5ffe7933e6b46aa3a26bed8a0c342c9a45ec331d9a8cf145e46b04b24f36"
        );
        let bare = "lscr.io/linuxserver/radarr@sha256:7add5ffe7933e6b46aa3a26bed8a0c342c9a45ec331d9a8cf145e46b04b24f36";
        assert!(running_digest(bare).is_some());
        assert!(running_digest("docker-pullable://repo@sha256:short").is_none());
        assert!(running_digest("").is_none());
    }

    #[test]
    fn selects_the_linux_amd64_manifest_from_an_index() {
        let body = br#"{"manifests":[
            {"digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","platform":{"os":"linux","architecture":"arm64"}},
            {"digest":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","platform":{"os":"linux","architecture":"amd64"}},
            {"digest":"sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","platform":{"os":"unknown","architecture":"unknown"}}
        ]}"#;
        assert_eq!(
            platform_manifest_digest(body).as_deref(),
            Some("sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        );

        let single = br#"{"manifests":[
            {"digest":"sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd","platform":{"os":"linux","architecture":"riscv64"}}
        ]}"#;
        assert!(platform_manifest_digest(single).is_none());

        assert!(platform_manifest_digest(b"{\"layers\":[]}").is_none());
    }

    #[test]
    fn parses_a_bearer_challenge() {
        let challenge = "Bearer realm=\"https://auth.docker.io/token\",service=\"registry.docker.io\",scope=\"repository:library/alpine:pull\"";
        assert_eq!(
            challenge_param(challenge, "realm").as_deref(),
            Some("https://auth.docker.io/token")
        );
        assert_eq!(
            challenge_param(challenge, "scope").as_deref(),
            Some("repository:library/alpine:pull")
        );
        assert!(challenge_param(challenge, "missing").is_none());
    }

    #[test]
    fn resolved_digests_match_the_index_or_the_platform_manifest() {
        let index = ResolvedDigest {
            digest: "sha256:index".to_string(),
            platform: Some("sha256:amd64".to_string()),
        };
        assert!(index.matches("sha256:index"));
        assert!(index.matches("sha256:amd64"));
        assert!(!index.matches("sha256:old"));
        let single = ResolvedDigest {
            digest: "sha256:single".to_string(),
            platform: None,
        };
        assert!(single.matches("sha256:single"));
        assert!(!single.matches("sha256:amd64"));
    }

    #[test]
    fn rejects_private_auth_realms() {
        assert!(validate_realm("https://auth.docker.io/token").is_ok());
        assert!(validate_realm("https://ghcr.io/token").is_ok());
        assert!(validate_realm("http://auth.docker.io/token").is_err());
        assert!(validate_realm("https://127.0.0.1/token").is_err());
        assert!(validate_realm("https://[::1]/token").is_err());
        assert!(validate_realm("https://localhost/token").is_err());
        assert!(validate_realm("https://registry/token").is_err());
        assert!(validate_realm("https://kubernetes.default.svc/token").is_err());
        assert!(validate_realm("https://user:pass@auth.docker.io/token").is_err());
    }

    #[test]
    fn short_digest_is_twelve_hex_characters() {
        assert_eq!(
            short_digest("sha256:ff1b1f21a200e19f52017bd1544031143715b1ab262b823a9dcb916a71da0956"),
            "ff1b1f21a200"
        );
    }
}
