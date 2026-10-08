//! naslos-api entry point (port of `api/cmd/main.go`).

use clap::Parser;
use naslos_api::server::build_router;
use naslos_api::state::AppState;
use std::sync::Arc;

/// API server configuration.
#[derive(Parser, Debug)]
#[command(name = "naslos-api", version, about = "Naslos API")]
struct Cli {
    /// HTTP listen address.
    #[arg(long, default_value = ":8080")]
    listen: String,
    /// Path to kubeconfig (empty = in-cluster).
    #[arg(long, default_value = "")]
    kubeconfig: String,
    /// Path to talosconfig (empty = use default).
    #[arg(long, default_value = "")]
    talosconfig: String,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let state = match AppState::from_env() {
        Ok(state) => Arc::new(state),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    // Background metrics collector (port of startMetricsCollector).
    let collector = state.clone();
    tokio::spawn(async move { run_metrics_collector(collector).await });

    let app = build_router(state.clone());

    let addr = parse_addr(&cli.listen);
    tracing::info!("naslos-api listening on {}", cli.listen);
    let mut server = axum_server::bind(addr);
    let builder = server.http_builder();
    let mut http1 = builder.http1();
    http1.timer(hyper_util::rt::TokioTimer::new());
    http1.header_read_timeout(std::time::Duration::from_secs(10));

    tokio::select! {
        res = server.serve(app.into_make_service_with_connect_info::<std::net::SocketAddr>()) => {
            if let Err(e) = res {
                eprintln!("server error: {e}");
                std::process::exit(1);
            }
        }
        _ = shutdown_signal() => {
            tracing::info!("Shutting down...");
        }
    }
}

/// Poll the Talos node for metrics every 5s (override with
/// METRICS_INTERVAL_SECONDS), collect immediately so the dashboard is populated
/// on first load.
async fn run_metrics_collector(state: Arc<AppState>) {
    let Some(talos) = state.talos.clone() else {
        tracing::warn!("Metrics collector disabled: no Talos client available");
        return;
    };
    let interval = std::env::var("METRICS_INTERVAL_SECONDS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .map(std::time::Duration::from_secs)
        .unwrap_or(std::time::Duration::from_secs(5));

    collect_once(&state, &talos).await;
    tracing::info!("Metrics collector started (interval {:?})", interval);

    let mut ticker = tokio::time::interval(interval);
    ticker.tick().await;
    loop {
        ticker.tick().await;
        collect_once(&state, &talos).await;
    }
}

async fn collect_once(state: &AppState, talos: &naslos_api::talos::TalosClient) {
    let tm = talos.get_system_metrics().await;
    state.metrics.update(tm);
}

fn parse_addr(addr: &str) -> std::net::SocketAddr {
    let normalized = match addr.strip_prefix(':') {
        Some(rest) => format!("0.0.0.0:{rest}"),
        None => addr.to_string(),
    };
    normalized
        .parse()
        .unwrap_or_else(|_| "0.0.0.0:8080".parse().unwrap())
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
