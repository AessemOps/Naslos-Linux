//! Share definitions (minimal port of `api/internal/shares` for the S2 slice).
//!
//! S2 only needs to read the persisted share definitions so dataset deletion can
//! refuse to remove a dataset that a share serves. The rendering/apply path and
//! LDAP user handling arrive in later slices.

use serde::{Deserialize, Serialize};

/// A share protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    #[default]
    Smb,
    Nfs,
    Afp,
}

/// A single share definition (field-for-field with the Go `Share`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Share {
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub protocol: Protocol,
    #[serde(default)]
    pub description: String,
    #[serde(default, rename = "readOnly")]
    pub read_only: bool,
    #[serde(default)]
    pub browseable: bool,
    #[serde(default, rename = "allowedHosts")]
    pub allowed_hosts: Vec<String>,
    #[serde(default, rename = "validUsers")]
    pub valid_users: Vec<String>,
    #[serde(default, rename = "validGroups")]
    pub valid_groups: Vec<String>,
    #[serde(default, rename = "timeMachine")]
    pub time_machine: bool,
    #[serde(default, rename = "noRootSquash")]
    pub no_root_squash: bool,
    #[serde(default)]
    pub enabled: bool,
}

/// Loads and holds the share definitions from the state file.
pub struct Store {
    shares: Vec<Share>,
}

impl Store {
    /// Load from `path`; a missing or malformed file yields an empty store (the
    /// Go manager's behavior).
    pub fn load(path: &str) -> Self {
        if path.is_empty() {
            return Self { shares: Vec::new() };
        }
        let Ok(data) = std::fs::read(path) else {
            return Self { shares: Vec::new() };
        };
        let mut shares: Vec<Share> = serde_json::from_slice(&data).unwrap_or_default();
        shares.retain(|s| !s.name.is_empty());
        shares.sort_by(|a, b| a.name.cmp(&b.name));
        Self { shares }
    }

    /// The configured shares, ordered by name.
    pub fn list(&self) -> &[Share] {
        &self.shares
    }
}

/// The default share config path (overridable by `SHARES_CONFIG`).
pub fn default_config_path() -> String {
    std::env::var("SHARES_CONFIG").unwrap_or_else(|_| "/var/lib/naslos/shares.json".to_string())
}
