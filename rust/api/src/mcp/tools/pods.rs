//! Pod/namespace tools (FR-MCP). `pod_logs` is a bounded, non-streaming
//! snapshot: the last N lines (capped), unlike the terminal's live WebSocket.

use std::sync::Arc;

use rmcp::model::JsonObject;
use serde_json::Value;

use super::{n, no_args, s, schema, Confirm, Method, Scope, Special, ToolDef};
use crate::mcp::tools::{CallError, Outcome};
use crate::state::AppState;

/// Hard output cap: a snapshot, not a firehose (the WS stream stays the only
/// way to follow logs live).
const MAX_OUTPUT_BYTES: usize = 256 * 1024;
const MAX_TAIL: i64 = 500;
const MAX_SINCE_SECONDS: i64 = 24 * 60 * 60;

pub const TOOLS: &[ToolDef] = &[
    tool!(
        "namespace_list",
        "Every Kubernetes namespace on the node.",
        Read,
        GET,
        "/api/namespaces",
        no_args
    ),
    tool!(
        "pod_list",
        "The pods (and containers) in one namespace (empty = every namespace).",
        Read,
        GET,
        "/api/pods",
        query = ["namespace"],
        || schema(&[], &[("namespace", s("The namespace to list (optional)."))])
    ),
    ToolDef {
        name: "pod_logs",
        desc: "A bounded snapshot of one pod's logs (the last N lines, text output). Not a live stream.",
        level: Scope::Read,
        method: Method::GET,
        path: "/api/pods/logs",
        query: &[],
        confirm: Confirm::No,
        schema: || {
            schema(
                &["namespace", "pod"],
                &[
                    ("namespace", s("The pod's namespace.")),
                    ("pod", s("The pod name (from pod_list).")),
                    ("container", s("One container's logs (default: the only/first container).")),
                    ("tail", n("How many lines to return, 1-500 (default 200).")),
                    ("sinceSeconds", n("Only lines newer than this many seconds (max 86400).")),
                ],
            )
        },
        special: Special::PodLogs,
    },
];

/// One-shot `kubectl logs` bounded by tail/since and an output byte cap.
/// Mirrors the WS stream's validation so the same name rules apply.
pub async fn pod_logs(state: &Arc<AppState>, args: &JsonObject) -> Result<Outcome, CallError> {
    use crate::server::pods::{default_namespace, validate_kube_name};

    let namespace = args
        .get("namespace")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or(&default_namespace())
        .to_string();
    let pod = args
        .get("pod")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or(CallError::BadPath { param: "pod" })?
        .to_string();
    let container = args
        .get("container")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or_default();
    let tail = args
        .get("tail")
        .and_then(Value::as_i64)
        .filter(|t| *t > 0)
        .unwrap_or(200)
        .min(MAX_TAIL);
    let since = args
        .get("sinceSeconds")
        .and_then(Value::as_i64)
        .filter(|s| *s > 0)
        .map(|s| s.min(MAX_SINCE_SECONDS));

    for (value, what) in [(namespace.as_str(), "namespace"), (pod.as_str(), "pod")] {
        if let Err(e) = validate_kube_name(value, what) {
            return Ok(Outcome {
                status: 400,
                body: format!("{{\"error\":{e:?}}}"),
            });
        }
    }
    if !container.is_empty() {
        if let Err(e) = validate_kube_name(container, "container") {
            return Ok(Outcome {
                status: 400,
                body: format!("{{\"error\":{e:?}}}"),
            });
        }
    }
    let _ = state; // the kubeconfig path is process-wide (like the WS stream)

    let mut cmd = tokio::process::Command::new(crate::server::ws::kubectl_binary());
    crate::proc::scrub(&mut cmd);
    cmd.arg("logs")
        .arg(&pod)
        .arg("-n")
        .arg(&namespace)
        .arg("--tail")
        .arg(tail.to_string());
    if !container.is_empty() {
        cmd.arg("-c").arg(container);
    }
    if let Some(since) = since {
        cmd.arg("--since").arg(format!("{since}s"));
    }
    for arg in crate::server::ws::kubeconfig_args() {
        cmd.arg(arg);
    }

    let output = cmd
        .output()
        .await
        .map_err(|e| CallError::Internal(e.to_string()))?;
    let mut logs = String::from_utf8_lossy(&output.stdout).into_owned();
    if logs.len() > MAX_OUTPUT_BYTES {
        // Keep the tail (the most recent lines), like `tail` semantics.
        let mut start = logs.len() - MAX_OUTPUT_BYTES;
        while start < logs.len() && !logs.is_char_boundary(start) {
            start += 1;
        }
        logs = format!(
            "[pod_logs: output truncated to the last {} KiB]\n{}",
            MAX_OUTPUT_BYTES / 1024,
            &logs[start..]
        );
    }
    if logs.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        return Ok(Outcome {
            status: if output.status.success() { 200 } else { 502 },
            body: if stderr.trim().is_empty() {
                "(no log output)".to_string()
            } else {
                stderr.trim().to_string()
            },
        });
    }
    Ok(Outcome {
        status: 200,
        body: logs,
    })
}
