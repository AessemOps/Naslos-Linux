//! naslos-agent entry point (port of `agent/cmd/main.go`).

use clap::Parser;
use naslos_agent::server::{Options, Server};
use naslos_agent::shares::{self, SharesClient};
use naslos_agent::zfs::{RealRunner, ZfsClient};
use std::path::Path;
use std::sync::Arc;

const HOST_ROOT: &str = "/host";

#[derive(Parser, Debug)]
#[command(name = "naslos-agent", version, about = "Naslos privileged ZFS agent")]
struct Cli {
    /// Node name this agent runs on.
    #[arg(long, env = "NODE_NAME", default_value = "")]
    node: String,

    /// HTTP listen address for the agent API.
    #[arg(long, default_value = ":9090")]
    listen: String,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse_from(normalize_args(std::env::args().collect()));

    tracing::info!("naslos-agent starting on node {}", cli.node);

    let runner = Arc::new(RealRunner::new(HOST_ROOT));
    let client = Arc::new(ZfsClient::new(runner, HOST_ROOT));

    // ZFS is optional: on nodes without the extension the agent starts in
    // degraded mode and its ZFS endpoints return 503 instead of crash-looping.
    let zfs: Option<Arc<ZfsClient>> = if !client.is_zfs_available() {
        tracing::warn!(
            "ZFS not available on host — starting in degraded mode (ZFS endpoints return 503)"
        );
        None
    } else {
        // Import any existing pools (idempotent).
        if let Err(e) = client.import_pool("").await {
            tracing::info!("Note: zpool import returned: {e}");
        }
        Some(client)
    };

    // Share configuration needs the host root mounted at /host.
    let shares_client: Option<Arc<SharesClient>> = if !shares::is_available(Path::new(HOST_ROOT)) {
        tracing::warn!(
            "host root not available at /host — share configuration endpoints return 503"
        );
        None
    } else {
        Some(Arc::new(SharesClient::new(HOST_ROOT)))
    };

    // There is no opt-out: the agent is privileged and must prove callers are
    // the API.
    let auth_token = std::env::var("AGENT_TOKEN").unwrap_or_default();
    if auth_token.is_empty() {
        eprintln!("AGENT_TOKEN is required; mount it from the naslos-agent Secret");
        std::process::exit(1);
    }

    // The agent is hostNetwork, so the bearer token must not cross the LAN in
    // cleartext. Both TLS vars must be set together or we refuse to start.
    let tls_cert = std::env::var("AGENT_TLS_CERT").unwrap_or_default();
    let tls_key = std::env::var("AGENT_TLS_KEY").unwrap_or_default();
    if (tls_cert.is_empty()) != (tls_key.is_empty()) {
        eprintln!("AGENT_TLS_CERT and AGENT_TLS_KEY must be set together");
        std::process::exit(1);
    }

    let server = Server::new(
        cli.listen.clone(),
        zfs,
        shares_client,
        Options {
            auth_token,
            tls_cert_file: tls_cert,
            tls_key_file: tls_key,
        },
    );

    tracing::info!("naslos-agent listening on {}", cli.listen);

    tokio::select! {
        res = server.start() => {
            if let Err(e) = res {
                eprintln!("Agent server error: {e}");
                std::process::exit(1);
            }
        }
        _ = shutdown_signal() => {
            tracing::info!("Shutting down...");
        }
    }
}

/// Accept Go's single-dash long flags (`-listen`, `-node`) as well as `--…`.
fn normalize_args(mut argv: Vec<String>) -> Vec<String> {
    for arg in argv.iter_mut().skip(1) {
        match arg.as_str() {
            "-listen" => *arg = "--listen".to_string(),
            "-node" => *arg = "--node".to_string(),
            _ => {}
        }
    }
    argv
}

async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(_) => {
            let _ = tokio::signal::ctrl_c().await;
            return;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}
