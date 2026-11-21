//! Optional external directory configuration (FR-EXT, phase 1).
//!
//! This module parses and validates the configuration for an external LDAP
//! directory source. It does **not** dial the provider and it does **not**
//! participate in request-time authentication: the internal OpenLDAP
//! directory remains the identity source of record (FR-IDN-01…16).
//!
//! The provider is disabled unless `EXTERNAL_DIRECTORY_ENABLED=true`. When
//! enabled, the configuration is validated fail-closed and the bind password
//! is read from a mounted Secret file; no inline password is accepted.

#[cfg(test)]
use std::collections::HashMap;
use std::fmt;
use std::path::Path;

/// Whether the external directory source is enabled. Default `false`.
pub const ENV_ENABLED: &str = "EXTERNAL_DIRECTORY_ENABLED";
/// Provider type. Only `ldap` is implemented in phase 1.
pub const ENV_PROVIDER: &str = "EXTERNAL_DIRECTORY_PROVIDER";
/// Directory server hostname (no scheme, port, or path).
pub const ENV_HOST: &str = "EXTERNAL_DIRECTORY_HOST";
/// Directory server port. Default: 636 with TLS, 389 without.
pub const ENV_PORT: &str = "EXTERNAL_DIRECTORY_PORT";
/// Base DN used for the provider.
pub const ENV_BASE_DN: &str = "EXTERNAL_DIRECTORY_BASE_DN";
/// Service-account bind DN.
pub const ENV_BIND_DN: &str = "EXTERNAL_DIRECTORY_BIND_DN";
/// Search base for users. Defaults to the base DN.
pub const ENV_USER_SEARCH_BASE: &str = "EXTERNAL_DIRECTORY_USER_SEARCH_BASE";
/// Search base for groups. Defaults to the base DN.
pub const ENV_GROUP_SEARCH_BASE: &str = "EXTERNAL_DIRECTORY_GROUP_SEARCH_BASE";
/// Attribute holding the login name. Default `uid`.
pub const ENV_UID_ATTRIBUTE: &str = "EXTERNAL_DIRECTORY_UID_ATTRIBUTE";
/// Attribute holding the POSIX uid. Default `uidNumber`.
pub const ENV_UID_NUMBER_ATTRIBUTE: &str = "EXTERNAL_DIRECTORY_UID_NUMBER_ATTRIBUTE";
/// Attribute holding the POSIX gid. Default `gidNumber`.
pub const ENV_GID_NUMBER_ATTRIBUTE: &str = "EXTERNAL_DIRECTORY_GID_NUMBER_ATTRIBUTE";
/// Attribute listing group members. Default `member`.
pub const ENV_GROUP_MEMBER_ATTRIBUTE: &str = "EXTERNAL_DIRECTORY_GROUP_MEMBER_ATTRIBUTE";
/// Use LDAPS. Default `true`; plaintext is refused off loopback.
pub const ENV_USE_TLS: &str = "EXTERNAL_DIRECTORY_USE_TLS";
/// CA bundle path used to verify the provider certificate.
pub const ENV_CA_CERT: &str = "EXTERNAL_DIRECTORY_CA_CERT";
/// File holding the service-account password. Required when enabled.
pub const ENV_BIND_PASS_FILE: &str = "EXTERNAL_DIRECTORY_BIND_PASS_FILE";

/// A supported external directory provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    /// Generic LDAP (phase 1).
    Ldap,
}

impl Provider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ldap => "ldap",
        }
    }

    fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "ldap" => Ok(Self::Ldap),
            "ad" | "active-directory" | "activedirectory" => Err(format!(
                "{ENV_PROVIDER}: Active Directory is reserved for phase 2 and is not implemented"
            )),
            other => Err(format!(
                "{ENV_PROVIDER}: unsupported provider {other:?}; only \"ldap\" is implemented"
            )),
        }
    }
}

/// A validated external directory configuration.
///
/// The bind password is deliberately **not** a field: it lives only in the
/// Secret-mounted file named by `bind_pass_file` and is read on demand, so a
/// derived `Debug` (or an accidental log) cannot expose it.
#[derive(Clone, PartialEq, Eq)]
pub struct Config {
    pub provider: Provider,
    pub host: String,
    pub port: u16,
    pub base_dn: String,
    pub bind_dn: String,
    pub bind_pass_file: String,
    pub user_search_base: String,
    pub group_search_base: String,
    pub uid_attribute: String,
    pub uid_number_attribute: String,
    pub gid_number_attribute: String,
    pub group_member_attribute: String,
    pub use_tls: bool,
    pub ca_cert_path: String,
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("provider", &self.provider)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("base_dn", &self.base_dn)
            .field("bind_dn", &self.bind_dn)
            .field("bind_pass_file", &self.bind_pass_file)
            .field("user_search_base", &self.user_search_base)
            .field("group_search_base", &self.group_search_base)
            .field("uid_attribute", &self.uid_attribute)
            .field("uid_number_attribute", &self.uid_number_attribute)
            .field("gid_number_attribute", &self.gid_number_attribute)
            .field("group_member_attribute", &self.group_member_attribute)
            .field("use_tls", &self.use_tls)
            .field("ca_cert_path", &self.ca_cert_path)
            .finish()
    }
}

/// The process-wide external directory configuration.
///
/// The default and every disabled state are inert: no provider is contacted
/// and no authentication path changes.
#[derive(Clone, Default)]
pub struct Store {
    config: Option<Config>,
}

impl fmt::Debug for Store {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.config {
            Some(config) => f.debug_tuple("Store").field(config).finish(),
            None => f.write_str("Store(disabled)"),
        }
    }
}

impl Store {
    /// Build from the process environment.
    pub fn from_env() -> Result<Self, String> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Build from an arbitrary lookup. Used by tests; the production path is
    /// [`Store::from_env`].
    pub fn from_lookup<F>(lookup: F) -> Result<Self, String>
    where
        F: Fn(&str) -> Option<String>,
    {
        let config = parse(&lookup)?;
        if let Some(config) = &config {
            // Fail closed if the mounted Secret is absent or empty. The
            // password is dropped immediately; it is not retained.
            let password = read_bind_password(&config.bind_pass_file)?;
            if password.is_empty() {
                return Err(format!(
                    "{ENV_BIND_PASS_FILE}: the bind password file {} is empty",
                    config.bind_pass_file
                ));
            }
            // The CA bundle is also a mounted Secret; a missing optional volume
            // must disable the source rather than leave an unverifiable trust
            // anchor configured.
            if !config.ca_cert_path.is_empty() {
                let ca = std::fs::read(&config.ca_cert_path).map_err(|e| {
                    format!("reading {ENV_CA_CERT} file {}: {e}", config.ca_cert_path)
                })?;
                if ca.is_empty() {
                    return Err(format!(
                        "{ENV_CA_CERT}: the CA bundle file {} is empty",
                        config.ca_cert_path
                    ));
                }
            }
        }
        Ok(Self { config })
    }

    /// An explicitly disabled store.
    pub fn disabled() -> Self {
        Self::default()
    }

    pub fn enabled(&self) -> bool {
        self.config.is_some()
    }

    pub fn config(&self) -> Option<&Config> {
        self.config.as_ref()
    }

    /// Read the bind password from its mounted Secret file.
    ///
    /// No request-time path uses this yet (phase 1 only stores the validated
    /// configuration), but keeping the only accessor file-based ensures a
    /// future client cannot accidentally read an inline credential.
    pub fn bind_password(&self) -> Result<String, String> {
        let config = self
            .config
            .as_ref()
            .ok_or_else(|| "external directory is disabled".to_string())?;
        read_bind_password(&config.bind_pass_file)
    }
}

fn parse<F>(lookup: F) -> Result<Option<Config>, String>
where
    F: Fn(&str) -> Option<String>,
{
    let enabled = optional_bool(&lookup, ENV_ENABLED, false)?;
    if !enabled {
        return Ok(None);
    }

    let provider = Provider::parse(
        optional_text(&lookup, ENV_PROVIDER)
            .as_deref()
            .unwrap_or("ldap"),
    )?;
    let use_tls = optional_bool(&lookup, ENV_USE_TLS, true)?;

    let host = required_text(&lookup, ENV_HOST)?;
    validate_host(&host)?;

    let port = match optional_text(&lookup, ENV_PORT) {
        Some(raw) => raw
            .parse::<u16>()
            .map_err(|_| format!("{ENV_PORT}: expected a port number, got {raw:?}"))?,
        None if use_tls => 636,
        None => 389,
    };
    if port == 0 {
        return Err(format!("{ENV_PORT}: port 0 is not valid"));
    }

    let base_dn = required_dn(&lookup, ENV_BASE_DN)?;
    let bind_dn = required_dn(&lookup, ENV_BIND_DN)?;

    let user_search_base = match optional_dn(&lookup, ENV_USER_SEARCH_BASE)? {
        Some(value) => {
            validate_search_base(ENV_USER_SEARCH_BASE, &value, &base_dn)?;
            value
        }
        None => base_dn.clone(),
    };
    let group_search_base = match optional_dn(&lookup, ENV_GROUP_SEARCH_BASE)? {
        Some(value) => {
            validate_search_base(ENV_GROUP_SEARCH_BASE, &value, &base_dn)?;
            value
        }
        None => base_dn.clone(),
    };

    let uid_attribute = optional_attribute(&lookup, ENV_UID_ATTRIBUTE, "uid")?;
    let uid_number_attribute = optional_attribute(&lookup, ENV_UID_NUMBER_ATTRIBUTE, "uidNumber")?;
    let gid_number_attribute = optional_attribute(&lookup, ENV_GID_NUMBER_ATTRIBUTE, "gidNumber")?;
    let group_member_attribute = optional_attribute(&lookup, ENV_GROUP_MEMBER_ATTRIBUTE, "member")?;

    let ca_cert_path = match optional_text(&lookup, ENV_CA_CERT) {
        Some(path) => {
            validate_absolute_path(ENV_CA_CERT, &path)?;
            path
        }
        None if use_tls => {
            return Err(format!(
                "{ENV_CA_CERT} is required when {ENV_USE_TLS}=true; refusing to trust an unverified directory certificate"
            ))
        }
        None => String::new(),
    };

    if !use_tls && !is_loopback_host(&host) {
        return Err(format!(
            "{ENV_USE_TLS}=false is refused for {host}: the service-account password would cross the network in cleartext"
        ));
    }

    let bind_pass_file = required_text(&lookup, ENV_BIND_PASS_FILE)?;
    validate_absolute_path(ENV_BIND_PASS_FILE, &bind_pass_file)?;

    Ok(Some(Config {
        provider,
        host,
        port,
        base_dn,
        bind_dn,
        bind_pass_file,
        user_search_base,
        group_search_base,
        uid_attribute,
        uid_number_attribute,
        gid_number_attribute,
        group_member_attribute,
        use_tls,
        ca_cert_path,
    }))
}

fn optional_text<F>(lookup: &F, name: &str) -> Option<String>
where
    F: Fn(&str) -> Option<String>,
{
    lookup(name)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn required_text<F>(lookup: &F, name: &str) -> Result<String, String>
where
    F: Fn(&str) -> Option<String>,
{
    optional_text(lookup, name).ok_or_else(|| format!("{name} is required when {ENV_ENABLED}=true"))
}

fn optional_bool<F>(lookup: &F, name: &str, default: bool) -> Result<bool, String>
where
    F: Fn(&str) -> Option<String>,
{
    match optional_text(lookup, name) {
        None => Ok(default),
        Some(value) if value.eq_ignore_ascii_case("true") => Ok(true),
        Some(value) if value.eq_ignore_ascii_case("false") => Ok(false),
        Some(value) => Err(format!(
            "{name}: expected \"true\" or \"false\", got {value:?}"
        )),
    }
}

fn required_dn<F>(lookup: &F, name: &str) -> Result<String, String>
where
    F: Fn(&str) -> Option<String>,
{
    let value = required_text(lookup, name)?;
    validate_dn(name, &value)?;
    Ok(value)
}

fn optional_dn<F>(lookup: &F, name: &str) -> Result<Option<String>, String>
where
    F: Fn(&str) -> Option<String>,
{
    match optional_text(lookup, name) {
        Some(value) => {
            validate_dn(name, &value)?;
            Ok(Some(value))
        }
        None => Ok(None),
    }
}

fn optional_attribute<F>(lookup: &F, name: &str, default: &str) -> Result<String, String>
where
    F: Fn(&str) -> Option<String>,
{
    let value = optional_text(lookup, name).unwrap_or_else(|| default.to_string());
    validate_attribute(name, &value)?;
    Ok(value)
}

fn validate_host(host: &str) -> Result<(), String> {
    if host.len() > 253 {
        return Err(format!("{ENV_HOST}: hostname is too long"));
    }
    if !host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    {
        return Err(format!(
            "{ENV_HOST}: expected a hostname or IPv4 address without a scheme, port, or path"
        ));
    }
    if host.starts_with(['.', '-']) || host.ends_with(['.', '-']) || host.contains("..") {
        return Err(format!("{ENV_HOST}: malformed hostname {host:?}"));
    }
    Ok(())
}

fn is_loopback_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    // Only an actual IPv4 loopback address counts: a name such as
    // "127.ldap.example.com" merely starts with "127." and is not loopback.
    host.parse::<std::net::Ipv4Addr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

fn validate_dn(name: &str, value: &str) -> Result<(), String> {
    if !value.contains('=') {
        return Err(format!(
            "{name}: expected an LDAP distinguished name containing '='"
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(format!(
            "{name}: control characters are not allowed in a DN"
        ));
    }
    Ok(())
}

fn validate_search_base(name: &str, value: &str, base_dn: &str) -> Result<(), String> {
    // Compare normalized DNs so `ou=people, dc=example, dc=test` and
    // `ou=people,dc=example,dc=test` are equivalent; the raw-string suffix
    // check rejected valid DNs with whitespace after a comma.
    let value = normalize_dn(value);
    let base = normalize_dn(base_dn);
    if value == base || value.ends_with(&format!(",{base}")) {
        return Ok(());
    }
    Err(format!(
        "{name}: the search base must equal {ENV_BASE_DN} or be a descendant of it"
    ))
}

fn normalize_dn(value: &str) -> String {
    value
        .split(',')
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(",")
        .to_ascii_lowercase()
}

fn validate_attribute(name: &str, value: &str) -> Result<(), String> {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return Err(format!("{name}: attribute name is empty"));
    };
    if !first.is_ascii_alphabetic() || !chars.all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(format!(
            "{name}: {value:?} is not a valid LDAP attribute name"
        ));
    }
    Ok(())
}

fn validate_absolute_path(name: &str, value: &str) -> Result<(), String> {
    if value.chars().any(char::is_control) {
        return Err(format!(
            "{name}: control characters are not allowed in a path"
        ));
    }
    if !Path::new(value).is_absolute() {
        return Err(format!("{name}: expected an absolute path"));
    }
    Ok(())
}

fn read_bind_password(path: &str) -> Result<String, String> {
    let value = std::fs::read_to_string(path)
        .map_err(|e| format!("reading bind password file {path}: {e}"))?;
    Ok(value.trim_end_matches(['\r', '\n']).to_string())
}

/// Parse a map of environment variables. Test helper kept private to the
/// module so the public surface stays the store.
#[cfg(test)]
fn parse_with(pairs: &[(&str, String)]) -> Result<Option<Config>, String> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), value.clone()))
        .collect();
    parse(|name| map.get(name).cloned())
}

#[cfg(test)]
fn store_with(pairs: &[(&str, String)]) -> Result<Store, String> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), value.clone()))
        .collect();
    Store::from_lookup(|name| map.get(name).cloned())
}

#[cfg(test)]
fn seed_secret_files(dir: &Path) {
    std::fs::write(dir.join("ca.crt"), "test-only-ca").unwrap();
    std::fs::write(dir.join("bind-password"), "not-a-real-secret\n").unwrap();
}

#[cfg(test)]
fn base_vars(dir: &Path) -> Vec<(&'static str, String)> {
    vec![
        (ENV_ENABLED, "true".to_string()),
        (ENV_HOST, "ldap.example.test".to_string()),
        (ENV_BASE_DN, "dc=example,dc=test".to_string()),
        (
            ENV_BIND_DN,
            "cn=naslos,ou=services,dc=example,dc=test".to_string(),
        ),
        (
            ENV_BIND_PASS_FILE,
            dir.join("bind-password").to_string_lossy().into_owned(),
        ),
        (
            ENV_CA_CERT,
            dir.join("ca.crt").to_string_lossy().into_owned(),
        ),
    ]
}

#[cfg(test)]
fn without(pairs: &[(&'static str, String)], name: &str) -> Vec<(&'static str, String)> {
    pairs
        .iter()
        .filter(|(key, _)| *key != name)
        .map(|(key, value)| (*key, value.clone()))
        .collect()
}

#[cfg(test)]
fn with(
    pairs: &[(&'static str, String)],
    name: &'static str,
    value: &str,
) -> Vec<(&'static str, String)> {
    let mut out = without(pairs, name);
    out.push((name, value.to_string()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn disabled_by_default_and_requires_no_settings() {
        assert!(parse_with(&[]).unwrap().is_none());
        assert!(!store_with(&[]).unwrap().enabled());
        // An explicit false behaves the same, even with junk in other vars.
        let vars = vec![
            (ENV_ENABLED, "false".to_string()),
            (ENV_HOST, "not a host".to_string()),
        ];
        assert!(parse_with(&vars).unwrap().is_none());
    }

    #[test]
    fn enabled_config_is_parsed_with_defaults() {
        let dir = tempdir().unwrap();
        seed_secret_files(dir.path());
        let config = parse_with(&base_vars(dir.path())).unwrap().unwrap();
        assert_eq!(config.provider, Provider::Ldap);
        assert_eq!(config.host, "ldap.example.test");
        assert_eq!(config.port, 636);
        assert!(config.use_tls);
        assert_eq!(config.user_search_base, config.base_dn);
        assert_eq!(config.group_search_base, config.base_dn);
        assert_eq!(config.uid_attribute, "uid");
        assert_eq!(config.uid_number_attribute, "uidNumber");
        assert_eq!(config.gid_number_attribute, "gidNumber");
        assert_eq!(config.group_member_attribute, "member");
    }

    #[test]
    fn enabled_requires_the_core_fields() {
        let err = parse_with(&[(ENV_ENABLED, "true".to_string())]).unwrap_err();
        assert!(err.contains(ENV_HOST), "{err}");
        assert!(!err.contains("not-a-real-secret"));
    }

    #[test]
    fn invalid_boolean_is_refused() {
        let err = parse_with(&[(ENV_ENABLED, "yes".to_string())]).unwrap_err();
        assert!(err.contains(ENV_ENABLED), "{err}");
    }

    #[test]
    fn active_directory_is_reserved_for_phase_two() {
        let dir = tempdir().unwrap();
        seed_secret_files(dir.path());
        let vars = with(&base_vars(dir.path()), ENV_PROVIDER, "ad");
        let err = parse_with(&vars).unwrap_err();
        assert!(err.contains("phase 2"), "{err}");
    }

    #[test]
    fn plaintext_ldap_is_refused_off_loopback() {
        let dir = tempdir().unwrap();
        seed_secret_files(dir.path());
        let vars = with(
            &without(&base_vars(dir.path()), ENV_CA_CERT),
            ENV_USE_TLS,
            "false",
        );
        let err = parse_with(&vars).unwrap_err();
        assert!(err.contains(ENV_USE_TLS), "{err}");

        // Loopback is the documented local-development exception.
        let vars = with(
            &with(&base_vars(dir.path()), ENV_HOST, "127.0.0.1"),
            ENV_USE_TLS,
            "false",
        );
        let vars = without(&vars, ENV_CA_CERT);
        assert!(parse_with(&vars).unwrap().is_some());

        // Only an actual loopback address is exempt: a DNS name that merely
        // starts with "127." is a remote host and must keep TLS.
        let vars = with(
            &with(
                &without(&base_vars(dir.path()), ENV_CA_CERT),
                ENV_HOST,
                "127.ldap.example.test",
            ),
            ENV_USE_TLS,
            "false",
        );
        let err = parse_with(&vars).unwrap_err();
        assert!(err.contains(ENV_USE_TLS), "{err}");
    }

    #[test]
    fn tls_requires_an_absolute_ca_path() {
        let dir = tempdir().unwrap();
        seed_secret_files(dir.path());
        let vars = without(&base_vars(dir.path()), ENV_CA_CERT);
        let err = parse_with(&vars).unwrap_err();
        assert!(err.contains(ENV_CA_CERT), "{err}");

        let vars = with(&base_vars(dir.path()), ENV_CA_CERT, "ca.crt");
        let err = parse_with(&vars).unwrap_err();
        assert!(err.contains("absolute"), "{err}");
    }

    #[test]
    fn a_missing_or_empty_ca_bundle_disables_the_source() {
        let dir = tempdir().unwrap();
        seed_secret_files(dir.path());

        let missing = dir.path().join("missing-ca.crt");
        let vars = with(
            &base_vars(dir.path()),
            ENV_CA_CERT,
            missing.to_str().unwrap(),
        );
        let err = store_with(&vars).unwrap_err();
        assert!(err.contains(ENV_CA_CERT), "{err}");

        let empty = dir.path().join("empty-ca.crt");
        std::fs::write(&empty, "").unwrap();
        let vars = with(&base_vars(dir.path()), ENV_CA_CERT, empty.to_str().unwrap());
        let err = store_with(&vars).unwrap_err();
        assert!(err.contains("empty"), "{err}");
    }

    #[test]
    fn bind_password_file_is_required_absolute_and_non_empty() {
        let dir = tempdir().unwrap();
        seed_secret_files(dir.path());

        let vars = without(&base_vars(dir.path()), ENV_BIND_PASS_FILE);
        let err = store_with(&vars).unwrap_err();
        assert!(err.contains(ENV_BIND_PASS_FILE), "{err}");

        let vars = with(&base_vars(dir.path()), ENV_BIND_PASS_FILE, "pw");
        let err = store_with(&vars).unwrap_err();
        assert!(err.contains("absolute"), "{err}");

        std::fs::write(dir.path().join("empty"), "").unwrap();
        let vars = with(
            &base_vars(dir.path()),
            ENV_BIND_PASS_FILE,
            dir.path().join("empty").to_str().unwrap(),
        );
        let err = store_with(&vars).unwrap_err();
        assert!(err.contains("empty"), "{err}");

        let vars = with(
            &base_vars(dir.path()),
            ENV_BIND_PASS_FILE,
            dir.path().join("missing").to_str().unwrap(),
        );
        let err = store_with(&vars).unwrap_err();
        assert!(err.contains("missing"), "{err}");
    }

    #[test]
    fn the_secret_is_read_on_demand_and_never_stored_or_logged() {
        let dir = tempdir().unwrap();
        seed_secret_files(dir.path());
        // An inline password variable is not part of the contract and must be
        // ignored; only the mounted file is read.
        let mut vars = base_vars(dir.path());
        vars.push(("EXTERNAL_DIRECTORY_BIND_PASS", "inline-not-used".into()));
        let store = store_with(&vars).unwrap();
        assert!(store.enabled());
        assert_eq!(store.bind_password().unwrap(), "not-a-real-secret");
        let rendered = format!("{store:?}");
        assert!(!rendered.contains("not-a-real-secret"), "{rendered}");
        assert!(!rendered.contains("inline-not-used"), "{rendered}");
        assert!(!rendered.contains("test-only-ca"), "{rendered}");
    }

    #[test]
    fn search_bases_must_stay_inside_the_base_dn() {
        let dir = tempdir().unwrap();
        seed_secret_files(dir.path());
        let vars = with(
            &base_vars(dir.path()),
            ENV_USER_SEARCH_BASE,
            "ou=people,dc=other,dc=test",
        );
        let err = parse_with(&vars).unwrap_err();
        assert!(err.contains(ENV_USER_SEARCH_BASE), "{err}");

        let vars = with(
            &base_vars(dir.path()),
            ENV_GROUP_SEARCH_BASE,
            "ou=groups,dc=example,dc=test",
        );
        let config = parse_with(&vars).unwrap().unwrap();
        assert_eq!(config.group_search_base, "ou=groups,dc=example,dc=test");

        // Whitespace after the RDN separators is valid and does not change
        // whether the value is a descendant.
        let vars = with(
            &base_vars(dir.path()),
            ENV_USER_SEARCH_BASE,
            "ou=people, dc=example, dc=test",
        );
        let config = parse_with(&vars).unwrap().unwrap();
        assert_eq!(config.user_search_base, "ou=people, dc=example, dc=test");
    }

    #[test]
    fn dns_paths_and_attributes_are_validated() {
        let dir = tempdir().unwrap();
        seed_secret_files(dir.path());
        let vars = with(&base_vars(dir.path()), ENV_BASE_DN, "example");
        let err = parse_with(&vars).unwrap_err();
        assert!(err.contains("distinguished name"), "{err}");

        let vars = with(&base_vars(dir.path()), ENV_UID_ATTRIBUTE, "bad attr");
        let err = parse_with(&vars).unwrap_err();
        assert!(err.contains("attribute"), "{err}");

        let vars = with(&base_vars(dir.path()), ENV_UID_ATTRIBUTE, "sAMAccountName");
        assert!(parse_with(&vars).unwrap().is_some());
    }
}
