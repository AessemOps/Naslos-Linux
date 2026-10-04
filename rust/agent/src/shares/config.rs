//! Share configuration apply/status (port of `shares/shares.go`).

use super::{
    read_host_file, write_atomic, Config, SharesClient, Status, CONFIG_DIR, GANESHA_CONF_PATH,
    NSS_DIR, REVISION_PATH, SAMBA_CONF_PATH, SMB_USERS_PATH,
};
use std::os::unix::fs::PermissionsExt;

impl SharesClient {
    /// Write the rendered configuration to the host and report the result.
    pub fn apply(&self, cfg: &Config) -> Result<Status, String> {
        if !super::is_available(self.host_root()) {
            return Err(format!(
                "host filesystem not available at {}",
                self.host_root().display()
            ));
        }

        // Only rewrite changed files: serving containers reload on mtime changes.
        if read_host_file(&self.host_path(SAMBA_CONF_PATH)) != cfg.samba_conf {
            write_atomic(&self.host_path(SAMBA_CONF_PATH), &cfg.samba_conf, 0o644)?;
        }
        if read_host_file(&self.host_path(GANESHA_CONF_PATH)) != cfg.ganesha_conf {
            write_atomic(&self.host_path(GANESHA_CONF_PATH), &cfg.ganesha_conf, 0o644)?;
        }
        if read_host_file(&self.host_path(SMB_USERS_PATH)) != cfg.samba_users {
            // 0600: NT hashes live here.
            write_atomic(&self.host_path(SMB_USERS_PATH), &cfg.samba_users, 0o600)?;
        }

        std::fs::create_dir_all(self.host_path(NSS_DIR))
            .map_err(|e| format!("creating extrausers dir: {e}"))?;
        let nss_files = [
            ("passwd", &cfg.nss_passwd, 0o644u32),
            ("group", &cfg.nss_group, 0o644),
            // 0600: the shadow mirror carries password hashes.
            ("shadow", &cfg.nss_shadow, 0o600),
        ];
        for (name, content, mode) in nss_files {
            let in_host = format!("{NSS_DIR}/{name}");
            let target = self.host_path(&in_host);
            if read_host_file(&target) == *content {
                // Content is current, but an upgrade may have changed the mode.
                if let Ok(meta) = std::fs::metadata(&target) {
                    if meta.permissions().mode() & 0o777 != mode {
                        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode))
                            .map_err(|e| format!("chmod {in_host}: {e}"))?;
                    }
                }
                continue;
            }
            write_atomic(&target, content, mode)?;
        }
        write_atomic(
            &self.host_path(REVISION_PATH),
            &format!("{}\n", cfg.revision),
            0o644,
        )?;

        // smbd needs its private/state/lock dirs before it starts.
        for dir in ["private", "lock", "state", "cache"] {
            std::fs::create_dir_all(self.host_path(&format!("{CONFIG_DIR}/{dir}")))
                .map_err(|e| format!("creating samba {dir} dir: {e}"))?;
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
