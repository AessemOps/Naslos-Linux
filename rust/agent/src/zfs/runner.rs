//! Host command execution.
//!
//! Everything destructive goes through `chroot /host <bin>`, exactly as the Go
//! agent does (Talos ships `zpool`/`zfs` inside the host root; the agent pod
//! preserves the same argv). The `Runner` trait exists so tests can pin the
//! exact command lines without a host.

use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, AsyncWrite};

/// A failed host command. `output` carries the combined stdout+stderr the Go
/// `CombinedOutput` returned (used in error messages); `message` is the exit
/// status text.
#[derive(Debug, Clone)]
pub struct RunError {
    pub output: String,
    pub message: String,
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RunError {}

pub type BoxedRead = Pin<Box<dyn AsyncRead + Send + Unpin>>;
pub type BoxedWrite = Pin<Box<dyn AsyncWrite + Send + Unpin>>;

/// A future that reports whether the spawned `zfs send`/`receive` succeeded,
/// yielding the command's stderr on failure.
pub type Waiter = Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>;

/// The slice of host execution the client uses. Buffered `run` is the only
/// method tests need; the streaming spawns are implemented by `RealRunner`.
#[async_trait]
pub trait Runner: Send + Sync + 'static {
    async fn run(&self, bin: &str, args: &[String]) -> Result<String, RunError>;

    async fn spawn_out(&self, bin: &str, args: &[String]) -> std::io::Result<(BoxedRead, Waiter)> {
        let _ = (bin, args);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "streaming not supported by this runner",
        ))
    }

    async fn spawn_in(&self, bin: &str, args: &[String]) -> std::io::Result<(BoxedWrite, Waiter)> {
        let _ = (bin, args);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "streaming not supported by this runner",
        ))
    }
}

/// Runs commands against a Talos host root mounted at `host_root`.
pub struct RealRunner {
    host_root: PathBuf,
}

impl RealRunner {
    pub fn new(host_root: impl Into<PathBuf>) -> Self {
        Self {
            host_root: host_root.into(),
        }
    }

    fn command(&self, bin: &str, args: &[String]) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("chroot");
        cmd.arg(self.host_root.as_os_str());
        cmd.arg(bin);
        cmd.args(args);
        cmd
    }
}

#[async_trait]
impl Runner for RealRunner {
    async fn run(&self, bin: &str, args: &[String]) -> Result<String, RunError> {
        let output = self.command(bin, args).output().await.map_err(|e| RunError {
            output: String::new(),
            message: e.to_string(),
        })?;

        let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
        combined.push_str(&String::from_utf8_lossy(&output.stderr));

        if output.status.success() {
            Ok(combined)
        } else {
            let message = match output.status.code() {
                Some(code) => format!("exit status {code}"),
                None => "process terminated by signal".to_string(),
            };
            Err(RunError {
                output: combined,
                message,
            })
        }
    }

    async fn spawn_out(&self, bin: &str, args: &[String]) -> std::io::Result<(BoxedRead, Waiter)> {
        let mut cmd = self.command(bin, args);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn()?;

        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");

        let stderr_buf = Arc::new(Mutex::new(String::new()));
        let stderr_writer = stderr_buf.clone();
        tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut stderr = stderr;
            let mut buf = Vec::new();
            let _ = stderr.read_to_end(&mut buf).await;
            if let Ok(mut guard) = stderr_writer.lock() {
                guard.push_str(&String::from_utf8_lossy(&buf));
            }
        });

        let waiter: Waiter = Box::pin(async move {
            let status = child
                .wait()
                .await
                .map_err(|e| format!("zfs: {e}"))?;
            if status.success() {
                Ok(())
            } else {
                // Give the stderr reader a moment to drain; the pipe closes when
                // the child exits, so the task is normally already done.
                tokio::task::yield_now().await;
                let msg = stderr_buf
                    .lock()
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                if msg.is_empty() {
                    Err("zfs: command failed".to_string())
                } else {
                    Err(format!("zfs: {msg}"))
                }
            }
        });

        Ok((Box::pin(stdout), waiter))
    }

    async fn spawn_in(&self, bin: &str, args: &[String]) -> std::io::Result<(BoxedWrite, Waiter)> {
        let mut cmd = self.command(bin, args);
        cmd.stdin(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn()?;

        let stdin = child.stdin.take().expect("stdin is piped");
        let stderr = child.stderr.take().expect("stderr is piped");

        let stderr_buf = Arc::new(Mutex::new(String::new()));
        let stderr_writer = stderr_buf.clone();
        tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut stderr = stderr;
            let mut buf = Vec::new();
            let _ = stderr.read_to_end(&mut buf).await;
            if let Ok(mut guard) = stderr_writer.lock() {
                guard.push_str(&String::from_utf8_lossy(&buf));
            }
        });

        let waiter: Waiter = Box::pin(async move {
            let status = child
                .wait()
                .await
                .map_err(|e| format!("zfs: {e}"))?;
            if status.success() {
                Ok(())
            } else {
                tokio::task::yield_now().await;
                let msg = stderr_buf
                    .lock()
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                if msg.is_empty() {
                    Err("zfs: command failed".to_string())
                } else {
                    Err(format!("zfs: {msg}"))
                }
            }
        });

        Ok((Box::pin(stdin), waiter))
    }
}

/// `host_root + absolute-in-host path`, without letting an absolute path
/// replace the root (`Path::join` would).
pub fn join_host(host_root: &Path, in_host: &str) -> PathBuf {
    host_root.join(in_host.trim_start_matches('/'))
}
