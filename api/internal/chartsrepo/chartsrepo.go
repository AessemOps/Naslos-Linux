// Package chartsrepo manages git-based Helm chart repositories: source
// definitions (official + user-added), credentials, a local clone cache and
// per-channel refresh. It is the ingestion layer for the app catalog.
//
// A source is a git repository laid out as:
//
//	<repo-root>/naslos-repo.yaml     # optional channel -> branch mapping
//	<repo-root>/apps/<name>/Chart.yaml
//	<repo-root>/apps/<name>/naslos-app.yaml
//
// The cache holds one working tree per (source, channel) so a channel maps to a
// git branch without re-cloning for every app read.
package chartsrepo

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

// DefaultChannel is the channel offered when a source declares no mapping.
const DefaultChannel = "Prod"

// appsDir is the directory holding one chart per app.
const appsDir = "apps"

// AuthType is how a source authenticates to its git remote.
type AuthType string

const (
	// AuthPublic clones over HTTPS with no credentials.
	AuthPublic AuthType = "public"
	// AuthToken clones over HTTPS with a token (GitHub PAT).
	AuthToken AuthType = "token"
	// AuthSSH clones over SSH with a deploy key.
	AuthSSH AuthType = "ssh"
)

// defaultChannels is the standard channel -> branch mapping.
func defaultChannels() map[string]string {
	return map[string]string{
		"Prod":         "Prod",
		"Develop":      "Develop",
		"Experimental": "Experimental",
	}
}

// Source is a configured chart repository.
type Source struct {
	Name        string   `json:"name"`
	DisplayName string   `json:"displayName,omitempty"`
	URL         string   `json:"url"`
	Auth        AuthType `json:"auth"`
	// CredentialsSecret names a Kubernetes Secret (in CredentialsNamespace,
	// defaulting to the API's namespace) holding the token or SSH key.
	CredentialsSecret    string `json:"credentialsSecret,omitempty"`
	CredentialsNamespace string `json:"credentialsNamespace,omitempty"`
	// TokenKey / SSHKeyKey name the Secret keys; sensible defaults apply.
	TokenKey  string `json:"tokenKey,omitempty"`
	SSHKeyKey string `json:"sshKeyKey,omitempty"`
	// Channels maps a channel name to a git branch.
	Channels map[string]string `json:"channels"`
	// Official marks the curated repository; user sources override it on name
	// collision.
	Official bool `json:"official,omitempty"`

	AddedAt   time.Time `json:"addedAt,omitempty"`
	LastSync  time.Time `json:"lastSync,omitempty"`
	LastError string    `json:"lastError,omitempty"`
}

// ChannelsFor returns the channel -> branch mapping. A source that declares
// channels uses exactly those (so a repository with only `main` can offer just
// `Prod`); otherwise the standard defaults apply.
func (s *Source) ChannelsFor() map[string]string {
	if len(s.Channels) > 0 {
		channels := make(map[string]string, len(s.Channels))
		for k, v := range s.Channels {
			if strings.TrimSpace(k) == "" || strings.TrimSpace(v) == "" {
				continue
			}
			channels[k] = v
		}
		if len(channels) > 0 {
			return channels
		}
	}
	return defaultChannels()
}

// ChannelNames returns the sorted channel names a source offers.
func (s *Source) ChannelNames() []string {
	channels := s.ChannelsFor()
	names := make([]string, 0, len(channels))
	for name := range channels {
		names = append(names, name)
	}
	sort.Strings(names)
	return names
}

// TokenKeyOr returns the Secret key holding an HTTPS token.
func (s *Source) TokenKeyOr() string {
	if s.TokenKey != "" {
		return s.TokenKey
	}
	return "token"
}

// SSHKeyKeyOr returns the Secret key holding an SSH private key.
func (s *Source) SSHKeyKeyOr() string {
	if s.SSHKeyKey != "" {
		return s.SSHKeyKey
	}
	return "ssh-private-key"
}

// Validate checks a source is well-formed and safe to use as a directory name.
func (s *Source) Validate() error {
	if err := validateName(s.Name); err != nil {
		return err
	}
	if strings.TrimSpace(s.URL) == "" {
		return fmt.Errorf("source %q: url is required", s.Name)
	}
	switch s.Auth {
	case "", AuthPublic:
	case AuthToken, AuthSSH:
		if s.CredentialsSecret == "" {
			return fmt.Errorf("source %q: auth %q requires credentialsSecret", s.Name, s.Auth)
		}
	default:
		return fmt.Errorf("source %q: unsupported auth %q", s.Name, s.Auth)
	}
	return nil
}

// normalize fills defaults and validates.
func (s *Source) normalize() error {
	if s.Auth == "" {
		s.Auth = AuthPublic
	}
	if len(s.Channels) == 0 {
		s.Channels = defaultChannels()
	}
	if s.AddedAt.IsZero() {
		s.AddedAt = time.Now().UTC()
	}
	return s.Validate()
}

// dns1123Label matches a lowercase RFC 1123 label (DNS-1123 label).
var dns1123Label = regexp.MustCompile(`^[a-z0-9]([-a-z0-9]*[a-z0-9])?$`)

// validateName enforces a DNS-1123 label so the name is safe as a directory.
func validateName(name string) error {
	if name == "" {
		return fmt.Errorf("source name is required")
	}
	if len(name) > 63 {
		return fmt.Errorf("source name %q must be 63 characters or fewer", name)
	}
	if !dns1123Label.MatchString(name) {
		return fmt.Errorf("source name %q must be a lowercase DNS-1123 label", name)
	}
	return nil
}

// Store persists the configured sources as JSON beside the other state files.
type Store struct {
	path    string
	mu      sync.Mutex
	sources map[string]*Source
}

// NewStore creates a source store backed by path (empty means memory only).
func NewStore(path string) *Store {
	return &Store{path: path, sources: make(map[string]*Source)}
}

// Load reads the persisted sources. A missing file is not an error.
func (s *Store) Load() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.loadLocked()
}

func (s *Store) loadLocked() error {
	if s.path == "" {
		return nil
	}
	data, err := os.ReadFile(s.path)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return fmt.Errorf("reading sources file: %w", err)
	}
	var list []*Source
	if err := json.Unmarshal(data, &list); err != nil {
		return fmt.Errorf("parsing sources file: %w", err)
	}
	for _, src := range list {
		if src == nil || src.Name == "" {
			continue
		}
		if src.Auth == "" {
			src.Auth = AuthPublic
		}
		if len(src.Channels) == 0 {
			src.Channels = defaultChannels()
		}
		s.sources[src.Name] = src
	}
	return nil
}

// saveLocked persists sources atomically.
func (s *Store) saveLocked() error {
	if s.path == "" {
		return nil
	}
	list := make([]*Source, 0, len(s.sources))
	for _, src := range s.sources {
		list = append(list, src)
	}
	sort.Slice(list, func(i, j int) bool { return list[i].Name < list[j].Name })

	data, err := json.MarshalIndent(list, "", "  ")
	if err != nil {
		return fmt.Errorf("encoding sources: %w", err)
	}
	if dir := filepath.Dir(s.path); dir != "" && dir != "." {
		if err := os.MkdirAll(dir, 0750); err != nil {
			return fmt.Errorf("creating sources directory: %w", err)
		}
	}
	tmp, err := os.CreateTemp(filepath.Dir(s.path), ".sources-*.tmp")
	if err != nil {
		return fmt.Errorf("creating temp sources file: %w", err)
	}
	tmpName := tmp.Name()
	defer os.Remove(tmpName)
	if _, err := tmp.Write(append(data, '\n')); err != nil {
		tmp.Close()
		return fmt.Errorf("writing sources file: %w", err)
	}
	if err := tmp.Sync(); err != nil {
		tmp.Close()
		return fmt.Errorf("syncing sources file: %w", err)
	}
	if err := tmp.Close(); err != nil {
		return fmt.Errorf("closing sources file: %w", err)
	}
	if err := os.Chmod(tmpName, 0600); err != nil {
		return fmt.Errorf("setting sources mode: %w", err)
	}
	return os.Rename(tmpName, s.path)
}

// List returns all sources ordered by name.
func (s *Store) List() []*Source {
	s.mu.Lock()
	defer s.mu.Unlock()
	list := make([]*Source, 0, len(s.sources))
	for _, src := range s.sources {
		list = append(list, cloneSource(src))
	}
	sort.Slice(list, func(i, j int) bool { return list[i].Name < list[j].Name })
	return list
}

// Get returns a source by name.
func (s *Store) Get(name string) (*Source, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	src, ok := s.sources[name]
	if !ok {
		return nil, fmt.Errorf("source %q not found", name)
	}
	return cloneSource(src), nil
}

// Upsert adds or replaces a source.
func (s *Store) Upsert(src *Source) error {
	if err := src.normalize(); err != nil {
		return err
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	s.sources[src.Name] = cloneSource(src)
	return s.saveLocked()
}

// Update persists a mutation applied by the caller under lock semantics.
func (s *Store) Update(name string, mutate func(*Source)) (*Source, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	src, ok := s.sources[name]
	if !ok {
		return nil, fmt.Errorf("source %q not found", name)
	}
	mutate(src)
	if err := s.saveLocked(); err != nil {
		return nil, err
	}
	return cloneSource(src), nil
}

// Delete removes a source.
func (s *Store) Delete(name string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if _, ok := s.sources[name]; !ok {
		return fmt.Errorf("source %q not found", name)
	}
	delete(s.sources, name)
	return s.saveLocked()
}

func cloneSource(src *Source) *Source {
	dup := *src
	if src.Channels != nil {
		dup.Channels = make(map[string]string, len(src.Channels))
		for k, v := range src.Channels {
			dup.Channels[k] = v
		}
	}
	return &dup
}
