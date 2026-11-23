//! Regression tests for the storage destroy-safety work package: safe pool
//! import, create-only encryption rejection, receive stream properties and the
//! per-dataset mutation lock (R-039, R-043, R-044).

mod common;

use common::FakeHost;
use naslos_agent::zfs::backup::{
    foreign_snapshot, receive_command_line, receive_mountpoint, OWNED_SNAPSHOT_PREFIXES,
};
use naslos_agent::zfs::devices::disk_in_use;
use naslos_agent::zfs::operations::validate_pool_options;
use naslos_agent::zfs::{PoolConfig, ZfsClient};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

fn client(host: &FakeHost, runner: Arc<common::FakeRunner>) -> ZfsClient {
    ZfsClient::new(runner, host.root.clone())
}

fn write_file(root: &Path, path: &str, body: &str) {
    let target = root.join(path);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(target, body).unwrap();
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
fn a_mounted_partition_makes_the_whole_disk_unsafe() {
    let host = FakeHost::new();
    assert_eq!(disk_in_use(&host.root, "/dev/sdb"), None);

    write_file(
        &host.root,
        "proc/mounts",
        "/dev/sdb1 /system/state ext4 rw,relatime 0 0\n/dev/sda2 / ext4 rw,relatime 0 0\n",
    );
    let reason = disk_in_use(&host.root, "/dev/sdb").expect("a mounted partition");
    assert!(reason.contains("mounted at /system/state"), "{reason}");

    // The disk backing the host root is named as such, not just "mounted".
    std::fs::write(host.root.join("dev/sda"), b"").unwrap();
    let reason = disk_in_use(&host.root, "/dev/sda").expect("the host root disk");
    assert!(reason.contains("host root"), "{reason}");
}

#[test]
fn device_aliases_cannot_hide_a_mounted_system_partition() {
    let host = FakeHost::new();
    std::fs::create_dir_all(host.root.join("dev/disk/by-uuid")).unwrap();
    std::os::unix::fs::symlink("../../sdb2", host.root.join("dev/disk/by-uuid/abcd-1234")).unwrap();
    write_file(
        &host.root,
        "proc/mounts",
        "/dev/disk/by-uuid/abcd-1234 /var ext4 rw,relatime 0 0\n",
    );
    let reason = disk_in_use(&host.root, "/dev/sdb").expect("an aliased mount");
    assert!(reason.contains("mounted at /var"), "{reason}");
}

#[test]
fn a_device_mapper_holder_makes_a_disk_unsafe() {
    let host = FakeHost::new();
    write_file(&host.root, "sys/class/block/sdc/sdc2/holders/dm-0", "");
    let reason = disk_in_use(&host.root, "/dev/sdc").expect("a held partition");
    assert!(reason.contains("dm-0"), "{reason}");
}

#[tokio::test]
async fn pool_create_refuses_a_mounted_system_disk_before_any_wipe() {
    let host = FakeHost::new();
    write_file(
        &host.root,
        "proc/mounts",
        "/dev/sdb1 / ext4 rw,relatime 0 0\n",
    );
    let runner = Arc::new(common::FakeRunner::new(&[
        "ERR:no such pool", // zpool list (existence check)
        "",                 // pool rows
        "",                 // import dry-run
    ]));
    let calls = runner.calls();
    let c = client(&host, runner);

    let cfg = PoolConfig {
        name: "tank".into(),
        topology: "single".into(),
        disks: vec!["/dev/sdb".into()],
        ..Default::default()
    };
    let err = c.create_pool(&cfg).await.expect_err("mounted disk refused");
    assert!(err.is_conflict(), "{err:?}");
    assert!(
        err.to_string().contains("refusing to create a pool"),
        "{err}"
    );
    let all = calls.lock().unwrap();
    assert!(
        !all.iter()
            .any(|c| c.contains("wipefs") || c.contains("zpool create")),
        "the disk must not be touched: {all:?}"
    );
}

#[tokio::test]
async fn pool_create_refuses_a_mounted_cache_disk() {
    let host = FakeHost::new();
    write_file(&host.root, "proc/mounts", "/dev/sdc2 /var ext4 rw 0 0\n");
    let runner = Arc::new(common::FakeRunner::new(&[
        "ERR:no such pool", // zpool list (existence check)
        "",                 // pool rows
        "",                 // import dry-run
    ]));
    let calls = runner.calls();
    let c = client(&host, runner);
    let cfg = PoolConfig {
        name: "tank".into(),
        topology: "single".into(),
        disks: vec!["/dev/sdb".into()],
        cache: "/dev/sdc".into(),
        ..Default::default()
    };
    let err = c
        .create_pool(&cfg)
        .await
        .expect_err("mounted cache refused");
    assert!(err.is_conflict(), "{err:?}");
    assert!(err.to_string().contains("/dev/sdc"), "{err}");
    assert!(!calls
        .lock()
        .unwrap()
        .iter()
        .any(|c| c.contains("zpool create")));
}

#[tokio::test]
async fn add_vdev_refuses_a_mounted_system_disk() {
    let host = FakeHost::new();
    write_file(
        &host.root,
        "proc/mounts",
        "/dev/sdc2 /ephemeral ext4 rw,relatime 0 0\n",
    );
    let runner = Arc::new(common::FakeRunner::new(&[
        "", // zpool list tank (exists)
        "", // pool rows
        "", // import dry-run
    ]));
    let calls = runner.calls();
    let c = client(&host, runner);

    let err = c
        .add_vdev("tank", "single", &["/dev/sdc".into()], false)
        .await
        .expect_err("mounted disk refused");
    assert!(err.is_conflict(), "{err:?}");
    assert!(err.to_string().contains("refusing to add"), "{err}");
    assert!(
        !calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.contains("zpool add")),
        "the disk must not be attached"
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
    let runner = Arc::new(common::FakeRunner::new(&[
        "",                                    // set the root mountpoint
        "tank/data\t/var/mnt/tank/data\ton\n", // scoped tree listing
        "",                                    // mount the root
    ]));
    let calls = runner.calls();
    let c = client(&host, runner);

    let done = c.finalize_receive("tank/data").await.expect("finalize");
    assert_eq!(done.mountpoint, "/var/mnt/tank/data");
    assert!(done.warnings.is_empty(), "{:?}", done.warnings);
    let all = calls.lock().unwrap();
    assert_eq!(
        all[0],
        "/usr/local/sbin/zfs set mountpoint=/var/mnt/tank/data tank/data"
    );
    assert_eq!(
        all[1],
        "/usr/local/sbin/zfs list -H -o name,mountpoint,canmount -r tank/data"
    );
    assert_eq!(all[2], "/usr/local/sbin/zfs mount tank/data");
    assert!(
        !all.iter().any(|c| c.contains("mount -a")),
        "a blind mount -a must never run: {all:?}"
    );
}

#[tokio::test]
async fn finalize_receive_mounts_recursive_children_scoped() {
    let host = FakeHost::new();
    let runner = Arc::new(common::FakeRunner::new(&[
        "", // root mountpoint
        // children of a recursive receive, one with a stale mountpoint
        "tank/data\t/var/mnt/tank/data\ton\n\
         tank/data/child\t/var/mnt/tank/data/child\ton\n\
         tank/data/child/grand\t/legacy/grand\ton\n",
        "", // mount root
        "", // mount child
        "", // set grand mountpoint
        "", // mount grand
    ]));
    let calls = runner.calls();
    let c = client(&host, runner);

    let done = c.finalize_receive("tank/data").await.expect("finalize");
    assert!(done.warnings.is_empty(), "{:?}", done.warnings);
    let all = calls.lock().unwrap();
    assert!(
        all.iter()
            .any(|c| c == "/usr/local/sbin/zfs mount tank/data/child"),
        "the received child must be mounted: {all:?}"
    );
    assert!(all
        .iter()
        .any(|c| c == "/usr/local/sbin/zfs set mountpoint=/var/mnt/tank/data/child/grand tank/data/child/grand"));
    assert!(all
        .iter()
        .any(|c| c == "/usr/local/sbin/zfs mount tank/data/child/grand"));
    assert!(
        !all.iter().any(|c| c.contains("mount -a")),
        "a blind mount -a must never run: {all:?}"
    );
}

#[tokio::test]
async fn a_failed_child_mount_is_reported_without_failing_the_receive() {
    let host = FakeHost::new();
    let runner = Arc::new(common::FakeRunner::new(&[
        "", // root mountpoint
        "tank/data\t/var/mnt/tank/data\ton\ntank/data/child\t/var/mnt/tank/data/child\ton\n",
        "ERR:filesystem is busy",        // mount root
        "ERR:mountpoint does not exist", // mount child
    ]));
    let c = client(&host, runner);
    let done = c.finalize_receive("tank/data").await.expect("finalize");
    assert_eq!(done.warnings.len(), 2, "{:?}", done.warnings);
    assert!(
        done.warnings[1].contains("tank/data/child"),
        "{:?}",
        done.warnings
    );
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
