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
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::Notify;

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
        cmd.kill_on_drop(true);
        cmd
    }
}

#[async_trait]
impl Runner for RealRunner {
    async fn run(&self, bin: &str, args: &[String]) -> Result<String, RunError> {
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(300),
            self.command(bin, args).output(),
        )
        .await
        .map_err(|_| RunError {
            output: String::new(),
            message: format!("{bin} timed out after 300s"),
        })?
        .map_err(|e| RunError {
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

        let abort = Arc::new(Notify::new());
        let waiter_abort = abort.clone();
        let waiter: Waiter = Box::pin(async move {
            let aborted = waiter_abort.notified();
            tokio::pin!(aborted);
            tokio::select! {
                status = child.wait() => {
                    let status = status.map_err(|e| format!("zfs: {e}"))?;
                    if status.success() {
                        Ok(())
                    } else {
                        // Give the stderr reader a moment to drain; the pipe
                        // closes when the child exits.
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
                }
                _ = &mut aborted => {
                    // The HTTP stream was dropped: kill the process so it does
                    // not block forever on a full pipe (SEC-2026-B7).
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                    Err("zfs: stream closed before the command finished".to_string())
                }
            }
        });

        Ok((
            Box::pin(GuardedRead {
                inner: stdout,
                abort,
                finished: false,
            }),
            waiter,
        ))
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

        let abort = Arc::new(Notify::new());
        let waiter_abort = abort.clone();
        let waiter: Waiter = Box::pin(async move {
            let aborted = waiter_abort.notified();
            tokio::pin!(aborted);
            let status = tokio::select! {
                status = child.wait() => status.map_err(|e| format!("zfs: {e}"))?,
                _ = &mut aborted => {
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                    return Err("zfs: stream closed before the command finished".to_string());
                }
            };
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

        Ok((
            Box::pin(GuardedWrite {
                inner: stdin,
                abort,
                finished: false,
            }),
            waiter,
        ))
    }
}

/// Kills the child when the caller drops the read stream (the send was
/// cancelled), instead of leaving it blocked on a full stdout pipe.
struct GuardedRead<R> {
    inner: R,
    abort: Arc<Notify>,
    finished: bool,
}

impl<R: AsyncRead + Unpin> AsyncRead for GuardedRead<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);
        if matches!(result, Poll::Ready(Ok(()))) && buf.filled().len() == before {
            self.finished = true;
        }
        result
    }
}

impl<R> Drop for GuardedRead<R> {
    fn drop(&mut self) {
        if !self.finished {
            self.abort.notify_one();
        }
    }
}

/// Same guard for `zfs receive`: dropping the writer aborts the child.
struct GuardedWrite<W> {
    inner: W,
    abort: Arc<Notify>,
    finished: bool,
}

impl<W: AsyncWrite + Unpin> AsyncWrite for GuardedWrite<W> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let result = Pin::new(&mut self.inner).poll_shutdown(cx);
        if matches!(result, Poll::Ready(Ok(()))) {
            self.finished = true;
        }
        result
    }
}

impl<W> Drop for GuardedWrite<W> {
    fn drop(&mut self) {
        if !self.finished {
            self.abort.notify_one();
        }
    }
}

/// `host_root + absolute-in-host path`, without letting an absolute path
/// replace the root (`Path::join` would).
pub fn join_host(host_root: &Path, in_host: &str) -> PathBuf {
    host_root.join(in_host.trim_start_matches('/'))
}
