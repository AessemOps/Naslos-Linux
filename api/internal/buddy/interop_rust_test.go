package buddy

import (
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"os"
	"testing"
)

// TestOpensRustSealedFixture pins the reverse interop direction: the Go side
// must open a chunk sealed by the Rust port and verify its manifest. The
// fixture in testdata/ is emitted by rust/api/examples/buddy_fixture.rs.
func TestOpensRustSealedFixture(t *testing.T) {
	raw, err := os.ReadFile("testdata/rust_fixture.json")
	if err != nil {
		t.Fatalf("reading fixture: %v", err)
	}
	var f struct {
		PublicKey string   `json:"publicKey"`
		KEK       string   `json:"kek"`
		Manifest  Manifest `json:"manifest"`
		Chunk     string   `json:"chunk"`
		Plaintext string   `json:"plaintext"`
	}
	if err := json.Unmarshal(raw, &f); err != nil {
		t.Fatalf("parsing fixture: %v", err)
	}

	if err := f.Manifest.VerifySignature(f.PublicKey); err != nil {
		t.Fatalf("Rust-signed manifest must verify in Go: %v", err)
	}
	if err := validateManifestShape(&f.Manifest); err != nil {
		t.Fatalf("manifest shape: %v", err)
	}

	kek, err := base64.StdEncoding.DecodeString(f.KEK)
	if err != nil {
		t.Fatalf("decoding kek: %v", err)
	}
	dek, err := UnwrapDEK(kek, f.Manifest.DEKWrapped, f.Manifest.Source, f.Manifest.Chain)
	if err != nil {
		t.Fatalf("unwrapping the Rust-wrapped DEK: %v", err)
	}
	prefix, err := f.Manifest.streamPrefix()
	if err != nil {
		t.Fatalf("stream prefix: %v", err)
	}
	sealed, err := base64.StdEncoding.DecodeString(f.Chunk)
	if err != nil {
		t.Fatalf("decoding chunk: %v", err)
	}
	c := f.Manifest.Chunks[0]
	plain, err := OpenChunk(dek, prefix, c.Generation, f.Manifest.Source, f.Manifest.Chain, c.Index, sealed)
	if err != nil {
		t.Fatalf("opening the Rust-sealed chunk: %v", err)
	}
	want, _ := base64.StdEncoding.DecodeString(f.Plaintext)
	if string(plain) != string(want) {
		t.Fatalf("plaintext = %q, want %q", plain, want)
	}
	sum := sha256.Sum256(plain)
	if hex.EncodeToString(sum[:]) != c.Sha256Plain {
		t.Fatalf("digest mismatch")
	}
}
