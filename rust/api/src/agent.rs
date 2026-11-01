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

/// Total deadline for ordinary (JSON) agent calls. Covers slow operations
/// (wipefs + `zpool create` on real spinning disks).
pub const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

/// Connection deadline shared by both clients. A host that accepts nothing
/// must fail fast instead of consuming the whole stream budget.
pub const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Idle deadline for `zfs send`/`receive` bodies: the longest gap tolerated
/// between two body chunks. It is **not** a total deadline (SEC-2026-D9): a
/// multi-hour backup that keeps making progress must keep running.
pub const DEFAULT_STREAM_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Env override for the stream idle deadline, in seconds.
pub const STREAM_IDLE_TIMEOUT_ENV: &str = "AGENT_STREAM_IDLE_TIMEOUT_SECS";

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

/// SMART data for one whole disk (mirrors the agent's `SmartInfo`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SmartInfo {
    #[serde(default)]
    pub available: bool,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub health: String,
    #[serde(default, rename = "temperatureC")]
    pub temperature_c: Option<i64>,
    #[serde(default, rename = "powerOnHours")]
    pub power_on_hours: Option<u64>,
    #[serde(default, rename = "wearPercent")]
    pub wear_percent: Option<u8>,
    #[serde(default, rename = "reallocatedSectors")]
    pub reallocated_sectors: Option<u64>,
    #[serde(default, rename = "pendingSectors")]
    pub pending_sectors: Option<u64>,
    #[serde(default, rename = "uncorrectableSectors")]
    pub uncorrectable_sectors: Option<u64>,
    #[serde(default, rename = "mediaErrors")]
    pub media_errors: Option<u64>,
    #[serde(default, rename = "errorCount")]
    pub error_count: Option<u64>,
    #[serde(default)]
    pub standby: bool,
    #[serde(default)]
    pub message: String,
}

/// A disk's ZFS vdev state/error counters (mirrors the agent's
/// `ZfsDiskHealth`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ZfsDiskHealth {
    pub pool: String,
    pub vdev: String,
    pub state: String,
    #[serde(rename = "readErrors")]
    pub read_errors: u64,
    #[serde(rename = "writeErrors")]
    pub write_errors: u64,
    #[serde(rename = "cksumErrors")]
    pub cksum_errors: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiskHealth {
    pub device: String,
    #[serde(default)]
    pub smart: SmartInfo,
    #[serde(default)]
    pub zfs: Option<ZfsDiskHealth>,
}

/// The agent's `/api/v1/disks/health` payload.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiskHealthReport {
    #[serde(rename = "smartctlAvailable")]
    pub smartctl_available: bool,
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub disks: Vec<DiskHealth>,
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

/// Resolve the stream idle deadline from `AGENT_STREAM_IDLE_TIMEOUT_SECS`.
/// A missing, unparsable, or zero value falls back to the default (and warns
/// for the latter two, so a typo does not silently disable the guard).
fn stream_idle_timeout_from_env() -> std::time::Duration {
    let Ok(raw) = std::env::var(STREAM_IDLE_TIMEOUT_ENV) else {
        return DEFAULT_STREAM_IDLE_TIMEOUT;
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return DEFAULT_STREAM_IDLE_TIMEOUT;
    }
    match raw.parse::<u64>() {
        Ok(secs) if secs > 0 => std::time::Duration::from_secs(secs),
        _ => {
            tracing::warn!(
                "{STREAM_IDLE_TIMEOUT_ENV}={raw:?} is not a positive integer; using {}s",
                DEFAULT_STREAM_IDLE_TIMEOUT.as_secs()
            );
            DEFAULT_STREAM_IDLE_TIMEOUT
        }
    }
}

/// A thin HTTP client for one agent endpoint.
pub struct Client {
    base_url: String,
    token: String,
    /// Ordinary calls: a total deadline (`DEFAULT_TIMEOUT`).
    http: reqwest::Client,
    /// Streaming calls: no total deadline, only connect + per-read idle
    /// (`DEFAULT_STREAM_IDLE_TIMEOUT`). Using `http` for a long `zfs send`
    /// silently truncated the backup after 180 s.
    stream_http: reqwest::Client,
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

    /// Create a client for `base_url` with a pinned CA. An HTTPS agent URL
    /// without a CA is refused: falling back to the system roots would accept
    /// any publicly trusted certificate for the in-cluster name (PF-M5).
    pub fn new(base_url: &str, token: &str, ca_file: &str) -> Result<Self, String> {
        Self::with_timeouts(
            base_url,
            token,
            ca_file,
            DEFAULT_TIMEOUT,
            stream_idle_timeout_from_env(),
        )
    }

    /// Build both clients with explicit deadlines. Public so tests can use
    /// short values; production goes through `new`/`from_env`.
    pub fn with_timeouts(
        base_url: &str,
        token: &str,
        ca_file: &str,
        total: std::time::Duration,
        stream_idle: std::time::Duration,
    ) -> Result<Self, String> {
        if base_url.starts_with("https://") && ca_file.trim().is_empty() {
            return Err(
                "AGENT_CA_FILE is required for an https agent URL; refusing to use the system roots"
                    .to_string(),
            );
        }
        let http = Self::builder(ca_file)?
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(total)
            .build()
            .map_err(|e| format!("building the agent HTTP client: {e}"))?;
        // No `.timeout()` here: reqwest applies a total deadline to the body
        // too, which would cut long `zfs send`/`receive` streams. `read_timeout`
        // is a per-read idle deadline, so slow-but-progressing backups survive
        // and a stalled peer still fails.
        let stream_http = Self::builder(ca_file)?
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(stream_idle)
            .build()
            .map_err(|e| format!("building the agent streaming HTTP client: {e}"))?;
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            http,
            stream_http,
        })
    }

    /// Shared TLS setup: a pinned CA, never the system roots when a CA is
    /// supplied, and no system roots at all for the agent identity.
    fn builder(ca_file: &str) -> Result<reqwest::ClientBuilder, String> {
        let mut builder = reqwest::Client::builder();
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
        Ok(builder)
    }

    /// The bearer token is applied per request (so streaming paths cannot forget
    /// it).
    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.authorize(self.http.request(method, self.url(path)))
    }

    /// Like `request`, but on the streaming client (idle deadline only).
    fn stream_request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.authorize(self.stream_http.request(method, self.url(path)))
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn authorize(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
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

    /// `force` maps to `zpool destroy -f`; the agent also requires
    /// `confirm` to repeat the pool name.
    pub async fn delete_pool(&self, name: &str, force: bool) -> Result<(), AgentError> {
        let path = format!("/api/v1/pools/{name}?confirm={name}&force={force}");
        self.do_empty(self.request(reqwest::Method::DELETE, &path))
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

    /// Per-disk SMART + ZFS health. The agent returns 200 with
    /// `smartctlAvailable:false` when SMART is unsupported, so this is only an
    /// error when the agent itself is unreachable or degraded.
    pub async fn disk_health(&self) -> Result<DiskHealthReport, AgentError> {
        self.do_json(self.request(reqwest::Method::GET, "/api/v1/disks/health"))
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
    fn https_without_a_ca_is_refused() {
        let err = match Client::new("https://naslos-agent.naslos.svc:9090", "t", "") {
            Ok(_) => panic!("an https URL without a CA must be refused"),
            Err(e) => e,
        };
        assert!(err.contains("AGENT_CA_FILE"), "{err}");
        assert!(Client::new("http://127.0.0.1:9090", "t", "").is_ok());
    }

    #[test]
    fn base_url_uses_namespace() {
        // The pattern substitutes the namespace.
        let url = DEFAULT_BASE_URL_PATTERN.replace("{}", "naslos");
        assert_eq!(url, "https://naslos-agent.naslos.svc.cluster.local:9090");
    }

    // ---- Streaming deadlines (W1) --------------------------------------
    //
    // Regression: both streaming methods used the ordinary client, whose
    // `.timeout()` is a **total** deadline including the body. Any `zfs send`
    // or `receive` longer than 180 s was therefore cut mid-transfer.

    use axum::body::Body;
    use axum::routing::get;
    use axum::Router;
    use futures::StreamExt;
    use std::time::Duration;
    use tokio::net::TcpListener;

    /// A mock agent whose send body emits `chunks` one-byte chunks `gap_ms`
    /// apart, stalling for five seconds after chunk `stall_after`.
    async fn spawn_stream_agent(chunks: usize, gap_ms: u64, stall_after: usize) -> String {
        let app = Router::new()
            .route(
                "/api/v1/zfs/send/{*dataset}",
                get(move || async move {
                    let body = futures::stream::unfold(0usize, move |i| async move {
                        if i >= chunks {
                            return None;
                        }
                        if i == stall_after {
                            tokio::time::sleep(Duration::from_secs(5)).await;
                        } else {
                            tokio::time::sleep(Duration::from_millis(gap_ms)).await;
                        }
                        Some((
                            Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"x")),
                            i + 1,
                        ))
                    });
                    Body::from_stream(body)
                }),
            )
            .route(
                "/api/v1/system/disk",
                get(|| async {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    axum::Json(serde_json::json!({ "total": 1 }))
                }),
            );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    fn send_opts() -> SendStreamOptions {
        SendStreamOptions {
            dataset: "tank/data".into(),
            to: "snap".into(),
            from: String::new(),
            raw: false,
        }
    }

    /// The stream client ignores the total deadline: with a 50 ms total (the
    /// old behaviour would abort here) a 500 ms body still completes.
    #[tokio::test]
    async fn send_stream_is_not_bounded_by_the_total_timeout() {
        let base = spawn_stream_agent(5, 100, usize::MAX).await;
        let client = Client::with_timeouts(
            &base,
            "token",
            "",
            Duration::from_millis(50),
            Duration::from_secs(2),
        )
        .unwrap();

        let stream = client.send_stream(&send_opts()).await.unwrap();
        let mut chunks = 0usize;
        let mut stream = stream;
        while let Some(item) = stream.next().await {
            item.expect("no chunk error");
            chunks += 1;
        }
        assert_eq!(chunks, 5, "the whole body must arrive");
    }

    /// A stalled body still fails: the idle deadline is a real guard.
    #[tokio::test]
    async fn send_stream_fails_when_the_body_stalls() {
        let base = spawn_stream_agent(5, 10, 1).await;
        let client = Client::with_timeouts(
            &base,
            "token",
            "",
            Duration::from_secs(30),
            Duration::from_millis(100),
        )
        .unwrap();

        let stream = client.send_stream(&send_opts()).await.unwrap();
        let outcome = tokio::time::timeout(Duration::from_millis(900), async move {
            let mut stream = stream;
            let mut chunks = 0usize;
            while let Some(item) = stream.next().await {
                if item.is_err() {
                    return Err(());
                }
                chunks += 1;
            }
            Ok(chunks)
        })
        .await
        .expect("the idle deadline must fire, not hang");

        assert!(outcome.is_err(), "a stalled stream must surface an error");
    }

    /// Ordinary calls keep the total deadline.
    #[tokio::test]
    async fn json_calls_still_use_the_total_timeout() {
        let base = spawn_stream_agent(0, 0, usize::MAX).await;
        let client = Client::with_timeouts(
            &base,
            "token",
            "",
            Duration::from_millis(100),
            Duration::from_secs(5),
        )
        .unwrap();

        let started = std::time::Instant::now();
        let err = client.get_disk_usage().await.expect_err("must time out");
        assert_eq!(err.status, 0, "transport-level failure");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the 100 ms total deadline did not fire: {:?}",
            started.elapsed()
        );
    }
}

// ---- Streaming (buddy send/restore) -------------------------------------

/// A snapshot's identity, from the agent's `/zfs/snapshots`.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SnapshotInfo {
    pub name: String,
    #[serde(default)]
    pub guid: String,
    #[serde(default)]
    pub created: String,
}

/// Options for a `zfs send`.
#[derive(Debug, Clone, Default)]
pub struct SendStreamOptions {
    pub dataset: String,
    pub to: String,
    pub from: String,
    pub raw: bool,
}

fn send_path(opts: &SendStreamOptions) -> String {
    let mut path = format!(
        "/api/v1/zfs/send/{}?to={}",
        escape_dataset_path(&opts.dataset),
        urlencode(&opts.to, true)
    );
    if !opts.from.is_empty() {
        path.push_str(&format!("&from={}", urlencode(&opts.from, true)));
    }
    path.push_str(if opts.raw { "&raw=true" } else { "&raw=false" });
    path
}

impl Client {
    /// Open a `zfs send` stream as an async reader.
    pub async fn send_stream(
        &self,
        opts: &SendStreamOptions,
    ) -> Result<futures::stream::BoxStream<'static, Result<bytes::Bytes, std::io::Error>>, AgentError>
    {
        let resp = self
            .stream_request(reqwest::Method::GET, &send_path(opts))
            .send()
            .await
            .map_err(|e| AgentError {
                status: 0,
                message: format!("starting zfs send: {e}"),
            })?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let body = resp.bytes().await.unwrap_or_default();
            return Err(AgentError {
                status,
                message: String::from_utf8_lossy(&body).trim().to_string(),
            });
        }
        use futures::StreamExt;
        Ok(resp
            .bytes_stream()
            .map(|r| r.map_err(std::io::Error::other))
            .boxed())
    }

    /// Ask the agent to estimate the stream size (`zfs send -nP`).
    pub async fn estimate_send(&self, opts: &SendStreamOptions) -> Result<i64, AgentError> {
        let path = format!("{}&estimate=true", send_path(opts));
        let payload: serde_json::Value = self
            .do_json(self.request(reqwest::Method::GET, &path))
            .await?;
        Ok(payload.get("bytes").and_then(|v| v.as_i64()).unwrap_or(0))
    }

    /// A dataset's snapshots with GUIDs.
    pub async fn snapshots_with_guid(
        &self,
        dataset: &str,
    ) -> Result<Vec<SnapshotInfo>, AgentError> {
        let path = format!("/api/v1/zfs/snapshots/{}", escape_dataset_path(dataset));
        let list: Vec<SnapshotInfo> = self
            .do_json(self.request(reqwest::Method::GET, &path))
            .await?;
        Ok(list)
    }

    /// Create a snapshot on the host.
    pub async fn create_snapshot(&self, dataset: &str, name: &str) -> Result<(), AgentError> {
        let path = format!("/api/v1/snapshots/{}", escape_dataset_path(dataset));
        self.do_empty(
            self.request(reqwest::Method::POST, &path)
                .json(&serde_json::json!({ "name": name })),
        )
        .await
    }

    /// Stream a restore body into `zfs receive`. The reader is consumed as the
    /// request body; the call resolves when the agent finishes the receive.
    pub async fn receive_stream_body(
        &self,
        dataset: &str,
        force: bool,
        reader: impl tokio::io::AsyncRead + Send + 'static,
    ) -> Result<(), AgentError> {
        let path = format!(
            "/api/v1/zfs/receive/{}?force={}",
            escape_dataset_path(dataset),
            force
        );
        let stream = tokio_util::io::ReaderStream::new(reader);
        let body = reqwest::Body::wrap_stream(stream);
        let resp = self
            .stream_request(reqwest::Method::POST, &path)
            .header("Content-Type", "application/octet-stream")
            .body(body)
            .send()
            .await
            .map_err(|e| AgentError {
                status: 0,
                message: format!("zfs receive: {e}"),
            })?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let body = resp.bytes().await.unwrap_or_default();
            return Err(AgentError {
                status,
                message: String::from_utf8_lossy(&body).trim().to_string(),
            });
        }
        Ok(())
    }
}

/// Host filesystem usage, reported by the agent (it can statvfs the host).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct DiskUsage {
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub free: u64,
    #[serde(default)]
    pub used: u64,
    #[serde(default, rename = "usagePercent")]
    pub usage_percent: f64,
}

impl Client {
    /// The host EPHEMERAL partition's usage.
    pub async fn get_disk_usage(&self) -> Result<DiskUsage, AgentError> {
        self.do_json(self.request(reqwest::Method::GET, "/api/v1/system/disk"))
            .await
    }
}
