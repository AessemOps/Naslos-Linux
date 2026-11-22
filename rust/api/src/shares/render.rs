//! smb.conf / ganesha.conf rendering (port of `shares/config.go`).

use super::{Manager, Protocol, Share};

/// VFS modules every share inherits from `[global]`. A share that needs extra
/// modules must repeat this list, because a share-level `vfs objects` replaces
/// the global one rather than extending it.
const GLOBAL_SMB_VFS_OBJECTS: [&str; 2] = ["fruit", "streams_xattr"];

/// Timestamp prefix for snapshots exposed as previous versions. Only names
/// matching this pattern are shown; arbitrary snapshot names are ignored by
/// `vfs_shadow_copy2`.
const SHADOW_COPY_FORMAT: &str = "naslos-%Y-%m-%d_%H-%M-%S";

/// Operations recorded by `full_audit` for an auditing share. The names are
/// fixed to the Samba version in the serving image; an unknown name makes the
/// module refuse the share, so this list must be re-checked on an image bump.
const AUDIT_SUCCESS_OPS: &str =
    "connect disconnect open close create_file pwrite ftruncate renameat mkdirat unlinkat";
const AUDIT_FAILURE_OPS: &str = "connect open create_file renameat mkdirat unlinkat";

impl Manager {
    /// Render `smb.conf` for all enabled SMB/AFP shares.
    pub fn generate_samba_config(&self) -> String {
        self.render_samba_config(smb_multichannel_enabled())
    }

    fn render_samba_config(&self, multichannel: bool) -> String {
        let mut sb = String::new();
        sb.push_str("[global]\n");
        sb.push_str("   workgroup = NASLOS\n");
        sb.push_str("   server string = Naslos\n");
        sb.push_str("   security = user\n");
        if let Some(name) = self.netbios_name() {
            sb.push_str(&format!("   netbios name = {name}\n"));
        }
        sb.push_str("   map to guest = Never\n");
        sb.push_str("   server min protocol = SMB3\n");
        sb.push_str(if multichannel {
            "   server multi channel support = yes\n"
        } else {
            "   server multi channel support = no\n"
        });
        sb.push_str("   smb encrypt = desired\n");
        sb.push_str("   log file = /var/log/samba/%m.log\n");
        sb.push_str("   max log size = 1000\n");
        // full_audit:syslog = no emits records as debug level 1 messages, and
        // the container has no syslog daemon. Raising the level and selecting
        // the file backend puts the records in the smbd log instead. Both are
        // only rendered when auditing is actually enabled, so a normal install
        // keeps today's logging behaviour.
        if self.any_audited_share() {
            sb.push_str("   logging = file\n");
            sb.push_str("   log level = 1\n");
        }
        sb.push_str("   dns proxy = no\n");
        sb.push_str("   passdb backend = tdbsam:/etc/naslos/shares/private/passdb.tdb\n");
        sb.push_str("   private dir = /etc/naslos/shares/private\n");
        sb.push_str("   lock directory = /etc/naslos/shares/lock\n");
        sb.push_str("   state directory = /etc/naslos/shares/state\n");
        sb.push_str("   cache directory = /etc/naslos/shares/cache\n");
        sb.push_str(&format!(
            "   vfs objects = {}\n",
            GLOBAL_SMB_VFS_OBJECTS.join(" ")
        ));
        sb.push_str("   fruit:time machine = yes\n");
        sb.push_str("   fruit:model = MacSamba\n");
        sb.push_str("   fruit:metadata = stream\n");
        sb.push_str("   fruit:posix_rename = yes\n");
        sb.push_str("   fruit:veto_appledouble = no\n");
        sb.push_str("   fruit:nfs_aces = no\n");
        sb.push_str("   fruit:wipe_intentionally_left_blank_rfork = yes\n");
        sb.push_str("   fruit:delete_empty_adfiles = yes\n\n");

        for share in self.list() {
            if share.protocol != Protocol::Smb && share.protocol != Protocol::Afp {
                continue;
            }
            if !share.enabled {
                continue;
            }

            sb.push_str(&format!("[{}]\n", share.name));
            sb.push_str(&format!("   path = {}\n", share.path));
            sb.push_str(&format!("   comment = {}\n", share.description));
            sb.push_str(if share.read_only {
                "   read only = yes\n"
            } else {
                "   read only = no\n"
            });
            sb.push_str(if share.browseable {
                "   browseable = yes\n"
            } else {
                "   browseable = no\n"
            });

            let clients = effective_clients(&share);
            if !has_wildcard(&clients) {
                sb.push_str(&format!("   hosts allow = {}\n", clients.join(" ")));
                sb.push_str("   hosts deny = all\n");
            }

            let access = access_list(&share);
            if !access.is_empty() {
                sb.push_str(&format!("   valid users = {}\n", access.join(" ")));
            }

            if share.time_machine {
                sb.push_str("   fruit:time machine = yes\n");
                sb.push_str("   fruit:time machine max size = 1T\n");
            }

            // A share-level VFS stack replaces the global one, so the global
            // modules are always repeated here.
            if share.shadow_copies || share.audit {
                let mut objects: Vec<&str> = Vec::new();
                if share.shadow_copies {
                    objects.push("shadow_copy2");
                }
                objects.extend(GLOBAL_SMB_VFS_OBJECTS);
                if share.audit {
                    objects.push("full_audit");
                }
                sb.push_str(&format!("   vfs objects = {}\n", objects.join(" ")));
            }

            if share.shadow_copies {
                // `.zfs` is the snapshot directory of the dataset that backs
                // the share. `snapdirseverywhere` finds it when the share is a
                // subfolder rather than the dataset root; it stops at the
                // mountpoint, so snapshots of a parent dataset are not exposed.
                sb.push_str("   shadow:snapdir = .zfs/snapshot\n");
                sb.push_str("   shadow:snapdirseverywhere = yes\n");
                sb.push_str("   shadow:sort = desc\n");
                sb.push_str(&format!("   shadow:format = {SHADOW_COPY_FORMAT}\n"));
                sb.push_str("   shadow:localtime = yes\n");
            }

            if share.audit {
                sb.push_str("   full_audit:prefix = %u|%I|%m|%S\n");
                sb.push_str(&format!("   full_audit:success = {AUDIT_SUCCESS_OPS}\n"));
                sb.push_str(&format!("   full_audit:failure = {AUDIT_FAILURE_OPS}\n"));
                // No syslog daemon runs in the serving container.
                sb.push_str("   full_audit:syslog = no\n");
            }

            sb.push_str("   create mask = 0660\n");
            sb.push_str("   directory mask = 0770\n\n");
        }

        sb
    }

    /// Render an NFS-Ganesha configuration for all enabled NFS shares.
    pub fn generate_ganesha_config(&self) -> String {
        let mut sb = String::new();
        sb.push_str("# Generated by Naslos - do not edit.\n");
        sb.push_str("NFS_CORE_PARAM {\n");
        sb.push_str("    Protocols = 4;\n");
        sb.push_str("    Enable_NLM = false;\n");
        sb.push_str("    Enable_RQUOTA = false;\n");
        sb.push_str("    NFS_Port = 2049;\n");
        sb.push_str("    mount_path_pseudo = true;\n");
        sb.push_str("}\n\n");
        sb.push_str("NFSv4 {\n");
        sb.push_str("    Grace_Period = 10;\n");
        sb.push_str("    Lease_Lifetime = 90;\n");
        sb.push_str("}\n\n");
        sb.push_str("NFS_KRB5 {\n");
        sb.push_str("    Active_krb5 = false;\n");
        sb.push_str("}\n\n");

        for share in self.list() {
            if share.protocol != Protocol::Nfs || !share.enabled {
                continue;
            }
            sb.push_str(&ganesha_export(&share));
        }
        sb
    }

    /// The NetBIOS name Samba advertises, sanitised, or `None`.
    pub fn netbios_name(&self) -> Option<String> {
        let name = sanitize_netbios_name(&std::env::var("SMB_NETBIOS_NAME").unwrap_or_default());
        if name.is_empty() {
            None
        } else {
            Some(name)
        }
    }

    /// Whether any enabled SMB share asks for `full_audit` logging.
    fn any_audited_share(&self) -> bool {
        self.list()
            .iter()
            .any(|s| s.enabled && s.protocol == Protocol::Smb && s.audit)
    }
}

/// `server multi channel support` from `SHARES_SMB_MULTICHANNEL`.
///
/// Samba 4.15+ enables multichannel on Linux by default, so an unset value
/// renders `yes` and preserves the serving image's behaviour. Only an explicit
/// false/off value disables it (e.g. for a single-NIC or virtual-interface
/// setup where advertising extra channels confuses clients).
pub fn smb_multichannel_enabled() -> bool {
    match std::env::var("SHARES_SMB_MULTICHANNEL") {
        Ok(value) => parse_switch(&value).unwrap_or(true),
        Err(_) => true,
    }
}

fn parse_switch(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn client_cidr() -> String {
    std::env::var("NASLOS_LAN_CIDR")
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// The client list to render: the share's own hosts, else the LAN CIDR, else
/// localhost (fail closed).
pub fn effective_clients(share: &Share) -> Vec<String> {
    let hosts = super::normalize_list(&share.allowed_hosts);
    if !hosts.is_empty() {
        return hosts;
    }
    let cidr = client_cidr();
    if !cidr.is_empty() {
        return vec![cidr];
    }
    vec!["127.0.0.1/32".to_string()]
}

fn has_wildcard(clients: &[String]) -> bool {
    clients.iter().any(|c| c.trim() == "*")
}

fn ganesha_export(share: &Share) -> String {
    let access = if share.read_only { "RO" } else { "RW" };
    let squash = if share.no_root_squash {
        "No_Root_Squash"
    } else {
        "Root_Squash"
    };
    let clients = effective_clients(share);

    let mut sb = String::new();
    sb.push_str("EXPORT {\n");
    sb.push_str(&format!(
        "    Export_Id = {};\n",
        ganesha_export_id(&share.name)
    ));
    sb.push_str(&format!("    Path = {};\n", share.path));
    sb.push_str(&format!("    Pseudo = /{};\n", share.name));
    sb.push_str(&format!("    Access_Type = {access};\n"));
    sb.push_str(&format!("    Squash = {squash};\n"));
    sb.push_str("    SecType = sys;\n");
    sb.push_str("    Protocols = 4;\n");
    sb.push_str("    Transports = TCP;\n");
    sb.push_str("    FSAL {\n");
    sb.push_str("        Name = VFS;\n");
    sb.push_str("    }\n");

    for host in &clients {
        let host = host.trim();
        if host.is_empty() {
            continue;
        }
        sb.push_str("    CLIENT {\n");
        sb.push_str(&format!("        Clients = {host};\n"));
        sb.push_str(&format!("        Access_Type = {access};\n"));
        sb.push_str("        Protocols = 4;\n");
        sb.push_str(&format!("        Squash = {squash};\n"));
        sb.push_str("    }\n");
    }

    sb.push_str("}\n\n");
    sb
}

/// A stable, unique non-zero Export_Id derived from the share name.
pub fn ganesha_export_id(name: &str) -> u32 {
    let mut hash: u32 = 0x811c9dc5;
    for b in format!("nfs-export:{name}").bytes() {
        hash ^= b as u32;
        hash = hash.wrapping_mul(0x01000193);
    }
    1 + (hash % 60000)
}

/// The `valid users` entry for a share: ValidUsers as-is (a `@`-prefixed entry
/// is a group), ValidGroups prefixed with `@`.
pub fn access_list(share: &Share) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut add = |entry: String| {
        if entry.is_empty() || !seen.insert(entry.clone()) {
            return;
        }
        out.push(entry);
    };

    for u in &share.valid_users {
        let u = u.trim();
        if u.is_empty() {
            continue;
        }
        if u.starts_with('@') {
            add(u.to_string());
            continue;
        }
        add(u.replace('@', ""));
    }
    for g in &share.valid_groups {
        let g = g.trim().trim_start_matches('@');
        if g.is_empty() {
            continue;
        }
        add(format!("@{g}"));
    }
    out
}

/// Enforce the NetBIOS rules: uppercase, single label, ≤15 chars, A-Z0-9-_.
pub fn sanitize_netbios_name(name: &str) -> String {
    let name = name.trim().to_uppercase();
    if name.is_empty() {
        return String::new();
    }
    let name = match name.find('.') {
        Some(i) => &name[..i],
        None => &name[..],
    };
    let mut out = String::new();
    for r in name.chars() {
        if r.is_ascii_uppercase() || r.is_ascii_digit() || r == '-' || r == '_' {
            out.push(r);
        }
    }
    out.chars().take(15).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shares::{CreateShareRequest, Protocol};

    fn manager_with(shares: Vec<CreateShareRequest>) -> Manager {
        let dir = tempfile::tempdir().unwrap();
        let mut m = Manager::new("", "/var/mnt");
        for req in shares {
            m.create(&req).unwrap();
        }
        let _ = dir;
        m
    }

    fn req(name: &str, path: &str, protocol: Protocol) -> CreateShareRequest {
        CreateShareRequest {
            name: name.into(),
            path: path.into(),
            protocol,
            browseable: Some(true),
            enabled: Some(true),
            ..Default::default()
        }
    }

    #[test]
    fn samba_config_renders_shares_in_stable_order() {
        let m = manager_with(vec![
            req("zzz", "/var/mnt/tank/zzz", Protocol::Smb),
            req("aaa", "/var/mnt/tank/aaa", Protocol::Smb),
        ]);
        let conf = m.generate_samba_config();
        let a = conf.find("[aaa]").unwrap();
        let z = conf.find("[zzz]").unwrap();
        assert!(a < z, "shares must render sorted by name");
        assert!(conf.contains("   path = /var/mnt/tank/aaa\n"));
        assert!(conf.contains("   hosts deny = all\n"));
    }

    #[test]
    fn wildcard_host_disables_hosts_deny() {
        let mut r = req("pub", "/var/mnt/tank/pub", Protocol::Smb);
        r.allowed_hosts = vec!["*".into()];
        let m = manager_with(vec![r]);
        let conf = m.generate_samba_config();
        assert!(!conf.contains("hosts deny"));
        assert!(!conf.contains("hosts allow"));
    }

    #[test]
    fn valid_groups_are_at_prefixed() {
        let mut r = req("team", "/var/mnt/tank/team", Protocol::Smb);
        r.valid_groups = vec!["naslos_users".into()];
        r.valid_users = vec!["alice".into()];
        let m = manager_with(vec![r]);
        let conf = m.generate_samba_config();
        assert!(conf.contains("   valid users = alice @naslos_users\n"));
    }

    #[test]
    fn ganesha_export_block_and_root_squash() {
        let mut r = req("media", "/var/mnt/tank/media", Protocol::Nfs);
        r.no_root_squash = true;
        r.read_only = true;
        let m = manager_with(vec![r]);
        let conf = m.generate_ganesha_config();
        assert!(conf.contains("EXPORT {\n"));
        assert!(conf.contains("    Pseudo = /media;\n"));
        assert!(conf.contains("    Access_Type = RO;\n"));
        assert!(conf.contains("    Squash = No_Root_Squash;\n"));
        assert!(conf.contains("        Clients = 127.0.0.1/32;\n"));
    }

    #[test]
    fn ganesha_export_id_is_stable_and_nonzero() {
        let id = ganesha_export_id("media");
        assert!((1..=60000).contains(&id));
        assert_eq!(id, ganesha_export_id("media"));
    }

    #[test]
    fn netbios_name_is_sanitised() {
        assert_eq!(sanitize_netbios_name("naslos.local"), "NASLOS");
        assert_eq!(sanitize_netbios_name("my nas"), "MYNAS");
        assert_eq!(sanitize_netbios_name(""), "");
        assert_eq!(sanitize_netbios_name(&"a".repeat(30)).len(), 15);
    }

    #[test]
    fn injection_in_description_is_refused_at_create() {
        let mut r = req("evil", "/var/mnt/tank/evil", Protocol::Smb);
        r.description = "x\n[injected]".into();
        let mut m = Manager::new("", "/var/mnt");
        assert!(
            m.create(&r).is_err(),
            "a newline in a field must be refused"
        );
    }

    #[test]
    fn multichannel_is_explicit_and_defaults_to_the_samba_default() {
        let m = Manager::new("", "/var/mnt");
        let on = m.render_samba_config(true);
        assert!(on.contains("   server multi channel support = yes\n"));
        let off = m.render_samba_config(false);
        assert!(off.contains("   server multi channel support = no\n"));
        assert!(parse_switch("TRUE").unwrap());
        assert!(!parse_switch("off").unwrap());
        assert!(parse_switch("maybe").is_none());
    }

    #[test]
    fn a_plain_share_keeps_the_global_vfs_stack_and_no_audit_logging() {
        let m = manager_with(vec![req("plain", "/var/mnt/tank/plain", Protocol::Smb)]);
        let conf = m.render_samba_config(true);
        assert!(conf.contains("   vfs objects = fruit streams_xattr\n"));
        let stanza = conf.split("[plain]").nth(1).unwrap();
        assert!(!stanza.contains("vfs objects"));
        assert!(!conf.contains("shadow_copy2"));
        assert!(!conf.contains("full_audit"));
        assert!(!conf.contains("logging = file"));
        assert!(!conf.contains("log level"));
    }

    #[test]
    fn shadow_copies_renders_the_zfs_shadow_copy_stack() {
        let mut r = req("media", "/var/mnt/tank/media", Protocol::Smb);
        r.shadow_copies = true;
        let m = manager_with(vec![r]);
        let conf = m.render_samba_config(true);
        assert!(conf.contains("   vfs objects = shadow_copy2 fruit streams_xattr\n"));
        assert!(conf.contains("   shadow:snapdir = .zfs/snapshot\n"));
        assert!(conf.contains("   shadow:snapdirseverywhere = yes\n"));
        assert!(conf.contains("   shadow:sort = desc\n"));
        assert!(conf.contains("   shadow:format = naslos-%Y-%m-%d_%H-%M-%S\n"));
        assert!(conf.contains("   shadow:localtime = yes\n"));
        assert!(!conf.contains("full_audit"));
    }

    #[test]
    fn audit_renders_full_audit_and_file_logging_only_for_audited_shares() {
        let mut r = req("records", "/var/mnt/tank/records", Protocol::Smb);
        r.audit = true;
        let m = manager_with(vec![r]);
        let conf = m.render_samba_config(true);
        assert!(conf.contains("   vfs objects = fruit streams_xattr full_audit\n"));
        assert!(conf.contains("   full_audit:prefix = %u|%I|%m|%S\n"));
        assert!(conf.contains(
            "   full_audit:success = connect disconnect open close create_file pwrite ftruncate renameat mkdirat unlinkat\n"
        ));
        assert!(conf.contains(
            "   full_audit:failure = connect open create_file renameat mkdirat unlinkat\n"
        ));
        assert!(conf.contains("   full_audit:syslog = no\n"));
        assert!(conf.contains("   logging = file\n"));
        assert!(conf.contains("   log level = 1\n"));
    }

    #[test]
    fn shadow_copies_and_audit_compose_in_one_vfs_stack() {
        let mut r = req("both", "/var/mnt/tank/both", Protocol::Smb);
        r.shadow_copies = true;
        r.audit = true;
        let m = manager_with(vec![r]);
        let conf = m.render_samba_config(true);
        assert!(conf.contains("   vfs objects = shadow_copy2 fruit streams_xattr full_audit\n"));
        assert!(conf.contains("   shadow:snapdir = .zfs/snapshot\n"));
        assert!(conf.contains("   full_audit:syslog = no\n"));
    }

    #[test]
    fn smb_only_options_are_refused_on_nfs_shares() {
        let mut m = Manager::new("", "/var/mnt");
        let mut shadow = req("media", "/var/mnt/tank/media", Protocol::Nfs);
        shadow.shadow_copies = true;
        let err = m.create(&shadow).unwrap_err();
        assert!(err.contains("shadowCopies"), "{err}");

        let mut audit = req("records", "/var/mnt/tank/records", Protocol::Nfs);
        audit.audit = true;
        let err = m.create(&audit).unwrap_err();
        assert!(err.contains("audit"), "{err}");

        // The same rule applies to an update, not just a create.
        let nfs = req("plain", "/var/mnt/tank/plain", Protocol::Nfs);
        let created = m.create(&nfs).unwrap();
        let req = crate::shares::UpdateShareRequest {
            audit: Some(true),
            ..Default::default()
        };
        let err = m.update(&created.name, &req).unwrap_err();
        assert!(err.contains("audit"), "{err}");
    }

    #[test]
    fn share_path_inside_the_zfs_snapshot_directory_is_refused() {
        let mut m = Manager::new("", "/var/mnt");
        let mut r = req("snap", "/var/mnt/tank/.zfs/snapshot/snap-1", Protocol::Smb);
        r.shadow_copies = true;
        let err = m.create(&r).unwrap_err();
        assert!(err.contains(".zfs"), "{err}");
    }

    #[test]
    fn toggling_shadow_copies_changes_the_revision() {
        let mut m = Manager::new("", "/var/mnt");
        let r = req("media", "/var/mnt/tank/media", Protocol::Smb);
        let created = m.create(&r).unwrap();
        let before = m.render_config_bundle().revision;
        let update = crate::shares::UpdateShareRequest {
            shadow_copies: Some(true),
            ..Default::default()
        };
        m.update(&created.name, &update).unwrap();
        assert_ne!(before, m.render_config_bundle().revision);
    }
}
