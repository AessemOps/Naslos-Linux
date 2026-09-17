package buddy

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
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

// sourcesIn lists the logical sources stored under a key directory (sources may be
// nested, so one level is walked).
func (s *Store) sourcesIn(keyDirName string) ([]string, error) {
	root := filepath.Join(s.root, keyDirName)
	entries, err := os.ReadDir(root)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, err
	}

	var sources []string
	for _, entry := range entries {
		if !entry.IsDir() {
			continue
		}
		if _, err := os.Stat(filepath.Join(root, entry.Name(), "current.json")); err == nil {
			sources = append(sources, entry.Name())
			continue
		}
		nested, _ := os.ReadDir(filepath.Join(root, entry.Name()))
		for _, child := range nested {
			if !child.IsDir() {
				continue
			}
			candidate := entry.Name() + "/" + child.Name()
			if _, err := os.Stat(filepath.Join(root, candidate, "current.json")); err == nil {
				sources = append(sources, candidate)
			}
		}
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
	s.addUsage(keyID, int64(len(sealed))-replaced)
	return nil
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
	return writeFileAtomic(filepath.Join(sourceDir, "current.json"), data)
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

// Chains returns every chain manifest stored for a source, newest first. A restore
// needs the whole chain, not just the current one: an incremental stream can only be
// applied on top of the chain it was taken from.
func (s *Store) Chains(keyID, source string) ([]Manifest, error) {
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

// addUsage accounts for newly stored bytes.
func (s *Store) addUsage(keyID string, delta int64) {
	s.mu.Lock()
	defer s.mu.Unlock()

	if _, ok := s.usage[keyID]; ok || s.loaded[keyID] {
		s.usage[keyID] += delta
	}
}

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
		name string
		when time.Time
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
			}
		}
		chains = append(chains, info)
	}
	sort.Slice(chains, func(i, j int) bool { return chains[i].when.After(chains[j].when) })

	removed := 0
	for i, chain := range chains {
		if i < keep {
			continue
		}
		if err := os.RemoveAll(filepath.Join(chainsDir, chain.name)); err == nil {
			removed++
		}
	}
	if removed > 0 {
		s.invalidateUsage(keyID)
	}
	return removed, nil
}

// dirSize sums the size of every regular file under dir.
func dirSize(dir string) (int64, error) {
	var total int64
	err := filepath.Walk(dir, func(_ string, info os.FileInfo, err error) error {
		if err != nil {
			return err
		}
		if info.Mode().IsRegular() {
			total += info.Size()
		}
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
