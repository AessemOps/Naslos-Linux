//! SMB account synchronisation (port of `shares/smbusers.go`).
//!
//! Naslos keeps one identity (OpenLDAP) and mirrors it into Samba's passdb. The
//! NT hash is captured at password-change time and recorded here; the accounts
//! render into an `smbpasswd`-format file the agent writes next to smb.conf.

use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::Path;

/// The placeholder Samba uses for "no LAN Manager hash".
const NO_LM_HASH: &str = "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX";

/// An SMB-visible account mirrored from an LDAP user.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SambaUser {
    #[serde(rename = "uid")]
    pub uid: String,
    #[serde(rename = "uidNumber")]
    pub uid_number: i32,
    #[serde(rename = "gidNumber")]
    pub gid_number: i32,
    #[serde(default)]
    pub gecos: String,
    #[serde(rename = "ntHash")]
    pub nt_hash: String,
    #[serde(rename = "passwordSetAt")]
    pub password_set_at: Option<DateTime<Utc>>,
    pub enabled: bool,
}

/// POSIX account attributes mirrored to the node.
#[derive(Debug, Clone, Default)]
pub struct PosixIdentity {
    pub uid: String,
    pub uid_num: i32,
    pub gid_num: i32,
    pub gecos: String,
    pub home_dir: String,
    pub shell: String,
}

/// An LDAP group mirrored into the extrausers group file.
#[derive(Debug, Clone, Default)]
pub struct NssGroup {
    pub name: String,
    pub gid: i32,
    pub members: Vec<String>,
}

/// Persists the SMB account mirror.
pub struct SambaUserStore {
    path: String,
    users: HashMap<String, SambaUser>,
}

impl SambaUserStore {
    pub fn new(path: &str) -> Self {
        let mut s = Self {
            path: path.to_string(),
            users: HashMap::new(),
        };
        s.load();
        s
    }

    pub fn from_env() -> Self {
        let path = std::env::var("SMB_USERS_CONFIG")
            .unwrap_or_else(|_| "/var/lib/naslos/smbusers.json".to_string());
        Self::new(&path)
    }

    fn load(&mut self) {
        if self.path.is_empty() {
            return;
        }
        let Ok(data) = std::fs::read(&self.path) else {
            return;
        };
        let Ok(list) = serde_json::from_slice::<Vec<SambaUser>>(&data) else {
            return;
        };
        for u in list {
            if !u.uid.is_empty() {
                self.users.insert(u.uid.clone(), u);
            }
        }
    }

    fn save(&self) -> Result<(), String> {
        if self.path.is_empty() {
            return Ok(());
        }
        let mut data = serde_json::to_vec_pretty(&self.list())
            .map_err(|e| format!("marshaling SMB accounts: {e}"))?;
        data.push(b'\n');

        let path = Path::new(&self.path);
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        if !dir.as_os_str().is_empty() && dir != Path::new(".") {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("creating SMB account directory: {e}"))?;
        }
        let tmp = tempfile::Builder::new()
            .prefix(".smbusers-")
            .suffix(".tmp")
            .tempfile_in(dir)
            .map_err(|e| format!("creating temp SMB account file: {e}"))?;
        {
            use std::io::Write;
            let mut f = tmp
                .as_file()
                .try_clone()
                .map_err(|e| format!("writing SMB accounts: {e}"))?;
            f.write_all(&data)
                .map_err(|e| format!("writing SMB accounts: {e}"))?;
        }
        let tmp_path = tmp.into_temp_path();
        std::fs::set_permissions(
            &tmp_path,
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
        .map_err(|e| format!("setting SMB account file mode: {e}"))?;
        std::fs::rename(&tmp_path, &self.path)
            .map_err(|e| format!("replacing SMB accounts: {e}"))?;
        Ok(())
    }

    /// The accounts ordered by uid.
    pub fn list(&self) -> Vec<SambaUser> {
        let mut out: Vec<SambaUser> = self.users.values().cloned().collect();
        out.sort_by(|a, b| a.uid.cmp(&b.uid));
        out
    }

    pub fn get(&self, uid: &str) -> Option<&SambaUser> {
        self.users.get(uid)
    }

    pub fn count(&self) -> usize {
        self.users.len()
    }

    /// Record the account's NT hash and POSIX identity.
    pub fn upsert(&mut self, id: &PosixIdentity, nt_hash: &str) -> Result<(), String> {
        let uid = id.uid.trim().to_string();
        if uid.is_empty() {
            return Err("uid is required".to_string());
        }
        if uid.contains([':', '\n', '\r', '\0', '\t']) {
            return Err(format!(
                "uid {uid:?} must not contain ':' or control characters"
            ));
        }
        validate_no_control_chars("gecos", &id.gecos)?;
        if id.gecos.contains(':') {
            return Err("gecos must not contain ':'".to_string());
        }
        validate_no_control_chars("home directory", &id.home_dir)?;

        let nt_hash = normalize_nt_hash(nt_hash)?;

        let mut gecos = id.gecos.clone();
        if gecos.is_empty() {
            gecos = uid.clone();
        }

        if let Some(existing) = self.users.get_mut(&uid) {
            existing.nt_hash = nt_hash;
            existing.password_set_at = Some(Utc::now());
            if id.uid_num > 0 {
                existing.uid_number = id.uid_num;
            }
            if id.gid_num > 0 {
                existing.gid_number = id.gid_num;
            }
            existing.gecos = gecos;
        } else {
            self.users.insert(
                uid.clone(),
                SambaUser {
                    uid,
                    uid_number: id.uid_num,
                    gid_number: id.gid_num,
                    gecos,
                    nt_hash,
                    password_set_at: Some(Utc::now()),
                    enabled: true,
                },
            );
        }
        self.save()
    }

    /// Mirror an LDAP enable/disable into the SMB account.
    pub fn set_enabled(&mut self, uid: &str, enabled: bool) -> Result<(), String> {
        let Some(u) = self.users.get_mut(uid) else {
            return Ok(());
        };
        u.enabled = enabled;
        self.save()
    }

    /// Drop an account.
    pub fn remove(&mut self, uid: &str) -> Result<(), String> {
        if self.users.remove(uid).is_none() {
            return Ok(());
        }
        self.save()
    }

    /// `/var/lib/extrausers/passwd` entries.
    pub fn render_passwd(&self) -> String {
        let mut sb = String::from("# Generated by Naslos - do not edit.\n");
        for u in self.list() {
            if u.uid_number <= 0 {
                continue;
            }
            let gid = if u.gid_number <= 0 {
                10000
            } else {
                u.gid_number
            };
            let gecos = if u.gecos.is_empty() {
                u.uid.clone()
            } else {
                u.gecos
            };
            sb.push_str(&format!(
                "{}:x:{}:{}:{}:/home/{}:/bin/bash\n",
                u.uid, u.uid_number, gid, gecos, u.uid
            ));
        }
        sb
    }

    /// `/var/lib/extrausers/group` entries (accounts + LDAP groups).
    pub fn render_group(&self, extra_groups: &[NssGroup]) -> String {
        struct Entry {
            name: String,
            members: std::collections::BTreeSet<String>,
        }
        let mut groups: std::collections::BTreeMap<i32, Entry> = std::collections::BTreeMap::new();

        for u in self.list() {
            if u.uid_number <= 0 {
                continue;
            }
            let gid = if u.gid_number <= 0 {
                10000
            } else {
                u.gid_number
            };
            let name = if gid == 10000 {
                "naslos_users".to_string()
            } else {
                format!("group{gid}")
            };
            groups
                .entry(gid)
                .or_insert_with(|| Entry {
                    name,
                    members: std::collections::BTreeSet::new(),
                })
                .members
                .insert(u.uid);
        }

        for g in extra_groups {
            if g.name.is_empty() {
                continue;
            }
            let gid = if g.gid <= 0 {
                group_gid(&g.name)
            } else {
                g.gid
            };
            let entry = groups.entry(gid).or_insert_with(|| Entry {
                name: g.name.clone(),
                members: std::collections::BTreeSet::new(),
            });
            if entry.name == format!("group{gid}") {
                entry.name = g.name.clone();
            }
            for m in &g.members {
                if !m.is_empty() {
                    entry.members.insert(m.clone());
                }
            }
        }

        let mut sb = String::from("# Generated by Naslos - do not edit.\n");
        for (gid, e) in &groups {
            let members: Vec<&str> = e.members.iter().map(|s| s.as_str()).collect();
            sb.push_str(&format!("{}:x:{}:{}\n", e.name, gid, members.join(",")));
        }
        sb
    }

    /// `/var/lib/extrausers/shadow` entries.
    pub fn render_shadow(&self) -> String {
        let mut sb = String::from("# Generated by Naslos - do not edit.\n");
        for u in self.list() {
            if u.uid_number <= 0 {
                continue;
            }
            sb.push_str(&format!("{}:*:19000:0:99999:7:::\n", u.uid));
        }
        sb
    }

    /// The accounts in Samba's smbpasswd format.
    pub fn render_smbpasswd(&self) -> String {
        let mut sb = String::from("# Generated by Naslos - do not edit.\n");
        sb.push_str("# NT hashes are synced from LDAP password changes; imported with\n");
        sb.push_str("# `pdbedit -i smbpasswd:<this file>` by the naslos-samba container.\n");
        for u in self.list() {
            if u.nt_hash.is_empty() {
                continue;
            }
            let flags = if u.enabled {
                "U          "
            } else {
                "DU         "
            };
            let lct = u.password_set_at.map(|t| t.timestamp()).unwrap_or(0).max(0);
            sb.push_str(&format!(
                "{}:{}:{}:{}:[{}]:LCT-{:X}:\n",
                u.uid,
                u.uid_number,
                NO_LM_HASH,
                u.nt_hash.to_uppercase(),
                flags,
                lct
            ));
        }
        sb
    }
}

/// Validate and canonicalise an NT hash (exactly 32 hex digits).
pub fn normalize_nt_hash(nt_hash: &str) -> Result<String, String> {
    let trimmed = nt_hash.trim();
    if trimmed.len() != 32 {
        return Err(format!(
            "NT hash must be 32 hexadecimal characters, got {}",
            trimmed.len()
        ));
    }
    if let Some(c) = trimmed.chars().find(|c| !c.is_ascii_hexdigit()) {
        return Err(format!("NT hash must be hexadecimal, found {c:?}"));
    }
    Ok(trimmed.to_uppercase())
}

/// A stable gid for a group name (FNV-1a, range 20000..28000).
pub fn group_gid(name: &str) -> i32 {
    let mut hash: u32 = 0x811c9dc5;
    for b in name.bytes() {
        hash ^= b as u32;
        hash = hash.wrapping_mul(0x01000193);
    }
    20000 + (hash % 8000) as i32
}

fn validate_no_control_chars(kind: &str, value: &str) -> Result<(), String> {
    if value.contains(['\n', '\r', '\0', '\t']) {
        return Err(format!(
            "{kind} must not contain control characters (newline, tab or NUL)"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nt_hash_is_validated_and_uppercased() {
        assert_eq!(
            normalize_nt_hash("8846f7eaee8fb117ad06bdd830b7586c").unwrap(),
            "8846F7EAEE8FB117AD06BDD830B7586C"
        );
        assert!(normalize_nt_hash("short").is_err());
        assert!(normalize_nt_hash(&"z".repeat(32)).is_err());
    }

    #[test]
    fn smbpasswd_renders_flags_and_lct() {
        let mut store = SambaUserStore::new("");
        store
            .upsert(
                &PosixIdentity {
                    uid: "alice".into(),
                    uid_num: 10042,
                    gid_num: 10000,
                    gecos: "Alice".into(),
                    ..Default::default()
                },
                "8846F7EAEE8FB117AD06BDD830B7586C",
            )
            .unwrap();
        let out = store.render_smbpasswd();
        assert!(out.contains("alice:10042:XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX:8846F7EAEE8FB117AD06BDD830B7586C:[U          ]:LCT-"));
        store.set_enabled("alice", false).unwrap();
        assert!(store.render_smbpasswd().contains("[DU         ]"));
    }

    #[test]
    fn passwd_group_shadow_render() {
        let mut store = SambaUserStore::new("");
        store
            .upsert(
                &PosixIdentity {
                    uid: "bob".into(),
                    uid_num: 10050,
                    gid_num: 10000,
                    gecos: "Bob".into(),
                    ..Default::default()
                },
                "8846F7EAEE8FB117AD06BDD830B7586C",
            )
            .unwrap();
        assert!(store
            .render_passwd()
            .contains("bob:x:10050:10000:Bob:/home/bob:/bin/bash"));
        assert!(store.render_shadow().contains("bob:*:19000:0:99999:7:::"));
        let g = store.render_group(&[NssGroup {
            name: "naslos_admins".into(),
            gid: 0,
            members: vec!["bob".into()],
        }]);
        assert!(g.contains("naslos_users:x:10000:bob"));
        assert!(g.contains("naslos_admins:x:"));
    }

    #[test]
    fn group_gid_is_stable() {
        let g = group_gid("naslos_admins");
        assert!((20000..28000).contains(&g));
        assert_eq!(g, group_gid("naslos_admins"));
    }

    #[test]
    fn upsert_rejects_colon_in_uid() {
        let mut store = SambaUserStore::new("");
        assert!(store
            .upsert(
                &PosixIdentity {
                    uid: "a:b".into(),
                    ..Default::default()
                },
                "8846F7EAEE8FB117AD06BDD830B7586C"
            )
            .is_err());
    }
}
