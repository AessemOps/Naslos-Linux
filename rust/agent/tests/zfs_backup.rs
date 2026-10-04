//! Port of the Go agent's backup tests: the send command line and snapshot-name
//! validation are the only backup inputs that reach a command line.

use naslos_agent::zfs::backup::{parse_send_size, send_command_line, validate_snapshot_name};
use naslos_agent::zfs::SendStreamOptions;

#[test]
fn send_command_line_full_and_incremental() {
    let full = send_command_line(&SendStreamOptions {
        dataset: "tank/data".into(),
        to: "snap1".into(),
        from: String::new(),
        raw: true,
    })
    .unwrap();
    assert_eq!(
        full,
        vec!["send".to_string(), "-w".into(), "tank/data@snap1".into()]
    );

    let incremental = send_command_line(&SendStreamOptions {
        dataset: "tank/data".into(),
        to: "snap2".into(),
        from: "snap1".into(),
        raw: false,
    })
    .unwrap();
    assert_eq!(
        incremental,
        vec![
            "send".to_string(),
            "-i".into(),
            "tank/data@snap1".into(),
            "tank/data@snap2".into()
        ]
    );
}

#[test]
fn send_command_line_refusals() {
    // Same base and target.
    let err = send_command_line(&SendStreamOptions {
        dataset: "tank/data".into(),
        to: "snap1".into(),
        from: "snap1".into(),
        raw: true,
    })
    .unwrap_err();
    assert!(err.to_string().contains("base and target are the same"));

    // Bad dataset.
    assert!(send_command_line(&SendStreamOptions {
        dataset: "data".into(),
        to: "snap1".into(),
        from: String::new(),
        raw: true,
    })
    .is_err());

    // Bad snapshot.
    assert!(send_command_line(&SendStreamOptions {
        dataset: "tank/data".into(),
        to: "-x".into(),
        from: String::new(),
        raw: true,
    })
    .is_err());
}

#[test]
fn validate_snapshot_name_cases() {
    for ok in ["buddy-20260101T000000Z-abcd", "snap_1", "a.b:c+d"] {
        assert!(
            validate_snapshot_name(ok).is_ok(),
            "ValidateSnapshotName({ok:?})"
        );
    }
    for bad in ["", "-r", "a/b", "a b", "a@b", "a\\b"] {
        assert!(
            validate_snapshot_name(bad).is_err(),
            "ValidateSnapshotName({bad:?})"
        );
    }
}

#[test]
fn parse_send_size_reads_the_size_line() {
    let out = "dataset\ttank/data@snap1\nsize\t123456\n";
    assert_eq!(parse_send_size(out).unwrap(), 123456);

    assert!(parse_send_size("no size here\n").is_err());
}
