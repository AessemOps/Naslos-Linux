package chartsrepo

import (
	"context"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	git "github.com/go-git/go-git/v5"
	"github.com/go-git/go-git/v5/plumbing"
	"github.com/go-git/go-git/v5/plumbing/transport"
	githttp "github.com/go-git/go-git/v5/plumbing/transport/http"
	gitssh "github.com/go-git/go-git/v5/plumbing/transport/ssh"
)

// DefaultTTL is how long a cached clone is considered fresh.
const DefaultTTL = 15 * time.Minute

// Guards against a pathological repository exhausting the cache volume or the
// API's memory when it is walked.
const (
	maxRepoBytes int64 = 256 << 20 // 256 MiB
	maxRepoFiles       = 20000
)

// Manager owns the source store and the local clone cache.
type Manager struct {
	store    *Store
	cacheDir string
	ttl      time.Duration
	creds    CredentialsProvider
	now      func() time.Time

	// refreshMu serialises git operations: go-git is not safe to run two
	// clones of the same working tree at once, and the API may refresh from a
	// request while the startup reconcile runs.
	refreshMu sync.Mutex
}

// NewManager creates a charts-repository manager.
func NewManager(store *Store, cacheDir string, ttl time.Duration, creds CredentialsProvider) *Manager {
	if ttl <= 0 {
		ttl = DefaultTTL
	}
	return &Manager{
		store:    store,
		cacheDir: cacheDir,
		ttl:      ttl,
		creds:    creds,
		now:      time.Now,
	}
}

// Sources returns the configured sources.
func (m *Manager) Sources() []*Source { return m.store.List() }

// GetSource returns one source by name.
func (m *Manager) GetSource(name string) (*Source, error) { return m.store.Get(name) }

// AddSource validates and persists a new source.
func (m *Manager) AddSource(src *Source) (*Source, error) {
	if err := src.normalize(); err != nil {
		return nil, err
	}
	if err := m.store.Upsert(src); err != nil {
		return nil, err
	}
	return m.store.Get(src.Name)
}

// DeleteSource removes a source and its cached clones.
func (m *Manager) DeleteSource(name string) error {
	if err := m.store.Delete(name); err != nil {
		return err
	}
	_ = os.RemoveAll(filepath.Join(m.cacheDir, name))
	return nil
}

// Dir returns the working tree for a source/channel, validating both names.
func (m *Manager) Dir(sourceName, channel string) (string, error) {
	if err := validateName(sourceName); err != nil {
		return "", err
	}
	channel = strings.TrimSpace(channel)
	if channel == "" {
		channel = DefaultChannel
	}
	if err := validateName(strings.ToLower(channel)); err != nil {
		return "", fmt.Errorf("invalid channel %q", channel)
	}
	return filepath.Join(m.cacheDir, sourceName, strings.ToLower(channel)), nil
}

// branchFor resolves a channel to its branch, rejecting an unknown channel so a
// typo cannot silently clone the default branch.
func branchFor(src *Source, channel string) (string, error) {
	channels := src.ChannelsFor()
	branch, ok := channels[channel]
	if !ok {
		return "", fmt.Errorf("source %q does not offer channel %q", src.Name, channel)
	}
	return branch, nil
}

// Refresh clones or pulls one channel of a source into the cache.
func (m *Manager) Refresh(ctx context.Context, name, channel string) error {
	src, err := m.store.Get(name)
	if err != nil {
		return err
	}
	return m.refreshSource(ctx, src, channel)
}

// RefreshAll refreshes every configured source and channel, returning a joined
// error so one unreachable remote does not hide the others.
func (m *Manager) RefreshAll(ctx context.Context) error {
	var errs []error
	for _, src := range m.store.List() {
		for _, channel := range src.ChannelNames() {
			if err := m.refreshSource(ctx, src, channel); err != nil {
				errs = append(errs, err)
			}
		}
	}
	return errors.Join(errs...)
}

func (m *Manager) refreshSource(ctx context.Context, src *Source, channel string) error {
	branch, err := branchFor(src, channel)
	if err != nil {
		return err
	}
	dir, err := m.Dir(src.Name, channel)
	if err != nil {
		return err
	}

	m.refreshMu.Lock()
	defer m.refreshMu.Unlock()

	if err := m.syncTree(ctx, src, branch, dir); err != nil {
		m.recordSync(src.Name, err)
		return fmt.Errorf("refreshing %s/%s: %w", src.Name, channel, err)
	}
	if err := statRepo(dir); err != nil {
		_ = os.RemoveAll(dir)
		m.recordSync(src.Name, err)
		return fmt.Errorf("refreshing %s/%s: %w", src.Name, channel, err)
	}
	m.recordSync(src.Name, nil)
	return nil
}

// syncTree clones into dir when absent and pulls otherwise.
func (m *Manager) syncTree(ctx context.Context, src *Source, branch, dir string) error {
	auth, err := m.authFor(ctx, src)
	if err != nil {
		return err
	}

	if _, err := os.Stat(filepath.Join(dir, ".git")); errors.Is(err, fs.ErrNotExist) {
		if err := os.MkdirAll(filepath.Dir(dir), 0750); err != nil {
			return fmt.Errorf("creating cache directory: %w", err)
		}
		_, err := git.PlainCloneContext(ctx, dir, false, &git.CloneOptions{
			URL:           src.URL,
			Auth:          auth,
			ReferenceName: plumbing.NewBranchReferenceName(branch),
			SingleBranch:  true,
			Depth:         1,
			Tags:          git.NoTags,
		})
		if err != nil {
			return fmt.Errorf("cloning %s: %w", src.URL, err)
		}
		return nil
	}

	repo, err := git.PlainOpen(dir)
	if err != nil {
		return fmt.Errorf("opening cached clone: %w", err)
	}
	wt, err := repo.Worktree()
	if err != nil {
		return fmt.Errorf("opening worktree: %w", err)
	}
	if err := wt.Checkout(&git.CheckoutOptions{
		Branch: plumbing.NewBranchReferenceName(branch),
		Force:  true,
	}); err != nil {
		return fmt.Errorf("checking out %s: %w", branch, err)
	}
	err = wt.PullContext(ctx, &git.PullOptions{
		RemoteName:    "origin",
		ReferenceName: plumbing.NewBranchReferenceName(branch),
		SingleBranch:  true,
		Depth:         1,
		Force:         true,
		Auth:          auth,
	})
	if errors.Is(err, git.NoErrAlreadyUpToDate) {
		return nil
	}
	if err != nil {
		return fmt.Errorf("pulling %s: %w", branch, err)
	}
	return nil
}

// authFor resolves the transport credentials for a source.
func (m *Manager) authFor(ctx context.Context, src *Source) (transport.AuthMethod, error) {
	if src.Auth == "" || src.Auth == AuthPublic {
		return nil, nil
	}
	if m.creds == nil {
		return nil, fmt.Errorf("source %q uses %q auth but no credential provider is configured", src.Name, src.Auth)
	}
	creds, err := m.creds.Resolve(ctx, src)
	if err != nil {
		return nil, err
	}
	switch src.Auth {
	case AuthToken:
		if strings.TrimSpace(creds.Token) == "" {
			return nil, fmt.Errorf("source %q: empty token", src.Name)
		}
		return &githttp.BasicAuth{Username: "x-access-token", Password: creds.Token}, nil
	case AuthSSH:
		if len(creds.SSHKey) == 0 {
			return nil, fmt.Errorf("source %q: empty SSH key", src.Name)
		}
		key, err := gitssh.NewPublicKeys("git", creds.SSHKey, "")
		if err != nil {
			return nil, fmt.Errorf("source %q: parsing SSH key: %w", src.Name, err)
		}
		// #nosec G106 -- the deploy key is configured by the appliance admin;
		// host-key pinning needs a known_hosts Secret and is a documented
		// follow-up (see docs/app-catalog.md).
		key.HostKeyCallback = insecureHostKeyCallback
		return key, nil
	default:
		return nil, fmt.Errorf("source %q: unsupported auth %q", src.Name, src.Auth)
	}
}

// recordSync stores the outcome of a refresh for status reporting.
func (m *Manager) recordSync(name string, syncErr error) {
	msg := ""
	if syncErr != nil {
		msg = syncErr.Error()
	}
	_, _ = m.store.Update(name, func(src *Source) {
		src.LastError = msg
		if syncErr == nil {
			src.LastSync = time.Now().UTC()
		}
	})
}

// EnsureFresh returns a source whose cache is fresh, refreshing when the TTL has
// expired. When the remote is unreachable it falls back to the stale clone
// rather than failing, so the catalog survives an outage.
func (m *Manager) EnsureFresh(ctx context.Context, name, channel string) (*Source, error) {
	src, err := m.store.Get(name)
	if err != nil {
		return nil, err
	}
	if _, err := m.Dir(name, channel); err != nil {
		return nil, err
	}
	if _, err := branchFor(src, channel); err != nil {
		return nil, err
	}

	cached := m.hasClone(name, channel)
	fresh := cached && !src.LastSync.IsZero() && m.now().Sub(src.LastSync) < m.ttl
	if fresh {
		return src, nil
	}

	if err := m.refreshSource(ctx, src, channel); err != nil {
		if cached {
			return m.store.Get(name)
		}
		return nil, err
	}
	return m.store.Get(name)
}

func (m *Manager) hasClone(name, channel string) bool {
	dir, err := m.Dir(name, channel)
	if err != nil {
		return false
	}
	info, err := os.Stat(filepath.Join(dir, ".git"))
	return err == nil && info.IsDir()
}

// AppNames lists the chart directories under apps/ for a cached source/channel.
func (m *Manager) AppNames(name, channel string) ([]string, error) {
	dir, err := m.Dir(name, channel)
	if err != nil {
		return nil, err
	}
	appsRoot := filepath.Join(dir, appsDir)
	entries, err := os.ReadDir(appsRoot)
	if err != nil {
		if errors.Is(err, fs.ErrNotExist) {
			return nil, nil
		}
		return nil, fmt.Errorf("listing apps: %w", err)
	}
	names := make([]string, 0, len(entries))
	for _, entry := range entries {
		if !entry.IsDir() {
			continue
		}
		if err := validateName(entry.Name()); err != nil {
			continue
		}
		names = append(names, entry.Name())
	}
	return names, nil
}

// AppDir resolves the directory of one app, refusing any path that escapes the
// clone (traversal, absolute paths, or a symlink pointing outside).
func (m *Manager) AppDir(name, channel, app string) (string, error) {
	if err := validateName(app); err != nil {
		return "", fmt.Errorf("invalid app name %q", app)
	}
	dir, err := m.Dir(name, channel)
	if err != nil {
		return "", err
	}
	return safeJoin(dir, filepath.Join(appsDir, app))
}

// safeJoin joins rel onto root and verifies the resolved path stays inside root.
func safeJoin(root, rel string) (string, error) {
	if rel == "" || filepath.IsAbs(rel) {
		return "", fmt.Errorf("invalid relative path %q", rel)
	}
	clean := filepath.Clean(rel)
	if clean == ".." || strings.HasPrefix(clean, ".."+string(filepath.Separator)) {
		return "", fmt.Errorf("path %q escapes the repository", rel)
	}
	full := filepath.Join(root, clean)

	rootResolved, err := filepath.EvalSymlinks(root)
	if err != nil {
		return "", fmt.Errorf("resolving repository root: %w", err)
	}
	resolved, err := filepath.EvalSymlinks(full)
	if err != nil {
		return "", fmt.Errorf("resolving %q: %w", rel, err)
	}
	if resolved != rootResolved && !strings.HasPrefix(resolved, rootResolved+string(filepath.Separator)) {
		return "", fmt.Errorf("path %q resolves outside the repository", rel)
	}
	return resolved, nil
}

// statRepo enforces the repository size and file-count guard, skipping .git.
func statRepo(dir string) error {
	var files int
	var bytes int64
	err := filepath.WalkDir(dir, func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if d.IsDir() && d.Name() == ".git" {
			return fs.SkipDir
		}
		if d.IsDir() {
			return nil
		}
		info, err := d.Info()
		if err != nil {
			return err
		}
		files++
		bytes += info.Size()
		if files > maxRepoFiles {
			return fmt.Errorf("repository has more than %d files", maxRepoFiles)
		}
		if bytes > maxRepoBytes {
			return fmt.Errorf("repository is larger than %d MiB", maxRepoBytes>>20)
		}
		return nil
	})
	return err
}
