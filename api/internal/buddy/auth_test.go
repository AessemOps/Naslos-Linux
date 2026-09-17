package buddy

import (
	"encoding/base64"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// TestNonceFormatIsValidated keeps the replay cache from becoming a memory sink:
// a valid key could otherwise store megabyte-long nonces (NAS-014).
func TestNonceFormatIsValidated(t *testing.T) {
	good := base64.StdEncoding.EncodeToString(randomBytes(t, 16))
	if err := validateNonce(good); err != nil {
		t.Errorf("validateNonce(valid) = %v, want nil", err)
	}

	bad := []string{
		"",
		"nonce-1", // not base64
		strings.Repeat("a", 129),
		base64.StdEncoding.EncodeToString(randomBytes(t, 8)),  // too short
		base64.StdEncoding.EncodeToString(randomBytes(t, 65)), // too long
	}
	for _, nonce := range bad {
		if err := validateNonce(nonce); err == nil {
			t.Errorf("validateNonce(%q) = nil, want a rejection", nonce)
		}
	}
}

// TestReplayCacheIsBoundedPerKey pins the bound and its isolation: one key cannot
// grow the cache without limit, and cannot evict another key's nonces.
func TestReplayCacheIsBoundedPerKey(t *testing.T) {
	a := NewAuthenticator(NewPeerStore(filepath.Join(t.TempDir(), "peers.json")))
	now := time.Now()

	for i := 0; i < maxNoncesPerKey+128; i++ {
		a.replay("key-1", base64.StdEncoding.EncodeToString(randomBytes(t, 16)), now)
	}

	a.mu.Lock()
	got := len(a.seen["key-1"])
	a.mu.Unlock()
	if got > maxNoncesPerKey {
		t.Errorf("cache holds %d nonces for one key, want at most %d", got, maxNoncesPerKey)
	}

	// A second key keeps its own entries.
	a.replay("key-2", base64.StdEncoding.EncodeToString(randomBytes(t, 16)), now)
	a.mu.Lock()
	other := len(a.seen["key-2"])
	a.mu.Unlock()
	if other != 1 {
		t.Errorf("key-2 has %d cached nonces, want 1 (isolation)", other)
	}

	// Detection still works for a nonce inside the window.
	nonce := base64.StdEncoding.EncodeToString(randomBytes(t, 16))
	if a.replay("key-3", nonce, now) {
		t.Error("a fresh nonce was reported as a replay")
	}
	if !a.replay("key-3", nonce, now) {
		t.Error("the second use of a nonce was not detected")
	}

	// Expired entries are dropped for the key that is being used.
	a.replay("key-4", base64.StdEncoding.EncodeToString(randomBytes(t, 16)), now.Add(-nonceTTL-time.Minute))
	a.replay("key-4", base64.StdEncoding.EncodeToString(randomBytes(t, 16)), now)
	a.mu.Lock()
	live := len(a.seen["key-4"])
	a.mu.Unlock()
	if live != 1 {
		t.Errorf("key-4 has %d live nonces, want the expired one dropped", live)
	}
}

// TestNoncesSurviveARestart is the NAS-014 fix: a request captured just before a
// restart could otherwise be replayed inside the clock-skew window, because the
// cache used to be memory-only.
func TestNoncesSurviveARestart(t *testing.T) {
	dir := t.TempDir()
	peers := NewPeerStore(filepath.Join(dir, "peers.json"))
	now := time.Now()
	nonce := base64.StdEncoding.EncodeToString(randomBytes(t, 16))

	first := NewAuthenticator(peers)
	first.PersistNonces(dir)
	if first.replay("key-1", nonce, now) {
		t.Fatal("a fresh nonce was reported as a replay")
	}

	// A new authenticator on the same directory is what a restart looks like.
	restarted := NewAuthenticator(peers)
	restarted.PersistNonces(dir)
	if !restarted.replay("key-1", nonce, now) {
		t.Error("the nonce was accepted after a restart")
	}

	// An expired entry is not restored, so it cannot block a legitimate request.
	old := time.Now().Add(-nonceTTL - time.Hour)
	expiredNonce := base64.StdEncoding.EncodeToString(randomBytes(t, 16))
	first.replay("key-1", expiredNonce, old)

	after := NewAuthenticator(peers)
	after.PersistNonces(dir)
	if after.replay("key-1", expiredNonce, now) {
		t.Error("an expired nonce was restored and treated as a replay")
	}
}

// TestNonceFileIsCompacted keeps the cache file bounded: it is rewritten once the
// number of live nonces passes the threshold.
func TestNonceFileIsCompacted(t *testing.T) {
	dir := t.TempDir()
	a := NewAuthenticator(NewPeerStore(filepath.Join(dir, "peers.json")))
	a.PersistNonces(dir)

	now := time.Now()
	for i := 0; i < nonceFileCompactThreshold+50; i++ {
		a.replay("key-1", base64.StdEncoding.EncodeToString(randomBytes(t, 16)), now)
	}

	raw, err := os.ReadFile(filepath.Join(dir, ".nonces"))
	if err != nil {
		t.Fatalf("reading the cache: %v", err)
	}
	lines := 0
	for _, line := range strings.Split(string(raw), "\n") {
		if strings.TrimSpace(line) != "" {
			lines++
		}
	}
	// After compaction the file holds roughly the live entries, not every append.
	if lines > nonceFileCompactThreshold+200 {
		t.Errorf("cache file has %d lines, want it compacted near %d", lines, nonceFileCompactThreshold)
	}

	// The live entries are still there: a restart still detects the newest replay.
	restarted := NewAuthenticator(NewPeerStore(filepath.Join(dir, "peers.json")))
	restarted.PersistNonces(dir)
	a.mu.Lock()
	var last string
	for nonce := range a.seen["key-1"] {
		last = nonce
		break
	}
	a.mu.Unlock()
	if last == "" {
		t.Fatal("no nonce was recorded")
	}
	if !restarted.replay("key-1", last, now) {
		t.Error("a live nonce was lost by compaction")
	}
}

// TestPersistNoncesWithoutAPathStaysInMemory keeps the standalone behaviour
// explicit: no path means no file, and no error.
func TestPersistNoncesWithoutAPathStaysInMemory(t *testing.T) {
	a := NewAuthenticator(NewPeerStore(filepath.Join(t.TempDir(), "peers.json")))
	a.PersistNonces("")

	nonce := base64.StdEncoding.EncodeToString(randomBytes(t, 16))
	if a.replay("key-1", nonce, time.Now()) {
		t.Fatal("a fresh nonce was reported as a replay")
	}
	if !a.replay("key-1", nonce, time.Now()) {
		t.Error("the second use of a nonce was not detected")
	}
}
