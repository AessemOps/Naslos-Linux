//! The naslos-vmd HTTP API server. Same shape as the agent server (NAS-002):
//! `/health` is public, every `/api/` route is behind the token check.

pub mod auth;
pub mod error;
mod handlers;

use crate::model::DATASETS_ROOT;
use crate::supervisor::Supervisor;
use axum::routing::{delete, get, post, put};
use axum::{middleware, Router};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

/// In-flight imports and their last errors, keyed by image name.
#[derive(Default)]
pub struct ImportRegistry {
    pub active: std::collections::HashSet<String>,
    pub errors: HashMap<String, String>,
}

/// Shared state for every handler.
pub struct AppState {
    pub auth_token: String,
    pub supervisor: Arc<Supervisor>,
    pub imports: Arc<Mutex<ImportRegistry>>,
    /// Root the pool datasets are mounted under (`/var/mnt`).
    pub datasets_root: PathBuf,
}

/// Service options. `tls_*` empty means plain HTTP (local development).
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub auth_token: String,
    pub tls_cert_file: String,
    pub tls_key_file: String,
    /// Datasets root; empty means `/var/mnt`. Tests set a temp directory and
    /// pass the same root to the supervisor.
    pub datasets_root: PathBuf,
}

pub struct Server {
    addr: String,
    state: Arc<AppState>,
    tls_cert_file: String,
    tls_key_file: String,
}

impl Server {
    pub fn new(addr: impl Into<String>, supervisor: Arc<Supervisor>, opts: Options) -> Self {
        let datasets_root = if opts.datasets_root.as_os_str().is_empty() {
            PathBuf::from(DATASETS_ROOT)
        } else {
            opts.datasets_root
        };
        Self {
            addr: addr.into(),
            state: Arc::new(AppState {
                auth_token: opts.auth_token,
                supervisor,
                imports: Arc::new(Mutex::new(ImportRegistry::default())),
                datasets_root,
            }),
            tls_cert_file: opts.tls_cert_file,
            tls_key_file: opts.tls_key_file,
        }
    }

    /// Build the router. `/health` is public; every `/api/` route is behind
    /// the token check, so a new route added here is authenticated by default.
    pub fn router(&self) -> Router {
        let api = Router::new()
            .route("/api/v1/capabilities", get(handlers::capabilities_get))
            .route("/api/v1/vms", get(handlers::vms_get))
            .route("/api/v1/vms/config", put(handlers::vms_config_put))
            .route(
                "/api/v1/vms/{name}/force-stop",
                post(handlers::vm_force_stop_post),
            )
            .route("/api/v1/vms/{name}/vnc", get(handlers::vm_vnc_tunnel))
            .route("/api/v1/images", get(handlers::images_get))
            .route("/api/v1/images/upload", post(handlers::image_upload_post))
            .route("/api/v1/images/import", post(handlers::image_import_post))
            // DELETE only: a mutating GET (a link, a prefetch, a browser
            // retry) must not be able to delete a library image.
            .route("/api/v1/images/{name}", delete(handlers::image_delete))
            .layer(middleware::from_fn_with_state(
                self.state.clone(),
                auth::require_auth,
            ));

        Router::new()
            .route("/health", get(handlers::health))
            .merge(api)
            .with_state(self.state.clone())
    }

    /// Start the HTTP(S) server.
    pub async fn start(&self) -> anyhow::Result<()> {
        let addr = parse_addr(&self.addr)?;
        let tls = !self.tls_cert_file.is_empty() && !self.tls_key_file.is_empty();
        if tls {
            let config = tls_config(&self.tls_cert_file, &self.tls_key_file).await?;
            let mut server = axum_server::bind_rustls(addr, config);
            configure_http(server.http_builder());
            server.serve(self.router().into_make_service()).await?;
        } else {
            let mut server = axum_server::bind(addr);
            configure_http(server.http_builder());
            server.serve(self.router().into_make_service()).await?;
        }
        Ok(())
    }
}

/// Install the process-level rustls crypto provider (ring, like the API
/// crate). Load-bearing: the dependency graph enables both the ring and
/// aws-lc-rs providers (axum-server pulls one, reqwest the other), and rustls
/// 0.23 panics at the first config build when it must guess — the released
/// 0.4.1 binary died exactly there. `install_default` is idempotent.
pub fn ensure_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Build the TLS server config exactly as `start` does. Split out so the
/// provider install is part of the tested TLS path, not an easy-to-drop
/// line in `main`.
async fn tls_config(
    cert_file: &str,
    key_file: &str,
) -> anyhow::Result<axum_server::tls_rustls::RustlsConfig> {
    ensure_crypto_provider();
    Ok(axum_server::tls_rustls::RustlsConfig::from_pem_file(cert_file, key_file).await?)
}

/// Install a timer (axum-server sets none) and a header-read timeout, the
/// same posture as the agent server.
fn configure_http(
    builder: &mut hyper_util::server::conn::auto::Builder<hyper_util::rt::TokioExecutor>,
) {
    let mut http1 = builder.http1();
    http1.timer(hyper_util::rt::TokioTimer::new());
    http1.header_read_timeout(std::time::Duration::from_secs(10));
}

/// Parse `:9091` / `0.0.0.0:9091` / `127.0.0.1:9091`.
fn parse_addr(addr: &str) -> anyhow::Result<SocketAddr> {
    let normalized = match addr.strip_prefix(':') {
        Some(rest) => format!("0.0.0.0:{rest}"),
        None => addr.to_string(),
    };
    normalized
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid listen address {addr:?}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervisor::Supervisor;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use std::path::PathBuf;
    use tower::ServiceExt;

    #[test]
    fn tls_path_installs_a_process_crypto_provider() {
        // Regression for the released 0.4.1 binary: with both rustls crypto
        // providers compiled (axum-server pulls ring, reqwest pulls aws-lc-rs),
        // ServerConfig::builder() panics when no process provider is
        // installed. If tls_config ever stops installing one, this panics
        // again; with it, the config build resolves through the installed
        // provider. install_default is idempotent, so a provider installed by
        // an earlier test in this binary does not mask a dropped install.
        ensure_crypto_provider();
        let _ = rustls::ServerConfig::builder();
    }

    fn test_server() -> Server {
        let supervisor = Arc::new(Supervisor::with_datasets_root(
            false,
            1,
            16,
            PathBuf::from("/tmp/vmd-test"),
            PathBuf::from("/tmp/vmd-test-datasets"),
        ));
        Server::new(
            "127.0.0.1:0",
            supervisor,
            Options {
                auth_token: "token".into(),
                tls_cert_file: String::new(),
                tls_key_file: String::new(),
                datasets_root: PathBuf::new(),
            },
        )
    }

    #[tokio::test]
    async fn health_is_public() {
        let server = test_server();
        let response = server
            .router()
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn every_api_route_requires_the_token() {
        let server = test_server();
        for uri in [
            "/api/v1/capabilities",
            "/api/v1/vms",
            "/api/v1/vms/config",
            "/api/v1/vms/x/force-stop",
            "/api/v1/vms/x/vnc?port=5901",
            "/api/v1/images?pool=tank",
        ] {
            let response = server
                .router()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{uri} must be behind the token"
            );
        }
    }

    #[tokio::test]
    async fn the_token_authenticates() {
        let server = test_server();
        let response = server
            .router()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/vms")
                    .header("authorization", "Bearer token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let wrong = server
            .router()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/vms")
                    .header("authorization", "Bearer wrong")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn the_vnc_tunnel_proxies_bytes_to_the_configured_loopback_port() {
        use futures::{SinkExt, StreamExt};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        use tokio_tungstenite::tungstenite::Message as TunnelMessage;

        // A fake QEMU VNC listener that echoes the client's first bytes.
        let vnc = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let vnc_port = vnc.local_addr().unwrap().port();
        assert!(vnc_port >= 5900, "the test needs a display-range port");
        tokio::spawn(async move {
            let (mut stream, _) = vnc.accept().await.unwrap();
            let mut buf = [0u8; 12];
            stream.read_exact(&mut buf).await.unwrap();
            stream.write_all(&buf).await.unwrap();
        });

        let dir = tempfile::tempdir().unwrap();
        let vnc_display = vnc_port - 5900;
        let supervisor = Arc::new(Supervisor::with_datasets_root(
            false,
            vnc_display,
            1,
            dir.path().to_path_buf(),
            dir.path().join("datasets"),
        ));
        // The tunnel binds the VM name to its own display: register the
        // process that owns this port.
        supervisor
            .test_register_running("drill-vm", vnc_display)
            .await;
        let server = Server::new(
            "127.0.0.1:0",
            supervisor,
            Options {
                auth_token: "token".into(),
                tls_cert_file: String::new(),
                tls_key_file: String::new(),
                datasets_root: PathBuf::new(),
            },
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = server.router();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        let url = format!("ws://{addr}/api/v1/vms/drill-vm/vnc?port={vnc_port}");
        let mut request = url.as_str().into_client_request().unwrap();
        request
            .headers_mut()
            .insert("authorization", "Bearer token".parse().unwrap());
        let (mut ws, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        ws.send(TunnelMessage::Binary(b"RFB 003.008\n".to_vec().into()))
            .await
            .unwrap();
        let echoed = ws.next().await.unwrap().unwrap();
        assert_eq!(
            echoed,
            TunnelMessage::Binary(b"RFB 003.008\n".to_vec().into())
        );

        // A port outside the configured display range is refused.
        let outside = format!("ws://{addr}/api/v1/vms/drill-vm/vnc?port={}", vnc_port + 1);
        let mut request = outside.as_str().into_client_request().unwrap();
        request
            .headers_mut()
            .insert("authorization", "Bearer token".parse().unwrap());
        assert!(
            tokio_tungstenite::connect_async(request).await.is_err(),
            "a port outside the display range must be refused"
        );

        // A different VM name with a valid display-range port is refused: the
        // name must own the port, or the tunnel would proxy another console.
        let other = format!("ws://{addr}/api/v1/vms/other-vm/vnc?port={vnc_port}");
        let mut request = other.as_str().into_client_request().unwrap();
        request
            .headers_mut()
            .insert("authorization", "Bearer token".parse().unwrap());
        assert!(
            tokio_tungstenite::connect_async(request).await.is_err(),
            "a name that does not own the display must be refused"
        );
    }

    #[tokio::test]
    async fn deleting_an_image_is_delete_only() {
        let server = test_server();
        for method in ["GET", "POST"] {
            let response = server
                .router()
                .oneshot(
                    Request::builder()
                        .uri("/api/v1/images/a.iso?pool=tank")
                        .method(method)
                        .header("authorization", "Bearer token")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::METHOD_NOT_ALLOWED,
                "{method} must not delete an image"
            );
        }
        // DELETE is routed (the image does not exist, so it is a 400 and not a
        // 405: the mutating alias is gone).
        let response = server
            .router()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/images/a.iso?pool=tank")
                    .method("DELETE")
                    .header("authorization", "Bearer token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_bad_config_is_rejected_before_applying() {
        let server = test_server();
        let response = server
            .router()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/vms/config")
                    .method("PUT")
                    .header("authorization", "Bearer token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"revision":"r1","vms":[{"name":"UPPER","cpu":1,"memMiB":16,"disk":{"sizeGB":1}}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }
}
