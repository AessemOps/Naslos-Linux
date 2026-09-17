package buddy

import (
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"fmt"
	"log"
	"net/http"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"time"
)

// Headers a peer sets on every request. A sender proves who it is by signing;
// the receiver never needs a shared secret, exactly like ssh.
const (
	HeaderKeyID     = "X-Buddy-Key"
	HeaderTimestamp = "X-Buddy-Timestamp"
	HeaderNonce     = "X-Buddy-Nonce"
	HeaderSignature = "X-Buddy-Signature"
)

// MaxClockSkew bounds how far a request's timestamp may be from ours: without it
// a captured request stays valid forever.
const MaxClockSkew = 5 * time.Minute

// nonceTTL keeps a nonce unusable for longer than a request could be replayed
// within the skew window.
const nonceTTL = 2 * MaxClockSkew

// CanonicalRequest is the exact string a sender signs and a receiver rebuilds.
// Everything that changes the meaning of the request is in it - method, path,
// body digest, timestamp, nonce - so a captured request cannot be moved to
// another path or replayed later.
func CanonicalRequest(method, path, bodyDigest, timestamp, nonce string) string {
	return strings.Join([]string{
		"BUDDY1",
		strings.ToUpper(method),
		path,
		bodyDigest,
		timestamp,
		nonce,
	}, "\n")
}

// BodyDigest is the hex SHA-256 of a request body (the empty body's digest signs
// bodyless requests).
func BodyDigest(body []byte) string {
	sum := sha256.Sum256(body)
	return hex.EncodeToString(sum[:])
}

// SignRequest authenticates an outgoing request. bodyDigest must be BodyDigest of
// exactly the bytes that will be sent.
func SignRequest(id *Identity, req *http.Request, bodyDigest string) error {
	keyID, err := id.Fingerprint()
	if err != nil {
		return err
	}
	nonce, err := randomNonce()
	if err != nil {
		return err
	}
	timestamp := strconv.FormatInt(time.Now().Unix(), 10)

	sig, err := id.Sign([]byte(CanonicalRequest(req.Method, req.URL.RequestURI(), bodyDigest, timestamp, nonce)))
	if err != nil {
		return err
	}

	req.Header.Set(HeaderKeyID, keyID)
	req.Header.Set(HeaderTimestamp, timestamp)
	req.Header.Set(HeaderNonce, nonce)
	req.Header.Set(HeaderSignature, base64.StdEncoding.EncodeToString(sig))
	return nil
}

// Authenticator verifies incoming peer requests against the authorized keys of a
// PeerStore, and remembers nonces so a request cannot be replayed.
type Authenticator struct {
	peers *PeerStore

	mu sync.Mutex
	// seen maps a key to the nonces it has used and when they expire. Per-key
	// nesting bounds one key's memory without letting it evict another's entries.
	seen map[string]map[string]time.Time
	// nonceFile is the append-only replay cache on disk (empty = memory only).
	nonceFile        string
	warnedNonceWrite bool
}

// NewAuthenticator creates an authenticator backed by the given peer store.
func NewAuthenticator(peers *PeerStore) *Authenticator {
	return &Authenticator{peers: peers, seen: make(map[string]map[string]time.Time)}
}

// PersistNonces makes the replay cache survive a restart by recording each used
// nonce under dir (.nonces). Without it, a request captured just before a restart
// could be replayed within the clock-skew window once the process came back with
// an empty cache (NAS-014). Persistence is best effort: a failure to read or write
// the file logs once and leaves the in-memory cache doing its job.
func (a *Authenticator) PersistNonces(dir string) {
	a.mu.Lock()
	defer a.mu.Unlock()

	if dir == "" {
		return
	}
	a.nonceFile = filepath.Join(dir, ".nonces")
	a.loadNoncesLocked(time.Now())
}

// nonceLine renders one persisted nonce: expiry, key, nonce.
func nonceLine(keyID, nonce string, expiry time.Time) string {
	return strconv.FormatInt(expiry.UnixNano(), 10) + " " + keyID + " " + nonce + "\n"
}

// loadNoncesLocked restores unexpired nonces from the file.
func (a *Authenticator) loadNoncesLocked(now time.Time) {
	if a.nonceFile == "" {
		return
	}
	raw, err := os.ReadFile(a.nonceFile)
	if err != nil {
		if !os.IsNotExist(err) {
			log.Printf("buddy auth: cannot read the nonce cache (%v); replay protection restarts from memory", err)
		}
		return
	}
	if a.seen == nil {
		a.seen = make(map[string]map[string]time.Time)
	}
	restored := 0
	for _, line := range strings.Split(string(raw), "\n") {
		fields := strings.SplitN(line, " ", 3)
		if len(fields) != 3 {
			continue
		}
		expiryNanos, err := strconv.ParseInt(fields[0], 10, 64)
		if err != nil {
			continue
		}
		expiry := time.Unix(0, expiryNanos)
		if expiry.Before(now) {
			continue
		}
		keyID, nonce := fields[1], fields[2]
		nonces := a.seen[keyID]
		if nonces == nil {
			nonces = make(map[string]time.Time)
			a.seen[keyID] = nonces
		}
		nonces[nonce] = expiry
		restored++
	}
	if restored > 0 {
		log.Printf("buddy auth: restored %d unexpired nonces from %s", restored, a.nonceFile)
	}
}

// persistNonceLocked appends one nonce and compacts the file when it grows past
// the bound.
func (a *Authenticator) persistNonceLocked(keyID, nonce string, expiry time.Time) {
	if a.nonceFile == "" {
		return
	}
	file, err := os.OpenFile(a.nonceFile, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o600)
	if err == nil {
		if _, writeErr := file.WriteString(nonceLine(keyID, nonce, expiry)); writeErr != nil && !a.warnedNonceWrite {
			a.warnedNonceWrite = true
			log.Printf("buddy auth: cannot persist nonces (%v); a restart would re-arm them within the skew window", writeErr)
		}
		_ = file.Close()
	} else if !a.warnedNonceWrite {
		a.warnedNonceWrite = true
		log.Printf("buddy auth: cannot persist nonces (%v); a restart would re-arm them within the skew window", err)
	}

	entries := 0
	for _, nonces := range a.seen {
		entries += len(nonces)
	}
	if entries < nonceFileCompactThreshold {
		return
	}
	// Rewrite with what is still live: the file would otherwise grow forever.
	var builder strings.Builder
	for usedKey, nonces := range a.seen {
		for used, usedExpiry := range nonces {
			builder.WriteString(nonceLine(usedKey, used, usedExpiry))
		}
	}
	if err := writeFileAtomic(a.nonceFile, []byte(builder.String())); err != nil && !a.warnedNonceWrite {
		a.warnedNonceWrite = true
		log.Printf("buddy auth: cannot compact the nonce cache (%v)", err)
	}
}

// Verify authenticates a request. bodyDigest is the digest of the body the caller
// actually received, and path is the request URI exactly as the sender signed it
// (mount prefix included). The receiver recomputes that path instead of trusting
// the request line, so a prefix-stripping proxy cannot change what a signature
// means.
func (a *Authenticator) Verify(req *http.Request, bodyDigest, path string, now time.Time) (*Peer, error) {
	keyID := req.Header.Get(HeaderKeyID)
	timestamp := req.Header.Get(HeaderTimestamp)
	nonce := req.Header.Get(HeaderNonce)
	signature := req.Header.Get(HeaderSignature)

	if keyID == "" || timestamp == "" || nonce == "" || signature == "" {
		return nil, fmt.Errorf("missing authentication headers")
	}

	ts, err := strconv.ParseInt(timestamp, 10, 64)
	if err != nil {
		return nil, fmt.Errorf("invalid timestamp")
	}
	drift := now.Sub(time.Unix(ts, 0))
	if drift < 0 {
		drift = -drift
	}
	if drift > MaxClockSkew {
		return nil, fmt.Errorf("request timestamp is outside the %s window (check the clocks)", MaxClockSkew)
	}

	peer := a.peers.ByFingerprint(keyID)
	if peer == nil {
		return nil, fmt.Errorf("unknown key %s", keyID)
	}
	if !peer.Enabled {
		return nil, fmt.Errorf("key %s is disabled", keyID)
	}

	sig, err := base64.StdEncoding.DecodeString(signature)
	if err != nil {
		return nil, fmt.Errorf("decoding signature: %w", err)
	}
	canonical := CanonicalRequest(req.Method, path, bodyDigest, timestamp, nonce)
	if err := Verify(peer.PublicKey, []byte(canonical), sig); err != nil {
		// Include what was signed: a URL prefix or proxy rewrite mismatch is the
		// usual cause, and without this the error is impossible to diagnose.
		return nil, fmt.Errorf("%w (signed request: %s %s)", err, req.Method, path)
	}

	// Only burn the nonce once the signature proved the caller owns the key:
	// otherwise anyone could exhaust a peer's nonces with junk.
	if err := validateNonce(nonce); err != nil {
		return nil, err
	}
	if a.replay(keyID, nonce, now) {
		return nil, fmt.Errorf("this request was already used (nonce replay)")
	}

	return peer, nil
}

// maxNoncesPerKey bounds one key's replay cache. A key that sends more requests
// than this within the TTL evicts its own soonest-to-expire entries, which is the
// residual risk of a bound: replaying an evicted nonce inside the clock-skew
// window. Every operation a replay could repeat is idempotent (a chunk write is
// digest-checked, a manifest cannot roll the pointer back), so the bound is worth
// the memory it saves (NAS-014).
const maxNoncesPerKey = 4096

// validateNonce bound-checks a nonce before it is stored: without this a key could
// insert megabyte-long entries and grow the cache without limit (NAS-014).
func validateNonce(nonce string) error {
	if len(nonce) > 128 {
		return fmt.Errorf("nonce is too long")
	}
	decoded, err := base64.StdEncoding.DecodeString(nonce)
	if err != nil {
		return fmt.Errorf("nonce is not valid base64")
	}
	if len(decoded) < 16 || len(decoded) > 64 {
		return fmt.Errorf("nonce must decode to between 16 and 64 bytes")
	}
	return nil
}

// replay records a nonce and reports whether it had been seen before.
func (a *Authenticator) replay(keyID, nonce string, now time.Time) bool {
	a.mu.Lock()
	defer a.mu.Unlock()

	if a.seen == nil {
		a.seen = make(map[string]map[string]time.Time)
	}
	nonces := a.seen[keyID]
	if nonces == nil {
		nonces = make(map[string]time.Time)
		a.seen[keyID] = nonces
	}

	// Opportunistic cleanup of this key's expired nonces keeps the cache bounded
	// without a background goroutine.
	for used, expiry := range nonces {
		if expiry.Before(now) {
			delete(nonces, used)
		}
	}

	if _, used := nonces[nonce]; used {
		return true
	}
	if len(nonces) >= maxNoncesPerKey {
		soonest, soonestExpiry := "", time.Time{}
		for used, expiry := range nonces {
			if soonest == "" || expiry.Before(soonestExpiry) {
				soonest, soonestExpiry = used, expiry
			}
		}
		if soonest != "" {
			delete(nonces, soonest)
		}
	}
	expiry := now.Add(nonceTTL)
	nonces[nonce] = expiry
	a.persistNonceLocked(keyID, nonce, expiry)
	return false
}

// nonceFileCompactThreshold is how many live nonces trigger a rewrite of the
// cache file: below it, appending is cheaper than rewriting.
const nonceFileCompactThreshold = 1024

// randomNonce returns a 128-bit random nonce, base64-encoded.
func randomNonce() (string, error) {
	buf := make([]byte, 16)
	if _, err := rand.Read(buf); err != nil {
		return "", fmt.Errorf("generating nonce: %w", err)
	}
	return base64.StdEncoding.EncodeToString(buf), nil
}
