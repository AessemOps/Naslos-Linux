//! Typed HTTP client for the naslos-agent DaemonSet (port of
//! `api/internal/agent`).
//!
//! The agent runs one pod per node (hostNetwork :9090, fronted by the headless
//! `naslos-agent` Service) and executes `zpool`/`zfs` via `chroot /host`. The
//! API uses this client to implement `/api/volumes/zfs`, `/api/datasets` and
//! the shares apply path.

use serde::{Deserialize, Serialize};

/// Used when `AGENT_BASE_URL` is unset; `{}` is the namespace.
pub const DEFAULT_BASE_URL_PATTERN: &str = "https://naslos-agent.{}.svc.cluster.local:9090";

/// Covers slow operations (wipefs + `zpool create` on real spinning disks).
pub const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

/// An agent-side failure carrying the upstream HTTP status so the API handler
/// can forward 503 (degraded, no ZFS on host) instead of collapsing everything
/// into 500.
#[derive(Debug, Clone)]
pub struct AgentError {
    pub status: u16,
    pub message: String,
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "agent: {}", self.message)
    }
}

impl std::error::Error for AgentError {}

/// A ZFS pool (mirrors the agent's `Pool` JSON shape).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Pool {
    pub name: String,
    pub size: String,
    pub alloc: String,
    pub free: String,
    pub health: String,
    #[serde(default)]
    pub topology: String,
    #[serde(default)]
    pub disks: Vec<String>,
    #[serde(default)]
    pub mountpoint: String,
}

/// `POST /api/v1/pools` request.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreatePoolRequest {
    pub name: String,
    pub topology: String,
    pub disks: Vec<String>,
    #[serde(default)]
    pub cache: String,
    #[serde(default)]
    pub options: std::collections::HashMap<String, String>,
}

/// A ZFS dataset (mirrors the agent's `Dataset` JSON shape).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Dataset {
    pub name: String,
    #[serde(default)]
    pub used: String,
    #[serde(default)]
    pub avail: String,
    #[serde(default)]
    pub refer: String,
    #[serde(default)]
    pub mountpoint: String,
    #[serde(default)]
    pub used_bytes: i64,
    #[serde(default)]
    pub mounted: bool,
}

/// An importable pool.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImportablePool {
    pub name: String,
    pub state: String,
    pub topology: String,
    #[serde(default)]
    pub disks: Vec<String>,
}

/// Structured pool health.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PoolHealth {
    pub name: String,
    pub state: String,
    pub scan: String,
    pub errors: String,
    #[serde(default)]
    pub config: Vec<PoolDevice>,
    #[serde(default, rename = "ioStats")]
    pub io_stats: PoolIoStats,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PoolDevice {
    pub name: String,
    pub state: String,
    pub read: String,
    pub write: String,
    pub cksum: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub devices: Vec<PoolDevice>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PoolIoStats {
    #[serde(default, rename = "readOps")]
    pub read_ops: String,
    #[serde(default, rename = "writeOps")]
    pub write_ops: String,
    #[serde(default, rename = "readBW")]
    pub read_bw: String,
    #[serde(default, rename = "writeBW")]
    pub write_bw: String,
}

/// The desired share configuration pushed to the node.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SharesConfigRequest {
    #[serde(rename = "sambaConf")]
    pub samba_conf: String,
    #[serde(rename = "ganeshaConf")]
    pub ganesha_conf: String,
    #[serde(rename = "sambaUsers")]
    pub samba_users: String,
    #[serde(rename = "nssPasswd")]
    pub nss_passwd: String,
    #[serde(rename = "nssGroup")]
    pub nss_group: String,
    #[serde(rename = "nssShadow")]
    pub nss_shadow: String,
    pub revision: String,
    #[serde(rename = "shareCount")]
    pub share_count: i32,
}

/// The agent's report of what it applied on the host.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SharesConfigStatus {
    pub applied: bool,
    pub revision: String,
    #[serde(rename = "sambaConfPath")]
    pub samba_conf_path: String,
    #[serde(rename = "ganeshaConfPath")]
    pub ganesha_conf_path: String,
    #[serde(rename = "smbShareCount")]
    pub smb_share_count: i32,
    #[serde(rename = "nfsExportCount")]
    pub nfs_export_count: i32,
    #[serde(default)]
    pub messages: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
}

/// A thin HTTP client for one agent endpoint.
pub struct Client {
    base_url: String,
    token: String,
    http: reqwest::Client,
}

impl Client {
    /// Build the base URL from `AGENT_BASE_URL`, else the in-cluster pattern
    /// using `NASLOS_NAMESPACE` (default `naslos`).
    pub fn base_url_from_env() -> String {
        if let Ok(v) = std::env::var("AGENT_BASE_URL") {
            if !v.is_empty() {
                return v.trim_end_matches('/').to_string();
            }
        }
        let ns = std::env::var("NASLOS_NAMESPACE").unwrap_or_else(|_| "naslos".to_string());
        DEFAULT_BASE_URL_PATTERN.replace("{}", &ns)
    }

    /// Build from the environment: base URL, `AGENT_TOKEN`, `AGENT_CA_FILE`.
    /// An unreadable or empty CA file is an error rather than a silent fallback
    /// to the system roots (fail closed, PF-M5).
    pub fn from_env() -> Result<Self, String> {
        let base = Self::base_url_from_env();
        let token = std::env::var("AGENT_TOKEN").unwrap_or_default();
        let ca_file = std::env::var("AGENT_CA_FILE").unwrap_or_default();
        Self::new(&base, &token, &ca_file)
    }

    /// Create a client for `base_url` with an optional pinned CA.
    pub fn new(base_url: &str, token: &str, ca_file: &str) -> Result<Self, String> {
        let mut builder = reqwest::Client::builder().timeout(DEFAULT_TIMEOUT);
        if !ca_file.is_empty() {
            let pem = std::fs::read(ca_file)
                .map_err(|e| format!("reading the agent CA {ca_file}: {e}"))?;
            let cert = reqwest::Certificate::from_pem(&pem).map_err(|e| {
                format!("the agent CA {ca_file} contains no usable certificate: {e}")
            })?;
            // Verify against that CA only.
            builder = builder
                .tls_built_in_root_certs(false)
                .add_root_certificate(cert);
        }
        let http = builder
            .build()
            .map_err(|e| format!("building the agent HTTP client: {e}"))?;
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            http,
        })
    }

    /// The bearer token is applied per request (so streaming paths cannot forget
    /// it).
    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let rb = self
            .http
            .request(method, format!("{}{}", self.base_url, path));
        if self.token.is_empty() {
            rb
        } else {
            rb.bearer_auth(&self.token)
        }
    }

    /// Perform a request, decode a JSON body into `T` on success. Agent errors
    /// are surfaced as `AgentError` with the upstream status.
    async fn do_json<T: serde::de::DeserializeOwned>(
        &self,
        rb: reqwest::RequestBuilder,
    ) -> Result<T, AgentError> {
        let resp = rb.send().await.map_err(|e| AgentError {
            status: 0,
            message: format!("contacting agent at {}: {e}", self.base_url),
        })?;
        let status = resp.status().as_u16();
        let body = resp.bytes().await.map_err(|e| AgentError {
            status: 0,
            message: format!("reading agent response: {e}"),
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
            return Err(AgentError {
                status,
                message: msg,
            });
        }

        if body.iter().all(|b| b.is_ascii_whitespace()) {
            return serde_json::from_slice(b"null").map_err(|e| AgentError {
                status: 0,
                message: format!("decoding agent response: {e}"),
            });
        }
        serde_json::from_slice(&body).map_err(|e| AgentError {
            status: 0,
            message: format!("decoding agent response: {e}"),
        })
    }

    /// A request whose successful response body is ignored.
    async fn do_empty(&self, rb: reqwest::RequestBuilder) -> Result<(), AgentError> {
        self.do_json::<serde_json::Value>(rb).await.map(|_| ())
    }

    // ---- Pools ----------------------------------------------------------

    pub async fn list_pools(&self) -> Result<Vec<Pool>, AgentError> {
        let pools: Vec<Pool> = self
            .do_json(self.request(reqwest::Method::GET, "/api/v1/pools"))
            .await?;
        Ok(pools)
    }

    pub async fn create_pool(&self, req: &CreatePoolRequest) -> Result<(), AgentError> {
        self.do_empty(
            self.request(reqwest::Method::POST, "/api/v1/pools")
                .json(req),
        )
        .await
    }

    pub async fn delete_pool(&self, name: &str) -> Result<(), AgentError> {
        self.do_empty(self.request(reqwest::Method::DELETE, &format!("/api/v1/pools/{name}")))
            .await
    }

    /// Raw `zpool status` output (`{"status": "..."}`).
    pub async fn pool_status(&self, name: &str) -> Result<String, AgentError> {
        let payload: std::collections::HashMap<String, String> = self
            .do_json(self.request(reqwest::Method::GET, &format!("/api/v1/pools/{name}")))
            .await?;
        Ok(payload.get("status").cloned().unwrap_or_default())
    }

    pub async fn pool_health(&self, name: &str) -> Result<PoolHealth, AgentError> {
        self.do_json(self.request(reqwest::Method::GET, &format!("/api/v1/pools/{name}")))
            .await
    }

    pub async fn list_importable_pools(&self) -> Result<Vec<ImportablePool>, AgentError> {
        let pools: Vec<ImportablePool> = self
            .do_json(self.request(reqwest::Method::GET, "/api/v1/pools/import"))
            .await?;
        Ok(pools)
    }

    pub async fn import_pool(&self, name: &str) -> Result<(), AgentError> {
        self.do_empty(
            self.request(reqwest::Method::POST, "/api/v1/pools/import")
                .json(&serde_json::json!({ "name": name })),
        )
        .await
    }

    pub async fn add_pool_vdev(
        &self,
        pool: &str,
        topology: &str,
        disks: &[String],
        force: bool,
    ) -> Result<(), AgentError> {
        self.do_empty(
            self.request(
                reqwest::Method::POST,
                &format!("/api/v1/pools/{pool}/devices"),
            )
            .json(&serde_json::json!({
                "disks": disks, "topology": topology, "force": force
            })),
        )
        .await
    }

    // ---- Datasets -------------------------------------------------------

    pub async fn list_datasets(&self) -> Result<Vec<Dataset>, AgentError> {
        let datasets: Vec<Dataset> = self
            .do_json(self.request(reqwest::Method::GET, "/api/v1/datasets"))
            .await?;
        Ok(datasets)
    }

    pub async fn create_dataset(
        &self,
        pool: &str,
        name: &str,
        options: &std::collections::HashMap<String, String>,
    ) -> Result<(), AgentError> {
        self.do_empty(
            self.request(
                reqwest::Method::POST,
                &format!("/api/v1/datasets/{}", escape_component(pool)),
            )
            .json(&serde_json::json!({ "name": name, "options": options })),
        )
        .await
    }

    pub async fn destroy_dataset(&self, name: &str, recursive: bool) -> Result<(), AgentError> {
        let mut path = format!("/api/v1/datasets/{}", escape_dataset_path(name));
        if recursive {
            path.push_str("?recursive=true");
        }
        self.do_empty(self.request(reqwest::Method::DELETE, &path))
            .await
    }

    // ---- Shares ---------------------------------------------------------

    pub async fn apply_shares_config(
        &self,
        req: &SharesConfigRequest,
    ) -> Result<SharesConfigStatus, AgentError> {
        self.do_json(
            self.request(reqwest::Method::PUT, "/api/v1/shares/config")
                .json(req),
        )
        .await
    }

    pub async fn get_shares_status(&self) -> Result<SharesConfigStatus, AgentError> {
        self.do_json(self.request(reqwest::Method::GET, "/api/v1/shares/status"))
            .await
    }

    pub async fn list_share_folders(&self, path: &str) -> Result<Vec<String>, AgentError> {
        let payload: FoldersPayload = self
            .do_json(self.request(
                reqwest::Method::GET,
                &format!("/api/v1/shares/folders?path={}", query_escape(path)),
            ))
            .await?;
        Ok(payload.folders)
    }

    pub async fn create_share_folder(&self, path: &str, name: &str) -> Result<String, AgentError> {
        let payload: std::collections::HashMap<String, String> = self
            .do_json(
                self.request(reqwest::Method::POST, "/api/v1/shares/folders")
                    .json(&serde_json::json!({ "path": path, "name": name })),
            )
            .await?;
        Ok(payload.get("path").cloned().unwrap_or_default())
    }

    pub async fn delete_share_folder(&self, path: &str) -> Result<(), AgentError> {
        self.do_empty(self.request(
            reqwest::Method::DELETE,
            &format!("/api/v1/shares/folders?path={}", query_escape(path)),
        ))
        .await
    }
}

#[derive(Deserialize)]
struct FoldersPayload {
    #[serde(default)]
    folders: Vec<String>,
}

/// Percent-escape one path component.
pub fn escape_component(s: &str) -> String {
    urlencode(s, false)
}

/// Escape each dataset component but keep the `/` separators.
pub fn escape_dataset_path(name: &str) -> String {
    name.split('/')
        .map(|p| urlencode(p, false))
        .collect::<Vec<_>>()
        .join("/")
}

fn query_escape(s: &str) -> String {
    urlencode(s, true)
}

/// Minimal RFC 3986 percent-encoding (no extra crate): unreserved chars pass,
/// everything else is `%XX`. In query mode a space becomes `+`.
fn urlencode(s: &str, query: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        let unreserved = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~');
        if unreserved {
            out.push(b as char);
        } else if query && b == b' ' {
            out.push('+');
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_dataset_components() {
        assert_eq!(escape_dataset_path("tank/media 2026"), "tank/media%202026");
        assert_eq!(escape_component("a/b"), "a%2Fb");
        assert_eq!(query_escape("tank/media 2026"), "tank%2Fmedia+2026");
    }

    #[test]
    fn base_url_uses_namespace() {
        // The pattern substitutes the namespace.
        let url = DEFAULT_BASE_URL_PATTERN.replace("{}", "naslos");
        assert_eq!(url, "https://naslos-agent.naslos.svc.cluster.local:9090");
    }
}
