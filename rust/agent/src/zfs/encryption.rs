//! ZFS native encryption: create, unlock/lock and keyfile handling (FR-ENC).
//!
//! The request-side half is a pure allow-list: it parses the encryption
//! *shape* a dataset-create request may ask for and validates it. The agent is
//! the only component that materialises key material for ZFS: a 32-byte raw key
//! is written to a 0600 file on the host's tmpfs, used by `zfs create` or
//! `zfs load-key`, and removed again. Key bytes never appear in argv,
//! environment variables, `-o` properties or logs.
//!
//! Passphrase-keyed datasets, encrypted send/receive and raw Buddy sends remain
//! **[OPEN]** (FR-ENC-05/07/08; see `docs/dataset-encryption.md`).

use super::dataset::validate_dataset_path;
use super::runner::join_host;
use super::validation::{ZfsError, ZfsResult};
use super::{ZfsClient, ZFS_BIN};
use crate::invalid;
use std::collections::HashMap;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// A raw ZFS dataset key is always 32 random bytes, regardless of the cipher.
pub const KEY_BYTES: usize = 32;

/// The host tmpfs directory the agent owns for transient key files.
pub const KEY_DIR_IN_HOST: &str = "/run/naslos-keys";

/// Stable refusal text for the unimplemented passphrase-keyed path.
pub const PASSPHRASE_NOT_IMPLEMENTED: &str = "passphrase-keyed datasets are not implemented yet: one-time passphrase entry and stdin transport are [OPEN] (FR-ENC-05)";

/// The encryption ciphers ZFS offers on the Talos ZFS build.
///
/// `encryption=on` is accepted as the documented alias for `aes-256-gcm`
/// (zfsprops(7)) and normalised in [`EncryptionAlgorithm::parse`].
pub const SUPPORTED_ALGORITHMS: [&str; 6] = [
    "aes-128-ccm",
    "aes-192-ccm",
    "aes-256-ccm",
    "aes-128-gcm",
    "aes-192-gcm",
    "aes-256-gcm",
];

/// Key formats accepted by the planner. `hex` is deliberately excluded: it has
/// no operational advantage over `raw` for a generated key.
pub const SUPPORTED_KEY_FORMATS: [&str; 2] = ["passphrase", "raw"];

/// The encryption cipher suite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncryptionAlgorithm {
    Aes128Ccm,
    Aes192Ccm,
    Aes256Ccm,
    Aes128Gcm,
    Aes192Gcm,
    Aes256Gcm,
}

impl EncryptionAlgorithm {
    /// Parse an allow-listed `encryption=` value. Values are case-sensitive and
    /// the ZFS default alias `on` normalises to AES-256-GCM.
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "on" | "aes-256-gcm" => Self::Aes256Gcm,
            "aes-128-ccm" => Self::Aes128Ccm,
            "aes-192-ccm" => Self::Aes192Ccm,
            "aes-256-ccm" => Self::Aes256Ccm,
            "aes-128-gcm" => Self::Aes128Gcm,
            "aes-192-gcm" => Self::Aes192Gcm,
            _ => return None,
        })
    }

    /// The canonical `zfs` property value (never `on`).
    pub fn as_zfs_value(self) -> &'static str {
        match self {
            Self::Aes128Ccm => "aes-128-ccm",
            Self::Aes192Ccm => "aes-192-ccm",
            Self::Aes256Ccm => "aes-256-ccm",
            Self::Aes128Gcm => "aes-128-gcm",
            Self::Aes192Gcm => "aes-192-gcm",
            Self::Aes256Gcm => "aes-256-gcm",
        }
    }
}

/// How the dataset key is supplied when it is loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyFormat {
    /// A generated 32-byte keyfile held by the key store (implemented).
    Raw,
    /// An operator passphrase processed through PBKDF2 ([OPEN]).
    Passphrase,
}

impl KeyFormat {
    /// Parse an allow-listed `keyformat=` value (`hex` is rejected).
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "raw" => Some(Self::Raw),
            "passphrase" => Some(Self::Passphrase),
            _ => None,
        }
    }

    /// The canonical `zfs` property value.
    pub fn as_zfs_value(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Passphrase => "passphrase",
        }
    }
}

/// A validated encryption request. It describes *shape* only; it carries no
/// key material, and the agent always chooses `keylocation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncryptionPlan {
    pub algorithm: EncryptionAlgorithm,
    pub key_format: KeyFormat,
}

impl EncryptionPlan {
    /// Stable `"<algorithm>,<keyformat>"` form used by the shared fixture.
    pub fn label(self) -> String {
        format!(
            "{},{}",
            self.algorithm.as_zfs_value(),
            self.key_format.as_zfs_value()
        )
    }
}

/// Parse and validate the encryption shape of a dataset-create request.
///
/// `Ok(None)` means the request does not ask for encryption. A malformed
/// request is a validation error. A well-formed request returns the plan; the
/// caller still decides whether that key format is implemented.
pub fn parse_plan(options: &HashMap<String, String>) -> ZfsResult<Option<EncryptionPlan>> {
    parse_plan_inner(options).map_err(ZfsError::Validation)
}

fn parse_plan_inner(options: &HashMap<String, String>) -> Result<Option<EncryptionPlan>, String> {
    let encryption = options.get("encryption").map(|v| v.trim());
    let has_key_format = options.contains_key("keyformat");
    let has_key_location = options.contains_key("keylocation");

    let Some(encryption) = encryption else {
        if has_key_format || has_key_location {
            return Err(
                "keyformat/keylocation require encryption to be set on the same create request"
                    .into(),
            );
        }
        return Ok(None);
    };

    if encryption == "off" {
        if has_key_format || has_key_location {
            return Err("encryption=off cannot be combined with keyformat or keylocation".into());
        }
        return Ok(None);
    }

    let Some(algorithm) = EncryptionAlgorithm::parse(encryption) else {
        return Err(format!(
            "unsupported encryption {encryption:?} (supported: on, {})",
            SUPPORTED_ALGORITHMS.join(", ")
        ));
    };

    let Some(raw_format) = options.get("keyformat").map(|v| v.trim()) else {
        return Err("keyformat is required for an encrypted dataset (raw or passphrase)".into());
    };
    let Some(key_format) = KeyFormat::parse(raw_format) else {
        return Err(format!(
            "unsupported keyformat {raw_format:?} (supported: {})",
            SUPPORTED_KEY_FORMATS.join(", ")
        ));
    };

    // A caller-chosen location is either an arbitrary host file path or an
    // HTTP(S) URL (zfsprops(7) accepts both); the key store, not the request,
    // owns that decision (FR-ENC-03).
    if has_key_location {
        return Err(
            "keylocation must not be supplied by a create request; the agent chooses it from the key store"
                .into(),
        );
    }

    // zfsprops(7): encrypted datasets may not use copies=3 because encryption
    // metadata already occupies the slot the third copy would use.
    if options.get("copies").map(|v| v.trim()) == Some("3") {
        return Err("encrypted datasets cannot use copies=3".into());
    }

    Ok(Some(EncryptionPlan {
        algorithm,
        key_format,
    }))
}

/// Validate a dataset-create option map for encryption.
///
/// Raw-key encryption is implemented. A passphrase request is refused with the
/// stable [`PASSPHRASE_NOT_IMPLEMENTED`] text; no request ever supplies the
/// key itself (the API sends it separately over the authenticated channel).
pub fn validate_create_options(options: &HashMap<String, String>) -> ZfsResult<()> {
    match parse_plan(options)? {
        None => Ok(()),
        Some(plan) if plan.key_format == KeyFormat::Raw => Ok(()),
        Some(_) => Err(ZfsError::Validation(PASSPHRASE_NOT_IMPLEMENTED.into())),
    }
}

/// Validate raw key material before it is materialised for ZFS.
pub fn validate_raw_key(key: &[u8]) -> ZfsResult<()> {
    if key.len() != KEY_BYTES {
        return Err(invalid!(
            "a raw ZFS dataset key must be exactly {KEY_BYTES} bytes"
        ));
    }
    Ok(())
}

/// The in-host keyfile path for a dataset (the dataset name is validated first,
/// so no component can escape the key directory).
pub fn keyfile_in_host(dataset: &str) -> ZfsResult<String> {
    validate_dataset_path(dataset)?;
    if dataset.contains('\\') {
        return Err(invalid!(
            "dataset {dataset:?} cannot be used for a keyfile path"
        ));
    }
    Ok(format!("{}/{}.key", KEY_DIR_IN_HOST, dataset))
}

fn host_keyfile(host_root: &Path, dataset: &str) -> ZfsResult<(String, PathBuf)> {
    let in_host = keyfile_in_host(dataset)?;
    Ok((in_host.clone(), join_host(host_root, &in_host)))
}

/// Write a 0600 keyfile under the agent-owned tmpfs directory. The path is
/// replaced atomically enough for ZFS's immediate read: an existing regular
/// file or symlink is removed first and `create_new` refuses a raced symlink.
fn write_keyfile_at(host_root: &Path, dataset: &str, key: &[u8]) -> ZfsResult<String> {
    validate_raw_key(key)?;
    let (in_host, path) = host_keyfile(host_root, dataset)?;
    let parent = path
        .parent()
        .ok_or_else(|| ZfsError::Other("the keyfile path has no parent".into()))?;

    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
        .map_err(|e| ZfsError::Other(format!("creating the dataset key directory: {e}")))?;
    let _ = std::fs::set_permissions(
        join_host(host_root, KEY_DIR_IN_HOST),
        std::fs::Permissions::from_mode(0o700),
    );

    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        if meta.file_type().is_dir() {
            return Err(ZfsError::Other(
                "the dataset key path is a directory".into(),
            ));
        }
        std::fs::remove_file(&path)
            .map_err(|e| ZfsError::Other(format!("replacing the dataset key file: {e}")))?;
    }

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| ZfsError::Other(format!("writing the dataset key file: {e}")))?;
    if let Err(e) = file.write_all(key).and_then(|_| file.sync_all()) {
        let _ = std::fs::remove_file(&path);
        return Err(ZfsError::Other(format!(
            "writing the dataset key file: {e}"
        )));
    }
    Ok(in_host)
}

/// Remove a transient keyfile; a missing file is success, and a directory at
/// the path is refused rather than followed.
fn remove_keyfile_at(host_root: &Path, dataset: &str) -> ZfsResult<()> {
    let (_, path) = host_keyfile(host_root, dataset)?;
    match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_dir() => Err(ZfsError::Other(
            "the dataset key path is a directory".into(),
        )),
        Ok(_) => std::fs::remove_file(&path)
            .map_err(|e| ZfsError::Other(format!("removing the dataset key file: {e}"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(ZfsError::Other(format!(
            "checking the dataset key file: {e}"
        ))),
    }
}

fn remove_keyfile_quietly(host_root: &Path, dataset: &str) {
    if let Err(e) = remove_keyfile_at(host_root, dataset) {
        tracing::warn!(
            "could not remove the transient keyfile for {}: {}",
            dataset,
            e
        );
    }
}

fn normalise_property(value: &str) -> String {
    let value = value.trim();
    if value == "-" {
        String::new()
    } else {
        value.to_string()
    }
}

fn normalise_keystatus(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() || value == "-" {
        "none".to_string()
    } else {
        value.to_string()
    }
}

fn parse_properties(out: &str) -> HashMap<String, String> {
    out.lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

fn build_create_argv(
    name: &str,
    options: &HashMap<String, String>,
    key_location: Option<&str>,
) -> Vec<String> {
    let mut argv: Vec<String> = vec!["create".into(), "-p".into()];
    let mut keys: Vec<&String> = options.keys().collect();
    keys.sort();
    for k in keys {
        argv.push("-o".into());
        argv.push(format!("{}={}", k, options[k].trim()));
    }
    if let Some(location) = key_location {
        argv.push("-o".into());
        argv.push(format!("keylocation={location}"));
    }
    argv.push(name.to_string());
    argv
}

/// Whether a `zfs list` failure proves the exact dataset is absent. Any other
/// failure must be propagated: treating it as "absent" could make a create run
/// over an existing dataset and a later rollback destroy it.
fn dataset_proven_absent(output: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    lower.contains("dataset does not exist") || lower.contains("no such pool")
}

impl ZfsClient {
    /// Read `encryption`, `keystatus`, `encryptionroot` and `mounted` for one
    /// dataset. `zfs get` is authoritative; list output is only a hint.
    pub async fn encryption_status(&self, dataset: &str) -> ZfsResult<super::EncryptionStatus> {
        validate_dataset_path(dataset)?;
        let out = self
            .host_exec(
                ZFS_BIN,
                &super::dataset::strings(&[
                    "get",
                    "-H",
                    "-o",
                    "property,value",
                    "encryption,keystatus,encryptionroot,mounted",
                    dataset,
                ]),
            )
            .await
            .map_err(|e| {
                ZfsError::Other(format!(
                    "reading the encryption status of {dataset}: {}: {}",
                    e.output.trim(),
                    e.message
                ))
            })?;
        let props = parse_properties(&out);
        Ok(super::EncryptionStatus {
            name: dataset.to_string(),
            encryption: normalise_property(
                props.get("encryption").map(String::as_str).unwrap_or(""),
            ),
            keystatus: normalise_keystatus(
                props.get("keystatus").map(String::as_str).unwrap_or(""),
            ),
            encryption_root: normalise_property(
                props
                    .get("encryptionroot")
                    .map(String::as_str)
                    .unwrap_or(""),
            ),
            mounted: props.get("mounted").map(String::as_str) == Some("yes"),
        })
    }

    /// The dataset whose key must be loaded for this dataset (`self` for an
    /// encryption root, the ancestor otherwise).
    fn encryption_root(status: &super::EncryptionStatus) -> String {
        if status.encryption_root.is_empty() {
            status.name.clone()
        } else {
            status.encryption_root.clone()
        }
    }

    async fn exact_dataset_exists(&self, name: &str) -> ZfsResult<bool> {
        match self
            .host_exec(
                ZFS_BIN,
                &super::dataset::strings(&["list", "-H", "-o", "name", name]),
            )
            .await
        {
            Ok(out) => Ok(out.lines().any(|line| line.trim() == name)),
            Err(e) if dataset_proven_absent(&e.output) => Ok(false),
            Err(e) => Err(ZfsError::Other(format!(
                "checking whether dataset {name} exists: {}: {}",
                e.output.trim(),
                e.message
            ))),
        }
    }

    /// Destroy only a dataset this create is known to have produced. The
    /// caller must not invoke it when existence could not be proven.
    async fn rollback_created_dataset(&self, name: &str) {
        if matches!(self.exact_dataset_exists(name).await, Ok(true)) {
            let _ = self
                .host_exec(ZFS_BIN, &super::dataset::strings(&["destroy", name]))
                .await;
        }
    }

    /// Create an encrypted dataset with an API-supplied raw key.
    ///
    /// The key is written to a 0600 tmpfs file, used as `keylocation`, and
    /// removed after `zfs create`. The exact dataset is verified afterwards
    /// because `zfs create -p` succeeds without applying properties when the
    /// target already exists (FR-ENC-06).
    pub async fn create_dataset_with_key(
        &self,
        name: &str,
        options: &HashMap<String, String>,
        key: Option<&[u8]>,
    ) -> ZfsResult<()> {
        super::dataset::validate_new_dataset_path(name)?;
        super::dataset::validate_dataset_options(options)?;

        let plan = parse_plan(options)?;
        let Some(plan) = plan else {
            if key.is_some() {
                return Err(invalid!(
                    "a raw key was supplied for an unencrypted dataset"
                ));
            }
            return self.create_plain_dataset(name, options).await;
        };
        if plan.key_format != KeyFormat::Raw {
            return Err(invalid!("{}", PASSPHRASE_NOT_IMPLEMENTED));
        }
        let key = key.ok_or_else(|| invalid!("an encrypted dataset needs its raw key"))?;
        validate_raw_key(key)?;

        // Serialise with pool destruction and other keyed creates: the
        // existence check and create must not interleave.
        let _guard = self.mutation_lock.lock().await;
        if self.exact_dataset_exists(name).await? {
            return Err(invalid!("dataset {name} already exists"));
        }

        let in_host = write_keyfile_at(&self.host_root, name, key)?;
        let location = format!("file://{in_host}");
        let argv = build_create_argv(name, options, Some(&location));
        let created = self.host_exec(ZFS_BIN, &argv).await;
        if let Err(e) = created {
            self.rollback_created_dataset(name).await;
            remove_keyfile_quietly(&self.host_root, name);
            return Err(ZfsError::Other(format!(
                "creating encrypted dataset: {}: {}",
                e.output.trim(),
                e.message
            )));
        }

        let status = self.encryption_status(name).await;
        let verified = matches!(
            &status,
            Ok(status)
                if status.encryption == plan.algorithm.as_zfs_value()
                    && status.keystatus == "available"
                    && (status.encryption_root.is_empty() || status.encryption_root == name)
        );
        if !verified {
            self.rollback_created_dataset(name).await;
            remove_keyfile_quietly(&self.host_root, name);
            return match status {
                Ok(status) => Err(invalid!(
                    "the created dataset did not verify as {}-encrypted and unlocked (encryption={:?}, keystatus={:?})",
                    plan.algorithm.as_zfs_value(),
                    status.encryption,
                    status.keystatus
                )),
                Err(e) => Err(e),
            };
        }

        remove_keyfile_quietly(&self.host_root, name);
        Ok(())
    }

    async fn create_plain_dataset(
        &self,
        name: &str,
        options: &HashMap<String, String>,
    ) -> ZfsResult<()> {
        let argv = build_create_argv(name, options, None);
        match self.host_exec(ZFS_BIN, &argv).await {
            Ok(_) => Ok(()),
            Err(e) => Err(ZfsError::Other(format!(
                "creating dataset: {}: {}",
                e.output.trim(),
                e.message
            ))),
        }
    }

    /// Load a dataset's raw key with `zfs load-key -L file://…` and remove the
    /// transient file. Repeated unlock is idempotent.
    pub async fn unlock_dataset(
        &self,
        dataset: &str,
        key: &[u8],
    ) -> ZfsResult<super::EncryptionStatus> {
        validate_dataset_path(dataset)?;
        validate_raw_key(key)?;
        let status = self.encryption_status(dataset).await?;
        if !status.is_encrypted() {
            return Err(invalid!("dataset {dataset} is not encrypted"));
        }
        let root = Self::encryption_root(&status);
        if status.keystatus == "available" {
            remove_keyfile_quietly(&self.host_root, &root);
            return Ok(status);
        }

        let in_host = write_keyfile_at(&self.host_root, &root, key)?;
        let location = format!("file://{in_host}");
        let loaded = self
            .host_exec(
                ZFS_BIN,
                &super::dataset::strings(&["load-key", "-L", &location, &root]),
            )
            .await;
        remove_keyfile_quietly(&self.host_root, &root);
        match loaded {
            Ok(_) => {
                let after = self.encryption_status(dataset).await?;
                if after.keystatus == "available" {
                    Ok(after)
                } else {
                    Err(ZfsError::Other(format!(
                        "the key was loaded but dataset {dataset} still reports keystatus={:?}",
                        after.keystatus
                    )))
                }
            }
            Err(e) => Err(ZfsError::Other(format!(
                "loading the dataset key: {}: {}",
                e.output.trim(),
                e.message
            ))),
        }
    }

    /// Unload a dataset's key. Refused while the dataset or a descendant is
    /// mounted; repeated lock is idempotent.
    pub async fn lock_dataset(&self, dataset: &str) -> ZfsResult<super::EncryptionStatus> {
        validate_dataset_path(dataset)?;
        let status = self.encryption_status(dataset).await?;
        if !status.is_encrypted() {
            return Err(invalid!("dataset {dataset} is not encrypted"));
        }
        let root = Self::encryption_root(&status);
        if status.keystatus != "available" {
            remove_keyfile_quietly(&self.host_root, &root);
            return Ok(status);
        }
        if status.mounted {
            return Err(invalid!(
                "dataset {dataset} is mounted; stop the shares/apps using it, unmount it, then lock"
            ));
        }

        let mounted = self
            .host_exec(
                ZFS_BIN,
                &super::dataset::strings(&[
                    "get",
                    "-H",
                    "-o",
                    "name,value",
                    "-r",
                    "mounted",
                    &root,
                ]),
            )
            .await
            .map_err(|e| {
                ZfsError::Other(format!(
                    "checking mounted children of {root}: {}: {}",
                    e.output.trim(),
                    e.message
                ))
            })?;
        if mounted
            .lines()
            .filter_map(|line| line.split_once('\t'))
            .any(|(_, value)| value.trim() == "yes")
        {
            return Err(invalid!(
                "dataset {root} has a mounted child; stop the shares/apps using it, unmount it, then lock"
            ));
        }

        self.host_exec(ZFS_BIN, &super::dataset::strings(&["unload-key", &root]))
            .await
            .map_err(|e| {
                ZfsError::Other(format!(
                    "unloading the dataset key: {}: {}",
                    e.output.trim(),
                    e.message
                ))
            })?;
        remove_keyfile_quietly(&self.host_root, &root);
        let after = self.encryption_status(dataset).await?;
        if after.keystatus == "available" {
            return Err(ZfsError::Other(format!(
                "dataset {dataset} still reports keystatus=available after unload-key"
            )));
        }
        Ok(after)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(raw: &str) -> HashMap<String, String> {
        raw.split(';')
            .filter(|p| !p.is_empty())
            .map(|pair| {
                let (k, v) = pair.split_once('=').expect("key=value fixture entry");
                (k.to_string(), v.to_string())
            })
            .collect()
    }

    fn key() -> Vec<u8> {
        (1u8..=32).collect()
    }

    #[test]
    fn planner_matches_shared_fixture() {
        for line in include_str!("../../../testdata/dataset_encryption.txt").lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (input, expected) = line
                .split_once('|')
                .expect("fixture line is input|expected");
            let result = parse_plan(&options(input));
            match expected {
                "none" => assert_eq!(result.unwrap(), None, "for {input:?}"),
                "err" => assert!(result.is_err(), "for {input:?}"),
                plan => {
                    let plan = plan.strip_prefix("plan:").expect("plan:<alg>,<format>");
                    assert_eq!(
                        result.unwrap().expect("a plan").label(),
                        plan,
                        "for {input:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn raw_key_creation_is_accepted_and_passphrase_is_refused() {
        let raw = options("encryption=aes-256-gcm;keyformat=raw");
        assert!(validate_create_options(&raw).is_ok());

        let passphrase = options("encryption=on;keyformat=passphrase");
        let err = validate_create_options(&passphrase).unwrap_err();
        assert!(err.is_validation());
        assert!(err.to_string().contains("passphrase"), "{err}");
    }

    #[test]
    fn key_location_is_never_caller_controlled() {
        let mut opts = options("encryption=aes-256-gcm;keyformat=raw");
        for location in [
            "file:///etc/shadow",
            "file:///var/lib/naslos/key",
            "https://example.invalid/key",
            "prompt",
        ] {
            opts.insert("keylocation".into(), location.into());
            let err = parse_plan(&opts).unwrap_err();
            assert!(err.to_string().contains("keylocation"), "{location}: {err}");
        }
    }

    #[test]
    fn raw_keys_must_be_exactly_32_bytes() {
        assert!(validate_raw_key(&key()).is_ok());
        for length in [0, 31, 33, 64] {
            let bytes = vec![7u8; length];
            let err = validate_raw_key(&bytes).unwrap_err();
            assert!(err.to_string().contains("32 bytes"), "{length}: {err}");
        }
    }

    #[test]
    fn keyfile_paths_stay_inside_the_key_directory() {
        assert_eq!(
            keyfile_in_host("tank/media").unwrap(),
            "/run/naslos-keys/tank/media.key"
        );
        for bad in [
            "tank/../etc",
            "tank/..",
            "/tank/media",
            "tank/media@snap",
            "tank\\media",
        ] {
            assert!(keyfile_in_host(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn keyfiles_are_private_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let in_host = write_keyfile_at(dir.path(), "tank/media", &key()).unwrap();
        let path = join_host(dir.path(), &in_host);
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::read(&path).unwrap(), key());

        remove_keyfile_at(dir.path(), "tank/media").unwrap();
        assert!(!path.exists());
        // Removing a missing file is idempotent.
        remove_keyfile_at(dir.path(), "tank/media").unwrap();
    }

    #[test]
    fn keyfiles_never_replace_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let in_host = keyfile_in_host("tank/media").unwrap();
        let path = join_host(dir.path(), &in_host);
        std::fs::create_dir_all(&path).unwrap();
        assert!(write_keyfile_at(dir.path(), "tank/media", &key()).is_err());
        assert!(remove_keyfile_at(dir.path(), "tank/media").is_err());
    }
}
