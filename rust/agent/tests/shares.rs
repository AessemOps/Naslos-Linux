//! Port of the Go agent's shares tests: folder confinement (incl. symlink
//! escape, PF-L5) and secret-file permissions (CR-15).

use naslos_agent::shares::folders::{
    validate_folder_name, DATASETS_BASE,
};
use naslos_agent::shares::{Config, SharesClient};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

fn client(root: &tempfile::TempDir) -> SharesClient {
    SharesClient::new(root.path())
}

#[test]
fn clean_folder_path_cases() {
    let root = tempfile::tempdir().unwrap();
    let c = client(&root);

    let cases: Vec<(&str, Option<&str>)> = vec![
        ("/var/mnt/test", Some("/var/mnt/test")),
        ("/var/mnt/test/media", Some("/var/mnt/test/media")),
        ("/var/mnt/test/media/", Some("/var/mnt/test/media")),
        ("/var/mnt/test/./media", Some("/var/mnt/test/media")),
        ("  /var/mnt/test  ", Some("/var/mnt/test")),
        ("", None),
        ("var/mnt/test", None),
        ("../var/mnt", None),
        ("/var/mnt", Some("/var/mnt")),
        ("/var/mnt/test/../../etc", None),
        ("/var/mnt/test/../..", None),
        ("/etc", None),
        ("/var/lib/naslos/shares", None),
        ("relative/path", None),
        ("/var/mnt-other/test", None),
    ];

    for (input, want) in cases {
        match c.clean_folder_path(input) {
            Ok(got) => {
                let want = want.unwrap_or_else(|| panic!("CleanFolderPath({input:?}) succeeded, want error"));
                assert_eq!(got, want, "CleanFolderPath({input:?})");
            }
            Err(_) => assert!(want.is_none(), "CleanFolderPath({input:?}) errored, want {want:?}"),
        }
    }
}

#[test]
fn clean_folder_path_rejects_symlink_escape() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().join("var/mnt");
    std::fs::create_dir_all(&base).unwrap();
    let outside = root.path().join("etc");
    std::fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, base.join("escape")).unwrap();
    std::fs::create_dir_all(base.join("real")).unwrap();
    std::os::unix::fs::symlink(base.join("real"), base.join("alias")).unwrap();

    let c = client(&root);
    assert!(
        c.clean_folder_path("/var/mnt/escape").is_err(),
        "CleanFolderPath followed a symlink out of the datasets base"
    );
    assert_eq!(
        c.clean_folder_path("/var/mnt/alias").unwrap(),
        "/var/mnt/alias"
    );
}

#[test]
fn validate_folder_name_cases() {
    for name in ["media", "My Photos", "backup-2026", "a", "_hidden", "dossier"] {
        assert!(validate_folder_name(name).is_ok(), "ValidateFolderName({name:?})");
    }

    let long = "x".repeat(256);
    let invalid: Vec<&str> = vec![
        "",
        "   ",
        " leading",
        "trailing ",
        ".",
        "..",
        "a/b",
        "a\\b",
        "a\0b",
        "two\nlines",
        ".zfs",
        &long,
    ];
    for name in invalid {
        assert!(validate_folder_name(name).is_err(), "ValidateFolderName({name:?})");
    }
}

#[test]
fn folder_operations() {
    let root = tempfile::tempdir().unwrap();
    let dataset = root.path().join("var/mnt/test");
    std::fs::create_dir_all(&dataset).unwrap();
    std::fs::create_dir_all(dataset.join(".zfs")).unwrap();

    let c = client(&root);

    assert!(c.list_folders("/var/mnt/test").unwrap().is_empty(), ".zfs must be hidden");

    let created = c.create_folder("/var/mnt/test", "media").unwrap();
    assert_eq!(created, "/var/mnt/test/media");
    assert!(dataset.join("media").is_dir());

    let folders = c.list_folders("/var/mnt/test").unwrap();
    assert_eq!(folders, vec!["media".to_string()]);

    assert!(
        c.create_folder("/var/mnt/test", "media").is_err(),
        "CreateFolder over an existing folder must error"
    );
    assert!(
        c.create_folder("/var/mnt/test/missing", "nested").is_err(),
        "CreateFolder under a missing parent must error"
    );

    c.delete_folder("/var/mnt/test/media").unwrap();
    assert!(!dataset.join("media").exists());

    std::fs::create_dir_all(dataset.join("keep")).unwrap();
    std::fs::write(dataset.join("keep/file.txt"), b"data").unwrap();
    assert!(
        c.delete_folder("/var/mnt/test/keep").is_err(),
        "DeleteFolder(non-empty) must error"
    );
    assert!(
        c.delete_folder(DATASETS_BASE).is_err(),
        "DeleteFolder(datasets root) must error"
    );
}

#[test]
fn apply_restricts_secret_mirrors() {
    let root = tempfile::tempdir().unwrap();
    let c = client(&root);
    let cfg = Config {
        samba_conf: "[global]\n   workgroup = NASLOS\n".into(),
        ganesha_conf: "EXPORT {\n}\n".into(),
        samba_users: "alice:0:NO_PASSWORD:8846F7EAEE8FB117AD06BDD830B7586C:[U          ]:LCT-00000000:\n".into(),
        nss_passwd: "alice:x:10001:10000::/home/alice:/bin/bash\n".into(),
        nss_group: "users:x:10000:\n".into(),
        nss_shadow: "alice:$6$hash:19000:0:99999:7:::\n".into(),
        revision: "7".into(),
        share_count: 0,
    };
    c.apply(&cfg).expect("Apply");

    let checks = [
        ("/var/lib/naslos/shares/smbusers", 0o600u32),
        ("/var/lib/naslos/shares/extrausers/shadow", 0o600),
        ("/var/lib/naslos/shares/extrausers/passwd", 0o644),
        ("/var/lib/naslos/shares/extrausers/group", 0o644),
    ];
    for (in_host, mode) in checks {
        let path: PathBuf = root.path().join(in_host.trim_start_matches('/'));
        let meta = std::fs::metadata(&path).unwrap_or_else(|e| panic!("stat {in_host}: {e}"));
        assert_eq!(
            meta.permissions().mode() & 0o777,
            mode,
            "{in_host} mode"
        );
    }

    // A content-identical Apply must still fix a changed mode (0644 -> 0600).
    let shadow = root
        .path()
        .join("var/lib/naslos/shares/extrausers/shadow");
    std::fs::set_permissions(&shadow, std::fs::Permissions::from_mode(0o644)).unwrap();
    c.apply(&cfg).expect("second Apply");
    let meta = std::fs::metadata(&shadow).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
}
