//! Port of the Go agent's dataset tests, including the AV-8 stale-mount ladder.

mod common;

use common::{FakeHost, FakeRunner};
use naslos_agent::zfs::dataset::{human_bytes, validate_dataset_name};
use naslos_agent::zfs::ZfsClient;
use std::collections::HashMap;
use std::sync::Arc;

fn client(host: &FakeHost, runner: Arc<FakeRunner>) -> ZfsClient {
    ZfsClient::new(runner, host.root.clone())
}

#[tokio::test]
async fn create_dataset_command_line_sorts_options() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[]));
    let calls = runner.calls();
    let c = client(&host, runner);

    c.create_dataset(
        "tank/media",
        &HashMap::from([
            ("quota".to_string(), "100G".to_string()),
            ("compression".to_string(), "zstd".to_string()),
        ]),
    )
    .await
    .expect("CreateDataset");

    let all = calls.lock().unwrap();
    assert_eq!(all[0], "/usr/local/sbin/zfs list -H -o name tank/media");
    assert_eq!(
        all[1],
        "/usr/local/sbin/zfs create -p -o compression=zstd -o quota=100G tank/media"
    );
}

#[tokio::test]
async fn create_dataset_without_options() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[]));
    let calls = runner.calls();
    let c = client(&host, runner);

    c.create_dataset("tank/photos/2026", &HashMap::new())
        .await
        .expect("CreateDataset");
    let all = calls.lock().unwrap();
    assert_eq!(
        all[0],
        "/usr/local/sbin/zfs list -H -o name tank/photos/2026"
    );
    assert_eq!(all[1], "/usr/local/sbin/zfs create -p tank/photos/2026");
}

#[tokio::test]
async fn create_dataset_refuses_an_existing_dataset() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&["tank/media\n"]));
    let calls = runner.calls();
    let c = client(&host, runner);

    let err = c
        .create_dataset("tank/media", &HashMap::new())
        .await
        .expect_err("existing dataset");
    assert!(err.to_string().contains("already exists"), "{err}");
    assert_eq!(calls.lock().unwrap().len(), 1, "no create must run");
}

#[tokio::test]
async fn create_dataset_refusals() {
    let name_cases: Vec<(&str, &str)> = vec![
        ("tank", "must be <pool>/<name>"),
        ("tank/../etc", "invalid dataset name"),
        ("/tank/media", "not an absolute path"),
        ("tank/@snap", "must not contain '@'"),
        ("tank/media@snap", "must not contain '@'"),
        ("tank/a//b", "invalid dataset name"),
        ("tank/-leading", "invalid dataset name"),
        ("tank/sha res", "invalid dataset name"),
        ("", "required"),
    ];
    for (name, want) in name_cases {
        let host = FakeHost::new();
        let runner = Arc::new(FakeRunner::new(&[]));
        let calls = runner.calls();
        let c = client(&host, runner);

        let err = c
            .create_dataset(name, &HashMap::new())
            .await
            .expect_err(&format!("CreateDataset({name:?})"));
        assert!(err.to_string().contains(want), "{name}: {err:?}");
        assert!(calls.lock().unwrap().is_empty(), "{name}: a command ran");
    }

    let option_cases: Vec<(&str, HashMap<String, String>, &str)> = vec![
        (
            "unknown key",
            HashMap::from([("shareiscsi".to_string(), "on".to_string())]),
            "unsupported dataset option",
        ),
        (
            "mountpoint is not settable here",
            HashMap::from([("mountpoint".to_string(), "/".to_string())]),
            "unsupported dataset option",
        ),
        (
            "bad compression",
            HashMap::from([("compression".to_string(), "fastest".to_string())]),
            "unsupported compression",
        ),
        (
            "bad quota",
            HashMap::from([("quota".to_string(), "lots".to_string())]),
            "invalid quota",
        ),
        (
            "bad recordsize",
            HashMap::from([("recordsize".to_string(), "big".to_string())]),
            "invalid recordsize",
        ),
        (
            "bad copies",
            HashMap::from([("copies".to_string(), "9".to_string())]),
            "copies must be 1, 2 or 3",
        ),
        (
            "bad atime",
            HashMap::from([("atime".to_string(), "maybe".to_string())]),
            "must be on or off",
        ),
    ];
    for (name, options, want) in option_cases {
        let host = FakeHost::new();
        let runner = Arc::new(FakeRunner::new(&[]));
        let calls = runner.calls();
        let c = client(&host, runner);

        let err = c
            .create_dataset("tank/media", &options)
            .await
            .expect_err(&format!("CreateDataset({name})"));
        assert!(err.to_string().contains(want), "{name}: {err:?}");
        assert!(calls.lock().unwrap().is_empty(), "{name}: a command ran");
    }
}

fn state_response() -> &'static str {
    "mountpoint\t/var/mnt/tank/stale\ncanmount\ton\nmounted\tyes\nguid\t1\ncreatetxg\t9\n"
}

#[tokio::test]
async fn destroy_dataset_checks_identity_then_destroys() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&["filesystem\t1\n", ""]));
    let calls = runner.calls();
    let c = client(&host, runner);

    c.destroy_dataset("tank/media", false)
        .await
        .expect("destroy");
    {
        let all = calls.lock().unwrap();
        assert_eq!(
            all[0],
            "/usr/local/sbin/zfs list -H -o type,guid tank/media"
        );
        assert_eq!(all[1], "/usr/local/sbin/zfs destroy tank/media");
    }

    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&["filesystem\t1\n", ""]));
    let calls = runner.calls();
    let c = client(&host, runner);
    c.destroy_dataset("tank/media", true)
        .await
        .expect("destroy -r");
    assert_eq!(
        calls.lock().unwrap()[1],
        "/usr/local/sbin/zfs destroy -r tank/media"
    );

    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&["filesystem\t1\n"]));
    let calls = runner.calls();
    let c = client(&host, runner);
    assert!(c.destroy_dataset("tank", true).await.is_err());
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn destroy_dataset_refuses_a_zvol_target() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&["volume\t7\n"]));
    let calls = runner.calls();
    let c = client(&host, runner);

    let err = c
        .destroy_dataset("tank/vol", false)
        .await
        .expect_err("zvol");
    assert!(err.to_string().contains("zvol"), "{err}");
    assert!(err.to_string().contains("/api/zvols"), "{err}");
    assert_eq!(calls.lock().unwrap().len(), 1, "no destroy command");
}

#[tokio::test]
async fn destroy_dataset_verifies_the_expected_guid_first() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&["filesystem\t1\n"]));
    let calls = runner.calls();
    let c = client(&host, runner);

    let err = c
        .destroy_dataset_with_guid("tank/media", false, Some("2"))
        .await
        .expect_err("guid mismatch");
    assert!(
        err.to_string().contains("changed before the destroy"),
        "{err}"
    );
    assert_eq!(calls.lock().unwrap().len(), 1, "no destroy command");
}

#[tokio::test]
async fn destroy_dataset_does_not_unmount_on_other_errors() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[
        "filesystem\t1\n",
        "ERR:cannot destroy 'tank/live': dataset is in use\n",
    ]));
    let calls = runner.calls();
    let c = client(&host, runner);

    assert!(c.destroy_dataset("tank/live", false).await.is_err());
    assert_eq!(
        calls.lock().unwrap().len(),
        2,
        "a non-busy error must not unmount"
    );
}

#[tokio::test]
async fn destroy_dataset_recovers_from_stale_mount() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[
        "filesystem\t1\n",
        "ERR:cannot destroy 'tank/stale': dataset is busy\n",
        state_response(),
        "", // zfs unmount -f
        state_response(),
        "", // zfs destroy (retry)
    ]));
    let calls = runner.calls();
    let c = client(&host, runner);

    c.destroy_dataset("tank/stale", true)
        .await
        .expect("should recover from a stale mount");
    let all = calls.lock().unwrap();
    assert_eq!(all.len(), 6, "expected 6 commands; got {all:?}");
    assert_eq!(
        all[0],
        "/usr/local/sbin/zfs list -H -o type,guid tank/stale"
    );
    assert_eq!(all[1], "/usr/local/sbin/zfs destroy -r tank/stale");
    assert_eq!(
        all[2],
        "/usr/local/sbin/zfs get -H -o property,value mountpoint,canmount,mounted,guid,createtxg tank/stale"
    );
    assert_eq!(all[3], "/usr/local/sbin/zfs unmount -f tank/stale");
    assert_eq!(all[4], all[2]);
    assert_eq!(all[5], all[1]);
}

#[tokio::test]
async fn destroy_dataset_restores_the_mount_when_the_retry_still_fails() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[
        "filesystem\t1\n",
        "ERR:cannot destroy 'tank/stale': dataset is busy\n",
        state_response(),
        "", // unmount succeeds
        state_response(),
        "ERR:cannot destroy 'tank/stale': dataset is busy\n",
        state_response(),
        "", // clear mountpoint
        "ERR:cannot destroy 'tank/stale': dataset is busy\n",
        "", // restore zfs set
        "", // restore zfs mount
    ]));
    let calls = runner.calls();
    let c = client(&host, runner);

    let err = c
        .destroy_dataset("tank/stale", false)
        .await
        .expect_err("still busy");
    assert!(err.is_conflict(), "{err:?}");
    let all = calls.lock().unwrap();
    assert_eq!(
        all[9],
        "/usr/local/sbin/zfs set mountpoint=/var/mnt/tank/stale canmount=on tank/stale"
    );
    assert_eq!(all[10], "/usr/local/sbin/zfs mount tank/stale");
}

#[tokio::test]
async fn destroy_dataset_clears_stale_mountpoint() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[
        "filesystem\t1\n",
        "ERR:cannot destroy 'tank/stale': dataset is busy\n",
        state_response(),
        "ERR:cannot unmount 'tank/stale': not currently mounted\n",
        state_response(),
        "", // zfs set mountpoint=none
        "", // zfs destroy (retry)
    ]));
    let calls = runner.calls();
    let c = client(&host, runner);

    c.destroy_dataset("tank/stale", true)
        .await
        .expect("should clear the stale mountpoint and succeed");
    let all = calls.lock().unwrap();
    assert_eq!(all.len(), 7, "expected 7 commands; got {all:?}");
    assert_eq!(
        all[5],
        "/usr/local/sbin/zfs set mountpoint=none canmount=off tank/stale"
    );
    assert_eq!(all[6], "/usr/local/sbin/zfs destroy -r tank/stale");
}

#[tokio::test]
async fn destroy_dataset_restores_when_clearing_the_mountpoint_fails() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[
        "filesystem\t1\n",
        "ERR:dataset is busy\n",
        state_response(),
        "", // unmount
        state_response(),
        "ERR:cannot destroy 'tank/stale': dataset is busy\n",
        state_response(),
        "ERR:cannot set property\n",
        "", // restore zfs set
        "", // restore zfs mount
    ]));
    let calls = runner.calls();
    let c = client(&host, runner);

    let err = c
        .destroy_dataset("tank/stale", false)
        .await
        .expect_err("property failure");
    assert!(
        err.to_string()
            .contains("clearing the mountpoint also failed"),
        "{err:?}"
    );
    assert_eq!(
        calls.lock().unwrap()[8],
        "/usr/local/sbin/zfs set mountpoint=/var/mnt/tank/stale canmount=on tank/stale"
    );
}

#[tokio::test]
async fn destroy_dataset_restores_when_the_forced_unmount_fails() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[
        "filesystem\t1\n",
        "ERR:dataset is busy\n",
        state_response(),
        "ERR:dataset is in use\n",
        "", // restore zfs set
        "", // restore zfs mount
    ]));
    let calls = runner.calls();
    let c = client(&host, runner);

    let err = c
        .destroy_dataset("tank/stale", false)
        .await
        .expect_err("error");
    assert!(err.to_string().contains("forced unmount"), "{err:?}");
    assert_eq!(
        calls.lock().unwrap()[4],
        "/usr/local/sbin/zfs set mountpoint=/var/mnt/tank/stale canmount=on tank/stale"
    );
}

#[tokio::test]
async fn destroy_dataset_refuses_a_changed_generation_before_a_retry() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[
        "filesystem\t1\n",
        "ERR:dataset is busy\n",
        "mountpoint\t/var/mnt/tank/stale\ncanmount\ton\nmounted\tyes\nguid\t1\ncreatetxg\t9\n",
        "", // unmount
        "mountpoint\t/var/mnt/tank/stale\ncanmount\ton\nmounted\tyes\nguid\t2\ncreatetxg\t10\n",
    ]));
    let calls = runner.calls();
    let c = client(&host, runner);

    let err = c
        .destroy_dataset("tank/stale", false)
        .await
        .expect_err("generation changed");
    assert!(err.is_conflict(), "{err:?}");
    assert!(err.to_string().contains("changed"), "{err}");
    assert_eq!(calls.lock().unwrap().len(), 5, "no second destroy");
}

#[test]
fn validate_dataset_name_cases() {
    for ok in ["media", "photos/2026", "a.b_c-d", "Media1", "a/b/c/d"] {
        assert!(
            validate_dataset_name(ok).is_ok(),
            "ValidateDatasetName({ok:?})"
        );
    }
    for bad in [
        "", " media", "media ", "../x", "a/../b", "a b", "-1-", "@", "a@b", "/abs",
    ] {
        assert!(
            validate_dataset_name(bad).is_err(),
            "ValidateDatasetName({bad:?})"
        );
    }
}

#[test]
fn human_bytes_cases() {
    let cases = [
        (0i64, "0B"),
        (512, "512B"),
        (1024, "1.0K"),
        (1536, "1.5K"),
        (10 * 1024, "10K"),
        (2 * 1024 * 1024 * 1024, "2.0G"),
    ];
    for (n, want) in cases {
        assert_eq!(human_bytes(n), want, "human_bytes({n})");
    }
}
