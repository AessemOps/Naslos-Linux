package buddy

import (
	"crypto/subtle"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"
)

// PathPrefix is the URL prefix every buddy endpoint lives under. The standalone
// Docker receiver and a Naslos instance serve exactly the same paths, so a sender
// does not know (or care) which kind of receiver it is talking to.
const PathPrefix = "/api/buddy/v1"

// maxManifestBytes bounds a manifest: it lists chunks, so it is small even for
// multi-terabyte chains.
const maxManifestBytes = 8 << 20

// Receiver is the HTTP surface peers push to. It has no Kubernetes, ZFS or
// identity dependency: the instance mounts it under PathPrefix, and the
// standalone container serves the very same handler.
type Receiver struct {
	// Store is where sealed chunks live.
	Store *Store
	// Peers is the authorized keys registry.
	Peers *PeerStore
	// Auth verifies request signatures (one per receiver so nonce state is
	// shared).
	Auth *Authenticator
	// Name labels this receiver in the sender's UI ("naslos-b", "backup-nas").
	Name string
	// Version is reported to senders so they can warn about a protocol
	// mismatch.
	Version string
	// EnrollToken bootstraps the first key: a sender presents it once and its
	// public key is authorized. Empty disables enrollment (the operator then
	// authorizes keys out of band).
	EnrollToken string
	// EnrollOpen keeps the token usable for more than one key. Single use is
	// the default because a leaked token otherwise authorizes anyone forever.
	EnrollOpen bool

	mu         sync.Mutex
	enrollUsed bool

	now func() time.Time
}

// Status is the receiver's report to a sender: the "space left" and "last backup"
// the owner sees in the UI, plus what is already stored (so a sender can show
// whether it is up to date).
type Status struct {
	Version     string            `json:"version"`
	Receiver    string            `json:"receiver"`
	KeyID       string            `json:"keyId"`
	FreeBytes   int64             `json:"freeBytes"`
	UsedBytes   int64             `json:"usedBytes"`
	QuotaBytes  int64             `json:"quotaBytes,omitempty"`
	LastBackup  *time.Time        `json:"lastBackup,omitempty"`
	Sources     []string          `json:"sources"`
	Chains      map[string]string `json:"chains,omitempty"`
	EnrollOpen  bool              `json:"enrollOpen"`
	PeerNames   []string          `json:"peers"`
	ServerTime  time.Time         `json:"serverTime"`
	ProtocolVer int               `json:"protocolVersion"`
}

// EnrollRequest is how a new sender presents its public key.
type EnrollRequest struct {
	Token     string   `json:"token"`
	Name      string   `json:"name"`
	PublicKey string   `json:"publicKey"`
	Sources   []string `json:"sources,omitempty"`
}

// chunkListEntry is one row of a chain's chunk list: the index plus the digest of
// the sealed bytes stored there, which lets a resuming sender prove the chunks it
// skips are the chunks it sent.
type chunkListEntry struct {
	Index  int    `json:"index"`
	Digest string `json:"digest"`
}

// Handler returns the receiver's routes, already stripped of PathPrefix.
func (r *Receiver) Handler() http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("/status", r.handleStatus)
	mux.HandleFunc("/enroll", r.handleEnroll)
	mux.HandleFunc("/backups", r.handleBackups)
	mux.HandleFunc("/chunks/", r.handleChunks)
	mux.HandleFunc("/manifest/", r.handleManifest)
	mux.HandleFunc("/prune/", r.handlePrune)
	return http.StripPrefix(PathPrefix, mux)
}

// clock is the receiver's time source (overridable in tests).
func (r *Receiver) clock() time.Time {
	if r.now != nil {
		return r.now()
	}
	return time.Now()
}

// canonicalPath rebuilds the path the sender signed. The signature covers the
// full URI (prefix included), so the mount point can never change what a request
// is allowed to do.
func canonicalPath(req *http.Request) string {
	return PathPrefix + req.URL.RequestURI()
}

// authenticate verifies the request signature over the given body digest, using
// the full URI (prefix included) that the sender signed.
func (r *Receiver) authenticate(req *http.Request, bodyDigest string) (*Peer, error) {
	if r.Auth == nil {
		return nil, fmt.Errorf("receiver has no authenticator")
	}
	return r.Auth.Verify(req, bodyDigest, canonicalPath(req), r.clock())
}

// readBody reads a bounded body and returns it with its digest.
func readBody(req *http.Request, limit int64) ([]byte, string, error) {
	body, err := io.ReadAll(io.LimitReader(req.Body, limit+1))
	if err != nil {
		return nil, "", fmt.Errorf("reading request body: %w", err)
	}
	if int64(len(body)) > limit {
		return nil, "", fmt.Errorf("request body exceeds %d bytes", limit)
	}
	return body, BodyDigest(body), nil
}

// writeJSON writes a JSON response.
func writeJSON(w http.ResponseWriter, status int, data any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(data)
}

// writeError writes an error response in the API's shape.
func writeError(w http.ResponseWriter, status int, msg string) {
	writeJSON(w, status, map[string]string{"error": msg})
}

// handleStatus reports what the sender needs to display and to decide what to
// send: free space on the receiver, what this key already stored, and when its
// last backup landed.
func (r *Receiver) handleStatus(w http.ResponseWriter, req *http.Request) {
	if req.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "GET only")
		return
	}
	peer, err := r.authenticate(req, BodyDigest(nil))
	if err != nil {
		writeError(w, http.StatusUnauthorized, err.Error())
		return
	}
	r.Peers.TouchSeen(peer.Name, r.clock())

	free, err := r.Store.FreeSpace()
	if err != nil {
		writeError(w, http.StatusInternalServerError, fmt.Sprintf("cannot read free space: %v", err))
		return
	}
	used, err := r.Store.Usage(peer.Fingerprint)
	if err != nil {
		writeError(w, http.StatusInternalServerError, fmt.Sprintf("cannot read usage: %v", err))
		return
	}
	last, _ := r.Store.LastBackup(peer.Fingerprint)
	sources, _ := r.Store.Sources(peer.Fingerprint)

	chains := make(map[string]string, len(sources))
	for _, source := range sources {
		if manifest, err := r.Store.Manifest(peer.Fingerprint, source); err == nil {
			chains[source] = manifest.Chain
		}
	}

	peerNames := make([]string, 0, r.Peers.Count())
	for _, p := range r.Peers.List() {
		peerNames = append(peerNames, p.Name)
	}

	status := Status{
		Version:     r.Version,
		Receiver:    r.Name,
		KeyID:       peer.Fingerprint,
		FreeBytes:   free,
		UsedBytes:   used,
		QuotaBytes:  peer.QuotaBytes,
		Sources:     sources,
		Chains:      chains,
		EnrollOpen:  r.EnrollToken != "",
		PeerNames:   peerNames,
		ServerTime:  r.clock().UTC(),
		ProtocolVer: EnvelopeVersion,
	}
	if !last.IsZero() {
		utc := last.UTC()
		status.LastBackup = &utc
	}
	writeJSON(w, http.StatusOK, status)
}

// handleEnroll authorizes a new sender's public key using the receiver's
// enrollment token. It is the only unsigned endpoint: everything else requires a
// key that is already authorized.
func (r *Receiver) handleEnroll(w http.ResponseWriter, req *http.Request) {
	if req.Method != http.MethodPost {
		writeError(w, http.StatusMethodNotAllowed, "POST only")
		return
	}
	if r.EnrollToken == "" {
		writeError(w, http.StatusForbidden, "enrollment is disabled on this receiver: authorize the key on the receiver instead")
		return
	}

	body, _, err := readBody(req, 64<<10)
	if err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	var enroll EnrollRequest
	if err := json.Unmarshal(body, &enroll); err != nil {
		writeError(w, http.StatusBadRequest, fmt.Sprintf("invalid enrollment request: %v", err))
		return
	}
	if subtle.ConstantTimeCompare([]byte(enroll.Token), []byte(r.EnrollToken)) != 1 {
		writeError(w, http.StatusUnauthorized, "invalid enrollment token")
		return
	}
	if strings.TrimSpace(enroll.Name) == "" || strings.TrimSpace(enroll.PublicKey) == "" {
		writeError(w, http.StatusBadRequest, "name and publicKey are required")
		return
	}
	if _, _, err := PublicKeyOf(enroll.PublicKey); err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	for _, source := range enroll.Sources {
		if err := ValidateSource(source); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
	}

	r.mu.Lock()
	if r.enrollUsed && !r.EnrollOpen {
		r.mu.Unlock()
		writeError(w, http.StatusForbidden, "this enrollment token has already been used")
		return
	}
	peer := &Peer{
		Name:           enroll.Name,
		PublicKey:      enroll.PublicKey,
		AllowedSources: enroll.Sources,
		Enabled:        true,
	}
	err = r.Peers.Add(peer)
	if err == nil && !r.EnrollOpen {
		r.enrollUsed = true
	}
	r.mu.Unlock()

	if err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	writeJSON(w, http.StatusCreated, map[string]any{
		"status":      "enrolled",
		"name":        peer.Name,
		"fingerprint": peer.Fingerprint,
	})
}

// peerFor authenticates a request and checks the key may write the source. It
// writes the error response itself, so handlers simply return when ok is false.
func (r *Receiver) peerFor(w http.ResponseWriter, req *http.Request, bodyDigest, source string) (*Peer, bool) {
	peer, err := r.authenticate(req, bodyDigest)
	if err != nil {
		writeError(w, http.StatusUnauthorized, err.Error())
		return nil, false
	}
	if !peer.Allows(source) {
		writeError(w, http.StatusForbidden, fmt.Sprintf("key %q is not allowed to write %s", peer.Name, source))
		return nil, false
	}
	r.Peers.TouchSeen(peer.Name, r.clock())
	return peer, true
}

// handleChunks serves the chunk endpoints. The index travels as a query
// parameter rather than a path segment because a source may itself contain
// slashes ("naslos-a/tank/data"), and guessing where the source ends would be a
// correctness bug waiting to happen.
//
//	GET PathPrefix/chunks/<source>?chain=<id>            list stored indices
//	GET PathPrefix/chunks/<source>?chain=<id>&index=<n>  fetch one sealed chunk
//	PUT PathPrefix/chunks/<source>?chain=<id>&index=<n>  upload one sealed chunk
//
// Chunk upload is the workhorse of a push, so it stays deliberately simple: the
// body is one sealed chunk (bounded), the signature covers it, and a re-sent
// chunk with identical bytes is idempotent - that is what makes a resumed push
// cheap.
func (r *Receiver) handleChunks(w http.ResponseWriter, req *http.Request) {
	source := strings.TrimPrefix(req.URL.Path, "/chunks/")
	chain := req.URL.Query().Get("chain")
	indexParam := req.URL.Query().Get("index")

	switch req.Method {
	case http.MethodGet:
		peer, ok := r.peerFor(w, req, BodyDigest(nil), source)
		if !ok {
			return
		}
		if chain == "" {
			writeError(w, http.StatusBadRequest, "chain is required")
			return
		}
		if indexParam != "" {
			index, err := strconv.Atoi(indexParam)
			if err != nil || index < 0 {
				writeError(w, http.StatusBadRequest, fmt.Sprintf("invalid chunk index %q", indexParam))
				return
			}
			sealed, err := r.Store.Chunk(peer.Fingerprint, source, chain, index)
			if err != nil {
				writeError(w, http.StatusNotFound, err.Error())
				return
			}
			w.Header().Set("Content-Type", "application/octet-stream")
			w.WriteHeader(http.StatusOK)
			_, _ = w.Write(sealed)
			return
		}
		digests, err := r.Store.ChunkDigests(peer.Fingerprint, source, chain)
		if err != nil {
			writeError(w, http.StatusInternalServerError, err.Error())
			return
		}
		// The digest lets a resuming sender prove the chunks it skips are the
		// chunks it sent.
		chunks := make([]chunkListEntry, 0, len(digests))
		for index, digest := range digests {
			chunks = append(chunks, chunkListEntry{Index: index, Digest: digest})
		}
		sort.Slice(chunks, func(i, j int) bool { return chunks[i].Index < chunks[j].Index })
		writeJSON(w, http.StatusOK, map[string]any{
			"source": source,
			"chain":  chain,
			"chunks": chunks,
		})

	case http.MethodPut:
		index, err := strconv.Atoi(indexParam)
		if err != nil || index < 0 {
			writeError(w, http.StatusBadRequest, fmt.Sprintf("invalid chunk index %q", indexParam))
			return
		}
		body, digest, err := readBody(req, MaxSealedChunkSize)
		if err != nil {
			writeError(w, http.StatusRequestEntityTooLarge, err.Error())
			return
		}
		peer, ok := r.peerFor(w, req, digest, source)
		if !ok {
			return
		}
		if chain == "" {
			writeError(w, http.StatusBadRequest, "chain is required")
			return
		}
		if peer.QuotaBytes > 0 {
			used, err := r.Store.Usage(peer.Fingerprint)
			if err != nil {
				writeError(w, http.StatusInternalServerError, err.Error())
				return
			}
			if used+int64(len(body)) > peer.QuotaBytes {
				writeError(w, http.StatusRequestEntityTooLarge, fmt.Sprintf(
					"quota exceeded: %d of %d bytes are already stored for this key", used, peer.QuotaBytes))
				return
			}
		}
		if err := r.Store.PutChunk(peer.Fingerprint, source, chain, index, body); err != nil {
			writeError(w, http.StatusConflict, err.Error())
			return
		}
		writeJSON(w, http.StatusCreated, map[string]any{
			"source":      source,
			"chain":       chain,
			"index":       index,
			"storedBytes": len(body),
		})

	default:
		writeError(w, http.StatusMethodNotAllowed, "GET or PUT only")
	}
}

// handleManifest serves the manifest endpoints:
//
//	GET PathPrefix/manifest/<source>?chain=<id>  fetch a stored manifest
//	PUT PathPrefix/manifest/<source>             publish a signed manifest
//
// The manifest is what turns a pile of chunks into a backup, so the receiver
// refuses one whose chunks are not all present: "the latest backup" can never end
// up pointing at an incomplete chain.
func (r *Receiver) handleManifest(w http.ResponseWriter, req *http.Request) {
	source := strings.TrimPrefix(req.URL.Path, "/manifest/")
	if err := ValidateSource(source); err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	chain := req.URL.Query().Get("chain")

	switch req.Method {
	case http.MethodGet:
		peer, ok := r.peerFor(w, req, BodyDigest(nil), source)
		if !ok {
			return
		}
		manifest, err := r.Store.Manifest(peer.Fingerprint, source)
		if err != nil {
			writeError(w, http.StatusNotFound, err.Error())
			return
		}
		if chain != "" && chain != manifest.Chain {
			writeError(w, http.StatusNotFound, fmt.Sprintf(
				"chain %s is not the current chain of %s (current: %s)", chain, source, manifest.Chain))
			return
		}
		writeJSON(w, http.StatusOK, manifest)

	case http.MethodPut:
		body, digest, err := readBody(req, maxManifestBytes)
		if err != nil {
			writeError(w, http.StatusRequestEntityTooLarge, err.Error())
			return
		}
		peer, ok := r.peerFor(w, req, digest, source)
		if !ok {
			return
		}

		var manifest Manifest
		if err := json.Unmarshal(body, &manifest); err != nil {
			writeError(w, http.StatusBadRequest, fmt.Sprintf("invalid manifest: %v", err))
			return
		}
		if manifest.Version != EnvelopeVersion {
			writeError(w, http.StatusBadRequest, fmt.Sprintf(
				"protocol version mismatch: the manifest is v%d, this receiver speaks v%d", manifest.Version, EnvelopeVersion))
			return
		}
		if manifest.Source != source {
			writeError(w, http.StatusBadRequest, fmt.Sprintf(
				"manifest is for %q but was sent to %q", manifest.Source, source))
			return
		}
		if manifest.Chain == "" {
			writeError(w, http.StatusBadRequest, "manifest has no chain id")
			return
		}
		if len(manifest.Chunks) == 0 {
			writeError(w, http.StatusBadRequest, "manifest lists no chunks")
			return
		}
		// The receiver cannot decrypt, but it can check that the manifest really
		// was signed by the key that is authorized for this namespace.
		if err := manifest.VerifySignature(peer.PublicKey); err != nil {
			writeError(w, http.StatusForbidden, err.Error())
			return
		}

		stored, err := r.Store.ListChunks(peer.Fingerprint, source, manifest.Chain)
		if err != nil {
			writeError(w, http.StatusInternalServerError, err.Error())
			return
		}
		if missing := missingChunks(manifest.Chunks, stored); len(missing) > 0 {
			writeJSON(w, http.StatusConflict, map[string]any{
				"error":         fmt.Sprintf("%d of %d chunks of chain %s are missing", len(missing), len(manifest.Chunks), manifest.Chain),
				"missingChunks": missing,
			})
			return
		}

		if err := r.Store.PutManifest(peer.Fingerprint, source, &manifest); err != nil {
			writeError(w, http.StatusInternalServerError, err.Error())
			return
		}

		var sealed int64
		for _, chunk := range manifest.Chunks {
			sealed += int64(chunk.SealedBytes)
		}
		writeJSON(w, http.StatusOK, map[string]any{
			"source":     source,
			"chain":      manifest.Chain,
			"chunks":     len(manifest.Chunks),
			"storedByte": sealed,
		})

	default:
		writeError(w, http.StatusMethodNotAllowed, "GET or PUT only")
	}
}

// missingChunks reports declared chunks that are not stored, capped so a broken
// manifest cannot produce an unbounded error payload.
func missingChunks(declared []ManifestChunk, stored []int) []int {
	have := make(map[int]bool, len(stored))
	for _, index := range stored {
		have[index] = true
	}
	var missing []int
	for _, chunk := range declared {
		if have[chunk.Index] {
			continue
		}
		missing = append(missing, chunk.Index)
		if len(missing) >= 20 {
			break
		}
	}
	return missing
}

// handlePrune applies the owner's retention request: keep the newest N chains of
// a source, delete the rest.
func (r *Receiver) handlePrune(w http.ResponseWriter, req *http.Request) {
	if req.Method != http.MethodPost {
		writeError(w, http.StatusMethodNotAllowed, "POST only")
		return
	}
	source := strings.TrimPrefix(req.URL.Path, "/prune/")
	if err := ValidateSource(source); err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}

	body, digest, err := readBody(req, 64<<10)
	if err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	peer, ok := r.peerFor(w, req, digest, source)
	if !ok {
		return
	}

	var request struct {
		Keep int `json:"keep"`
	}
	if err := json.Unmarshal(body, &request); err != nil {
		writeError(w, http.StatusBadRequest, fmt.Sprintf("invalid prune request: %v", err))
		return
	}
	if request.Keep < 1 {
		writeError(w, http.StatusBadRequest, "keep must be at least 1")
		return
	}

	removed, err := r.Store.Prune(peer.Fingerprint, source, request.Keep)
	if err != nil {
		writeError(w, http.StatusInternalServerError, err.Error())
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{
		"source":        source,
		"keep":          request.Keep,
		"removedChains": removed,
	})
}

// handleBackups lists what this key has stored (the sender's "backups" view).
func (r *Receiver) handleBackups(w http.ResponseWriter, req *http.Request) {
	if req.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "GET only")
		return
	}
	// No source to scope here: a key may always see its own backups.
	peer, err := r.authenticate(req, BodyDigest(nil))
	if err != nil {
		writeError(w, http.StatusUnauthorized, err.Error())
		return
	}
	r.Peers.TouchSeen(peer.Name, r.clock())

	backups, err := r.Store.Summary(peer.Fingerprint)
	if err != nil {
		writeError(w, http.StatusInternalServerError, err.Error())
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"backups": backups})
}
