//! Cross-implementation interop: the Rust port must open a chunk sealed by the
//! Go sender and verify its signed manifest. The fixture in `tests/fixtures/`
//! was produced by the Go `buddy` package (see the plan's Execution log); this
//! pins the wire format and the Go-compatible canonical JSON.

use base64::Engine;
use naslos_api::buddy::envelope::{open_chunk, unwrap_dek, validate_manifest_shape, Manifest};

#[test]
fn opens_a_go_sealed_chunk_and_verifies_the_manifest() {
    let raw = include_str!("fixtures/buddy_fixture.json");
    let fixture: serde_json::Value = serde_json::from_str(raw).unwrap();

    let public_key = fixture["publicKey"].as_str().unwrap();
    let kek = base64::engine::general_purpose::STANDARD
        .decode(fixture["kek"].as_str().unwrap())
        .unwrap();
    let chunk = base64::engine::general_purpose::STANDARD
        .decode(fixture["chunk"].as_str().unwrap())
        .unwrap();
    let plaintext = base64::engine::general_purpose::STANDARD
        .decode(fixture["plaintext"].as_str().unwrap())
        .unwrap();

    let manifest: Manifest = serde_json::from_value(fixture["manifest"].clone()).unwrap();

    // The Go-signed manifest must verify with the Go public key.
    manifest
        .verify_signature(public_key)
        .expect("a Go-signed manifest must verify in Rust");

    // The receiver-visible shape must validate.
    validate_manifest_shape(&manifest).expect("manifest shape");

    // Unwrap the per-chain DEK with the KEK, then open the Go-sealed chunk.
    let dek = unwrap_dek(
        &kek,
        &manifest.dek_wrapped,
        &manifest.source,
        &manifest.chain,
    )
    .expect("unwrapping the Go-wrapped DEK");
    let prefix = manifest.stream_prefix_bytes().unwrap();
    let chunk_meta = &manifest.chunks[0];
    let opened = open_chunk(
        &dek,
        &prefix,
        chunk_meta.generation,
        &manifest.source,
        &manifest.chain,
        chunk_meta.index,
        &chunk,
    )
    .expect("opening the Go-sealed chunk");
    assert_eq!(
        opened, plaintext,
        "Rust must decrypt Go's plaintext exactly"
    );

    // The recorded plaintext digest must match.
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(&opened);
    let digest: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(digest, chunk_meta.sha256_plain);
}
