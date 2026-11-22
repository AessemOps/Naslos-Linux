//! FR-ENC: create/unlock/lock command construction and key hygiene. The key
//! must never reach argv, and the transient keyfile must be gone afterwards.

mod common;

use common::{FakeHost, FakeRunner};
use naslos_agent::zfs::ZfsClient;
use std::collections::HashMap;
use std::sync::Arc;

const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";

fn client(host: &FakeHost, runner: Arc<FakeRunner>) -> ZfsClient {
    ZfsClient::new(runner, host.root.clone())
}

fn raw_options() -> HashMap<String, String> {
    HashMap::from([
        ("encryption".to_string(), "aes-256-gcm".to_string()),
        ("keyformat".to_string(), "raw".to_string()),
    ])
}

fn encrypted_status(keystatus: &str, mounted: &str) -> String {
    format!(
        "encryption\taes-256-gcm\nkeystatus\t{keystatus}\nencryptionroot\ttank/media\nmounted\t{mounted}\n"
    )
}

#[tokio::test]
async fn create_encrypted_dataset_never_puts_the_key_in_argv() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[
        "ERR:cannot open 'tank/media': dataset does not exist\n",
        "",
        &encrypted_status("available", "yes"),
    ]));
    let calls = runner.calls();
    let argv = runner.argv();
    let c = client(&host, runner);

    c.create_dataset_with_key("tank/media", &raw_options(), Some(KEY))
        .await
        .expect("create encrypted dataset");

    let all = calls.lock().unwrap();
    assert_eq!(all.len(), 3, "expected list, create, get; got {all:?}");
    assert_eq!(all[0], "/usr/local/sbin/zfs list -H -o name tank/media");
    assert!(all[1].contains("-o encryption=aes-256-gcm"));
    assert!(all[1].contains("-o keyformat=raw"));
    assert!(
        all[1].contains("-o keylocation=file:///run/naslos-keys/tank/media.key"),
        "{}",
        all[1]
    );
    assert!(all[1].ends_with("tank/media"), "{}", all[1]);
    assert_eq!(all[2], "/usr/local/sbin/zfs get -H -o property,value encryption,keystatus,encryptionroot,mounted tank/media");

    let key_text = String::from_utf8_lossy(KEY);
    for args in argv.lock().unwrap().iter() {
        for arg in args {
            assert!(
                !arg.contains(key_text.as_ref()),
                "the key reached argv: {arg}"
            );
        }
    }
    assert!(
        !host.root.join("run/naslos-keys/tank/media.key").exists(),
        "the transient keyfile must be removed after create"
    );
}

#[tokio::test]
async fn encrypted_create_requires_a_32_byte_key_and_never_runs_zfs() {
    for key in [&KEY[..31], &[0u8; 33][..]] {
        let host = FakeHost::new();
        let runner = Arc::new(FakeRunner::new(&[]));
        let calls = runner.calls();
        let c = client(&host, runner);
        let err = c
            .create_dataset_with_key("tank/media", &raw_options(), Some(key))
            .await
            .expect_err("short/long key");
        assert!(err.to_string().contains("32 bytes"), "{err}");
        assert!(calls.lock().unwrap().is_empty(), "a command ran");
    }

    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[]));
    let calls = runner.calls();
    let c = client(&host, runner);
    let err = c
        .create_dataset_with_key("tank/media", &raw_options(), None)
        .await
        .expect_err("missing key");
    assert!(err.to_string().contains("needs its raw key"), "{err}");
    assert!(calls.lock().unwrap().is_empty(), "a command ran");
}

#[tokio::test]
async fn passphrase_creation_is_refused_before_any_zfs_call() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[]));
    let calls = runner.calls();
    let c = client(&host, runner);
    let options = HashMap::from([
        ("encryption".to_string(), "aes-256-gcm".to_string()),
        ("keyformat".to_string(), "passphrase".to_string()),
    ]);
    let err = c
        .create_dataset_with_key("tank/media", &options, Some(KEY))
        .await
        .expect_err("passphrase");
    assert!(err.to_string().contains("passphrase"), "{err}");
    assert!(calls.lock().unwrap().is_empty(), "a command ran");
}

#[tokio::test]
async fn create_rolls_back_a_dataset_that_did_not_verify() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[
        "ERR:cannot open 'tank/media': dataset does not exist\n",
        "",
        "encryption\toff\nkeystatus\tnone\nencryptionroot\t-\nmounted\tno\n",
        "tank/media\n", // rollback existence check
        "",             // zfs destroy
    ]));
    let calls = runner.calls();
    let c = client(&host, runner);

    let err = c
        .create_dataset_with_key("tank/media", &raw_options(), Some(KEY))
        .await
        .expect_err("verification must fail");
    assert!(err.to_string().contains("did not verify"), "{err}");

    let all = calls.lock().unwrap();
    assert_eq!(all[3], "/usr/local/sbin/zfs list -H -o name tank/media");
    assert_eq!(all[4], "/usr/local/sbin/zfs destroy tank/media");
    assert!(
        !host.root.join("run/naslos-keys/tank/media.key").exists(),
        "the keyfile must be removed on rollback"
    );
}

#[tokio::test]
async fn unlock_loads_the_key_from_the_private_file_and_removes_it() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[
        &encrypted_status("unavailable", "no"),
        "",
        &encrypted_status("available", "yes"),
    ]));
    let calls = runner.calls();
    let argv = runner.argv();
    let c = client(&host, runner);

    let status = c.unlock_dataset("tank/media", KEY).await.expect("unlock");
    assert_eq!(status.keystatus, "available");

    let all = calls.lock().unwrap();
    assert_eq!(all.len(), 3, "expected get, load-key, get; got {all:?}");
    assert_eq!(
        all[1],
        "/usr/local/sbin/zfs load-key -L file:///run/naslos-keys/tank/media.key tank/media"
    );
    let key_text = String::from_utf8_lossy(KEY);
    for args in argv.lock().unwrap().iter() {
        for arg in args {
            assert!(!arg.contains(key_text.as_ref()), "the key reached argv");
        }
    }
    assert!(!host.root.join("run/naslos-keys/tank/media.key").exists());
}

#[tokio::test]
async fn unlock_is_idempotent() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[&encrypted_status("available", "yes")]));
    let calls = runner.calls();
    let c = client(&host, runner);

    let status = c
        .unlock_dataset("tank/media", KEY)
        .await
        .expect("already unlocked");
    assert_eq!(status.keystatus, "available");
    assert_eq!(calls.lock().unwrap().len(), 1, "no load-key must run");
}

#[tokio::test]
async fn lock_is_refused_while_mounted() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[&encrypted_status("available", "yes")]));
    let calls = runner.calls();
    let c = client(&host, runner);

    let err = c.lock_dataset("tank/media").await.expect_err("mounted");
    assert!(err.to_string().contains("mounted"), "{err}");
    assert_eq!(calls.lock().unwrap().len(), 1, "no unload-key must run");
}

#[tokio::test]
async fn lock_refuses_a_mounted_child() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[
        &encrypted_status("available", "no"),
        "tank/media\tno\ntank/media/child\tyes\n",
    ]));
    let calls = runner.calls();
    let c = client(&host, runner);

    let err = c
        .lock_dataset("tank/media")
        .await
        .expect_err("mounted child");
    assert!(err.to_string().contains("mounted child"), "{err}");
    assert_eq!(calls.lock().unwrap().len(), 2, "no unload-key must run");
}

#[tokio::test]
async fn lock_unloads_the_key_and_removes_the_keyfile() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[
        &encrypted_status("available", "no"),
        "tank/media\tno\n",
        "",
        &encrypted_status("unavailable", "no"),
    ]));
    let calls = runner.calls();
    let c = client(&host, runner);

    let status = c.lock_dataset("tank/media").await.expect("lock");
    assert_eq!(status.keystatus, "unavailable");

    let all = calls.lock().unwrap();
    assert_eq!(
        all.len(),
        4,
        "expected get, get -r, unload-key, get; got {all:?}"
    );
    assert_eq!(
        all[1],
        "/usr/local/sbin/zfs get -H -o name,value -r mounted tank/media"
    );
    assert_eq!(all[2], "/usr/local/sbin/zfs unload-key tank/media");
}

#[tokio::test]
async fn lock_is_idempotent() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[&encrypted_status("unavailable", "no")]));
    let calls = runner.calls();
    let c = client(&host, runner);

    let status = c.lock_dataset("tank/media").await.expect("already locked");
    assert_eq!(status.keystatus, "unavailable");
    assert_eq!(calls.lock().unwrap().len(), 1, "no unload-key must run");
}
