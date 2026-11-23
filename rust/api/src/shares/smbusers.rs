//! SMB account synchronisation (port of `shares/smbusers.go`).
//!
//! Naslos keeps one identity (OpenLDAP) and mirrors it into Samba's passdb. The
//! NT hash is captured at password-change time and recorded here; the accounts
//! render into an `smbpasswd`-format file the agent writes next to smb.conf.

use crate::identity::normalize_uid;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::Path;

use super::backup_corrupt_store;

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
    /// Whether the persisted mirror was read successfully. A missing or empty
    /// file is a valid empty mirror (fresh install); only an existing
    /// unparsable file fails closed, so it is never pushed as "no accounts"
    /// over a working node mirror.
    loaded: bool,
    /// Why the store failed to load, with recovery guidance.
    load_error: Option<String>,
}

impl SambaUserStore {
    pub fn new(path: &str) -> Self {
        let mut s = Self {
            path: path.to_string(),
            users: HashMap::new(),
            loaded: false,
            load_error: None,
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
        self.loaded = false;
        self.load_error = None;
        if self.path.is_empty() {
            self.loaded = true;
            return;
        }
        let data = match std::fs::read(&self.path) {
            Ok(data) => data,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Fresh install: the mirror is created on the first upsert.
                self.loaded = true;
                return;
            }
            Err(e) => {
                self.load_error = Some(format!(
                    "SMB account store {} could not be read: {e}; refusing to overwrite it",
                    self.path
                ));
                return;
            }
        };
        if data.iter().all(|b| b.is_ascii_whitespace()) {
            // Writes are durable (temp `fsync` + rename + directory `fsync`),
            // so an existing empty file is not a crash artefact: treating it
            // as "no accounts" would mirror an empty account database over a
            // working node. Fail closed and preserve it instead.
            let backup = match backup_corrupt_store(&self.path, &data) {
                Ok(path) => format!(" (backup saved to {})", path.display()),
                Err(backup_err) => format!(" ({backup_err})"),
            };
            self.load_error = Some(format!(
                "SMB account store {} is empty; repair it or replace it with a valid store before changing accounts{backup}",
                self.path
            ));
            return;
        }
        let list = match serde_json::from_slice::<Vec<SambaUser>>(&data) {
            Ok(list) => list,
            Err(e) => {
                let backup = match backup_corrupt_store(&self.path, &data) {
                    Ok(path) => format!(" (backup saved to {})", path.display()),
                    Err(backup_err) => format!(" ({backup_err})"),
                };
                self.load_error = Some(format!(
                    "SMB account store {} is corrupt: {e}{backup}; repair it or replace it with a valid store before changing accounts",
                    self.path
                ));
                return;
            }
        };
        self.loaded = true;
        let mut changed = false;
        for mut u in list {
            let uid = normalize_uid(&u.uid);
            if uid.is_empty() {
                changed = true;
                continue;
            }
            if u.uid != uid {
                u.uid = uid.clone();
                changed = true;
            }
            // A legacy mirror could hold the same POSIX account under two
            // casings (the uid typed at creation and the canonical LDAP uid).
            // Keep the newest password: a stale duplicate with the same
            // uidNumber makes `pdbedit -i` skip the real hash. Disablement is
            // not timestamped, so it wins: a duplicate marked disabled must
            // never be resurrected by the merge.
            let existing = self.users.get(&uid).cloned();
            match existing {
                Some(existing) => {
                    changed = true;
                    let mut keep = if u.password_set_at > existing.password_set_at {
                        u
                    } else {
                        existing.clone()
                    };
                    keep.uid = uid.clone();
                    keep.enabled = existing.enabled && keep.enabled;
                    self.users.insert(uid, keep);
                }
                None => {
                    self.users.insert(uid, u);
                }
            }
        }
        if changed {
            if let Err(e) = self.save() {
                tracing::warn!("could not compact the SMB account mirror: {e}");
            }
        }
    }

    fn save(&self) -> Result<(), String> {
        self.write_snapshot(&self.users)
    }

    /// Persist a candidate map without touching in-memory state (R-049b).
    fn write_snapshot(&self, users: &HashMap<String, SambaUser>) -> Result<(), String> {
        self.require_loaded()?;
        if self.path.is_empty() {
            return Ok(());
        }
        let mut list: Vec<SambaUser> = users.values().cloned().collect();
        list.sort_by(|a, b| a.uid.cmp(&b.uid));
        let mut data = serde_json::to_vec_pretty(&list)
            .map_err(|e| format!("marshaling SMB accounts: {e}"))?;
        data.push(b'\n');

        let path = Path::new(&self.path);
        // A single-component relative store path has an empty parent; the
        // directory to fsync (and to place the temp file in) is the working
        // directory itself, never "".
        let dir = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        if dir != Path::new(".") {
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
            f.sync_all()
                .map_err(|e| format!("syncing SMB accounts: {e}"))?;
        }
        let tmp_path = tmp.into_temp_path();
        std::fs::set_permissions(
            &tmp_path,
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
        .map_err(|e| format!("setting SMB account file mode: {e}"))?;
        std::fs::rename(&tmp_path, &self.path)
            .map_err(|e| format!("replacing SMB accounts: {e}"))?;
        // Durability: the rename itself must survive a crash, or the store
        // can come back as a missing/empty mirror.
        std::fs::File::open(dir)
            .and_then(|dir| dir.sync_all())
            .map_err(|e| format!("syncing the SMB account directory: {e}"))?;
        Ok(())
    }

    /// The accounts ordered by uid.
    pub fn list(&self) -> Vec<SambaUser> {
        let mut out: Vec<SambaUser> = self.users.values().cloned().collect();
        out.sort_by(|a, b| a.uid.cmp(&b.uid));
        out
    }

    pub fn get(&self, uid: &str) -> Option<&SambaUser> {
        self.users.get(&normalize_uid(uid))
    }

    pub fn count(&self) -> usize {
        self.users.len()
    }

    /// Whether the persisted mirror was read successfully.
    pub fn loaded(&self) -> bool {
        self.loaded
    }

    /// Fail closed unless the persisted mirror was read successfully.
    pub fn require_loaded(&self) -> Result<(), String> {
        if self.loaded {
            Ok(())
        } else {
            Err(self
                .load_error
                .clone()
                .unwrap_or_else(|| format!("SMB account store {} is unavailable", self.path)))
        }
    }

    /// Record the account's NT hash and POSIX identity.
    pub fn upsert(&mut self, id: &PosixIdentity, nt_hash: &str) -> Result<(), String> {
        let uid = normalize_uid(&id.uid);
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

        let entry = if let Some(existing) = self.users.get(&uid) {
            let mut updated = existing.clone();
            updated.nt_hash = nt_hash;
            updated.password_set_at = Some(Utc::now());
            if id.uid_num > 0 {
                updated.uid_number = id.uid_num;
            }
            if id.gid_num > 0 {
                updated.gid_number = id.gid_num;
            }
            updated.gecos = gecos;
            updated
        } else {
            SambaUser {
                uid: uid.clone(),
                uid_number: id.uid_num,
                gid_number: id.gid_num,
                gecos,
                nt_hash,
                password_set_at: Some(Utc::now()),
                enabled: true,
            }
        };
        let mut next = self.users.clone();
        next.insert(uid, entry);
        self.write_snapshot(&next)?;
        self.users = next;
        Ok(())
    }

    /// Mirror an LDAP enable/disable into the SMB account.
    pub fn set_enabled(&mut self, uid: &str, enabled: bool) -> Result<(), String> {
        let uid = normalize_uid(uid);
        if !self.users.contains_key(&uid) {
            return Ok(());
        }
        let mut next = self.users.clone();
        if let Some(u) = next.get_mut(&uid) {
            u.enabled = enabled;
        }
        self.write_snapshot(&next)?;
        self.users = next;
        Ok(())
    }

    /// Drop an account.
    pub fn remove(&mut self, uid: &str) -> Result<(), String> {
        let uid = normalize_uid(uid);
        if !self.users.contains_key(&uid) {
            return Ok(());
        }
        let mut next = self.users.clone();
        next.remove(&uid);
        self.write_snapshot(&next)?;
        self.users = next;
        Ok(())
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
    ///
    /// Keyed by group **name**, not gid: the primary-gid entry for
    /// `naslos_users` (10000) and the LDAP group of the same name used to render
    /// as two lines with the same name and different gids.
    pub fn render_group(&self, extra_groups: &[NssGroup]) -> String {
        struct Entry {
            gid: i32,
            members: std::collections::BTreeSet<String>,
        }
        let mut groups: std::collections::BTreeMap<String, Entry> =
            std::collections::BTreeMap::new();

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
                .entry(name)
                .or_insert_with(|| Entry {
                    gid,
                    members: std::collections::BTreeSet::new(),
                })
                .members
                .insert(u.uid);
        }

        for g in extra_groups {
            let name = normalize_uid(&g.name);
            if name.is_empty() {
                continue;
            }
            let gid = if g.gid <= 0 { group_gid(&name) } else { g.gid };
            let entry = groups.entry(name).or_insert_with(|| Entry {
                gid,
                members: std::collections::BTreeSet::new(),
            });
            for m in &g.members {
                let member = normalize_uid(m);
                if !member.is_empty() {
                    entry.members.insert(member);
                }
            }
        }

        let mut sb = String::from("# Generated by Naslos - do not edit.\n");
        for (name, e) in &groups {
            let members: Vec<&str> = e.members.iter().map(|s| s.as_str()).collect();
            sb.push_str(&format!("{}:x:{}:{}\n", name, e.gid, members.join(",")));
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
    fn load_merges_case_variants_and_keeps_the_newest_hash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("smbusers.json");
        let old = SambaUser {
            uid: "Elisa".into(),
            uid_number: 10003,
            gid_number: 10000,
            gecos: "Elisa".into(),
            nt_hash: "783A3D4FCDFBAFC2B2AF6FA38792FB99".into(),
            password_set_at: Some(
                DateTime::parse_from_rfc3339("2026-11-06T22:57:37Z")
                    .unwrap()
                    .with_timezone(&Utc),
            ),
            enabled: true,
        };
        let new = SambaUser {
            uid: "elisa".into(),
            uid_number: 10003,
            gid_number: 10000,
            gecos: "elisa".into(),
            nt_hash: "29401A4C5D1899F6B43359AEB6FAD7BF".into(),
            password_set_at: Some(
                DateTime::parse_from_rfc3339("2026-11-08T16:21:13Z")
                    .unwrap()
                    .with_timezone(&Utc),
            ),
            enabled: true,
        };
        std::fs::write(&path, serde_json::to_vec(&vec![old, new.clone()]).unwrap()).unwrap();

        let store = SambaUserStore::new(path.to_str().unwrap());
        let list = store.list();
        assert_eq!(list.len(), 1, "{list:?}");
        assert_eq!(list[0].uid, "elisa");
        assert_eq!(list[0].nt_hash, new.nt_hash);
        assert!(store.render_smbpasswd().contains("elisa:10003:"));
        assert!(!store.render_smbpasswd().contains("Elisa:"));

        // The compacted file is persisted, so a restart cannot resurrect it.
        let persisted = std::fs::read_to_string(&path).unwrap();
        assert!(!persisted.contains("\"Elisa\""), "{persisted}");
    }

    #[test]
    fn load_merge_keeps_a_disabled_duplicate_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("smbusers.json");
        let disabled_old = SambaUser {
            uid: "Elisa".into(),
            uid_number: 10003,
            gid_number: 10000,
            gecos: "Elisa".into(),
            nt_hash: "783A3D4FCDFBAFC2B2AF6FA38792FB99".into(),
            password_set_at: Some(
                DateTime::parse_from_rfc3339("2026-11-06T22:57:37Z")
                    .unwrap()
                    .with_timezone(&Utc),
            ),
            enabled: false,
        };
        let enabled_new = SambaUser {
            uid: "elisa".into(),
            uid_number: 10003,
            gid_number: 10000,
            gecos: "elisa".into(),
            nt_hash: "29401A4C5D1899F6B43359AEB6FAD7BF".into(),
            password_set_at: Some(
                DateTime::parse_from_rfc3339("2026-11-08T16:21:13Z")
                    .unwrap()
                    .with_timezone(&Utc),
            ),
            enabled: true,
        };
        std::fs::write(
            &path,
            serde_json::to_vec(&vec![disabled_old, enabled_new.clone()]).unwrap(),
        )
        .unwrap();

        let store = SambaUserStore::new(path.to_str().unwrap());
        let merged = store.get("elisa").unwrap();
        assert_eq!(merged.nt_hash, enabled_new.nt_hash);
        assert!(!merged.enabled, "disablement must win the merge");
        assert!(store.render_smbpasswd().contains("[DU         ]"));
    }

    #[test]
    fn load_reports_an_unreadable_mirror() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.json");
        assert!(
            SambaUserStore::new(missing.to_str().unwrap()).loaded(),
            "a missing mirror is a valid empty mirror on a fresh install"
        );

        let corrupt = dir.path().join("corrupt.json");
        std::fs::write(&corrupt, "{not json").unwrap();
        assert!(!SambaUserStore::new(corrupt.to_str().unwrap()).loaded());

        let empty = dir.path().join("empty.json");
        std::fs::write(&empty, "[]").unwrap();
        assert!(SambaUserStore::new(empty.to_str().unwrap()).loaded());

        // With durable writes in place an existing empty/whitespace file is
        // not a crash artefact: it must fail closed instead of mirroring an
        // empty account database over a working node.
        for (name, contents) in [("zero.json", ""), ("blank.json", "  \n\t\n")] {
            let path = dir.path().join(name);
            std::fs::write(&path, contents).unwrap();
            let store = SambaUserStore::new(path.to_str().unwrap());
            assert!(!store.loaded(), "{name} must be corrupt");
            assert!(store.require_loaded().unwrap_err().contains("empty"));
        }

        // An empty path is an in-memory test store, not a failed load.
        assert!(SambaUserStore::new("").loaded());
    }

    #[test]
    fn missing_mirror_allows_the_first_account_and_is_created() {
        let dir = tempfile::tempdir().unwrap();
        let store_path = dir.path().join("smbusers.json");
        let mut store = SambaUserStore::new(store_path.to_str().unwrap());
        assert!(store.loaded());

        store
            .upsert(
                &PosixIdentity {
                    uid: "elisa".into(),
                    uid_num: 10003,
                    gid_num: 10000,
                    ..Default::default()
                },
                "8846F7EAEE8FB117AD06BDD830B7586C",
            )
            .unwrap();
        assert!(store_path.exists(), "the first save must create the mirror");

        let reloaded = SambaUserStore::new(store_path.to_str().unwrap());
        assert!(reloaded.loaded());
        assert_eq!(reloaded.count(), 1);
        assert!(reloaded.get("elisa").is_some());
    }

    #[test]
    fn mutators_use_the_canonical_uid() {
        let mut store = SambaUserStore::new("");
        store
            .upsert(
                &PosixIdentity {
                    uid: "Elisa".into(),
                    uid_num: 10003,
                    gid_num: 10000,
                    ..Default::default()
                },
                "8846F7EAEE8FB117AD06BDD830B7586C",
            )
            .unwrap();
        assert!(store.get("elisa").is_some());
        assert_eq!(store.list()[0].uid, "elisa");
        store.set_enabled("ELISA", false).unwrap();
        assert!(!store.get("Elisa").unwrap().enabled);
        store.remove("eLiSa").unwrap();
        assert_eq!(store.count(), 0);
    }

    #[test]
    fn ldap_group_merges_with_the_primary_gid_group() {
        let mut store = SambaUserStore::new("");
        for uid in ["florentin", "elisa"] {
            store
                .upsert(
                    &PosixIdentity {
                        uid: uid.into(),
                        uid_num: 10002,
                        gid_num: 10000,
                        ..Default::default()
                    },
                    "8846F7EAEE8FB117AD06BDD830B7586C",
                )
                .unwrap();
        }
        let out = store.render_group(&[NssGroup {
            name: "naslos_users".into(),
            gid: crate::shares::group_gid("naslos_users"),
            members: vec!["elisa".into()],
        }]);
        assert_eq!(out.matches("naslos_users:").count(), 1, "{out}");
        assert!(
            out.contains("naslos_users:x:10000:elisa,florentin\n"),
            "{out}"
        );
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

    #[test]
    fn corrupt_account_store_is_backed_up_and_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let store_path = dir.path().join("smbusers.json");
        std::fs::write(&store_path, "{not json").unwrap();

        let mut store = SambaUserStore::new(store_path.to_str().unwrap());
        assert!(!store.loaded());
        let err = store.require_loaded().unwrap_err();
        assert!(err.contains("corrupt"), "{err}");
        assert!(err.contains(".corrupt-"), "{err}");

        let backups: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".corrupt-"))
            .collect();
        assert_eq!(backups.len(), 1, "one corrupt backup expected");

        let err = store
            .upsert(
                &PosixIdentity {
                    uid: "elisa".into(),
                    uid_num: 10003,
                    gid_num: 10000,
                    ..Default::default()
                },
                "8846F7EAEE8FB117AD06BDD830B7586C",
            )
            .unwrap_err();
        assert!(err.contains("corrupt"), "{err}");
        assert_eq!(std::fs::read_to_string(&store_path).unwrap(), "{not json");
        assert_eq!(store.count(), 0);
    }

    #[test]
    fn failed_save_leaves_account_memory_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let store_path = dir.path().join("smbusers.json");
        std::fs::write(&store_path, "[]").unwrap();
        let blocker = dir.path().join("blocked");
        std::fs::write(&blocker, "not a directory").unwrap();

        let mut store = SambaUserStore::new(store_path.to_str().unwrap());
        assert!(store.loaded());
        store.path = blocker.join("smbusers.json").to_string_lossy().into_owned();

        assert!(store
            .upsert(
                &PosixIdentity {
                    uid: "elisa".into(),
                    uid_num: 10003,
                    gid_num: 10000,
                    ..Default::default()
                },
                "8846F7EAEE8FB117AD06BDD830B7586C",
            )
            .is_err());
        assert_eq!(store.count(), 0);
        assert_eq!(std::fs::read_to_string(&store_path).unwrap(), "[]");
    }
}
