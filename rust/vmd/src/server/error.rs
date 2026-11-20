//! JSON response helpers (mirrors the agent's error module).

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// A JSON control request is tiny; the upload chunks are the only large
/// bodies and get a high but finite cap.
pub const MAX_JSON_BODY_BYTES: usize = 4 << 20; // 4 MiB

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

/// Read a JSON body with a cap, mapping any failure to a 400.
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
