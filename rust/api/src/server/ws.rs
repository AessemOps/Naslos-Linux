//! Terminal WebSocket endpoints (port of `server/websocket.go`): log streaming
//! and an interactive exec session, both over the bundled `kubectl`.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, Query, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
    let (mut sender, mut receiver) = socket.split();

    let mut args = kubeconfig_args();
    args.extend([
        "exec".into(),
        "-i".into(),
        "--tty".into(),
        pod.clone(),
        "-n".into(),
        namespace.clone(),
        "-c".into(),
        container.clone(),
        "--".into(),
    ]);
    args.extend(command.iter().map(|s| s.to_string()));

    let mut child = match tokio::process::Command::new(kubectl_binary())
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
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
    let mut stdin = child.stdin.take().expect("stdin piped");
    let mut stdout = child.stdout.take().expect("stdout piped");
    let mut stderr = child.stderr.take().expect("stderr piped");

    // Output: child stdout/stderr → websocket text frames.
    let output_task = tokio::spawn(async move {
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
                    Err(_) => break,
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
        let _ = sender.close().await;
    });

    // Input: binary frames are keystrokes; text frames are control messages
    // (resize is ignored: kubectl does not expose it).
    while let Some(msg) = receiver.next().await {
        match msg {
            Ok(Message::Binary(data)) => {
                if stdin.write_all(&data).await.is_err() {
                    break;
                }
            }
            Ok(Message::Text(_)) => {}
            Ok(Message::Close(_)) | Err(_) => break,
            _ => {}
        }
    }
    drop(stdin);
    let _ = child.kill().await;
    let _ = output_task.await;
}
