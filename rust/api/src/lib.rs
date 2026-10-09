//! Naslos HTTP API (Rust port of `api/`).
//!
//! Slice S1 (Phase 2 foundation): config, logsafe, auth, metrics and the Talos
//! CLI adapter, plus the full route surface behind the correct auth gates. The
//! four dashboard endpoints and `/api/auth/me` are live; not-yet-ported owner
//! handlers return a documented 501.

pub mod agent;
pub mod apps;
pub mod auth;
pub mod authelia;
pub mod catalog;
pub mod certs;
pub mod chartsrepo;
pub mod config;
pub mod ddns;
pub mod helm;
pub mod identity;
pub mod kube;
pub mod logsafe;
pub mod metrics;
pub mod providers;
pub mod routing;
pub mod server;
pub mod shares;
pub mod state;
pub mod talos;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// Serialize like Go's `json.NewEncoder.Encode`: JSON plus a trailing newline.
pub fn json(status: StatusCode, value: serde_json::Value) -> Response {
    let mut body = serde_json::to_vec(&value).unwrap_or_default();
    body.push(b'\n');
    (
        status,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response()
}
