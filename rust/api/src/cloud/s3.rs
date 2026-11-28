//! The S3-compatible transport.
//!
//! `object_store` does the SigV4 signing and S3 XML work, but all I/O goes
//! through this module's reqwest 0.12 client so the endpoint policy, TLS roots
//! and proxy/redirect rules match the rest of the API. Static credentials are
//! always passed explicitly: an empty key is refused, so the builder can never
//! fall back to the instance metadata endpoint.
//!
//! Conditional create is a capability, not an assumption: Backblaze B2 rejects
//! `If-None-Match` with 501. The `/test` probe records what the bucket supports;
//! without it a chunk write is a HEAD followed by an overwrite PUT, and an
//! existing object is accepted only after a byte-for-byte GET comparison (an
//! ETag is not a portable digest).

use std::sync::Arc;

use object_store::aws::{AmazonS3, AmazonS3Builder, AwsCredential};
use object_store::client::{
    HttpClient, HttpConnector, HttpError, HttpErrorKind, HttpRequest, HttpResponse,
    HttpResponseBody, HttpService,
};
use object_store::{ClientOptions, ObjectMeta, PutMode, PutOptions, RetryConfig};
use object_store::{ObjectStore, ObjectStoreExt, StaticCredentialProvider};

use super::net::{http_client, parse_endpoint, Endpoint, NetConfig};
use super::{Credentials, Destination};

/// A stable error category. The `/test` response and job errors expose this
/// string, never the upstream body, URL query or headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Dns,
    BlockedAddress,
    Tls,
    Auth,
    Bucket,
    Region,
    Network,
    Unsupported,
    /// An object is absent (a missing chunk, not a missing bucket).
    NotFound,
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Dns => "dns",
            Category::BlockedAddress => "blocked-address",
            Category::Tls => "tls",
            Category::Auth => "auth",
            Category::Bucket => "bucket",
            Category::Region => "region",
            Category::Network => "network",
            Category::Unsupported => "unsupported",
            Category::NotFound => "bucket",
        }
    }
}

/// A sanitized S3 failure.
#[derive(Debug, Clone)]
pub struct S3Error {
    pub category: Category,
    pub message: String,
}

impl S3Error {
    fn new(op: &str, category: Category) -> Self {
        Self {
            category,
            message: format!("{op} failed ({})", category.as_str()),
        }
    }
}

impl std::fmt::Display for S3Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<S3Error> for String {
    fn from(error: S3Error) -> Self {
        error.message
    }
}

fn classify(op: &str, error: object_store::Error) -> S3Error {
    use object_store::Error as E;
    let category = match &error {
        E::Unauthenticated { .. } | E::PermissionDenied { .. } => Category::Auth,
        E::NotFound { .. } => Category::NotFound,
        E::NotImplemented { .. } | E::NotSupported { .. } => Category::Unsupported,
        E::AlreadyExists { .. } | E::Precondition { .. } | E::NotModified { .. } => {
            Category::Unsupported
        }
        _ => classify_text(&error.to_string()),
    };
    // The payload of an object-store error can quote a response body. Keep it
    // out of every surfaced message (logs get the category only).
    tracing::debug!("cloud s3 {op}: category {}", category.as_str());
    S3Error::new(op, category)
}

fn classify_text(text: &str) -> Category {
    let text = text.to_ascii_lowercase();
    let has = |needle: &str| text.contains(needle);
    if has("blocked address")
        || has("cloudprivateendpoints")
        || has("refusing to resolve")
        || has("not an allowed address")
    {
        return Category::BlockedAddress;
    }
    if has("dns") || has("lookup") || has("name or service not known") || has("no such host") {
        return Category::Dns;
    }
    if has("certificate")
        || has("tls")
        || has("handshake")
        || has("unknown issuer")
        || has("invalid peer")
    {
        return Category::Tls;
    }
    if has("status code: 401")
        || has("status code: 403")
        || has("unauthorized")
        || has("accessdenied")
        || has("signaturedoesnotmatch")
        || has("invalidaccesskeyid")
    {
        return Category::Auth;
    }
    if has("status code: 404") || has("nosuchbucket") {
        return Category::NotFound;
    }
    if has("status code: 301")
        || has("status code: 307")
        || has("permanentredirect")
        || has("incorrectly configured region")
    {
        return Category::Region;
    }
    if has("status code: 501")
        || has("not implemented")
        || has("notimplemented")
        || has("conditional")
    {
        return Category::Unsupported;
    }
    if has("status code: 400") || has("status code: 409") || has("status code: 412") {
        return Category::Bucket;
    }
    if has("timed out")
        || has("timeout")
        || has("connect")
        || has("connection")
        || has("broken pipe")
        || has("reset by peer")
    {
        return Category::Network;
    }
    Category::Network
}

/// The per-destination object store.
pub struct S3Storage {
    store: AmazonS3,
    root: String,
    pub conditional_create: bool,
    pub endpoint: Endpoint,
}

impl S3Storage {
    /// Build the transport for one destination. Empty credentials are refused
    /// before `build()`, so the IMDS credential provider is unreachable.
    pub fn build(
        destination: &Destination,
        credentials: &Credentials,
        config: &NetConfig,
    ) -> Result<Self, S3Error> {
        if credentials.access_key_id.trim().is_empty() || credentials.secret_access_key.is_empty() {
            return Err(S3Error::new("building the S3 client", Category::Auth));
        }
        let endpoint = parse_endpoint(&destination.endpoint, destination.allow_insecure_http)
            .map_err(|error| S3Error {
                category: if error.contains("blocked") || error.contains("cloudPrivateEndpoints") {
                    Category::BlockedAddress
                } else {
                    Category::Network
                },
                message: format!("building the S3 client: {error}"),
            })?;
        // A literal address never reaches the DNS resolver, so enforce the
        // private allowlist here as well (a hostname is vetted by the resolver).
        if let Ok(ip) = endpoint
            .host
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
        {
            if !config.address_allowed(&ip, endpoint.port, endpoint.plaintext) {
                return Err(S3Error {
                    category: Category::BlockedAddress,
                    message: "building the S3 client: the endpoint address is not allowed (public endpoints need https on 443; private endpoints need cloudPrivateEndpoints)"
                        .to_string(),
                });
            }
        }
        let client = http_client(&endpoint, config).map_err(|error| S3Error {
            category: Category::Network,
            message: format!("building the S3 client: {error}"),
        })?;
        let token = credentials
            .session_token
            .as_ref()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty());
        let credential = AwsCredential {
            key_id: credentials.access_key_id.trim().to_string(),
            secret_key: credentials.secret_access_key.clone(),
            token,
        };
        let retry = RetryConfig {
            max_retries: 3,
            retry_timeout: std::time::Duration::from_secs(60),
            ..RetryConfig::default()
        };
        let builder = AmazonS3Builder::new()
            .with_bucket_name(&destination.bucket)
            .with_region(&destination.region)
            .with_endpoint(&endpoint.url)
            .with_virtual_hosted_style_request(!destination.path_style)
            .with_allow_http(destination.allow_insecure_http)
            .with_credentials(Arc::new(StaticCredentialProvider::new(credential)))
            .with_http_connector(ReqwestConnector {
                client: client.clone(),
            })
            .with_retry(retry);
        let store = builder.build().map_err(|e| S3Error {
            category: Category::Network,
            message: format!("building the S3 client: {e}"),
        })?;
        Ok(Self {
            store,
            root: destination.prefix.trim_matches('/').to_string(),
            conditional_create: destination.conditional_create,
            endpoint,
        })
    }

    /// The configured key prefix (trimmed, may be empty).
    pub fn root_prefix(&self) -> String {
        self.root.clone()
    }

    /// `<prefix>/<source>` (no trailing slash).
    pub fn source_root(&self, source: &str) -> String {
        if self.root.is_empty() {
            source.to_string()
        } else {
            format!("{}/{}", self.root, source)
        }
    }

    /// A random probe key under the configured prefix.
    pub fn probe_key(&self) -> String {
        let mut buf = [0u8; 8];
        let _ = getrandom::fill(&mut buf);
        let suffix: String = buf.iter().map(|b| format!("{b:02x}")).collect();
        let root = if self.root.is_empty() {
            String::new()
        } else {
            format!("{}/", self.root)
        };
        format!("{root}.naslos-probe/{suffix}")
    }

    pub async fn put_create_or_equal(&self, key: &str, bytes: &[u8]) -> Result<(), S3Error> {
        if self.conditional_create {
            let path = object_store::path::Path::from(key);
            let opts = PutOptions {
                mode: PutMode::Create,
                ..PutOptions::default()
            };
            match self
                .store
                .put_opts(&path, bytes.to_vec().into(), opts)
                .await
            {
                Ok(_) => return Ok(()),
                Err(object_store::Error::AlreadyExists { .. })
                | Err(object_store::Error::Precondition { .. })
                | Err(object_store::Error::NotModified { .. }) => {
                    return self.existing_matches(key, bytes).await;
                }
                Err(object_store::Error::NotImplemented { .. })
                | Err(object_store::Error::NotSupported { .. }) => {
                    // The saved capability said yes but the provider disagrees:
                    // fall through to HEAD+PUT rather than failing the backup.
                }
                Err(e) => return Err(classify("conditional create", e)),
            }
        }
        match self.head(key).await? {
            None => self.put_overwrite(key, bytes).await,
            Some(_) => self.existing_matches(key, bytes).await,
        }
    }

    async fn existing_matches(&self, key: &str, bytes: &[u8]) -> Result<(), S3Error> {
        let existing = self.get(key).await?;
        if existing == bytes {
            Ok(())
        } else {
            Err(S3Error {
                category: Category::Unsupported,
                message: "the bucket already holds different bytes for the same object; start a new attempt"
                    .to_string(),
            })
        }
    }

    pub async fn put_overwrite(&self, key: &str, bytes: &[u8]) -> Result<(), S3Error> {
        let path = object_store::path::Path::from(key);
        self.store
            .put(&path, bytes.to_vec().into())
            .await
            .map(|_| ())
            .map_err(|e| classify("put", e))
    }

    pub async fn get(&self, key: &str) -> Result<Vec<u8>, S3Error> {
        let path = object_store::path::Path::from(key);
        let result = self
            .store
            .get(&path)
            .await
            .map_err(|e| classify("get", e))?;
        let bytes = result.bytes().await.map_err(|_error| S3Error {
            category: Category::Network,
            message: format!("reading the object failed ({})", Category::Network.as_str()),
        })?;
        Ok(bytes.to_vec())
    }

    pub async fn head(&self, key: &str) -> Result<Option<ObjectMeta>, S3Error> {
        let path = object_store::path::Path::from(key);
        match self.store.head(&path).await {
            Ok(meta) => Ok(Some(meta)),
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(classify("head", e)),
        }
    }

    pub async fn list(&self, prefix: &str) -> Result<Vec<ObjectMeta>, S3Error> {
        use futures::StreamExt;
        let path = object_store::path::Path::from(prefix);
        let stream = self.store.list(Some(&path));
        let mut out = Vec::new();
        let mut stream = stream;
        while let Some(item) = stream.next().await {
            out.push(item.map_err(|e| classify("list", e))?);
        }
        Ok(out)
    }

    pub async fn delete(&self, key: &str) -> Result<(), S3Error> {
        let path = object_store::path::Path::from(key);
        match self.store.delete(&path).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(e) => Err(classify("delete", e)),
        }
    }

    /// Probe whether this bucket implements `If-None-Match: *` on PUT. A 501
    /// (Backblaze B2) or any non-capability failure that still proves writes
    /// work means `false`; auth/network/TLS failures are returned as errors so
    /// `/test` can report them.
    pub async fn probe_conditional_create(&self) -> Result<bool, S3Error> {
        let key = self.probe_key();
        let path = object_store::path::Path::from(key.as_str());
        let create = PutOptions {
            mode: PutMode::Create,
            ..PutOptions::default()
        };
        match self
            .store
            .put_opts(&path, Vec::new().into(), create.clone())
            .await
        {
            Ok(_) => {}
            Err(object_store::Error::AlreadyExists { .. })
            | Err(object_store::Error::Precondition { .. })
            | Err(object_store::Error::NotModified { .. }) => {
                // A stale probe object; that still proves the header is
                // accepted, so the second probe answers the capability.
            }
            Err(object_store::Error::NotImplemented { .. })
            | Err(object_store::Error::NotSupported { .. }) => {
                return Ok(false);
            }
            Err(e) => {
                let classified = classify("conditional create probe", e);
                if matches!(
                    classified.category,
                    Category::Auth
                        | Category::Tls
                        | Category::Dns
                        | Category::BlockedAddress
                        | Category::Network
                        | Category::Region
                ) {
                    return Err(classified);
                }
                return Ok(false);
            }
        }
        let second = self.store.put_opts(&path, Vec::new().into(), create).await;
        let supported = match second {
            Ok(_) => false,
            Err(object_store::Error::AlreadyExists { .. })
            | Err(object_store::Error::Precondition { .. })
            | Err(object_store::Error::NotModified { .. }) => true,
            Err(object_store::Error::NotImplemented { .. })
            | Err(object_store::Error::NotSupported { .. }) => false,
            Err(e) => {
                let classified = classify("conditional create probe", e);
                if matches!(
                    classified.category,
                    Category::Auth
                        | Category::Tls
                        | Category::Dns
                        | Category::BlockedAddress
                        | Category::Network
                        | Category::Region
                ) {
                    return Err(classified);
                }
                false
            }
        };
        let _ = self.delete(&key).await;
        Ok(supported)
    }

    /// A `/test` probe: bucket reachable with these credentials, then the
    /// conditional-create capability. Returns `(ok, category, conditional)`.
    pub async fn test(&self) -> (bool, Option<Category>, bool) {
        match self.list(&self.probe_prefix()).await {
            Ok(_) => {}
            Err(e) => {
                let category = if e.category == Category::NotFound {
                    Category::Bucket
                } else {
                    e.category
                };
                return (false, Some(category), false);
            }
        }
        match self.probe_conditional_create().await {
            Ok(conditional) => (true, None, conditional),
            Err(e) => (false, Some(e.category), false),
        }
    }

    fn probe_prefix(&self) -> String {
        if self.root.is_empty() {
            ".naslos-probe/".to_string()
        } else {
            format!("{}/.naslos-probe/", self.root)
        }
    }
}

/// The saved request fields never carry the URL query or body into an error.
#[derive(Debug, Clone)]
struct ReqwestConnector {
    client: reqwest::Client,
}

impl HttpConnector for ReqwestConnector {
    fn connect(&self, _options: &ClientOptions) -> object_store::Result<HttpClient> {
        Ok(HttpClient::new(CloudHttpService {
            client: self.client.clone(),
        }))
    }
}

/// The reqwest 0.12-backed `HttpService` object_store talks through.
#[derive(Debug, Clone)]
struct CloudHttpService {
    client: reqwest::Client,
}

#[async_trait::async_trait]
impl HttpService for CloudHttpService {
    async fn call(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        use http_body_util::BodyExt;
        let (parts, body) = request.into_parts();
        let contiguous = body.as_bytes().cloned();
        let body = match contiguous {
            Some(bytes) => bytes,
            None => body
                .collect()
                .await
                .map_err(|e| HttpError::new(HttpErrorKind::Decode, e))?
                .to_bytes(),
        };
        let url: reqwest::Url = parts
            .uri
            .to_string()
            .parse()
            .map_err(|e| HttpError::new(HttpErrorKind::Request, e))?;
        let mut req = reqwest::Request::new(parts.method, url);
        *req.headers_mut() = parts.headers;
        *req.body_mut() = Some(reqwest::Body::from(body));
        let response = self.client.execute(req).await.map_err(map_reqwest_error)?;
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.bytes().await.map_err(map_reqwest_error)?;
        let mut out = HttpResponse::new(HttpResponseBody::from(bytes));
        *out.status_mut() = status;
        *out.headers_mut() = headers;
        Ok(out)
    }
}

fn map_reqwest_error(e: reqwest::Error) -> HttpError {
    let kind = if e.is_timeout() {
        HttpErrorKind::Timeout
    } else if e.is_connect() {
        HttpErrorKind::Connect
    } else if e.is_decode() {
        HttpErrorKind::Decode
    } else {
        HttpErrorKind::Request
    };
    // `without_url` keeps the endpoint (and any query) out of the chain even
    // before our classification discards the text.
    HttpError::new(kind, e.without_url())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_text_maps_to_the_documented_categories() {
        let cases = [
            (
                "Server returned non-2xx status code: 403 Forbidden",
                Category::Auth,
            ),
            (
                "Server returned non-2xx status code: 401 Unauthorized",
                Category::Auth,
            ),
            (
                "Server returned non-2xx status code: 404 Not Found",
                Category::NotFound,
            ),
            (
                "Server returned non-2xx status code: 301 Moved Permanently",
                Category::Region,
            ),
            (
                "Server returned non-2xx status code: 501 Not Implemented",
                Category::Unsupported,
            ),
            (
                "Server returned non-2xx status code: 400 Bad Request",
                Category::Bucket,
            ),
            (
                "error sending request: dns error: failed to lookup address",
                Category::Dns,
            ),
            (
                "the endpoint resolves to a blocked address or port",
                Category::BlockedAddress,
            ),
            ("invalid peer certificate: UnknownIssuer", Category::Tls),
            ("connection reset by peer", Category::Network),
        ];
        for (text, expected) in cases {
            assert_eq!(classify_text(text), expected, "{text}");
        }
    }

    #[test]
    fn the_sanitized_message_never_echoes_the_upstream_text() {
        let err = S3Error::new("put", Category::Auth);
        assert_eq!(err.message, "put failed (auth)");
        assert!(!err.message.contains("http"));
    }

    #[test]
    fn a_private_literal_endpoint_is_checked_before_the_client_is_built() {
        let destination = Destination {
            endpoint: "https://192.168.1.50:9000".into(),
            bucket: "naslos-backups".into(),
            region: "us-east-1".into(),
            path_style: true,
            ..Default::default()
        };
        let credentials = Credentials {
            access_key_id: "AKIA".into(),
            secret_access_key: "secret".into(),
            session_token: None,
        };
        let error = match S3Storage::build(&destination, &credentials, &NetConfig::default()) {
            Ok(_) => panic!("an unlisted private literal must be refused"),
            Err(error) => error,
        };
        assert_eq!(error.category, Category::BlockedAddress);
    }

    #[test]
    fn the_source_root_joins_the_configured_prefix() {
        // Construction of a real S3Storage needs credentials and a client; the
        // prefix join is exercised through the pure helper below.
        assert_eq!(join_root("", "tank/data"), "tank/data");
        assert_eq!(join_root("backups", "tank/data"), "backups/tank/data");
    }

    fn join_root(root: &str, source: &str) -> String {
        if root.is_empty() {
            source.to_string()
        } else {
            format!("{root}/{source}")
        }
    }
}
