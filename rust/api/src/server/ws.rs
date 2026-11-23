//! Terminal WebSocket endpoints (port of `server/websocket.go`): log streaming
//! and an interactive exec session, both over the bundled `kubectl`.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Extension, FromRequestParts, Query, Request, State};
use axum::http::{header::HOST, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use futures::{SinkExt, StreamExt};
use tokio::io::AsyncReadExt;

use super::pods::{default_namespace, validate_kube_name};
use crate::audit::SessionAudit;
use crate::auth::UserInfo;
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

pub(crate) fn kubectl_binary() -> String {
    std::env::var("KUBECTL_BIN").unwrap_or_else(|_| "kubectl".to_string())
}

pub(crate) fn kubeconfig_args() -> Vec<String> {
    match crate::helm::resolve_api_kubeconfig() {
        Some(p) => vec!["--kubeconfig".into(), p.display().to_string()],
        None => Vec::new(),
    }
}

/// Terminal session bounds. Read once from the environment:
/// `TERMINAL_MAX_SESSION_SECS` (default 8 h), `TERMINAL_IDLE_TIMEOUT_SECS`
/// (default 30 min), `TERMINAL_MAX_SESSIONS` (default 8 concurrent execs).
struct SessionLimits {
    max: std::time::Duration,
    idle: std::time::Duration,
    max_sessions: usize,
}

fn env_secs(name: &str, default: u64) -> std::time::Duration {
    let Ok(raw) = std::env::var(name) else {
        return std::time::Duration::from_secs(default);
    };
    match raw.trim().parse::<u64>() {
        Ok(n) if n > 0 => std::time::Duration::from_secs(n),
        _ => {
            tracing::warn!("{name}={raw:?} is not a positive integer; using {default}s");
            std::time::Duration::from_secs(default)
        }
    }
}

fn session_limits() -> &'static SessionLimits {
    static LIMITS: std::sync::OnceLock<SessionLimits> = std::sync::OnceLock::new();
    LIMITS.get_or_init(|| SessionLimits {
        max: env_secs("TERMINAL_MAX_SESSION_SECS", 8 * 60 * 60),
        idle: env_secs("TERMINAL_IDLE_TIMEOUT_SECS", 30 * 60),
        max_sessions: env_secs("TERMINAL_MAX_SESSIONS", 8).as_secs() as usize,
    })
}

fn exec_slots() -> &'static std::sync::Arc<tokio::sync::Semaphore> {
    static SLOTS: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
        std::sync::OnceLock::new();
    SLOTS.get_or_init(|| {
        std::sync::Arc::new(tokio::sync::Semaphore::new(session_limits().max_sessions))
    })
}

/// Log streams have their own session cap (A-6): each one spawns a `kubectl
/// logs -f` process, so a tab farm must be bounded like the exec sessions.
fn logs_slots() -> &'static std::sync::Arc<tokio::sync::Semaphore> {
    static SLOTS: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
        std::sync::OnceLock::new();
    SLOTS.get_or_init(|| {
        std::sync::Arc::new(tokio::sync::Semaphore::new(session_limits().max_sessions))
    })
}

/// `GET /api/ws/logs?namespace&pod&container`
pub async fn logs_ws(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<Arc<UserInfo>>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if let Some(response) = audit_degraded(&state) {
        return response;
    }
    if !origin_allowed(&headers, &[]) {
        return json(
            StatusCode::FORBIDDEN,
            serde_json::json!({ "error": "websocket origin refused" }),
        );
    }
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

    // Acquire the session slot before the durable begin record, so a refused
    // attach never leaves an orphan begin.
    let permit = match logs_slots().clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return json(
                StatusCode::TOO_MANY_REQUESTS,
                serde_json::json!({ "error": "too many log streams; close one and retry" }),
            )
        }
    };
    let target = format!("{namespace}/{pod}/{container}");
    let pending = crate::audit::Pending::session(
        &user,
        "logs",
        &axum::http::Method::GET,
        "/api/ws/logs",
        &target,
    );
    let audit = match SessionAudit::begin(&state, pending).await {
        Ok(audit) => audit,
        Err(_) => return audit_refused(),
    };
    ws.max_message_size(4 << 20)
        .max_frame_size(1 << 20)
        .on_upgrade(move |socket| async move {
            logs_stream(socket, namespace, pod, container, permit).await;
            audit.finish(&state, 101).await;
        })
}

async fn logs_stream(
    socket: WebSocket,
    namespace: String,
    pod: String,
    container: String,
    _slot: tokio::sync::OwnedSemaphorePermit,
) {
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
    let mut logs_cmd = tokio::process::Command::new(kubectl_binary());
    crate::proc::scrub(&mut logs_cmd);
    let mut child = match logs_cmd
        .kill_on_drop(true)
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
    // cleanly; when it does, stop the process. The stream is also bounded so a
    // forgotten tab cannot follow logs forever.
    let deadline = tokio::time::Instant::now() + session_limits().max;
    loop {
        let next = tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {
                tracing::info!("logs websocket hit the maximum session duration");
                None
            }
            msg = receiver.next() => msg,
        };
        match next {
            None => break,
            Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
            Some(Ok(_)) => {}
        }
    }
    let _ = child.kill().await;
    send_task.abort();
    let _ = send_task.await;
}

/// `GET /api/ws/exec?namespace&pod&container&shell`
pub async fn exec_ws(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
    request: Request,
) -> Response {
    let user = request
        .extensions()
        .get::<Arc<UserInfo>>()
        .cloned()
        .unwrap_or_else(|| {
            Arc::new(UserInfo {
                username: String::new(),
                groups: Vec::new(),
                email: String::new(),
                display_name: String::new(),
            })
        });
    if !origin_allowed(request.headers(), &[]) {
        return json(
            StatusCode::FORBIDDEN,
            serde_json::json!({ "error": "websocket origin refused" }),
        );
    }
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
    // The audit log is the precondition for opening an interactive session:
    // refuse the upgrade when it is degraded (R-027-core).
    if let Some(response) = audit_degraded(&state) {
        return response;
    }
    // Take the session slot first: a 429 must not leave an orphan begin
    // record behind (A-7).
    let permit = match exec_slots().clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => {
            return json(
                StatusCode::TOO_MANY_REQUESTS,
                serde_json::json!({ "error": "too many terminal sessions; close one and retry" }),
            )
        }
    };
    let target = format!("{namespace}/{pod}/{resolved}");
    let pending = crate::audit::Pending::session(
        &user,
        "terminal",
        &axum::http::Method::GET,
        "/api/ws/exec",
        &target,
    );
    let audit = match SessionAudit::begin(&state, pending).await {
        Ok(audit) => audit,
        Err(_) => return audit_refused(),
    };

    ws.max_message_size(1 << 20)
        .max_frame_size(256 << 10)
        .on_upgrade(move |socket| async move {
            exec_session(socket, namespace, pod, resolved, command, permit).await;
            audit.finish(&state, 101).await;
        })
}

/// The 503 shape for a degraded audit log: a session that cannot be recorded
/// is never opened.
fn audit_degraded(state: &Arc<AppState>) -> Option<Response> {
    state.audit.status().ne("ok").then(|| {
        json(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({ "error": "audit unavailable: sessions are refused until the audit log is repaired" }),
        )
    })
}

fn audit_refused() -> Response {
    json(
        StatusCode::SERVICE_UNAVAILABLE,
        serde_json::json!({ "error": "audit unavailable: the session could not be recorded" }),
    )
}

/// Same-host WebSocket origin check (port of the Go `CheckOrigin`, NAS-009;
/// tightened by R-028a).
///
/// Browsers always send `Origin` on a WebSocket handshake; non-browser clients
/// (CLI, tests) do not. A present `Origin` must be `https` and its authority
/// must be **exactly** the request `Host` (including the port). SSO domains
/// are not an expansion: an SSO page that wants the terminal must be served
/// from the same authority. `http` is refused so a downgraded origin cannot
/// borrow the session.
pub(crate) fn origin_allowed(headers: &HeaderMap, _extra_hosts: &[String]) -> bool {
    let Some(origin) = headers
        .get(axum::http::header::ORIGIN)
        .and_then(|v| v.to_str().ok())
    else {
        return true;
    };
    let uri: Uri = match origin.parse() {
        Ok(uri) => uri,
        Err(_) => return false,
    };
    if uri.scheme_str() != Some("https") {
        return false;
    }
    // Reject an origin that carries a path/query: an Origin header is an
    // origin, not a URL.
    if uri.path_and_query().is_some_and(|pq| pq.as_str() != "/") {
        return false;
    }
    let Some(authority) = uri.authority().map(|a| a.to_string()) else {
        return false;
    };
    // `Host`, never `X-Forwarded-Host`: a hop that forwards the client value
    // would otherwise let `Origin: https://evil` + `X-Forwarded-Host: evil`
    // pass this check. Traefik preserves the real Host.
    headers
        .get(HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .is_some_and(|host| !host.is_empty() && authority.eq_ignore_ascii_case(host))
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
    _slot: tokio::sync::OwnedSemaphorePermit,
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
    // The PTY's CommandBuilder has its own environment handling, so the
    // shared scrub helper cannot be used; apply the same allowlist.
    cmd.env_clear();
    for key in crate::proc::allowlist() {
        if let Ok(value) = std::env::var(key) {
            if !value.is_empty() {
                cmd.env(key, value);
            }
        }
    }
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
    let limits = session_limits();
    let started = tokio::time::Instant::now();
    let mut last_input = started;
    let mut reason = "client closed the socket";
    loop {
        let deadline = (started + limits.max).min(last_input + limits.idle);
        let next = tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {
                reason = if last_input + limits.idle < started + limits.max {
                    "idle timeout"
                } else {
                    "maximum session duration"
                };
                break;
            }
            msg = receiver.next() => msg,
        };
        let Some(msg) = next else { break };
        last_input = tokio::time::Instant::now();
        match msg {
            Ok(Message::Binary(data)) => {
                use std::io::Write;
                if data.len() > (256 << 10) {
                    break;
                }
                if writer.write_all(&data).is_err() {
                    break;
                }
                let _ = writer.flush();
            }
            Ok(Message::Text(text)) => {
                // Control message: apply a window resize to the PTY. Cap the
                // frame so a client cannot make the API parse megabytes of
                // JSON per keystroke (SEC-2026-B14).
                if text.len() > 4096 {
                    break;
                }
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                    if value.get("type").and_then(|v| v.as_str()) == Some("resize") {
                        let cols = value.get("cols").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
                        let rows = value.get("rows").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
                        if cols > 0 && rows > 0 && cols <= 1000 && rows <= 1000 {
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
    tracing::info!("terminal session ended: {reason}");
    let _ = child.kill();
    // Closing the receiver makes the PTY reader thread's blocking_send fail, so
    // it exits instead of pinning a thread until the kernel closes the pty.
    output_task.abort();
    let _ = output_task.await;
    // Reap the child; an unreaped portable-pty child becomes a zombie per
    // session because the API is not PID 1.
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::task::spawn_blocking(move || {
            let mut child = child;
            let _ = child.wait();
        }),
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::{logs_slots, origin_allowed, resolve_container, session_limits, shell_command};
    use axum::http::{header::HOST, HeaderMap};

    #[test]
    fn log_streams_are_bounded_by_a_session_slot_pool() {
        let limit = session_limits().max_sessions;
        let held: Vec<_> = (0..limit)
            .map(|_| {
                logs_slots()
                    .clone()
                    .try_acquire_owned()
                    .expect("a free log slot")
            })
            .collect();
        assert!(
            logs_slots().clone().try_acquire_owned().is_err(),
            "a full pool must refuse another log stream (429 path)"
        );
        drop(held);
        assert!(
            logs_slots().clone().try_acquire_owned().is_ok(),
            "released slots are reusable"
        );
    }

    fn headers(origin: Option<&str>, host: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(HOST, host.parse().unwrap());
        if let Some(o) = origin {
            h.insert(axum::http::header::ORIGIN, o.parse().unwrap());
        }
        h
    }

    #[test]
    fn absent_origin_is_a_non_browser_client() {
        assert!(origin_allowed(&headers(None, "naslos.local"), &[]));
    }

    #[test]
    fn same_authority_over_https_is_allowed() {
        assert!(origin_allowed(
            &headers(Some("https://naslos.local"), "naslos.local"),
            &[]
        ));
        assert!(origin_allowed(
            &headers(Some("https://naslos.local:8443"), "naslos.local:8443"),
            &[]
        ));
    }

    #[test]
    fn a_port_mismatch_is_refused() {
        assert!(!origin_allowed(
            &headers(Some("https://naslos.local:8443"), "naslos.local"),
            &[]
        ));
        assert!(!origin_allowed(
            &headers(Some("https://naslos.local"), "naslos.local:8443"),
            &[]
        ));
    }

    #[test]
    fn foreign_origins_are_refused() {
        assert!(!origin_allowed(
            &headers(Some("https://evil.naslos.local"), "naslos.local"),
            &[]
        ));
        assert!(!origin_allowed(
            &headers(Some("https://evil.example"), "naslos.local"),
            &[]
        ));
        assert!(!origin_allowed(
            &headers(Some("https://naslos.local/terminal"), "naslos.local"),
            &[]
        ));
    }

    #[test]
    fn http_and_opaque_origins_are_refused() {
        // A downgraded origin must not borrow the session (R-028a).
        assert!(!origin_allowed(
            &headers(Some("http://naslos.local"), "naslos.local"),
            &[]
        ));
        assert!(!origin_allowed(&headers(Some("null"), "naslos.local"), &[]));
        assert!(!origin_allowed(
            &headers(Some("file://naslos.local"), "naslos.local"),
            &[]
        ));
    }

    #[test]
    fn an_sso_domain_is_not_an_origin_expansion() {
        assert!(!origin_allowed(
            &headers(Some("https://sso.example"), "naslos.local"),
            &[]
        ));
    }

    #[test]
    fn forwarded_host_cannot_override_host() {
        // A proxy that forwarded the client's X-Forwarded-Host must not make a
        // foreign Origin look same-origin.
        let mut h = headers(Some("https://evil.example"), "naslos.local");
        h.insert("x-forwarded-host", "evil.example".parse().unwrap());
        assert!(!origin_allowed(&h, &[]));
    }

    #[test]
    fn an_empty_or_blank_shell_defaults_to_sh() {
        let expected = vec!["/usr/bin/env", "TERM=xterm-256color", "/bin/sh"];
        assert_eq!(
            shell_command("").unwrap(),
            expected,
            "an empty shell should default to sh"
        );
        assert_eq!(
            shell_command("   ").unwrap(),
            expected,
            "a blank shell should default to sh"
        );
    }

    #[test]
    fn supported_shells_are_trimmed_and_case_insensitive() {
        assert_eq!(
            shell_command(" Bash ").unwrap(),
            vec!["/usr/bin/env", "TERM=xterm-256color", "/bin/bash", "-l"],
            "bash should run as a login shell"
        );
        assert_eq!(
            shell_command("ZSH").unwrap(),
            vec!["/usr/bin/env", "TERM=xterm-256color", "/bin/zsh", "-l"],
            "zsh should run as a login shell"
        );
        assert_eq!(
            shell_command("ash").unwrap(),
            vec!["/usr/bin/env", "TERM=xterm-256color", "/bin/ash"],
            "ash should map to /bin/ash"
        );
    }

    #[test]
    fn unsupported_shells_list_the_supported_set() {
        let err = shell_command("fish").expect_err("fish is not a supported shell");
        assert!(
            err.contains("unsupported shell"),
            "error should say the shell is unsupported: {err}"
        );
        for shell in ["bash", "sh", "ash", "zsh"] {
            assert!(
                err.contains(shell),
                "error should list the supported shell {shell}: {err}"
            );
        }
    }

    fn pod_with_containers(names: &[&str]) -> serde_json::Value {
        let containers: Vec<serde_json::Value> = names
            .iter()
            .map(|name| serde_json::json!({ "name": name }))
            .collect();
        serde_json::json!({ "spec": { "containers": containers } })
    }

    #[test]
    fn a_requested_container_must_exist_in_the_pod() {
        let pod = pod_with_containers(&["app", "sidecar"]);
        assert_eq!(
            resolve_container(&pod, "app"),
            Ok("app".to_string()),
            "an existing container should resolve to its own name"
        );
        let err = resolve_container(&pod, "missing").expect_err("a missing container must fail");
        assert!(
            err.contains("no container"),
            "error should say the container was not found: {err}"
        );
        assert!(
            err.contains("app") && err.contains("sidecar"),
            "error should list both pod containers: {err}"
        );
    }

    #[test]
    fn an_empty_request_asks_for_a_container_when_several_exist() {
        let pod = pod_with_containers(&["app", "sidecar"]);
        let err = resolve_container(&pod, "").expect_err("several containers need a choice");
        assert!(
            err.contains("pick one"),
            "error should ask the caller to pick a container: {err}"
        );
    }

    #[test]
    fn an_empty_request_resolves_the_only_container() {
        let pod = pod_with_containers(&["solo"]);
        assert_eq!(
            resolve_container(&pod, ""),
            Ok("solo".to_string()),
            "a single container should resolve without an explicit request"
        );
    }

    #[test]
    fn pods_without_containers_are_rejected() {
        let empty = pod_with_containers(&[]);
        assert_eq!(
            resolve_container(&empty, ""),
            Err("pod has no containers".to_string()),
            "a pod with no containers must be rejected"
        );

        let malformed = serde_json::json!({ "metadata": { "name": "no-spec" } });
        assert_eq!(
            resolve_container(&malformed, ""),
            Err("pod has no containers".to_string()),
            "a pod without a spec should behave like a pod with no containers"
        );
    }
}
