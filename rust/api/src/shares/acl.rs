//! Fixed POSIX ACL presets and the service-account lockout guard (FR-ACL).
//!
//! This is the pure first slice of the ACL editor: a small allow-list of
//! presets maps to POSIX mode bits and base ACL entries, and a preset is
//! refused when it would leave the Naslos service account without the access it
//! needs. No ACL is read or written here, and no `setfacl` argument is built;
//! live ACL application, inheritance and the editor UI remain [OPEN] in
//! `docs/spec.md` and are described in `docs/acl.md`.

use std::fmt;
use std::str::FromStr;

/// The fixed ACL preset allow-list.
///
/// Presets are applied as a whole. Free-form ACL text and arbitrary ACL entries
/// are deliberately not accepted (FR-ACL-01).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AclPreset {
    /// Owner full access; group and others none (`0700`).
    Private,
    /// Owner full access; owning group read and traverse (`0750`).
    GroupRead,
    /// Owner and owning group full access (`0770`, the Naslos dataset access
    /// level; setgid is a separate dataset policy this module never applies).
    GroupWrite,
    /// Owner full access; group and others read and traverse (`0755`).
    PublicRead,
}

impl AclPreset {
    /// Every allowed preset, in the order the API/UI should present them.
    pub const ALL: [Self; 4] = [
        Self::Private,
        Self::GroupRead,
        Self::GroupWrite,
        Self::PublicRead,
    ];

    /// The stable wire name of the preset.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::GroupRead => "group-read",
            Self::GroupWrite => "group-write",
            Self::PublicRead => "public-read",
        }
    }

    /// The permissions the preset grants to one POSIX class.
    pub const fn permissions(self, class: PosixClass) -> PosixPermission {
        match self {
            Self::Private => match class {
                PosixClass::Owner => PosixPermission::READ_WRITE_EXECUTE,
                PosixClass::Group | PosixClass::Other => PosixPermission::NONE,
            },
            Self::GroupRead => match class {
                PosixClass::Owner => PosixPermission::READ_WRITE_EXECUTE,
                PosixClass::Group => PosixPermission::READ_EXECUTE,
                PosixClass::Other => PosixPermission::NONE,
            },
            Self::GroupWrite => match class {
                PosixClass::Owner | PosixClass::Group => PosixPermission::READ_WRITE_EXECUTE,
                PosixClass::Other => PosixPermission::NONE,
            },
            Self::PublicRead => match class {
                PosixClass::Owner => PosixPermission::READ_WRITE_EXECUTE,
                PosixClass::Group | PosixClass::Other => PosixPermission::READ_EXECUTE,
            },
        }
    }

    /// The classic UNIX mode bits (`0700`…`0755`) equivalent to the preset.
    ///
    /// The setgid bit is not part of an ACL preset: this module never sets it,
    /// and dataset creation does not apply it either.
    pub const fn mode_bits(self) -> u16 {
        ((self.permissions(PosixClass::Owner).bits() as u16) << 6)
            | ((self.permissions(PosixClass::Group).bits() as u16) << 3)
            | (self.permissions(PosixClass::Other).bits() as u16)
    }

    /// The base POSIX ACL entries for the preset.
    ///
    /// The mask equals the group permissions because the allow-list contains no
    /// named user or named group entries. Default (inheritable) entries are not
    /// produced by this slice.
    pub const fn entries(self) -> [PosixAclEntry; 4] {
        [
            PosixAclEntry {
                tag: PosixTag::UserObj,
                permissions: self.permissions(PosixClass::Owner),
            },
            PosixAclEntry {
                tag: PosixTag::GroupObj,
                permissions: self.permissions(PosixClass::Group),
            },
            PosixAclEntry {
                tag: PosixTag::Other,
                permissions: self.permissions(PosixClass::Other),
            },
            PosixAclEntry {
                tag: PosixTag::Mask,
                permissions: self.permissions(PosixClass::Group),
            },
        ]
    }

    /// The permissions the preset gives `service` on a dataset owned as given.
    ///
    /// POSIX class selection: a matching owner uid wins, then any matching
    /// group id (including supplementary groups), otherwise the other class.
    /// Root bypasses these checks and is not a meaningful subject here; pass
    /// the non-root serving identity.
    pub fn effective_permissions(
        self,
        owner: DatasetOwner,
        service: ServiceAccount<'_>,
    ) -> PosixPermission {
        let class = if service.uid == owner.uid {
            PosixClass::Owner
        } else if service.gids.contains(&owner.gid) {
            PosixClass::Group
        } else {
            PosixClass::Other
        };
        self.permissions(class)
    }

    /// Refuse a preset that would leave the service account without the access
    /// `required` on a dataset owned as given (FR-ACL-02).
    pub fn validate_for_service_account(
        self,
        owner: DatasetOwner,
        service: ServiceAccount<'_>,
        required: AccessRequirement,
    ) -> Result<(), AclError> {
        let needed = required.permissions();
        let effective = self.effective_permissions(owner, service);
        if effective.contains(needed) {
            Ok(())
        } else {
            Err(AclError::LocksOutServiceAccount {
                preset: self,
                needed,
                effective,
            })
        }
    }
}

impl fmt::Display for AclPreset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AclPreset {
    type Err = AclError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "private" => Ok(Self::Private),
            "group-read" => Ok(Self::GroupRead),
            "group-write" => Ok(Self::GroupWrite),
            "public-read" => Ok(Self::PublicRead),
            other => Err(AclError::UnknownPreset(other.to_string())),
        }
    }
}

/// The POSIX permission classes a preset can grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PosixClass {
    Owner,
    Group,
    Other,
}

/// A read/write/execute permission set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PosixPermission {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}

impl PosixPermission {
    pub const NONE: Self = Self {
        read: false,
        write: false,
        execute: false,
    };
    pub const READ: Self = Self {
        read: true,
        write: false,
        execute: false,
    };
    pub const READ_WRITE: Self = Self {
        read: true,
        write: true,
        execute: false,
    };
    pub const READ_EXECUTE: Self = Self {
        read: true,
        write: false,
        execute: true,
    };
    pub const READ_WRITE_EXECUTE: Self = Self {
        read: true,
        write: true,
        execute: true,
    };

    /// The permission as three bits (`r=4`, `w=2`, `x=1`).
    pub const fn bits(self) -> u8 {
        ((self.read as u8) << 2) | ((self.write as u8) << 1) | (self.execute as u8)
    }

    /// Whether every permission in `required` is present.
    pub const fn contains(self, required: Self) -> bool {
        self.bits() & required.bits() == required.bits()
    }
}

impl fmt::Display for PosixPermission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let read = if self.read { 'r' } else { '-' };
        let write = if self.write { 'w' } else { '-' };
        let execute = if self.execute { 'x' } else { '-' };
        write!(f, "{read}{write}{execute}")
    }
}

/// A base POSIX ACL tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PosixTag {
    UserObj,
    GroupObj,
    Other,
    Mask,
}

/// One typed POSIX ACL entry. No string command line is produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PosixAclEntry {
    pub tag: PosixTag,
    pub permissions: PosixPermission,
}

/// The dataset uid/gid the preset is evaluated against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DatasetOwner {
    pub uid: u32,
    pub gid: u32,
}

/// The identity that must retain access after a preset is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceAccount<'a> {
    pub uid: u32,
    /// Primary and supplementary group ids.
    pub gids: &'a [u32],
}

impl<'a> ServiceAccount<'a> {
    pub const fn new(uid: u32, gids: &'a [u32]) -> Self {
        Self { uid, gids }
    }
}

/// The minimum access a caller requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessRequirement {
    /// Read and traverse (`r-x`): enough to list and read.
    ReadOnly,
    /// Read, write and traverse (`rwx`): enough to serve and mutate.
    ReadWrite,
}

impl AccessRequirement {
    pub const fn permissions(self) -> PosixPermission {
        match self {
            Self::ReadOnly => PosixPermission::READ_EXECUTE,
            Self::ReadWrite => PosixPermission::READ_WRITE_EXECUTE,
        }
    }
}

/// A preset validation error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AclError {
    #[error(
        "unknown ACL preset {0:?}; allowed presets: private, group-read, group-write, public-read"
    )]
    UnknownPreset(String),
    #[error(
        "preset {preset} would remove the service account's required {needed} access (it would have {effective})"
    )]
    LocksOutServiceAccount {
        preset: AclPreset,
        needed: PosixPermission,
        effective: PosixPermission,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: DatasetOwner = DatasetOwner {
        uid: 10001,
        gid: 10000,
    };

    #[test]
    fn preset_allow_list_maps_to_posix_mode_and_entries() {
        assert_eq!(PosixPermission::NONE.bits(), 0o0);
        assert_eq!(PosixPermission::READ.bits(), 0o4);
        assert_eq!(PosixPermission::READ_WRITE.bits(), 0o6);
        assert_eq!(PosixPermission::READ_EXECUTE.bits(), 0o5);
        assert_eq!(PosixPermission::READ_WRITE_EXECUTE.bits(), 0o7);

        assert_eq!(AclPreset::Private.mode_bits(), 0o700);
        assert_eq!(AclPreset::GroupRead.mode_bits(), 0o750);
        assert_eq!(AclPreset::GroupWrite.mode_bits(), 0o770);
        assert_eq!(AclPreset::PublicRead.mode_bits(), 0o755);

        for preset in AclPreset::ALL {
            let entries = preset.entries();
            assert_eq!(
                entries[0],
                PosixAclEntry {
                    tag: PosixTag::UserObj,
                    permissions: preset.permissions(PosixClass::Owner),
                }
            );
            assert_eq!(
                entries[1],
                PosixAclEntry {
                    tag: PosixTag::GroupObj,
                    permissions: preset.permissions(PosixClass::Group),
                }
            );
            assert_eq!(
                entries[2],
                PosixAclEntry {
                    tag: PosixTag::Other,
                    permissions: preset.permissions(PosixClass::Other),
                }
            );
            assert_eq!(
                entries[3],
                PosixAclEntry {
                    tag: PosixTag::Mask,
                    permissions: preset.permissions(PosixClass::Group),
                }
            );
        }
    }

    #[test]
    fn preset_names_round_trip_and_unknown_names_are_refused() {
        for preset in AclPreset::ALL {
            assert_eq!(preset.as_str().parse::<AclPreset>(), Ok(preset));
        }
        assert_eq!(
            "public-write".parse::<AclPreset>(),
            Err(AclError::UnknownPreset("public-write".into()))
        );
        assert!("Public-Read".parse::<AclPreset>().is_err());
        assert!("".parse::<AclPreset>().is_err());
    }

    #[test]
    fn every_preset_keeps_owner_full_access() {
        for preset in AclPreset::ALL {
            assert_eq!(
                preset.permissions(PosixClass::Owner),
                PosixPermission::READ_WRITE_EXECUTE
            );
        }
    }

    #[test]
    fn no_preset_grants_world_write() {
        for preset in AclPreset::ALL {
            assert!(!preset.permissions(PosixClass::Other).write, "{preset}");
        }
    }

    #[test]
    fn effective_permissions_follow_owner_group_other() {
        let group_member = ServiceAccount::new(65532, &[10000, 65532]);
        assert_eq!(
            AclPreset::GroupWrite.effective_permissions(OWNER, group_member),
            PosixPermission::READ_WRITE_EXECUTE
        );

        let outsider = ServiceAccount::new(65532, &[65532]);
        assert_eq!(
            AclPreset::GroupWrite.effective_permissions(OWNER, outsider),
            PosixPermission::NONE
        );

        let dataset_owner = ServiceAccount::new(10001, &[]);
        assert_eq!(
            AclPreset::Private.effective_permissions(OWNER, dataset_owner),
            PosixPermission::READ_WRITE_EXECUTE
        );
    }

    #[test]
    fn preset_that_locks_out_the_service_account_is_refused() {
        let outsider = ServiceAccount::new(65532, &[65532]);
        for preset in [
            AclPreset::Private,
            AclPreset::GroupRead,
            AclPreset::GroupWrite,
        ] {
            assert_eq!(
                preset.validate_for_service_account(OWNER, outsider, AccessRequirement::ReadWrite),
                Err(AclError::LocksOutServiceAccount {
                    preset,
                    needed: PosixPermission::READ_WRITE_EXECUTE,
                    effective: PosixPermission::NONE,
                })
            );
        }

        let group_member = ServiceAccount::new(65532, &[10000]);
        assert!(AclPreset::GroupWrite
            .validate_for_service_account(OWNER, group_member, AccessRequirement::ReadWrite)
            .is_ok());
        assert_eq!(
            AclPreset::GroupRead.validate_for_service_account(
                OWNER,
                group_member,
                AccessRequirement::ReadWrite
            ),
            Err(AclError::LocksOutServiceAccount {
                preset: AclPreset::GroupRead,
                needed: PosixPermission::READ_WRITE_EXECUTE,
                effective: PosixPermission::READ_EXECUTE,
            })
        );
    }

    #[test]
    fn read_only_requirement_accepts_read_presets() {
        let outsider = ServiceAccount::new(65532, &[65532]);
        assert!(AclPreset::PublicRead
            .validate_for_service_account(OWNER, outsider, AccessRequirement::ReadOnly)
            .is_ok());
        assert!(AclPreset::PublicRead
            .validate_for_service_account(OWNER, outsider, AccessRequirement::ReadWrite)
            .is_err());

        let group_member = ServiceAccount::new(65532, &[10000]);
        assert!(AclPreset::GroupRead
            .validate_for_service_account(OWNER, group_member, AccessRequirement::ReadOnly)
            .is_ok());
        assert!(AclPreset::Private
            .validate_for_service_account(OWNER, outsider, AccessRequirement::ReadOnly)
            .is_err());
    }
}
