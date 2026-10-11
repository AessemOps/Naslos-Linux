//! Buddy envelope + signed manifest (port of `api/internal/buddy/envelope.go`).
//!
//! The wire format is fixed and must match the Go sender byte-for-byte: a
//! receiver stores opaque ciphertext, and a Rust owner must open chunks sealed
//! by a Go sender (and vice-versa). Chunks are sealed independently so a push is
//! resumable and memory is bounded on both sides.

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use base64::Engine;
use chrono::{DateTime, Utc};

use super::keys::{public_key_of, verify, Identity};

pub const ENVELOPE_VERSION: i32 = 2;
pub const CHUNK_PLAIN_SIZE: usize = 1 << 20;
pub const MAX_SEALED_CHUNK_SIZE: usize = CHUNK_PLAIN_SIZE + 4096;
pub const MAX_GENERATION: u16 = u16::MAX;
pub const MAX_CHUNK_INDEX: i64 = (1i64 << 32) - 1;

const STREAM_PREFIX_LEN: usize = 6;
const CHUNK_MAGIC: &[u8; 4] = b"NBC2";
const AEAD_LABEL: &str = "NB2";
const NONCE_SIZE: usize = 12;

/// One sealed chunk as recorded in the manifest.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ManifestChunk {
    pub index: i64,
    #[serde(rename = "plainBytes")]
    pub plain_bytes: i64,
    #[serde(rename = "sealedBytes")]
    pub sealed_bytes: i64,
    pub generation: u16,
    #[serde(rename = "sha256Plain")]
    pub sha256_plain: String,
}

/// One pushed segment of a backup chain.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Manifest {
    pub version: i32,
    pub source: String,
    pub chain: String,
    pub kind: String,
    #[serde(
        rename = "fromSnapshot",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub from_snapshot: String,
    #[serde(
        rename = "toSnapshot",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub to_snapshot: String,
    #[serde(rename = "fromGUID", default, skip_serializing_if = "String::is_empty")]
    pub from_guid: String,
    #[serde(rename = "toGUID", default, skip_serializing_if = "String::is_empty")]
    pub to_guid: String,
    #[serde(rename = "createdAt")]
    pub created_at: DateTime<Utc>,
    #[serde(rename = "keyId")]
    pub key_id: String,
    pub sequence: u64,
    pub generation: u16,
    #[serde(rename = "streamPrefix")]
    pub stream_prefix: String,
    #[serde(rename = "chunkPlainSize")]
    pub chunk_plain_size: i64,
    pub chunks: Vec<ManifestChunk>,
    #[serde(rename = "dekWrapped")]
    pub dek_wrapped: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub signature: String,
}

/// The random prefix that keeps a chain's nonces unique.
pub fn new_stream_prefix() -> Result<Vec<u8>, String> {
    let mut prefix = [0u8; STREAM_PREFIX_LEN];
    getrandom::getrandom(&mut prefix).map_err(|e| format!("generating stream prefix: {e}"))?;
    Ok(prefix.to_vec())
}

/// `prefix || generation || index`.
pub fn chunk_nonce(prefix: &[u8], generation: u16, index: i64) -> [u8; NONCE_SIZE] {
    let mut nonce = [0u8; NONCE_SIZE];
    nonce[..STREAM_PREFIX_LEN].copy_from_slice(&prefix[..STREAM_PREFIX_LEN]);
    nonce[STREAM_PREFIX_LEN..STREAM_PREFIX_LEN + 2].copy_from_slice(&generation.to_be_bytes());
    nonce[STREAM_PREFIX_LEN + 2..].copy_from_slice(&(index as u32).to_be_bytes());
    nonce
}

pub fn validate_chunk_index(index: i64) -> Result<(), String> {
    if !(0..=MAX_CHUNK_INDEX).contains(&index) {
        return Err(format!(
            "chunk index {index} is outside 0..{MAX_CHUNK_INDEX}"
        ));
    }
    Ok(())
}

fn chunk_aad(source: &str, chain: &str, generation: u16, index: i64, plain_len: usize) -> Vec<u8> {
    format!("{AEAD_LABEL}|{source}|{chain}|{generation}|{index}|{plain_len}").into_bytes()
}

fn seal_prefix(prefix: &[u8]) -> Result<(), String> {
    if prefix.len() != STREAM_PREFIX_LEN {
        return Err(format!(
            "stream prefix must be {STREAM_PREFIX_LEN} bytes, got {}",
            prefix.len()
        ));
    }
    Ok(())
}

/// Read the plaintext length a sealed blob declares (receiver-side, no key).
pub fn chunk_plain_len(sealed: &[u8]) -> Option<usize> {
    if sealed.len() < CHUNK_MAGIC.len() + 4 + NONCE_SIZE || &sealed[..4] != CHUNK_MAGIC {
        return None;
    }
    Some(u32::from_be_bytes([sealed[4], sealed[5], sealed[6], sealed[7]]) as usize)
}

fn new_dek() -> Result<[u8; 32], String> {
    let mut dek = [0u8; 32];
    getrandom::getrandom(&mut dek).map_err(|e| format!("generating data key: {e}"))?;
    Ok(dek)
}

fn aead_for(key: &[u8]) -> Result<Aes256Gcm, String> {
    if key.len() != 32 {
        return Err(format!(
            "AES-256-GCM needs a 32-byte key, got {}",
            key.len()
        ));
    }
    Aes256Gcm::new_from_slice(key).map_err(|e| e.to_string())
}

fn digest_of(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Seal one chunk; returns the blob plus the SHA-256 of the plaintext.
pub fn seal_chunk(
    dek: &[u8],
    prefix: &[u8],
    generation: u16,
    source: &str,
    chain: &str,
    index: i64,
    plain: &[u8],
) -> Result<(Vec<u8>, String), String> {
    validate_chunk_index(index)?;
    seal_prefix(prefix)?;
    if plain.len() as u64 > u32::MAX as u64 {
        return Err(format!("chunk too large to seal: {} bytes", plain.len()));
    }
    let aead = aead_for(dek)?;
    let nonce = chunk_nonce(prefix, generation, index);
    let aad = chunk_aad(source, chain, generation, index, plain.len());
    let sealed = aead
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plain,
                aad: &aad,
            },
        )
        .map_err(|e| format!("sealing chunk: {e}"))?;

    let mut buf = Vec::with_capacity(4 + 4 + NONCE_SIZE + sealed.len());
    buf.extend_from_slice(CHUNK_MAGIC);
    buf.extend_from_slice(&(plain.len() as u32).to_be_bytes());
    buf.extend_from_slice(&nonce);
    buf.extend_from_slice(&sealed);
    Ok((buf, digest_of(plain)))
}

/// Open a sealed chunk, failing on tampering, a wrong index/generation, or
/// truncation. `prefix`/`generation` come from the signed manifest.
pub fn open_chunk(
    dek: &[u8],
    prefix: &[u8],
    generation: u16,
    source: &str,
    chain: &str,
    index: i64,
    sealed: &[u8],
) -> Result<Vec<u8>, String> {
    validate_chunk_index(index)?;
    seal_prefix(prefix)?;
    if sealed.len() < CHUNK_MAGIC.len() + 4 + NONCE_SIZE {
        return Err("chunk is too short to be a valid envelope".to_string());
    }
    if &sealed[..4] != CHUNK_MAGIC {
        return Err("chunk is not a NBC2 envelope".to_string());
    }
    let plain_len = u32::from_be_bytes([sealed[4], sealed[5], sealed[6], sealed[7]]) as usize;
    if plain_len > CHUNK_PLAIN_SIZE {
        return Err(format!(
            "chunk declares an impossible plaintext size ({plain_len})"
        ));
    }
    if sealed[8..8 + NONCE_SIZE] != chunk_nonce(prefix, generation, index) {
        if sealed[8..8 + STREAM_PREFIX_LEN] != prefix[..STREAM_PREFIX_LEN] {
            return Err(format!(
                "chunk {index} carries a different stream prefix than the manifest"
            ));
        }
        return Err(format!(
            "chunk {index} carries a mismatched nonce: it belongs to another index or generation"
        ));
    }
    let aead = aead_for(dek)?;
    let aad = chunk_aad(source, chain, generation, index, plain_len);
    let plain = aead
        .decrypt(
            Nonce::from_slice(&sealed[8..8 + NONCE_SIZE]),
            Payload {
                msg: &sealed[8 + NONCE_SIZE..],
                aad: &aad,
            },
        )
        .map_err(|e| {
            format!("chunk {index} failed authentication (wrong key, or the data was altered): {e}")
        })?;
    if plain.len() != plain_len {
        return Err(format!(
            "chunk {index} decrypted to {} bytes, expected {plain_len}",
            plain.len()
        ));
    }
    Ok(plain)
}

/// Seal a chain's data key with the owner's KEK.
pub fn wrap_dek(kek: &[u8], dek: &[u8], source: &str, chain: &str) -> Result<String, String> {
    let aead = aead_for(kek)?;
    let mut nonce = [0u8; 12];
    getrandom::getrandom(&mut nonce).map_err(|e| format!("generating wrap nonce: {e}"))?;
    let aad = format!("{AEAD_LABEL}|dek|{source}|{chain}");
    let sealed = aead
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: dek,
                aad: aad.as_bytes(),
            },
        )
        .map_err(|e| format!("wrapping data key: {e}"))?;
    let mut out = nonce.to_vec();
    out.extend_from_slice(&sealed);
    Ok(base64::engine::general_purpose::STANDARD.encode(out))
}

/// Open a wrapped data key with the owner's KEK.
pub fn unwrap_dek(kek: &[u8], wrapped: &str, source: &str, chain: &str) -> Result<Vec<u8>, String> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(wrapped)
        .map_err(|e| format!("decoding wrapped key: {e}"))?;
    if raw.len() < 12 + 32 + 16 {
        return Err("wrapped key is too short".to_string());
    }
    let aead = aead_for(kek)?;
    let aad = format!("{AEAD_LABEL}|dek|{source}|{chain}");
    aead.decrypt(
        Nonce::from_slice(&raw[..12]),
        Payload {
            msg: &raw[12..],
            aad: aad.as_bytes(),
        },
    )
    .map_err(|e| {
        format!("cannot unwrap the data key: this backup was not made with this key ({e})")
    })
}

/// A fresh per-chain data key (exposed for the sender).
pub fn generate_dek() -> Result<[u8; 32], String> {
    new_dek()
}

impl Manifest {
    /// The manifest's signed form: everything except the signature, marshalled
    /// with Go's `encoding/json` escaping so a Go-signed manifest verifies here.
    pub fn canonical(&self) -> Result<Vec<u8>, String> {
        let mut copy = self.clone();
        copy.signature = String::new();
        let json = serde_json::to_string(&copy).map_err(|e| e.to_string())?;
        Ok(go_json_escape(&json))
    }

    /// Sign the manifest and stamp it with the sender's key id.
    pub fn sign(&mut self, id: &Identity) -> Result<(), String> {
        self.key_id = id.fingerprint()?;
        let canonical = self.canonical()?;
        let sig = id.sign(&canonical)?;
        self.signature = base64::engine::general_purpose::STANDARD.encode(sig);
        Ok(())
    }

    /// Verify the manifest was signed by the holder of the authorized key, and
    /// that the key matches the manifest's own key id.
    pub fn verify_signature(&self, authorized_key: &str) -> Result<(), String> {
        if self.signature.is_empty() {
            return Err("manifest is not signed".to_string());
        }
        let (_, fingerprint) = public_key_of(authorized_key)?;
        if self.key_id != fingerprint {
            return Err(format!(
                "manifest was signed by {fingerprint} but claims {}",
                self.key_id
            ));
        }
        let sig = base64::engine::general_purpose::STANDARD
            .decode(&self.signature)
            .map_err(|e| format!("decoding manifest signature: {e}"))?;
        let canonical = self.canonical()?;
        verify(authorized_key, &canonical, &sig)
    }

    /// The manifest's decoded stream prefix.
    pub fn stream_prefix_bytes(&self) -> Result<Vec<u8>, String> {
        let prefix = base64::engine::general_purpose::STANDARD
            .decode(&self.stream_prefix)
            .map_err(|e| format!("decoding stream prefix: {e}"))?;
        seal_prefix(&prefix)?;
        Ok(prefix)
    }
}

/// Escape `<`, `>`, `&`, U+2028 and U+2029 the way Go's `encoding/json` does,
/// so canonical bytes are identical across implementations.
pub fn go_json_escape(json: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(json.len());
    for ch in json.chars() {
        match ch {
            '<' => out.extend_from_slice(b"\\u003c"),
            '>' => out.extend_from_slice(b"\\u003e"),
            '&' => out.extend_from_slice(b"\\u0026"),
            '\u{2028}' => out.extend_from_slice(b"\\u2028"),
            '\u{2029}' => out.extend_from_slice(b"\\u2029"),
            c => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    out
}

/// Check the sender-controlled parts of a manifest the receiver can validate.
pub fn validate_manifest_shape(m: &Manifest) -> Result<(), String> {
    if m.kind != "zfs-send" && m.kind != "tar" {
        return Err(format!("unsupported payload kind {:?}", m.kind));
    }
    if m.chunk_plain_size != CHUNK_PLAIN_SIZE as i64 {
        return Err(format!(
            "chunk plain size is {}, this receiver stores {CHUNK_PLAIN_SIZE}-byte chunks",
            m.chunk_plain_size
        ));
    }
    match base64::engine::general_purpose::STANDARD.decode(&m.stream_prefix) {
        Ok(prefix) if prefix.len() == STREAM_PREFIX_LEN => {}
        _ => {
            return Err(format!(
                "stream prefix must be base64 for {STREAM_PREFIX_LEN} bytes"
            ))
        }
    }
    if m.dek_wrapped.is_empty() {
        return Err("manifest carries no wrapped data key".to_string());
    }
    if base64::engine::general_purpose::STANDARD
        .decode(&m.dek_wrapped)
        .is_err()
    {
        return Err("wrapped data key is not valid base64".to_string());
    }
    for (i, chunk) in m.chunks.iter().enumerate() {
        if chunk.index != i as i64 {
            return Err(format!(
                "chunk {i} is out of order (index {}): the manifest must list chunks 0..n-1 exactly once",
                chunk.index
            ));
        }
        if chunk.generation > m.generation {
            return Err(format!(
                "chunk {i} declares generation {}, above the manifest's {}",
                chunk.generation, m.generation
            ));
        }
        if chunk.plain_bytes <= 0 || chunk.plain_bytes > CHUNK_PLAIN_SIZE as i64 {
            return Err(format!(
                "chunk {i} declares {} plain bytes, outside (0, {CHUNK_PLAIN_SIZE}]",
                chunk.plain_bytes
            ));
        }
        if chunk.sealed_bytes <= chunk.plain_bytes
            || chunk.sealed_bytes > MAX_SEALED_CHUNK_SIZE as i64
        {
            return Err(format!(
                "chunk {i} declares {} sealed bytes, which cannot hold {} plain bytes",
                chunk.sealed_bytes, chunk.plain_bytes
            ));
        }
        if chunk.sha256_plain.len() != 64
            || !chunk.sha256_plain.chars().all(|c| c.is_ascii_hexdigit())
        {
            return Err(format!("chunk {i} has an invalid plaintext digest"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_round_trip() {
        let dek = [7u8; 32];
        let prefix = new_stream_prefix().unwrap();
        let plain = b"hello buddy";
        let (sealed, digest) = seal_chunk(&dek, &prefix, 0, "src", "chain", 0, plain).unwrap();
        assert_eq!(digest, digest_of(plain));
        assert_eq!(chunk_plain_len(&sealed), Some(plain.len()));
        let opened = open_chunk(&dek, &prefix, 0, "src", "chain", 0, &sealed).unwrap();
        assert_eq!(opened, plain);
        // Wrong index fails.
        assert!(open_chunk(&dek, &prefix, 0, "src", "chain", 1, &sealed).is_err());
    }

    #[test]
    fn wrap_unwrap_dek() {
        let kek = [9u8; 32];
        let dek = [3u8; 32];
        let wrapped = wrap_dek(&kek, &dek, "src", "chain").unwrap();
        assert_eq!(unwrap_dek(&kek, &wrapped, "src", "chain").unwrap(), dek);
        // Wrong chain fails.
        assert!(unwrap_dek(&kek, &wrapped, "src", "other").is_err());
    }

    #[test]
    fn manifest_sign_verify() {
        let id = super::super::keys::new_identity("a").unwrap();
        let mut m = sample_manifest();
        m.sign(&id).unwrap();
        m.verify_signature(&id.public_key).unwrap();
        // Tampering breaks it.
        let mut bad = m.clone();
        bad.chain = "tampered".to_string();
        assert!(bad.verify_signature(&id.public_key).is_err());
    }

    #[test]
    fn go_json_escaping() {
        assert_eq!(
            go_json_escape("a<b>&c"),
            b"a\\u003cb\\u003e\\u0026c".to_vec()
        );
    }

    fn sample_manifest() -> Manifest {
        Manifest {
            version: ENVELOPE_VERSION,
            source: "naslos-test/data".into(),
            chain: "abc".into(),
            kind: "zfs-send".into(),
            from_snapshot: String::new(),
            to_snapshot: "buddy-1".into(),
            from_guid: String::new(),
            to_guid: "guid".into(),
            created_at: DateTime::from_timestamp(1700000000, 0).unwrap(),
            key_id: String::new(),
            sequence: 1,
            generation: 0,
            stream_prefix: base64::engine::general_purpose::STANDARD.encode([1u8, 2, 3, 4, 5, 6]),
            chunk_plain_size: CHUNK_PLAIN_SIZE as i64,
            chunks: vec![ManifestChunk {
                index: 0,
                plain_bytes: 10,
                sealed_bytes: 26,
                generation: 0,
                sha256_plain: "a".repeat(64),
            }],
            dek_wrapped: base64::engine::general_purpose::STANDARD.encode([0u8; 60]),
            signature: String::new(),
        }
    }
}
