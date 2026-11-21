//! The MCP settings handlers (FR-MCP). These are ordinary owner routes
//! (Authelia + admin): they configure the endpoint and mint tokens. They are
//! deliberately NOT MCP tools, so a token can never mint tokens.
//!
//! The plaintext token appears exactly once, in the `POST /api/mcp/tokens`
//! response; everything else returns the record without the hash.

use crate::auth::UserInfo;
use crate::json;
use crate::mcp::store::{Scope, TokenRecord};
use crate::state::AppState;
use axum::body::Body;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use std::sync::Arc;

/// The settings view of one token (never the hash).
fn token_public(token: &TokenRecord) -> serde_json::Value {
    serde_json::json!({
        "id": token.id,
        "name": token.name,
        "scope": token.scope.as_str(),
        "createdBy": token.created_by,
        "createdAt": token.created_at,
        "expiresAt": token.expires_at,
        "lastUsedAt": token.last_used_at,
    })
}

/// `GET /api/mcp`
pub async fn mcp_get(State(state): State<Arc<AppState>>) -> Response {
    let endpoint_url = if state.base_domain.is_empty() {
        String::new()
    } else {
        format!("https://{}/mcp", state.base_domain)
    };
    json(
        StatusCode::OK,
        serde_json::json!({
            "enabled": state.mcp.store.enabled(),
            "endpointUrl": endpoint_url,
            "tokens": state.mcp.store.tokens_public().iter().map(token_public).collect::<Vec<_>>(),
            "recentCalls": state.mcp.store.recent_calls(),
        }),
    )
}

/// `PUT /api/mcp` — `{ enabled }`. Disabling keeps the tokens but the guard
/// rejects every request with 404 until it is re-enabled.
pub async fn mcp_put(State(state): State<Arc<AppState>>, body: Body) -> Response {
    #[derive(Default, serde::Deserialize)]
    struct Req {
        #[serde(default)]
        enabled: bool,
    }
    let req: Req = match super::zfs::read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    match state.mcp.store.set_enabled(req.enabled) {
        Ok(()) => json(
            StatusCode::OK,
            serde_json::json!({ "status": "updated", "enabled": req.enabled }),
        ),
        Err(e) => json(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "error": e }),
        ),
    }
}

/// `POST /api/mcp/tokens` — `{ name, scope, expiresInDays? }`.
///
/// Defaults to the `read` scope: a `full` token is an admin without 2FA, so
/// the UI (and any other caller) has to ask for more explicitly.
pub async fn tokens_post(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<Arc<UserInfo>>,
    body: Body,
) -> Response {
    #[derive(Default, serde::Deserialize)]
    struct Req {
        #[serde(default)]
        name: String,
        #[serde(default)]
        scope: String,
        #[serde(default, rename = "expiresInDays")]
        expires_in_days: Option<u64>,
    }
    let req: Req = match super::zfs::read_json(body).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let scope = match req.scope.trim() {
        "" => Scope::Read,
        raw => match Scope::parse(raw) {
            Some(scope) => scope,
            None => {
                return json(
                    StatusCode::BAD_REQUEST,
                    serde_json::json!({ "error": "scope must be one of read, write, full" }),
                )
            }
        },
    };
    match state
        .mcp
        .store
        .create_token(&req.name, scope, req.expires_in_days, &user.username)
    {
        Ok((record, plaintext)) => {
            let mut value = token_public(&record);
            // Shown exactly once; only the hash is stored.
            value["token"] = serde_json::json!(plaintext);
            json(StatusCode::CREATED, value)
        }
        Err(e) => json(StatusCode::BAD_REQUEST, serde_json::json!({ "error": e })),
    }
}

/// `DELETE /api/mcp/tokens/{id}`
pub async fn token_delete(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    match state.mcp.store.revoke(&id) {
        Ok(true) => json(StatusCode::OK, serde_json::json!({ "status": "revoked" })),
        Ok(false) => json(
            StatusCode::NOT_FOUND,
            serde_json::json!({ "error": "no such token" }),
        ),
        Err(e) => json(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "error": e }),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::UserInfo;
    use crate::server::internal_router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn app() -> (axum::Router, Arc<AppState>) {
        let state = Arc::new(AppState::for_test("test-secret"));
        (internal_router(state.clone()), state)
    }

    async fn body_json(response: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn admin_request(method: &str, uri: &str, body: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .extension(Arc::new(UserInfo {
                username: "admin".into(),
                groups: vec!["naslos_admins".into()],
                email: String::new(),
                display_name: String::new(),
            }));
        if body.is_some() {
            builder = builder.header("content-type", "application/json");
        }
        match body {
            Some(text) => builder.body(Body::from(text.to_string())),
            None => builder.body(Body::empty()),
        }
        .unwrap()
    }

    #[tokio::test]
    async fn mcp_get_reports_disabled_by_default_with_no_tokens() {
        let (app, _) = app();
        let resp = app
            .oneshot(admin_request("GET", "/api/mcp", None))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["enabled"], serde_json::json!(false));
        assert_eq!(body["tokens"], serde_json::json!([]));
        // No endpoint on a dev/test state without a domain.
        assert_eq!(body["endpointUrl"], serde_json::json!(""));
    }

    #[tokio::test]
    async fn tokens_post_creates_a_read_token_by_default_and_shows_it_once() {
        let (app, state) = app();
        let resp = app
            .clone()
            .oneshot(admin_request(
                "POST",
                "/api/mcp/tokens",
                Some(r#"{"name":"Drill"}"#),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = body_json(resp).await;
        assert_eq!(body["scope"], serde_json::json!("read"));
        assert_eq!(body["createdBy"], serde_json::json!("admin"));
        let plaintext = body["token"].as_str().unwrap().to_string();
        assert!(plaintext.starts_with("nmcp_"));

        // The token verifies, and the stored form never reappears.
        assert!(
            state.mcp.store.verify(&plaintext).is_none(),
            "disabled by default"
        );
        state.mcp.store.set_enabled(true).unwrap();
        let auth = state.mcp.store.verify(&plaintext).unwrap();
        assert_eq!(auth.name, "Drill");

        let resp = app
            .oneshot(admin_request("GET", "/api/mcp", None))
            .await
            .unwrap();
        let body = body_json(resp).await;
        let tokens = body["tokens"].as_array().unwrap();
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0]["name"], serde_json::json!("Drill"));
        assert!(tokens[0].get("hash").is_none() || tokens[0]["hash"] == serde_json::json!(""));
        assert!(body.as_object().unwrap().get("token").is_none());
    }

    #[tokio::test]
    async fn tokens_post_rejects_bad_scopes_and_duplicate_names() {
        let (app, _) = app();
        let resp = app
            .clone()
            .oneshot(admin_request(
                "POST",
                "/api/mcp/tokens",
                Some(r#"{"name":"a","scope":"king"}"#),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let resp = app
            .clone()
            .oneshot(admin_request(
                "POST",
                "/api/mcp/tokens",
                Some(r#"{"name":"dup","scope":"write"}"#),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let resp = app
            .oneshot(admin_request(
                "POST",
                "/api/mcp/tokens",
                Some(r#"{"name":"dup"}"#),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn revoking_makes_the_token_unusable_and_404s_on_repeat() {
        let (app, state) = app();
        state.mcp.store.set_enabled(true).unwrap();
        let resp = app
            .clone()
            .oneshot(admin_request(
                "POST",
                "/api/mcp/tokens",
                Some(r#"{"name":"gone","scope":"full"}"#),
            ))
            .await
            .unwrap();
        let body = body_json(resp).await;
        let id = body["id"].as_str().unwrap().to_string();
        let plaintext = body["token"].as_str().unwrap().to_string();
        assert_eq!(body["scope"], serde_json::json!("full"));

        let resp = app
            .clone()
            .oneshot(admin_request(
                "DELETE",
                &format!("/api/mcp/tokens/{id}"),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(state.mcp.store.verify(&plaintext).is_none());

        let resp = app
            .oneshot(admin_request(
                "DELETE",
                &format!("/api/mcp/tokens/{id}"),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn mcp_put_toggles_the_endpoint() {
        let (app, state) = app();
        let resp = app
            .clone()
            .oneshot(admin_request(
                "PUT",
                "/api/mcp",
                Some(r#"{"enabled":true}"#),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(state.mcp.store.enabled());

        let resp = app
            .oneshot(admin_request(
                "PUT",
                "/api/mcp",
                Some(r#"{"enabled":false}"#),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(!state.mcp.store.enabled());
    }
}
