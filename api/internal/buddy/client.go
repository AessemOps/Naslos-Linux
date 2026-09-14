package buddy

import (
	"bytes"
	"crypto/rand"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"
)

// Client is a sender: it authenticates to a receiver with one identity and pushes
// backups that the receiver cannot read. The same client drives the CLI, the API
// and standalone tooling - the protocol is identical for every receiver kind.
type Client struct {
	// Identity is the sender's key material.
	Identity *Identity
	// BaseURL is the receiver root without PathPrefix ("https://naslos-b.local").
	BaseURL string
	// HTTP is the HTTP client: a push is many small requests, not one long one,
	// so a per-request timeout is the only bound needed.
	HTTP *http.Client
}

// NewClient creates a sender for a receiver.
func NewClient(baseURL string, id *Identity) *Client {
	return &Client{
		Identity: id,
		BaseURL:  strings.TrimRight(baseURL, "/"),
		HTTP:     &http.Client{Timeout: 5 * time.Minute},
	}
}

// do signs and sends one request, returning the body and status.
func (c *Client) do(method, path string, body []byte) ([]byte, int, error) {
	req, err := http.NewRequest(method, c.BaseURL+PathPrefix+path, bytes.NewReader(body))
	if err != nil {
		return nil, 0, err
	}
	if len(body) > 0 {
		req.Header.Set("Content-Type", "application/json")
	}
	if err := SignRequest(c.Identity, req, BodyDigest(body)); err != nil {
		return nil, 0, err
	}

	client := c.HTTP
	if client == nil {
		client = &http.Client{Timeout: 5 * time.Minute}
	}
	resp, err := client.Do(req)
	if err != nil {
		return nil, 0, err
	}
	defer resp.Body.Close()

	data, err := io.ReadAll(io.LimitReader(resp.Body, maxManifestBytes))
	if err != nil {
		return nil, resp.StatusCode, err
	}
	if resp.StatusCode >= 400 {
		return data, resp.StatusCode, fmt.Errorf("%s %s: %s", method, path, describeError(resp.StatusCode, data))
	}
	return data, resp.StatusCode, nil
}

// describeError turns an error response into something worth showing an operator.
func describeError(status int, data []byte) string {
	var payload struct {
		Error string `json:"error"`
	}
	if json.Unmarshal(data, &payload) == nil && payload.Error != "" {
		return fmt.Sprintf("%s (%d)", payload.Error, status)
	}
	text := strings.TrimSpace(string(data))
	if text == "" {
		text = http.StatusText(status)
	}
	if len(text) > 300 {
		text = text[:300] + "..."
	}
	return fmt.Sprintf("%s (%d)", text, status)
}

// Status fetches the receiver's report: free space, what is stored and when this
// key last backed up.
func (c *Client) Status() (*Status, error) {
	data, _, err := c.do(http.MethodGet, "/status", nil)
	if err != nil {
		return nil, err
	}
	var status Status
	if err := json.Unmarshal(data, &status); err != nil {
		return nil, fmt.Errorf("parsing status: %w", err)
	}
	return &status, nil
}

// Enroll authorizes this client's public key on a receiver that has an enrollment
// token. It is the only unauthenticated call in the protocol: how the first key
// gets in.
func (c *Client) Enroll(token, name string, sources []string) (string, error) {
	payload, err := json.Marshal(EnrollRequest{
		Token:     token,
		Name:      name,
		PublicKey: c.Identity.PublicKey,
		Sources:   sources,
	})
	if err != nil {
		return "", err
	}
	data, _, err := c.do(http.MethodPost, "/enroll", payload)
	if err != nil {
		return "", err
	}
	var response struct {
		Fingerprint string `json:"fingerprint"`
	}
	if err := json.Unmarshal(data, &response); err != nil {
		return "", fmt.Errorf("parsing enrollment response: %w", err)
	}
	return response.Fingerprint, nil
}

// Backups lists the backups stored on the receiver for this key.
func (c *Client) Backups() ([]Backups, error) {
	data, _, err := c.do(http.MethodGet, "/backups", nil)
	if err != nil {
		return nil, err
	}
	var response struct {
		Backups []Backups `json:"backups"`
	}
	if err := json.Unmarshal(data, &response); err != nil {
		return nil, fmt.Errorf("parsing backups: %w", err)
	}
	return response.Backups, nil
}

// Manifest fetches a stored manifest (the current one when chain is empty).
func (c *Client) Manifest(source, chain string) (*Manifest, error) {
	if err := ValidateSource(source); err != nil {
		return nil, err
	}
	path := "/manifest/" + source
	if chain != "" {
		path += "?chain=" + chain
	}
	data, _, err := c.do(http.MethodGet, path, nil)
	if err != nil {
		return nil, err
	}
	var manifest Manifest
	if err := json.Unmarshal(data, &manifest); err != nil {
		return nil, fmt.Errorf("parsing manifest: %w", err)
	}
	return &manifest, nil
}

// storedChunks returns the digest of every sealed chunk the receiver already
// holds for a chain: how an interrupted push knows where to continue, and how it
// proves the bytes it skips are the bytes it sent.
func (c *Client) storedChunks(source, chain string) (map[int]string, error) {
	if err := ValidateSource(source); err != nil {
		return nil, err
	}
	data, _, err := c.do(http.MethodGet, "/chunks/"+source+"?chain="+chain, nil)
	if err != nil {
		return nil, err
	}
	var response struct {
		Chunks []chunkListEntry `json:"chunks"`
	}
	if err := json.Unmarshal(data, &response); err != nil {
		return nil, fmt.Errorf("parsing chunk list: %w", err)
	}
	stored := make(map[int]string, len(response.Chunks))
	for _, chunk := range response.Chunks {
		stored[chunk.Index] = chunk.Digest
	}
	return stored, nil
}

// Prune asks the receiver to keep only the newest chains of a source.
func (c *Client) Prune(source string, keep int) (int, error) {
	if err := ValidateSource(source); err != nil {
		return 0, err
	}
	payload, err := json.Marshal(map[string]int{"keep": keep})
	if err != nil {
		return 0, err
	}
	data, _, err := c.do(http.MethodPost, "/prune/"+source, payload)
	if err != nil {
		return 0, err
	}
	var response struct {
		RemovedChains int `json:"removedChains"`
	}
	if err := json.Unmarshal(data, &response); err != nil {
		return 0, fmt.Errorf("parsing prune response: %w", err)
	}
	return response.RemovedChains, nil
}

// randomChainID returns a fresh chain identifier.
func randomChainID() (string, error) {
	buf := make([]byte, 8)
	if _, err := rand.Read(buf); err != nil {
		return "", fmt.Errorf("generating chain id: %w", err)
	}
	return hex.EncodeToString(buf), nil
}

// sealedOverhead is the exact non-payload size SealChunk adds to each chunk
// (magic 4 + length 4 + nonce 12 + GCM tag 16). It lets a sender describe chunks
// it skipped during a resume without re-sealing them.
const sealedOverhead = len(chunkMagic) + 4 + 12 + 16

// ChainStateVersion is the on-disk version of the resume state.
const ChainStateVersion = 1

// ChainState is what a sender must remember to resume an interrupted push: the
// chain id, the data key and the nonce prefix. It holds the chain's DEK, so it is
// as sensitive as the identity itself and must be stored the same way (0600).
type ChainState struct {
	Version   int       `json:"version"`
	Source    string    `json:"source"`
	Chain     string    `json:"chain"`
	Kind      string    `json:"kind"`
	DEK       string    `json:"dek"`
	Prefix    string    `json:"prefix"`
	StartedAt time.Time `json:"startedAt"`
}

// NewChainState starts a fresh chain segment for a source.
func NewChainState(source, kind string) (*ChainState, error) {
	if err := ValidateSource(source); err != nil {
		return nil, err
	}
	if kind == "" {
		kind = "tar"
	}
	chain, err := randomChainID()
	if err != nil {
		return nil, err
	}
	dek, err := newDEK()
	if err != nil {
		return nil, err
	}
	prefix, err := newStreamPrefix()
	if err != nil {
		return nil, err
	}
	return &ChainState{
		Version:   ChainStateVersion,
		Source:    source,
		Chain:     chain,
		Kind:      kind,
		DEK:       base64.StdEncoding.EncodeToString(dek),
		Prefix:    base64.StdEncoding.EncodeToString(prefix),
		StartedAt: time.Now().UTC(),
	}, nil
}

// LoadChainState reads resume state from disk.
func LoadChainState(path string) (*ChainState, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	var state ChainState
	if err := json.Unmarshal(data, &state); err != nil {
		return nil, fmt.Errorf("parsing chain state %s: %w", path, err)
	}
	return &state, nil
}

// Save writes the resume state with owner-only permissions: it carries a DEK.
func (s *ChainState) Save(path string) error {
	data, err := json.MarshalIndent(s, "", "  ")
	if err != nil {
		return err
	}
	if dir := filepath.Dir(path); dir != "" && dir != "." {
		if err := os.MkdirAll(dir, 0700); err != nil {
			return err
		}
	}
	tmp, err := os.CreateTemp(filepath.Dir(path), ".chain-*.tmp")
	if err != nil {
		return err
	}
	tmpName := tmp.Name()
	defer os.Remove(tmpName)

	if err := tmp.Chmod(0600); err != nil {
		tmp.Close()
		return err
	}
	if _, err := tmp.Write(data); err != nil {
		tmp.Close()
		return err
	}
	if err := tmp.Close(); err != nil {
		return err
	}
	return os.Rename(tmpName, path)
}

// dek decodes the chain's data key.
func (s *ChainState) dek() ([]byte, error) {
	dek, err := base64.StdEncoding.DecodeString(s.DEK)
	if err != nil {
		return nil, fmt.Errorf("decoding chain key: %w", err)
	}
	if len(dek) != 32 {
		return nil, fmt.Errorf("chain key must be 32 bytes, got %d", len(dek))
	}
	return dek, nil
}

// prefix decodes the chain's nonce prefix.
func (s *ChainState) prefix() ([]byte, error) {
	prefix, err := base64.StdEncoding.DecodeString(s.Prefix)
	if err != nil {
		return nil, fmt.Errorf("decoding stream prefix: %w", err)
	}
	if len(prefix) != 8 {
		return nil, fmt.Errorf("stream prefix must be 8 bytes, got %d", len(prefix))
	}
	return prefix, nil
}

// validChainID guards the chain id that goes into URLs and file names.
func validChainID(chain string) error {
	if chain == "" {
		return fmt.Errorf("chain id is required")
	}
	if len(chain) > 64 {
		return fmt.Errorf("chain id must be 64 characters or fewer")
	}
	for _, r := range chain {
		switch {
		case r >= 'a' && r <= 'z', r >= '0' && r <= '9':
		default:
			return fmt.Errorf("invalid chain id %q: use lowercase letters and digits", chain)
		}
	}
	return nil
}

// PushOptions describes one push.
type PushOptions struct {
	// Source is the logical name this backup is stored under on the receiver.
	Source string
	// Kind labels the payload ("tar", "zfs-send") so a restore knows what it got.
	Kind string
	// Reader is the plaintext stream (a tar stream, a zfs send stream, ...).
	Reader io.Reader
	// State resumes an interrupted push; nil starts a fresh chain.
	State *ChainState
	// Snapshot metadata, carried through for the restore side.
	FromSnapshot, ToSnapshot, FromGUID, ToGUID string
	// PruneKeep, when greater than zero, asks the receiver to keep only the
	// newest N chains once this push has been published.
	PruneKeep int
	// Progress is called after every chunk (nil is fine).
	Progress func(PushProgress)
}

// PushProgress is one progress tick, for a UI or a progress bar.
type PushProgress struct {
	Chunk       int   `json:"chunk"`
	PlainBytes  int64 `json:"plainBytes"`
	SealedBytes int64 `json:"sealedBytes"`
	Uploaded    int   `json:"uploaded"`
	Skipped     int   `json:"skipped"`
}

// PushResult summarises a push.
type PushResult struct {
	Source       string      `json:"source"`
	Chain        string      `json:"chain"`
	Chunks       int         `json:"chunks"`
	Uploaded     int         `json:"uploaded"`
	Skipped      int         `json:"skipped"`
	PlainBytes   int64       `json:"plainBytes"`
	SealedBytes  int64       `json:"sealedBytes"`
	PrunedChains int         `json:"prunedChains,omitempty"`
	State        *ChainState `json:"-"`
	Manifest     *Manifest   `json:"-"`
}

// Push streams the plaintext, encrypts it chunk by chunk, uploads the chunks the
// receiver does not have yet and publishes a signed manifest.
//
// Interrupting a push is safe by design: re-running it with the same State reuses
// the chain, the data key and the nonce prefix, skips the chunks already stored,
// and finishes the manifest. Because the chain's bytes are keyed by (DEK, prefix,
// index), the receiver can also reject a resume that no longer matches the data
// it already holds - a changed source is a new chain, never a corrupted one.
func (c *Client) Push(opts PushOptions) (*PushResult, error) {
	if err := ValidateSource(opts.Source); err != nil {
		return nil, err
	}
	if opts.Reader == nil {
		return nil, fmt.Errorf("push needs an input stream")
	}
	if opts.Kind == "" {
		opts.Kind = "tar"
	}

	state := opts.State
	if state == nil {
		var err error
		state, err = NewChainState(opts.Source, opts.Kind)
		if err != nil {
			return nil, err
		}
	}
	if state.Source != opts.Source {
		return nil, fmt.Errorf("resume state belongs to %s, not %s", state.Source, opts.Source)
	}
	if err := validChainID(state.Chain); err != nil {
		return nil, err
	}
	dek, err := state.dek()
	if err != nil {
		return nil, err
	}
	prefix, err := state.prefix()
	if err != nil {
		return nil, err
	}

	stored, err := c.storedChunks(opts.Source, state.Chain)
	if err != nil {
		return nil, err
	}

	manifest := &Manifest{
		Version:        EnvelopeVersion,
		Source:         opts.Source,
		Chain:          state.Chain,
		Kind:           state.Kind,
		FromSnapshot:   opts.FromSnapshot,
		ToSnapshot:     opts.ToSnapshot,
		FromGUID:       opts.FromGUID,
		ToGUID:         opts.ToGUID,
		CreatedAt:      time.Now().UTC(),
		StreamPrefix:   state.Prefix,
		ChunkPlainSize: ChunkPlainSize,
	}
	result := &PushResult{Source: opts.Source, Chain: state.Chain, State: state}

	buf := make([]byte, ChunkPlainSize)
	for index := 0; ; index++ {
		n, readErr := io.ReadFull(opts.Reader, buf)
		if readErr == io.EOF && n == 0 {
			break
		}
		if readErr != nil && readErr != io.EOF && readErr != io.ErrUnexpectedEOF {
			return nil, fmt.Errorf("reading input: %w", readErr)
		}

		plain := buf[:n]
		sealed, sha, err := SealChunk(dek, prefix, opts.Source, state.Chain, index, plain)
		if err != nil {
			return nil, err
		}
		entry := ManifestChunk{
			Index:       index,
			PlainBytes:  n,
			SealedBytes: len(sealed),
			Sha256Plain: sha,
		}

		if storedDigest, ok := stored[index]; ok {
			// Already on the receiver: describe it, do not re-upload. The sealed
			// size is deterministic, so the manifest stays exact. If the bytes
			// differ the source changed since the interruption and continuing
			// would produce a chain that cannot be restored - fail instead.
			if storedDigest != digestOf(sealed) {
				return result, fmt.Errorf(
					"the receiver already holds different bytes for chunk %d of chain %s: the source changed since the push was interrupted, start a new chain",
					index, state.Chain)
			}
			entry.SealedBytes = n + sealedOverhead
			result.Skipped++
		} else {
			path := fmt.Sprintf("/chunks/%s?chain=%s&index=%d", opts.Source, state.Chain, index)
			if _, _, err := c.do(http.MethodPut, path, sealed); err != nil {
				return result, err
			}
			result.Uploaded++
		}

		result.Chunks++
		result.PlainBytes += int64(n)
		result.SealedBytes += int64(entry.SealedBytes)
		manifest.Chunks = append(manifest.Chunks, entry)
		if opts.Progress != nil {
			opts.Progress(PushProgress{
				Chunk:       index,
				PlainBytes:  result.PlainBytes,
				SealedBytes: result.SealedBytes,
				Uploaded:    result.Uploaded,
				Skipped:     result.Skipped,
			})
		}

		if readErr != nil {
			break // EOF or a short final chunk
		}
	}

	if len(manifest.Chunks) == 0 {
		return result, fmt.Errorf("nothing to back up: the input stream produced no data")
	}

	kek, err := c.Identity.KEKBytes()
	if err != nil {
		return result, err
	}
	wrapped, err := WrapDEK(kek, dek, opts.Source, state.Chain)
	if err != nil {
		return result, err
	}
	manifest.DEKWrapped = wrapped
	if err := manifest.Sign(c.Identity); err != nil {
		return result, err
	}

	payload, err := json.Marshal(manifest)
	if err != nil {
		return result, err
	}
	if _, _, err := c.do(http.MethodPut, "/manifest/"+opts.Source, payload); err != nil {
		return result, err
	}

	if opts.PruneKeep > 0 {
		removed, err := c.Prune(opts.Source, opts.PruneKeep)
		if err != nil {
			return result, err
		}
		result.PrunedChains = removed
	}

	result.Manifest = manifest
	return result, nil
}

// RestoreOptions describes a restore from a receiver.
type RestoreOptions struct {
	Source string
	// Chain is the chain to restore; empty means the current one.
	Chain string
	// Out receives the decrypted stream (a tar extractor, a zfs receive pipe, a
	// verification buffer...).
	Out io.Writer
	// Progress is called after every chunk (nil is fine).
	Progress func(RestoreProgress)
	// SkipSignatureCheck accepts a manifest whose signature does not verify with
	// this identity. Only for forensics on damaged data.
	SkipSignatureCheck bool
}

// RestoreProgress is one restore progress tick.
type RestoreProgress struct {
	Chunk      int   `json:"chunk"`
	Chunks     int   `json:"chunks"`
	PlainBytes int64 `json:"plainBytes"`
}

// RestoreResult summarises a restore.
type RestoreResult struct {
	Source     string    `json:"source"`
	Chain      string    `json:"chain"`
	Chunks     int       `json:"chunks"`
	PlainBytes int64     `json:"plainBytes"`
	Manifest   *Manifest `json:"-"`
}

// Restore pulls the stored chunks back, decrypts them and writes the original
// stream to Out. It verifies three things before trusting a byte: the manifest
// signature (it is the sender's own), the chain's contiguity (a missing chunk
// would silently corrupt a zfs send stream), and each chunk's SHA-256 against the
// signed manifest (the receiver cannot forge that).
func (c *Client) Restore(opts RestoreOptions) (*RestoreResult, error) {
	if err := ValidateSource(opts.Source); err != nil {
		return nil, err
	}
	if opts.Out == nil {
		return nil, fmt.Errorf("restore needs an output stream")
	}

	manifest, err := c.Manifest(opts.Source, opts.Chain)
	if err != nil {
		return nil, err
	}
	if !opts.SkipSignatureCheck {
		if err := manifest.VerifySignature(c.Identity.PublicKey); err != nil {
			return nil, fmt.Errorf("refusing to restore: %w", err)
		}
	}
	kek, err := c.Identity.KEKBytes()
	if err != nil {
		return nil, err
	}
	dek, err := UnwrapDEK(kek, manifest.DEKWrapped, manifest.Source, manifest.Chain)
	if err != nil {
		return nil, err
	}

	result := &RestoreResult{Source: manifest.Source, Chain: manifest.Chain, Manifest: manifest}

	// Chunks are written in index order: for a zfs send stream the chunk order is
	// the stream, so ordering is not cosmetic.
	chunks := append([]ManifestChunk(nil), manifest.Chunks...)
	sort.Slice(chunks, func(i, j int) bool { return chunks[i].Index < chunks[j].Index })

	for expected, chunk := range chunks {
		if chunk.Index != expected {
			return result, fmt.Errorf("chain %s is not contiguous: expected chunk %d, found %d",
				manifest.Chain, expected, chunk.Index)
		}
		sealed, err := c.fetchChunk(manifest.Source, manifest.Chain, chunk.Index)
		if err != nil {
			return result, err
		}
		plain, err := OpenChunk(dek, manifest.Source, manifest.Chain, chunk.Index, sealed)
		if err != nil {
			return result, err
		}
		if digestOf(plain) != chunk.Sha256Plain {
			return result, fmt.Errorf("chunk %d does not match the signed manifest", chunk.Index)
		}
		if _, err := opts.Out.Write(plain); err != nil {
			return result, fmt.Errorf("writing output: %w", err)
		}
		result.Chunks++
		result.PlainBytes += int64(len(plain))
		if opts.Progress != nil {
			opts.Progress(RestoreProgress{
				Chunk:      chunk.Index,
				Chunks:     len(chunks),
				PlainBytes: result.PlainBytes,
			})
		}
	}
	return result, nil
}

// fetchChunk downloads one sealed chunk.
func (c *Client) fetchChunk(source, chain string, index int) ([]byte, error) {
	data, _, err := c.do(http.MethodGet, fmt.Sprintf("/chunks/%s?chain=%s&index=%d", source, chain, index), nil)
	if err != nil {
		return nil, err
	}
	return data, nil
}
