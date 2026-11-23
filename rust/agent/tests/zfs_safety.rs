//! Regression tests for the storage destroy-safety work package: safe pool
//! import, create-only encryption rejection, receive stream properties and the
//! per-dataset mutation lock (R-039, R-043, R-044).

mod common;

use common::FakeHost;
use naslos_agent::zfs::backup::{
    foreign_snapshot, receive_command_line, receive_mountpoint, OWNED_SNAPSHOT_PREFIXES,
};
use naslos_agent::zfs::operations::validate_pool_options;
use naslos_agent::zfs::{PoolConfig, ZfsClient};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

fn client(host: &FakeHost, runner: Arc<common::FakeRunner>) -> ZfsClient {
    ZfsClient::new(runner, host.root.clone())
}

#[tokio::test]
async fn import_uses_no_mount_then_remaps_and_mounts_deliberately() {
    let host = FakeHost::new();
    let runner = Arc::new(common::FakeRunner::new(&[
        "",                            // pool list before import
        "",                            // zpool import -N tank
        "tank\t10G\t1G\t9G\tONLINE\n", // pool list after import
        "tank\t/mnt/legacy\t-\ntank/data\t/var/mnt/tank/data\ton\n",
        "", // set mountpoint
        "", // mount tank
        "", // mount tank/data
    ]));
    let calls = runner.calls();
    let c = client(&host, runner);

    c.import_pool("tank", false).await.expect("import");
    let all = calls.lock().unwrap();
    assert!(all
        .iter()
        .any(|c| c == "/usr/local/sbin/zpool import -N tank"));
    assert!(
        all.iter()
            .any(|c| c == "/usr/local/sbin/zfs set mountpoint=/var/mnt/tank tank"),
        "{all:?}"
    );
    assert!(all
        .iter()
        .any(|c| c == "/usr/local/sbin/zfs mount tank/data"));
}

#[tokio::test]
async fn import_only_forces_with_an_explicit_request() {
    let host = FakeHost::new();
    let runner = Arc::new(common::FakeRunner::new(&["", "", "", ""]));
    let calls = runner.calls();
    let c = client(&host, runner);
    c.import_pool("tank", true).await.expect("forced import");
    assert!(calls
        .lock()
        .unwrap()
        .iter()
        .any(|c| c == "/usr/local/sbin/zpool import -N -f tank"));
}

#[test]
fn create_only_encryption_is_refused_for_a_pool() {
    for key in ["encryption", "keyformat", "keylocation", "encryptionroot"] {
        let options = HashMap::from([(key.to_string(), "aes-256-gcm".to_string())]);
        let err = validate_pool_options(&options).unwrap_err();
        assert!(
            err.to_string().contains("create-only encryption"),
            "{key}: {err}"
        );
        assert!(err.is_validation(), "{key}: {err:?}");
    }
}

#[tokio::test]
async fn pool_create_rejects_encryption_before_any_host_command() {
    let host = FakeHost::new();
    let runner = Arc::new(common::FakeRunner::new(&[]));
    let calls = runner.calls();
    let c = client(&host, runner);

    let cfg = PoolConfig {
        name: "tank".into(),
        topology: "single".into(),
        disks: vec!["/dev/sdb".into()],
        options: HashMap::from([("encryption".to_string(), "aes-256-gcm".to_string())]),
        ..Default::default()
    };
    let err = c.create_pool(&cfg).await.expect_err("encryption refused");
    assert!(err.to_string().contains("create-only encryption"), "{err}");
    assert!(calls.lock().unwrap().is_empty(), "no disk was touched");
}

#[tokio::test]
async fn pool_create_returns_a_partial_report_when_a_follow_up_fails() {
    let host = FakeHost::new();
    let runner = Arc::new(common::FakeRunner::new(&[
        "ERR:no such pool",          // zpool list (existence check)
        "",                          // pool rows
        "",                          // import dry-run
        "",                          // zpool create
        "ERR:cannot set mountpoint", // zfs set mountpoint
        "",                          // acltype
        "",                          // atime
        "",                          // compression
        "",                          // dnodesize
        "",                          // recordsize
        "",                          // relatime
        "",                          // xattr
        "",                          // SELinux contexts
    ]));
    let calls = runner.calls();
    let c = client(&host, runner);

    let cfg = PoolConfig {
        name: "tank".into(),
        topology: "single".into(),
        disks: vec!["/dev/sdb".into()],
        ..Default::default()
    };
    let report = c.create_pool(&cfg).await.expect("pool exists");
    assert!(report.created);
    assert!(report.partial, "a follow-up step failed");
    assert!(
        report.warnings.iter().any(|w| w.contains("mountpoint")),
        "{:?}",
        report.warnings
    );
    assert!(
        report
            .repair
            .iter()
            .any(|r| r.contains("zfs set mountpoint=")),
        "{:?}",
        report.repair
    );
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.contains("zpool create")),
        "the pool was created"
    );
}

#[test]
fn receive_drops_stream_mount_properties_and_defers_mounting() {
    let argv = receive_command_line("tank/data", false);
    assert_eq!(
        argv,
        vec![
            "receive",
            "-u",
            "-x",
            "mountpoint",
            "-x",
            "canmount",
            "-x",
            "sharenfs",
            "-x",
            "sharesmb",
            "tank/data",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>()
    );
    assert!(receive_command_line("tank/data", true)
        .iter()
        .any(|a| a == "-F"));
    assert_eq!(
        receive_mountpoint("tank/data/child").as_deref(),
        Some("/var/mnt/tank/data/child")
    );
    assert_eq!(receive_mountpoint("tank"), None);
}

#[test]
fn foreign_snapshots_are_recognised_by_prefix() {
    for prefix in OWNED_SNAPSHOT_PREFIXES {
        let listing = format!("tank/data@{prefix}20260101T000000Z\n");
        assert_eq!(foreign_snapshot(&listing), None, "{prefix}");
    }
    assert_eq!(
        foreign_snapshot("tank/data@user-snapshot\n").as_deref(),
        Some("user-snapshot")
    );
}

#[tokio::test]
async fn forced_receive_refuses_a_foreign_snapshot() {
    let host = FakeHost::new();
    let runner = Arc::new(common::FakeRunner::new(&["tank/data@user-snapshot\n"]));
    let c = client(&host, runner);

    let err = match c.receive_stream("tank/data", true).await {
        Ok(_) => panic!("a foreign snapshot must be refused before the receive"),
        Err(e) => e,
    };
    assert!(err.is_conflict(), "{err:?}");
    assert!(err.to_string().contains("user-snapshot"), "{err}");
}

#[tokio::test]
async fn finalize_receive_sets_the_server_mountpoint() {
    let host = FakeHost::new();
    let runner = Arc::new(common::FakeRunner::new(&["", ""]));
    let calls = runner.calls();
    let c = client(&host, runner);

    let done = c.finalize_receive("tank/data").await.expect("finalize");
    assert_eq!(done.mountpoint, "/var/mnt/tank/data");
    let all = calls.lock().unwrap();
    assert_eq!(
        all[0],
        "/usr/local/sbin/zfs set mountpoint=/var/mnt/tank/data tank/data"
    );
    assert_eq!(all[1], "/usr/local/sbin/zfs mount tank/data");
}

#[tokio::test]
async fn dataset_mutations_serialize_per_name() {
    let host = FakeHost::new();
    let runner = Arc::new(common::FakeRunner::new(&[]));
    let c = Arc::new(client(&host, runner));

    let guard = c.lock_dataset_mutation("tank/a").await;
    let waiter = {
        let c = c.clone();
        tokio::spawn(async move {
            let _guard = c.lock_dataset_mutation("tank/a").await;
            true
        })
    };
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!waiter.is_finished(), "the same name must wait");
    drop(guard);
    assert!(tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .expect("released")
        .expect("task"));

    // A different name is independent.
    let guard = c.lock_dataset_mutation("tank/a").await;
    assert!(
        tokio::time::timeout(
            Duration::from_millis(200),
            c.lock_dataset_mutation("tank/b")
        )
        .await
        .is_ok(),
        "different names must not block each other"
    );
    drop(guard);
}
