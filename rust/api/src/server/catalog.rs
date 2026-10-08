//! Catalog and chart-source handlers (port of `server/apps.go` catalog part
//! and `server/sources.go`).

use crate::chartsrepo::Source;
use crate::json;
use crate::state::{build_catalog, AppState};
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use std::collections::HashMap;
use std::sync::Arc;

fn charts_unavailable() -> Response {
    json(
        StatusCode::SERVICE_UNAVAILABLE,
        serde_json::json!({ "error": "chart repositories are not available" }),
    )
}

#[allow(clippy::result_large_err)]
fn charts(state: &AppState) -> Result<Arc<crate::chartsrepo::Manager>, Response> {
    state.charts.clone().ok_or_else(charts_unavailable)
}

/// `GET /api/catalog`
pub async fn catalog_get(State(state): State<Arc<AppState>>) -> Response {
    match state.catalog.load() {
        Some(catalog) => json(
            StatusCode::OK,
            serde_json::to_value(catalog.list()).unwrap(),
        ),
        None => json(StatusCode::OK, serde_json::json!([])),
    }
}

/// `GET /api/catalog/{name}`
pub async fn catalog_app_get(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    let Some(catalog) = state.catalog.load() else {
        return json(
            StatusCode::NOT_FOUND,
            serde_json::json!({ "error": "catalog is not available" }),
        );
    };
    match catalog.get(&name) {
        Ok(app) => json(StatusCode::OK, serde_json::to_value(app).unwrap()),
        Err(e) => json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e })),
    }
}

/// `GET|POST /api/sources`
pub async fn sources_get(State(state): State<Arc<AppState>>) -> Response {
    let charts = match charts(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    json(
        StatusCode::OK,
        serde_json::to_value(charts.sources()).unwrap(),
    )
}

pub async fn sources_post(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let charts = match charts(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let mut src: Source = match super::zfs::read_json(body).await {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    // Official is reserved for the chart-seeded source.
    src.official = false;
    match charts.add_source(src) {
        Ok(added) => json(StatusCode::CREATED, serde_json::to_value(added).unwrap()),
        Err(e) => json(StatusCode::BAD_REQUEST, serde_json::json!({ "error": e })),
    }
}

/// `POST /api/sources/refresh?name=`
pub async fn sources_refresh(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let charts = match charts(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let name = query
        .get("name")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    if !name.is_empty() {
        let src = match charts.get_source(&name) {
            Ok(s) => s,
            Err(e) => return json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e })),
        };
        let mut errs = Vec::new();
        for channel in src.channel_names() {
            if let Err(e) = charts.refresh(&name, &channel).await {
                errs.push(e);
            }
        }
        state.catalog.store(Arc::new(build_catalog(&charts)));
        if !errs.is_empty() {
            let count = state.catalog.load().map(|c| c.list().len()).unwrap_or(0);
            return json(
                StatusCode::BAD_GATEWAY,
                serde_json::json!({ "status": "partial", "errors": errs, "catalog": count }),
            );
        }
    } else if let Err(e) = charts.refresh_all().await {
        state.catalog.store(Arc::new(build_catalog(&charts)));
        let count = state.catalog.load().map(|c| c.list().len()).unwrap_or(0);
        return json(
            StatusCode::BAD_GATEWAY,
            serde_json::json!({ "status": "partial", "errors": [e], "catalog": count }),
        );
    } else {
        state.catalog.store(Arc::new(build_catalog(&charts)));
    }

    let count = state.catalog.load().map(|c| c.list().len()).unwrap_or(0);
    json(
        StatusCode::OK,
        serde_json::json!({ "status": "refreshed", "catalog": count }),
    )
}

/// `GET|DELETE /api/sources/{name}`
pub async fn source_detail_get(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    let charts = match charts(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    match charts.get_source(&name) {
        Ok(src) => json(StatusCode::OK, serde_json::to_value(src).unwrap()),
        Err(e) => json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e })),
    }
}

pub async fn source_detail_delete(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    let charts = match charts(&state) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    if let Err(e) = charts.delete_source(&name) {
        return json(StatusCode::NOT_FOUND, serde_json::json!({ "error": e }));
    }
    state.catalog.store(Arc::new(build_catalog(&charts)));
    json(
        StatusCode::OK,
        serde_json::json!({ "status": "source removed", "name": name }),
    )
}
