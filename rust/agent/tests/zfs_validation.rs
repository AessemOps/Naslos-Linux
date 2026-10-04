//! Port of the Go agent's zfs validation tests (NAS-003): caller input must
//! never reach `zpool`/`zfs`/`wipefs` argv unchecked, and every refusal must
//! assert that no command ran.

mod common;

use common::{ran_command, FakeHost, FakeRunner};
use naslos_agent::zfs::dataset::{
    is_busy_error, validate_dataset_name, validate_dataset_options, validate_dataset_path,
};
use naslos_agent::zfs::devices::{is_whole_disk, normalize_vdev_topology, validate_pool_name};
use naslos_agent::zfs::PoolConfig;
use naslos_agent::zfs::{ZfsClient, ZfsError};
use std::collections::HashMap;
use std::sync::Arc;

fn client(host: &FakeHost, runner: Arc<FakeRunner>) -> ZfsClient {
    ZfsClient::new(runner, host.root.clone())
}

#[test]
fn validators_classify_as_validation_errors() {
    let cases: Vec<(&str, Result<(), ZfsError>)> = vec![
        ("pool name", validate_pool_name("-f")),
        ("dataset name", validate_dataset_name("../etc")),
        ("dataset path", validate_dataset_path("data")),
        (
            "snapshot name",
            naslos_agent::zfs::backup::validate_snapshot_name("a/b"),
        ),
        (
            "dataset options",
            validate_dataset_options(&HashMap::from([(
                "mountpoint".to_string(),
                "/".to_string(),
            )])),
        ),
        ("topology", normalize_vdev_topology("raidz9").map(|_| ())),
    ];

    for (name, result) in cases {
        let err = result.expect_err(&format!("{name} accepted invalid input"));
        assert!(
            err.is_validation(),
            "{name}: error {err:?} is not a Validation error, so the HTTP layer would answer 500"
        );
    }
}

#[tokio::test]
async fn create_pool_refusals_before_any_host_call() {
    let cases: Vec<(&str, PoolConfig, &str)> = vec![
        (
            "flag-shaped pool name",
            PoolConfig {
                name: "-f".into(),
                disks: vec!["/dev/sdb".into()],
                ..Default::default()
            },
            "pool name",
        ),
        (
            "path-shaped pool name",
            PoolConfig {
                name: "../etc".into(),
                disks: vec!["/dev/sdb".into()],
                ..Default::default()
            },
            "pool name",
        ),
        (
            "unknown topology",
            PoolConfig {
                name: "tank".into(),
                topology: "raidz9".into(),
                disks: vec!["/dev/sdb".into()],
                ..Default::default()
            },
            "topology",
        ),
        (
            "no disks",
            PoolConfig {
                name: "tank".into(),
                ..Default::default()
            },
            "at least one disk",
        ),
        (
            "mirror with one disk",
            PoolConfig {
                name: "tank".into(),
                topology: "mirror".into(),
                disks: vec!["/dev/sdb".into()],
                ..Default::default()
            },
            "needs at least 2",
        ),
        (
            "unsupported option",
            PoolConfig {
                name: "tank".into(),
                disks: vec!["/dev/sdb".into()],
                options: HashMap::from([("mountpoint".to_string(), "/".to_string())]),
                ..Default::default()
            },
            "unsupported dataset option",
        ),
        (
            "bad compression",
            PoolConfig {
                name: "tank".into(),
                disks: vec!["/dev/sdb".into()],
                options: HashMap::from([("compression".to_string(), "magic".to_string())]),
                ..Default::default()
            },
            "unsupported compression",
        ),
    ];

    for (name, cfg, want) in cases {
        let host = FakeHost::new();
        let runner = Arc::new(FakeRunner::new(&[]));
        let calls = runner.calls();
        let c = client(&host, runner);

        let err = c
            .create_pool(&cfg)
            .await
            .expect_err(&format!("{name}: expected an error"));
        assert!(
            err.to_string().contains(want),
            "{name}: error {err:?} should mention {want:?}"
        );
        assert!(
            !ran_command(&calls, "create") && !ran_command(&calls, "wipefs"),
            "{name}: a refused request still ran a command"
        );
    }
}

#[tokio::test]
async fn create_pool_refuses_bad_disks() {
    let cases: Vec<(&str, PoolConfig, &str)> = vec![
        (
            "relative disk",
            PoolConfig {
                name: "tank".into(),
                disks: vec!["sdb".into()],
                ..Default::default()
            },
            "absolute path",
        ),
        (
            "traversal in path",
            PoolConfig {
                name: "tank".into(),
                disks: vec!["/dev/../etc/passwd".into()],
                ..Default::default()
            },
            "invalid disk path",
        ),
        (
            "absent device",
            PoolConfig {
                name: "tank".into(),
                disks: vec!["/dev/sdz".into()],
                ..Default::default()
            },
            "not found",
        ),
        (
            "duplicate disk",
            PoolConfig {
                name: "tank".into(),
                disks: vec!["/dev/sdb".into(), "/dev/sdb".into()],
                ..Default::default()
            },
            "more than once",
        ),
        (
            "bad cache device",
            PoolConfig {
                name: "tank".into(),
                disks: vec!["/dev/sdb".into()],
                cache: "/dev/sdz".into(),
                ..Default::default()
            },
            "not found",
        ),
        (
            "cache is also a data disk",
            PoolConfig {
                name: "tank".into(),
                disks: vec!["/dev/sdb".into()],
                cache: "/dev/sdb".into(),
                ..Default::default()
            },
            "both a data disk and the cache",
        ),
    ];

    for (name, cfg, want) in cases {
        let host = FakeHost::new();
        // 1: the pool does not exist yet; 2: no pools, so no disk is a member.
        let runner = Arc::new(FakeRunner::new(&[
            "ERR:cannot open 'tank': no such pool\n",
            "",
        ]));
        let calls = runner.calls();
        let c = client(&host, runner);

        let err = c
            .create_pool(&cfg)
            .await
            .expect_err(&format!("{name}: expected an error"));
        assert!(
            err.to_string().contains(want),
            "{name}: error {err:?} should mention {want:?}"
        );
        assert!(
            !ran_command(&calls, " create "),
            "{name}: a refused request still ran zpool create"
        );
    }
}

#[tokio::test]
async fn create_pool_refuses_disk_in_another_pool() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[
        "ERR:cannot open 'tank': no such pool\n",
        "other\t10G\t1G\t9G\tONLINE\n",
        "config:\n\n        NAME   STATE  READ WRITE CKSUM\n        other  ONLINE 0 0 0\n          /dev/sdb ONLINE 0 0 0\n\n",
    ]));
    let calls = runner.calls();
    let c = client(&host, runner);

    let err = c
        .create_pool(&PoolConfig {
            name: "tank".into(),
            disks: vec!["/dev/sdb".into()],
            ..Default::default()
        })
        .await
        .expect_err("expected an error");
    assert!(
        err.to_string().contains("already belongs to pool"),
        "error {err:?} should name the owning pool"
    );
    assert!(
        !ran_command(&calls, " create "),
        "a refused request still ran zpool create"
    );
}

#[tokio::test]
async fn create_pool_command_line() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[
        "ERR:cannot open 'tank': no such pool\n",
        "",
    ]));
    let calls = runner.calls();
    let c = client(&host, runner);

    c.create_pool(&PoolConfig {
        name: "tank".into(),
        topology: "mirror".into(),
        disks: vec!["/dev/sdb".into(), "/dev/sdc".into()],
        cache: "/dev/sdd".into(),
        options: HashMap::from([("compression".to_string(), "lz4".to_string())]),
    })
    .await
    .expect("CreatePool should succeed");

    assert!(
        ran_command(
            &calls,
            "/usr/local/sbin/zpool create -f -o ashift=12 tank mirror /dev/sdb /dev/sdc"
        ),
        "want the mirror create line; calls: {:?}",
        calls.lock().unwrap()
    );
    assert!(
        ran_command(&calls, "/usr/local/sbin/zpool add tank cache /dev/sdd"),
        "want the cache device added; calls: {:?}",
        calls.lock().unwrap()
    );
}

#[tokio::test]
async fn pool_sinks_refuse_invalid_names() {
    let bad = ["", "  ", "-f", "tank/name", "../etc", "tank pool"];
    for name in bad {
        let host = FakeHost::new();
        let runner = Arc::new(FakeRunner::new(&[]));
        let calls = runner.calls();
        let c = client(&host, runner);

        assert!(c.destroy_pool(name).await.is_err(), "DestroyPool({name:?})");
        assert!(c.pool_status(name).await.is_err(), "PoolStatus({name:?})");
        assert!(c.pool_health(name).await.is_err(), "PoolHealth({name:?})");
        assert!(c.export_pool(name).await.is_err(), "ExportPool({name:?})");
        assert!(c.datasets(name).await.is_err(), "Datasets({name:?})");

        assert!(
            calls.lock().unwrap().is_empty(),
            "refused name {name:?} still ran a command"
        );
    }
}

#[tokio::test]
async fn import_pool_validates_a_named_pool() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[]));
    let calls = runner.calls();
    let c = client(&host, runner);

    assert!(c.import_pool("-f").await.is_err());
    assert!(c.import_pool("tank/name").await.is_err());
    assert!(calls.lock().unwrap().is_empty());

    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[]));
    let calls = runner.calls();
    let c = client(&host, runner);
    c.import_pool("").await.expect("import-all is accepted");
    assert!(ran_command(&calls, "/usr/local/sbin/zpool import -f"));
}

#[tokio::test]
async fn snapshot_refuses_invalid_arguments() {
    let cases: Vec<(&str, &str, &str, &str)> = vec![
        (
            "flag-shaped snapshot",
            "tank/data",
            "-r",
            "must not start with '-'",
        ),
        (
            "snapshot with a slash",
            "tank/data",
            "a/b",
            "must not contain",
        ),
        (
            "snapshot with a space",
            "tank/data",
            "a b",
            "must not contain",
        ),
        (
            "dataset without a pool",
            "data",
            "snap1",
            "must be <pool>/<name>",
        ),
        (
            "absolute dataset",
            "/tank/data",
            "snap1",
            "not an absolute path",
        ),
        (
            "dataset with a snapshot",
            "tank/data@old",
            "snap1",
            "must not contain '@'",
        ),
    ];
    for (name, dataset, snap, want) in cases {
        let host = FakeHost::new();
        let runner = Arc::new(FakeRunner::new(&[]));
        let calls = runner.calls();
        let c = client(&host, runner);

        let err = c
            .snapshot(dataset, snap)
            .await
            .expect_err(&format!("{name}: expected an error"));
        assert!(
            err.to_string().contains(want),
            "{name}: error {err:?} should mention {want:?}"
        );
        assert!(calls.lock().unwrap().is_empty(), "{name}: a command ran");
    }

    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[]));
    let calls = runner.calls();
    let c = client(&host, runner);
    c.snapshot("tank/data", "buddy-20260101T000000Z-abcd")
        .await
        .expect("valid snapshot");
    assert!(ran_command(
        &calls,
        "/usr/local/sbin/zfs snapshot tank/data@buddy-20260101T000000Z-abcd"
    ));
}

#[tokio::test]
async fn snapshots_refuses_invalid_dataset() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[]));
    let calls = runner.calls();
    let c = client(&host, runner);

    assert!(c.snapshots("tank/data@old").await.is_err());
    assert!(calls.lock().unwrap().is_empty());
}

// ---- devices -------------------------------------------------------------

#[tokio::test]
async fn add_vdev_command_line() {
    let cases: Vec<(&str, &str, Vec<&str>, bool, &str)> = vec![
        (
            "single disk",
            "single",
            vec!["/dev/sdb"],
            false,
            "/usr/local/sbin/zpool add tank /dev/sdb",
        ),
        (
            "empty topology means stripe",
            "",
            vec!["/dev/sdb"],
            false,
            "/usr/local/sbin/zpool add tank /dev/sdb",
        ),
        (
            "stripe spelling",
            "stripe",
            vec!["/dev/sdb", "/dev/sdc"],
            false,
            "/usr/local/sbin/zpool add tank /dev/sdb /dev/sdc",
        ),
        (
            "mirror",
            "mirror",
            vec!["/dev/sdb", "/dev/sdc"],
            false,
            "/usr/local/sbin/zpool add tank mirror /dev/sdb /dev/sdc",
        ),
        (
            "raidz2",
            "raidz2",
            vec!["/dev/sdb", "/dev/sdc", "/dev/sdd"],
            false,
            "/usr/local/sbin/zpool add tank raidz2 /dev/sdb /dev/sdc /dev/sdd",
        ),
        (
            "force is opt-in and comes before the pool",
            "mirror",
            vec!["/dev/sdb", "/dev/sdc"],
            true,
            "/usr/local/sbin/zpool add -f tank mirror /dev/sdb /dev/sdc",
        ),
    ];
    for (name, topology, disks, force, want) in cases {
        let host = FakeHost::new();
        let runner = Arc::new(FakeRunner::new(&["tank\n", "tank\t1G\n", ""]));
        let calls = runner.calls();
        let c = client(&host, runner);
        let disks: Vec<String> = disks.iter().map(|s| s.to_string()).collect();

        c.add_vdev("tank", topology, &disks, force)
            .await
            .unwrap_or_else(|e| panic!("{name}: AddVDev failed: {e}"));
        let all = calls.lock().unwrap();
        let last = all.last().expect("no command ran");
        assert_eq!(last, want, "{name}: last command mismatch; all: {all:?}");
    }
}

#[tokio::test]
async fn add_vdev_refusals() {
    let cases: Vec<(&str, &str, &str, Vec<&str>, &str)> = vec![
        ("no disks", "tank", "mirror", vec![], "at least one disk"),
        (
            "mirror needs two",
            "tank",
            "mirror",
            vec!["/dev/sdb"],
            "at least 2 disks",
        ),
        (
            "raidz2 needs three",
            "tank",
            "raidz2",
            vec!["/dev/sdb", "/dev/sdc"],
            "at least 3 disks",
        ),
        (
            "raidz3 needs four",
            "tank",
            "raidz3",
            vec!["/dev/sdb", "/dev/sdc", "/dev/sdd"],
            "at least 4 disks",
        ),
        (
            "unknown topology",
            "tank",
            "raidz9",
            vec!["/dev/sdb"],
            "unsupported topology",
        ),
        (
            "bad pool name",
            "tank/../etc",
            "single",
            vec!["/dev/sdb"],
            "invalid pool name",
        ),
        (
            "relative disk",
            "tank",
            "single",
            vec!["sdb"],
            "absolute path under /dev",
        ),
        (
            "device does not exist",
            "tank",
            "single",
            vec!["/dev/sdq"],
            "not found on the node",
        ),
        (
            "duplicate disk",
            "tank",
            "single",
            vec!["/dev/sdb", "/dev/sdb"],
            "more than once",
        ),
    ];
    for (name, pool, topology, disks, want) in cases {
        let host = FakeHost::new();
        let runner = Arc::new(FakeRunner::new(&["tank\n", "tank\t1G\n", ""]));
        let calls = runner.calls();
        let c = client(&host, runner);
        let disks: Vec<String> = disks.iter().map(|s| s.to_string()).collect();

        let err = c
            .add_vdev(pool, topology, &disks, false)
            .await
            .expect_err(&format!("{name}: expected an error"));
        assert!(
            err.to_string().contains(want),
            "{name}: error {err:?} should contain {want:?}"
        );
        assert!(
            !ran_command(&calls, " add "),
            "{name}: a refused request still ran zpool add"
        );
    }
}

#[tokio::test]
async fn add_vdev_refuses_disk_in_another_pool() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[
        "tank\n",
        "other\t10G\t1G\t9G\tONLINE\n",
        "config:\n\n        NAME   STATE  READ WRITE CKSUM\n        other  ONLINE 0 0 0\n          /dev/sdb ONLINE 0 0 0\n\n",
    ]));
    let calls = runner.calls();
    let c = client(&host, runner);

    let err = c
        .add_vdev("tank", "single", &["/dev/sdb".to_string()], true)
        .await
        .expect_err("expected an error");
    assert!(err.to_string().contains("already belongs to pool"));
    assert!(!ran_command(&calls, " add "));
}

#[tokio::test]
async fn add_vdev_missing_pool_is_reported() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[
        "ERR:cannot open 'nosuch': no such pool\n",
    ]));
    let calls = runner.calls();
    let c = client(&host, runner);

    let err = c
        .add_vdev("nosuch", "single", &["/dev/sdb".to_string()], false)
        .await
        .expect_err("expected an error");
    assert!(err.to_string().contains("not found"));
    assert!(!ran_command(&calls, " add "));
}

#[test]
fn is_whole_disk_cases() {
    for name in ["sda", "vdb", "hdc", "nvme0n1"] {
        assert!(is_whole_disk(name), "isWholeDisk({name}) should be true");
    }
    for name in [
        "sda1",
        "vdb12",
        "nvme0n1p1",
        "loop0",
        "sr0",
        "zram0",
        "dm-0",
    ] {
        assert!(!is_whole_disk(name), "isWholeDisk({name}) should be false");
    }
}

#[test]
fn validate_pool_name_cases() {
    for ok in ["tank", "pool1", "my-pool", "a.b_c:d"] {
        assert!(validate_pool_name(ok).is_ok(), "ValidatePoolName({ok:?})");
    }
    for bad in [
        "",
        "  ",
        "-tank",
        "tank/name",
        "tank pool",
        "../etc",
        "tank@snap",
    ] {
        assert!(
            validate_pool_name(bad).is_err(),
            "ValidatePoolName({bad:?})"
        );
    }
}

#[test]
fn is_busy_error_matches_the_stale_mount_signature() {
    assert!(is_busy_error("cannot destroy 'tank/x': dataset is busy"));
    assert!(is_busy_error("cannot destroy 'tank/x': dataset is busy\n"));
    assert!(!is_busy_error("dataset is in use"));
}
