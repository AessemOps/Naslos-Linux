//! ZFS native encryption planning and key custody (FR-ENC).
//!
//! API-side counterpart of `naslos_agent::zfs::encryption`. The API validates
//! first so a create request fails with a 400 before it reaches the agent, and
//! the agent validates again because the API is not the only caller it trusts.
//!
//! Key custody is the API's job: it generates a 32-byte raw key with the OS
//! CSPRNG, stores it in a Kubernetes Secret, and sends it to the agent only over
//! the authenticated API→agent channel. Key bytes are never returned, logged,
//! exported, put in argv or treated as a dataset option. `RawKey` redacts its
//! `Debug` output and zeroes its buffer on drop.
//!
//! Passphrase-keyed datasets, encrypted send/receive and raw Buddy sends remain
//! **[OPEN]** (FR-ENC-05/07/08; see `docs/dataset-encryption.md`).

use crate::kube::is_dns1123_subdomain;
use base64::Engine as _;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

/// A raw ZFS dataset key is always 32 random bytes.
pub const KEY_BYTES: usize = 32;

/// Prefix of every dataset-key Secret. The API only reads Secrets under this
/// prefix in its dedicated key namespace.
pub const KEY_SECRET_PREFIX: &str = "naslos-dataset-key-";

/// Annotation that binds a key Secret to the exact dataset it belongs to.
pub const DATASET_ANNOTATION: &str = "naslos.local/dataset";

/// Default namespace for dataset key Secrets (overridable in the chart).
pub const DEFAULT_KEYS_NAMESPACE: &str = "naslos-keys";

/// Stable refusal text for the unimplemented passphrase-keyed path.
pub const PASSPHRASE_NOT_IMPLEMENTED: &str = "passphrase-keyed datasets are not implemented yet: one-time passphrase entry and stdin transport are [OPEN] (FR-ENC-05)";

/// The encryption ciphers ZFS offers on the Talos ZFS build (`on` is the
/// documented alias for `aes-256-gcm`).
pub const SUPPORTED_ALGORITHMS: [&str; 6] = [
    "aes-128-ccm",
    "aes-192-ccm",
    "aes-256-ccm",
    "aes-128-gcm",
    "aes-192-gcm",
    "aes-256-gcm",
];

/// Key formats the planner accepts (`hex` is excluded).
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
    /// Parse an allow-listed `encryption=` value; `on` normalises to AES-256-GCM.
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

/// A validated encryption request: shape only, no key material.
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

/// A raw 32-byte dataset key whose `Debug` output never exposes the bytes.
pub struct RawKey([u8; KEY_BYTES]);

impl RawKey {
    /// Generate a key with the OS CSPRNG.
    pub fn generate() -> Result<Self, String> {
        let mut bytes = [0u8; KEY_BYTES];
        getrandom::fill(&mut bytes).map_err(|e| format!("generating the dataset key: {e}"))?;
        Ok(Self(bytes))
    }

    /// Validate and copy key bytes received from a Secret or an agent response.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, String> {
        validate_raw_key(bytes)?;
        let mut key = [0u8; KEY_BYTES];
        key.copy_from_slice(bytes);
        Ok(Self(key))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn to_base64(&self) -> String {
        base64::engine::general_purpose::STANDARD.encode(self.0)
    }
}

impl std::fmt::Debug for RawKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RawKey(<redacted>)")
    }
}

impl Drop for RawKey {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

/// Validate raw key material. The error deliberately reports only the length
/// rule, never the bytes.
pub fn validate_raw_key(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() != KEY_BYTES {
        return Err(format!(
            "a raw ZFS dataset key must be exactly {KEY_BYTES} bytes"
        ));
    }
    Ok(())
}

/// The namespace chart-deployed key Secrets live in. A namespace is not a
/// secret; the error path in the API reports it without key material.
pub fn keys_namespace() -> String {
    std::env::var("DATASET_KEYS_NAMESPACE")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_KEYS_NAMESPACE.to_string())
}

/// Validate a dataset-key Secret name before it is used with `kubectl`.
pub fn validate_key_secret_name(name: &str) -> Result<(), String> {
    if !name.starts_with(KEY_SECRET_PREFIX) || !is_dns1123_subdomain(name) {
        return Err(format!("invalid dataset key Secret name {name:?}"));
    }
    Ok(())
}

/// Deterministic Secret name for a dataset.
///
/// A readable slug plus a truncated SHA-256 digest keeps the name stable across
/// restarts, DNS-1123 safe for any dataset charset, and collision-resistant even
/// when two datasets slug identically.
pub fn secret_name_for_dataset(dataset: &str) -> Result<String, String> {
    let dataset = dataset.trim();
    if dataset.is_empty() {
        return Err("dataset name is required".into());
    }

    // `naslos-dataset-key-` is 19 bytes, the `-` and 16-hex digest add 17, so
    // the readable slug may use at most 27 bytes to stay inside the 63-char
    // DNS-1123 label limit.
    let mut slug = String::with_capacity(27);
    for ch in dataset.chars() {
        if slug.len() >= 27 {
            break;
        }
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "dataset" } else { slug };

    let digest = Sha256::digest(dataset.as_bytes());
    let hash: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    let name = format!("{KEY_SECRET_PREFIX}{slug}-{hash}");
    validate_key_secret_name(&name)?;
    Ok(name)
}

/// The Secret object for a new dataset key. The key exists only in `data.key`
/// (base64, which is what the Kubernetes API expects); metadata carries the
/// dataset name for verification, never the key.
pub fn key_secret_object(
    dataset: &str,
    key: &RawKey,
    namespace: &str,
) -> Result<serde_json::Value, String> {
    let name = secret_name_for_dataset(dataset)?;
    Ok(json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": name,
            "namespace": namespace,
            "labels": {
                "app.kubernetes.io/name": "naslos",
                "app.kubernetes.io/component": "dataset-key",
                "naslos.local/component": "dataset-key"
            },
            "annotations": { "naslos.local/dataset": dataset }
        },
        "type": "Opaque",
        "data": { "key": key.to_base64() }
    }))
}

/// Extract and validate the raw key from a `kubectl get secret -o json` object.
///
/// The dataset annotation must match exactly: a hash collision or a renamed
/// object must never load the wrong key. Errors never include the encoded key.
pub fn parse_key_secret(secret: &serde_json::Value, dataset: &str) -> Result<RawKey, String> {
    let annotated = secret
        .pointer("/metadata/annotations")
        .and_then(|a| a.get(DATASET_ANNOTATION))
        .and_then(|v| v.as_str());
    if annotated != Some(dataset) {
        return Err("the dataset key Secret does not belong to this dataset".into());
    }

    let encoded = secret
        .pointer("/data/key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "the dataset key Secret has no key material".to_string())?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| "the dataset key Secret key is not valid base64".to_string())?;
    RawKey::from_slice(&decoded)
}

/// Parse and validate the encryption shape of a dataset-create request.
///
/// `Ok(None)` means the request does not ask for encryption. A malformed
/// request is an error. A well-formed request returns the plan; the caller
/// still decides whether that key format is implemented.
pub fn parse_plan(options: &HashMap<String, String>) -> Result<Option<EncryptionPlan>, String> {
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

    if has_key_location {
        return Err(
            "keylocation must not be supplied by a create request; the agent chooses it from the key store"
                .into(),
        );
    }

    // zfsprops(7): encrypted datasets may not use copies=3.
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
/// Raw-key encryption is implemented. A passphrase-keyed request is refused
/// with the stable [`PASSPHRASE_NOT_IMPLEMENTED`] text so the UI can recognise
/// "valid request, not implemented yet".
pub fn validate_create_options(options: &HashMap<String, String>) -> Result<(), String> {
    match parse_plan(options)? {
        None => Ok(()),
        Some(plan) if plan.key_format == KeyFormat::Raw => Ok(()),
        Some(_) => Err(PASSPHRASE_NOT_IMPLEMENTED.into()),
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

    #[test]
    fn planner_matches_shared_fixture() {
        for line in include_str!("../../testdata/dataset_encryption.txt").lines() {
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
        validate_create_options(&options("encryption=aes-256-gcm;keyformat=raw")).expect("raw key");
        let err =
            validate_create_options(&options("encryption=on;keyformat=passphrase")).unwrap_err();
        assert!(err.contains("passphrase"), "{err}");
        assert!(err.contains("[OPEN]"), "{err}");
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
            assert!(err.contains("keylocation"), "{location}: {err}");
        }
    }

    #[test]
    fn generated_keys_are_32_bytes_and_redacted() {
        let key = RawKey::generate().expect("generate");
        assert_eq!(key.as_bytes().len(), KEY_BYTES);
        let other = RawKey::generate().expect("generate");
        assert_ne!(
            key.as_bytes(),
            other.as_bytes(),
            "the CSPRNG repeated a key"
        );

        let debug = format!("{key:?}");
        assert_eq!(debug, "RawKey(<redacted>)");
        assert!(!debug.contains(&key.to_base64()));

        let round_trip = base64::engine::general_purpose::STANDARD
            .decode(key.to_base64())
            .expect("valid base64");
        assert_eq!(round_trip, key.as_bytes());
    }

    #[test]
    fn raw_keys_must_be_exactly_32_bytes() {
        for length in [0, 1, 31, 33, 64] {
            let err = validate_raw_key(&vec![9u8; length]).unwrap_err();
            assert!(err.contains("32 bytes"), "{length}: {err}");
        }
        assert!(validate_raw_key(&[9u8; 32]).is_ok());
    }

    #[test]
    fn secret_names_are_deterministic_and_dns_safe() {
        let a = secret_name_for_dataset("tank/media").unwrap();
        assert_eq!(a, secret_name_for_dataset("tank/media").unwrap());
        assert!(a.starts_with(KEY_SECRET_PREFIX), "{a}");
        assert!(a.len() <= 63, "{a}");
        assert!(validate_key_secret_name(&a).is_ok(), "{a}");

        let long = secret_name_for_dataset("tank/backups/weekly-snapshots").unwrap();
        assert!(long.len() <= 63, "{long}");
        assert!(validate_key_secret_name(&long).is_ok(), "{long}");

        let b = secret_name_for_dataset("tank/media2").unwrap();
        assert_ne!(a, b);
        // A dataset with characters the slug cannot keep still maps safely and
        // distinctly.
        let c = secret_name_for_dataset("tank/a.b_c-d").unwrap();
        assert!(validate_key_secret_name(&c).is_ok(), "{c}");
        assert_ne!(c, secret_name_for_dataset("tank/a-b-c-d").unwrap());

        for bad in [
            "",
            "naslos-dataset-key-Upper",
            "naslos-dataset-key-../x",
            "other-name",
        ] {
            assert!(validate_key_secret_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn key_secret_object_keeps_the_key_out_of_metadata() {
        let key = RawKey::from_slice(&[3u8; 32]).unwrap();
        let object = key_secret_object("tank/media", &key, "naslos-keys").unwrap();
        assert_eq!(
            object["metadata"]["name"],
            secret_name_for_dataset("tank/media").unwrap()
        );
        assert_eq!(
            object["metadata"]["annotations"][DATASET_ANNOTATION],
            "tank/media"
        );
        assert!(!serde_json::to_string(&object["metadata"])
            .unwrap()
            .contains(&key.to_base64()));
        assert_eq!(
            parse_key_secret(&object, "tank/media").unwrap().as_bytes(),
            key.as_bytes()
        );
    }

    #[test]
    fn key_secret_parsing_refuses_mismatches_without_echoing_the_key() {
        let key = RawKey::from_slice(&[5u8; 32]).unwrap();
        let object = key_secret_object("tank/media", &key, "naslos-keys").unwrap();
        let encoded = key.to_base64();

        for other in ["tank/other", ""] {
            let err = parse_key_secret(&object, other).unwrap_err();
            assert!(err.contains("does not belong"), "{err}");
            assert!(!err.contains(&encoded), "the error leaked the key: {err}");
        }

        let mut missing = object.clone();
        missing["data"] = json!({});
        let err = parse_key_secret(&missing, "tank/media").unwrap_err();
        assert!(err.contains("no key material"), "{err}");
        assert!(!err.contains(&encoded), "{err}");

        let mut bad_base64 = object.clone();
        bad_base64["data"]["key"] = json!("not base64!");
        let err = parse_key_secret(&bad_base64, "tank/media").unwrap_err();
        assert!(err.contains("not valid base64"), "{err}");

        let mut short = object;
        short["data"]["key"] = json!(base64::engine::general_purpose::STANDARD.encode([1u8; 31]));
        let err = parse_key_secret(&short, "tank/media").unwrap_err();
        assert!(err.contains("32 bytes"), "{err}");
    }
}
