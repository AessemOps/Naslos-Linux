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

/// Used when `VMD_BASE_URL` is unset; `{}` is the workload namespace.
pub const DEFAULT_BASE_URL_PATTERN: &str = "https://naslos-vmd.{}.svc.cluster.local:9091";

/// Total deadline for ordinary (JSON) vmd calls. Reconcile can start a VM
/// (disk create/convert) but not a download.
pub const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

pub const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

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
    /// a multi-gigabyte ISO never lands in API memory.
    pub async fn upload_chunk(
        &self,
        pool: &str,
        name: &str,
        offset: u64,
        final_chunk: bool,
        body: axum::body::Body,
    ) -> Result<u64, VmdError> {
        use futures::StreamExt;
        let path = format!(
            "/api/v1/images/upload?pool={}&name={}&offset={}&final={}",
            crate::agent::escape_component(pool),
            crate::agent::escape_component(name),
            offset,
            final_chunk
        );
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
    /// it is hostNetwork, whose egress is exempt from pod policy).
    pub async fn import_image(
        &self,
        pool: &str,
        name: &str,
        url: &str,
        sha256: Option<&str>,
    ) -> Result<(), VmdError> {
        let mut payload = serde_json::json!({ "pool": pool, "name": name, "url": url });
        if let Some(sha) = sha256 {
            payload["sha256"] = serde_json::Value::String(sha.to_string());
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
