package buddy

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strings"
	"sync"
	"time"
)

// sourcePattern is the shape of a logical backup source ("naslos-a/tank/data").
// It becomes a directory name on the receiver, so it is validated strictly.
var sourcePattern = regexp.MustCompile(`^[a-z0-9][a-z0-9._/-]*$`)

// ValidateSource rejects anything that could escape the receiver's storage tree
// or collide with another peer's namespace.
func ValidateSource(source string) error {
	if strings.TrimSpace(source) == "" {
		return fmt.Errorf("source is required")
	}
	if len(source) > 200 {
		return fmt.Errorf("source must be 200 characters or fewer")
	}
	if strings.Contains(source, "..") || strings.HasPrefix(source, "/") {
		return fmt.Errorf("invalid source %q: it must be a relative logical name", source)
	}
	if !sourcePattern.MatchString(source) {
		return fmt.Errorf("invalid source %q: use lowercase letters, digits, '.', '_', '-' or '/'", source)
	}
	return nil
}

// Peer is a sender authorized to push to this instance.
type Peer struct {
	Name        string `json:"name"`
	PublicKey   string `json:"publicKey"`
	Fingerprint string `json:"fingerprint"`
	// AllowedSources scopes what this key may write. Each entry matches a source
	// exactly or as a prefix ("naslos-a/" allows "naslos-a/tank"). An empty list
	// allows any source: the operator authorized the key, so this is only depth.
	AllowedSources []string  `json:"allowedSources,omitempty"`
	QuotaBytes     int64     `json:"quotaBytes,omitempty"`
	Enabled        bool      `json:"enabled"`
	CreatedAt      time.Time `json:"createdAt"`
	LastSeenAt     time.Time `json:"lastSeenAt,omitempty"`
}

// Allows reports whether the peer may write the given source.
func (p *Peer) Allows(source string) bool {
	if len(p.AllowedSources) == 0 {
		return true
	}
	for _, allowed := range p.AllowedSources {
		allowed = strings.TrimSpace(allowed)
		if allowed == "" {
			continue
		}
		if source == allowed || strings.HasPrefix(source, strings.TrimSuffix(allowed, "/")+"/") {
			return true
		}
	}
	return false
}

// PeerStore is the peer registry, persisted as JSON next to the other Naslos
// state files with atomic writes.
type PeerStore struct {
	path string

	mu    sync.RWMutex
	peers map[string]*Peer
}

// NewPeerStore opens (or lazily creates) the registry at path.
func NewPeerStore(path string) *PeerStore {
	return &PeerStore{path: path, peers: make(map[string]*Peer)}
}

// StorePath is where the registry is persisted.
func (s *PeerStore) StorePath() string { return s.path }

// Load reads the registry; a missing file is an empty registry, not an error.
func (s *PeerStore) Load() error {
	s.mu.Lock()
	defer s.mu.Unlock()

	s.peers = make(map[string]*Peer)
	if s.path == "" {
		return nil
	}
	data, err := os.ReadFile(s.path)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return err
	}
	var peers []*Peer
	if err := json.Unmarshal(data, &peers); err != nil {
		return fmt.Errorf("parsing peer registry %s: %w", s.path, err)
	}
	for _, p := range peers {
		if p == nil || p.Name == "" || p.PublicKey == "" {
			continue
		}
		if p.Fingerprint == "" {
			_, fp, err := PublicKeyOf(p.PublicKey)
			if err != nil {
				continue
			}
			p.Fingerprint = fp
		}
		s.peers[p.Name] = p
	}
	return nil
}

// save writes the registry atomically.
func (s *PeerStore) save() error {
	if s.path == "" {
		return nil
	}
	peers := make([]*Peer, 0, len(s.peers))
	for _, p := range s.peers {
		peers = append(peers, p)
	}
	sort.Slice(peers, func(i, j int) bool { return peers[i].Name < peers[j].Name })

	data, err := json.MarshalIndent(peers, "", "  ")
	if err != nil {
		return err
	}
	data = append(data, '\n')

	if dir := filepath.Dir(s.path); dir != "" && dir != "." {
		if err := os.MkdirAll(dir, 0755); err != nil {
			return err
		}
	}
	tmp, err := os.CreateTemp(filepath.Dir(s.path), ".peers-*.tmp")
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
	return os.Rename(tmpName, s.path)
}

// Add authorizes a peer (enrollment), rejecting a key that is already known under
// another name so a peer cannot be shadowed.
func (s *PeerStore) Add(p *Peer) error {
	if strings.TrimSpace(p.Name) == "" {
		return fmt.Errorf("peer name is required")
	}
	_, fingerprint, err := PublicKeyOf(p.PublicKey)
	if err != nil {
		return err
	}
	p.Fingerprint = fingerprint
	if p.CreatedAt.IsZero() {
		p.CreatedAt = time.Now().UTC()
	}

	s.mu.Lock()
	defer s.mu.Unlock()

	for _, existing := range s.peers {
		if existing.Fingerprint == fingerprint && existing.Name != p.Name {
			return fmt.Errorf("this key is already authorized as %q", existing.Name)
		}
	}
	s.peers[p.Name] = p
	return s.save()
}

// Remove revokes a peer's access.
func (s *PeerStore) Remove(name string) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	if _, ok := s.peers[name]; !ok {
		return fmt.Errorf("peer %q not found", name)
	}
	delete(s.peers, name)
	return s.save()
}

// List returns the peers sorted by name.
func (s *PeerStore) List() []*Peer {
	s.mu.RLock()
	defer s.mu.RUnlock()

	out := make([]*Peer, 0, len(s.peers))
	for _, p := range s.peers {
		out = append(out, p)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Name < out[j].Name })
	return out
}

// Get returns a peer by name.
func (s *PeerStore) Get(name string) *Peer {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.peers[name]
}

// ByFingerprint returns the peer owning a key id: the lookup that authenticates a
// request.
func (s *PeerStore) ByFingerprint(fingerprint string) *Peer {
	s.mu.RLock()
	defer s.mu.RUnlock()

	for _, p := range s.peers {
		if p.Fingerprint == fingerprint {
			return p
		}
	}
	return nil
}

// Count reports how many keys are authorized.
func (s *PeerStore) Count() int {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return len(s.peers)
}

// TouchSeen records that a peer made a request. Best effort: it is metadata, so a
// write failure must not fail the request.
func (s *PeerStore) TouchSeen(name string, t time.Time) {
	s.mu.Lock()
	defer s.mu.Unlock()

	if p, ok := s.peers[name]; ok {
		p.LastSeenAt = t.UTC()
		_ = s.save()
	}
}
