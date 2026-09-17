package buddy

import (
	"testing"
	"time"
)

// TestSourcesInFindsNestedSources is the NAS-021 regression: a three-level source
// was invisible to listings (the walk stopped after two levels) while the quota
// still counted its bytes.
func TestSourcesInFindsNestedSources(t *testing.T) {
	store := NewStore(t.TempDir())
	nested := "naslos-a/media/films"

	if err := store.PutChunk("key", nested, "chain1", 0, randomBytes(t, 256)); err != nil {
		t.Fatalf("put chunk: %v", err)
	}
	manifest := &Manifest{
		Version:        EnvelopeVersion,
		Source:         nested,
		Chain:          "chain1",
		Kind:           "tar",
		CreatedAt:      time.Now().UTC(),
		StreamPrefix:   "AAAAAAAAAAA=",
		ChunkPlainSize: ChunkPlainSize,
		DEKWrapped:     "AAAA",
	}
	if err := store.PutManifest("key", nested, manifest); err != nil {
		t.Fatalf("put manifest: %v", err)
	}

	used, err := store.Usage("key")
	if err != nil {
		t.Fatalf("usage: %v", err)
	}
	if used == 0 {
		t.Error("usage did not count the nested source")
	}

	summary, err := store.Summary("key")
	if err != nil {
		t.Fatalf("summary: %v", err)
	}
	found := false
	for _, row := range summary {
		if row.Source == nested {
			found = true
		}
	}
	if !found {
		t.Errorf("summary = %+v, want it to list the nested source %q", summary, nested)
	}
}

// TestChunkIndexOverflowIsRefused keeps the 32-bit nonce counter honest: an index
// past it would wrap and reuse a nonce, which GCM must never see.
func TestChunkIndexOverflowIsRefused(t *testing.T) {
	dek := randomBytes(t, 32)
	prefix := randomBytes(t, 8)

	if _, _, err := SealChunk(dek, prefix, "naslos-a/data", "chain1", int(MaxChunkIndex)+1, []byte("x")); err == nil {
		t.Error("SealChunk accepted an index past the counter")
	}
	if _, err := OpenChunk(dek, "naslos-a/data", "chain1", -1, []byte("not an envelope")); err == nil {
		t.Error("OpenChunk accepted a negative index")
	}
	if _, _, err := SealChunk(dek, prefix, "naslos-a/data", "chain1", int(MaxChunkIndex), []byte("x")); err != nil {
		t.Errorf("SealChunk at the largest index = %v, want it accepted", err)
	}
}
