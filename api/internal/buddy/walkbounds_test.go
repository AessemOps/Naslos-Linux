package buddy

import (
	"bytes"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"
	"time"
)

// TestSequenceFollowsTheGuidIndex pins NAS-021's restore path: the sequence is
// followed through the receiver's own GUID index (one manifest read per step)
// rather than by reading every chain the source ever stored.
func TestSequenceFollowsTheGuidIndex(t *testing.T) {
	receiver := newTestReceiver(t, "")
	sender := newTestIdentity(t, "naslos-a")
	receiver.authorize(t, sender, nil, 0)
	client := receiver.client(sender)

	source := "naslos-a/history"
	data := randomBytes(t, 2048)

	first, err := client.Push(PushOptions{
		Source: source, Reader: bytes.NewReader(data), ToSnapshot: "s1", ToGUID: "1000",
	})
	if err != nil {
		t.Fatalf("full push: %v", err)
	}
	second, err := client.Push(PushOptions{
		Source: source, Reader: bytes.NewReader(data),
		FromSnapshot: "s1", ToSnapshot: "s2", FromGUID: "1000", ToGUID: "2000",
	})
	if err != nil {
		t.Fatalf("first incremental: %v", err)
	}
	third, err := client.Push(PushOptions{
		Source: source, Reader: bytes.NewReader(data),
		FromSnapshot: "s2", ToSnapshot: "s3", FromGUID: "2000", ToGUID: "3000",
	})
	if err != nil {
		t.Fatalf("second incremental: %v", err)
	}

	fingerprint, err := sender.Fingerprint()
	if err != nil {
		t.Fatalf("fingerprint: %v", err)
	}

	// The index exists and maps each base GUID to the chain that produced it.
	links := receiver.store.readLinks(fingerprint, source)
	if links["3000"] != third.Chain || links["2000"] != second.Chain || links["1000"] != first.Chain {
		t.Fatalf("links = %v, want an entry per chain", links)
	}

	sequence, err := receiver.store.Sequence(fingerprint, source, "")
	if err != nil {
		t.Fatalf("Sequence: %v", err)
	}
	if len(sequence) != 3 {
		t.Fatalf("sequence = %d chains, want 3", len(sequence))
	}
	// Oldest first: the full send, then the incrementals in order.
	if sequence[0].Chain != first.Chain || sequence[1].Chain != second.Chain || sequence[2].Chain != third.Chain {
		t.Errorf("sequence order = %s, %s, %s", sequence[0].Chain, sequence[1].Chain, sequence[2].Chain)
	}

	// A requested chain returns only what is needed to rebuild *it*.
	sequence, err = receiver.store.Sequence(fingerprint, source, second.Chain)
	if err != nil {
		t.Fatalf("Sequence(second): %v", err)
	}
	if len(sequence) != 2 || sequence[0].Chain != first.Chain || sequence[1].Chain != second.Chain {
		t.Errorf("sequence for the second chain = %+v, want the first and second", sequence)
	}

	// The index is a cache: losing it costs a scan, not correctness.
	sourceDir, err := receiver.store.sourceDir(fingerprint, source)
	if err != nil {
		t.Fatalf("source dir: %v", err)
	}
	if err := os.Remove(filepath.Join(sourceDir, "chains", linksFile)); err != nil {
		t.Fatalf("removing the index: %v", err)
	}
	sequence, err = receiver.store.Sequence(fingerprint, source, "")
	if err != nil {
		t.Fatalf("Sequence without an index: %v", err)
	}
	if len(sequence) != 3 {
		t.Errorf("sequence without an index = %d chains, want 3", len(sequence))
	}
	if links = receiver.store.readLinks(fingerprint, source); links["2000"] != second.Chain {
		t.Errorf("links = %v, want the index rebuilt from the scan", links)
	}
}

// TestSequenceRefusesWhenTheBaseIsGone keeps the promise that a restore never
// applies a partial stream: a missing base is an error up front.
func TestSequenceRefusesWhenTheBaseIsGone(t *testing.T) {
	receiver := newTestReceiver(t, "")
	sender := newTestIdentity(t, "naslos-a")
	receiver.authorize(t, sender, nil, 0)
	client := receiver.client(sender)

	source := "naslos-a/broken"
	data := randomBytes(t, 512)
	if _, err := client.Push(PushOptions{
		Source: source, Reader: bytes.NewReader(data), ToSnapshot: "s1", ToGUID: "1000",
	}); err != nil {
		t.Fatalf("full push: %v", err)
	}
	second, err := client.Push(PushOptions{
		Source: source, Reader: bytes.NewReader(data),
		FromSnapshot: "s1", ToSnapshot: "s2", FromGUID: "1000", ToGUID: "2000",
	})
	if err != nil {
		t.Fatalf("incremental push: %v", err)
	}

	// Simulate a lost base chain: its manifest is what the walk needs.
	fingerprint, err := sender.Fingerprint()
	if err != nil {
		t.Fatalf("fingerprint: %v", err)
	}
	chains, err := receiver.store.Chains(fingerprint, source)
	if err != nil {
		t.Fatalf("chains: %v", err)
	}
	var base string
	for _, chain := range chains {
		if chain.Chain != second.Chain {
			base = chain.Chain
		}
	}
	baseDir, err := receiver.store.chainDir(fingerprint, source, base)
	if err != nil {
		t.Fatalf("chain dir: %v", err)
	}
	if err := os.Remove(filepath.Join(baseDir, "manifest.json")); err != nil {
		t.Fatalf("removing the base manifest: %v", err)
	}

	if _, err := receiver.store.Sequence(fingerprint, source, ""); err == nil {
		t.Fatal("Sequence rebuilt a sequence whose base is missing")
	}
}

// TestChainsListingIsCapped bounds the listing work: a source with a long history
// must not make one request read every manifest, and the cap must be visible.
func TestChainsListingIsCapped(t *testing.T) {
	store := NewStore(t.TempDir())
	source := "naslos-a/many"

	const total = 12
	for i := 0; i < total; i++ {
		manifest := &Manifest{
			Version:        EnvelopeVersion,
			Source:         source,
			Chain:          "chain" + string(rune('a'+i)),
			Kind:           "tar",
			CreatedAt:      time.Now().UTC().Add(time.Duration(i) * time.Minute),
			StreamPrefix:   "AAAAAAAA",
			ChunkPlainSize: ChunkPlainSize,
			DEKWrapped:     "AAAA",
			ToGUID:         "guid" + string(rune('a'+i)),
		}
		if err := store.PutManifest("key", source, manifest); err != nil {
			t.Fatalf("put manifest %d: %v", i, err)
		}
	}

	all, err := store.Chains("key", source)
	if err != nil {
		t.Fatalf("chains: %v", err)
	}
	if len(all) != total {
		t.Fatalf("chains = %d, want %d below the cap", len(all), total)
	}

	capped, err := store.ChainsLimited("key", source, 5)
	if err != nil {
		t.Fatalf("chains limited: %v", err)
	}
	if len(capped) != 5 {
		t.Fatalf("capped listing = %d chains, want 5", len(capped))
	}
	// Newest first, and the cap takes the newest ones.
	if !capped[0].CreatedAt.After(capped[4].CreatedAt) {
		t.Error("the capped listing is not newest-first")
	}
	if capped[0].Chain != all[0].Chain {
		t.Errorf("capped[0] = %s, want the newest chain %s", capped[0].Chain, all[0].Chain)
	}

	count, err := store.ChainCount("key", source)
	if err != nil {
		t.Fatalf("chain count: %v", err)
	}
	if count != total {
		t.Errorf("count = %d, want the real total %d", count, total)
	}
}

// TestRestoreSequenceFallsBackToTheListing covers a receiver that predates the
// /sequence endpoint: the client must still be able to rebuild a sequence.
func TestRestoreSequenceFallsBackToTheListing(t *testing.T) {
	// A receiver-shaped stub: /chains answers, /sequence does not exist.
	mux := http.NewServeMux()
	mux.HandleFunc(PathPrefix+"/chains/", func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"source":"naslos-a/old","chains":[
			{"chain":"newer","fromGUID":"1000","toGUID":"2000"},
			{"chain":"base","toGUID":"1000"}
		]}`))
	})
	server := httptest.NewServer(mux)
	defer server.Close()

	client := NewClient(server.URL, newTestIdentity(t, "naslos-a"))
	sequence, err := client.RestoreSequenceContext(t.Context(), "naslos-a/old", "")
	if err != nil {
		t.Fatalf("RestoreSequenceContext with an old receiver: %v", err)
	}
	if len(sequence) != 2 || sequence[0].Chain != "base" || sequence[1].Chain != "newer" {
		t.Errorf("sequence = %+v, want the base then the incremental", sequence)
	}
}

// TestSequenceEndpointIsReachable exercises the receiver route through the client.
func TestSequenceEndpointIsReachable(t *testing.T) {
	receiver := newTestReceiver(t, "")
	sender := newTestIdentity(t, "naslos-a")
	receiver.authorize(t, sender, nil, 0)
	client := receiver.client(sender)

	source := "naslos-a/endpoint"
	data := randomBytes(t, 256)
	if _, err := client.Push(PushOptions{Source: source, Reader: bytes.NewReader(data), ToGUID: "42"}); err != nil {
		t.Fatalf("push: %v", err)
	}

	sequence, err := client.RestoreSequenceContext(t.Context(), source, "")
	if err != nil {
		t.Fatalf("RestoreSequenceContext: %v", err)
	}
	if len(sequence) != 1 {
		t.Fatalf("sequence = %d chains, want 1", len(sequence))
	}
	if sequence[0].ToGUID != "42" {
		t.Errorf("sequence[0].ToGUID = %q, want the pushed GUID", sequence[0].ToGUID)
	}
}
