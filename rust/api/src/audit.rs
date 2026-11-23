//! Append-only, tamper-evident audit log (FR-AUD).
//!
//! One JSON Lines record per line in `segments/audit-<seq>.jsonl` under the
//! state volume. A SHA-256 hash chain (`prev`/`hash`) makes edits, reordering,
//! insertion and deletion detectable; a checkpoint records the chunk anchors
//! left behind by retention so the retained chain still verifies.
//!
//! Failure semantics are fail-closed for mutations: the begin record is
//! written and `fsync`ed before the handler runs, and an unwritable audit sink
//! answers `503` instead of allowing an unaudited change. Once the log is
//! degraded (append or verification failure), reads keep working but every
//! later mutation is refused until an operator repairs the log; nothing here
//! ever truncates or "repairs" a chain automatically.
//!
//! Records never contain request/response bodies, query strings, headers
//! (other than a truncated user agent), MCP arguments, tokens, hashes, keys or
//! credentials. `action.target` comes from a route path parameter only.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::extract::{ConnectInfo, Query, Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::auth::UserInfo;
use crate::state::AppState;

/// Where the audit log lives when `AUDIT_DIR` is unset.
pub const DEFAULT_AUDIT_DIR: &str = "/var/lib/naslos/audit";
const DEFAULT_RETENTION_DAYS: u64 = 90;
const DEFAULT_MAX_BYTES: u64 = 16 * 1024 * 1024;
const DEFAULT_SEGMENT_BYTES: u64 = 8 * 1024 * 1024;
const DEFAULT_MIN_FREE_BYTES: u64 = 8 * 1024 * 1024;

const RECORD_VERSION: u64 = 1;
const ZERO_HASH: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";
/// Marks a `ensure_capacity` refusal. These are transient (the volume is
/// full) and never permanently degrade the chain; any other append failure
/// does until an operator repairs the log.
const CAPACITY_REFUSAL_MARKER: &str = "space is reclaimed";
const MAX_FILTER_LIMIT: usize = 1000;
const MAX_EXPORT_LIMIT: usize = 50_000;
const MAX_ERRORS: usize = 20;

// ---------------------------------------------------------------------------
// Record model (stable JSON contract, see docs/rbac-audit.md §7.2)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Actor {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default)]
    pub groups: Vec<String>,
    pub auth: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    pub method: String,
    pub path: String,
    pub route: String,
    pub peer: Option<String>,
    pub client: Option<String>,
    pub user_agent: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Action {
    pub name: String,
    pub target: Option<String>,
    pub phase: String,
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    pub state: String,
    pub http_status: Option<u16>,
    pub error_code: Option<String>,
    pub error: Option<String>,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Correlation {
    pub mcp_call_id: Option<String>,
    pub job_id: Option<String>,
    pub operation_id: Option<String>,
}

/// One pending begin/end pair (or a single end record for a read).
#[derive(Clone)]
pub struct Pending {
    request_id: String,
    actor: Actor,
    source: Source,
    action: Action,
    correlation: Correlation,
    started: Instant,
}

impl Pending {
    /// An authenticated user request. The route template (not the concrete
    /// path) is the action source; only the first path parameter becomes the
    /// target.
    pub fn user(
        user: &UserInfo,
        method: &Method,
        path: &str,
        template: &str,
        peer: Option<String>,
        user_agent: Option<String>,
        request_id: String,
    ) -> Self {
        Self {
            request_id,
            actor: Actor {
                kind: "user".to_string(),
                id: None,
                name: user.username.clone(),
                role: Some(
                    crate::rbac::role_for_groups(&user.groups)
                        .as_str()
                        .to_string(),
                ),
                groups: user.groups.clone(),
                auth: "authelia".to_string(),
            },
            source: Source {
                method: method.as_str().to_string(),
                path: path.to_string(),
                route: template.to_string(),
                peer,
                client: None,
                user_agent,
            },
            action: Action {
                name: crate::rbac::action_name(method, template),
                target: crate::rbac::action_target(template, path),
                phase: String::new(),
                params: json!({}),
            },
            correlation: Correlation {
                mcp_call_id: None,
                job_id: None,
                operation_id: None,
            },
            started: Instant::now(),
        }
    }

    /// An MCP `tools/call`. Arguments are never copied; only the tool name and
    /// the resolved path parameter (target) are recorded.
    #[allow(clippy::too_many_arguments)]
    pub fn mcp(
        token_id: &str,
        token_name: &str,
        scope: &str,
        tool_name: &str,
        method: &Method,
        route: &str,
        resolved_path: &str,
        target: Option<String>,
    ) -> Self {
        let request_id = random_hex(16);
        Self {
            request_id: request_id.clone(),
            actor: Actor {
                kind: "mcp".to_string(),
                id: Some(token_id.to_string()),
                name: token_name.to_string(),
                role: Some(scope.to_string()),
                groups: Vec::new(),
                auth: "mcp-token".to_string(),
            },
            source: Source {
                method: method.as_str().to_string(),
                path: resolved_path.to_string(),
                route: route.to_string(),
                peer: None,
                client: None,
                user_agent: None,
            },
            action: Action {
                name: "mcp.tool_call".to_string(),
                target: target.or_else(|| Some(tool_name.to_string())),
                phase: String::new(),
                params: json!({}),
            },
            correlation: Correlation {
                mcp_call_id: Some(request_id.clone()),
                job_id: None,
                operation_id: None,
            },
            started: Instant::now(),
        }
    }

    /// A system component action (retention pruning today; scheduled jobs
    /// later). `params` is still passed through the redactor.
    pub fn system(component: &str, action: &str, target: Option<String>, params: Value) -> Self {
        let mut params = params;
        redact_value(&mut params);
        Self {
            request_id: random_hex(16),
            actor: Actor {
                kind: "system".to_string(),
                id: None,
                name: component.to_string(),
                role: None,
                groups: Vec::new(),
                auth: "system".to_string(),
            },
            source: Source {
                method: "INTERNAL".to_string(),
                path: String::new(),
                route: String::new(),
                peer: None,
                client: None,
                user_agent: None,
            },
            action: Action {
                name: action.to_string(),
                target,
                phase: String::new(),
                params,
            },
            correlation: Correlation {
                mcp_call_id: None,
                job_id: None,
                operation_id: None,
            },
            started: Instant::now(),
        }
    }

    /// A long-lived session (WebSocket terminal/VNC, Buddy receive stream).
    /// `kind` becomes `session.<kind>`; the target is the resolved resource.
    pub fn session(
        user: &UserInfo,
        kind: &str,
        method: &Method,
        route: &str,
        target: &str,
    ) -> Self {
        Self {
            request_id: random_hex(16),
            actor: Actor {
                kind: "user".to_string(),
                id: None,
                name: user.username.clone(),
                role: Some(
                    crate::rbac::role_for_groups(&user.groups)
                        .as_str()
                        .to_string(),
                ),
                groups: user.groups.clone(),
                auth: "authelia".to_string(),
            },
            source: Source {
                method: method.as_str().to_string(),
                path: route.to_string(),
                route: route.to_string(),
                peer: None,
                client: None,
                user_agent: None,
            },
            action: Action {
                name: format!("session.{kind}"),
                target: Some(target.to_string()),
                phase: String::new(),
                params: json!({}),
            },
            correlation: Correlation {
                mcp_call_id: None,
                job_id: None,
                operation_id: None,
            },
            started: Instant::now(),
        }
    }
}

/// Audit handle for a long-lived session. Shared helper for the terminal/VNC
/// WebSocket handlers (R-027-core) and the Buddy receiver (WP-06).
///
/// `SessionAudit::begin` writes the durable begin record; `Err` means the
/// caller must refuse the upgrade with 503. `finish` writes the end record and
/// degrades the log (with one alert) if that write fails, because the session
/// already happened.
pub struct SessionAudit {
    pending: Pending,
}

impl SessionAudit {
    pub async fn begin(state: &Arc<AppState>, pending: Pending) -> Result<Self, String> {
        let pending = state.audit.begin_async(pending).await.inspect_err(|e| {
            if state.audit.note_write_failure(e) {
                spawn_degraded_alert(state.clone(), e.clone());
            }
        })?;
        Ok(Self { pending })
    }

    pub async fn finish(&self, state: &Arc<AppState>, status: u16) {
        if let Err(e) = state.audit.finish_async(&self.pending, status).await {
            if state.audit.note_write_failure(&e) {
                spawn_degraded_alert(state.clone(), e);
            }
        }
    }
}

/// A role denial on a safe method is audited here because the audit
/// middleware only opens a begin/end pair for unsafe methods (R-027-core).
/// One end-only record carries the authenticated actor and the route
/// template, never the concrete path.
pub async fn record_role_denied(
    state: &Arc<AppState>,
    user: &UserInfo,
    method: &Method,
    path: &str,
) {
    let template = crate::rbac::route_template(method, path);
    let pending = Pending::user(user, method, path, template, None, None, random_hex(16));
    if let Err(e) = state
        .audit
        .record_refusal_async(&pending, "denied", "role")
        .await
    {
        if state.audit.note_write_failure(&e) {
            spawn_degraded_alert(state.clone(), e);
        }
    }
}

// ---------------------------------------------------------------------------
// Configuration, segments and checkpoint
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct AuditConfig {
    pub retention_days: u64,
    pub max_bytes: u64,
    pub segment_bytes: u64,
    pub min_free_bytes: u64,
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            retention_days: DEFAULT_RETENTION_DAYS,
            max_bytes: DEFAULT_MAX_BYTES,
            segment_bytes: DEFAULT_SEGMENT_BYTES,
            min_free_bytes: DEFAULT_MIN_FREE_BYTES,
        }
    }
}

impl AuditConfig {
    fn from_env() -> Self {
        let mut config = Self::default();
        if let Some(value) = env_u64("AUDIT_RETENTION_DAYS") {
            config.retention_days = value;
        }
        if let Some(value) = env_u64("AUDIT_MAX_BYTES") {
            config.max_bytes = value;
        }
        if let Some(value) = env_u64("AUDIT_SEGMENT_BYTES") {
            config.segment_bytes = value;
        }
        if let Some(value) = env_u64("AUDIT_MIN_FREE_BYTES") {
            config.min_free_bytes = value;
        }
        config.sanitized()
    }

    fn sanitized(mut self) -> Self {
        // A zero segment size would rotate forever; keep a small floor.
        self.segment_bytes = self.segment_bytes.max(4096);
        self.max_bytes = self.max_bytes.max(self.segment_bytes);
        self
    }

    fn for_test() -> Self {
        Self {
            retention_days: DEFAULT_RETENTION_DAYS,
            max_bytes: DEFAULT_MAX_BYTES,
            segment_bytes: DEFAULT_SEGMENT_BYTES,
            min_free_bytes: 0,
        }
    }
}

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
}

#[derive(Debug, Clone)]
struct SegmentMeta {
    first_seq: u64,
    path: PathBuf,
    bytes: u64,
    records: u64,
    last_ts: Option<DateTime<Utc>>,
    last_hash: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Checkpoint {
    #[serde(default)]
    v: u64,
    #[serde(default)]
    first_seq: Option<u64>,
    #[serde(default)]
    last_seq: Option<u64>,
    #[serde(default)]
    records: u64,
    #[serde(default)]
    head_hash: String,
    #[serde(default)]
    pruned_heads: Vec<String>,
    #[serde(default)]
    updated_at: String,
}

struct AuditState {
    active: Option<File>,
    active_path: PathBuf,
    active_first_seq: u64,
    active_bytes: u64,
    active_records: u64,
    active_last_ts: Option<DateTime<Utc>>,
    next_seq: u64,
    prev_hash: String,
    first_seq: Option<u64>,
    records: u64,
    sealed: Vec<SegmentMeta>,
    pruned_heads: Vec<String>,
    degraded: Option<String>,
    /// `degraded` was caused by a full volume rather than a chain/integrity
    /// failure. Retryable: the next append clears it once space is reclaimed.
    degraded_retryable: bool,
    #[cfg(test)]
    fail_next_append: bool,
    #[cfg(test)]
    fail_next_end: bool,
    /// Test-only fsync stand-in: a deliberate delay inside the append path.
    #[cfg(test)]
    append_delay: Option<std::time::Duration>,
}

struct Inner {
    dir: PathBuf,
    segments_dir: PathBuf,
    checkpoint_path: PathBuf,
    config: AuditConfig,
    state: Mutex<AuditState>,
    _temp: Option<tempfile::TempDir>,
}

/// The process-wide audit log handle. Cloning shares the same chain state.
#[derive(Clone)]
pub struct AuditLog {
    inner: Arc<Inner>,
}

impl AuditLog {
    /// Open (or create) the audit directory. A missing/unwritable directory is
    /// a fatal configuration error; a chain that does not verify opens in the
    /// degraded state so reads and verification still work and mutations fail
    /// closed.
    pub fn open(dir: PathBuf, config: AuditConfig) -> Result<Self, String> {
        let config = config.sanitized();
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("creating audit directory {}: {e}", dir.display()))?;
        set_dir_mode(&dir)?;
        let segments_dir = dir.join("segments");
        std::fs::create_dir_all(&segments_dir).map_err(|e| {
            format!(
                "creating audit segment directory {}: {e}",
                segments_dir.display()
            )
        })?;
        set_dir_mode(&segments_dir)?;
        probe_writable(&segments_dir)?;

        let checkpoint_path = dir.join("checkpoint.json");
        let checkpoint = load_checkpoint(&checkpoint_path).unwrap_or_else(|e| {
            tracing::warn!("audit checkpoint unreadable ({e}); verification starts from genesis");
            Checkpoint {
                v: RECORD_VERSION,
                ..Checkpoint::default()
            }
        });
        let paths = segment_paths(&segments_dir)?;
        let (mut report, mut metas) = verify_paths(&paths, &checkpoint);
        if !checkpoint_path.exists() && !paths.is_empty() {
            report.errors.push(
                "audit checkpoint is missing; refusing to trust an unanchored log".to_string(),
            );
            report.ok = false;
        }

        let mut state = AuditState {
            active: None,
            active_path: PathBuf::new(),
            active_first_seq: 1,
            active_bytes: 0,
            active_records: 0,
            active_last_ts: None,
            next_seq: 1,
            prev_hash: ZERO_HASH.to_string(),
            first_seq: report.first_seq,
            records: report.records,
            sealed: Vec::new(),
            pruned_heads: checkpoint.pruned_heads.clone(),
            degraded: None,
            degraded_retryable: false,
            #[cfg(test)]
            fail_next_append: false,
            #[cfg(test)]
            fail_next_end: false,
            #[cfg(test)]
            append_delay: None,
        };

        if !report.ok {
            // Never truncate or repair. Reads stay available; the next mutation
            // is refused and `/api/ready` reports `audit: degraded`.
            state.degraded = Some(report.errors.join("; "));
            tracing::error!(
                "audit chain verification failed at startup: {}",
                state.degraded.as_deref().unwrap_or("unknown error")
            );
        } else {
            if let Some(active_meta) = metas.pop() {
                // Reopen the newest segment for append.
                let file = OpenOptions::new()
                    .read(true)
                    .append(true)
                    .mode(0o600)
                    .open(&active_meta.path)
                    .map_err(|e| format!("opening active audit segment: {e}"))?;
                state.active_first_seq = active_meta.first_seq;
                state.active_path = active_meta.path.clone();
                state.active_bytes = active_meta.bytes;
                state.active_records = active_meta.records;
                state.active_last_ts = active_meta.last_ts;
                state.next_seq = if active_meta.records == 0 {
                    active_meta.first_seq
                } else {
                    report.last_seq.unwrap_or(active_meta.first_seq) + 1
                };
                state.prev_hash = if active_meta.records == 0 {
                    metas
                        .iter()
                        .rev()
                        .find(|meta| meta.records > 0)
                        .map(|meta| meta.last_hash.clone())
                        .or_else(|| {
                            if checkpoint.head_hash.is_empty() {
                                None
                            } else {
                                Some(checkpoint.head_hash.clone())
                            }
                        })
                        .unwrap_or_else(|| ZERO_HASH.to_string())
                } else {
                    active_meta.last_hash.clone()
                };
                state.active = Some(file);
                metas.retain(|meta| meta.records > 0);
                state.sealed = metas;
            } else {
                // Fresh log: create the first segment.
                state.sealed = Vec::new();
                state.active_path = segment_path(&segments_dir, 1);
                state.active = Some(create_segment(&state.active_path)?);
                state.active_first_seq = 1;
                state.active_bytes = 0;
                state.active_records = 0;
                state.next_seq = 1;
                state.prev_hash = ZERO_HASH.to_string();
            }
            if let Err(e) = write_checkpoint_locked(&checkpoint_path, &state) {
                tracing::warn!("could not seed the audit checkpoint: {e}");
            }
        }

        Ok(Self {
            inner: Arc::new(Inner {
                dir,
                segments_dir,
                checkpoint_path,
                config,
                state: Mutex::new(state),
                _temp: None,
            }),
        })
    }

    pub fn from_env() -> Result<Self, String> {
        let dir = std::env::var("AUDIT_DIR").unwrap_or_else(|_| DEFAULT_AUDIT_DIR.to_string());
        Self::open(PathBuf::from(dir), AuditConfig::from_env())
    }

    /// A log in a private temporary directory for tests. The directory lives
    /// as long as the handle.
    pub fn for_test() -> Self {
        let temp = tempfile::tempdir().expect("creating an audit temp dir");
        let mut log = Self::open(temp.path().to_path_buf(), AuditConfig::for_test())
            .expect("opening the test audit log");
        if let Some(inner) = Arc::get_mut(&mut log.inner) {
            inner._temp = Some(temp);
        }
        log
    }

    /// `"ok"` or `"degraded"` for `/api/ready`.
    pub fn status(&self) -> &'static str {
        let state = self.inner.state.lock().unwrap();
        if state.degraded.is_some() {
            "degraded"
        } else {
            "ok"
        }
    }

    pub fn degraded_reason(&self) -> Option<String> {
        self.inner.state.lock().unwrap().degraded.clone()
    }

    /// Test-only failure injection for the fail-closed contract tests.
    #[cfg(test)]
    pub fn fail_next_append(&self) {
        self.inner.state.lock().unwrap().fail_next_append = true;
    }

    #[cfg(test)]
    pub fn fail_next_end(&self) {
        self.inner.state.lock().unwrap().fail_next_end = true;
    }

    #[cfg(test)]
    pub fn set_append_delay(&self, delay: Option<std::time::Duration>) {
        self.inner.state.lock().unwrap().append_delay = delay;
    }

    /// Whether the current degradation is a capacity refusal (retryable) rather
    /// than a chain/integrity failure (permanent until repaired).
    pub fn degraded_is_retryable(&self) -> bool {
        let state = self.inner.state.lock().unwrap();
        state.degraded.is_some() && state.degraded_retryable
    }

    /// Mark the chain degraded. Returns `true` only for the first transition,
    /// so callers can raise exactly one alert.
    pub fn mark_degraded(&self, reason: &str) -> bool {
        let mut state = self.inner.state.lock().unwrap();
        if state.degraded.is_some() {
            return false;
        }
        state.degraded = Some(sanitize_text(reason, 512));
        state.degraded_retryable = false;
        true
    }

    /// Record a failed audit write. A full volume is transient: the log stays
    /// degraded and mutations stay refused, but the refusal is retryable and
    /// the next append re-checks the volume (so reclaiming space recovers
    /// without a restart). Any other failure degrades permanently until an
    /// operator repairs the chain. Returns `true` only on the first
    /// transition, so callers alert exactly once.
    pub fn note_write_failure(&self, reason: &str) -> bool {
        if !is_capacity_refusal(reason) {
            return self.mark_degraded(reason);
        }
        let mut state = self.inner.state.lock().unwrap();
        if state.degraded.is_some() {
            return false;
        }
        state.degraded = Some(sanitize_text(reason, 512));
        state.degraded_retryable = true;
        true
    }

    /// Whether the volume has at least `incoming` bytes of room above the
    /// configured free-space reserve. An unreadable `statvfs` is treated as
    /// "unknown, proceed": the append itself still surfaces a real failure.
    fn capacity_reclaimed(&self, incoming: u64) -> bool {
        match available_bytes(&self.inner.dir) {
            Some(available) => {
                available >= self.inner.config.min_free_bytes.saturating_add(incoming)
            }
            None => true,
        }
    }

    /// Durably write the begin record. On failure the caller must mark the log
    /// degraded (and alert once) and refuse the mutation (503).
    pub fn begin(&self, pending: Pending) -> Result<Pending, String> {
        let attempted = Outcome {
            state: "attempted".to_string(),
            http_status: None,
            error_code: None,
            error: None,
            duration_ms: None,
        };
        self.append(&pending, "begin", &attempted).map(|()| pending)
    }

    /// Write the end record for a begin. The caller must mark the log degraded
    /// and treat an error as "the action may have been applied": it is not
    /// safe to report success.
    pub fn finish(&self, pending: &Pending, status: u16) -> Result<(), String> {
        #[cfg(test)]
        {
            let mut state = self.inner.state.lock().unwrap();
            if state.fail_next_end {
                state.fail_next_end = false;
                return Err("injected audit end failure".to_string());
            }
        }
        let audience = end_outcome(status, Some(pending.started.elapsed()));
        self.append(pending, "end", &audience)
    }

    /// One durable record for a call that does not need a begin/end pair
    /// (MCP read tools).
    pub fn record_once(&self, pending: &Pending, status: u16) -> Result<(), String> {
        let outcome = end_outcome(status, Some(pending.started.elapsed()));
        self.append(pending, "end", &outcome)
    }

    /// One durable record for a call refused before dispatch (unknown tool,
    /// scope, `confirm`, path validation). No begin exists; a single end
    /// record with the stable refusal code is enough.
    pub fn record_refusal(
        &self,
        pending: &Pending,
        state: &str,
        error_code: &str,
    ) -> Result<(), String> {
        let outcome = Outcome {
            state: state.to_string(),
            http_status: None,
            error_code: Some(error_code.to_string()),
            error: None,
            duration_ms: Some(pending.started.elapsed().as_millis().min(u64::MAX as u128) as u64),
        };
        self.append(pending, "end", &outcome)
    }

    /// Async variants used by request paths. The durable append (fsync
    /// included) runs on the blocking pool so a slow disk never stalls the
    /// Tokio workers and the `std::sync::Mutex` is never held across an
    /// `.await` (R-062).
    pub async fn begin_async(&self, pending: Pending) -> Result<Pending, String> {
        let log = self.clone();
        tokio::task::spawn_blocking(move || log.begin(pending))
            .await
            .map_err(|e| format!("audit begin task failed: {e}"))?
    }

    pub async fn finish_async(&self, pending: &Pending, status: u16) -> Result<(), String> {
        let log = self.clone();
        let pending = pending.clone();
        tokio::task::spawn_blocking(move || log.finish(&pending, status))
            .await
            .map_err(|e| format!("audit finish task failed: {e}"))?
    }

    pub async fn record_once_async(&self, pending: &Pending, status: u16) -> Result<(), String> {
        let log = self.clone();
        let pending = pending.clone();
        tokio::task::spawn_blocking(move || log.record_once(&pending, status))
            .await
            .map_err(|e| format!("audit record task failed: {e}"))?
    }

    pub async fn record_refusal_async(
        &self,
        pending: &Pending,
        state: &str,
        error_code: &str,
    ) -> Result<(), String> {
        let log = self.clone();
        let pending = pending.clone();
        let state = state.to_string();
        let error_code = error_code.to_string();
        tokio::task::spawn_blocking(move || log.record_refusal(&pending, &state, &error_code))
            .await
            .map_err(|e| format!("audit refusal task failed: {e}"))?
    }

    fn append(&self, pending: &Pending, phase: &str, outcome: &Outcome) -> Result<(), String> {
        let mut state = self.inner.state.lock().unwrap();
        if let Some(reason) = state.degraded.clone() {
            // A capacity refusal clears itself once the operator reclaims
            // space; a chain failure never does.
            if state.degraded_retryable && self.capacity_reclaimed(0) {
                state.degraded = None;
                state.degraded_retryable = false;
            } else {
                return Err(format!("audit log is degraded: {reason}"));
            }
        }
        self.append_inner(&mut state, pending, phase, outcome, true)
    }

    /// Rotate before writing if the active segment would exceed its bound.
    fn ensure_capacity(&self, state: &mut AuditState, incoming: u64) -> Result<(), String> {
        let available = available_bytes(&self.inner.dir);
        if let Some(available) = available {
            if available < self.inner.config.min_free_bytes.saturating_add(incoming) {
                return Err(
                    "the audit volume is full; refusing mutations until space is reclaimed"
                        .to_string(),
                );
            }
        }
        if state.active_bytes > 0
            && state.active_bytes.saturating_add(incoming) > self.inner.config.segment_bytes
        {
            self.rotate(state)?;
        }
        Ok(())
    }

    fn rotate(&self, state: &mut AuditState) -> Result<(), String> {
        if state.active_bytes == 0 {
            return Ok(());
        }
        if let Some(file) = state.active.as_mut() {
            file.sync_all()
                .map_err(|e| format!("syncing the sealed audit segment: {e}"))?;
        }
        state.sealed.push(SegmentMeta {
            first_seq: state.active_first_seq,
            path: state.active_path.clone(),
            bytes: state.active_bytes,
            records: state.active_records,
            last_ts: state.active_last_ts,
            last_hash: state.prev_hash.clone(),
        });
        let next = state.next_seq;
        let path = segment_path(&self.inner.segments_dir, next);
        let file = create_segment(&path)?;
        state.active = Some(file);
        state.active_path = path;
        state.active_first_seq = next;
        state.active_bytes = 0;
        state.active_records = 0;
        state.active_last_ts = None;
        let _ = sync_dir(&self.inner.segments_dir);
        if let Err(e) = write_checkpoint_locked(&self.inner.checkpoint_path, state) {
            tracing::warn!("could not update the audit checkpoint after rotation: {e}");
        }
        Ok(())
    }

    /// Age/byte retention. Only sealed segments are candidates, and the newest
    /// sealed segment is never removed (the active segment is not a candidate
    /// either). The prune is itself recorded in the active segment with the
    /// removed segments' head hashes; the checkpoint keeps the anchors so the
    /// retained chain verifies.
    fn prune(&self, state: &mut AuditState) -> Result<(), String> {
        let now = Utc::now();
        let days = self.inner.config.retention_days.min(36_500) as i64;
        let cutoff = now - Duration::days(days);
        let mut pruned: Vec<SegmentMeta> = Vec::new();
        loop {
            if state.sealed.len() <= 1 {
                break;
            }
            let total = state
                .sealed
                .iter()
                .map(|segment| segment.bytes)
                .sum::<u64>()
                .saturating_add(state.active_bytes);
            let oldest = &state.sealed[0];
            let too_old = self.inner.config.retention_days == 0
                || oldest.last_ts.map(|ts| ts < cutoff).unwrap_or(false);
            if total <= self.inner.config.max_bytes && !too_old {
                break;
            }
            pruned.push(state.sealed.remove(0));
        }
        if pruned.is_empty() {
            return Ok(());
        }

        let mut params = Vec::new();
        let mut heads = Vec::new();
        for segment in &pruned {
            heads.push(segment.last_hash.clone());
            params.push(json!({
                "segment": segment
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_default(),
                "firstSeq": segment.first_seq,
                "head": segment.last_hash,
            }));
        }
        let target = pruned
            .iter()
            .map(|segment| segment.first_seq.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let pending = Pending::system(
            "audit",
            "audit.retention.prune",
            Some(target),
            json!({ "segments": params }),
        );
        let outcome = end_outcome(204, None);
        // `append` with retention disabled: avoid recursing into this method.
        self.append_inner(state, &pending, "end", &outcome, false)?;

        for head in heads {
            if !state.pruned_heads.iter().any(|known| known == &head) {
                state.pruned_heads.push(head);
            }
        }
        if state.pruned_heads.len() > 128 {
            let excess = state.pruned_heads.len() - 128;
            state.pruned_heads.drain(0..excess);
        }
        state.records = state
            .records
            .saturating_sub(pruned.iter().map(|segment| segment.records).sum::<u64>());
        write_checkpoint_locked(&self.inner.checkpoint_path, state)?;

        let mut removed = 0u64;
        for segment in &pruned {
            match std::fs::remove_file(&segment.path) {
                Ok(()) => removed += segment.bytes,
                Err(e) => tracing::warn!(
                    "could not remove the pruned audit segment {}: {e}",
                    segment.path.display()
                ),
            }
        }
        if removed > 0 {
            let _ = sync_dir(&self.inner.segments_dir);
        }
        Ok(())
    }

    fn append_inner(
        &self,
        state: &mut AuditState,
        pending: &Pending,
        phase: &str,
        outcome: &Outcome,
        allow_prune: bool,
    ) -> Result<(), String> {
        #[cfg(test)]
        if state.fail_next_append {
            state.fail_next_append = false;
            return Err("injected audit append failure".to_string());
        }
        #[cfg(test)]
        if let Some(delay) = state.append_delay {
            std::thread::sleep(delay);
        }
        let seq = state.next_seq;
        let now = Utc::now();
        let value = record_value(pending, seq, &state.prev_hash, phase, outcome, now);
        let hash = canonical_hash(&value);
        let mut record = value;
        if let Some(object) = record.as_object_mut() {
            object.insert("hash".to_string(), Value::String(hash.clone()));
        }
        let mut line =
            serde_json::to_vec(&record).map_err(|e| format!("serializing an audit record: {e}"))?;
        line.push(b'\n');
        self.ensure_capacity(state, line.len() as u64)?;
        // Anchor the prospective tail before the record becomes visible. If
        // the process dies between the checkpoint and the append, the missing
        // record is detected; if an attacker removes the last record later,
        // the checkpoint no longer matches. The prune record uses this too, so
        // a failed retention checkpoint still describes the full (not yet
        // deleted) log.
        let first_seq = state
            .sealed
            .first()
            .map(|segment| segment.first_seq)
            .or(Some(state.active_first_seq));
        write_checkpoint_snapshot(
            &self.inner.checkpoint_path,
            first_seq,
            Some(seq),
            state.records + 1,
            &hash,
            &state.pruned_heads,
        )?;
        let Some(file) = state.active.as_mut() else {
            return Err("no active audit segment".to_string());
        };
        file.write_all(&line)
            .and_then(|_| file.sync_all())
            .map_err(|e| format!("writing an audit record: {e}"))?;
        state.next_seq = seq + 1;
        state.prev_hash = hash;
        state.records += 1;
        state.active_records += 1;
        state.first_seq.get_or_insert(seq);
        state.active_bytes += line.len() as u64;
        state.active_last_ts = Some(now);
        if allow_prune {
            if let Err(e) = self.prune(state) {
                tracing::warn!("audit retention failed: {e}");
            }
        }
        Ok(())
    }

    // -- Read APIs ----------------------------------------------------------

    pub fn query(&self, query: &AuditQuery) -> Result<QueryResponse, String> {
        let filter = parse_filter(query, 100, MAX_FILTER_LIMIT)?;
        let views = self.snapshot_segments()?;
        let records = self.read_records_snapshot(&views)?;
        let mut out = Vec::new();
        let mut truncated = false;
        for value in records {
            if !filter.matches(&value) {
                continue;
            }
            if out.len() >= filter.limit {
                truncated = true;
                break;
            }
            out.push(value);
        }
        Ok(QueryResponse {
            count: out.len(),
            first_seq: out
                .first()
                .and_then(|v| v.get("seq").and_then(Value::as_u64)),
            last_seq: out
                .last()
                .and_then(|v| v.get("seq").and_then(Value::as_u64)),
            truncated,
            records: out,
        })
    }

    pub fn export(&self, query: &AuditQuery) -> Result<String, String> {
        let filter = parse_filter(query, MAX_EXPORT_LIMIT, MAX_EXPORT_LIMIT)?;
        let views = self.snapshot_segments()?;
        let records = self.read_records_snapshot(&views)?;
        let mut out = String::new();
        let mut exported = 0usize;
        for value in records {
            if !filter.matches(&value) {
                continue;
            }
            if exported >= filter.limit {
                break;
            }
            let line = serde_json::to_string(&value)
                .map_err(|e| format!("serializing an audit record: {e}"))?;
            out.push_str(&line);
            out.push('\n');
            exported += 1;
        }
        Ok(out)
    }

    /// Re-verify the retained chain. On failure the log becomes degraded
    /// **before** this returns, so a caller can never observe a failed report
    /// while mutations keep flowing (R-026).
    pub fn verify(&self) -> VerifyReport {
        self.verify_with_transition().0
    }

    /// Like [`verify`](Self::verify), plus whether this call caused the
    /// transition to degraded (the caller alerts exactly once).
    pub fn verify_with_transition(&self) -> (VerifyReport, bool) {
        let report = self.verify_locked();
        let mut transitioned = false;
        if !report.ok {
            let reason = if report.errors.is_empty() {
                "audit chain verification failed".to_string()
            } else {
                report.errors.join("; ")
            };
            transitioned = self.mark_degraded(&reason);
            if transitioned {
                tracing::error!("audit chain verification failed: {reason}");
            }
        } else {
            // Explicit recovery path: a chain that verifies and a volume with
            // room again clears a capacity-only degradation without a restart.
            let mut state = self.inner.state.lock().unwrap();
            if state.degraded.is_some() && state.degraded_retryable && self.capacity_reclaimed(0) {
                state.degraded = None;
                state.degraded_retryable = false;
                tracing::info!("audit log recovered: the volume has free space again");
            }
        }
        (report, transitioned)
    }

    fn verify_locked(&self) -> VerifyReport {
        let _state = self.inner.state.lock().unwrap();
        let paths = match segment_paths(&self.inner.segments_dir) {
            Ok(paths) => paths,
            Err(e) => {
                return VerifyReport {
                    ok: false,
                    first_seq: None,
                    last_seq: None,
                    first_hash: None,
                    last_hash: None,
                    records: 0,
                    errors: vec![e],
                }
            }
        };
        let checkpoint = match load_checkpoint(&self.inner.checkpoint_path) {
            Ok(checkpoint) => checkpoint,
            Err(e) => {
                return VerifyReport {
                    ok: false,
                    first_seq: None,
                    last_seq: None,
                    first_hash: None,
                    last_hash: None,
                    records: 0,
                    errors: vec![e],
                }
            }
        };
        let (mut report, _) = verify_paths(&paths, &checkpoint);
        if !self.inner.checkpoint_path.exists() && !paths.is_empty() {
            report.errors.push(
                "audit checkpoint is missing; refusing to trust an unanchored log".to_string(),
            );
            report.ok = false;
        }
        report
    }

    /// Snapshot the stable byte length of every segment while holding the
    /// append lock, then read the files without it. Writers only update
    /// `active_bytes` after a complete fsynced record, so a reader never sees
    /// a half-written line even if the file grows concurrently.
    #[cfg(test)]
    fn read_records(&self) -> Result<Vec<Value>, String> {
        let views = self.snapshot_segments()?;
        self.read_records_snapshot(&views)
    }

    fn snapshot_segments(&self) -> Result<Vec<SegmentView>, String> {
        let state = self.inner.state.lock().unwrap();
        let paths = segment_paths(&self.inner.segments_dir)?;
        let mut views = Vec::with_capacity(paths.len());
        for path in paths {
            let len = if path == state.active_path {
                state.active_bytes
            } else {
                std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0)
            };
            views.push(SegmentView { path, len });
        }
        Ok(views)
    }

    fn read_records_snapshot(&self, views: &[SegmentView]) -> Result<Vec<Value>, String> {
        let mut out = Vec::new();
        for view in views {
            let file = match File::open(&view.path) {
                Ok(file) => file,
                // Retention may remove a sealed segment between the snapshot
                // and the read; it is outside the query window now.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    return Err(format!(
                        "reading audit segment {}: {e}",
                        view.path.display()
                    ))
                }
            };
            let mut buf = Vec::with_capacity(view.len.min(1024 * 1024) as usize);
            file.take(view.len)
                .read_to_end(&mut buf)
                .map_err(|e| format!("reading audit segment {}: {e}", view.path.display()))?;
            let text = String::from_utf8_lossy(&buf);
            for line in text.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                let value: Value = serde_json::from_str(line)
                    .map_err(|e| format!("parsing audit segment {}: {e}", view.path.display()))?;
                out.push(value);
            }
        }
        Ok(out)
    }
}

#[derive(Debug)]
struct SegmentView {
    path: PathBuf,
    len: u64,
}

// ---------------------------------------------------------------------------
// Record helpers
// ---------------------------------------------------------------------------

fn record_value(
    pending: &Pending,
    seq: u64,
    prev: &str,
    phase: &str,
    outcome: &Outcome,
    now: DateTime<Utc>,
) -> Value {
    let mut action = Map::new();
    action.insert(
        "name".to_string(),
        Value::String(pending.action.name.clone()),
    );
    action.insert(
        "target".to_string(),
        pending
            .action
            .target
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    action.insert("phase".to_string(), Value::String(phase.to_string()));
    action.insert("params".to_string(), pending.action.params.clone());

    let mut map = Map::new();
    map.insert("v".to_string(), json!(RECORD_VERSION));
    map.insert("seq".to_string(), json!(seq));
    map.insert("id".to_string(), Value::String(random_hex(16)));
    map.insert(
        "ts".to_string(),
        Value::String(now.to_rfc3339_opts(SecondsFormat::Nanos, true)),
    );
    map.insert(
        "requestId".to_string(),
        Value::String(pending.request_id.clone()),
    );
    map.insert("prev".to_string(), Value::String(prev.to_string()));
    map.insert(
        "actor".to_string(),
        serde_json::to_value(&pending.actor).unwrap_or(Value::Null),
    );
    map.insert(
        "source".to_string(),
        serde_json::to_value(&pending.source).unwrap_or(Value::Null),
    );
    map.insert("action".to_string(), Value::Object(action));
    map.insert(
        "outcome".to_string(),
        serde_json::to_value(outcome).unwrap_or(Value::Null),
    );
    map.insert(
        "correlation".to_string(),
        serde_json::to_value(&pending.correlation).unwrap_or(Value::Null),
    );
    Value::Object(map)
}

fn end_outcome(status: u16, elapsed: Option<std::time::Duration>) -> Outcome {
    let (state, code) = match status {
        401 | 403 => ("denied", Some("forbidden")),
        500..=599 => ("failed", Some("server_error")),
        400..=499 => ("failed", Some("client_error")),
        _ => ("succeeded", None),
    };
    Outcome {
        state: state.to_string(),
        http_status: Some(status),
        error_code: code.map(str::to_string),
        error: None,
        duration_ms: elapsed.map(|d| d.as_millis().min(u64::MAX as u128) as u64),
    }
}

/// SHA-256 over the canonical JSON (BTreeMap-ordered) record with `hash`
/// omitted.
fn canonical_hash(value: &Value) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    let digest = Sha256::digest(&bytes);
    format!("sha256:{}", hex(digest.as_ref()))
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn random_hex(len: usize) -> String {
    let mut bytes = vec![0u8; len];
    let _ = getrandom::fill(&mut bytes);
    hex(&bytes)
}

/// Recursive redactor for structured values copied into `action.params`.
pub fn redact_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if forbidden_key(key) {
                    *child = Value::String("<redacted>".to_string());
                } else {
                    redact_value(child);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_value(item);
            }
        }
        _ => {}
    }
}

fn forbidden_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    [
        "password",
        "passphrase",
        "secret",
        "token",
        "key",
        "credential",
        "authorization",
        "cookie",
        "hash",
        "private",
        "seed",
        "kek",
        "dek",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn sanitize_text(value: &str, max_chars: usize) -> String {
    value
        .chars()
        .take(max_chars)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

// ---------------------------------------------------------------------------
// Verification and segment metadata
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyReport {
    pub ok: bool,
    pub first_seq: Option<u64>,
    pub last_seq: Option<u64>,
    pub first_hash: Option<String>,
    pub last_hash: Option<String>,
    pub records: u64,
    pub errors: Vec<String>,
}

fn verify_paths(paths: &[PathBuf], checkpoint: &Checkpoint) -> (VerifyReport, Vec<SegmentMeta>) {
    let mut report = VerifyReport {
        ok: true,
        first_seq: None,
        last_seq: None,
        first_hash: None,
        last_hash: None,
        records: 0,
        errors: Vec::new(),
    };
    let mut metas = Vec::new();
    let mut expected_seq: Option<u64> = None;
    let mut previous_hash: Option<String> = None;

    for path in paths {
        let metadata = std::fs::metadata(path).ok();
        let bytes = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
        let fallback_first = segment_seq(path).unwrap_or(0);
        let mut first_seq = fallback_first;
        let mut segment_records = 0u64;
        let mut last_ts: Option<DateTime<Utc>> = None;
        let mut last_hash = String::new();
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) => {
                report
                    .errors
                    .push(format!("reading {}: {e}", path.display()));
                continue;
            }
        };
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let value: Value = match serde_json::from_str(line) {
                Ok(value) => value,
                Err(e) => {
                    report
                        .errors
                        .push(format!("{}: unparseable record: {e}", path.display()));
                    continue;
                }
            };
            let seq = value.get("seq").and_then(Value::as_u64);
            let hash = value.get("hash").and_then(Value::as_str);
            let prev = value.get("prev").and_then(Value::as_str);
            let (Some(seq), Some(hash), Some(prev)) = (seq, hash, prev) else {
                report.errors.push(format!(
                    "{}: record is missing seq/hash/prev",
                    path.display()
                ));
                continue;
            };
            if segment_records == 0 {
                first_seq = seq;
            }
            if let Some(expected) = expected_seq {
                if seq != expected {
                    report.errors.push(format!(
                        "{}: sequence {seq} does not follow {expected}",
                        path.display()
                    ));
                }
            } else if prev != ZERO_HASH
                && !checkpoint.pruned_heads.iter().any(|known| known == prev)
            {
                report.errors.push(format!(
                    "{}: first retained record points at unknown prev {prev}",
                    path.display()
                ));
            }
            if let Some(previous) = &previous_hash {
                if prev != previous {
                    report.errors.push(format!(
                        "{}: sequence {seq} prev does not match the previous record",
                        path.display()
                    ));
                }
            }
            let mut without_hash = value.clone();
            if let Some(object) = without_hash.as_object_mut() {
                object.remove("hash");
            }
            let actual = canonical_hash(&without_hash);
            if actual != hash {
                report.errors.push(format!(
                    "{}: sequence {seq} hash does not match its contents",
                    path.display()
                ));
            }
            if report.first_seq.is_none() {
                report.first_seq = Some(seq);
                report.first_hash = Some(hash.to_string());
            }
            report.last_seq = Some(seq);
            report.last_hash = Some(hash.to_string());
            report.records += 1;
            expected_seq = Some(seq.saturating_add(1));
            previous_hash = Some(hash.to_string());
            segment_records += 1;
            last_hash = hash.to_string();
            last_ts = value
                .get("ts")
                .and_then(Value::as_str)
                .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
                .map(|ts| ts.with_timezone(&Utc));
        }
        if segment_records > 0 {
            metas.push(SegmentMeta {
                first_seq,
                path: path.clone(),
                bytes,
                records: segment_records,
                last_ts,
                last_hash,
            });
        } else if path.exists() {
            // A rotation can leave the newest segment empty (for example a
            // crash before its first record). Keep it as the active segment
            // so the next append does not reopen the previous one.
            metas.push(SegmentMeta {
                first_seq: fallback_first,
                path: path.clone(),
                bytes,
                records: 0,
                last_ts: None,
                last_hash: String::new(),
            });
        }
    }
    report.errors.extend(checkpoint_errors(checkpoint, &report));
    if report.errors.len() > MAX_ERRORS {
        report.errors.truncate(MAX_ERRORS);
        report
            .errors
            .push("additional verification errors omitted".to_string());
    }
    report.ok = report.errors.is_empty();
    (report, metas)
}

/// The checkpoint is the durable tail anchor. Because it is written before
/// the record it describes, it must match the retained log exactly: a shorter
/// log is a truncated/rolled-back tail, a longer log is an unanchored tail,
/// and a different head hash is tampering.
fn checkpoint_errors(checkpoint: &Checkpoint, report: &VerifyReport) -> Vec<String> {
    let has_anchor =
        checkpoint.last_seq.is_some() || checkpoint.records > 0 || !checkpoint.head_hash.is_empty();
    if !has_anchor {
        return if report.records == 0 {
            Vec::new()
        } else {
            vec!["audit checkpoint is missing; refusing to trust an unanchored log".to_string()]
        };
    }
    if checkpoint.last_seq == Some(0) && report.records == 0 && checkpoint.records == 0 {
        // A freshly opened, still-empty log.
        return Vec::new();
    }
    let mut errors = Vec::new();
    match (checkpoint.last_seq, report.last_seq) {
        (Some(expected), Some(actual)) => {
            if expected != actual {
                errors.push(format!(
                    "audit checkpoint expects the log to end at sequence {expected}, but it ends at {actual}"
                ));
            } else if !checkpoint.head_hash.is_empty()
                && report.last_hash.as_deref() != Some(checkpoint.head_hash.as_str())
            {
                errors.push("audit checkpoint head hash does not match the log tail".to_string());
            }
        }
        (Some(expected), None) => errors.push(format!(
            "audit checkpoint expects the log to end at sequence {expected}, but the log is empty"
        )),
        (None, Some(actual)) => errors.push(format!(
            "audit checkpoint does not anchor the log tail at sequence {actual}"
        )),
        (None, None) => {}
    }
    errors
}

fn segment_paths(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut paths = Vec::new();
    let entries = std::fs::read_dir(dir)
        .map_err(|e| format!("listing audit segments {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("reading an audit segment entry: {e}"))?;
        let path = entry.path();
        if segment_seq(&path).is_some() {
            paths.push(path);
        }
    }
    paths.sort_by_key(|path| segment_seq(path).unwrap_or(0));
    Ok(paths)
}

fn segment_seq(path: &Path) -> Option<u64> {
    let name = path.file_name()?.to_str()?;
    let digits = name.strip_prefix("audit-")?.strip_suffix(".jsonl")?;
    digits.parse::<u64>().ok()
}

fn segment_path(dir: &Path, seq: u64) -> PathBuf {
    dir.join(format!("audit-{seq:020}.jsonl"))
}

fn create_segment(path: &Path) -> Result<File, String> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("creating audit segment {}: {e}", path.display()))?;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    let _ = sync_dir(path.parent().unwrap_or_else(|| Path::new(".")));
    Ok(file)
}

fn set_dir_mode(path: &Path) -> Result<(), String> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("setting audit directory mode on {}: {e}", path.display()))
}

fn probe_writable(dir: &Path) -> Result<(), String> {
    let probe = dir.join(".audit-write-probe");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&probe)
        .map_err(|e| {
            format!(
                "audit storage {} is not writable (fail closed): {e}",
                dir.display()
            )
        })?;
    file.write_all(b"probe")
        .and_then(|_| file.sync_all())
        .map_err(|e| format!("audit storage {} is not writable: {e}", dir.display()))?;
    drop(file);
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

fn sync_dir(dir: &Path) -> std::io::Result<()> {
    File::open(dir)?.sync_all()
}

fn load_checkpoint(path: &Path) -> Result<Checkpoint, String> {
    match std::fs::read(path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|e| format!("parsing {}: {e}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Checkpoint {
            v: RECORD_VERSION,
            ..Checkpoint::default()
        }),
        Err(e) => Err(format!("reading {}: {e}", path.display())),
    }
}

fn write_checkpoint_locked(path: &Path, state: &AuditState) -> Result<(), String> {
    let first_seq =
        state
            .sealed
            .first()
            .map(|segment| segment.first_seq)
            .or(if state.active_bytes > 0 {
                Some(state.active_first_seq)
            } else {
                state.first_seq
            });
    write_checkpoint_snapshot(
        path,
        first_seq,
        state.next_seq.checked_sub(1),
        state.records,
        if state.prev_hash.is_empty() {
            ZERO_HASH
        } else {
            &state.prev_hash
        },
        &state.pruned_heads,
    )
}

fn write_checkpoint_snapshot(
    path: &Path,
    first_seq: Option<u64>,
    last_seq: Option<u64>,
    records: u64,
    head_hash: &str,
    pruned_heads: &[String],
) -> Result<(), String> {
    let checkpoint = Checkpoint {
        v: RECORD_VERSION,
        first_seq,
        last_seq,
        records,
        head_hash: head_hash.to_string(),
        pruned_heads: pruned_heads.to_vec(),
        updated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true),
    };
    let data = serde_json::to_vec_pretty(&checkpoint)
        .map_err(|e| format!("serializing the audit checkpoint: {e}"))?;
    let path = path
        .to_str()
        .ok_or_else(|| "audit checkpoint path is not valid UTF-8".to_string())?;
    crate::chartsrepo::write_atomic(path, &data, 0o600, 0o700)?;
    if let Some(dir) = Path::new(path).parent() {
        let _ = sync_dir(dir);
    }
    Ok(())
}

/// Whether an append failure is the transient "audit volume is full" refusal
/// (`ensure_capacity`) rather than a durable write or chain failure.
fn is_capacity_refusal(reason: &str) -> bool {
    reason.contains(CAPACITY_REFUSAL_MARKER)
}

fn available_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `statvfs` writes into the provided struct and `c_path` is a
    // valid NUL-terminated C string for the duration of the call.
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    Some((stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
}

// ---------------------------------------------------------------------------
// HTTP middleware and query API
// ---------------------------------------------------------------------------

/// The security-relevant reads (FR-AUD-09) an operator needs to inspect the
/// log and recover it. These keep working while the log is degraded; only the
/// record of the read itself is best effort.
fn is_security_read(method: &Method, path: &str) -> bool {
    matches!(*method, Method::GET | Method::HEAD)
        && (path == "/api/audit"
            || path.starts_with("/api/audit/")
            || path == "/api/system/config/export")
}

/// Whether a request must produce a durable begin/end pair: everything unsafe
/// plus the security-relevant audit reads (FR-AUD-09).
fn should_audit(method: &Method, path: &str) -> bool {
    crate::rbac::is_unsafe(method) || is_security_read(method, path)
}

/// The request id attached by [`request_id_middleware`]. It is correlation
/// only, never authorization.
#[derive(Debug, Clone)]
pub struct RequestId(pub String);

/// Outermost middleware: accept/validate a client `X-Request-ID` (or generate
/// one), attach it to the request, and echo it on every response, including
/// public routes and `401`s. The audit middleware reuses this value so the
/// record and the response header always match.
pub async fn request_id_middleware(mut req: Request, next: Next) -> Response {
    let id = request_id(&req);
    req.extensions_mut().insert(RequestId(id.clone()));
    let response = next.run(req).await;
    with_request_id(response, &id)
}

/// Request correlation: accept a well-formed client id, otherwise generate
/// one. The value is never used for authorization.
fn request_id(req: &Request) -> String {
    if let Some(value) = req
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
    {
        let value = value.trim();
        if valid_request_id(value) {
            return value.to_string();
        }
    }
    random_hex(16)
}

fn valid_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

/// Audit middleware. It runs after `require_auth` and before `require_role`,
/// so a denied mutation is still recorded with the authenticated actor; the
/// end record wraps the role check and the handler.
pub async fn middleware(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let request_id = req
        .extensions()
        .get::<RequestId>()
        .map(|id| id.0.clone())
        .unwrap_or_else(|| request_id(&req));
    let audit = should_audit(&method, &path);

    let pending = if audit {
        let Some(user) = crate::auth::user_from_request(&req) else {
            // require_auth is outer, so this only happens on a mis-wired
            // router; do not invent an actor.
            return next.run(req).await;
        };
        let template = crate::rbac::route_template(&method, &path);
        let peer = req
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|info| info.0.ip().to_string());
        let user_agent = req
            .headers()
            .get(header::USER_AGENT)
            .and_then(|value| value.to_str().ok())
            .map(|value| sanitize_text(value, 160));
        Some(Pending::user(
            &user,
            &method,
            &path,
            template,
            peer,
            user_agent,
            request_id.clone(),
        ))
    } else {
        None
    };

    let audit_read = is_security_read(&method, &path);
    let response = match pending {
        None => next.run(req).await,
        Some(pending) => {
            let fallback = pending.clone();
            match state.audit.begin_async(pending).await {
                Ok(pending) => {
                    let response = next.run(req).await;
                    if let Err(e) = state
                        .audit
                        .finish_async(&pending, response.status().as_u16())
                        .await
                    {
                        if state.audit.note_write_failure(&e) {
                            spawn_degraded_alert(state.clone(), e.clone());
                        }
                        if !audit_read {
                            return with_request_id(
                                crate::json(
                                    StatusCode::SERVICE_UNAVAILABLE,
                                    json!({
                                        "error": format!(
                                            "audit_write_failed: {e}; the action may have been applied"
                                        )
                                    }),
                                ),
                                &request_id,
                            );
                        }
                    }
                    response
                }
                Err(e) => {
                    if state.audit.note_write_failure(&e) {
                        spawn_degraded_alert(state.clone(), e.clone());
                    }
                    if audit_read {
                        // Reads stay available while the chain is degraded so
                        // an operator can inspect and repair it. The read's
                        // own record is best effort.
                        let response = next.run(req).await;
                        let _ = state
                            .audit
                            .record_once_async(&fallback, response.status().as_u16())
                            .await;
                        return with_request_id(response, &request_id);
                    }
                    return with_request_id(
                        crate::json(
                            StatusCode::SERVICE_UNAVAILABLE,
                            json!({
                                "error": "audit unavailable: mutations are refused until the audit log is repaired"
                            }),
                        ),
                        &request_id,
                    );
                }
            }
        }
    };
    with_request_id(response, &request_id)
}

fn with_request_id(mut response: Response, request_id: &str) -> Response {
    if let Ok(value) = HeaderValue::from_str(request_id) {
        response.headers_mut().insert("x-request-id", value);
    }
    response
}

/// Raise one alert when the audit log transitions to degraded. Delivery is
/// spawned so a slow alert sink cannot hold the refusing request open.
pub fn spawn_degraded_alert(state: Arc<AppState>, reason: String) {
    tokio::spawn(async move {
        let notif = crate::notifications::Notification {
            title: "Naslos audit log degraded".to_string(),
            message: format!(
                "Mutating API requests are refused until the audit log is repaired. {reason}"
            ),
            severity: crate::notifications::SEVERITY_CRITICAL.to_string(),
            tags: vec!["warning".to_string()],
        };
        if let Err(e) = state.notifications.send(&notif).await {
            tracing::warn!("audit alert via ntfy failed: {e}");
        }
        if let Err(e) = state
            .webhook
            .send(crate::notifications::EVENT_AUDIT, &notif)
            .await
        {
            tracing::warn!("audit alert via webhook failed: {e}");
        }
    });
}

/// Query parameters for `GET /api/audit` and `GET /api/audit/export`.
#[derive(Debug, Default, Deserialize, Clone)]
pub struct AuditQuery {
    pub from: Option<String>,
    pub to: Option<String>,
    pub actor: Option<String>,
    pub action: Option<String>,
    pub outcome: Option<String>,
    pub limit: Option<String>,
}

#[derive(Debug, Default)]
struct AuditFilter {
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    actor: Option<String>,
    action: Option<String>,
    outcome: Option<String>,
    limit: usize,
}

fn parse_filter(
    query: &AuditQuery,
    default_limit: usize,
    max_limit: usize,
) -> Result<AuditFilter, String> {
    let from = parse_bound(query.from.as_deref(), "from")?;
    let to = parse_bound(query.to.as_deref(), "to")?;
    let limit = match query.limit.as_deref() {
        None | Some("") => default_limit,
        Some(raw) => raw
            .trim()
            .parse::<usize>()
            .map_err(|_| "limit must be a positive integer".to_string())?,
    };
    if limit == 0 {
        return Err("limit must be at least 1".to_string());
    }
    Ok(AuditFilter {
        from,
        to,
        actor: query.actor.clone().filter(|value| !value.is_empty()),
        action: query.action.clone().filter(|value| !value.is_empty()),
        outcome: query.outcome.clone().filter(|value| !value.is_empty()),
        limit: limit.min(max_limit),
    })
}

fn parse_bound(raw: Option<&str>, name: &str) -> Result<Option<DateTime<Utc>>, String> {
    let Some(raw) = raw.filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };
    DateTime::parse_from_rfc3339(raw.trim())
        .map(|value| Some(value.with_timezone(&Utc)))
        .map_err(|_| format!("{name} must be an RFC 3339 timestamp"))
}

impl AuditFilter {
    fn matches(&self, record: &Value) -> bool {
        let ts = record
            .get("ts")
            .and_then(Value::as_str)
            .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
            .map(|value| value.with_timezone(&Utc));
        if let Some(from) = self.from {
            if ts.map(|value| value < from).unwrap_or(true) {
                return false;
            }
        }
        if let Some(to) = self.to {
            if ts.map(|value| value > to).unwrap_or(true) {
                return false;
            }
        }
        if let Some(actor) = &self.actor {
            if record
                .pointer("/actor/name")
                .and_then(Value::as_str)
                .map(|value| value != actor)
                .unwrap_or(true)
            {
                return false;
            }
        }
        if let Some(action) = &self.action {
            if record
                .pointer("/action/name")
                .and_then(Value::as_str)
                .map(|value| value != action)
                .unwrap_or(true)
            {
                return false;
            }
        }
        if let Some(outcome) = &self.outcome {
            if record
                .pointer("/outcome/state")
                .and_then(Value::as_str)
                .map(|value| value != outcome)
                .unwrap_or(true)
            {
                return false;
            }
        }
        true
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryResponse {
    pub records: Vec<Value>,
    pub count: usize,
    pub first_seq: Option<u64>,
    pub last_seq: Option<u64>,
    pub truncated: bool,
}

/// `GET /api/audit` — admin-only, bounded query.
pub async fn list(State(state): State<Arc<AppState>>, Query(query): Query<AuditQuery>) -> Response {
    let audit = state.audit.clone();
    match tokio::task::spawn_blocking(move || audit.query(&query)).await {
        Ok(Ok(response)) => crate::json(
            StatusCode::OK,
            serde_json::to_value(response).unwrap_or_default(),
        ),
        Ok(Err(e)) => crate::json(StatusCode::BAD_REQUEST, json!({ "error": e })),
        Err(e) => crate::json(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "error": format!("audit query task failed: {e}") }),
        ),
    }
}

/// `GET /api/audit/verify` — re-verify the retained chain on demand.
pub async fn verify(State(state): State<Arc<AppState>>) -> Response {
    let audit = state.audit.clone();
    match tokio::task::spawn_blocking(move || audit.verify_with_transition()).await {
        Ok((report, transitioned)) => {
            if transitioned {
                let reason = state
                    .audit
                    .degraded_reason()
                    .unwrap_or_else(|| "audit chain verification failed".to_string());
                spawn_degraded_alert(state.clone(), reason);
            }
            crate::json(
                StatusCode::OK,
                serde_json::to_value(report).unwrap_or_default(),
            )
        }
        Err(e) => crate::json(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "error": format!("audit verify task failed: {e}") }),
        ),
    }
}

/// `GET /api/audit/export` — admin-only JSONL export. The read itself is
/// audited by the middleware.
pub async fn export(
    State(state): State<Arc<AppState>>,
    Query(query): Query<AuditQuery>,
) -> Response {
    let audit = state.audit.clone();
    match tokio::task::spawn_blocking(move || audit.export(&query)).await {
        Ok(Ok(body)) => (
            [
                (header::CONTENT_TYPE, "application/x-ndjson"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"naslos-audit.jsonl\"",
                ),
            ],
            body,
        )
            .into_response(),
        Ok(Err(e)) => crate::json(StatusCode::BAD_REQUEST, json!({ "error": e })),
        Err(e) => crate::json(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "error": format!("audit export task failed: {e}") }),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(name: &str, groups: &[&str]) -> UserInfo {
        UserInfo {
            username: name.to_string(),
            groups: groups.iter().map(|group| group.to_string()).collect(),
            email: String::new(),
            display_name: String::new(),
        }
    }

    fn begin_post(log: &AuditLog, request_id: &str) -> Pending {
        let user = user("boss", &["naslos_admins"]);
        let pending = Pending::user(
            &user,
            &Method::POST,
            "/api/shares",
            "/api/shares",
            Some("127.0.0.1".to_string()),
            None,
            request_id.to_string(),
        );
        log.begin(pending).expect("begin record")
    }

    #[test]
    fn record_shape_and_chain() {
        let log = AuditLog::for_test();
        let pending = begin_post(&log, "req-1");
        log.finish(&pending, 201).expect("end record");
        let records = log.read_records().expect("records");
        assert_eq!(records.len(), 2);

        let begin = &records[0];
        assert_eq!(begin["v"], 1);
        assert_eq!(begin["seq"], 1);
        assert_eq!(begin["requestId"], "req-1");
        assert_eq!(begin["prev"], ZERO_HASH);
        assert!(begin["hash"].as_str().unwrap().starts_with("sha256:"));
        assert!(begin["id"].as_str().unwrap().len() >= 16);
        assert!(begin["ts"].as_str().unwrap().contains('T'));
        assert_eq!(begin["actor"]["kind"], "user");
        assert_eq!(begin["actor"]["name"], "boss");
        assert_eq!(begin["actor"]["role"], "admin");
        assert_eq!(begin["actor"]["auth"], "authelia");
        assert_eq!(begin["source"]["method"], "POST");
        assert_eq!(begin["source"]["route"], "/api/shares");
        assert_eq!(begin["action"]["name"], "shares.create");
        assert_eq!(begin["action"]["phase"], "begin");
        assert_eq!(begin["outcome"]["state"], "attempted");

        let end = &records[1];
        assert_eq!(end["seq"], 2);
        assert_eq!(end["prev"], begin["hash"]);
        assert_eq!(end["requestId"], "req-1");
        assert_eq!(end["action"]["phase"], "end");
        assert_eq!(end["outcome"]["state"], "succeeded");
        assert_eq!(end["outcome"]["httpStatus"], 201);
        assert!(end["outcome"]["durationMs"].is_u64());

        let report = log.verify();
        assert!(report.ok, "{:?}", report.errors);
        assert_eq!(report.records, 2);
        assert_eq!(report.first_seq, Some(1));
        assert_eq!(report.last_seq, Some(2));
    }

    #[test]
    fn chain_detects_edited_and_deleted_records() {
        let log = AuditLog::for_test();
        let pending = begin_post(&log, "req-tamper");
        log.finish(&pending, 201).unwrap();
        let path = log
            .inner
            .segments_dir
            .read_dir()
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| segment_seq(path).is_some())
            .expect("segment file");
        let original = std::fs::read_to_string(&path).unwrap();
        let edited = original.replacen("shares.create", "shares.delete", 1);
        std::fs::write(&path, &edited).unwrap();
        let report = log.verify();
        assert!(!report.ok, "an edited record must fail verification");
        assert!(report.errors.iter().any(|error| error.contains("hash")));

        // Restore and remove the first line: the next record's prev no longer
        // has an anchor.
        std::fs::write(&path, &original).unwrap();
        let without_first = original
            .split_once('\n')
            .map(|(_, rest)| rest.to_string())
            .unwrap_or_default();
        std::fs::write(&path, without_first).unwrap();
        let report = log.verify();
        assert!(!report.ok, "a deleted record must fail verification");
        assert!(report
            .errors
            .iter()
            .any(|error| error.contains("prev") || error.contains("sequence")));
    }

    #[test]
    fn truncating_the_active_tail_fails_verification() {
        let log = AuditLog::for_test();
        let pending = begin_post(&log, "req-truncate");
        log.finish(&pending, 201).unwrap();
        assert!(log.verify().ok);
        let path = std::fs::read_dir(&log.inner.segments_dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| segment_seq(path).is_some())
            .expect("segment file");
        let original = std::fs::read_to_string(&path).unwrap();
        let (first, _) = original.split_once('\n').expect("two records");
        std::fs::write(&path, format!("{first}\n")).unwrap();
        let report = log.verify();
        assert!(!report.ok, "a truncated tail must fail verification");
        assert!(
            report
                .errors
                .iter()
                .any(|error| error.contains("checkpoint")),
            "{:?}",
            report.errors
        );
    }

    #[test]
    fn a_missing_checkpoint_fails_verification() {
        let log = AuditLog::for_test();
        let pending = begin_post(&log, "req-no-checkpoint");
        log.finish(&pending, 201).unwrap();
        std::fs::remove_file(&log.inner.checkpoint_path).unwrap();
        let report = log.verify();
        assert!(!report.ok, "an unanchored log must fail verification");
        assert!(
            report
                .errors
                .iter()
                .any(|error| error.contains("checkpoint")),
            "{:?}",
            report.errors
        );
    }

    #[test]
    fn export_returns_more_than_the_query_limit() {
        let log = AuditLog::for_test();
        for index in 0..1005 {
            let pending = Pending::system("test", "audit.test", Some(index.to_string()), json!({}));
            log.record_once(&pending, 200).unwrap();
        }
        let export = log
            .export(&AuditQuery {
                limit: Some("50000".to_string()),
                ..AuditQuery::default()
            })
            .unwrap();
        assert_eq!(export.lines().count(), 1005);
        let query = log
            .query(&AuditQuery {
                limit: Some("50000".to_string()),
                ..AuditQuery::default()
            })
            .unwrap();
        assert_eq!(query.count, MAX_FILTER_LIMIT);
        assert!(query.truncated);
    }

    #[test]
    fn the_export_limit_honours_the_documented_maximum() {
        let query = AuditQuery {
            limit: Some("50000".to_string()),
            ..AuditQuery::default()
        };
        assert_eq!(
            parse_filter(&query, 100, MAX_FILTER_LIMIT).unwrap().limit,
            MAX_FILTER_LIMIT
        );
        assert_eq!(
            parse_filter(&query, MAX_EXPORT_LIMIT, MAX_EXPORT_LIMIT)
                .unwrap()
                .limit,
            MAX_EXPORT_LIMIT
        );
        let default_export =
            parse_filter(&AuditQuery::default(), MAX_EXPORT_LIMIT, MAX_EXPORT_LIMIT).unwrap();
        assert_eq!(default_export.limit, MAX_EXPORT_LIMIT);
    }

    #[test]
    fn survives_restart_and_appends() {
        let dir = tempfile::tempdir().unwrap();
        {
            let log = AuditLog::open(dir.path().to_path_buf(), AuditConfig::for_test()).unwrap();
            let pending = begin_post(&log, "req-restart");
            log.finish(&pending, 201).unwrap();
        }
        let log = AuditLog::open(dir.path().to_path_buf(), AuditConfig::for_test()).unwrap();
        assert!(log.verify().ok);
        assert_eq!(log.read_records().unwrap().len(), 2);
        let pending = begin_post(&log, "req-restart-2");
        log.finish(&pending, 200).unwrap();
        let records = log.read_records().unwrap();
        assert_eq!(records.len(), 4);
        assert_eq!(records[2]["seq"], 3);
        assert!(log.verify().ok);
    }

    #[test]
    fn redacts_canary_secrets_in_params() {
        let mut value = json!({
            "password": "canary-password",
            "nested": {
                "token": "canary-token",
                "apiKey": "canary-key",
                "safe": "kept",
            },
            "items": [{"authorization": "canary-auth", "name": "kept"}],
        });
        redact_value(&mut value);
        let serialized = serde_json::to_string(&value).unwrap();
        for canary in [
            "canary-password",
            "canary-token",
            "canary-key",
            "canary-auth",
        ] {
            assert!(
                !serialized.contains(canary),
                "{canary} leaked: {serialized}"
            );
        }
        assert!(serialized.contains("kept"));

        let log = AuditLog::for_test();
        let pending = Pending::system(
            "audit",
            "audit.test",
            Some("target".to_string()),
            json!({"secret": "canary-system-secret"}),
        );
        log.record_once(&pending, 200).unwrap();
        let raw = log
            .read_records()
            .unwrap()
            .into_iter()
            .map(|value| serde_json::to_string(&value).unwrap())
            .collect::<String>();
        assert!(!raw.contains("canary-system-secret"));
    }

    #[test]
    fn retention_prunes_only_sealed_segments_and_records_it() {
        let dir = tempfile::tempdir().unwrap();
        let config = AuditConfig {
            retention_days: 0,
            max_bytes: 8 * 1024,
            segment_bytes: 1024,
            min_free_bytes: 0,
        };
        let log = AuditLog::open(dir.path().to_path_buf(), config).unwrap();
        for index in 0..24 {
            let mut pending = begin_post(&log, &format!("req-{index}"));
            pending.action.params = json!({"padding": "x".repeat(180)});
            log.finish(&pending, 200).unwrap();
        }
        let records = log.read_records().unwrap();
        assert!(
            records
                .iter()
                .any(|record| record["action"]["name"] == "audit.retention.prune"),
            "pruning must leave an audit.retention.prune record"
        );
        assert!(log.verify().ok, "{:?}", log.verify().errors);
        let state = log.inner.state.lock().unwrap();
        assert_eq!(state.sealed.len(), 1, "the newest sealed segment is kept");
        assert!(
            state.active_bytes > 0,
            "the active segment is never removed"
        );
        drop(state);
        let files = std::fs::read_dir(&log.inner.segments_dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| segment_seq(&entry.path()).is_some())
            .count();
        assert!(files >= 2, "active + newest sealed remain");
        assert!(files < 24, "old segments were pruned");
    }

    #[test]
    fn request_ids_are_validated() {
        assert!(valid_request_id("abc-123_4.5:6"));
        assert!(valid_request_id(&"a".repeat(128)));
        assert!(!valid_request_id(""));
        assert!(!valid_request_id(&"a".repeat(129)));
        assert!(!valid_request_id("space here"));
        assert!(!valid_request_id("bad\nvalue"));
    }

    #[test]
    fn a_degraded_log_still_serves_the_audit_reads() {
        let log = AuditLog::for_test();
        let pending = begin_post(&log, "req-readable");
        log.finish(&pending, 201).unwrap();
        assert!(log.mark_degraded("simulated verification failure"));

        // GET /api/audit and /api/audit/export read the retained segments; a
        // degraded chain must not hide the records an operator needs to
        // diagnose and repair it.
        let query = log.query(&AuditQuery::default()).expect("query");
        assert_eq!(query.count, 2, "{query:?}");
        let export = log.export(&AuditQuery::default()).expect("export");
        assert_eq!(export.lines().count(), 2);
    }

    #[test]
    fn a_capacity_refusal_degrades_retryably_and_recovers() {
        let log = AuditLog::for_test();
        let capacity = "the audit volume is full; refusing mutations until space is reclaimed";
        assert!(log.note_write_failure(capacity), "first transition alerts");
        assert_eq!(log.status(), "degraded");
        assert!(
            log.degraded_is_retryable(),
            "a full volume must not be permanent"
        );

        // Space is reclaimed: the next append re-checks the volume, clears the
        // retryable degradation and succeeds without a restart.
        let pending = begin_post(&log, "req-after-reclaim");
        log.finish(&pending, 201).expect("append after reclaim");
        assert_eq!(log.status(), "ok");
        assert!(!log.degraded_is_retryable());
        assert_eq!(log.read_records().unwrap().len(), 2);

        // Verify is the explicit recovery path for a degraded-but-valid chain.
        assert!(log.note_write_failure(capacity));
        let (report, transitioned) = log.verify_with_transition();
        assert!(report.ok, "{:?}", report.errors);
        assert!(!transitioned);
        assert_eq!(log.status(), "ok", "verify clears a capacity degradation");

        // A real chain failure stays permanent: verify must not clear it.
        assert!(log.mark_degraded("simulated verification failure"));
        let (report, _) = log.verify_with_transition();
        assert!(report.ok);
        assert_eq!(log.status(), "degraded");
        assert!(!log.degraded_is_retryable());
    }

    #[test]
    fn an_io_failure_is_not_retryable() {
        let log = AuditLog::for_test();
        assert!(log.note_write_failure("writing an audit record: disk I/O error"));
        assert!(!log.degraded_is_retryable());
        let err = log
            .begin(pending_post("req-io"))
            .err()
            .expect("permanent degradation");
        assert!(err.contains("degraded"), "{err}");
    }

    async fn insert_user(mut req: Request, next: Next) -> Response {
        req.extensions_mut()
            .insert(std::sync::Arc::new(user("boss", &["naslos_admins"])));
        next.run(req).await
    }

    #[tokio::test]
    async fn audit_http_reads_are_served_while_degraded_but_mutations_are_refused() {
        use axum::body::Body;
        use axum::routing::get;
        use axum::Router;
        use tower::ServiceExt;

        let state = Arc::new(AppState::for_test("secret"));
        state.audit.mark_degraded("simulated verification failure");
        let app = Router::new()
            .route("/api/audit", get(|| async { "records" }))
            .route("/api/shares", axum::routing::post(|| async { "created" }))
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                middleware,
            ))
            .layer(axum::middleware::from_fn(insert_user));

        let read = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/audit")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            read.status(),
            StatusCode::OK,
            "the audit read must stay available while degraded"
        );

        let mutation = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/shares")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            mutation.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "mutations stay fail-closed while degraded"
        );
    }

    #[test]
    fn degraded_log_refuses_begin() {
        let log = AuditLog::for_test();
        assert!(log.mark_degraded("simulated verification failure"));
        assert!(!log.mark_degraded("second transition"));
        let user = user("boss", &["naslos_admins"]);
        let pending = Pending::user(
            &user,
            &Method::POST,
            "/api/shares",
            "/api/shares",
            None,
            None,
            "req-degraded".to_string(),
        );
        let err = log.begin(pending).err().expect("begin must fail");
        assert!(err.contains("degraded"), "{err}");
        assert_eq!(log.status(), "degraded");
        assert!(log.read_records().unwrap().is_empty());
    }

    fn pending_post(request_id: &str) -> Pending {
        let user = user("boss", &["naslos_admins"]);
        Pending::user(
            &user,
            &Method::POST,
            "/api/shares",
            "/api/shares",
            Some("127.0.0.1".to_string()),
            None,
            request_id.to_string(),
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn durable_appends_do_not_block_the_async_runtime() {
        let log = AuditLog::for_test();
        // Stand-in for a slow fsync inside the append path.
        log.set_append_delay(Some(std::time::Duration::from_millis(300)));
        let begin = {
            let log = log.clone();
            tokio::spawn(async move { log.begin_async(pending_post("slow-begin")).await })
        };
        // On a one-worker runtime this timer only runs if the append is on the
        // blocking pool (R-062), not on the worker.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !begin.is_finished(),
            "begin_async must run its fsync off the runtime worker"
        );
        let pending = begin.await.expect("join").expect("begin");
        log.finish_async(&pending, 201).await.expect("finish");
        assert_eq!(log.read_records().unwrap().len(), 2);
    }

    #[test]
    fn tampering_after_startup_degrades_and_refuses_mutations() {
        let dir = tempfile::tempdir().unwrap();
        let log = AuditLog::open(dir.path().to_path_buf(), AuditConfig::for_test()).unwrap();
        let pending = begin_post(&log, "req-good");
        log.finish(&pending, 201).unwrap();
        assert_eq!(log.status(), "ok");

        // Mutate a sealed record on disk after the process trusted the chain.
        let mut tampered = false;
        for entry in std::fs::read_dir(&log.inner.segments_dir)
            .unwrap()
            .flatten()
        {
            let path = entry.path();
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            if let Some(pos) = bytes.windows(4).position(|w| w == b"boss") {
                let mut changed = bytes.clone();
                changed[pos..pos + 4].copy_from_slice(b"bosz");
                std::fs::write(&path, changed).unwrap();
                tampered = true;
                break;
            }
        }
        assert!(tampered, "the fixture segment must contain the actor name");

        let report = log.verify();
        assert!(!report.ok, "verify must fail on the tampered chain");
        assert_eq!(log.status(), "degraded", "verify must mark degraded");
        let err = log
            .begin(pending_post("req-after-tamper"))
            .err()
            .expect("mutations must be refused after tampering");
        assert!(err.contains("degraded"), "{err}");
    }

    #[test]
    fn query_filters_actor_action_and_outcome() {
        let log = AuditLog::for_test();
        let pending = begin_post(&log, "req-query");
        log.finish(&pending, 403).unwrap();
        let query = AuditQuery {
            actor: Some("boss".to_string()),
            outcome: Some("denied".to_string()),
            ..AuditQuery::default()
        };
        let response = log.query(&query).unwrap();
        assert_eq!(response.count, 1);
        assert_eq!(response.records[0]["action"]["phase"], "end");
        let missing = log
            .query(&AuditQuery {
                actor: Some("nobody".to_string()),
                ..AuditQuery::default()
            })
            .unwrap();
        assert_eq!(missing.count, 0);
    }
}
