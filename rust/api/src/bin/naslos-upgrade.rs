//! `naslos-upgrade` entry point. Runs inside the upgrader Job only.

use clap::Parser;
use naslos_api::upgrade::{run, Cli};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    if let Err(e) = run(cli).await {
        eprintln!("naslos-upgrade: {e}");
        std::process::exit(1);
    }
}
