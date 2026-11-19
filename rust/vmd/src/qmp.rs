//! A minimal QMP (QEMU Machine Protocol) client.
//!
//! vmd opens one short-lived connection per command: QEMU's unix-socket QMP
//! server serves one client at a time, and a stateless connection per command
//! cannot get stuck holding the socket. Events arriving while a response is
//! pending are skipped (they carry no `return`/`error` member).

use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

pub struct Qmp {
    socket: PathBuf,
}

impl Qmp {
    pub fn new(socket: impl AsRef<Path>) -> Self {
        Self {
            socket: socket.as_ref().to_path_buf(),
        }
    }

    /// Execute one QMP command and return its `return` member.
    pub async fn execute(&self, command: &str, arguments: Option<Value>) -> Result<Value, String> {
        let conn = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            UnixStream::connect(&self.socket),
        )
        .await
        .map_err(|_| format!("connecting QMP socket {}: timed out", self.socket.display()))?
        .map_err(|e| format!("connecting QMP socket {}: {e}", self.socket.display()))?;
        let (reader, mut writer) = conn.into_split();
        let mut lines = BufReader::new(reader);

        // Greeting: {"QMP": {...}}
        let greeting = read_line(&mut lines).await?;
        if greeting.get("QMP").is_none() {
            return Err(format!(
                "unexpected QMP greeting on {}: {greeting}",
                self.socket.display()
            ));
        }

        // Capability negotiation.
        send(
            &mut writer,
            &serde_json::json!({ "execute": "qmp_capabilities" }),
        )
        .await?;
        read_return(&mut lines).await?;

        // The command itself.
        let mut request = serde_json::json!({ "execute": command });
        if let Some(args) = arguments {
            request["arguments"] = args;
        }
        send(&mut writer, &request).await?;
        read_return(&mut lines).await
    }

    /// Ask the guest to power down (ACPI). Returns even if the guest ignores it.
    pub async fn system_powerdown(&self) -> Result<(), String> {
        self.execute("system_powerdown", None).await.map(|_| ())
    }

    /// Terminate QEMU immediately (the force-stop action).
    pub async fn quit(&self) -> Result<(), String> {
        self.execute("quit", None).await.map(|_| ())
    }

    /// `query-balloon`: the guest's current memory usage in MiB, once the
    /// balloon driver inside the guest is up (`None` before that).
    pub async fn balloon_actual_mib(&self) -> Result<Option<u64>, String> {
        let value = self.execute("query-balloon", None).await?;
        if value.get("actual").is_none() {
            // The balloon driver is not ready yet (guest boot).
            return Ok(None);
        }
        Ok(value
            .get("actual")
            .and_then(|v| v.as_u64())
            .map(|v| v / (1024 * 1024)))
    }

    /// Set the VNC password for this VM.
    pub async fn set_vnc_password(&self, password: &str) -> Result<(), String> {
        self.execute(
            "set_password",
            Some(serde_json::json!({
                "protocol": "vnc",
                "password": password
            })),
        )
        .await
        .map(|_| ())
    }
}

async fn send(
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
    value: &Value,
) -> Result<(), String> {
    let mut line = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    line.push(b'\n');
    writer
        .write_all(&line)
        .await
        .map_err(|e| format!("writing QMP request: {e}"))
}

async fn read_line(lines: &mut (impl tokio::io::AsyncBufRead + Unpin)) -> Result<Value, String> {
    loop {
        let mut line = String::new();
        let n = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            lines.read_line(&mut line),
        )
        .await
        .map_err(|_| "reading QMP response: timed out".to_string())?
        .map_err(|e| format!("reading QMP response: {e}"))?;
        if n == 0 {
            return Err("the QMP socket closed before a response".to_string());
        }
        if line.trim().is_empty() {
            continue;
        }
        return serde_json::from_str(line.trim())
            .map_err(|e| format!("parsing QMP response {line:?}: {e}"));
    }
}

async fn read_return(lines: &mut (impl tokio::io::AsyncBufRead + Unpin)) -> Result<Value, String> {
    loop {
        let value = read_line(lines).await?;
        if value.get("event").is_some() {
            continue;
        }
        if let Some(err) = value.get("error") {
            let desc = err
                .get("desc")
                .and_then(|d| d.as_str())
                .unwrap_or("unknown QMP error");
            return Err(desc.to_string());
        }
        if let Some(ret) = value.get("return") {
            return Ok(ret.clone());
        }
        return Err(format!("unexpected QMP response: {value}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny QMP server on a unix socket for round-trip tests. It writes
    /// the greeting immediately on accept (a real QMP server never waits
    /// for the client to speak first).
    async fn spawn_qmp(socket: &Path) -> tokio::task::JoinHandle<()> {
        let listener = tokio::net::UnixListener::bind(socket).unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let (reader, mut writer) = stream.into_split();
                let mut lines = BufReader::new(reader);
                let _ = writer
                    .write_all(b"{\"QMP\":{\"version\":{\"qemu\":\"test\"}}}\n")
                    .await;
                // qmp_capabilities
                let mut line = String::new();
                if lines.read_line(&mut line).await.is_err() {
                    return;
                }
                let _ = writer.write_all(b"{\"return\":{}}\n").await;
                // the actual command
                let mut line = String::new();
                if lines.read_line(&mut line).await.is_err() {
                    return;
                }
                // An event first: the client must skip it.
                let _ = writer
                    .write_all(b"{\"event\":\"SHUTDOWN\",\"data\":{}}\n")
                    .await;
                let _ = writer
                    .write_all(b"{\"return\":{\"status\":\"running\"}}\n")
                    .await;
                let _ = writer.shutdown().await;
            }
        })
    }

    #[tokio::test]
    async fn commands_round_trip_and_skip_events() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("test.qmp");
        let server = spawn_qmp(&socket).await;
        let qmp = Qmp::new(&socket);
        // The mock answers set_password with {"return":{...}} after skipping
        // one event, which covers request framing, event skipping and
        // response parsing in a single round trip.
        qmp.set_vnc_password("pw12345")
            .await
            .expect("set_password must round-trip");
        server.abort();
    }

    #[tokio::test]
    async fn a_dead_socket_reports_an_error() {
        let qmp = Qmp::new("/nonexistent/qmp.sock");
        assert!(qmp.set_vnc_password("pw").await.is_err());
    }
}
