//! Person and group operations (port of `identity/persons*.go` and
//! `identity/groups.go`).

use super::*;

/// An unguessable placeholder password for a brand-new entry (NAS-010). The
/// real password replaces it immediately.
fn random_placeholder() -> String {
    let mut buf = [0u8; 24];
    if getrandom::fill(&mut buf).is_err() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        return format!("unusable-{nanos}");
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

impl Client {
    /// Create a person in LDAP.
    pub async fn create_person(
        &self,
        uid: &str,
        display_name: &str,
        email: &str,
        first_name: &str,
        last_name: &str,
    ) -> Result<Person, String> {
        let uid = normalize_uid(uid);
        validate_identity_name("username", &uid)?;
        let dn = self.person_dn(&uid);

        // Hold the allocator across existence-check → uidNumber search → add.
        let _alloc = self.user_alloc.lock().await;
        if self.get_person(&uid).await.is_ok() {
            return Err(format!("person {uid:?} already exists"));
        }
        let uid_number = self.next_uid_number().await?;

        let cn = person_cn(display_name, first_name, last_name, &uid);
        let sn = if last_name.trim().is_empty() {
            cn.clone()
        } else {
            last_name.trim().to_string()
        };

        let mut attrs: Vec<(String, HashSet<String>)> = vec![
            (
                "objectClass".into(),
                ["inetOrgPerson", "posixAccount", "shadowAccount"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            ),
            ("uid".into(), set1(&uid)),
            ("cn".into(), set1(&cn)),
            ("sn".into(), set1(&sn)),
        ];
        let given = first_name.trim();
        if !given.is_empty() {
            attrs.push(("givenName".into(), set1(given)));
        }
        attrs.push(("displayName".into(), set1(&cn)));
        let mail = email.trim();
        if !mail.is_empty() {
            attrs.push(("mail".into(), set1(mail)));
        }
        attrs.push(("uidNumber".into(), set1(&uid_number.to_string())));
        attrs.push(("gidNumber".into(), set1("10000")));
        attrs.push(("homeDirectory".into(), set1(&format!("/home/{uid}"))));
        attrs.push(("loginShell".into(), set1("/bin/bash")));
        attrs.push(("userPassword".into(), set1(&random_placeholder())));
        attrs.push(("shadowExpire".into(), set1("-1")));

        self.run(Op::Add { dn, attrs })
            .await
            .map_err(|e| format!("creating person: {e}"))?;
        self.get_person(&uid).await
    }

    /// Allocate the next POSIX uid. Callers must hold `user_alloc`.
    async fn next_uid_number(&self) -> Result<i32, String> {
        const FLOOR: i32 = 10001;
        let entries = self
            .run(Op::Search {
                base: self.people_base(),
                filter: "(uidNumber=*)".into(),
                attrs: vec!["uidNumber".into()],
            })
            .await
            .map_err(|e| format!("listing uidNumber values: {e}"))?
            .entries()?;
        let highest = entries
            .iter()
            .filter_map(|e| attr(e, "uidNumber").parse::<i32>().ok())
            .max()
            .unwrap_or(FLOOR - 1);
        highest
            .checked_add(1)
            .filter(|n| *n >= FLOOR && *n < i32::MAX)
            .ok_or_else(|| "no uidNumber left to allocate".to_string())
    }

    /// Retrieve a person by uid.
    pub async fn get_person(&self, uid: &str) -> Result<Person, String> {
        let uid = normalize_uid(uid);
        validate_identity_name("username", &uid)?;
        let entries = self
            .run(Op::Search {
                base: self.people_base(),
                filter: format!("(uid={})", escape_filter(&uid)),
                attrs: vec![
                    "dn".into(),
                    "uid".into(),
                    "displayName".into(),
                    "mail".into(),
                    "givenName".into(),
                    "sn".into(),
                    "memberOf".into(),
                    "shadowExpire".into(),
                ],
            })
            .await
            .map_err(|e| format!("searching for person: {e}"))?
            .entries()?;
        let entry = entries
            .into_iter()
            .next()
            .ok_or_else(|| format!("person {uid:?} not found"))?;
        Ok(person_from_entry(&entry))
    }

    /// List all people.
    pub async fn list_people(&self) -> Result<Vec<Person>, String> {
        let entries = self
            .run(Op::Search {
                base: self.people_base(),
                filter: "(objectClass=inetOrgPerson)".into(),
                attrs: vec![
                    "uid".into(),
                    "displayName".into(),
                    "mail".into(),
                    "givenName".into(),
                    "sn".into(),
                    "memberOf".into(),
                    "shadowExpire".into(),
                ],
            })
            .await
            .map_err(|e| format!("listing people: {e}"))?
            .entries()?;
        Ok(entries.iter().map(person_from_entry).collect())
    }

    /// Update a person's attributes (only non-empty fields are replaced).
    pub async fn update_person(
        &self,
        uid: &str,
        display_name: &str,
        email: &str,
        first_name: &str,
        last_name: &str,
    ) -> Result<(), String> {
        let uid = normalize_uid(uid);
        validate_identity_name("username", &uid)?;
        let dn = self.person_dn(&uid);

        let mut mods = Vec::new();
        if !display_name.is_empty() {
            mods.push(Mod::Replace("displayName".into(), set1(display_name)));
            mods.push(Mod::Replace("cn".into(), set1(display_name)));
        }
        if !email.is_empty() {
            mods.push(Mod::Replace("mail".into(), set1(email)));
        }
        if !first_name.is_empty() {
            mods.push(Mod::Replace("givenName".into(), set1(first_name)));
        }
        if !last_name.is_empty() {
            mods.push(Mod::Replace("sn".into(), set1(last_name)));
        }
        self.run(Op::Modify { dn, mods }).await?;
        Ok(())
    }

    /// Delete a person.
    pub async fn delete_person(&self, uid: &str) -> Result<(), String> {
        let uid = normalize_uid(uid);
        validate_identity_name("username", &uid)?;
        self.run(Op::Delete {
            dn: self.person_dn(&uid),
        })
        .await?;
        Ok(())
    }

    /// Set a password via RFC 3062 and return the NT hash for SMB sync.
    pub async fn set_password(&self, uid: &str, password: &str) -> Result<String, String> {
        let uid = normalize_uid(uid);
        validate_identity_name("username", &uid)?;
        let dn = self.person_dn(&uid);
        self.run(Op::PasswordModify {
            dn,
            password: password.to_string(),
        })
        .await
        .map_err(|e| format!("setting password: {e}"))?;
        Ok(compute_nt_hash(password))
    }

    /// POSIX uidNumber/gidNumber of a person.
    pub async fn get_posix_ids(&self, uid: &str) -> Result<(i32, i32), String> {
        let uid = normalize_uid(uid);
        validate_identity_name("username", &uid)?;
        let entries = self
            .run(Op::Search {
                base: self.people_base(),
                filter: format!("(uid={})", escape_filter(&uid)),
                attrs: vec!["uidNumber".into(), "gidNumber".into()],
            })
            .await
            .map_err(|e| format!("searching for POSIX ids: {e}"))?
            .entries()?;
        let entry = entries
            .into_iter()
            .next()
            .ok_or_else(|| format!("person {uid:?} not found"))?;
        let parse = |attr_name: &str| -> Result<i32, String> {
            let value = attr(&entry, attr_name);
            if value.is_empty() {
                return Err(format!("person {uid:?} has no {attr_name}"));
            }
            value
                .parse()
                .map_err(|e| format!("person {uid:?} has an invalid {attr_name} {value:?}: {e}"))
        };
        Ok((parse("uidNumber")?, parse("gidNumber")?))
    }

    /// Enable a person account.
    pub async fn enable_person(&self, uid: &str) -> Result<(), String> {
        self.set_shadow_expire(uid, "-1").await
    }

    /// Disable a person account.
    pub async fn disable_person(&self, uid: &str) -> Result<(), String> {
        self.set_shadow_expire(uid, "1").await
    }

    async fn set_shadow_expire(&self, uid: &str, value: &str) -> Result<(), String> {
        let uid = normalize_uid(uid);
        validate_identity_name("username", &uid)?;
        self.run(Op::Modify {
            dn: self.person_dn(&uid),
            mods: vec![Mod::Replace("shadowExpire".into(), set1(value))],
        })
        .await?;
        Ok(())
    }

    /// Create a group.
    pub async fn create_group(&self, cn: &str, description: &str) -> Result<Group, String> {
        let cn = normalize_group_name(cn)?;
        let dn = self.group_dn(&cn);
        let mut attrs: Vec<(String, HashSet<String>)> = vec![
            ("objectClass".into(), set1("groupOfNames")),
            ("cn".into(), set1(&cn)),
        ];
        if !description.is_empty() {
            attrs.push(("description".into(), set1(description)));
        }
        attrs.push(("member".into(), set1(&self.placeholder_member_dn())));
        self.run(Op::Add { dn, attrs })
            .await
            .map_err(|e| format!("creating group: {e}"))?;
        self.get_group(&cn).await
    }

    /// Retrieve a group by cn.
    pub async fn get_group(&self, cn: &str) -> Result<Group, String> {
        let cn = normalize_group_name(cn)?;
        let entries = self
            .run(Op::Search {
                base: self.groups_base(),
                filter: format!("(cn={})", escape_filter(&cn)),
                attrs: vec![
                    "dn".into(),
                    "cn".into(),
                    "description".into(),
                    "member".into(),
                ],
            })
            .await
            .map_err(|e| format!("searching for group: {e}"))?
            .entries()?;
        let entry = entries
            .into_iter()
            .next()
            .ok_or_else(|| format!("group {cn:?} not found"))?;
        Ok(self.group_from_entry(&entry))
    }

    /// List all groups.
    pub async fn list_groups(&self) -> Result<Vec<Group>, String> {
        let entries = self
            .run(Op::Search {
                base: self.groups_base(),
                filter: "(objectClass=groupOfNames)".into(),
                attrs: vec!["cn".into(), "description".into(), "member".into()],
            })
            .await
            .map_err(|e| format!("listing groups: {e}"))?
            .entries()?;
        Ok(entries.iter().map(|e| self.group_from_entry(e)).collect())
    }

    /// Add a person to a group.
    pub async fn add_member(&self, group_cn: &str, person_uid: &str) -> Result<(), String> {
        let group_cn = normalize_group_name(group_cn)?;
        let uid = normalize_uid(person_uid);
        validate_identity_name("username", &uid)?;
        self.run(Op::Modify {
            dn: self.group_dn(&group_cn),
            mods: vec![Mod::Add("member".into(), set1(&self.person_dn(&uid)))],
        })
        .await?;
        Ok(())
    }

    /// Remove a person from a group.
    pub async fn remove_member(&self, group_cn: &str, person_uid: &str) -> Result<(), String> {
        let group_cn = normalize_group_name(group_cn)?;
        let uid = normalize_uid(person_uid);
        validate_identity_name("username", &uid)?;
        self.run(Op::Modify {
            dn: self.group_dn(&group_cn),
            mods: vec![Mod::Delete("member".into(), set1(&self.person_dn(&uid)))],
        })
        .await?;
        Ok(())
    }

    /// Delete a group.
    pub async fn delete_group(&self, cn: &str) -> Result<(), String> {
        let cn = normalize_group_name(cn)?;
        self.run(Op::Delete {
            dn: self.group_dn(&cn),
        })
        .await?;
        Ok(())
    }

    /// All groups a person belongs to.
    pub async fn get_person_groups(&self, uid: &str) -> Result<Vec<String>, String> {
        let groups = self.list_groups().await?;
        let uid = normalize_uid(uid);
        validate_identity_name("username", &uid)?;
        let person_dn = self.person_dn(&uid);
        let mut memberships = Vec::new();
        for g in groups {
            if g.members.iter().any(|m| m == &person_dn) {
                memberships.push(g.cn);
            }
        }
        Ok(memberships)
    }

    fn group_from_entry(&self, entry: &SearchEntry) -> Group {
        let members = attrs(entry, "member")
            .into_iter()
            .filter(|m| !m.starts_with("cn=empty-members,"))
            .collect();
        Group {
            dn: entry.dn.clone(),
            cn: attr(entry, "cn"),
            description: attr(entry, "description"),
            members,
        }
    }
}

fn person_from_entry(entry: &SearchEntry) -> Person {
    Person {
        dn: entry.dn.clone(),
        uid: attr(entry, "uid"),
        display_name: attr(entry, "displayName"),
        email: attr(entry, "mail"),
        first_name: attr(entry, "givenName"),
        last_name: attr(entry, "sn"),
        groups: short_names(&attrs(entry, "memberOf")),
        enabled: is_person_enabled(&attr(entry, "shadowExpire")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A `SearchEntry` carrying only string attributes (the same shape
    /// `SearchEntry::construct` produces for an LDAP search result).
    fn make_entry(dn: &str, attrs: Vec<(&str, Vec<&str>)>) -> SearchEntry {
        SearchEntry {
            dn: dn.to_string(),
            attrs: attrs
                .into_iter()
                .map(|(name, values)| {
                    (
                        name.to_string(),
                        values.into_iter().map(str::to_string).collect(),
                    )
                })
                .collect(),
            bin_attrs: HashMap::new(),
        }
    }

    #[test]
    fn person_from_entry_maps_every_field() {
        let entry = make_entry(
            "uid=alice,ou=people,dc=example,dc=test",
            vec![
                ("uid", vec!["alice"]),
                // Deliberately different case: the `attr` helper is
                // case-insensitive for LDAP attribute names.
                ("DisplayName", vec!["Alice Example"]),
                ("mail", vec!["alice@example.test"]),
                ("givenName", vec!["Alice"]),
                ("sn", vec!["Example"]),
                (
                    "memberOf",
                    vec![
                        "cn=naslos_admins,ou=groups,dc=example,dc=test",
                        "CN=Media,ou=groups,dc=example,dc=test",
                    ],
                ),
                ("shadowExpire", vec!["-1"]),
            ],
        );
        let person = person_from_entry(&entry);
        assert_eq!(
            person.dn, "uid=alice,ou=people,dc=example,dc=test",
            "the entry DN must be copied verbatim"
        );
        assert_eq!(person.uid, "alice", "uid must map from the uid attribute");
        assert_eq!(
            person.display_name, "Alice Example",
            "displayName must map regardless of its attribute-name case"
        );
        assert_eq!(person.email, "alice@example.test", "email maps from mail");
        assert_eq!(person.first_name, "Alice", "firstName maps from givenName");
        assert_eq!(person.last_name, "Example", "lastName maps from sn");
        assert_eq!(
            person.groups,
            vec!["naslos_admins", "media"],
            "memberOf DNs must reduce to lowercase short names"
        );
        assert!(
            person.enabled,
            "shadowExpire=-1 means the account is enabled"
        );
    }

    #[test]
    fn person_from_entry_with_no_attributes_is_empty_and_enabled() {
        let entry = make_entry("uid=bob,ou=people,dc=example,dc=test", vec![]);
        let person = person_from_entry(&entry);
        assert_eq!(person.dn, "uid=bob,ou=people,dc=example,dc=test");
        assert!(
            person.uid.is_empty(),
            "a missing uid must be an empty string"
        );
        assert!(
            person.display_name.is_empty(),
            "a missing displayName must be an empty string"
        );
        assert!(
            person.email.is_empty(),
            "a missing mail must be an empty string"
        );
        assert!(
            person.first_name.is_empty(),
            "a missing givenName must be an empty string"
        );
        assert!(
            person.last_name.is_empty(),
            "a missing sn must be an empty string"
        );
        assert!(
            person.groups.is_empty(),
            "a missing memberOf must produce no groups"
        );
        assert!(
            person.enabled,
            "an absent shadowExpire means the account is enabled"
        );
    }

    #[test]
    fn person_from_entry_with_a_past_shadow_expire_is_disabled() {
        let entry = make_entry(
            "uid=carol,ou=people,dc=example,dc=test",
            vec![("shadowExpire", vec!["1"])],
        );
        assert!(
            !person_from_entry(&entry).enabled,
            "shadowExpire=1 is a day in 1970, so the account must be disabled"
        );
    }
}
