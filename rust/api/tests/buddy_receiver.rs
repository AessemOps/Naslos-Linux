//! End-to-end buddy receiver flow: enroll a key, then status, a sealed chunk and
//! a signed manifest, all authenticated the way a real sender does.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use chrono::Utc;
use http_body_util::BodyExt;
use naslos_api::buddy::auth::{body_digest, sign_request_for, RequestAuth};
use naslos_api::buddy::envelope::{
    new_stream_prefix, seal_chunk, Manifest, ManifestChunk, CHUNK_PLAIN_SIZE, ENVELOPE_VERSION,
};
use naslos_api::buddy::keys::new_identity;
use naslos_api::buddy::{Authenticator, PeerStore, Store};
use naslos_api::server::buddy::Receiver;
use naslos_api::server::build_router;
use naslos_api::state::AppState;
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;

const TOKEN: &str = "enroll-secret";
const RECEIVER: &str = "naslos-test";

fn state_with_receiver(root: &str) -> Arc<AppState> {
    let peers = Arc::new(PeerStore::new(""));
    let store = Arc::new(Store::new(root));
    let auth = Arc::new(Authenticator::new(peers.clone()));
    let mut receiver = Receiver::new(store, peers, auth, RECEIVER);
    receiver.enroll_token = TOKEN.to_string();
    let mut st = AppState::for_test("proxy");
    st.buddy = Some(Arc::new(receiver));
    Arc::new(st)
}

fn auth_headers(a: &RequestAuth) -> [(&'static str, String); 4] {
    [
        ("X-Buddy-Key", a.key_id.clone()),
        ("X-Buddy-Timestamp", a.timestamp.clone()),
        ("X-Buddy-Nonce", a.nonce.clone()),
        ("X-Buddy-Signature", a.signature.clone()),
    ]
}

async fn send(state: Arc<AppState>, req: Request<Body>) -> (StatusCode, Value) {
    let resp = build_router(state).oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn signed(id: &naslos_api::buddy::Identity, method: &str, uri: &str, body: &[u8]) -> Request<Body> {
    let digest = body_digest(body);
    let auth = sign_request_for(id, method, uri, &digest, RECEIVER, Utc::now()).unwrap();
    let mut b = Request::builder().method(method).uri(uri);
    for (k, v) in auth_headers(&auth) {
        b = b.header(k, v);
    }
    b.body(Body::from(body.to_vec())).unwrap()
}

#[tokio::test]
async fn enroll_status_chunk_and_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().display().to_string();
    let state = state_with_receiver(&root);
    let id = new_identity("peer-a").unwrap();

    // Enroll the key.
    let enroll_body = serde_json::json!({
        "token": TOKEN, "name": "peer-a", "publicKey": id.public_key
    })
    .to_string();
    let (status, body) = send(
        state.clone(),
        Request::builder()
            .method("POST")
            .uri("/api/buddy/v1/enroll")
            .header("content-type", "application/json")
            .body(Body::from(enroll_body))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["name"], "peer-a");

    // Status is authenticated and reports the key.
    let (status, body) = send(
        state.clone(),
        signed(&id, "GET", "/api/buddy/v1/status", b""),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["receiver"], RECEIVER);
    assert_eq!(body["enrollOpen"], true);

    // Upload one sealed chunk.
    let dek = [5u8; 32];
    let prefix = new_stream_prefix().unwrap();
    let source = "naslos-test/data";
    let chain = "chain-1";
    let plain = b"hello buddy receiver";
    let (sealed, digest) = seal_chunk(&dek, &prefix, 0, source, chain, 0, plain).unwrap();
    let chunk_uri = format!("/api/buddy/v1/chunks/{source}?chain={chain}&index=0");
    let (status, body) = send(state.clone(), signed(&id, "PUT", &chunk_uri, &sealed)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // Publish a signed manifest covering it.
    let mut manifest = Manifest {
        version: ENVELOPE_VERSION,
        source: source.to_string(),
        chain: chain.to_string(),
        kind: "zfs-send".to_string(),
        from_snapshot: String::new(),
        to_snapshot: "buddy-1".to_string(),
        from_guid: String::new(),
        to_guid: "guid-1".to_string(),
        created_at: Utc::now(),
        key_id: String::new(),
        sequence: 1,
        generation: 0,
        stream_prefix: base64::engine::general_purpose::STANDARD.encode(&prefix),
        chunk_plain_size: CHUNK_PLAIN_SIZE as i64,
        chunks: vec![ManifestChunk {
            index: 0,
            plain_bytes: plain.len() as i64,
            sealed_bytes: sealed.len() as i64,
            generation: 0,
            sha256_plain: digest,
        }],
        dek_wrapped: base64::engine::general_purpose::STANDARD.encode([0u8; 60]),
        signature: String::new(),
    };
    manifest.sign(&id).unwrap();
    let body = serde_json::to_vec(&manifest).unwrap();
    let (status, resp) = send(
        state.clone(),
        signed(
            &id,
            "PUT",
            &format!("/api/buddy/v1/manifest/{source}"),
            &body,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(resp["chain"], chain);

    // The backup is now visible.
    let (status, resp) = send(
        state,
        signed(&id, "GET", "/api/buddy/v1/manifest/naslos-test/data", b""),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(resp["chain"], chain);
}
