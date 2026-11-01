//! Child-process execution for the upgrader.
//!
//! Every command is bounded, runs with the scrubbed environment, and reports
//! the failing command without dumping secrets. The binaries are the same
//! pinned `helm`/`kubectl`/`talosctl` the API image ships.

use std::process::Stdio;
use std::time::Duration;

pub async fn run(binary: &str, args: &[String], timeout: Duration) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new(binary);
    crate::proc::scrub(&mut cmd);
    cmd.kill_on_drop(true);
    cmd.args(args);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let output = tokio::time::timeout(timeout, cmd.output())
        .await
        .map_err(|_| format!("{binary} {}: timed out after {timeout:?}", args.join(" ")))?
        .map_err(|e| format!("running {binary}: {e}"))?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        let err = err.trim();
        let err = if err.len() > 4096 { &err[..4096] } else { err };
        return Err(format!(
            "{binary} {} exited {}: {err}",
            args.join(" "),
            output.status.code().unwrap_or(-1)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub async fn run_str(binary: &str, args: &[&str], timeout: Duration) -> Result<String, String> {
    let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    run(binary, &owned, timeout).await
}

/// Run a command with stdin (server-side apply).
pub async fn run_stdin(
    binary: &str,
    args: &[String],
    stdin: &str,
    timeout: Duration,
) -> Result<String, String> {
    use tokio::io::AsyncWriteExt;

    let mut cmd = tokio::process::Command::new(binary);
    crate::proc::scrub(&mut cmd);
    cmd.kill_on_drop(true);
    cmd.args(args);
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| format!("running {binary}: {e}"))?;
    if let Some(mut si) = child.stdin.take() {
        si.write_all(stdin.as_bytes())
            .await
            .map_err(|e| format!("writing to {binary} stdin: {e}"))?;
        let _ = si.shutdown().await;
    }
    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| format!("{binary} {}: timed out after {timeout:?}", args.join(" ")))?
        .map_err(|e| format!("running {binary}: {e}"))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        let err = err.trim();
        let err = if err.len() > 4096 { &err[..4096] } else { err };
        return Err(format!(
            "{binary} {} exited {}: {err}",
            args.join(" "),
            output.status.code().unwrap_or(-1)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub fn kubectl() -> String {
    std::env::var("KUBECTL_BIN").unwrap_or_else(|_| "kubectl".to_string())
}

pub fn helm() -> String {
    std::env::var("HELM_BIN").unwrap_or_else(|_| "helm".to_string())
}

pub fn talosctl() -> String {
    std::env::var("TALOSCTL_BIN").unwrap_or_else(|_| "talosctl".to_string())
}

/// The API image's in-cluster kubeconfig (the Job's service account).
pub fn kubeconfig() -> Option<String> {
    crate::helm::resolve_api_kubeconfig().map(|p| p.display().to_string())
}
