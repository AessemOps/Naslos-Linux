//! The `naslos-upgrade` operation binary (spec FR-UPD, docs/installer-contract §7).
//!
//! It runs in an in-cluster Job, never inside the API pod: the API cannot
//! upgrade its own release (no write access to `naslos`, and its pod is
//! replaced mid-upgrade). One operation at a time is recorded in the
//! `naslos-system-operation` ConfigMap, which is also the lock.

pub mod commands;
pub mod ops;
pub mod overrides;
pub mod status;

use clap::{Parser, ValueEnum};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Operation {
    Naslos,
    Talos,
    Kubernetes,
}

#[derive(Debug, Parser)]
#[command(
    name = "naslos-upgrade",
    version,
    about = "Naslos system upgrade operation"
)]
pub struct Cli {
    #[arg(long, value_enum)]
    pub op: Operation,
    #[arg(long)]
    pub operation_id: String,
    /// The release being upgraded to (the pack's naslosVersion).
    #[arg(long)]
    pub version: String,
    /// The release currently running (recorded in the status).
    #[arg(long)]
    pub from: String,
    /// Download URL of the verified install pack.
    #[arg(long)]
    pub pack_url: String,
    /// sha256 the administrator approved; the download MUST match it.
    #[arg(long)]
    pub sha256: String,
    #[arg(long, default_value = "naslos")]
    pub namespace: String,
    #[arg(long, default_value = "naslos")]
    pub release: String,
    #[arg(long, default_value = "/var/run/secrets/talos.dev/config")]
    pub talosconfig: String,
    #[arg(long, default_value = "/var/lib/naslos")]
    pub state_dir: String,
}

pub async fn run(cli: Cli) -> Result<(), String> {
    let mut writer = status::Writer::new(
        &cli.namespace,
        &cli.state_dir,
        &cli.operation_id,
        cli.op,
        &cli.from,
        &cli.version,
    );
    if let Some(conflict) = writer.load_or_init().await {
        return Err(conflict);
    }
    // A Job retry after a terminal state must not repeat the operation.
    match writer.current_phase() {
        "succeeded" => return Ok(()),
        "failed" => return Err("the operation already failed".into()),
        _ => {}
    }
    let result = match cli.op {
        Operation::Naslos => ops::naslos(&cli, &mut writer).await,
        Operation::Talos => ops::talos(&cli, &mut writer).await,
        Operation::Kubernetes => ops::kubernetes(&cli, &mut writer).await,
    };
    match result {
        Ok(message) => {
            writer.succeeded(&message).await;
            Ok(())
        }
        Err(e) => {
            writer.failed(&e).await;
            Err(e)
        }
    }
}
