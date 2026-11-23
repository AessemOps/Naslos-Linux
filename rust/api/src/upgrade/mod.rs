//! The `naslos-upgrade` operation binary (spec FR-UPD, docs/installer-contract §7).
//!
//! It runs in an in-cluster Job, never inside the API pod: the API cannot
//! upgrade its own release (no write access to `naslos`, and its pod is
//! replaced mid-upgrade). One operation at a time is recorded in the
//! `naslos-system-operation` ConfigMap, which is also the lock.

pub mod commands;
pub mod ops;
pub mod overrides;
pub mod status;

use clap::{Parser, ValueEnum};

/// Largest install pack the Job will download (matches the API cap).
pub const MAX_PACK_BYTES: usize = 512 * 1024 * 1024;

/// Verify a detached Ed25519 signature over the pack bytes. `pubkey` is a
/// base64 (or hex) 32-byte Ed25519 public key; `signature` is base64 or hex.
/// Fail closed on any decode or verification error.
pub fn verify_pack_signature(pack: &[u8], signature: &str, pubkey: &str) -> Result<(), String> {
    let key_bytes = decode_key_material(pubkey, 32, "pack public key")?;
    let sig_bytes = decode_key_material(signature, 64, "pack signature")?;
    let key = ed25519_dalek::VerifyingKey::from_bytes(
        &key_bytes
            .as_slice()
            .try_into()
            .map_err(|_| "pack public key must be 32 bytes".to_string())?,
    )
    .map_err(|e| format!("invalid pack public key: {e}"))?;
    let sig = ed25519_dalek::Signature::from_bytes(
        &sig_bytes
            .as_slice()
            .try_into()
            .map_err(|_| "pack signature must be 64 bytes".to_string())?,
    );
    key.verify_strict(pack, &sig)
        .map_err(|e| format!("pack signature does not verify: {e}"))
}

fn decode_key_material(text: &str, expected: usize, what: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(format!("the {what} is empty"));
    }
    let decoded = if let Ok(bytes) =
        base64::engine::general_purpose::STANDARD.decode(trimmed.as_bytes())
    {
        bytes
    } else if trimmed.len().is_multiple_of(2) && trimmed.bytes().all(|b| b.is_ascii_hexdigit()) {
        let mut out = Vec::with_capacity(trimmed.len() / 2);
        for index in (0..trimmed.len()).step_by(2) {
            out.push(
                u8::from_str_radix(&trimmed[index..index + 2], 16)
                    .map_err(|e| format!("the {what} is not valid hex: {e}"))?,
            );
        }
        out
    } else {
        return Err(format!("the {what} is not base64 or hex"));
    };
    if decoded.len() != expected {
        return Err(format!(
            "the {what} must decode to {expected} bytes (got {})",
            decoded.len()
        ));
    }
    Ok(decoded)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Operation {
    Naslos,
    Talos,
    Kubernetes,
}

#[derive(Debug, Parser)]
#[command(
    name = "naslos-upgrade",
    version,
    about = "Naslos system upgrade operation"
)]
pub struct Cli {
    #[arg(long, value_enum)]
    pub op: Operation,
    #[arg(long)]
    pub operation_id: String,
    /// The release being upgraded to (the pack's naslosVersion).
    #[arg(long)]
    pub version: String,
    /// The release currently running (recorded in the status).
    #[arg(long)]
    pub from: String,
    /// Download URL of the verified install pack.
    #[arg(long)]
    pub pack_url: String,
    /// sha256 the administrator approved; the download MUST match it.
    #[arg(long)]
    pub sha256: String,
    /// Optional detached Ed25519 signature URL for the pack.
    #[arg(long, default_value = "")]
    pub pack_sig_url: String,
    /// Optional base64/hex Ed25519 public key used to verify the pack.
    #[arg(long, default_value = "")]
    pub pack_pubkey: String,
    /// Cached, already-verified pack on the shared state volume (C-1c). When
    /// set and valid this is used instead of a network download, so the Job
    /// completes under the namespace default-deny.
    #[arg(long, default_value = "")]
    pub pack_file: String,
    /// Cached detached signature beside `--pack-file`.
    #[arg(long, default_value = "")]
    pub pack_sig_file: String,
    #[arg(long, default_value = "naslos")]
    pub namespace: String,
    #[arg(long, default_value = "naslos")]
    pub release: String,
    #[arg(long, default_value = "/var/run/secrets/talos.dev/config")]
    pub talosconfig: String,
    #[arg(long, default_value = "/var/lib/naslos")]
    pub state_dir: String,
}

pub async fn run(cli: Cli) -> Result<(), String> {
    // `--version` and `--from` are strict semver: the operation records
    // versions and compares them, so a loose string must never reach it
    // (R-063).
    crate::semver::Version::parse(&cli.version)
        .ok_or_else(|| format!("--version {:?} is not strict semver", cli.version))?;
    crate::semver::Version::parse(&cli.from)
        .ok_or_else(|| format!("--from {:?} is not strict semver", cli.from))?;
    if cli.sha256.len() != 64 || !cli.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("--sha256 must be 64 hex characters".into());
    }
    let mut writer = status::Writer::new(
        &cli.namespace,
        &cli.state_dir,
        &cli.operation_id,
        cli.op,
        &cli.from,
        &cli.version,
    );
    if let Some(conflict) = writer.load_or_init().await {
        return Err(conflict);
    }
    // A Job retry after a terminal state must not repeat the operation.
    match writer.current_phase() {
        "succeeded" => return Ok(()),
        "failed" => return Err("the operation already failed".into()),
        _ => {}
    }
    let result = match cli.op {
        Operation::Naslos => ops::naslos(&cli, &mut writer).await,
        Operation::Talos => ops::talos(&cli, &mut writer).await,
        Operation::Kubernetes => ops::kubernetes(&cli, &mut writer).await,
    };
    match result {
        Ok(message) => {
            writer.succeeded(&message).await;
            Ok(())
        }
        Err(e) => {
            writer.failed(&e).await;
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use ed25519_dalek::Signer;

    #[test]
    fn pack_urls_are_https_and_host_allowlisted() {
        assert!(crate::system::pack_url_allowed(
            "https://github.com/o/r/releases/download/v1/pack.tar.gz",
            "https://api.github.com/repos/o/r/releases"
        )
        .is_ok());
        assert!(crate::system::pack_url_allowed(
            "https://mirror.example/packs/pack.tar.gz",
            "https://mirror.example/releases.json"
        )
        .is_ok());
        // GitHub redirects release assets to this host; the hop check must
        // admit it or every real download fails after the redirect.
        assert!(crate::system::pack_url_allowed(
            "https://release-assets.githubusercontent.com/github-production-release-asset/x",
            "https://api.github.com/repos/o/r/releases"
        )
        .is_ok());
        assert!(crate::system::pack_url_allowed(
            "http://github.com/o/r/pack.tar.gz",
            "https://api.github.com/repos/o/r/releases"
        )
        .is_err());
        assert!(crate::system::pack_url_allowed(
            "https://evil.example/pack.tar.gz",
            "https://api.github.com/repos/o/r/releases"
        )
        .is_err());
    }

    #[test]
    fn pack_signature_roundtrip_and_tamper() {
        use base64::engine::general_purpose::STANDARD;
        let key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let pack = b"the install pack bytes";
        let signature = key.sign(pack);
        let sig_b64 = STANDARD.encode(signature.to_bytes());
        let key_b64 = STANDARD.encode(key.verifying_key().to_bytes());
        assert!(verify_pack_signature(pack, &sig_b64, &key_b64).is_ok());
        assert!(verify_pack_signature(b"tampered", &sig_b64, &key_b64).is_err());
        assert!(verify_pack_signature(pack, &sig_b64, "not-a-key").is_err());
        assert!(verify_pack_signature(pack, "", &key_b64).is_err());
    }
}
