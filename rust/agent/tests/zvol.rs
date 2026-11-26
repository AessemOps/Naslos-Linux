//! zvol validation, command lines and in-use refusals.

mod common;

use common::{FakeHost, FakeRunner};
use naslos_agent::zfs::zvol::{validate_volblocksize, validate_volsize, zvol_in_use};
use naslos_agent::zfs::ZfsClient;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

fn client(host: &FakeHost, runner: Arc<FakeRunner>) -> ZfsClient {
    ZfsClient::new(runner, host.root.clone())
}

fn host_with_zvol_device(host: &FakeHost) {
    std::fs::create_dir_all(host.root.join("dev/zvol/tank")).unwrap();
    std::fs::write(host.root.join("dev/zd0"), b"").unwrap();
    std::os::unix::fs::symlink("../../zd0", host.root.join("dev/zvol/tank/vol")).unwrap();
}

#[tokio::test]
async fn create_zvol_command_line_sorts_options() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[]));
    let calls = runner.calls();
    let c = client(&host, runner);

    c.create_zvol(
        "tank/targets/vol1",
        "10G",
        &HashMap::from([
            ("volblocksize".to_string(), "16K".to_string()),
            ("compression".to_string(), "zstd".to_string()),
        ]),
    )
    .await
    .expect("CreateZvol");

    assert_eq!(
        calls.lock().unwrap()[0],
        "/usr/local/sbin/zfs create -p -V 10G -o compression=zstd -o volblocksize=16K tank/targets/vol1"
    );
}

#[tokio::test]
async fn create_zvol_refusals() {
    let name_cases: Vec<(&str, &str)> = vec![
        ("tank", "must be <pool>/<name>"),
        ("tank/../etc", "invalid dataset name"),
        ("/tank/vol", "not an absolute path"),
        ("tank/vol@snap", "must not contain '@'"),
        ("tank/a//b", "invalid dataset name"),
        ("", "required"),
    ];
    for (name, want) in name_cases {
        let host = FakeHost::new();
        let runner = Arc::new(FakeRunner::new(&[]));
        let calls = runner.calls();
        let c = client(&host, runner);

        let err = c
            .create_zvol(name, "10G", &HashMap::new())
            .await
            .expect_err(&format!("CreateZvol({name:?})"));
        assert!(err.to_string().contains(want), "{name}: {err:?}");
        assert!(calls.lock().unwrap().is_empty(), "{name}: a command ran");
    }

    let size_cases: Vec<(&str, &str)> = vec![
        ("", "size is required"),
        ("0", "invalid zvol size"),
        ("lots", "invalid zvol size"),
        ("10X", "invalid zvol size"),
        ("10g", "invalid zvol size"),
        ("-1G", "invalid zvol size"),
    ];
    for (size, want) in size_cases {
        let host = FakeHost::new();
        let runner = Arc::new(FakeRunner::new(&[]));
        let calls = runner.calls();
        let c = client(&host, runner);

        let err = c
            .create_zvol("tank/vol", size, &HashMap::new())
            .await
            .expect_err(&format!("CreateZvol(size={size:?})"));
        assert!(err.to_string().contains(want), "{size}: {err:?}");
        assert!(calls.lock().unwrap().is_empty(), "{size}: a command ran");
    }

    let option_cases: Vec<(&str, HashMap<String, String>, &str)> = vec![
        (
            "dataset-only option",
            HashMap::from([("recordsize".to_string(), "128K".to_string())]),
            "dataset option",
        ),
        (
            "volsize is not an option",
            HashMap::from([("volsize".to_string(), "10G".to_string())]),
            "size field",
        ),
        (
            "unknown option",
            HashMap::from([("shareiscsi".to_string(), "on".to_string())]),
            "unsupported zvol option",
        ),
        (
            "bad compression",
            HashMap::from([("compression".to_string(), "fastest".to_string())]),
            "unsupported compression",
        ),
        (
            "bad volblocksize",
            HashMap::from([("volblocksize".to_string(), "3M".to_string())]),
            "invalid volblocksize",
        ),
    ];
    for (label, options, want) in option_cases {
        let host = FakeHost::new();
        let runner = Arc::new(FakeRunner::new(&[]));
        let calls = runner.calls();
        let c = client(&host, runner);

        let err = c
            .create_zvol("tank/vol", "10G", &options)
            .await
            .expect_err(label);
        assert!(err.to_string().contains(want), "{label}: {err:?}");
        assert!(calls.lock().unwrap().is_empty(), "{label}: a command ran");
    }
}

#[tokio::test]
async fn list_zvols_parses_rows_and_pool_filter() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[
        "tank/vol\t10737418240\t16384\t1048576\t9663676416\t10737418240\toff\taes-256-gcm\ttank/enc\n",
        "tank/vol\t10737418240\t16384\t1048576\t9663676416\t10737418240\toff\taes-256-gcm\ttank/enc\n",
    ]));
    let calls = runner.calls();
    let c = client(&host, runner);

    let all = c.all_zvols().await.expect("all zvols");
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].name, "tank/vol");
    assert_eq!(all[0].volsize, "10G");
    assert_eq!(all[0].volsize_bytes, 10 * 1024 * 1024 * 1024);
    assert_eq!(all[0].volblocksize, "16384");
    assert_eq!(all[0].compression, "off");
    assert_eq!(all[0].encryption, "aes-256-gcm");
    assert_eq!(all[0].encryption_root, "tank/enc");

    let pool = c.zvols("tank").await.expect("pool zvols");
    assert_eq!(pool.len(), 1);
    let calls = calls.lock().unwrap();
    assert_eq!(
        calls[0],
        "/usr/local/sbin/zfs list -H -p -o name,volsize,volblocksize,used,avail,refer,compression,encryption,encryptionroot -t volume -r"
    );
    assert_eq!(
        calls[1],
        "/usr/local/sbin/zfs list -H -p -o name,volsize,volblocksize,used,avail,refer,compression,encryption,encryptionroot -t volume -r tank"
    );
}

#[tokio::test]
async fn list_zvols_refuses_an_invalid_pool_without_a_command() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[]));
    let calls = runner.calls();
    let c = client(&host, runner);

    assert!(c.zvols("-f").await.is_err());
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn resize_zvol_grows_with_a_validated_size() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&["volume\n", "10737418240\n", ""]));
    let calls = runner.calls();
    let c = client(&host, runner);

    c.resize_zvol("tank/vol", "20G").await.expect("resize");
    let all = calls.lock().unwrap();
    assert_eq!(all[0], "/usr/local/sbin/zfs list -H -o type tank/vol");
    assert_eq!(all[1], "/usr/local/sbin/zfs list -H -p -o volsize tank/vol");
    assert_eq!(all[2], "/usr/local/sbin/zfs set volsize=20G tank/vol");
}

#[tokio::test]
async fn resize_refuses_a_shrink_or_a_non_zvol() {
    // 20G current, 10G requested.
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&["volume\n", "21474836480\n"]));
    let calls = runner.calls();
    let c = client(&host, runner);
    let err = c.resize_zvol("tank/vol", "10G").await.expect_err("shrink");
    assert!(err.to_string().contains("shrinking"), "{err}");
    assert_eq!(calls.lock().unwrap().len(), 2, "no volsize set");

    // A filesystem with the same path must not be resized.
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&["filesystem\n"]));
    let calls = runner.calls();
    let c = client(&host, runner);
    let err = c
        .resize_zvol("tank/vol", "20G")
        .await
        .expect_err("not a zvol");
    assert!(err.to_string().contains("not a zvol"), "{err}");
    assert_eq!(calls.lock().unwrap().len(), 1, "no volsize read or set");
}

#[tokio::test]
async fn resize_is_a_noop_at_the_current_size() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&["volume\n", "21474836480\n"]));
    let calls = runner.calls();
    let c = client(&host, runner);

    c.resize_zvol("tank/vol", "20G").await.expect("same size");
    assert_eq!(calls.lock().unwrap().len(), 2, "no set for an equal size");
}

#[tokio::test]
async fn destroy_zvol_checks_type_then_destroys() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&["volume\n", ""]));
    let calls = runner.calls();
    let c = client(&host, runner);

    c.destroy_zvol("tank/vol").await.expect("destroy");
    let all = calls.lock().unwrap();
    assert_eq!(all[0], "/usr/local/sbin/zfs list -H -o type tank/vol");
    assert_eq!(all[1], "/usr/local/sbin/zfs destroy tank/vol");
}

#[tokio::test]
async fn destroy_refuses_a_filesystem_and_a_busy_volume() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&["filesystem\n"]));
    let calls = runner.calls();
    let c = client(&host, runner);
    let err = c.destroy_zvol("tank/media").await.expect_err("not a zvol");
    assert!(err.to_string().contains("not a zvol"), "{err}");
    assert_eq!(calls.lock().unwrap().len(), 1, "no destroy ran");

    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[
        "volume\n",
        "ERR:cannot destroy 'tank/vol': volume is busy\n",
    ]));
    let calls = runner.calls();
    let c = client(&host, runner);
    let err = c.destroy_zvol("tank/vol").await.expect_err("busy");
    assert!(err.to_string().contains("in use"), "{err}");
    assert_eq!(calls.lock().unwrap().len(), 2, "no retry after busy");
}

#[test]
fn sizes_and_volblocksizes_are_validated() {
    assert_eq!(validate_volsize("10G").unwrap(), 10 * 1024 * 1024 * 1024);
    for bad in ["", "0", "0.1", "lots", "10X", "-1G"] {
        assert!(validate_volsize(bad).is_err(), "{bad}");
    }
    for ok in ["512", "4K", "16K", "128K", "1M", "16M"] {
        assert!(validate_volblocksize(ok).is_ok(), "{ok}");
    }
    for bad in ["256", "3M", "17M", "32M", "1G", "1023.5"] {
        assert!(validate_volblocksize(bad).is_err(), "{bad}");
    }
}

fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, body).unwrap();
}

#[tokio::test]
async fn in_use_detection_covers_mounts_holders_and_open_files() {
    let host = FakeHost::new();
    host_with_zvol_device(&host);
    assert!(zvol_in_use(&host.root, "tank/vol").is_none());

    let mounted = FakeHost::new();
    host_with_zvol_device(&mounted);
    write(
        &mounted.root.join("proc/mounts"),
        "/dev/zd0 /mnt/vol ext4 rw,relatime 0 0\n",
    );
    let reason = zvol_in_use(&mounted.root, "tank/vol").expect("mounted");
    assert!(reason.contains("mounted at /mnt/vol"), "{reason}");

    // A partition of the zvol is a different device node (`zd0p1`) and must
    // count as in use too.
    let partitioned = FakeHost::new();
    host_with_zvol_device(&partitioned);
    std::fs::create_dir_all(partitioned.root.join("sys/block/zd0/zd0p1")).unwrap();
    write(
        &partitioned.root.join("proc/mounts"),
        "/dev/zd0p1 /mnt/part ext4 rw,relatime 0 0\n",
    );
    let reason = zvol_in_use(&partitioned.root, "tank/vol").expect("mounted partition");
    assert!(reason.contains("mounted at /mnt/part"), "{reason}");

    // A dm/md holder of the partition lives under the partition's sysfs node.
    let partition_held = FakeHost::new();
    host_with_zvol_device(&partition_held);
    write(
        &partition_held.root.join("sys/block/zd0/zd0p1/holders/dm-1"),
        "",
    );
    let reason = zvol_in_use(&partition_held.root, "tank/vol").expect("held partition");
    assert!(reason.contains("held by dm-1"), "{reason}");

    let held = FakeHost::new();
    host_with_zvol_device(&held);
    write(&held.root.join("sys/block/zd0/holders/dm-0"), "");
    let reason = zvol_in_use(&held.root, "tank/vol").expect("held");
    assert!(reason.contains("held by dm-0"), "{reason}");

    let opened = FakeHost::new();
    host_with_zvol_device(&opened);
    std::fs::create_dir_all(opened.root.join("proc/4242/fd")).unwrap();
    std::os::unix::fs::symlink(
        opened.root.join("dev/zd0"),
        opened.root.join("proc/4242/fd/7"),
    )
    .unwrap();
    let reason = zvol_in_use(&opened.root, "tank/vol").expect("open fd");
    assert!(reason.contains("process 4242"), "{reason}");
}

#[tokio::test]
async fn resize_and_destroy_refuse_an_in_use_zvol() {
    let host = FakeHost::new();
    host_with_zvol_device(&host);
    write(&host.root.join("sys/block/zd0/holders/dm-0"), "");

    let runner = Arc::new(FakeRunner::new(&["volume\n", "volume\n"]));
    let calls = runner.calls();
    let c = client(&host, runner.clone());

    let err = c
        .resize_zvol("tank/vol", "20G")
        .await
        .expect_err("resize in use");
    assert!(err.to_string().contains("in use"), "{err}");
    assert_eq!(calls.lock().unwrap().len(), 1, "only the type check ran");

    let c = client(&host, runner.clone());
    let err = c.destroy_zvol("tank/vol").await.expect_err("delete in use");
    assert!(err.to_string().contains("in use"), "{err}");
    assert_eq!(calls.lock().unwrap().len(), 2, "no destroy ran");
}
