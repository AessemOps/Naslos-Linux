//! Emit a Rust-sealed buddy fixture for the Go interop test.
use base64::Engine;
use naslos_api::buddy::envelope::{
    generate_dek, new_stream_prefix, seal_chunk, wrap_dek, Manifest, ManifestChunk,
    CHUNK_PLAIN_SIZE, ENVELOPE_VERSION,
};
use naslos_api::buddy::keys::new_identity;

fn main() {
    let id = new_identity("naslos-rust-fixture").unwrap();
    let kek = id.kek_bytes().unwrap();
    let dek = generate_dek().unwrap();
    let prefix = new_stream_prefix().unwrap();
    let source = "naslos-test/data";
    let chain = "rustchain";
    let plain = b"rust-sealed buddy payload";
    let (sealed, digest) = seal_chunk(&dek, &prefix, 0, source, chain, 0, plain).unwrap();
    let wrapped = wrap_dek(&kek, &dek, source, chain).unwrap();
    let mut m = Manifest {
        version: ENVELOPE_VERSION,
        source: source.to_string(),
        chain: chain.to_string(),
        kind: "zfs-send".to_string(),
        from_snapshot: String::new(),
        to_snapshot: "buddy-20260101T000000Z-rust".to_string(),
        from_guid: String::new(),
        to_guid: "0123456789abcdef".to_string(),
        created_at: chrono::DateTime::from_timestamp(1700000000, 0).unwrap(),
        key_id: String::new(),
        sequence: 1,
        generation: 0,
        stream_prefix: base64::engine::general_purpose::STANDARD.encode(&prefix),
        chunk_plain_size: CHUNK_PLAIN_SIZE as i64,
        chunks: vec![ManifestChunk {
            index: 0,
            plain_bytes: plain.len() as i64,
            sealed_bytes: sealed.len() as i64,
            generation: 0,
            sha256_plain: digest,
        }],
        dek_wrapped: wrapped,
        signature: String::new(),
    };
    m.sign(&id).unwrap();
    let fixture = serde_json::json!({
        "publicKey": id.public_key,
        "kek": id.kek,
        "manifest": m,
        "chunk": base64::engine::general_purpose::STANDARD.encode(&sealed),
        "plaintext": base64::engine::general_purpose::STANDARD.encode(plain),
    });
    println!("{}", serde_json::to_string_pretty(&fixture).unwrap());
}
