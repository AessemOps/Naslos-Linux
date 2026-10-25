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

    // Dynamic-DNS reconcile loop (port of ddns.Manager.Start).
    if let Some(ddns) = state.ddns.clone() {
        tokio::spawn(async move { ddns.run_loop().await });
    }

    // Re-apply Traefik routing for every installed app (FR-APP-11). This was
    // never called after the Rust port, so a restart could leave an installed
    // app unrouted (SEC-2026-H9).
    if let Some(manager) = state.app_manager.clone() {
        tokio::spawn(async move {
            if let Err(e) = manager.reconcile_routes().await {
                tracing::warn!("reconciling app routes at startup: {e}");
            }
        });
    }

    // Scheduled buddy backups: catch-up + interval.
    {
        let scheduler_state = state.clone();
        tokio::spawn(async move {
            naslos_api::server::buddy_schedules::run_scheduler(scheduler_state).await
        });
    }

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
    let mut tm = talos.get_system_metrics().await;
    // The host filesystem's usage is only visible from the agent (the CLI's
    // `mountstatus` has no size/available); the Go API used the Talos Mounts API.
    if let Some(agent) = &state.agent {
        if let Ok(d) = agent.get_disk_usage().await {
            tm.disk = naslos_api::metrics::DiskMetrics {
                total: d.total,
                used: d.used,
                free: d.free,
                usage_percent: d.usage_percent,
            };
        }
        // The dashboard's `zfs.pools` was never populated after the Rust port
        // (SEC-2026-H7); the agent has the live pool list.
        if let Ok(pools) = agent.list_pools().await {
            tm.zfs.pools = Some(
                pools
                    .into_iter()
                    .map(|p| {
                        let size = naslos_api::metrics::parse_zfs_size(&p.size).unwrap_or(0);
                        let alloc = naslos_api::metrics::parse_zfs_size(&p.alloc).unwrap_or(0);
                        let free = naslos_api::metrics::parse_zfs_size(&p.free).unwrap_or(0);
                        let usage_percent = if size > 0 {
                            (alloc as f64 / size as f64) * 100.0
                        } else {
                            0.0
                        };
                        naslos_api::metrics::PoolMetrics {
                            name: p.name,
                            size,
                            alloc,
                            free,
                            usage_percent,
                            health: p.health,
                        }
                    })
                    .collect(),
            );
        }
    }
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
