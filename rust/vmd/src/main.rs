//! naslos-vmd entry point.

use clap::Parser;
use naslos_vmd::capabilities;
use naslos_vmd::server::{Options, Server};
use naslos_vmd::supervisor::{self, Supervisor};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser, Debug)]
#[command(name = "naslos-vmd", version, about = "Naslos QEMU/KVM supervisor")]
struct Cli {
    /// HTTP listen address for the vmd API.
    #[arg(long, default_value = ":9091")]
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

    // There is no opt-out: vmd is privileged and must prove callers are the
    // API (NAS-002 pattern).
    let auth_token = std::env::var("VMD_TOKEN").unwrap_or_default();
    if auth_token.is_empty() {
        eprintln!("VMD_TOKEN is required; mount it from the naslos-vmd Secret");
        std::process::exit(1);
    }

    // vmd is hostNetwork, so the bearer token must not cross the LAN in
    // cleartext. Both TLS vars must be set together or we refuse to start.
    let tls_cert = std::env::var("VMD_TLS_CERT").unwrap_or_default();
    let tls_key = std::env::var("VMD_TLS_KEY").unwrap_or_default();
    if (tls_cert.is_empty()) != (tls_key.is_empty()) {
        eprintln!("VMD_TLS_CERT and VMD_TLS_KEY must be set together");
        std::process::exit(1);
    }
    if tls_cert.is_empty() {
        // Fail closed like the agent: plaintext is an explicit, visible
        // choice made only by the chart (VMD_ALLOW_PLAINTEXT=1).
        if std::env::var("VMD_ALLOW_PLAINTEXT").as_deref() != Ok("1") {
            eprintln!(
                "VMD_TLS_CERT/VMD_TLS_KEY are unset and VMD_ALLOW_PLAINTEXT is not 1; refusing to serve the vmd token in cleartext"
            );
            std::process::exit(1);
        }
        tracing::warn!("vmd is serving plaintext (VMD_ALLOW_PLAINTEXT=1)");
    }

    let caps = capabilities::detect();
    tracing::info!(
        "naslos-vmd starting (kvm={}, nics={})",
        caps.kvm,
        caps.nics.join(",")
    );

    let vnc_first: u16 = std::env::var("VMD_VNC_FIRST_DISPLAY")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let vnc_count: u16 = std::env::var("VMD_VNC_DISPLAYS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(16);
    let run_dir =
        PathBuf::from(std::env::var("VMD_RUN_DIR").unwrap_or_else(|_| "/run/vmd".to_string()));
    let _ = std::fs::create_dir_all(&run_dir);

    let supervisor = Arc::new(Supervisor::new(caps.kvm, vnc_first, vnc_count, run_dir));
    tokio::spawn(supervisor::run_watchdog(supervisor.clone()));

    let server = Server::new(
        cli.listen.clone(),
        supervisor,
        Options {
            auth_token,
            tls_cert_file: tls_cert,
            tls_key_file: tls_key,
        },
    );

    tracing::info!("naslos-vmd listening on {}", cli.listen);

    tokio::select! {
        res = server.start() => {
            if let Err(e) = res {
                eprintln!("vmd server error: {e}");
                std::process::exit(1);
            }
        }
        _ = shutdown_signal() => {
            tracing::info!("Shutting down...");
        }
    }
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
