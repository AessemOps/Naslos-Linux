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
    assert_eq!(
        all[0],
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
    assert_eq!(
        calls.lock().unwrap()[0],
        "/usr/local/sbin/zfs create -p tank/photos/2026"
    );
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

#[tokio::test]
async fn destroy_dataset_requires_recursive_flag() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[]));
    let calls = runner.calls();
    let c = client(&host, runner);

    c.destroy_dataset("tank/media", false)
        .await
        .expect("destroy");
    assert_eq!(
        calls.lock().unwrap()[0],
        "/usr/local/sbin/zfs destroy tank/media"
    );

    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[]));
    let calls = runner.calls();
    let c = client(&host, runner);
    c.destroy_dataset("tank/media", true)
        .await
        .expect("destroy -r");
    assert_eq!(
        calls.lock().unwrap()[0],
        "/usr/local/sbin/zfs destroy -r tank/media"
    );

    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[]));
    let calls = runner.calls();
    let c = client(&host, runner);
    assert!(c.destroy_dataset("tank", true).await.is_err());
    assert!(calls.lock().unwrap().is_empty());
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

#[tokio::test]
async fn destroy_dataset_recovers_from_stale_mount() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[
        "ERR:cannot destroy 'tank/stale': dataset is busy\n",
        "", // zfs unmount -f
        "", // zfs destroy (retry)
    ]));
    let calls = runner.calls();
    let c = client(&host, runner);

    c.destroy_dataset("tank/stale", true)
        .await
        .expect("should recover from a stale mount");
    let all = calls.lock().unwrap();
    assert_eq!(
        all.len(),
        3,
        "expected destroy, unmount, destroy; got {all:?}"
    );
    assert_eq!(all[0], "/usr/local/sbin/zfs destroy -r tank/stale");
    assert_eq!(all[1], "/usr/local/sbin/zfs unmount -f tank/stale");
    assert_eq!(all[2], all[0]);
}

#[tokio::test]
async fn destroy_dataset_does_not_unmount_on_other_errors() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[
        "ERR:cannot destroy 'tank/live': dataset is in use\n",
    ]));
    let calls = runner.calls();
    let c = client(&host, runner);

    assert!(c.destroy_dataset("tank/live", false).await.is_err());
    assert_eq!(
        calls.lock().unwrap().len(),
        1,
        "a non-busy error must not unmount"
    );
}

#[tokio::test]
async fn destroy_dataset_reports_unmount_failure() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[
        "ERR:dataset is busy\n",
        "ERR:dataset is in use\n",
    ]));
    let calls = runner.calls();
    let c = client(&host, runner);

    let err = c
        .destroy_dataset("tank/stale", false)
        .await
        .expect_err("error");
    assert!(err.to_string().contains("forced unmount"), "{err:?}");
    assert_eq!(calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn destroy_dataset_clears_stale_mountpoint() {
    let host = FakeHost::new();
    let runner = Arc::new(FakeRunner::new(&[
        "ERR:cannot destroy 'tank/stale': dataset is busy\n",
        "ERR:cannot unmount 'tank/stale': not currently mounted\n",
        "", // zfs set mountpoint=none
        "", // zfs destroy (retry)
    ]));
    let calls = runner.calls();
    let c = client(&host, runner);

    c.destroy_dataset("tank/stale", true)
        .await
        .expect("should clear the stale mountpoint and succeed");
    let all = calls.lock().unwrap();
    assert_eq!(all.len(), 4, "expected 4 commands; got {all:?}");
    assert_eq!(all[1], "/usr/local/sbin/zfs unmount -f tank/stale");
    assert_eq!(
        all[2],
        "/usr/local/sbin/zfs set mountpoint=none canmount=off tank/stale"
    );
    assert_eq!(all[3], "/usr/local/sbin/zfs destroy -r tank/stale");
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
