//! JSON response helpers and error → status mapping (mirrors the Go handlers).

use crate::zfs::validation::ZfsError;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// A JSON control request is tiny; the `zfs receive` stream is the only large
/// body and gets a high but finite cap.
pub const MAX_JSON_BODY_BYTES: usize = 1 << 20; // 1 MiB
pub const MAX_STREAM_BODY_BYTES: u64 = 1 << 40; // 1 TiB

/// Serialize like Go's `json.NewEncoder.Encode`: JSON plus a trailing newline.
pub fn json_response<T: Serialize>(status: StatusCode, value: &T) -> Response {
    let mut body = serde_json::to_vec(value).unwrap_or_default();
    body.push(b'\n');
    (status, [(header::CONTENT_TYPE, "application/json")], body).into_response()
}

/// `{"error": "..."}` with the given status.
pub fn write_error(status: StatusCode, msg: impl Into<String>) -> Response {
    json_response(status, &serde_json::json!({ "error": msg.into() }))
}

/// Caller-fixable input is 400; anything else is a 500 (NAS-003).
pub fn write_client_error(err: &ZfsError) -> Response {
    if err.is_validation() {
        write_error(StatusCode::BAD_REQUEST, err.to_string())
    } else {
        write_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
    }
}

/// Read a JSON body with a cap, mapping any failure to a 400 (the Go decoder
/// returned 400 for every decode error).
// The Err variant is an axum `Response` (>128 bytes); boxing it would ripple
// through every handler for no real benefit here.
#[allow(clippy::result_large_err)]
pub async fn read_json<T: serde::de::DeserializeOwned>(
    body: axum::body::Body,
    limit: usize,
) -> Result<T, Response> {
    let bytes = axum::body::to_bytes(body, limit)
        .await
        .map_err(|e| write_error(StatusCode::BAD_REQUEST, e.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|e| write_error(StatusCode::BAD_REQUEST, e.to_string()))
}
