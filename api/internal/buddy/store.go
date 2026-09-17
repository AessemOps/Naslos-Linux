package buddy

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"log"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"
)

// Store is the receiver-side storage: opaque sealed chunks plus signed manifests,
// rooted at one directory. On a Naslos instance that root is a ZFS dataset; in
// the standalone container it is a mounted volume. Nothing here can decrypt: the
// store never sees a key.
type Store struct {
	root string

	// mu guards the per-key usage cache. Quota checks happen on every chunk
	// upload, and walking the tree each time would be quadratic over the life of
	// a chain, so usage is computed once per key and then maintained.
	mu     sync.Mutex
	usage  map[string]int64
	loaded map[string]bool
}

// NewStore roots a store at dir.
func NewStore(dir string) *Store {
	return &Store{
		root:   dir,
		usage:  make(map[string]int64),
		loaded: make(map[string]bool),
	}
}

// Root is the storage root.
func (s *Store) Root() string { return s.root }

// enrollmentMarker is the file that records a spent enrollment token.
const enrollmentMarker = ".enroll-used"

// MarkEnrollmentUsed records that the one-time enrollment token has been spent.
// It is a file rather than a flag because the flag lived in memory: restarting
// the receiver re-armed the token, so a leaked token worked again after every
// restart (NAS-011).
func (s *Store) MarkEnrollmentUsed() error {
	if err := os.MkdirAll(s.root, 0o755); err != nil {
		return err
	}
	return writeFileAtomic(filepath.Join(s.root, enrollmentMarker),
		[]byte(time.Now().UTC().Format(time.RFC3339)+"\n"))
}

// EnrollmentUsed reports whether the enrollment token has already been spent.
func (s *Store) EnrollmentUsed() bool {
	_, err := os.Stat(filepath.Join(s.root, enrollmentMarker))
	return err == nil
}

// keyDir maps a key fingerprint to a directory name.
//
// A fingerprint is derived from a base64 hash, so it can contain '/', '+' and ':';
// '/' in particular would silently turn one key's tree into nested directories - it
// did, for the first instance-side sender (SHA256:du3R5I6q+piUaqr61Y/NV5FZv6qk0y…),
// whose backups became unreachable. The directory is therefore a hash of the
// fingerprint, not a sanitised copy of it, and nothing ever parses it back: callers
// that walk the tree work with these names directly.
func keyDir(fingerprint string) string {
	sum := sha256.Sum256([]byte(fingerprint))
	return "k" + hex.EncodeToString(sum[:16])
}

// sourcesIn lists the logical sources stored under a key directory.
//
// A source may be nested (`naslos-a/media/films`), so the walk is recursive: any
// directory holding a current.json is a source, and its path relative to the key
// directory is its name. The previous two-level walk hid a three-level source
// from listings while the quota still counted its bytes (NAS-021).
func (s *Store) sourcesIn(keyDirName string) ([]string, error) {
	root := filepath.Join(s.root, keyDirName)
	if _, err := os.Stat(root); err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, err
	}

	var sources []string
	err := filepath.Walk(root, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			// A source that vanished mid-walk is not an error for a listing.
			if os.IsNotExist(err) {
				return nil
			}
			return err
		}
		if !info.IsDir() || path == root {
			return nil
		}
		if _, err := os.Stat(filepath.Join(path, "current.json")); err != nil {
			return nil
		}
		relative, err := filepath.Rel(root, path)
		if err != nil {
			return nil
		}
		sources = append(sources, filepath.ToSlash(relative))
		// A source's own tree holds no nested sources.
		return filepath.SkipDir
	})
	if err != nil {
		return nil, err
	}
	sort.Strings(sources)
	return sources, nil
}

// sourceDirIn is <root>/<keyDir>/<source>, validating every component so a peer can
// never write outside its own tree.
func (s *Store) sourceDirIn(keyDirName, source string) (string, error) {
	if err := ValidateSource(source); err != nil {
		return "", err
	}
	if strings.TrimSpace(keyDirName) == "" {
		return "", fmt.Errorf("key id is required")
	}
	return filepath.Join(s.root, keyDirName, source), nil
}

// sourceDir is <root>/<key>/<source>, validating every component so a peer can
// never write outside its own tree.
func (s *Store) sourceDir(keyID, source string) (string, error) {
	if strings.TrimSpace(keyID) == "" {
		return "", fmt.Errorf("key id is required")
	}
	return s.sourceDirIn(keyDir(keyID), source)
}

// chainDirIn is the directory holding one chain's chunks, by key directory name.
func (s *Store) chainDirIn(keyDirName, source, chain string) (string, error) {
	if chain == "" || strings.ContainsAny(chain, "/\\") || strings.Contains(chain, "..") {
		return "", fmt.Errorf("invalid chain id %q", chain)
	}
	dir, err := s.sourceDirIn(keyDirName, source)
	if err != nil {
		return "", err
	}
	return filepath.Join(dir, "chains", chain), nil
}

// manifestIn reads the current manifest of a source, by key directory name.
func (s *Store) manifestIn(keyDirName, source string) (*Manifest, error) {
	dir, err := s.sourceDirIn(keyDirName, source)
	if err != nil {
		return nil, err
	}
	raw, err := os.ReadFile(filepath.Join(dir, "current.json"))
	if err != nil {
		if os.IsNotExist(err) {
			return nil, fmt.Errorf("no backup stored for %s", source)
		}
		return nil, err
	}
	var manifest Manifest
	if err := json.Unmarshal(raw, &manifest); err != nil {
		return nil, fmt.Errorf("parsing manifest for %s: %w", source, err)
	}
	return &manifest, nil
}

// chainDir is the directory holding one chain's chunks.
func (s *Store) chainDir(keyID, source, chain string) (string, error) {
	if chain == "" || strings.ContainsAny(chain, "/\\") || strings.Contains(chain, "..") {
		return "", fmt.Errorf("invalid chain id %q", chain)
	}
	dir, err := s.sourceDir(keyID, source)
	if err != nil {
		return "", err
	}
	return filepath.Join(dir, "chains", chain), nil
}

// chunkName is the zero-padded file name of a chunk.
func chunkName(index int) string {
	return fmt.Sprintf("chunk-%06d.enc", index)
}

// PutChunk stores one sealed chunk.
//
// A chunk that already exists must normally be byte-identical: a published chain is
// immutable, because a restore may already reference those bytes. The exception is
// an *unpublished* chain (an interrupted push), where the highest chunk may be
// rewritten: that is the partial tail a stream that died in the middle left behind,
// and the sender re-sends it whole when it resumes. Only the highest index may be
// replaced, so a peer can never mix two versions of the data into one chain by
// rewriting something that already has successors.
func (s *Store) PutChunk(keyID, source, chain string, index int, sealed []byte) error {
	return s.PutChunkWithin(keyID, source, chain, index, sealed, 0)
}

// PutChunkWithin is PutChunk with a per-key quota (0 = unlimited).
//
// The quota is enforced atomically with the accounting: the bytes are reserved
// under the store lock *before* they are written, so two concurrent uploads
// cannot both pass a check that was true only before either of them wrote
// (NAS-013). A chunk that is already stored with the same digest is a no-op that
// costs nothing, which is what lets a sender resume a chain that already fills
// the quota - the old pre-check charged it again and answered 413.
func (s *Store) PutChunkWithin(keyID, source, chain string, index int, sealed []byte, quota int64) error {
	if index < 0 {
		return fmt.Errorf("invalid chunk index %d", index)
	}
	if len(sealed) > MaxSealedChunkSize {
		return fmt.Errorf("chunk is %d bytes, the maximum is %d", len(sealed), MaxSealedChunkSize)
	}

	dir, err := s.chainDir(keyID, source, chain)
	if err != nil {
		return err
	}
	if err := os.MkdirAll(dir, 0755); err != nil {
		return err
	}

	target := filepath.Join(dir, chunkName(index))
	var replaced int64
	if existing, err := os.ReadFile(target); err == nil {
		if digestOf(existing) == digestOf(sealed) {
			return nil // already stored by an earlier attempt
		}
		allowed, err := s.mayReplace(keyID, source, chain, index)
		if err != nil {
			return err
		}
		if !allowed {
			return fmt.Errorf("chunk %d already exists with different content", index)
		}
		replaced = int64(len(existing))
	}

	// Reserve before creating anything. The usage baseline is computed from disk
	// the first time a key is seen, so a temp file that exists at that moment
	// would be counted as already-stored bytes and then charged again.
	delta := int64(len(sealed)) - replaced
	if err := s.reserveUsage(keyID, delta, quota); err != nil {
		return err
	}
	stored := false
	defer func() {
		if !stored {
			s.releaseUsage(keyID, delta)
		}
	}()

	tmp, err := os.CreateTemp(dir, ".chunk-*.tmp")
	if err != nil {
		return err
	}
	tmpName := tmp.Name()
	defer os.Remove(tmpName)

	if _, err := tmp.Write(sealed); err != nil {
		tmp.Close()
		return err
	}
	if err := tmp.Sync(); err != nil {
		tmp.Close()
		return err
	}
	if err := tmp.Close(); err != nil {
		return err
	}
	if err := os.Rename(tmpName, target); err != nil {
		return err
	}
	stored = true
	return nil
}

// QuotaError reports that a write would push a key past its quota.
type QuotaError struct {
	Used   int64
	Quota  int64
	Needed int64
}

func (e *QuotaError) Error() string {
	return fmt.Sprintf("quota exceeded: %d of %d bytes are already stored for this key", e.Used, e.Quota)
}

// Is lets callers match a quota refusal with errors.Is.
func (e *QuotaError) Is(target error) bool { return target == ErrQuotaExceeded }

// ErrQuotaExceeded is the sentinel for a quota refusal.
var ErrQuotaExceeded = errors.New("quota exceeded")

// reserveUsage charges delta against a key's usage, refusing the charge when it
// would exceed quota (0 = unlimited). Under the store lock, so concurrent writers
// serialize here rather than both reading a stale usage.
func (s *Store) reserveUsage(keyID string, delta, quota int64) error {
	if delta == 0 {
		return nil
	}
	s.mu.Lock()
	defer s.mu.Unlock()

	used, err := s.usageLocked(keyID)
	if err != nil {
		return err
	}
	if quota > 0 && delta > 0 && used+delta > quota {
		return &QuotaError{Used: used, Quota: quota, Needed: delta}
	}
	if _, tracked := s.usage[keyID]; tracked || s.loaded[keyID] {
		s.usage[keyID] += delta
	}
	return nil
}

// releaseUsage returns a reservation after a failed write.
func (s *Store) releaseUsage(keyID string, delta int64) {
	if delta == 0 {
		return
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if _, tracked := s.usage[keyID]; tracked || s.loaded[keyID] {
		s.usage[keyID] -= delta
	}
}

// mayReplace reports whether a chunk of an unfinished chain can be rewritten: only
// the highest index of a chain that has no manifest yet.
func (s *Store) mayReplace(keyID, source, chain string, index int) (bool, error) {
	published, err := s.chainPublished(keyID, source, chain)
	if err != nil {
		return false, err
	}
	if published {
		return false, nil
	}

	indices, err := s.ListChunks(keyID, source, chain)
	if err != nil {
		return false, err
	}
	if len(indices) == 0 {
		return false, nil
	}
	return indices[len(indices)-1] == index, nil
}

// chainPublished reports whether a chain has a manifest, i.e. whether it is a
// finished backup rather than an upload in progress.
func (s *Store) chainPublished(keyID, source, chain string) (bool, error) {
	dir, err := s.chainDir(keyID, source, chain)
	if err != nil {
		return false, err
	}
	if _, err := os.Stat(filepath.Join(dir, "manifest.json")); err == nil {
		return true, nil
	} else if !os.IsNotExist(err) {
		return false, err
	}
	return false, nil
}

// Chunk reads a sealed chunk back (what a restore does).
func (s *Store) Chunk(keyID, source, chain string, index int) ([]byte, error) {
	dir, err := s.chainDir(keyID, source, chain)
	if err != nil {
		return nil, err
	}
	data, err := os.ReadFile(filepath.Join(dir, chunkName(index)))
	if err != nil {
		if os.IsNotExist(err) {
			return nil, fmt.Errorf("chunk %d is not stored", index)
		}
		return nil, err
	}
	return data, nil
}

// ListChunks returns the chunk indices present for a chain, which is how a sender
// resumes an interrupted push.
func (s *Store) ListChunks(keyID, source, chain string) ([]int, error) {
	dir, err := s.chainDir(keyID, source, chain)
	if err != nil {
		return nil, err
	}
	entries, err := os.ReadDir(dir)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, err
	}

	var indices []int
	for _, entry := range entries {
		name := entry.Name()
		if !strings.HasPrefix(name, "chunk-") || !strings.HasSuffix(name, ".enc") {
			continue
		}
		index, err := strconv.Atoi(strings.TrimSuffix(strings.TrimPrefix(name, "chunk-"), ".enc"))
		if err != nil {
			continue
		}
		indices = append(indices, index)
	}
	sort.Ints(indices)
	return indices, nil
}

// ChunkDigests returns the SHA-256 of every sealed chunk stored for a chain. A
// sender uses it to prove that the bytes it is about to skip are the bytes it
// sent: resuming with a changed source must fail loudly instead of quietly mixing
// two versions of the data into one chain.
func (s *Store) ChunkDigests(keyID, source, chain string) (map[int]string, error) {
	indices, err := s.ListChunks(keyID, source, chain)
	if err != nil {
		return nil, err
	}
	out := make(map[int]string, len(indices))
	for _, index := range indices {
		sealed, err := s.Chunk(keyID, source, chain, index)
		if err != nil {
			return nil, err
		}
		out[index] = digestOf(sealed)
	}
	return out, nil
}

// PutManifest stores a chain's manifest and points "current" at it, which is what
// the receiver reports as the latest backup for a source.
//
// It refuses to move "current" backwards: pointing the pointer at a chain that is
// already stored is a rollback (a compromised or stale sender key replaying an
// older manifest to make an old backup look like the latest one), and the
// receiver is the only party that can notice it - it cannot decrypt, so it
// enforces pointer monotonicity itself (NAS-012).
func (s *Store) PutManifest(keyID, source string, m *Manifest) error {
	dir, err := s.chainDir(keyID, source, m.Chain)
	if err != nil {
		return err
	}
	if err := os.MkdirAll(dir, 0755); err != nil {
		return err
	}
	data, err := json.MarshalIndent(m, "", "  ")
	if err != nil {
		return err
	}

	previous := ""
	current, currentErr := s.Manifest(keyID, source)
	if currentErr == nil {
		previous = current.Chain
	}

	// A chain that is already stored and is not the current one must not become
	// current again. Re-publishing the *current* chain is legitimate: that is what
	// a resumed (or retried) push does, with a fresh creation time.
	if previous != "" && previous != m.Chain {
		if _, err := os.Stat(filepath.Join(filepath.Dir(dir), m.Chain, "manifest.json")); err == nil {
			return fmt.Errorf("refusing to move %s back to the already-stored chain %s (rollback)", source, m.Chain)
		}
	}

	if err := writeFileAtomic(filepath.Join(dir, "manifest.json"), data); err != nil {
		return err
	}

	sourceDir, err := s.sourceDir(keyID, source)
	if err != nil {
		return err
	}
	if err := os.MkdirAll(sourceDir, 0755); err != nil {
		return err
	}
	// Keep the GUID index current so a restore can follow the history without
	// reading every manifest (NAS-021).
	if m.ToGUID != "" {
		links := s.readLinks(keyID, source)
		if links == nil {
			links = map[string]string{}
		}
		if links[m.ToGUID] != m.Chain {
			links[m.ToGUID] = m.Chain
			s.writeLinks(keyID, source, links)
		}
	}

	if previous != m.Chain {
		log.Printf("buddy receiver: %s current chain %s -> %s (%s)", source, orNone(previous), m.Chain, m.Kind)
	}
	return writeFileAtomic(filepath.Join(sourceDir, "current.json"), data)
}

// orNone renders an empty chain id for a log line.
func orNone(chain string) string {
	if chain == "" {
		return "(none)"
	}
	return chain
}

// Manifest returns the current manifest of a source (what a restore reads first).
func (s *Store) Manifest(keyID, source string) (*Manifest, error) {
	dir, err := s.sourceDir(keyID, source)
	if err != nil {
		return nil, err
	}
	data, err := os.ReadFile(filepath.Join(dir, "current.json"))
	if err != nil {
		if os.IsNotExist(err) {
			return nil, fmt.Errorf("no backup stored for %s", source)
		}
		return nil, err
	}
	var m Manifest
	if err := json.Unmarshal(data, &m); err != nil {
		return nil, fmt.Errorf("parsing manifest for %s: %w", source, err)
	}
	return &m, nil
}

// Sources lists the logical sources stored for a key.
func (s *Store) Sources(keyID string) ([]string, error) {
	return s.sourcesIn(keyDir(keyID))
}

// Backups summarises what is stored for one source.
type Backups struct {
	Source      string    `json:"source"`
	Chain       string    `json:"chain"`
	Kind        string    `json:"kind"`
	CreatedAt   time.Time `json:"createdAt"`
	Chunks      int       `json:"chunks"`
	StoredBytes int64     `json:"storedBytes"`
}

// ManifestForChain returns the manifest of one specific chain of a source.
func (s *Store) ManifestForChain(keyID, source, chain string) (*Manifest, error) {
	dir, err := s.chainDir(keyID, source, chain)
	if err != nil {
		return nil, err
	}
	raw, err := os.ReadFile(filepath.Join(dir, "manifest.json"))
	if err != nil {
		if os.IsNotExist(err) {
			return nil, fmt.Errorf("chain %s is not stored for %s", chain, source)
		}
		return nil, err
	}
	var manifest Manifest
	if err := json.Unmarshal(raw, &manifest); err != nil {
		return nil, fmt.Errorf("parsing manifest of chain %s: %w", chain, err)
	}
	return &manifest, nil
}

// MaxListedChains bounds a chain listing: a source that has grown for years must
// not make one request walk an unbounded directory tree (NAS-021). The listing
// reports `truncated` when it hit the cap; a restore does not use it (see
// Sequence), so nothing that needs the whole history is cut off by it.
const MaxListedChains = 500

// linksFile maps a chain's ToGUID to the chain that produced it, so a restore can
// follow a source's history backwards by reading one manifest per step instead of
// every manifest stored (NAS-021). It is a cache: a missing or stale entry falls
// back to a scan, which rewrites the index.
const linksFile = "links.json"

// readLinks loads the GUID index of a source (ToGUID -> chain).
func (s *Store) readLinks(keyID, source string) map[string]string {
	dir, err := s.sourceDir(keyID, source)
	if err != nil {
		return nil
	}
	raw, err := os.ReadFile(filepath.Join(dir, "chains", linksFile))
	if err != nil {
		return nil
	}
	links := map[string]string{}
	if err := json.Unmarshal(raw, &links); err != nil {
		return nil
	}
	return links
}

// writeLinks persists the GUID index, best effort: losing it only costs a scan.
func (s *Store) writeLinks(keyID, source string, links map[string]string) {
	dir, err := s.sourceDir(keyID, source)
	if err != nil {
		return
	}
	if err := os.MkdirAll(filepath.Join(dir, "chains"), 0o755); err != nil {
		return
	}
	data, err := json.MarshalIndent(links, "", "  ")
	if err != nil {
		return
	}
	_ = writeFileAtomic(filepath.Join(dir, "chains", linksFile), data)
}

// ChainCount counts a source's stored chains without reading their manifests, so
// a truncated listing can still report the real total cheaply.
func (s *Store) ChainCount(keyID, source string) (int, error) {
	dir, err := s.sourceDir(keyID, source)
	if err != nil {
		return 0, err
	}
	entries, err := os.ReadDir(filepath.Join(dir, "chains"))
	if err != nil {
		if os.IsNotExist(err) {
			return 0, nil
		}
		return 0, err
	}
	count := 0
	for _, entry := range entries {
		if entry.IsDir() {
			count++
		}
	}
	return count, nil
}

// Sequence returns the chains needed to rebuild a source, oldest first: the last
// full send plus every incremental that follows it. It follows the FromGUID links
// from the requested chain (or the current one) backwards, so the work is
// proportional to the sequence rather than to the source's whole history.
func (s *Store) Sequence(keyID, source, chain string) ([]Manifest, error) {
	target, err := s.manifestFor(keyID, source, chain)
	if err != nil {
		return nil, err
	}
	if target == nil {
		return nil, fmt.Errorf("no backup of %s is stored here", source)
	}

	links := s.readLinks(keyID, source)
	needsScan := links == nil
	byToGUID := map[string]Manifest{}

	load := func(chainID string) (*Manifest, error) {
		if manifest, ok := byToGUID[chainID]; ok {
			return &manifest, nil
		}
		return s.manifestFor(keyID, source, chainID)
	}

	sequence := make([]Manifest, 0, 8)
	current := target
	seen := map[string]bool{}
	for current != nil {
		if seen[current.Chain] {
			return nil, fmt.Errorf("the chain history of %s contains a loop at %s", source, current.Chain)
		}
		seen[current.Chain] = true
		sequence = append(sequence, *current)

		if current.FromGUID == "" {
			break
		}
		next := ""
		if !needsScan {
			next = links[current.FromGUID]
		}
		if next == "" {
			// No index (or a stale entry): find the chain that produced the base
			// GUID, rebuild the index from what is stored, and continue.
			all, err := s.Chains(keyID, source)
			if err != nil {
				return nil, err
			}
			rebuilt := make(map[string]string, len(all))
			for _, candidate := range all {
				if candidate.ToGUID != "" {
					rebuilt[candidate.ToGUID] = candidate.Chain
					byToGUID[candidate.Chain] = candidate
				}
			}
			links = rebuilt
			needsScan = false
			s.writeLinks(keyID, source, rebuilt)
			next = links[current.FromGUID]
		}
		if next == "" {
			// The base is not stored: the sequence cannot be rebuilt from here.
			// Restore refuses up front rather than applying a partial stream.
			return nil, fmt.Errorf("the chain %s needs the chain that produced GUID %s, which is not stored here",
				current.Chain, current.FromGUID)
		}
		base, err := load(next)
		if err != nil {
			return nil, err
		}
		if base == nil {
			// The index named a chain whose manifest is gone: refuse rather than
			// return a partial sequence.
			return nil, fmt.Errorf("the chain %s needs the chain that produced GUID %s, which is not stored here",
				current.Chain, current.FromGUID)
		}
		current = base
	}

	// Reverse: oldest first.
	for i, j := 0, len(sequence)-1; i < j; i, j = i+1, j-1 {
		sequence[i], sequence[j] = sequence[j], sequence[i]
	}
	return sequence, nil
}

// manifestFor reads one manifest: the current one when chain is empty, else that
// chain's. It returns (nil, nil) when the source has no current manifest.
func (s *Store) manifestFor(keyID, source, chain string) (*Manifest, error) {
	if chain == "" {
		manifest, err := s.Manifest(keyID, source)
		if err != nil {
			if os.IsNotExist(err) {
				return nil, nil
			}
			return nil, err
		}
		return manifest, nil
	}
	dir, err := s.chainDir(keyID, source, chain)
	if err != nil {
		return nil, err
	}
	raw, err := os.ReadFile(filepath.Join(dir, "manifest.json"))
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, err
	}
	var manifest Manifest
	if err := json.Unmarshal(raw, &manifest); err != nil {
		return nil, err
	}
	return &manifest, nil
}

// Chains returns every chain manifest stored for a source, newest first. A restore
// needs the whole chain, not just the current one: an incremental stream can only be
// applied on top of the chain it was taken from.
func (s *Store) Chains(keyID, source string) ([]Manifest, error) {
	return s.ChainsLimited(keyID, source, MaxListedChains)
}

// ChainsLimited is Chains with an explicit cap (0 means no cap). It exists so the
// HTTP layer can report truncation, and so tests can stay small.
func (s *Store) ChainsLimited(keyID, source string, limit int) ([]Manifest, error) {
	dir, err := s.sourceDir(keyID, source)
	if err != nil {
		return nil, err
	}
	entries, err := os.ReadDir(filepath.Join(dir, "chains"))
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, err
	}

	manifests := make([]Manifest, 0, len(entries))
	for _, entry := range entries {
		if !entry.IsDir() {
			continue
		}
		raw, err := os.ReadFile(filepath.Join(dir, "chains", entry.Name(), "manifest.json"))
		if err != nil {
			continue // an unfinished chain has no manifest yet
		}
		var manifest Manifest
		if err := json.Unmarshal(raw, &manifest); err != nil {
			continue
		}
		manifests = append(manifests, manifest)
	}
	sort.Slice(manifests, func(i, j int) bool { return manifests[i].CreatedAt.After(manifests[j].CreatedAt) })
	if limit > 0 && len(manifests) > limit {
		manifests = manifests[:limit]
	}
	return manifests, nil
}
func (s *Store) Summary(keyID string) ([]Backups, error) {
	// Work with directory names rather than fingerprints: a fingerprint cannot be
	// parsed back out of a directory name, and it never needs to be.
	keyDirs := []string{keyDir(keyID)}
	if keyID == "" {
		entries, err := os.ReadDir(s.root)
		if err != nil {
			if os.IsNotExist(err) {
				return nil, nil
			}
			return nil, err
		}
		keyDirs = keyDirs[:0]
		for _, entry := range entries {
			if entry.IsDir() {
				keyDirs = append(keyDirs, entry.Name())
			}
		}
	}

	var out []Backups
	for _, dir := range keyDirs {
		sources, err := s.sourcesIn(dir)
		if err != nil {
			return nil, err
		}
		for _, source := range sources {
			manifest, err := s.manifestIn(dir, source)
			if err != nil {
				continue
			}
			row := Backups{
				Source:    source,
				Chain:     manifest.Chain,
				Kind:      manifest.Kind,
				CreatedAt: manifest.CreatedAt,
				Chunks:    len(manifest.Chunks),
			}
			if chainDir, err := s.chainDirIn(dir, source, manifest.Chain); err == nil {
				if size, err := dirSize(chainDir); err == nil {
					row.StoredBytes = size
				}
			}
			out = append(out, row)
		}
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Source < out[j].Source })
	return out, nil
}

// Usage is the bytes stored for a key (or for every key when keyID is empty).
func (s *Store) Usage(keyID string) (int64, error) {
	if keyID == "" {
		return s.uncachedUsage()
	}

	s.mu.Lock()
	defer s.mu.Unlock()
	return s.usageLocked(keyID)
}

// usageLocked returns the cached usage of a key, computing it once.
func (s *Store) usageLocked(keyID string) (int64, error) {
	if s.loaded[keyID] {
		return s.usage[keyID], nil
	}
	size, err := dirSize(filepath.Join(s.root, keyDir(keyID)))
	if err != nil {
		if !os.IsNotExist(err) {
			return 0, err
		}
		size = 0
	}
	s.usage[keyID] = size
	s.loaded[keyID] = true
	return size, nil
}

// uncachedUsage walks the whole tree (used for reporting, not for quota checks).
func (s *Store) uncachedUsage() (int64, error) {
	size, err := dirSize(s.root)
	if err != nil {
		if !os.IsNotExist(err) {
			return 0, err
		}
		return 0, nil
	}
	return size, nil
}

// InvalidateUsage forgets a key's cached usage so the next read recomputes it
// from disk. Called after a prune and after a manifest write (the manifest is
// stored bytes the running total did not include).
func (s *Store) InvalidateUsage(keyID string) { s.invalidateUsage(keyID) }

// invalidateUsage forgets a key's cached usage after a prune.
func (s *Store) invalidateUsage(keyID string) {
	s.mu.Lock()
	defer s.mu.Unlock()

	delete(s.usage, keyID)
	delete(s.loaded, keyID)
}

// FreeSpace reports the bytes available on the filesystem holding the store: the
// "space left on the backup pool" the sender displays.
func (s *Store) FreeSpace() (int64, error) {
	if err := os.MkdirAll(s.root, 0755); err != nil {
		return 0, err
	}
	var stat syscall.Statfs_t
	if err := syscall.Statfs(s.root, &stat); err != nil {
		return 0, err
	}
	return int64(stat.Bavail) * int64(stat.Bsize), nil
}

// LastBackup is the newest stored backup time for a key (zero when none).
func (s *Store) LastBackup(keyID string) (time.Time, error) {
	backups, err := s.Summary(keyID)
	if err != nil {
		return time.Time{}, err
	}
	var newest time.Time
	for _, b := range backups {
		if b.CreatedAt.After(newest) {
			newest = b.CreatedAt
		}
	}
	return newest, nil
}

// Prune keeps the newest `keep` chains of a source and deletes the rest: the
// retention policy the owner asks the receiver to enforce.
//
// It never deletes a chain the newest one descends from. An incremental zfs-send
// chain can only be rebuilt with every chain below it, so a naive keep-count
// would leave a backup that looks fine (the send succeeded, the manifest is
// signed) and only fails at restore or verify time. Keeping a few more chains
// than asked is the safe side of that trade; the alternative is silent data loss.
func (s *Store) Prune(keyID, source string, keep int) (int, error) {
	if keep < 1 {
		return 0, fmt.Errorf("keep must be at least 1")
	}
	dir, err := s.sourceDir(keyID, source)
	if err != nil {
		return 0, err
	}
	chainsDir := filepath.Join(dir, "chains")
	entries, err := os.ReadDir(chainsDir)
	if err != nil {
		if os.IsNotExist(err) {
			return 0, nil
		}
		return 0, err
	}

	type chainInfo struct {
		name     string
		when     time.Time
		fromGUID string
		toGUID   string
	}
	chains := make([]chainInfo, 0, len(entries))
	for _, entry := range entries {
		if !entry.IsDir() {
			continue
		}
		info := chainInfo{name: entry.Name()}
		if raw, err := os.ReadFile(filepath.Join(chainsDir, entry.Name(), "manifest.json")); err == nil {
			var manifest Manifest
			if json.Unmarshal(raw, &manifest) == nil {
				info.when = manifest.CreatedAt
				info.fromGUID = manifest.FromGUID
				info.toGUID = manifest.ToGUID
			}
		}
		chains = append(chains, info)
	}
	sort.Slice(chains, func(i, j int) bool { return chains[i].when.After(chains[j].when) })

	// Survivorship follows the receiver's own current pointer, not the sender's
	// timestamps: a rewritten CreatedAt must not be able to make the live chain
	// look like the oldest one and get it pruned (NAS-012).
	currentChain := ""
	if raw, err := os.ReadFile(filepath.Join(dir, "current.json")); err == nil {
		var current Manifest
		if json.Unmarshal(raw, &current) == nil {
			currentChain = current.Chain
		}
	}

	// Walk the dependency chain of the current backup: the current chain, then
	// the chain whose ToGUID is its FromGUID, and so on.
	required := map[string]bool{}
	newest := ""
	if currentChain != "" {
		newest = currentChain
	} else if len(chains) > 0 {
		// No current pointer yet (an interrupted first push): fall back to the
		// newest by creation time.
		newest = chains[0].name
	}
	if newest != "" {
		required[newest] = true
		fromGUID := ""
		for _, chain := range chains {
			if chain.name == newest {
				fromGUID = chain.fromGUID
				break
			}
		}
		for fromGUID != "" {
			next := ""
			for _, candidate := range chains {
				if candidate.toGUID == fromGUID && !required[candidate.name] {
					next = candidate.name
					fromGUID = candidate.fromGUID
					break
				}
			}
			if next == "" {
				break
			}
			required[next] = true
		}
	}

	// Delete oldest-first until only `keep` chains remain, never touching a chain
	// the current backup descends from.
	remaining, removed := len(chains), 0
	for i := len(chains) - 1; i >= 0 && remaining > keep; i-- {
		chain := chains[i]
		if required[chain.name] {
			continue
		}
		if err := os.RemoveAll(filepath.Join(chainsDir, chain.name)); err == nil {
			removed++
			remaining--
		}
	}
	if removed > 0 {
		s.invalidateUsage(keyID)
		// The index pointed at chains that no longer exist: rebuild it from what
		// survived.
		links := make(map[string]string, len(chains))
		for _, chain := range chains {
			if chain.toGUID == "" {
				continue
			}
			if _, err := os.Stat(filepath.Join(chainsDir, chain.name)); err == nil {
				links[chain.toGUID] = chain.name
			}
		}
		s.writeLinks(keyID, source, links)
	}
	return removed, nil
}

// dirSize sums the size of every regular file under dir, ignoring the store's own
// temporary artifacts (they are dot-prefixed and must never count towards usage:
// a stray temp file from a crashed write used to inflate a key's usage until the
// process restarted).
func dirSize(dir string) (int64, error) {
	var total int64
	err := filepath.Walk(dir, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return err
		}
		if info.Mode().IsRegular() && !strings.HasPrefix(info.Name(), ".") {
			total += info.Size()
		}
		_ = path
		return nil
	})
	return total, err
}

// writeFileAtomic writes data to path via a temp file + rename.
func writeFileAtomic(path string, data []byte) error {
	dir := filepath.Dir(path)
	tmp, err := os.CreateTemp(dir, ".write-*.tmp")
	if err != nil {
		return err
	}
	tmpName := tmp.Name()
	defer os.Remove(tmpName)

	if _, err := tmp.Write(data); err != nil {
		tmp.Close()
		return err
	}
	if err := tmp.Close(); err != nil {
		return err
	}
	if err := os.Chmod(tmpName, 0644); err != nil {
		return err
	}
	return os.Rename(tmpName, path)
}
