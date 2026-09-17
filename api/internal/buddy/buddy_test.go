package buddy

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"
)

// testReceiver is the fake-peer harness: a real receiver (the same handlers,
// authenticator and store the instance runs) behind an httptest server, so the
// tests exercise the protocol over HTTP rather than calling internals.
type testReceiver struct {
	server *httptest.Server
	store  *Store
	peers  *PeerStore
	dir    string
	token  string
}

// newTestReceiver builds a receiver rooted in a temp dir.
func newTestReceiver(t *testing.T, token string) *testReceiver {
	t.Helper()

	dir := t.TempDir()
	store := NewStore(filepath.Join(dir, "data"))
	peers := NewPeerStore(filepath.Join(dir, "peers.json"))
	if err := peers.Load(); err != nil {
		t.Fatalf("loading peer store: %v", err)
	}

	receiver := &Receiver{
		Store:       store,
		Peers:       peers,
		Auth:        NewAuthenticator(peers),
		Name:        "test-receiver",
		Version:     "test",
		EnrollToken: token,
	}
	server := httptest.NewServer(receiver.Handler())
	t.Cleanup(server.Close)

	return &testReceiver{server: server, store: store, peers: peers, dir: dir, token: token}
}

// authorize registers an identity's public key directly (the out-of-band path).
func (r *testReceiver) authorize(t *testing.T, id *Identity, sources []string, quota int64) *Peer {
	t.Helper()

	peer := &Peer{
		Name:           id.Name,
		PublicKey:      id.PublicKey,
		AllowedSources: sources,
		QuotaBytes:     quota,
		Enabled:        true,
	}
	if err := r.peers.Add(peer); err != nil {
		t.Fatalf("authorizing %s: %v", id.Name, err)
	}
	return peer
}

// client returns a sender pointed at the receiver.
func (r *testReceiver) client(id *Identity) *Client {
	return NewClient(r.server.URL, id)
}

// newTestIdentity creates an identity in memory (no key file needed).
func newTestIdentity(t *testing.T, name string) *Identity {
	t.Helper()

	id, err := NewIdentity(name)
	if err != nil {
		t.Fatalf("creating identity: %v", err)
	}
	return id
}

// randomBytes returns n random bytes.
func randomBytes(t *testing.T, n int) []byte {
	t.Helper()

	data := make([]byte, n)
	if _, err := rand.Read(data); err != nil {
		t.Fatalf("generating test data: %v", err)
	}
	return data
}

func TestIdentityRoundTrip(t *testing.T) {
	id := newTestIdentity(t, "naslos-a")

	path := filepath.Join(t.TempDir(), "identity.json")
	if err := id.Save(path); err != nil {
		t.Fatalf("saving identity: %v", err)
	}

	info, err := os.Stat(path)
	if err != nil {
		t.Fatalf("stat identity: %v", err)
	}
	if perm := info.Mode().Perm(); perm != 0600 {
		t.Errorf("identity permissions are %o, want 600: a private key must not be world readable", perm)
	}

	loaded, err := LoadIdentity(path)
	if err != nil {
		t.Fatalf("loading identity: %v", err)
	}
	if loaded.PublicKey != id.PublicKey {
		t.Error("public key changed across save/load")
	}

	fingerprint, err := loaded.Fingerprint()
	if err != nil {
		t.Fatalf("fingerprint: %v", err)
	}
	if !strings.HasPrefix(fingerprint, "SHA256:") {
		t.Errorf("fingerprint %q is not ssh-style", fingerprint)
	}

	// The signature must verify with the public key alone, and must not verify
	// against a different message.
	sig, err := loaded.Sign([]byte("payload"))
	if err != nil {
		t.Fatalf("signing: %v", err)
	}
	if err := Verify(loaded.PublicKey, []byte("payload"), sig); err != nil {
		t.Fatalf("verifying own signature: %v", err)
	}
	if err := Verify(loaded.PublicKey, []byte("other payload"), sig); err == nil {
		t.Error("signature verified against a different message")
	}
}

func TestEnvelopeRejectsTampering(t *testing.T) {
	dek := randomBytes(t, 32)
	prefix := randomBytes(t, 8)
	plain := []byte("the quick brown fox jumps over the lazy dog")

	sealed, sha, err := SealChunk(dek, prefix, "naslos-a/data", "chain1", 0, plain)
	if err != nil {
		t.Fatalf("sealing: %v", err)
	}
	if sha != digestOf(plain) {
		t.Error("seal returned the wrong plaintext digest")
	}

	opened, err := OpenChunk(dek, "naslos-a/data", "chain1", 0, sealed)
	if err != nil {
		t.Fatalf("opening: %v", err)
	}
	if !bytes.Equal(opened, plain) {
		t.Error("round trip changed the data")
	}

	cases := []struct {
		name    string
		mutate  func([]byte) []byte
		atIndex int
	}{
		{"flipped ciphertext bit", func(in []byte) []byte {
			out := append([]byte(nil), in...)
			out[len(out)-1] ^= 0x01
			return out
		}, 0},
		{"truncated", func(in []byte) []byte { return in[:len(in)-4] }, 0},
		{"wrong chunk index", func(in []byte) []byte { return in }, 1},
		{"bad magic", func(in []byte) []byte {
			out := append([]byte(nil), in...)
			out[0] = 'X'
			return out
		}, 0},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if _, err := OpenChunk(dek, "naslos-a/data", "chain1", tc.atIndex, tc.mutate(sealed)); err == nil {
				t.Error("tampered chunk opened without error")
			}
		})
	}

	// A chunk moved to another source or chain must not open either: the AAD
	// binds it to its position.
	if _, err := OpenChunk(dek, "naslos-b/data", "chain1", 0, sealed); err == nil {
		t.Error("chunk opened under a different source")
	}
	if _, err := OpenChunk(dek, "naslos-a/data", "chain2", 0, sealed); err == nil {
		t.Error("chunk opened under a different chain")
	}
}

func TestDEKWrapNeedsTheOwnersKEK(t *testing.T) {
	owner := newTestIdentity(t, "naslos-a")
	stranger := newTestIdentity(t, "nosy-receiver")

	dek := randomBytes(t, 32)
	kek, err := owner.KEKBytes()
	if err != nil {
		t.Fatalf("owner kek: %v", err)
	}
	wrapped, err := WrapDEK(kek, dek, "naslos-a/data", "chain1")
	if err != nil {
		t.Fatalf("wrapping: %v", err)
	}

	unwrapped, err := UnwrapDEK(kek, wrapped, "naslos-a/data", "chain1")
	if err != nil {
		t.Fatalf("unwrapping: %v", err)
	}
	if !bytes.Equal(unwrapped, dek) {
		t.Error("unwrapped key differs from the original")
	}

	// The receiver's KEK is a different secret: it must fail, which is the whole
	// zero-knowledge property.
	strangerKEK, err := stranger.KEKBytes()
	if err != nil {
		t.Fatalf("stranger kek: %v", err)
	}
	if _, err := UnwrapDEK(strangerKEK, wrapped, "naslos-a/data", "chain1"); err == nil {
		t.Error("a stranger's key unwrapped the data key")
	}
	// Binding the wrapped key to its chain prevents moving it around.
	if _, err := UnwrapDEK(kek, wrapped, "naslos-a/data", "chain2"); err == nil {
		t.Error("wrapped key opened under a different chain")
	}
}

func TestManifestSignature(t *testing.T) {
	id := newTestIdentity(t, "naslos-a")
	manifest := &Manifest{
		Version:      EnvelopeVersion,
		Source:       "naslos-a/data",
		Chain:        "chain1",
		Kind:         "tar",
		CreatedAt:    time.Now().UTC(),
		Chunks:       []ManifestChunk{{Index: 0, PlainBytes: 10, SealedBytes: 46, Sha256Plain: digestOf([]byte("0123456789"))}},
		DEKWrapped:   base64.StdEncoding.EncodeToString(randomBytes(t, 60)),
		StreamPrefix: base64.StdEncoding.EncodeToString(randomBytes(t, 8)),
	}
	if err := manifest.Sign(id); err != nil {
		t.Fatalf("signing manifest: %v", err)
	}
	if err := manifest.VerifySignature(id.PublicKey); err != nil {
		t.Fatalf("verifying manifest: %v", err)
	}

	// Editing the metadata after signing must invalidate it: this is what stops a
	// receiver from lying about what it stored.
	tampered := *manifest
	tampered.Chunks = []ManifestChunk{{Index: 0, PlainBytes: 10, SealedBytes: 46, Sha256Plain: digestOf([]byte("9999999999"))}}
	if err := tampered.VerifySignature(id.PublicKey); err == nil {
		t.Error("a tampered manifest still verified")
	}

	other := newTestIdentity(t, "naslos-b")
	if err := manifest.VerifySignature(other.PublicKey); err == nil {
		t.Error("a manifest verified against another identity's key")
	}
}

// failAfterReader serves data up to limit bytes and then fails, standing in for a
// dropped connection or a killed process mid-push.
type failAfterReader struct {
	data  []byte
	limit int
	pos   int
}

func (r *failAfterReader) Read(p []byte) (int, error) {
	if r.pos >= r.limit {
		return 0, errSimulatedInterruption
	}
	end := r.limit
	if end > len(r.data) {
		end = len(r.data)
	}
	n := copy(p, r.data[r.pos:end])
	r.pos += n
	if n == 0 {
		return 0, errSimulatedInterruption
	}
	return n, nil
}

var errSimulatedInterruption = errors.New("simulated connection loss")

// assertNoPlaintext walks the receiver's storage and fails if any of it contains
// the plaintext needle. It is the check that backs the "the receiver cannot read
// your backups" claim.
func assertNoPlaintext(t *testing.T, dir string, needle []byte) {
	t.Helper()

	err := filepath.Walk(dir, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return err
		}
		if !info.Mode().IsRegular() {
			return nil
		}
		content, err := os.ReadFile(path)
		if err != nil {
			return err
		}
		if bytes.Contains(content, needle) {
			t.Errorf("plaintext leaked into %s", path)
		}
		return nil
	})
	if err != nil {
		t.Fatalf("walking receiver storage: %v", err)
	}
}

func TestPushRestoreRoundTrip(t *testing.T) {
	receiver := newTestReceiver(t, "")
	sender := newTestIdentity(t, "naslos-a")
	receiver.authorize(t, sender, nil, 0)
	client := receiver.client(sender)

	source := "naslos-a/data"
	// Deliberately not a whole number of chunks: the final partial chunk is the
	// case a naive implementation gets wrong.
	data := randomBytes(t, ChunkPlainSize*2+1234)

	result, err := client.Push(PushOptions{Source: source, Kind: "tar", Reader: bytes.NewReader(data)})
	if err != nil {
		t.Fatalf("push: %v", err)
	}
	if result.Chunks != 3 {
		t.Errorf("chunks = %d, want 3", result.Chunks)
	}
	if result.Uploaded != 3 || result.Skipped != 0 {
		t.Errorf("uploaded/skipped = %d/%d, want 3/0", result.Uploaded, result.Skipped)
	}
	if result.PlainBytes != int64(len(data)) {
		t.Errorf("plain bytes = %d, want %d", result.PlainBytes, len(data))
	}

	// The receiver stores ciphertext only.
	assertNoPlaintext(t, receiver.dir, data[:64])

	status, err := client.Status()
	if err != nil {
		t.Fatalf("status: %v", err)
	}
	if status.FreeBytes <= 0 {
		t.Errorf("receiver reported %d free bytes", status.FreeBytes)
	}
	if status.UsedBytes <= 0 {
		t.Errorf("receiver reported %d used bytes after a push", status.UsedBytes)
	}
	if status.LastBackup == nil {
		t.Error("status did not report a last backup time")
	}
	if len(status.Sources) != 1 || status.Sources[0] != source {
		t.Errorf("status sources = %v, want [%s]", status.Sources, source)
	}
	if status.Chains[source] != result.Chain {
		t.Errorf("status chain = %q, want %q", status.Chains[source], result.Chain)
	}

	var restored bytes.Buffer
	restoreResult, err := client.Restore(RestoreOptions{Source: source, Out: &restored})
	if err != nil {
		t.Fatalf("restore: %v", err)
	}
	if !bytes.Equal(restored.Bytes(), data) {
		t.Error("restored bytes differ from the original")
	}
	if restoreResult.Chunks != 3 || restoreResult.Chain != result.Chain {
		t.Errorf("restore = %d chunks on chain %q, want 3 on %q", restoreResult.Chunks, restoreResult.Chain, result.Chain)
	}

	backups, err := client.Backups()
	if err != nil {
		t.Fatalf("backups: %v", err)
	}
	if len(backups) != 1 || backups[0].Source != source {
		t.Fatalf("backups = %+v, want one row for %s", backups, source)
	}
	if backups[0].StoredBytes <= 0 {
		t.Error("backups row reported no stored bytes")
	}
}

// signedRequest builds a request signed with an explicit timestamp and nonce, so
// tests can exercise clock skew and replay outside the normal signing path.
func signedRequest(t *testing.T, id *Identity, url, method string, body []byte, timestamp int64, nonce string) *http.Request {
	t.Helper()

	req, err := http.NewRequest(method, url, bytes.NewReader(body))
	if err != nil {
		t.Fatalf("building request: %v", err)
	}
	stamp := strconv.FormatInt(timestamp, 10)
	canonical := CanonicalRequest(method, req.URL.RequestURI(), BodyDigest(body), stamp, nonce)
	sig, err := id.Sign([]byte(canonical))
	if err != nil {
		t.Fatalf("signing: %v", err)
	}
	fingerprint, err := id.Fingerprint()
	if err != nil {
		t.Fatalf("fingerprint: %v", err)
	}
	req.Header.Set(HeaderKeyID, fingerprint)
	req.Header.Set(HeaderTimestamp, stamp)
	req.Header.Set(HeaderNonce, nonce)
	req.Header.Set(HeaderSignature, base64.StdEncoding.EncodeToString(sig))
	return req
}

func TestPushResumeUploadsOnlyMissingChunks(t *testing.T) {
	receiver := newTestReceiver(t, "")
	sender := newTestIdentity(t, "naslos-a")
	receiver.authorize(t, sender, nil, 0)
	client := receiver.client(sender)

	source := "naslos-a/big"
	data := randomBytes(t, ChunkPlainSize*3)

	state, err := NewChainState(source, "zfs-send")
	if err != nil {
		t.Fatalf("chain state: %v", err)
	}

	// First attempt dies after two chunks.
	interrupted := &failAfterReader{data: data, limit: ChunkPlainSize * 2}
	if _, err := client.Push(PushOptions{Source: source, Kind: "zfs-send", Reader: interrupted, State: state}); err == nil {
		t.Fatal("the interrupted push reported success")
	}

	// Resuming must reuse the chain and only move what is missing.
	result, err := client.Push(PushOptions{Source: source, State: state, Reader: bytes.NewReader(data)})
	if err != nil {
		t.Fatalf("resume: %v", err)
	}
	if result.Uploaded != 1 {
		t.Errorf("resume uploaded %d chunks, want 1", result.Uploaded)
	}
	if result.Skipped != 2 {
		t.Errorf("resume skipped %d chunks, want 2", result.Skipped)
	}
	if result.Skipped+result.Uploaded != result.Chunks {
		t.Errorf("accounting mismatch: %d skipped + %d uploaded != %d chunks", result.Skipped, result.Uploaded, result.Chunks)
	}

	var restored bytes.Buffer
	if _, err := client.Restore(RestoreOptions{Source: source, Out: &restored}); err != nil {
		t.Fatalf("restore after resume: %v", err)
	}
	if !bytes.Equal(restored.Bytes(), data) {
		t.Error("restored data differs after a resume")
	}

	// Resuming the same chain with different data must fail instead of mixing two
	// versions into one chain.
	changed := randomBytes(t, ChunkPlainSize*3)
	_, err = client.Push(PushOptions{Source: source, State: state, Reader: bytes.NewReader(changed)})
	if err == nil {
		t.Fatal("resuming a chain with different data succeeded")
	}
	if !strings.Contains(err.Error(), "different bytes") {
		t.Errorf("unexpected error: %v", err)
	}
}

func TestReceiverRefusesTamperedChunk(t *testing.T) {
	receiver := newTestReceiver(t, "")
	sender := newTestIdentity(t, "naslos-a")
	receiver.authorize(t, sender, nil, 0)
	client := receiver.client(sender)

	source := "naslos-a/data"
	data := randomBytes(t, ChunkPlainSize)
	if _, err := client.Push(PushOptions{Source: source, Reader: bytes.NewReader(data)}); err != nil {
		t.Fatalf("push: %v", err)
	}

	// Corrupt the stored ciphertext the way a failing disk would.
	var target string
	err := filepath.Walk(receiver.dir, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return err
		}
		if info.Mode().IsRegular() && strings.HasSuffix(path, "chunk-000000.enc") {
			target = path
		}
		return nil
	})
	if err != nil || target == "" {
		t.Fatalf("could not find the stored chunk (target=%q err=%v)", target, err)
	}
	content, err := os.ReadFile(target)
	if err != nil {
		t.Fatalf("reading chunk: %v", err)
	}
	content[len(content)-1] ^= 0xff
	if err := os.WriteFile(target, content, 0644); err != nil {
		t.Fatalf("corrupting chunk: %v", err)
	}

	var out bytes.Buffer
	if _, err := client.Restore(RestoreOptions{Source: source, Out: &out}); err == nil {
		t.Fatal("restore succeeded from a corrupted chunk")
	} else if !strings.Contains(err.Error(), "authentication") {
		t.Errorf("unexpected restore error: %v", err)
	}
}

func TestScopeAndQuota(t *testing.T) {
	t.Run("scope", func(t *testing.T) {
		receiver := newTestReceiver(t, "")
		sender := newTestIdentity(t, "naslos-a")
		receiver.authorize(t, sender, []string{"naslos-a/data"}, 0)
		client := receiver.client(sender)

		_, err := client.Push(PushOptions{Source: "naslos-a/other", Reader: bytes.NewReader(randomBytes(t, 64))})
		if err == nil {
			t.Fatal("a scoped key pushed outside its scope")
		}
		if !strings.Contains(err.Error(), "not allowed") {
			t.Errorf("unexpected error: %v", err)
		}

		if _, err := client.Push(PushOptions{Source: "naslos-a/data", Reader: bytes.NewReader(randomBytes(t, 64))}); err != nil {
			t.Fatalf("push inside the scope failed: %v", err)
		}
	})

	t.Run("quota", func(t *testing.T) {
		receiver := newTestReceiver(t, "")
		sender := newTestIdentity(t, "naslos-a")
		// One and a half chunks of room: the first chunk fits, the second does not.
		receiver.authorize(t, sender, nil, int64(ChunkPlainSize)+ChunkPlainSize/2)
		client := receiver.client(sender)

		_, err := client.Push(PushOptions{Source: "naslos-a/data", Reader: bytes.NewReader(randomBytes(t, ChunkPlainSize*2))})
		if err == nil {
			t.Fatal("a push beyond the quota succeeded")
		}
		if !strings.Contains(err.Error(), "quota exceeded") {
			t.Errorf("unexpected error: %v", err)
		}
	})
}

func TestRequestAuthentication(t *testing.T) {
	receiver := newTestReceiver(t, "")
	sender := newTestIdentity(t, "naslos-a")
	receiver.authorize(t, sender, nil, 0)
	statusURL := receiver.server.URL + PathPrefix + "/status"

	t.Run("unsigned", func(t *testing.T) {
		resp, err := http.Get(statusURL)
		if err != nil {
			t.Fatalf("request: %v", err)
		}
		defer resp.Body.Close()
		if resp.StatusCode != http.StatusUnauthorized {
			t.Errorf("unsigned request got %d, want 401", resp.StatusCode)
		}
	})

	t.Run("unknown key", func(t *testing.T) {
		stranger := newTestIdentity(t, "not-authorized")
		nonce, err := randomNonce()
		if err != nil {
			t.Fatalf("generating nonce: %v", err)
		}
		req := signedRequest(t, stranger, statusURL, http.MethodGet, nil, time.Now().Unix(), nonce)
		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatalf("request: %v", err)
		}
		defer resp.Body.Close()
		if resp.StatusCode != http.StatusUnauthorized {
			t.Errorf("unauthorized key got %d, want 401", resp.StatusCode)
		}
	})

	t.Run("stale timestamp", func(t *testing.T) {
		// Signed correctly, but an hour old: a captured request must not stay
		// valid forever.
		nonce, err := randomNonce()
		if err != nil {
			t.Fatalf("generating nonce: %v", err)
		}
		req := signedRequest(t, sender, statusURL, http.MethodGet, nil, time.Now().Add(-time.Hour).Unix(), nonce)
		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatalf("request: %v", err)
		}
		defer resp.Body.Close()
		if resp.StatusCode != http.StatusUnauthorized {
			t.Errorf("stale request got %d, want 401", resp.StatusCode)
		}
	})

	t.Run("replay", func(t *testing.T) {
		timestamp := time.Now().Unix()
		nonce, err := randomNonce()
		if err != nil {
			t.Fatalf("generating nonce: %v", err)
		}
		first := signedRequest(t, sender, statusURL, http.MethodGet, nil, timestamp, nonce)
		resp, err := http.DefaultClient.Do(first)
		if err != nil {
			t.Fatalf("first request: %v", err)
		}
		resp.Body.Close()
		if resp.StatusCode != http.StatusOK {
			t.Fatalf("first request got %d, want 200", resp.StatusCode)
		}

		second := signedRequest(t, sender, statusURL, http.MethodGet, nil, timestamp, nonce)
		resp2, err := http.DefaultClient.Do(second)
		if err != nil {
			t.Fatalf("second request: %v", err)
		}
		defer resp2.Body.Close()
		if resp2.StatusCode != http.StatusUnauthorized {
			t.Errorf("replayed request got %d, want 401", resp2.StatusCode)
		}
		body, _ := io.ReadAll(resp2.Body)
		if !strings.Contains(string(body), "already used") {
			t.Errorf("replay was rejected for the wrong reason: %s", body)
		}
	})
}

func TestEnrollAuthorizesTheFirstKey(t *testing.T) {
	receiver := newTestReceiver(t, "one-time-token")

	t.Run("wrong token", func(t *testing.T) {
		sender := newTestIdentity(t, "naslos-a")
		if _, err := receiver.client(sender).Enroll("not-the-token", "naslos-a", nil); err == nil {
			t.Error("enrollment with a wrong token succeeded")
		}
	})

	sender := newTestIdentity(t, "naslos-a")
	if _, err := receiver.client(sender).Enroll("one-time-token", "naslos-a", []string{"naslos-a/"}); err != nil {
		t.Fatalf("enrollment: %v", err)
	}

	// The enrolled key can now work.
	client := receiver.client(sender)
	if _, err := client.Status(); err != nil {
		t.Fatalf("status after enrollment: %v", err)
	}
	if _, err := client.Push(PushOptions{Source: "naslos-a/data", Reader: bytes.NewReader(randomBytes(t, 128))}); err != nil {
		t.Fatalf("push after enrollment: %v", err)
	}

	// A second key must not be able to use the same token: a leaked token would
	// otherwise authorize anyone forever.
	other := newTestIdentity(t, "naslos-c")
	if _, err := receiver.client(other).Enroll("one-time-token", "naslos-c", nil); err == nil {
		t.Error("a single-use enrollment token authorized a second key")
	}

	// And an enrollment on a receiver without a token is refused outright.
	closed := newTestReceiver(t, "")
	if _, err := closed.client(other).Enroll("anything", "naslos-c", nil); err == nil {
		t.Error("enrollment succeeded on a receiver that has it disabled")
	}
}

func TestRestoreSequenceFollowsTheChainLinks(t *testing.T) {
	receiver := newTestReceiver(t, "")
	sender := newTestIdentity(t, "naslos-a")
	receiver.authorize(t, sender, nil, 0)
	client := receiver.client(sender)
	source := "naslos-a/incremental"

	// A full send, then an incremental that names the first snapshot's GUID as its
	// base - the shape the instance-side sender writes.
	full, err := client.Push(PushOptions{
		Source: source, Kind: "zfs-send", Reader: bytes.NewReader(randomBytes(t, 512)),
		ToSnapshot: "buddy-1", ToGUID: "1000",
	})
	if err != nil {
		t.Fatalf("full push: %v", err)
	}
	delta, err := client.Push(PushOptions{
		Source: source, Kind: "zfs-send", Reader: bytes.NewReader(randomBytes(t, 128)),
		FromSnapshot: "buddy-1", FromGUID: "1000", ToSnapshot: "buddy-2", ToGUID: "2000",
	})
	if err != nil {
		t.Fatalf("incremental push: %v", err)
	}

	sequence, err := client.RestoreSequence(source, "")
	if err != nil {
		t.Fatalf("RestoreSequence: %v", err)
	}
	if len(sequence) != 2 {
		t.Fatalf("sequence = %d chains, want 2", len(sequence))
	}
	if sequence[0].Chain != full.Chain || sequence[1].Chain != delta.Chain {
		t.Errorf("sequence = [%s %s], want [full %s then incremental %s]",
			sequence[0].Chain, sequence[1].Chain, full.Chain, delta.Chain)
	}

	// Asking for the incremental alone must still pull in its base: an incremental
	// stream cannot be applied on its own.
	sequence, err = client.RestoreSequence(source, delta.Chain)
	if err != nil {
		t.Fatalf("RestoreSequence(%s): %v", delta.Chain, err)
	}
	if len(sequence) != 2 || sequence[0].Chain != full.Chain {
		t.Errorf("sequence for the incremental = %+v, want the full chain first", sequence)
	}

	// A chain whose base is missing cannot be rebuilt, and saying so beats applying
	// half a backup.
	if _, err := client.Push(PushOptions{
		Source: source, Kind: "zfs-send", Reader: bytes.NewReader(randomBytes(t, 64)),
		FromSnapshot: "buddy-9", FromGUID: "9999", ToSnapshot: "buddy-3", ToGUID: "3000",
	}); err != nil {
		t.Fatalf("orphan push: %v", err)
	}
	if _, err := client.RestoreSequence(source, ""); err == nil {
		t.Error("a chain with a missing base was accepted for restore")
	} else if !strings.Contains(err.Error(), "not stored here") {
		t.Errorf("unexpected error: %v", err)
	}
}

func TestChainsAreListedNewestFirst(t *testing.T) {
	receiver := newTestReceiver(t, "")
	sender := newTestIdentity(t, "naslos-a")
	receiver.authorize(t, sender, nil, 0)
	client := receiver.client(sender)
	source := "naslos-a/history"

	for i := 0; i < 3; i++ {
		if _, err := client.Push(PushOptions{Source: source, Reader: bytes.NewReader(randomBytes(t, 256))}); err != nil {
			t.Fatalf("push %d: %v", i, err)
		}
		// CreatedAt has second granularity in the manifest only when it is scrubbed;
		// the store sorts on the real timestamp, so a small wait keeps the order
		// deterministic.
		time.Sleep(2 * time.Millisecond)
	}

	chains, err := client.Chains(source)
	if err != nil {
		t.Fatalf("Chains: %v", err)
	}
	if len(chains) != 3 {
		t.Fatalf("chains = %d, want 3", len(chains))
	}
	for i := 1; i < len(chains); i++ {
		if chains[i-1].CreatedAt.Before(chains[i].CreatedAt) {
			t.Errorf("chains are not newest-first: %+v", chains)
		}
	}
	if chains[0].Chunks == 0 {
		t.Error("the chain summary does not report its chunks")
	}
}

// TestKeyDirHandlesFingerprintsWithSlashes pins the bug the first instance-side
// sender hit: a fingerprint is base64-derived and can contain '/', which used to
// split one key's tree into nested directories and made its backups unreachable from
// the listing endpoints.
func TestKeyDirHandlesFingerprintsWithSlashes(t *testing.T) {
	fingerprint := "SHA256:du3R5I6q+piUaqr61Y/NV5FZv6qk0yrOXaxY0/5pHbM"

	dir := keyDir(fingerprint)
	if strings.ContainsAny(dir, "/\\") {
		t.Fatalf("keyDir(%q) = %q, want a name with no path separators", fingerprint, dir)
	}
	if keyDir(fingerprint) != dir {
		t.Error("keyDir is not deterministic")
	}
	if keyDir(fingerprint) == keyDir("SHA256:du3R5I6q+piUaqr61Y/NV5FZv6qk0yrOXaxY0/5pHbN") {
		t.Error("two different fingerprints mapped to the same directory")
	}

	// The whole round trip has to work: a store that accepted such a key must still
	// be able to list what it holds.
	store := NewStore(t.TempDir())
	manifest := &Manifest{
		Version:   EnvelopeVersion,
		Source:    "naslos-a/data",
		Chain:     "c1",
		Kind:      "tar",
		CreatedAt: time.Now().UTC(),
		Chunks:    []ManifestChunk{{Index: 0, PlainBytes: 1, SealedBytes: 37, Sha256Plain: digestOf([]byte("x"))}},
	}
	if err := store.PutManifest(fingerprint, "naslos-a/data", manifest); err != nil {
		t.Fatalf("PutManifest: %v", err)
	}

	sources, err := store.Sources(fingerprint)
	if err != nil {
		t.Fatalf("Sources: %v", err)
	}
	if len(sources) != 1 || sources[0] != "naslos-a/data" {
		t.Errorf("Sources = %v, want [naslos-a/data]", sources)
	}

	summary, err := store.Summary("")
	if err != nil {
		t.Fatalf("Summary: %v", err)
	}
	if len(summary) != 1 || summary[0].Chain != "c1" {
		t.Errorf("Summary = %+v, want the stored chain", summary)
	}
}

func TestPruneKeepsNewestChains(t *testing.T) {
	receiver := newTestReceiver(t, "")
	sender := newTestIdentity(t, "naslos-a")
	receiver.authorize(t, sender, nil, 0)
	client := receiver.client(sender)

	source := "naslos-a/data"
	data := randomBytes(t, 4096)

	for i := 0; i < 3; i++ {
		if _, err := client.Push(PushOptions{Source: source, Reader: bytes.NewReader(data)}); err != nil {
			t.Fatalf("push %d: %v", i, err)
		}
	}
	current, err := client.Manifest(source, "")
	if err != nil {
		t.Fatalf("manifest: %v", err)
	}

	removed, err := client.Prune(source, 1)
	if err != nil {
		t.Fatalf("prune: %v", err)
	}
	if removed != 2 {
		t.Errorf("prune removed %d chains, want 2", removed)
	}

	// The current chain survives and still restores.
	var restored bytes.Buffer
	if _, err := client.Restore(RestoreOptions{Source: source, Out: &restored}); err != nil {
		t.Fatalf("restore after prune: %v", err)
	}
	if !bytes.Equal(restored.Bytes(), data) {
		t.Error("restored data differs after a prune")
	}

	chains := 0
	err = filepath.Walk(receiver.dir, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return err
		}
		if info.IsDir() && info.Name() == current.Chain {
			chains++
		}
		return nil
	})
	if err != nil {
		t.Fatalf("walking storage: %v", err)
	}
	if chains != 1 {
		t.Errorf("the pruned chain is still on disk (found %d copies of %s)", chains, current.Chain)
	}
}

// TestPruneNeverOrphansAnIncremental pins the retention rule: a keep-count must
// not delete a chain that the newest backup descends from. Before this, a
// schedule with pruneKeep=1 on an incremental chain reported success and pruned
// the base, leaving a backup that only failed later at verify/restore time.
func TestPruneNeverOrphansAnIncremental(t *testing.T) {
	receiver := newTestReceiver(t, "")
	sender := newTestIdentity(t, "naslos-a")
	receiver.authorize(t, sender, nil, 0)
	client := receiver.client(sender)

	source := "naslos-a/incremental"
	data := randomBytes(t, 4096)

	// A full chain, then an incremental that descends from it.
	if _, err := client.Push(PushOptions{
		Source: source, Reader: bytes.NewReader(data), ToSnapshot: "s1", ToGUID: "1000",
	}); err != nil {
		t.Fatalf("full push: %v", err)
	}
	if _, err := client.Push(PushOptions{
		Source: source, Reader: bytes.NewReader(data),
		FromSnapshot: "s1", ToSnapshot: "s2", FromGUID: "1000", ToGUID: "2000",
	}); err != nil {
		t.Fatalf("incremental push: %v", err)
	}

	// keep=1 would delete the base. The receiver must keep both instead: an
	// incremental without its base is not a backup.
	removed, err := client.Prune(source, 1)
	if err != nil {
		t.Fatalf("prune: %v", err)
	}
	if removed != 0 {
		t.Errorf("prune removed %d chains, want 0 (the base is required to restore)", removed)
	}

	sequence, err := client.RestoreSequence(source, "")
	if err != nil {
		t.Fatalf("restore sequence: %v", err)
	}
	if len(sequence) != 2 {
		t.Errorf("sequence = %d chains, want the full chain plus the incremental", len(sequence))
	}

	var restored bytes.Buffer
	if _, err := client.Restore(RestoreOptions{Source: source, Out: &restored}); err != nil {
		t.Fatalf("restore after prune: %v", err)
	}
	if !bytes.Equal(restored.Bytes(), data) {
		t.Error("restored data differs after a prune")
	}

	// With keep=2 the same graph prunes nothing either; with an independent
	// second chain, keep=1 prunes that one but still keeps the sequence.
	removed, err = client.Prune(source, 2)
	if err != nil {
		t.Fatalf("prune keep=2: %v", err)
	}
	if removed != 0 {
		t.Errorf("prune keep=2 removed %d chains, want 0", removed)
	}
}

// TestManifestLookupAbortsWhenTheContextIsCancelled covers the other half of
// prompt cancellation: the base-manifest lookup happens before any chunk, so it
// must be abortable too. The live drill found a cancel waiting out the receiver's
// whole stall here.
func TestManifestLookupAbortsWhenTheContextIsCancelled(t *testing.T) {
	reached := make(chan struct{})
	stall := make(chan struct{})

	var once sync.Once
	mux := http.NewServeMux()
	mux.HandleFunc(PathPrefix+"/manifest/", func(w http.ResponseWriter, _ *http.Request) {
		once.Do(func() { close(reached) })
		<-stall
	})
	server := httptest.NewServer(mux)
	defer func() {
		close(stall)
		server.Close()
	}()

	client := NewClient(server.URL, newTestIdentity(t, "naslos-a"))
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	go func() {
		<-reached
		cancel()
	}()

	started := time.Now()
	_, err := client.ManifestContext(ctx, "naslos-a/stalled", "")
	if err == nil {
		t.Fatal("ManifestContext returned a manifest after the context was cancelled")
	}
	if !errors.Is(err, context.Canceled) {
		t.Errorf("error = %v, want it to wrap context.Canceled", err)
	}
	if elapsed := time.Since(started); elapsed > 5*time.Second {
		t.Errorf("ManifestContext took %s to abort, want it bounded by the cancellation", elapsed)
	}
}

// TestPushAbortsWhenTheContextIsCancelled pins FR-BUD-16: a cancelled job must
// stop a running push *promptly*. Before PushOptions carried a context, a
// DELETE only took effect when the in-flight chunk request returned (the live
// drill measured the receiver's full stall, 30 s). Here the receiver stalls on
// the first chunk PUT and the test asserts Push returns as soon as the context
// is cancelled, not when the receiver answers.
func TestPushAbortsWhenTheContextIsCancelled(t *testing.T) {
	reached := make(chan struct{})
	stall := make(chan struct{})

	var once sync.Once
	mux := http.NewServeMux()
	mux.HandleFunc(PathPrefix+"/chunks/", func(w http.ResponseWriter, r *http.Request) {
		switch r.Method {
		case http.MethodGet:
			// The resume lookup: nothing stored yet, so every chunk is uploaded.
			w.Header().Set("Content-Type", "application/json")
			_, _ = w.Write([]byte(`{"published":false,"chunks":[]}`))
		case http.MethodPut:
			once.Do(func() { close(reached) })
			<-stall // hold the request until the test is done
		default:
			w.WriteHeader(http.StatusMethodNotAllowed)
		}
	})
	server := httptest.NewServer(mux)
	// Release the stalled handler *before* closing the server: Close waits for
	// outstanding requests, and the handler is blocked by design.
	defer func() {
		close(stall)
		server.Close()
	}()

	sender := newTestIdentity(t, "naslos-a")
	client := NewClient(server.URL, sender)

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	go func() {
		<-reached
		cancel()
	}()

	// Two chunks, so the loop would keep going after the first one.
	payload := bytes.Repeat([]byte("x"), ChunkPlainSize*2)
	started := time.Now()
	_, err := client.Push(PushOptions{
		Context: ctx,
		Source:  "naslos-a/cancel",
		Kind:    "zfs-send",
		Reader:  bytes.NewReader(payload),
	})

	if err == nil {
		t.Fatal("Push reported success after the context was cancelled")
	}
	if !errors.Is(err, context.Canceled) {
		t.Errorf("error = %v, want it to wrap context.Canceled", err)
	}
	if elapsed := time.Since(started); elapsed > 5*time.Second {
		t.Errorf("Push took %s to abort, want it bounded by the cancellation, not by the receiver", elapsed)
	}
}

// TestManifestShapeValidation pins the receiver-side checks on sender-controlled
// metadata (NAS-012): the receiver cannot decrypt, so refusing a malformed or
// self-inconsistent manifest is the only protection it has.
func TestManifestShapeValidation(t *testing.T) {
	valid := func() *Manifest {
		return &Manifest{
			Version:        EnvelopeVersion,
			Source:         "naslos-a/data",
			Chain:          "abcdef0123456789",
			Kind:           "zfs-send",
			CreatedAt:      time.Now().UTC(),
			StreamPrefix:   base64.StdEncoding.EncodeToString(randomBytes(t, 8)),
			ChunkPlainSize: ChunkPlainSize,
			DEKWrapped:     base64.StdEncoding.EncodeToString(randomBytes(t, 60)),
			Chunks: []ManifestChunk{
				{Index: 0, PlainBytes: ChunkPlainSize, SealedBytes: ChunkPlainSize + 28, Sha256Plain: strings.Repeat("a", 64)},
				{Index: 1, PlainBytes: 1024, SealedBytes: 1024 + 28, Sha256Plain: strings.Repeat("b", 64)},
			},
		}
	}
	if err := validateManifestShape(valid()); err != nil {
		t.Fatalf("validateManifestShape(valid) = %v, want nil", err)
	}

	cases := []struct {
		name   string
		mutate func(*Manifest)
		want   string
	}{
		{"unknown kind", func(m *Manifest) { m.Kind = "exec" }, "unsupported payload kind"},
		{"no creation time", func(m *Manifest) { m.CreatedAt = time.Time{} }, "creation time"},
		{"wrong chunk size", func(m *Manifest) { m.ChunkPlainSize = 4096 }, "chunk plain size"},
		{"bad stream prefix", func(m *Manifest) { m.StreamPrefix = "not-base64!!" }, "stream prefix"},
		{"missing data key", func(m *Manifest) { m.DEKWrapped = "" }, "wrapped data key"},
		{"bad data key", func(m *Manifest) { m.DEKWrapped = "!!!" }, "base64"},
		{"duplicate index", func(m *Manifest) { m.Chunks[1].Index = 0 }, "out of order"},
		{"gap in indices", func(m *Manifest) { m.Chunks[1].Index = 5 }, "out of order"},
		{"oversized chunk", func(m *Manifest) { m.Chunks[0].PlainBytes = ChunkPlainSize + 1 }, "outside"},
		{"zero plain bytes", func(m *Manifest) { m.Chunks[0].PlainBytes = 0 }, "outside"},
		{"sealed smaller than plain", func(m *Manifest) { m.Chunks[0].SealedBytes = 10 }, "cannot hold"},
		{"digest not hex", func(m *Manifest) { m.Chunks[0].Sha256Plain = strings.Repeat("z", 64) }, "digest"},
		{"digest too short", func(m *Manifest) { m.Chunks[0].Sha256Plain = "abcd" }, "digest"},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			m := valid()
			tc.mutate(m)
			err := validateManifestShape(m)
			if err == nil {
				t.Fatal("validateManifestShape accepted a malformed manifest")
			}
			if !strings.Contains(err.Error(), tc.want) {
				t.Errorf("error = %q, want it to mention %q", err, tc.want)
			}
		})
	}
}

// TestReceiverRefusesChainRollback is the NAS-012 regression: once a newer chain
// is current, re-publishing an older stored chain must not move the pointer back.
func TestReceiverRefusesChainRollback(t *testing.T) {
	receiver := newTestReceiver(t, "")
	sender := newTestIdentity(t, "naslos-a")
	receiver.authorize(t, sender, nil, 0)
	client := receiver.client(sender)

	source := "naslos-a/rollback"
	data := randomBytes(t, 4096)

	first, err := client.Push(PushOptions{
		Source: source, Reader: bytes.NewReader(data), ToSnapshot: "s1", ToGUID: "1000",
	})
	if err != nil {
		t.Fatalf("first push: %v", err)
	}
	second, err := client.Push(PushOptions{
		Source: source, Reader: bytes.NewReader(data),
		FromSnapshot: "s1", ToSnapshot: "s2", FromGUID: "1000", ToGUID: "2000",
	})
	if err != nil {
		t.Fatalf("second push: %v", err)
	}

	current, err := client.Manifest(source, "")
	if err != nil {
		t.Fatalf("reading the current manifest: %v", err)
	}
	if current.Chain != second.Chain {
		t.Fatalf("current chain = %s, want the second push %s", current.Chain, second.Chain)
	}

	// Re-publish the first (older) chain's manifest: a rollback attempt.
	older, err := client.Manifest(source, first.Chain)
	if err != nil {
		t.Fatalf("reading the older manifest: %v", err)
	}
	fingerprint, err := sender.Fingerprint()
	if err != nil {
		t.Fatalf("fingerprint: %v", err)
	}
	if err := receiver.store.PutManifest(fingerprint, source, older); err == nil {
		t.Fatal("the receiver accepted a rollback of the current chain")
	} else if !strings.Contains(err.Error(), "rollback") {
		t.Errorf("error = %q, want it to explain the rollback", err)
	}

	// The current pointer is unchanged and the backup still restores.
	after, err := client.Manifest(source, "")
	if err != nil {
		t.Fatalf("re-reading the current manifest: %v", err)
	}
	if after.Chain != second.Chain {
		t.Errorf("current chain = %s, want it still %s", after.Chain, second.Chain)
	}
	var restored bytes.Buffer
	if _, err := client.Restore(RestoreOptions{Source: source, Out: &restored}); err != nil {
		t.Fatalf("restore after the refused rollback: %v", err)
	}
	if !bytes.Equal(restored.Bytes(), data) {
		t.Error("restored data differs")
	}
}

// TestPruneSurvivesAForgedTimestamp keeps pruning honest about which chain is
// live: survivorship follows the receiver's current pointer, not the sender's
// CreatedAt, so a rewritten timestamp cannot get the live chain pruned (NAS-012).
func TestPruneSurvivesAForgedTimestamp(t *testing.T) {
	receiver := newTestReceiver(t, "")
	sender := newTestIdentity(t, "naslos-a")
	receiver.authorize(t, sender, nil, 0)
	client := receiver.client(sender)

	source := "naslos-a/forged-time"
	data := randomBytes(t, 4096)

	first, err := client.Push(PushOptions{
		Source: source, Reader: bytes.NewReader(data), ToSnapshot: "s1", ToGUID: "1000",
	})
	if err != nil {
		t.Fatalf("first push: %v", err)
	}
	if _, err := client.Push(PushOptions{
		Source: source, Reader: bytes.NewReader(data),
		FromSnapshot: "s1", ToSnapshot: "s2", FromGUID: "1000", ToGUID: "2000",
	}); err != nil {
		t.Fatalf("second push: %v", err)
	}

	// Rewrite the *older* chain's manifest so it claims to be the newest. The
	// signature no longer verifies, which is exactly the point: survivorship must
	// not depend on this value at all.
	fingerprint, err := sender.Fingerprint()
	if err != nil {
		t.Fatalf("fingerprint: %v", err)
	}
	older, err := client.Manifest(source, first.Chain)
	if err != nil {
		t.Fatalf("reading the older manifest: %v", err)
	}
	forged := *older
	forged.CreatedAt = time.Now().UTC().Add(24 * time.Hour)
	raw, err := json.MarshalIndent(&forged, "", "  ")
	if err != nil {
		t.Fatalf("encoding the forged manifest: %v", err)
	}
	chainDir, err := receiver.store.chainDir(fingerprint, source, first.Chain)
	if err != nil {
		t.Fatalf("chain dir: %v", err)
	}
	if err := os.WriteFile(filepath.Join(chainDir, "manifest.json"), raw, 0o644); err != nil {
		t.Fatalf("rewriting the manifest: %v", err)
	}

	// keep=1 with the current chain being an incremental: the live chain and its
	// base must both survive.
	if _, err := client.Prune(source, 1); err != nil {
		t.Fatalf("prune: %v", err)
	}
	var restored bytes.Buffer
	if _, err := client.Restore(RestoreOptions{Source: source, Out: &restored}); err != nil {
		t.Fatalf("restore after prune with a forged timestamp: %v", err)
	}
	if !bytes.Equal(restored.Bytes(), data) {
		t.Error("restored data differs after prune")
	}
}
