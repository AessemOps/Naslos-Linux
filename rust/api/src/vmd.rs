//! Typed HTTP client for the naslos-vmd supervisor pod (the `agent.rs`
//! pattern, applied to QEMU).
//!
//! vmd runs one privileged hostNetwork pod (headless `naslos-vmd` Service on
//! :9091) and reconciles QEMU processes against the desired state the API
//! pushes. The API uses this client for `/api/virt/*`: capabilities, the
//! live-state list, the revision-tagged config push (identical revisions are
//! no-ops on the vmd side), console details and the
//! image library relay.

use serde::{Deserialize, Serialize};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// Used when `VMD_BASE_URL` is unset; `{}` is the workload namespace.
pub const DEFAULT_BASE_URL_PATTERN: &str = "https://naslos-vmd.{}.svc.cluster.local:9091";

/// Total deadline for ordinary (JSON) vmd calls. Reconcile can start a VM
/// (disk create/convert) but not a download.
pub const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

pub const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The byte stream under the console tunnel (TLS or plain TCP).
pub trait TunnelStream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> TunnelStream for T {}

/// The API side of the console tunnel: a WebSocket carrying RFB bytes.
pub type VncTunnel = tokio_tungstenite::WebSocketStream<Box<dyn TunnelStream>>;

/// Build the tunnel URL from the client base URL, mapping https/http to
/// wss/ws and keeping the exact authority (including a non-default port).
pub fn vnc_tunnel_url(base_url: &str, name: &str, port: u16) -> Result<String, String> {
    let url = reqwest::Url::parse(base_url).map_err(|e| format!("parsing the vmd URL: {e}"))?;
    let host = url
        .host_str()
        .ok_or_else(|| "the vmd URL has no host".to_string())?;
    let authority = match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    };
    let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
    Ok(format!(
        "{scheme}://{authority}/api/v1/vms/{name}/vnc?port={port}"
    ))
}

/// A vmd-side failure carrying the upstream HTTP status.
#[derive(Debug, Clone)]
pub struct VmdError {
    pub status: u16,
    pub message: String,
}

impl std::fmt::Display for VmdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "vmd: {}", self.message)
    }
}

impl std::error::Error for VmdError {}

/// The node's hardware report (FR-VMT preflight).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Capabilities {
    #[serde(default)]
    pub kvm: bool,
    #[serde(default, rename = "cpuModel")]
    pub cpu_model: String,
    #[serde(default)]
    pub nested: bool,
    #[serde(default)]
    pub nics: Vec<String>,
    /// The supervisor image ships the OVMF Secure Boot firmware pair.
    #[serde(default, rename = "secureBoot")]
    pub secure_boot: bool,
}

/// One VM's live state as vmd reports it. `vnc_port`/`vnc_password` are set
/// while the VM runs; the console bridge needs both.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VmLive {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default, rename = "vncPort")]
    pub vnc_port: Option<u16>,
    #[serde(default, rename = "vncPassword")]
    pub vnc_password: String,
    #[serde(default, rename = "cpuPercent")]
    pub cpu_percent: f64,
    #[serde(default, rename = "memUsedMiB")]
    pub mem_used_mib: u32,
    #[serde(default, rename = "unexpectedExit")]
    pub unexpected_exit: bool,
    #[serde(default, rename = "lastError")]
    pub last_error: String,
}

/// The desired state the API renders and pushes (DM-7 records plus the
/// per-VM desired runtime state).
pub type DesiredConfig = naslos_vmd::model::DesiredConfig;

/// One image library listing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImagesList {
    #[serde(default)]
    pub images: Vec<ImageEntry>,
    #[serde(default)]
    pub importing: Vec<String>,
    #[serde(default)]
    pub errors: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImageEntry {
    pub name: String,
    #[serde(default, rename = "sizeBytes")]
    pub size_bytes: u64,
}

/// A thin HTTP client for the vmd endpoints.
pub struct Client {
    base_url: String,
    token: String,
    /// PEM CA used for both reqwest and the console tunnel's TLS handshake.
    ca_file: String,
    http: reqwest::Client,
    /// Streaming uploads: no total deadline, only connect + per-read idle, so
    /// a large chunk on a slow LAN is not cut mid-body.
    stream_http: reqwest::Client,
}

impl Client {
    /// Build the base URL from `VMD_BASE_URL`, else the in-cluster pattern
    /// using the workload namespace.
    pub fn base_url_from_env() -> String {
        if let Ok(v) = std::env::var("VMD_BASE_URL") {
            if !v.is_empty() {
                return v.trim_end_matches('/').to_string();
            }
        }
        let ns = std::env::var("NASLOS_WORKLOAD_NAMESPACE")
            .or_else(|_| std::env::var("NASLOS_NAMESPACE"))
            .unwrap_or_else(|_| "naslos-privileged".to_string());
        DEFAULT_BASE_URL_PATTERN.replace("{}", &ns)
    }

    /// Build from the environment: base URL, `VMD_TOKEN`, `VMD_CA_FILE`.
    /// An unreadable or empty CA file is an error rather than a silent
    /// fallback to the system roots (fail closed, PF-M5).
    pub fn from_env() -> Result<Self, String> {
        let base = Self::base_url_from_env();
        let token = std::env::var("VMD_TOKEN").unwrap_or_default();
        let ca_file = std::env::var("VMD_CA_FILE").unwrap_or_default();
        Self::new(&base, &token, &ca_file)
    }

    /// Create a client for `base_url` with a pinned CA. An HTTPS vmd URL
    /// without a CA is refused (PF-M5).
    pub fn new(base_url: &str, token: &str, ca_file: &str) -> Result<Self, String> {
        if base_url.starts_with("https://") && ca_file.trim().is_empty() {
            return Err(
                "VMD_CA_FILE is required for an https vmd URL; refusing to use the system roots"
                    .to_string(),
            );
        }
        let http = Self::builder(ca_file)?
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(DEFAULT_TIMEOUT)
            .build()
            .map_err(|e| format!("building the vmd HTTP client: {e}"))?;
        let stream_http = Self::builder(ca_file)?
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(std::time::Duration::from_secs(300))
            .build()
            .map_err(|e| format!("building the vmd streaming HTTP client: {e}"))?;
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            ca_file: ca_file.to_string(),
            http,
            stream_http,
        })
    }

    fn builder(ca_file: &str) -> Result<reqwest::ClientBuilder, String> {
        let mut builder = reqwest::Client::builder();
        if !ca_file.is_empty() {
            let pem =
                std::fs::read(ca_file).map_err(|e| format!("reading the vmd CA {ca_file}: {e}"))?;
            let cert = reqwest::Certificate::from_pem(&pem)
                .map_err(|e| format!("the vmd CA {ca_file} contains no usable certificate: {e}"))?;
            builder = builder
                .tls_built_in_root_certs(false)
                .add_root_certificate(cert);
        }
        Ok(builder)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    async fn do_json<T: serde::de::DeserializeOwned>(
        &self,
        rb: reqwest::RequestBuilder,
    ) -> Result<T, VmdError> {
        let resp = rb.send().await.map_err(|e| VmdError {
            status: 0,
            message: format!("contacting vmd at {}: {e}", self.base_url),
        })?;
        let status = resp.status().as_u16();
        let body = resp.bytes().await.map_err(|e| VmdError {
            status: 0,
            message: format!("reading vmd response: {e}"),
        })?;
        if !(200..300).contains(&status) {
            let raw = String::from_utf8_lossy(&body);
            let mut msg = raw.trim().to_string();
            if let Ok(payload) =
                serde_json::from_slice::<std::collections::HashMap<String, String>>(&body)
            {
                if let Some(inner) = payload.get("error") {
                    if !inner.is_empty() {
                        msg = inner.clone();
                    }
                }
            }
            return Err(VmdError {
                status,
                message: msg,
            });
        }
        if body.iter().all(|b| b.is_ascii_whitespace()) {
            return serde_json::from_slice(b"null").map_err(|e| VmdError {
                status: 0,
                message: format!("decoding vmd response: {e}"),
            });
        }
        serde_json::from_slice(&body).map_err(|e| VmdError {
            status: 0,
            message: format!("decoding vmd response: {e}"),
        })
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let rb = self.http.request(method, self.url(path));
        if self.token.is_empty() {
            rb
        } else {
            rb.bearer_auth(&self.token)
        }
    }

    /// The host the vmd Service resolves to (the node's own address): the
    /// console bridge opens its TCP leg there.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Open the authenticated console tunnel to vmd: a WebSocket on vmd's
    /// existing TLS listener whose payload is proxied to QEMU's loopback VNC
    /// port. The API no longer dials the node's VNC port directly, so QEMU can
    /// stay bound to 127.0.0.1.
    pub async fn vnc_tunnel(&self, name: &str, port: u16) -> Result<VncTunnel, String> {
        let url =
            reqwest::Url::parse(&self.base_url).map_err(|e| format!("parsing the vmd URL: {e}"))?;
        let host = url
            .host_str()
            .ok_or_else(|| "the vmd URL has no host".to_string())?
            .to_string();
        let connect_port = url
            .port_or_known_default()
            .ok_or_else(|| "the vmd URL has no port".to_string())?;
        let tls = url.scheme() == "https";
        let tcp = tokio::time::timeout(
            CONNECT_TIMEOUT,
            tokio::net::TcpStream::connect((host.as_str(), connect_port)),
        )
        .await
        .map_err(|_| "connecting to vmd timed out".to_string())?
        .map_err(|e| format!("connecting to vmd: {e}"))?;
        let stream: Box<dyn TunnelStream> = if tls {
            let config = self.tunnel_tls_config()?;
            let server_name = rustls::pki_types::ServerName::try_from(host.clone())
                .map_err(|_| "the vmd host name is not a valid TLS server name".to_string())?;
            let tls_stream = tokio_rustls::TlsConnector::from(config)
                .connect(server_name, tcp)
                .await
                .map_err(|e| format!("the vmd console tunnel TLS handshake failed: {e}"))?;
            Box::new(tls_stream)
        } else {
            Box::new(tcp)
        };

        let mut request = vnc_tunnel_url(&self.base_url, name, port)?
            .into_client_request()
            .map_err(|e| format!("building the vmd console tunnel request: {e}"))?;
        let bearer = axum::http::HeaderValue::from_str(&format!("Bearer {}", self.token))
            .map_err(|_| "the vmd token is not a valid header value".to_string())?;
        request
            .headers_mut()
            .insert(axum::http::header::AUTHORIZATION, bearer);
        let (tunnel, _response) = tokio_tungstenite::client_async(request, stream)
            .await
            .map_err(|e| format!("opening the vmd console tunnel: {e}"))?;
        Ok(tunnel)
    }

    /// Trust only the configured vmd CA for the tunnel (fail closed, same
    /// posture as the reqwest client).
    fn tunnel_tls_config(&self) -> Result<std::sync::Arc<rustls::ClientConfig>, String> {
        if self.ca_file.trim().is_empty() {
            return Err("VMD_CA_FILE is required for the https console tunnel".to_string());
        }
        let _ = rustls::crypto::ring::default_provider().install_default();
        let pem = std::fs::read(&self.ca_file)
            .map_err(|e| format!("reading the vmd CA {}: {e}", self.ca_file))?;
        let mut roots = rustls::RootCertStore::empty();
        for cert in rustls_pemfile::certs(&mut std::io::BufReader::new(pem.as_slice())) {
            roots
                .add(cert.map_err(|e| format!("parsing the vmd CA certificate: {e}"))?)
                .map_err(|e| format!("adding the vmd CA certificate: {e}"))?;
        }
        if roots.is_empty() {
            return Err("the vmd CA file contains no usable certificate".to_string());
        }
        Ok(std::sync::Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        ))
    }

    // ---- Capabilities ----------------------------------------------------

    pub async fn capabilities(&self) -> Result<Capabilities, VmdError> {
        self.do_json(self.request(reqwest::Method::GET, "/api/v1/capabilities"))
            .await
    }

    // ---- Live state ------------------------------------------------------

    pub async fn vms_live(&self) -> Result<Vec<VmLive>, VmdError> {
        self.do_json(self.request(reqwest::Method::GET, "/api/v1/vms"))
            .await
    }

    // ---- Desired state ---------------------------------------------------

    /// Push the whole desired config (atomic on the vmd side: it validates
    /// everything before applying, and an identical revision is a no-op).
    pub async fn push_config(&self, config: &DesiredConfig) -> Result<(), VmdError> {
        let rb = self
            .request(reqwest::Method::PUT, "/api/v1/vms/config")
            .json(config);
        self.do_json::<serde_json::Value>(rb).await.map(|_| ())
    }

    /// Terminate one VM immediately (the force-stop/restart actions).
    pub async fn force_stop(&self, name: &str) -> Result<(), VmdError> {
        let path = format!(
            "/api/v1/vms/{}/force-stop",
            crate::agent::escape_component(name)
        );
        let rb = self.request(reqwest::Method::POST, &path);
        self.do_json::<serde_json::Value>(rb).await.map(|_| ())
    }

    // ---- Image library ---------------------------------------------------

    pub async fn images(&self, pool: &str) -> Result<ImagesList, VmdError> {
        let path = format!(
            "/api/v1/images?pool={}",
            crate::agent::escape_component(pool)
        );
        self.do_json(self.request(reqwest::Method::GET, &path))
            .await
    }

    /// Stream one upload chunk to vmd. The body is relayed (not buffered) so
    /// a multi-gigabyte ISO never lands in API memory. `confirm` must equal
    /// the image name to replace an existing library image (vmd refuses the
    /// overwrite without it).
    pub async fn upload_chunk(
        &self,
        pool: &str,
        name: &str,
        offset: u64,
        final_chunk: bool,
        confirm: Option<&str>,
        body: axum::body::Body,
    ) -> Result<u64, VmdError> {
        use futures::StreamExt;
        let mut path = format!(
            "/api/v1/images/upload?pool={}&name={}&offset={}&final={}",
            crate::agent::escape_component(pool),
            crate::agent::escape_component(name),
            offset,
            final_chunk
        );
        if let Some(confirm) = confirm {
            path.push_str(&format!(
                "&confirm={}",
                crate::agent::escape_component(confirm)
            ));
        }
        let stream = body.into_data_stream();
        let body = reqwest::Body::wrap_stream(stream.map(|r| r.map_err(std::io::Error::other)));
        let resp = self
            .stream_http
            .request(reqwest::Method::POST, self.url(&path))
            .bearer_auth(&self.token)
            .header("Content-Type", "application/octet-stream")
            .body(body)
            .send()
            .await
            .map_err(|e| VmdError {
                status: 0,
                message: format!("uploading the chunk to vmd: {e}"),
            })?;
        let status = resp.status().as_u16();
        let bytes = resp.bytes().await.map_err(|e| VmdError {
            status: 0,
            message: format!("reading the vmd upload response: {e}"),
        })?;
        if !(200..300).contains(&status) {
            let raw = String::from_utf8_lossy(&bytes);
            let mut msg = raw.trim().to_string();
            if let Ok(payload) =
                serde_json::from_slice::<std::collections::HashMap<String, String>>(&bytes)
            {
                if let Some(inner) = payload.get("error") {
                    if !inner.is_empty() {
                        msg = inner.clone();
                    }
                }
            }
            return Err(VmdError {
                status,
                message: msg,
            });
        }
        let payload: std::collections::HashMap<String, serde_json::Value> =
            serde_json::from_slice(&bytes).map_err(|e| VmdError {
                status: 0,
                message: format!("decoding the vmd upload response: {e}"),
            })?;
        Ok(payload
            .get("sizeBytes")
            .and_then(|v| v.as_u64())
            .unwrap_or(0))
    }

    /// Ask vmd to import an image from a URL (the SSRF guard lives in vmd:
    /// it is hostNetwork, whose egress is exempt from pod policy). `confirm`
    /// must equal the image name to replace an existing library image.
    pub async fn import_image(
        &self,
        pool: &str,
        name: &str,
        url: &str,
        sha256: Option<&str>,
        confirm: Option<&str>,
    ) -> Result<(), VmdError> {
        let mut payload = serde_json::json!({ "pool": pool, "name": name, "url": url });
        if let Some(sha) = sha256 {
            payload["sha256"] = serde_json::Value::String(sha.to_string());
        }
        if let Some(confirm) = confirm {
            payload["confirm"] = serde_json::Value::String(confirm.to_string());
        }
        let rb = self
            .request(reqwest::Method::POST, "/api/v1/images/import")
            .json(&payload);
        self.do_json::<serde_json::Value>(rb).await.map(|_| ())
    }

    pub async fn delete_image(&self, pool: &str, name: &str) -> Result<(), VmdError> {
        let path = format!(
            "/api/v1/images/{}?pool={}",
            crate::agent::escape_component(name),
            crate::agent::escape_component(pool)
        );
        let rb = self.request(reqwest::Method::DELETE, &path);
        self.do_json::<serde_json::Value>(rb).await.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_without_a_ca_is_refused() {
        let err = match Client::new("https://naslos-vmd.naslos-privileged.svc:9091", "t", "") {
            Ok(_) => panic!("an https URL without a CA must be refused"),
            Err(e) => e,
        };
        assert!(err.contains("VMD_CA_FILE"), "{err}");
        assert!(Client::new("http://127.0.0.1:9091", "t", "").is_ok());
    }

    #[test]
    fn base_url_uses_the_workload_namespace() {
        let url = DEFAULT_BASE_URL_PATTERN.replace("{}", "naslos-privileged");
        assert_eq!(
            url,
            "https://naslos-vmd.naslos-privileged.svc.cluster.local:9091"
        );
    }

    #[test]
    fn capabilities_parse_secure_boot_and_default_it_off() {
        let caps: Capabilities = serde_json::from_value(serde_json::json!({
            "kvm": true,
            "secureBoot": true
        }))
        .unwrap();
        assert!(caps.secure_boot);
        // An older supervisor without the field means "unavailable".
        let legacy: Capabilities =
            serde_json::from_value(serde_json::json!({ "kvm": true })).unwrap();
        assert!(!legacy.secure_boot);
    }
}

#[cfg(test)]
mod vnc_tunnel_url_tests {
    use super::vnc_tunnel_url;

    #[test]
    fn the_tunnel_keeps_the_vmd_authority_and_maps_the_scheme() {
        assert_eq!(
            vnc_tunnel_url("https://naslos-vmd.naslos-privileged.svc:9091", "vm1", 5901).unwrap(),
            "wss://naslos-vmd.naslos-privileged.svc:9091/api/v1/vms/vm1/vnc?port=5901"
        );
        assert_eq!(
            vnc_tunnel_url("http://127.0.0.1:9091", "vm1", 5902).unwrap(),
            "ws://127.0.0.1:9091/api/v1/vms/vm1/vnc?port=5902"
        );
        assert!(vnc_tunnel_url("not a url", "vm1", 5901).is_err());
    }
}
