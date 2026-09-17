package buddy

import (
	"encoding/base64"
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
