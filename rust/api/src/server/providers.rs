//! Declarative DNS provider listing (port of `server/ddns.go` provider part,
//! FR-DNS-01).

use crate::json;
use crate::state::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use std::sync::Arc;

/// `GET /api/providers`
pub async fn providers_get(State(state): State<Arc<AppState>>) -> Response {
    let mut views = Vec::new();
    for p in state.providers.list() {
        let mut fields = p.fields.clone();
        if fields.is_empty() {
            fields = Vec::new();
        }
        views.push(serde_json::json!({
            "name": p.name,
            "displayName": p.display_name,
            "description": p.description,
            "icon": p.icon,
            "apiRights": p.api_rights,
            "fields": fields,
            "certManager": p.supports_certificates(),
            "ddns": p.has_ddns(),
            "driver": p.ddns.as_ref().map(|d| d.driver.clone()).unwrap_or_default(),
        }));
    }
    json(
        StatusCode::OK,
        serde_json::json!({ "providers": views, "errors": state.providers.errors() }),
    )
}
