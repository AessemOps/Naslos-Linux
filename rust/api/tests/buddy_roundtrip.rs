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
            save_state: None,
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
                save_state: Some(&|s| s.save(&state_path.display().to_string())),
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
