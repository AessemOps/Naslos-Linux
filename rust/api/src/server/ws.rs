//! Terminal WebSocket endpoints (port of `server/websocket.go`): log streaming
//! and an interactive exec session, both over the bundled `kubectl`.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, Query, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use futures::{SinkExt, StreamExt};
use tokio::io::AsyncReadExt;

use super::pods::{default_namespace, validate_kube_name};
use crate::json;
use crate::state::AppState;

/// The shells the terminal offers, in preference order.
pub fn shell_names() -> Vec<&'static str> {
    vec!["bash", "sh", "ash", "zsh"]
}

fn shell_command(shell: &str) -> Result<Vec<&'static str>, String> {
    let name = if shell.trim().is_empty() {
        "sh".to_string()
    } else {
        shell.trim().to_lowercase()
    };
    match name.as_str() {
        "bash" => Ok(vec![
            "/usr/bin/env",
            "TERM=xterm-256color",
            "/bin/bash",
            "-l",
        ]),
        "sh" => Ok(vec!["/usr/bin/env", "TERM=xterm-256color", "/bin/sh"]),
        "ash" => Ok(vec!["/usr/bin/env", "TERM=xterm-256color", "/bin/ash"]),
        "zsh" => Ok(vec![
            "/usr/bin/env",
            "TERM=xterm-256color",
            "/bin/zsh",
            "-l",
        ]),
        _ => Err(format!(
            "unsupported shell {name:?} (supported: {})",
            shell_names().join(", ")
        )),
    }
}

fn kubectl_binary() -> String {
    std::env::var("KUBECTL_BIN").unwrap_or_else(|_| "kubectl".to_string())
}

fn kubeconfig_args() -> Vec<String> {
    match crate::helm::resolve_kubeconfig() {
        Some(p) => vec!["--kubeconfig".into(), p.display().to_string()],
        None => Vec::new(),
    }
}

/// `GET /api/ws/logs?namespace&pod&container`
pub async fn logs_ws(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
    ws: WebSocketUpgrade,
) -> Response {
    let _ = &state;
    let namespace = query
        .get("namespace")
        .cloned()
        .unwrap_or_else(default_namespace);
    let pod = query.get("pod").cloned().unwrap_or_default();
    let container = query.get("container").cloned().unwrap_or_default();
    if pod.is_empty() {
        return json(
            StatusCode::BAD_REQUEST,
            serde_json::json!({ "error": "pod parameter required" }),
        );
    }
    if let Err(e) = validate_kube_name(&namespace, "namespace") {
        return json(StatusCode::BAD_REQUEST, serde_json::json!({ "error": e }));
    }
    if let Err(e) = validate_kube_name(&pod, "pod") {
        return json(StatusCode::BAD_REQUEST, serde_json::json!({ "error": e }));
    }

    ws.on_upgrade(move |socket| logs_stream(socket, namespace, pod, container))
}

async fn logs_stream(socket: WebSocket, namespace: String, pod: String, container: String) {
    let (mut sender, mut receiver) = socket.split();
    let mut args = kubeconfig_args();
    args.extend([
        "logs".into(),
        "-f".into(),
        pod.clone(),
        "-n".into(),
        namespace.clone(),
        "--tail=200".into(),
    ]);
    if !container.is_empty() {
        args.push("-c".into());
        args.push(container.clone());
    }
    let mut child = match tokio::process::Command::new(kubectl_binary())
        .args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = sender
                .send(Message::Text(
                    format!("\r\n\x1b[31mCannot stream logs: {e}\x1b[0m\r\n").into(),
                ))
                .await;
            return;
        }
    };
    let mut stdout = child.stdout.take().expect("stdout piped");
    let mut stderr = child.stderr.take().expect("stderr piped");

    let send_task = {
        let mut sender = sender;
        tokio::spawn(async move {
            let mut out_buf = [0u8; 4096];
            let mut err_buf = [0u8; 4096];
            loop {
                tokio::select! {
                    r = stdout.read(&mut out_buf) => match r {
                        Ok(0) => break,
                        Ok(n) => {
                            if sender.send(Message::Text(String::from_utf8_lossy(&out_buf[..n]).into_owned().into())).await.is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            let _ = sender.send(Message::Text(format!("\r\n\x1b[31mError reading logs: {e}\x1b[0m\r\n").into())).await;
                            break;
                        }
                    },
                    r = stderr.read(&mut err_buf) => {
                        if let Ok(n) = r {
                            if n > 0 {
                                let _ = sender.send(Message::Text(String::from_utf8_lossy(&err_buf[..n]).into_owned().into())).await;
                            }
                        }
                    }
                }
            }
            let _ = sender
                .send(Message::Text(
                    "\r\n\x1b[33m--- log stream ended ---\x1b[0m\r\n".into(),
                ))
                .await;
            let _ = sender.close().await;
        })
    };

    // Drain incoming frames (the logs client sends none) so the socket closes
    // cleanly; when it does, stop the process.
    while let Some(msg) = receiver.next().await {
        if msg.is_err() || matches!(msg, Ok(Message::Close(_))) {
            break;
        }
    }
    let _ = child.kill().await;
    let _ = send_task.await;
}

/// `GET /api/ws/exec?namespace&pod&container&shell`
pub async fn exec_ws(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
    request: Request,
) -> Response {
    let namespace = query
        .get("namespace")
        .cloned()
        .unwrap_or_else(default_namespace);
    let pod = query.get("pod").cloned().unwrap_or_default();
    let container = query.get("container").cloned().unwrap_or_default();
    let shell = query.get("shell").cloned().unwrap_or_default();
    if pod.is_empty() {
        return json(
            StatusCode::BAD_REQUEST,
            serde_json::json!({ "error": "pod parameter required" }),
        );
    }
    if let Err(e) = validate_kube_name(&namespace, "namespace") {
        return json(StatusCode::BAD_REQUEST, serde_json::json!({ "error": e }));
    }
    if let Err(e) = validate_kube_name(&pod, "pod") {
        return json(StatusCode::BAD_REQUEST, serde_json::json!({ "error": e }));
    }
    let command = match shell_command(&shell) {
        Ok(c) => c,
        Err(e) => return json(StatusCode::BAD_REQUEST, serde_json::json!({ "error": e })),
    };
    let Some(kube) = state.kube.clone() else {
        return json(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({ "error": "cannot reach the Kubernetes API: no client configured" }),
        );
    };

    // Resolve the pod and container before upgrading, so a bad target is an HTTP
    // status the UI can show rather than text inside a terminal.
    let pod_obj = match kube.get_json("pod", &pod, &namespace).await {
        Ok(Some(v)) => v,
        _ => {
            return json(
                StatusCode::NOT_FOUND,
                serde_json::json!({ "error": format!("cannot find pod {namespace}/{pod}") }),
            )
        }
    };
    let resolved = match resolve_container(&pod_obj, &container) {
        Ok(c) => c,
        Err(e) => return json(StatusCode::BAD_REQUEST, serde_json::json!({ "error": e })),
    };
    let phase = pod_obj
        .get("status")
        .and_then(|s| s.get("phase"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if phase != "Running" {
        return json(
            StatusCode::CONFLICT,
            serde_json::json!({ "error": format!("pod {pod} is {phase}, not running") }),
        );
    }

    let is_ws = request
        .headers()
        .get(axum::http::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);
    if !is_ws {
        let shell_name = if shell.trim().is_empty() {
            "sh".to_string()
        } else {
            shell.trim().to_lowercase()
        };
        return json(
            StatusCode::OK,
            serde_json::json!({
                "namespace": namespace, "pod": pod, "container": resolved,
                "shell": shell_name, "status": "ready to attach",
            }),
        );
    }

    let (mut parts, _body) = request.into_parts();
    let ws = match WebSocketUpgrade::from_request_parts(&mut parts, &state).await {
        Ok(ws) => ws,
        Err(e) => return e.into_response(),
    };
    ws.on_upgrade(move |socket| exec_session(socket, namespace, pod, resolved, command))
}

fn resolve_container(pod: &serde_json::Value, requested: &str) -> Result<String, String> {
    let names: Vec<String> = pod
        .get("spec")
        .and_then(|s| s.get("containers"))
        .and_then(|c| c.as_array())
        .map(|cs| {
            cs.iter()
                .filter_map(|c| c.get("name").and_then(|n| n.as_str()))
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();
    if !requested.is_empty() {
        if names.iter().any(|n| n == requested) {
            return Ok(requested.to_string());
        }
        return Err(format!(
            "pod has no container {requested:?} (containers: {})",
            names.join(", ")
        ));
    }
    match names.len() {
        0 => Err("pod has no containers".to_string()),
        1 => Ok(names[0].clone()),
        _ => Err(format!(
            "pod has several containers, pick one: {}",
            names.join(", ")
        )),
    }
}

async fn exec_session(
    socket: WebSocket,
    namespace: String,
    pod: String,
    container: String,
    command: Vec<&'static str>,
) {
    use portable_pty::{native_pty_system, CommandBuilder, PtySize};

    let (mut sender, mut receiver) = socket.split();

    let pty_system = native_pty_system();
    let pair = match pty_system.openpty(PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    }) {
        Ok(pair) => pair,
        Err(e) => {
            let _ = sender
                .send(Message::Text(
                    format!("\r\n\x1b[31mCannot create a PTY: {e}\x1b[0m\r\n").into(),
                ))
                .await;
            return;
        }
    };

    let mut cmd = CommandBuilder::new(kubectl_binary());
    for a in kubeconfig_args() {
        cmd.arg(a);
    }
    for a in [
        "exec", "-i", "--tty", &pod, "-n", &namespace, "-c", &container, "--",
    ] {
        cmd.arg(a);
    }
    for a in &command {
        cmd.arg(a);
    }

    let mut child = match pair.slave.spawn_command(cmd) {
        Ok(c) => c,
        Err(e) => {
            let _ = sender
                .send(Message::Text(
                    format!("\r\n\x1b[31mCannot create the exec session: {e}\x1b[0m\r\n").into(),
                ))
                .await;
            return;
        }
    };
    drop(pair.slave);

    let mut reader = match pair.master.try_clone_reader() {
        Ok(r) => r,
        Err(e) => {
            let _ = sender
                .send(Message::Text(
                    format!("\r\n\x1b[31mCannot read the PTY: {e}\x1b[0m\r\n").into(),
                ))
                .await;
            return;
        }
    };
    let mut writer = match pair.master.take_writer() {
        Ok(w) => w,
        Err(e) => {
            let _ = sender
                .send(Message::Text(
                    format!("\r\n\x1b[31mCannot write the PTY: {e}\x1b[0m\r\n").into(),
                ))
                .await;
            return;
        }
    };

    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if out_tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let output_task = tokio::spawn(async move {
        while let Some(chunk) = out_rx.recv().await {
            if sender
                .send(Message::Text(
                    String::from_utf8_lossy(&chunk).into_owned().into(),
                ))
                .await
                .is_err()
            {
                break;
            }
        }
        let _ = sender.close().await;
    });

    let master = pair.master;
    while let Some(msg) = receiver.next().await {
        match msg {
            Ok(Message::Binary(data)) => {
                use std::io::Write;
                if writer.write_all(&data).is_err() {
                    break;
                }
                let _ = writer.flush();
            }
            Ok(Message::Text(text)) => {
                // Control message: apply a window resize to the PTY.
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                    if value.get("type").and_then(|v| v.as_str()) == Some("resize") {
                        let cols = value.get("cols").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
                        let rows = value.get("rows").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
                        if cols > 0 && rows > 0 {
                            let _ = master.resize(PtySize {
                                rows,
                                cols,
                                pixel_width: 0,
                                pixel_height: 0,
                            });
                        }
                    }
                }
            }
            Ok(Message::Close(_)) | Err(_) => break,
            _ => {}
        }
    }
    let _ = child.kill();
    let _ = output_task.await;
}
