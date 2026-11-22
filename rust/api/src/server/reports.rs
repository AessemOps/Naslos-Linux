//! Historical reporting routes (FR-RPT): metadata and downsampled series.

use crate::json;
use crate::reporting::ReportRange;
use crate::state::AppState;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use chrono::Utc;
use serde::Deserialize;
use std::sync::Arc;

#[derive(Debug, Default, Deserialize)]
pub struct SeriesQuery {
    pub range: Option<String>,
    /// `metric` is the documented name; `series` is accepted as the design
    /// doc's older spelling. Both accept a comma-separated list.
    pub metric: Option<String>,
    pub series: Option<String>,
}

/// `GET /api/reports` — the fixed ranges and the series allowlist actually
/// present in the store.
pub async fn reports_get(State(state): State<Arc<AppState>>) -> Response {
    let meta = state.reporting.metadata();
    match serde_json::to_value(meta) {
        Ok(value) => json(StatusCode::OK, value),
        Err(_) => json(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "error": "could not encode reporting metadata" }),
        ),
    }
}

/// `GET /api/reports/series?range=24h|7d|30d&metric=...`
pub async fn reports_series_get(
    State(state): State<Arc<AppState>>,
    Query(query): Query<SeriesQuery>,
) -> Response {
    let Some(range_text) = query.range.as_deref() else {
        return json(
            StatusCode::BAD_REQUEST,
            serde_json::json!({ "error": "range is required (24h, 7d or 30d)" }),
        );
    };
    let Some(range) = ReportRange::parse(range_text) else {
        return json(
            StatusCode::BAD_REQUEST,
            serde_json::json!({ "error": "range must be one of 24h, 7d, 30d" }),
        );
    };

    let mut metrics = Vec::new();
    for raw in [query.metric, query.series].into_iter().flatten() {
        for key in raw.split(',') {
            let key = key.trim();
            if !key.is_empty() {
                metrics.push(key.to_string());
            }
        }
    }
    // Bound the parse surface; the store also caps the selected series at 8
    // and reports everything beyond it as ignored.
    metrics.truncate(64);

    let reporter = state.reporting.clone();
    let now = Utc::now().timestamp();
    match tokio::task::spawn_blocking(move || reporter.query(range, &metrics, now)).await {
        Ok(result) => match serde_json::to_value(result) {
            Ok(value) => json(StatusCode::OK, value),
            Err(_) => json(
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({ "error": "could not encode reporting series" }),
            ),
        },
        Err(e) => {
            tracing::warn!("reporting query task failed: {e}");
            json(
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({ "error": "reporting query failed" }),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth;
    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::http::Request;
    use std::net::SocketAddr;
    use tower::ServiceExt;

    fn state_with_store() -> (Arc<AppState>, tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.bin");
        let mut state = AppState::for_test("test-secret");
        state.reporting = Arc::new(crate::reporting::Reporter::new(
            true,
            path.clone(),
            crate::reporting::DEFAULT_MAX_BYTES,
        ));
        (Arc::new(state), dir, path)
    }

    fn admin_request(uri: &str) -> Request<Body> {
        Request::builder()
            .uri(uri)
            .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 55555u16))))
            .header(auth::PROXY_SECRET_HEADER, "test-secret")
            .header("Remote-User", "boss")
            .header("Remote-Groups", "naslos_admins")
            .body(Body::empty())
            .unwrap()
    }

    async fn body_json(response: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn metadata_lists_the_three_ranges() {
        let (state, _dir, _path) = state_with_store();
        let app = crate::server::build_router(state);
        let response = app.oneshot(admin_request("/api/reports")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["available"], true);
        let ranges = body["ranges"].as_array().unwrap();
        assert_eq!(ranges.len(), 3);
        assert_eq!(ranges[0]["range"], "24h");
        assert_eq!(ranges[1]["resolution"], 300);
        assert_eq!(ranges[2]["range"], "30d");
    }

    #[tokio::test]
    async fn series_requires_a_known_range() {
        let (state, _dir, _path) = state_with_store();
        let app = crate::server::build_router(state);
        for uri in [
            "/api/reports/series",
            "/api/reports/series?range=1h",
            "/api/reports/series?range=86400",
        ] {
            let response = app.clone().oneshot(admin_request(uri)).await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
        }
    }

    #[tokio::test]
    async fn series_returns_points_and_reports_ignored_keys() {
        let (state, _dir, _path) = state_with_store();
        let now = Utc::now().timestamp();
        state
            .reporting
            .record_sample(now - 60, "cpu.usage_percent", "cpu", "percent", 12.5);
        let app = crate::server::build_router(state);
        let response = app
            .oneshot(admin_request(
                "/api/reports/series?range=24h&metric=cpu.usage_percent,not.a.metric",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["available"], true);
        assert_eq!(body["range"], "24h");
        assert_eq!(body["resolution"], 60);
        let series = body["series"].as_array().unwrap();
        assert_eq!(series.len(), 1);
        assert_eq!(series[0]["key"], "cpu.usage_percent");
        assert_eq!(series[0]["unit"], "percent");
        let point = &series[0]["points"][0];
        assert_eq!(point.as_array().unwrap().len(), 6);
        assert!(body["ignored"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "not.a.metric"));
    }

    #[tokio::test]
    async fn series_request_is_capped_at_eight_and_defaults_when_omitted() {
        let (state, _dir, _path) = state_with_store();
        let now = Utc::now().timestamp();
        state
            .reporting
            .record_sample(now - 60, "cpu.usage_percent", "cpu", "percent", 12.5);
        state
            .reporting
            .record_sample(now - 60, "memory.usage_percent", "memory", "percent", 25.0);
        let app = crate::server::build_router(state);
        let response = app
            .clone()
            .oneshot(admin_request("/api/reports/series?range=24h"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["series"].as_array().unwrap().len(), 2);

        let mut uri = String::from("/api/reports/series?range=24h&metric=");
        for index in 0..10 {
            if index > 0 {
                uri.push(',');
            }
            uri.push_str(&format!("cpu.usage_percent.{}", index));
        }
        let response = app.oneshot(admin_request(&uri)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["series"].as_array().unwrap().len(), 0);
        assert_eq!(body["ignored"].as_array().unwrap().len(), 10);
    }

    #[tokio::test]
    async fn a_disabled_store_reports_unavailable_without_an_error_page() {
        let state = Arc::new(AppState::for_test("test-secret"));
        let app = crate::server::build_router(state);
        let response = app
            .oneshot(admin_request("/api/reports/series?range=24h"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["available"], false);
        assert!(body["error"].is_string());
        assert!(body["series"].as_array().unwrap().is_empty());
    }
}
