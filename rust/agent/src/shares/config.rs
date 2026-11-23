//! Share configuration apply/status (port of `shares/shares.go`).

use super::{
    read_host_file, write_atomic, Config, SharesClient, Status, CONFIG_DIR, GANESHA_CONF_PATH,
    NSS_DIR, REVISION_PATH, SAMBA_CONF_PATH, SMB_USERS_PATH,
};
use std::collections::HashSet;
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

/// One pending host write: new content plus the previous state to restore.
struct PendingWrite<'a> {
    target: PathBuf,
    content: &'a str,
    mode: u32,
    original: Option<(String, u32)>,
}

impl SharesClient {
    /// Write the rendered configuration to the host and report the result.
    ///
    /// The rendered files are validated before the first rename, every write
    /// is snapshot-then-atomic, and a failure restores the previous working
    /// files. A strictly older revision is refused (R-021, R-054); an equal
    /// revision is an idempotent retry, because the revision file is written
    /// last and an interrupted apply must be safely retryable.
    pub fn apply(&self, cfg: &Config) -> Result<Status, String> {
        if !super::is_available(self.host_root()) {
            return Err(format!(
                "host filesystem not available at {}",
                self.host_root().display()
            ));
        }

        validate_samba_config(&cfg.samba_conf)?;
        validate_ganesha_config(&cfg.ganesha_conf)?;

        let current_revision = read_host_file(&self.host_path(REVISION_PATH))
            .trim()
            .to_string();
        if revision_is_stale(&cfg.revision, &current_revision) {
            return Err(format!(
                "stale revision {} (current {}); refusing the older apply",
                cfg.revision, current_revision
            ));
        }
        // An equal revision is a retry of an interrupted apply: fall through
        // and let the per-file comparison below skip everything already in
        // place. A stale revision is still refused above.

        // smbd needs its private/state/lock dirs before it starts, and the
        // staged testparm file lives in CONFIG_DIR.
        std::fs::create_dir_all(self.host_path(CONFIG_DIR))
            .map_err(|e| format!("creating {CONFIG_DIR}: {e}"))?;
        std::fs::create_dir_all(self.host_path(NSS_DIR))
            .map_err(|e| format!("creating extrausers dir: {e}"))?;
        for dir in ["private", "lock", "state", "cache"] {
            std::fs::create_dir_all(self.host_path(&format!("{CONFIG_DIR}/{dir}")))
                .map_err(|e| format!("creating samba {dir} dir: {e}"))?;
        }

        if read_host_file(&self.host_path(SAMBA_CONF_PATH)) != cfg.samba_conf {
            validate_with_testparm(self, &cfg.samba_conf)?;
        }

        let revision_content = format!("{}\n", cfg.revision);
        let passwd_path = format!("{NSS_DIR}/passwd");
        let group_path = format!("{NSS_DIR}/group");
        let shadow_path = format!("{NSS_DIR}/shadow");
        let files: [(&str, &str, u32); 7] = [
            (SAMBA_CONF_PATH, cfg.samba_conf.as_str(), 0o644),
            (GANESHA_CONF_PATH, cfg.ganesha_conf.as_str(), 0o644),
            // 0600: NT hashes live here.
            (SMB_USERS_PATH, cfg.samba_users.as_str(), 0o600),
            (&passwd_path, cfg.nss_passwd.as_str(), 0o644),
            (&group_path, cfg.nss_group.as_str(), 0o644),
            // 0600: the shadow mirror carries password hashes.
            (&shadow_path, cfg.nss_shadow.as_str(), 0o600),
            // Written last: an interrupted apply must not advertise itself as
            // current, so the API can retry the same revision.
            (REVISION_PATH, revision_content.as_str(), 0o644),
        ];

        // Snapshot the previous content of every file that will change, so a
        // mid-apply failure can put the working configuration back.
        let mut writes: Vec<PendingWrite<'_>> = Vec::new();
        for (in_host, content, mode) in files {
            let target = self.host_path(in_host);
            let current = std::fs::read_to_string(&target).ok();
            let current_mode = std::fs::metadata(&target)
                .ok()
                .map(|m| m.permissions().mode() & 0o777);
            if current.as_deref() == Some(content) {
                if current_mode != Some(mode) {
                    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode))
                        .map_err(|e| format!("chmod {in_host}: {e}"))?;
                }
                continue;
            }
            writes.push(PendingWrite {
                target,
                content,
                mode,
                original: current.map(|data| (data, current_mode.unwrap_or(mode))),
            });
        }

        let mut written: Vec<usize> = Vec::new();
        for (idx, write) in writes.iter().enumerate() {
            if let Err(e) = write_atomic(&write.target, write.content, write.mode) {
                for previous in written.iter().rev() {
                    let previous = &writes[*previous];
                    match &previous.original {
                        Some((data, old_mode)) => {
                            let _ = write_atomic(&previous.target, data, *old_mode);
                        }
                        None => {
                            let _ = std::fs::remove_file(&previous.target);
                        }
                    }
                }
                return Err(format!(
                    "applying share configuration failed ({e}); the previous configuration was restored"
                ));
            }
            written.push(idx);
        }

        self.status()
    }

    /// Report the configuration currently present on the host.
    pub fn status(&self) -> Result<Status, String> {
        let mut st = Status {
            revision: read_host_file(&self.host_path(REVISION_PATH))
                .trim()
                .to_string(),
            samba_conf_path: self.host_path(SAMBA_CONF_PATH).display().to_string(),
            ganesha_conf_path: self.host_path(GANESHA_CONF_PATH).display().to_string(),
            messages: Vec::new(),
            ..Default::default()
        };

        let samba_conf = read_host_file(&self.host_path(SAMBA_CONF_PATH));
        let ganesha_conf = read_host_file(&self.host_path(GANESHA_CONF_PATH));

        if samba_conf.is_empty() {
            st.messages
                .push("no smb.conf applied on this node yet".to_string());
        } else {
            st.applied = true;
            st.smb_share_count = count_samba_sections(&samba_conf);
        }
        if ganesha_conf.is_empty() {
            st.messages
                .push("no ganesha.conf applied on this node yet".to_string());
        } else {
            st.nfs_export_count = count_ganesha_exports(&ganesha_conf);
        }

        Ok(st)
    }
}

/// Run `testparm` on a staged copy when the binary is available.
///
/// The agent image is scratch-based and has no samba tools; the Samba
/// container still runs `testparm` on every reload and keeps the running
/// configuration when it fails. The structural checks below are always
/// enforced before any host file is replaced.
fn validate_with_testparm(shares: &SharesClient, conf: &str) -> Result<(), String> {
    let dir = shares.host_path(CONFIG_DIR);
    let mut staged = tempfile::Builder::new()
        .prefix(".staged-smb-")
        .tempfile_in(&dir)
        .map_err(|e| format!("creating staged smb.conf: {e}"))?;
    {
        use std::io::Write;
        staged
            .as_file_mut()
            .write_all(conf.as_bytes())
            .map_err(|e| format!("writing staged smb.conf: {e}"))?;
        staged
            .as_file()
            .sync_all()
            .map_err(|e| format!("syncing staged smb.conf: {e}"))?;
    }
    match Command::new("testparm")
        .arg("-s")
        .arg(staged.path())
        .output()
    {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => Err(format!(
            "testparm rejected the rendered smb.conf: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Err(e) if e.kind() == ErrorKind::NotFound => {
            tracing::warn!(
                "testparm is not available in the agent image; relying on structural validation"
            );
            Ok(())
        }
        Err(e) => Err(format!("running testparm: {e}")),
    }
}

/// Structural checks mirroring what testparm would refuse.
pub fn validate_samba_config(conf: &str) -> Result<(), String> {
    if conf.trim().is_empty() {
        return Err("rendered smb.conf is empty".to_string());
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut in_section = false;
    for (idx, raw) in conf.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with(['#', ';']) {
            continue;
        }
        if line.ends_with('\\') {
            return Err(format!(
                "smb.conf line {line_no} ends with a continuation character"
            ));
        }
        if let Some(rest) = trimmed.strip_prefix('[') {
            let Some(name) = rest.strip_suffix(']') else {
                return Err(format!(
                    "smb.conf line {line_no} has an unterminated section"
                ));
            };
            let name = name.trim();
            if name.is_empty() {
                return Err(format!("smb.conf line {line_no} has an empty section name"));
            }
            if name.chars().any(|c| matches!(c, '\\' | '#' | '{' | '}')) {
                return Err(format!(
                    "smb.conf section [{name}] contains a config-structure character"
                ));
            }
            if !seen.insert(name.to_ascii_lowercase()) {
                return Err(format!("smb.conf has duplicate section [{name}]"));
            }
            in_section = true;
        } else {
            if !in_section {
                return Err(format!(
                    "smb.conf line {line_no} is a directive before any section"
                ));
            }
            let Some((key, value)) = line.split_once('=') else {
                return Err(format!(
                    "smb.conf line {line_no} is not a key = value directive"
                ));
            };
            if key.trim().is_empty() {
                return Err(format!("smb.conf line {line_no} has an empty key"));
            }
            if value.contains(['{', '}']) {
                return Err(format!(
                    "smb.conf line {line_no} contains a config-structure character"
                ));
            }
        }
    }
    if !seen.contains("global") {
        return Err("rendered smb.conf has no [global] section".to_string());
    }
    Ok(())
}

/// Structural checks for the Ganesha grammar used by the renderer.
pub fn validate_ganesha_config(conf: &str) -> Result<(), String> {
    if conf.trim().is_empty() {
        return Err("rendered ganesha.conf is empty".to_string());
    }
    let mut depth = 0i32;
    let mut export_open = false;
    let mut export_has_path = false;
    for (idx, raw) in conf.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if raw.trim_end().ends_with('\\') {
            return Err(format!(
                "ganesha.conf line {line_no} ends with a continuation character"
            ));
        }
        if line.contains('\\') {
            return Err(format!("ganesha.conf line {line_no} contains a backslash"));
        }
        if !(line.ends_with(';') || line.ends_with('{') || line.ends_with('}')) {
            return Err(format!(
                "ganesha.conf line {line_no} is not terminated with ';' or a block delimiter"
            ));
        }
        if line.starts_with("Path") && line.contains('=') {
            export_has_path = true;
        }
        for ch in line.chars() {
            match ch {
                '{' => {
                    if depth == 0 && line.starts_with("EXPORT") {
                        export_open = true;
                        export_has_path = false;
                    }
                    depth += 1;
                }
                '}' => {
                    depth -= 1;
                    if depth < 0 {
                        return Err(format!(
                            "ganesha.conf line {line_no} closes an unopened block"
                        ));
                    }
                    if depth == 0 && export_open {
                        if !export_has_path {
                            return Err(format!(
                                "ganesha.conf line {line_no} closes an EXPORT block without Path"
                            ));
                        }
                        export_open = false;
                    }
                }
                _ => {}
            }
        }
    }
    if depth != 0 {
        return Err("ganesha.conf has unbalanced braces".to_string());
    }
    Ok(())
}

/// Parse the ordering prefix of a revision: `<millis>-<sequence>-...`.
fn parse_revision(revision: &str) -> Option<(u128, u64)> {
    let mut parts = revision.split('-');
    let millis = parts.next()?.parse().ok()?;
    let sequence = parts.next()?.parse().ok()?;
    Some((millis, sequence))
}

/// Whether `new` is older than `current` and must be refused.
///
/// An equal revision is not stale: the revision file is written after every
/// other file, so the API retrying the same revision after an interrupted
/// apply must succeed as a no-op. A legacy revision (the old content hash) has
/// no ordering prefix and is accepted, so the first apply after an upgrade
/// still lands.
pub fn revision_is_stale(new: &str, current: &str) -> bool {
    revision_order(new, current) == Some(std::cmp::Ordering::Less)
}

/// Compare the ordering prefixes of two revisions. `None` when either side has
/// no ordering prefix; such a pair is always accepted.
fn revision_order(new: &str, current: &str) -> Option<std::cmp::Ordering> {
    match (parse_revision(new), parse_revision(current)) {
        (Some(new), Some(current)) => Some(new.cmp(&current)),
        _ => None,
    }
}

/// Count `[share]` sections excluding `[global]`.
pub fn count_samba_sections(conf: &str) -> i32 {
    let mut count = 0;
    for line in conf.split('\n') {
        let line = line.trim();
        if !line.starts_with('[') || !line.ends_with(']') {
            continue;
        }
        if line.eq_ignore_ascii_case("[global]") {
            continue;
        }
        count += 1;
    }
    count
}

/// Count `EXPORT {` blocks in the rendered Ganesha config.
pub fn count_ganesha_exports(conf: &str) -> i32 {
    let mut count = 0;
    for line in conf.split('\n') {
        if line.trim() == "EXPORT {" {
            count += 1;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shares::SharesClient;

    const GOOD_SAMBA: &str = "[global]\n   workgroup = NASLOS\n\n[media]\n   path = /var/mnt/tank/media\n   read only = no\n";
    const GOOD_GANESHA: &str = "NFS_CORE_PARAM {\n    Protocols = 4;\n}\n\nEXPORT {\n    Export_Id = 1;\n    Path = /var/mnt/tank/media;\n    Protocols = 4;\n}\n";

    fn client() -> (tempfile::TempDir, SharesClient) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("var/lib/naslos/shares/extrausers")).unwrap();
        let client = SharesClient::new(dir.path());
        (dir, client)
    }

    fn config(revision: &str, samba: &str, ganesha: &str) -> Config {
        Config {
            samba_conf: samba.to_string(),
            ganesha_conf: ganesha.to_string(),
            samba_users: String::new(),
            nss_passwd: "root:x:0:0:root:/root:/bin/sh\n".to_string(),
            nss_group: "root:x:0:\n".to_string(),
            nss_shadow: String::new(),
            revision: revision.to_string(),
            share_count: 1,
        }
    }

    #[test]
    fn invalid_samba_config_is_rejected() {
        let bad_name = "[global]\n   workgroup = NASLOS\n\n[bad\\name]\n   path = /var/mnt/x\n";
        assert!(validate_samba_config(bad_name).is_err());
        let continuation = "[global]\n   workgroup = NASLOS\n\n[media]\n   path = /var/mnt/x\\\n   read only = no\n";
        assert!(validate_samba_config(continuation).is_err());
        let no_global = "[media]\n   path = /var/mnt/x\n";
        assert!(validate_samba_config(no_global).is_err());
        assert!(validate_samba_config(GOOD_SAMBA).is_ok());
    }

    #[test]
    fn invalid_ganesha_config_is_rejected() {
        let unbalanced = "EXPORT {\n    Path = /var/mnt/x;\n";
        assert!(validate_ganesha_config(unbalanced).is_err());
        let missing_path = "EXPORT {\n    Export_Id = 1;\n}\n";
        assert!(validate_ganesha_config(missing_path).is_err());
        let continuation = "EXPORT {\n    Path = /var/mnt/x;\\\n}\n";
        assert!(validate_ganesha_config(continuation).is_err());
        assert!(validate_ganesha_config(GOOD_GANESHA).is_ok());
    }

    #[test]
    fn stale_revision_is_refused_and_previous_config_is_kept() {
        let (_dir, client) = client();
        let first = config(
            "0000000000002-0000000001-00000001-aaaa",
            GOOD_SAMBA,
            GOOD_GANESHA,
        );
        client.apply(&first).unwrap();
        let applied = client.status().unwrap();
        assert_eq!(applied.revision, first.revision);

        let invalid = config(
            "0000000000003-0000000002-00000002-bbbb",
            "[global]\n   workgroup = NASLOS\n\n[bad\\name]\n   path = /var/mnt/x\n",
            GOOD_GANESHA,
        );
        let err = client.apply(&invalid).unwrap_err();
        assert!(err.contains("smb.conf"), "{err}");
        assert_eq!(client.status().unwrap().revision, first.revision);
        assert_eq!(
            read_host_file(&client.host_path(SAMBA_CONF_PATH)),
            GOOD_SAMBA,
            "an invalid render must not replace the working config"
        );

        let stale = config(
            "0000000000001-0000000001-00000001-cccc",
            GOOD_SAMBA,
            GOOD_GANESHA,
        );
        let err = client.apply(&stale).unwrap_err();
        assert!(err.contains("stale revision"), "{err}");
        assert_eq!(client.status().unwrap().revision, first.revision);
    }

    #[test]
    fn apply_writes_all_files_and_revision_last() {
        let (_dir, client) = client();
        let cfg = config(
            "0000000000005-0000000001-00000001-dddd",
            GOOD_SAMBA,
            GOOD_GANESHA,
        );
        let status = client.apply(&cfg).unwrap();
        assert!(status.applied);
        assert_eq!(status.smb_share_count, 1);
        assert_eq!(status.nfs_export_count, 1);
        assert_eq!(
            read_host_file(&client.host_path(SAMBA_CONF_PATH)),
            GOOD_SAMBA
        );
        assert_eq!(
            read_host_file(&client.host_path(GANESHA_CONF_PATH)),
            GOOD_GANESHA
        );
        assert_eq!(status.revision, cfg.revision);
    }

    #[test]
    fn an_equal_revision_is_an_idempotent_retry() {
        let (_dir, client) = client();
        let cfg = config(
            "0000000000005-0000000001-00000001-eeee",
            GOOD_SAMBA,
            GOOD_GANESHA,
        );
        client.apply(&cfg).unwrap();
        // The API retries the same revision after an interrupted apply; the
        // second call must succeed and leave the same configuration.
        let retried = client.apply(&cfg).unwrap();
        assert!(retried.applied);
        assert_eq!(retried.revision, cfg.revision);
        assert_eq!(
            read_host_file(&client.host_path(SAMBA_CONF_PATH)),
            GOOD_SAMBA
        );
        assert_eq!(
            read_host_file(&client.host_path(GANESHA_CONF_PATH)),
            GOOD_GANESHA
        );
    }

    #[test]
    fn revision_ordering_accepts_legacy_current_hashes() {
        assert!(!revision_is_stale(
            "0000000000002-0000000002-00000000-x",
            "0000000000001-0000000001-00000000-x"
        ));
        assert!(revision_is_stale(
            "0000000000001-0000000001-00000000-x",
            "0000000000002-0000000002-00000000-x"
        ));
        assert!(!revision_is_stale(
            "0000000000002-0000000002-00000000-x",
            "0000000000002-0000000002-00000000-x"
        ));
        assert!(!revision_is_stale(
            "0000000000003-0000000001-00000000-x",
            "0000000000002-0000000001-00000000-x"
        ));
        assert!(!revision_is_stale(
            "0000000000001-0000000001-00000000-x",
            "deadbeefdeadbeef"
        ));
    }
}
