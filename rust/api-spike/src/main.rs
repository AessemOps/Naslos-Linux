//! naslos-api-spike entry point.

use clap::Parser;
use naslos_api_spike::build_router;
use naslos_api_spike::state::AppState;
use std::sync::Arc;

#[derive(Parser, Debug)]
#[command(name = "naslos-api-spike", version, about = "Naslos API Rust spike")]
struct Cli {
    /// HTTP listen address.
    #[arg(long, default_value = ":8080")]
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

    let cli = Cli::parse();
    let state = match AppState::from_env() {
        Ok(state) => Arc::new(state),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    // Start the metrics collector so /api/dashboard has data immediately.
    let collector = state.clone();
    tokio::spawn(async move { collector.run_metrics_collector().await });

    let app = build_router(state.clone());

    let addr = parse_addr(&cli.listen);
    tracing::info!("naslos-api-spike listening on {}", cli.listen);
    let mut server = axum_server::bind(addr);
    let builder = server.http_builder();
    let mut http1 = builder.http1();
    http1.timer(hyper_util::rt::TokioTimer::new());
    http1.header_read_timeout(std::time::Duration::from_secs(10));
    if let Err(e) = server.serve(app.into_make_service()).await {
        eprintln!("server error: {e}");
        std::process::exit(1);
    }
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
