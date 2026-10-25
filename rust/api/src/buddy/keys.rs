//! Buddy identity keys (port of `api/internal/buddy/keys.go`).
//!
//! Ed25519 in OpenSSH form, so operators can use `ssh-keygen -t ed25519` and
//! paste an authorized_keys line. The sender holds the private key + the KEK;
//! the receiver holds only the public key.

use base64::Engine;
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use ssh_key::private::{Ed25519Keypair, KeypairData};
use ssh_key::{HashAlg, LineEnding, PrivateKey, PublicKey};

/// The on-disk identity format version.
pub const IDENTITY_VERSION: i32 = 1;

/// The environment variable that overrides the KEK in the identity file.
pub const ENV_KEK: &str = "BUDDY_KEK";

/// The local instance's key material.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Identity {
    pub version: i32,
    #[serde(default)]
    pub name: String,
    #[serde(rename = "publicKey")]
    pub public_key: String,
    #[serde(rename = "privateKey")]
    pub private_key: String,
    /// Empty when `BUDDY_KEK` supplies the key (PF-M14): the KEK must not be
    /// persisted beside the identity private key.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kek: String,
    #[serde(rename = "createdAt")]
    pub created_at: DateTime<Utc>,
}

/// Whether the environment supplies the KEK.
fn env_kek_is_set() -> bool {
    std::env::var(ENV_KEK)
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
}

/// Generate a fresh Ed25519 keypair and (unless the environment supplies one)
/// a random KEK.
pub fn new_identity(name: &str) -> Result<Identity, String> {
    new_identity_with_env(name, env_kek_is_set())
}

fn new_identity_with_env(name: &str, env_kek: bool) -> Result<Identity, String> {
    let signing = SigningKey::generate(&mut ssh_key::rand_core::OsRng);
    let ed = ssh_key::private::Ed25519PrivateKey::from(&signing);
    let priv_key = PrivateKey::new(
        KeypairData::Ed25519(Ed25519Keypair {
            public: ed.clone().into(),
            private: ed,
        }),
        format!("naslos-buddy {name}"),
    )
    .map_err(|e| format!("encoding private key: {e}"))?;
    let private_pem = priv_key
        .to_openssh(LineEnding::LF)
        .map_err(|e| format!("encoding private key: {e}"))?
        .to_string();
    let public_line = priv_key
        .public_key()
        .to_openssh()
        .map_err(|e| format!("encoding public key: {e}"))?;

    let kek = if env_kek {
        String::new()
    } else {
        let mut kek = [0u8; 32];
        getrandom::getrandom(&mut kek)
            .map_err(|e| format!("generating key encryption key: {e}"))?;
        base64::engine::general_purpose::STANDARD.encode(kek)
    };

    Ok(Identity {
        version: IDENTITY_VERSION,
        name: name.to_string(),
        public_key: public_line.trim().to_string(),
        private_key: private_pem,
        kek,
        created_at: Utc::now(),
    })
}

/// Read an identity file. When `BUDDY_KEK` is set, the persisted `kek` field is
/// removed and the file rewritten (PF-M14), so at-rest copies stop yielding the
/// key that unwraps every backup.
pub fn load_identity(path: &str) -> Result<Identity, String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let mut id: Identity =
        serde_json::from_slice(&data).map_err(|e| format!("parsing identity {path}: {e}"))?;
    if id.private_key.is_empty() || id.public_key.is_empty() {
        return Err(format!("identity {path} is incomplete"));
    }
    if env_kek_is_set() && !id.kek.is_empty() {
        id.kek.clear();
        id.save(path)?;
    }
    Ok(id)
}

impl Identity {
    /// Write the identity with owner-only permissions (0600), atomically.
    pub fn save(&self, path: &str) -> Result<(), String> {
        let data = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        let dir = std::path::Path::new(path)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        if dir != std::path::Path::new(".") {
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        }
        let tmp = tempfile::Builder::new()
            .prefix(".identity-")
            .suffix(".tmp")
            .tempfile_in(&dir)
            .map_err(|e| e.to_string())?;
        {
            use std::io::Write;
            let mut f = tmp.as_file().try_clone().map_err(|e| e.to_string())?;
            f.write_all(&data).map_err(|e| e.to_string())?;
            f.sync_all().map_err(|e| e.to_string())?;
        }
        let tmp_path = tmp.into_temp_path();
        std::fs::set_permissions(
            &tmp_path,
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
        .map_err(|e| e.to_string())?;
        std::fs::rename(&tmp_path, path).map_err(|e| e.to_string())
    }

    /// The decoded Ed25519 signing key.
    pub fn signing_key(&self) -> Result<SigningKey, String> {
        let key = PrivateKey::from_openssh(self.private_key.as_bytes())
            .map_err(|e| format!("parsing private key: {e}"))?;
        match key.key_data() {
            KeypairData::Ed25519(kp) => Ok(SigningKey::from(&kp.private)),
            _ => Err("unsupported key type: buddy keys are Ed25519".to_string()),
        }
    }

    /// The raw Ed25519 signature of `msg`.
    pub fn sign(&self, msg: &[u8]) -> Result<Vec<u8>, String> {
        Ok(self.signing_key()?.sign(msg).to_bytes().to_vec())
    }

    /// The identity's key id (ssh-style `SHA256:…`).
    pub fn fingerprint(&self) -> Result<String, String> {
        let pub_key = PublicKey::from_openssh(self.public_key.trim())
            .map_err(|e| format!("parsing public key: {e}"))?;
        Ok(pub_key.fingerprint(HashAlg::Sha256).to_string())
    }

    /// Decode the KEK (env `BUDDY_KEK` takes precedence).
    pub fn kek_bytes(&self) -> Result<Vec<u8>, String> {
        let mut source = self.kek.trim().to_string();
        if let Ok(env) = std::env::var(ENV_KEK) {
            let env = env.trim().to_string();
            if !env.is_empty() {
                source = env;
            }
        }
        if source.is_empty() {
            return Err(format!(
                "no KEK: set {ENV_KEK} (recommended) or restore the identity file's kek field"
            ));
        }
        let kek = base64::engine::general_purpose::STANDARD
            .decode(source)
            .map_err(|e| format!("decoding kek: {e}"))?;
        if kek.len() != 32 {
            return Err(format!("kek must be 32 bytes, got {}", kek.len()));
        }
        Ok(kek)
    }
}

/// Parse an authorized_keys line into the Ed25519 public key plus fingerprint.
pub fn public_key_of(line: &str) -> Result<(VerifyingKey, String), String> {
    let pub_key =
        PublicKey::from_openssh(line.trim()).map_err(|e| format!("parsing authorized key: {e}"))?;
    let ed = pub_key
        .key_data()
        .ed25519()
        .ok_or_else(|| "unsupported key type: buddy keys are Ed25519".to_string())?;
    let bytes: [u8; 32] = ed.0;
    let verifying = VerifyingKey::from_bytes(&bytes)
        .map_err(|e| format!("unsupported key type: buddy keys are Ed25519 ({e})"))?;
    Ok((verifying, pub_key.fingerprint(HashAlg::Sha256).to_string()))
}

/// Verify a signature made by the holder of the given authorized key.
pub fn verify(authorized_key: &str, msg: &[u8], signature: &[u8]) -> Result<(), String> {
    let (verifying, _) = public_key_of(authorized_key)?;
    if signature.len() != 64 {
        return Err(format!(
            "signature must be 64 bytes, got {}",
            signature.len()
        ));
    }
    let sig_bytes: [u8; 64] = signature
        .try_into()
        .map_err(|_| "signature must be 64 bytes".to_string())?;
    let sig = ed25519_dalek::Signature::from_bytes(&sig_bytes);
    verifying
        .verify(msg, &sig)
        .map_err(|_| "signature verification failed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_sign_verify_round_trip() {
        let id = new_identity("naslos-a").unwrap();
        assert!(id.fingerprint().unwrap().starts_with("SHA256:"));
        let msg = b"hello";
        let sig = id.sign(msg).unwrap();
        verify(&id.public_key, msg, &sig).unwrap();
        assert!(verify(&id.public_key, b"other", &sig).is_err());
    }

    #[test]
    fn kek_is_32_bytes() {
        let id = new_identity_with_env("x", false).unwrap();
        assert_eq!(id.kek_bytes().unwrap().len(), 32);
    }

    #[test]
    fn new_identity_keeps_the_kek_out_of_the_file_when_env_supplies_it() {
        let id = new_identity_with_env("naslos-a", true).unwrap();
        assert!(id.kek.is_empty());
        let json = serde_json::to_string(&id).unwrap();
        assert!(!json.contains("kek"), "kek must not be serialized: {json}");
    }

    #[test]
    fn new_identity_persists_a_kek_without_the_env() {
        let id = new_identity_with_env("naslos-a", false).unwrap();
        assert!(!id.kek.is_empty());
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(&id.kek)
                .unwrap()
                .len(),
            32
        );
    }
}
