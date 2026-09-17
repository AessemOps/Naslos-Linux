package buddy

import (
	"errors"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

// TestQuotaIsAtomicUnderConcurrentWrites is the NAS-013 regression: the check and
// the charge happen under one lock, so racing uploads cannot all pass a check that
// was true only before any of them wrote.
func TestQuotaIsAtomicUnderConcurrentWrites(t *testing.T) {
	store := NewStore(t.TempDir())
	payload := randomBytes(t, 4096)
	quota := int64(len(payload) * 3) // room for exactly three chunks

	var wg sync.WaitGroup
	var stored, refused int64
	for index := 0; index < 12; index++ {
		wg.Add(1)
		go func(index int) {
			defer wg.Done()
			err := store.PutChunkWithin("key", "naslos-a/data", "chain1", index, payload, quota)
			switch {
			case err == nil:
				atomic.AddInt64(&stored, 1)
			case errors.Is(err, ErrQuotaExceeded):
				atomic.AddInt64(&refused, 1)
			default:
				t.Errorf("unexpected error for chunk %d: %v", index, err)
			}
		}(index)
	}
	wg.Wait()

	if stored > 3 {
		t.Errorf("%d chunks were stored with room for only 3", stored)
	}
	if refused == 0 {
		t.Error("no write was refused: the quota was not enforced")
	}
	used, err := store.Usage("key")
	if err != nil {
		t.Fatalf("usage: %v", err)
	}
	if used > quota {
		t.Errorf("usage = %d bytes, want at most the quota %d", used, quota)
	}
}

// TestQuotaStillAllowsResumingAnIdenticalChunk pins the other half: a chunk that is
// already stored costs nothing, so resuming a chain that already fills the quota
// works. The old pre-check charged the re-upload and answered 413.
func TestQuotaStillAllowsResumingAnIdenticalChunk(t *testing.T) {
	store := NewStore(t.TempDir())
	payload := randomBytes(t, 4096)
	quota := int64(len(payload)) // exactly one chunk of room

	if err := store.PutChunkWithin("key", "naslos-a/data", "chain1", 0, payload, quota); err != nil {
		t.Fatalf("first upload: %v", err)
	}
	if err := store.PutChunkWithin("key", "naslos-a/data", "chain1", 0, payload, quota); err != nil {
		t.Fatalf("re-uploading an identical chunk at the quota was refused: %v", err)
	}

	used, err := store.Usage("key")
	if err != nil {
		t.Fatalf("usage: %v", err)
	}
	if used > quota {
		t.Errorf("usage = %d, want at most %d", used, quota)
	}
}

// TestQuotaRefusesAnOversizedWrite covers the single-chunk case: the write is
// refused and leaves nothing behind.
func TestQuotaRefusesAnOversizedWrite(t *testing.T) {
	store := NewStore(t.TempDir())
	err := store.PutChunkWithin("key", "naslos-a/data", "chain1", 0, randomBytes(t, 8192), 4096)
	if !errors.Is(err, ErrQuotaExceeded) {
		t.Fatalf("error = %v, want a quota refusal", err)
	}

	chunks, err := store.ListChunks("key", "naslos-a/data", "chain1")
	if err != nil {
		t.Fatalf("listing chunks: %v", err)
	}
	if len(chunks) != 0 {
		t.Errorf("a refused write stored %d chunk(s)", len(chunks))
	}
}

// TestManifestBytesCountTowardUsage: the manifest is stored bytes too, and the
// running total used to miss it until a restart recomputed from disk.
func TestManifestBytesCountTowardUsage(t *testing.T) {
	store := NewStore(t.TempDir())
	payload := randomBytes(t, 2048)
	if err := store.PutChunk("key", "naslos-a/data", "chain1", 0, payload); err != nil {
		t.Fatalf("put chunk: %v", err)
	}
	before, err := store.Usage("key")
	if err != nil {
		t.Fatalf("usage: %v", err)
	}

	manifest := &Manifest{
		Version:        EnvelopeVersion,
		Source:         "naslos-a/data",
		Chain:          "chain1",
		Kind:           "tar",
		CreatedAt:      time.Now().UTC(),
		StreamPrefix:   "AAAAAAAAAAA=",
		ChunkPlainSize: ChunkPlainSize,
		DEKWrapped:     "AAAA",
		Chunks:         []ManifestChunk{{Index: 0, PlainBytes: len(payload), SealedBytes: len(payload), Sha256Plain: "aa"}},
	}
	if err := store.PutManifest("key", "naslos-a/data", manifest); err != nil {
		t.Fatalf("put manifest: %v", err)
	}
	store.InvalidateUsage("key")

	after, err := store.Usage("key")
	if err != nil {
		t.Fatalf("usage after: %v", err)
	}
	if after <= before {
		t.Errorf("usage = %d after storing a manifest, want more than the %d bytes of chunks", after, before)
	}
}

// TestValidateQuota keeps a peer's quota plausible: 0 is the documented
// "unlimited", a negative or sub-chunk value is a trap, and an absurd one is
// almost certainly a unit mistake.
func TestValidateQuota(t *testing.T) {
	for _, bad := range []int64{-1, 1, MinPeerQuota - 1, MaxPeerQuota + 1} {
		if err := ValidateQuota(bad); err == nil {
			t.Errorf("ValidateQuota(%d) = nil, want a rejection", bad)
		}
	}
	for _, good := range []int64{0, MinPeerQuota, MinPeerQuota * 4, MaxPeerQuota} {
		if err := ValidateQuota(good); err != nil {
			t.Errorf("ValidateQuota(%d) = %v, want it accepted", good, err)
		}
	}
}
