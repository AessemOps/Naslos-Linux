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
        let err = stderr_tail(err, 4096);
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
        let err = stderr_tail(err, 4096);
        return Err(format!(
            "{binary} {} exited {}: {err}",
            args.join(" "),
            output.status.code().unwrap_or(-1)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Keep the end of a long stderr. The actionable failure follows the warnings
/// (a helm upgrade prints a PodSecurity warning per workload first), so the
/// head-only truncation used to hide the real cause behind 4 KiB of warnings.
fn stderr_tail(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn run_returns_stdout() {
        let out = run_str("sh", &["-c", "printf hello"], Duration::from_secs(5))
            .await
            .expect("printf hello must succeed");
        assert_eq!(out, "hello", "stdout must be captured verbatim");
    }

    #[tokio::test]
    async fn run_reports_exit_status_and_stderr() {
        let err = run_str(
            "sh",
            &["-c", "echo boom >&2; exit 7"],
            Duration::from_secs(5),
        )
        .await
        .expect_err("a non-zero exit must be an error");
        assert!(
            err.contains("exited 7"),
            "error must carry the exit code, got: {err}"
        );
        assert!(
            err.contains("boom"),
            "error must carry the trimmed stderr, got: {err}"
        );
    }

    #[tokio::test]
    async fn run_reports_a_missing_binary() {
        let err = run_str(
            "naslos-definitely-missing-binary",
            &[],
            Duration::from_secs(5),
        )
        .await
        .expect_err("a missing binary must be an error");
        assert!(
            err.contains("running naslos-definitely-missing-binary"),
            "spawn failure must name the binary, got: {err}"
        );
    }

    #[tokio::test]
    async fn run_times_out() {
        let err = run_str("sh", &["-c", "sleep 5"], Duration::from_millis(100))
            .await
            .expect_err("a command exceeding the timeout must be an error");
        assert!(
            err.contains("timed out after"),
            "timeout error must be reported, got: {err}"
        );
    }

    #[tokio::test]
    async fn run_stdin_feeds_the_child() {
        let args: Vec<String> = vec!["-c".into(), "cat".into()];
        let out = run_stdin("sh", &args, "payload", Duration::from_secs(5))
            .await
            .expect("cat must echo its stdin");
        assert_eq!(out, "payload", "stdin must reach the child");
    }

    #[tokio::test]
    async fn stderr_is_truncated_to_four_kib() {
        let err = run_str(
            "sh",
            &[
                "-c",
                "head -c 5000 /dev/zero | tr \"\\0\" \"\\170\" >&2; exit 1",
            ],
            Duration::from_secs(5),
        )
        .await
        .expect_err("the forced exit 1 must be an error");
        let (_, captured) = err
            .split_once("exited 1: ")
            .expect("error must report the exit status before the captured stderr");
        assert_eq!(
            captured.matches('x').count(),
            4096,
            "captured stderr must be truncated to exactly 4096 x chars, got {} (message len {})",
            captured.matches('x').count(),
            err.len()
        );
        assert!(
            !err.contains(&"x".repeat(4097)),
            "error must not contain a run of more than 4096 x chars"
        );
    }

    #[tokio::test]
    async fn run_stdin_reports_a_missing_binary() {
        let err = run_stdin(
            "naslos-definitely-missing-binary",
            &[],
            "payload",
            Duration::from_secs(5),
        )
        .await
        .expect_err("a missing binary must be a spawn error");
        assert!(
            err.contains("running"),
            "spawn failure must be reported, got: {err}"
        );
    }

    #[test]
    fn stderr_truncation_keeps_the_failure_at_the_end() {
        let mut text = "warnings.go: would violate PodSecurity\n".repeat(200);
        text.push_str("Error: conflict occurred while applying object");
        let tail = stderr_tail(&text, 256);
        assert!(tail.len() <= 256);
        assert!(tail.contains("conflict occurred"), "{tail}");
        assert!(!text.starts_with(tail));
    }

    #[test]
    fn short_stderr_is_unchanged() {
        assert_eq!(stderr_tail("boom", 4096), "boom");
    }
}
