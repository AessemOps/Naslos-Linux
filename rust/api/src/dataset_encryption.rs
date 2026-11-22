//! ZFS native encryption planning and validation (FR-ENC).
//!
//! API-side mirror of `naslos_agent::zfs::encryption`. The API validates first
//! so a create request fails with a 400 before it reaches the agent, and the
//! agent validates again because the API is not the only caller it trusts.
//!
//! Pure validation only: no key material is accepted, generated, stored or
//! returned here. Custody, unlock/lock and encrypted send are **[OPEN]**
//! (FR-ENC-03…09; see `docs/dataset-encryption.md`), so a well-formed request
//! is refused with [`NOT_IMPLEMENTED`] instead of creating a dataset whose key
//! would be lost on the next boot.

use std::collections::HashMap;

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

/// Key formats the first implementation will accept (`hex` is excluded).
pub const SUPPORTED_KEY_FORMATS: [&str; 2] = ["passphrase", "raw"];

/// Stable refusal text, identical to the agent's so the future UI can
/// recognise "valid request, feature not implemented yet".
pub const NOT_IMPLEMENTED: &str = "dataset encryption is not implemented yet: key custody, unlock/lock and encrypted send are [OPEN] (FR-ENC; see docs/dataset-encryption.md)";

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
    /// A generated 32-byte keyfile held by the key store (recommended).
    Raw,
    /// An operator passphrase processed through PBKDF2.
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

/// Parse and validate the encryption shape of a dataset-create request.
///
/// `Ok(None)` means the request does not ask for encryption. A malformed
/// request is an error. A well-formed request returns the plan; the caller
/// decides whether the feature is implemented (it is not yet).
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
/// The allow-list is enforced first; a well-formed request is then refused
/// with [`NOT_IMPLEMENTED`]. This slice deliberately creates no encrypted
/// dataset: custody, unlock and lock are not implemented, and a key held only
/// in memory would be lost on the next boot.
pub fn validate_create_options(options: &HashMap<String, String>) -> Result<(), String> {
    match parse_plan(options)? {
        None => Ok(()),
        Some(_) => Err(NOT_IMPLEMENTED.into()),
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
    fn valid_encryption_requests_are_refused_before_the_agent() {
        let mut opts = HashMap::new();
        opts.insert("encryption".into(), "aes-256-gcm".into());
        opts.insert("keyformat".into(), "raw".into());
        let err = validate_create_options(&opts).unwrap_err();
        assert!(err.contains("not implemented yet"), "{err}");
    }

    #[test]
    fn key_location_is_never_caller_controlled() {
        let mut opts = HashMap::new();
        opts.insert("encryption".into(), "aes-256-gcm".into());
        opts.insert("keyformat".into(), "raw".into());
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
}
