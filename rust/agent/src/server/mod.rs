//! The naslos-agent HTTP API server (port of `internal/server`).

pub mod auth;
pub mod error;
mod handlers;

use crate::shares::SharesClient;
use crate::zfs::ZfsClient;
use axum::routing::{get, post, put};
use axum::{middleware, Router};
use std::net::SocketAddr;
use std::sync::Arc;

/// Authentication options. `tls_*` empty means plain HTTP (local development).
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub auth_token: String,
    pub tls_cert_file: String,
    pub tls_key_file: String,
}

/// Shared state for every handler.
pub struct AppState {
    pub zfs: Option<Arc<ZfsClient>>,
    pub shares: Option<Arc<SharesClient>>,
    pub auth_token: String,
}

/// The agent's HTTP server.
pub struct Server {
    addr: String,
    state: Arc<AppState>,
    tls_cert_file: String,
    tls_key_file: String,
}

impl Server {
    pub fn new(
        addr: impl Into<String>,
        zfs: Option<Arc<ZfsClient>>,
        shares: Option<Arc<SharesClient>>,
        opts: Options,
    ) -> Self {
        Self {
            addr: addr.into(),
            state: Arc::new(AppState {
                zfs,
                shares,
                auth_token: opts.auth_token,
            }),
            tls_cert_file: opts.tls_cert_file,
            tls_key_file: opts.tls_key_file,
        }
    }

    /// Build the router. `/health` is public; every `/api/` route is behind the
    /// token check, so a new route added to the API router is authenticated by
    /// default.
    pub fn router(&self) -> Router {
        let api = Router::new()
            .route(
                "/api/v1/pools",
                get(handlers::pools_get).post(handlers::pools_post),
            )
            .route(
                "/api/v1/pools/import",
                get(handlers::pool_import_get).post(handlers::pool_import_post),
            )
            .route(
                "/api/v1/pools/{pool}/devices",
                get(handlers::pool_devices_get).post(handlers::pool_devices_post),
            )
            .route(
                "/api/v1/pools/{pool}",
                get(handlers::pool_detail_get).delete(handlers::pool_detail_delete),
            )
            .route("/api/v1/datasets", get(handlers::datasets_all_get))
            .route(
                "/api/v1/datasets/{*rest}",
                get(handlers::datasets_get)
                    .post(handlers::datasets_post)
                    .delete(handlers::datasets_delete),
            )
            .route(
                "/api/v1/snapshots/{*dataset}",
                get(handlers::snapshots_get).post(handlers::snapshots_post),
            )
            .route(
                "/api/v1/shares/config",
                put(handlers::shares_config).post(handlers::shares_config),
            )
            .route("/api/v1/shares/status", get(handlers::shares_status))
            .route(
                "/api/v1/shares/folders",
                get(handlers::shares_folders_get)
                    .post(handlers::shares_folders_post)
                    .delete(handlers::shares_folders_delete),
            )
            .route("/api/v1/zfs/send/{*dataset}", get(handlers::zfs_send))
            .route(
                "/api/v1/zfs/receive/{*dataset}",
                post(handlers::zfs_receive),
            )
            .route(
                "/api/v1/zfs/snapshots/{*dataset}",
                get(handlers::zfs_snapshots),
            )
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
        let app = self.router().into_make_service();

        if !self.tls_cert_file.is_empty() && !self.tls_key_file.is_empty() {
            let config = axum_server::tls_rustls::RustlsConfig::from_pem_file(
                &self.tls_cert_file,
                &self.tls_key_file,
            )
            .await?;
            axum_server::bind_rustls(addr, config).serve(app).await?;
        } else {
            axum_server::bind(addr).serve(app).await?;
        }
        Ok(())
    }
}

/// Parse `:9090` / `0.0.0.0:9090` / `127.0.0.1:9090`.
fn parse_addr(addr: &str) -> anyhow::Result<SocketAddr> {
    let normalized = match addr.strip_prefix(':') {
        Some(rest) => format!("0.0.0.0:{rest}"),
        None => addr.to_string(),
    };
    normalized
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid listen address {addr:?}: {e}"))
}
