//! Full sender↔receiver round trip: the Rust client pushes a sealed backup to
//! the Rust receiver and restores it, exercising the wire protocol end to end.

use naslos_api::buddy::auth::Authenticator;
use naslos_api::buddy::client::{Client, PushOptions, RestoreOptions};
use naslos_api::buddy::keys::new_identity;
use naslos_api::buddy::{Peer, PeerStore, Store};
use naslos_api::server::buddy::Receiver;
use naslos_api::server::build_router;
use naslos_api::state::AppState;
use std::sync::Arc;

#[tokio::test]
async fn push_then_restore_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Arc::new(new_identity("sender").unwrap());

    let peers = Arc::new(PeerStore::new(""));
    peers
        .add(Peer {
            name: "sender".into(),
            public_key: identity.public_key.clone(),
            enabled: true,
            ..Default::default()
        })
        .unwrap();
    let store = Arc::new(Store::new(&dir.path().display().to_string()));
    let auth = Arc::new(Authenticator::new(peers.clone()));
    let mut receiver = Receiver::new(store, peers, auth, "naslos-test");
    receiver.enroll_token = String::new();

    let mut st = AppState::for_test("proxy");
    st.buddy = Some(Arc::new(receiver));
    let state = Arc::new(st);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = build_router(state);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = Client::new(&format!("http://{addr}"), identity);
    let plaintext: Vec<u8> = (0..(3 * 1024 * 1024u32)).map(|i| (i % 251) as u8).collect();
    let mut reader: &[u8] = &plaintext;
    let (result, err) = client
        .push(PushOptions {
            source: "naslos-test/data",
            kind: "tar",
            reader: &mut reader,
            state: None,
            from_snapshot: "",
            to_snapshot: "snap-1",
            from_guid: "",
            to_guid: "guid-1",
            prune_keep: 0,
            progress: None,
            before_publish: None,
            save_state: &|_| Ok(()),
        })
        .await;
    assert!(err.is_none(), "push failed: {err:?}");
    assert!(
        result.chunks >= 3,
        "expected multiple chunks, got {}",
        result.chunks
    );
    assert_eq!(result.plain_bytes, plaintext.len() as i64);

    let mut out: Vec<u8> = Vec::new();
    let (restored, err) = client
        .restore(RestoreOptions {
            source: "naslos-test/data",
            chain: "",
            out: &mut out,
            progress: None,
            skip_signature_check: false,
            expected_sequence: None,
        })
        .await;
    assert!(err.is_none(), "restore failed: {err:?}");
    assert_eq!(restored.plain_bytes, plaintext.len() as i64);
    assert_eq!(
        out, plaintext,
        "restored bytes must match the original exactly"
    );
    assert_eq!(restored.chunks, result.chunks);
}

#[tokio::test]
async fn failed_push_bumps_and_persists_the_nonce_generation() {
    // SEC-2026-C1: a push that fails after sealing chunks must persist a new
    // generation, so a retry can never re-use a (prefix, generation, index)
    // nonce for different plaintext.
    use naslos_api::buddy::client::ChainState;

    let dir = tempfile::tempdir().unwrap();
    let identity = Arc::new(new_identity("sender").unwrap());
    let peers = Arc::new(PeerStore::new(""));
    peers
        .add(Peer {
            name: "sender".into(),
            public_key: identity.public_key.clone(),
            enabled: true,
            ..Default::default()
        })
        .unwrap();
    let store = Arc::new(Store::new(&dir.path().display().to_string()));
    let auth = Arc::new(Authenticator::new(peers.clone()));
    let receiver = Receiver::new(store, peers, auth, "naslos-test");
    let mut st = AppState::for_test("proxy");
    st.buddy = Some(Arc::new(receiver));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, build_router(Arc::new(st)))
            .await
            .unwrap();
    });

    let client = Client::new(&format!("http://{addr}"), identity);
    let state_dir = tempfile::tempdir().unwrap();
    let state_path = state_dir.path().join("chain.json");
    let source = "naslos-test/data";
    let initial = ChainState::new(source, "tar").unwrap();
    initial.save(&state_path.display().to_string()).unwrap();
    assert_eq!(initial.generation, 0);

    let plaintext = vec![7u8; 2 * 1024 * 1024];
    for expected_generation in [1u16, 2] {
        let persisted = ChainState::load(&state_path.display().to_string()).unwrap();
        let mut reader: &[u8] = &plaintext;
        let (result, err) = client
            .push(PushOptions {
                source,
                kind: "tar",
                reader: &mut reader,
                state: Some(persisted),
                from_snapshot: "",
                to_snapshot: "",
                from_guid: "",
                to_guid: "",
                prune_keep: 0,
                progress: None,
                before_publish: Some(&|_| Err("stop before publish".to_string())),
                save_state: &|s| s.save(&state_path.display().to_string()),
            })
            .await;
        assert!(err.is_some(), "the push must fail at before_publish");
        assert!(result.chunks > 0);
        let reloaded = ChainState::load(&state_path.display().to_string()).unwrap();
        assert_eq!(
            reloaded.generation, expected_generation,
            "each attempt must persist a fresh generation"
        );
    }
}

#[tokio::test]
async fn self_receiver_url_dials_the_loopback_listener() {
    // A receiver base that resolves back to this node must be dialed
    // through the API's own loopback listener: the node's HTTPS port is a
    // hostPort, which Cilium does not answer for pods on the same node, so
    // a self-send through the public URL would time out. The base below
    // points at the "node" (127.0.0.1 via NASLOS_NODE_IP) on the standard
    // entryport while nothing listens on 443 — only the loopback rewrite
    // can make the request land. The receiver's Ed25519 auth must accept
    // it unchanged (the signature covers the path, never the host).
    let dir = tempfile::tempdir().unwrap();
    let identity = Arc::new(new_identity("sender").unwrap());
    let peers = Arc::new(PeerStore::new(""));
    peers
        .add(Peer {
            name: "sender".into(),
            public_key: identity.public_key.clone(),
            enabled: true,
            ..Default::default()
        })
        .unwrap();
    let store = Arc::new(Store::new(&dir.path().display().to_string()));
    let auth = Arc::new(Authenticator::new(peers.clone()));
    let receiver = Receiver::new(store, peers, auth, "naslos-test");
    let mut st = AppState::for_test("proxy");
    st.buddy = Some(Arc::new(receiver));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    std::env::set_var("NASLOS_NODE_IP", "127.0.0.1");
    std::env::set_var("NASLOS_API_PORT", port.to_string());
    tokio::spawn(async move {
        axum::serve(listener, build_router(Arc::new(st)))
            .await
            .unwrap();
    });

    let base = "https://127.0.0.1";
    let client = Client::new(base, identity);
    let status = client.status().await;
    // Clean the process env before asserting so a failure cannot leak the
    // rewrite into other tests (their random ports never match the 80/443
    // entryport guard, but keep the hygiene anyway).
    std::env::remove_var("NASLOS_NODE_IP");
    std::env::remove_var("NASLOS_API_PORT");
    assert!(status.is_ok(), "loopback /status failed: {status:?}");
    assert_eq!(
        client.base_url, base,
        "the stored receiver URL must stay exactly as typed"
    );

    // A different port on the same host must NOT be rewritten: it is not
    // the Traefik entryport, it could be a standalone receiver.
    let node: std::net::IpAddr = "127.0.0.1".parse().unwrap();
    let remote =
        naslos_api::buddy::client::base_points_at_node("http://127.0.0.1:9443", node).await;
    assert!(!remote, "non-entryport receivers must stay remote");
    let entry = naslos_api::buddy::client::base_points_at_node("https://127.0.0.1", node).await;
    assert!(entry, "the standard entryport must count as this node");
}

#[tokio::test]
async fn a_replay_window_refusal_is_retried_with_a_fresh_nonce() {
    // The receiver's replay cache refuses with "retry after the replay
    // window" when a key outruns its unexpired-nonce budget; the sender
    // must re-issue the request with a FRESH nonce instead of failing the
    // whole send. The middleware below rewrites the first chunk PUT into
    // that refusal; the push must still complete, and the retried PUT
    // must carry a different nonce.
    use axum::http::StatusCode;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let dir = tempfile::tempdir().unwrap();
    let identity = Arc::new(new_identity("sender").unwrap());
    let peers = Arc::new(PeerStore::new(""));
    peers
        .add(Peer {
            name: "sender".into(),
            public_key: identity.public_key.clone(),
            enabled: true,
            ..Default::default()
        })
        .unwrap();
    let store = Arc::new(Store::new(&dir.path().display().to_string()));
    let auth = Arc::new(Authenticator::new(peers.clone()));
    let receiver = Receiver::new(store, peers, auth, "naslos-test");
    let mut st = AppState::for_test("proxy");
    st.buddy = Some(Arc::new(receiver));

    let put_count = Arc::new(AtomicUsize::new(0));
    let nonces: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let nonces_seen = nonces.clone();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = build_router(Arc::new(st)).layer(axum::middleware::from_fn(
        move |req: axum::http::Request<axum::body::Body>,
              next: axum::middleware::Next| {
            let put_count = put_count.clone();
            let nonces = nonces.clone();
            async move {
                let is_chunk_put = req.method() == "PUT"
                    && req.uri().path().starts_with("/api/buddy/v1/chunks/");
                let nonce = req
                    .headers()
                    .get("X-Buddy-Nonce")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                let seen = next.run(req).await;
                if is_chunk_put {
                    nonces.lock().unwrap().push(nonce);
                    let n = put_count.fetch_add(1, Ordering::SeqCst);
                    if n == 0 {
                        // The first chunk PUT becomes the retryable refusal.
                        use axum::response::IntoResponse;
                        return (
                            StatusCode::UNAUTHORIZED,
                            axum::Json(serde_json::json!({
                                "error": "too many unexpired signed requests for this key; retry after the replay window"
                            })),
                        )
                            .into_response();
                    }
                }
                seen
            }
        },
    ));
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = Client::new(&format!("http://{addr}"), identity);
    let plaintext = vec![0u8; 256 * 1024];
    let mut reader: &[u8] = &plaintext;
    let (result, err) = client
        .push(PushOptions {
            source: "naslos-test/data",
            kind: "tar",
            reader: &mut reader,
            state: None,
            from_snapshot: "",
            to_snapshot: "snap-1",
            from_guid: "",
            to_guid: "guid-1",
            prune_keep: 0,
            progress: None,
            before_publish: None,
            save_state: &|_| Ok(()),
        })
        .await;
    assert!(
        err.is_none(),
        "push must survive the replay refusal: {err:?}"
    );
    assert!(result.chunks >= 1, "chunks must be stored: {result:?}");
    let nonces = nonces_seen.lock().unwrap().clone();
    assert!(
        nonces.len() >= 2,
        "the first chunk PUT must have been issued twice, saw {nonces:?}"
    );
    assert_ne!(
        nonces[0], nonces[1],
        "the retry must re-sign with a fresh nonce"
    );
}
