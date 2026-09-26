package chartsrepo

import (
	"context"
	"os"
	"path/filepath"
	"testing"
	"time"

	git "github.com/go-git/go-git/v5"
	"github.com/go-git/go-git/v5/plumbing"
	"github.com/go-git/go-git/v5/plumbing/object"
)

// newOriginRepo creates a local git repository on the Prod branch holding the
// given files, and returns its path.
func newOriginRepo(t *testing.T, files map[string]string) string {
	t.Helper()
	dir := t.TempDir()
	repo, err := git.PlainInitWithOptions(dir, &git.PlainInitOptions{
		InitOptions: git.InitOptions{DefaultBranch: plumbing.NewBranchReferenceName("Prod")},
	})
	if err != nil {
		t.Fatalf("init origin: %v", err)
	}
	writeFiles(t, dir, files)
	wt, err := repo.Worktree()
	if err != nil {
		t.Fatalf("worktree: %v", err)
	}
	if err := wt.AddWithOptions(&git.AddOptions{All: true}); err != nil {
		t.Fatalf("add: %v", err)
	}
	if _, err := wt.Commit("init", &git.CommitOptions{
		Author: &object.Signature{Name: "test", Email: "test@example.com", When: time.Now()},
	}); err != nil {
		t.Fatalf("commit: %v", err)
	}
	return dir
}

func writeFiles(t *testing.T, root string, files map[string]string) {
	t.Helper()
	for rel, content := range files {
		full := filepath.Join(root, filepath.FromSlash(rel))
		if err := os.MkdirAll(filepath.Dir(full), 0755); err != nil {
			t.Fatalf("mkdir %s: %v", rel, err)
		}
		if err := os.WriteFile(full, []byte(content), 0644); err != nil {
			t.Fatalf("write %s: %v", rel, err)
		}
	}
}

func newTestManager(t *testing.T, storePath string) *Manager {
	t.Helper()
	store := NewStore(storePath)
	if err := store.Load(); err != nil {
		t.Fatalf("load store: %v", err)
	}
	return NewManager(store, t.TempDir(), time.Minute, nil)
}

func TestRefreshClonesAndListsApps(t *testing.T) {
	origin := newOriginRepo(t, map[string]string{
		"apps/jellyfin/Chart.yaml":      "name: jellyfin\nversion: 1.0.0\n",
		"apps/jellyfin/naslos-app.yaml": "name: jellyfin\n",
	})
	m := newTestManager(t, "")
	if _, err := m.AddSource(&Source{Name: "naslos", URL: origin, Official: true}); err != nil {
		t.Fatalf("add source: %v", err)
	}
	if err := m.Refresh(context.Background(), "naslos", "Prod"); err != nil {
		t.Fatalf("refresh: %v", err)
	}
	names, err := m.AppNames("naslos", "Prod")
	if err != nil {
		t.Fatalf("app names: %v", err)
	}
	if len(names) != 1 || names[0] != "jellyfin" {
		t.Fatalf("app names = %v, want [jellyfin]", names)
	}
	dir, err := m.AppDir("naslos", "Prod", "jellyfin")
	if err != nil {
		t.Fatalf("app dir: %v", err)
	}
	if _, err := os.Stat(filepath.Join(dir, "Chart.yaml")); err != nil {
		t.Fatalf("chart not cloned: %v", err)
	}
	src, err := m.GetSource("naslos")
	if err != nil {
		t.Fatalf("get source: %v", err)
	}
	if src.LastError != "" {
		t.Fatalf("unexpected LastError %q", src.LastError)
	}
	if src.LastSync.IsZero() {
		t.Fatal("LastSync was not recorded")
	}
}

func TestRefreshUnknownChannelFails(t *testing.T) {
	origin := newOriginRepo(t, map[string]string{"apps/x/Chart.yaml": "name: x\n"})
	m := newTestManager(t, "")
	if _, err := m.AddSource(&Source{Name: "naslos", URL: origin}); err != nil {
		t.Fatalf("add source: %v", err)
	}
	if err := m.Refresh(context.Background(), "naslos", "Nope"); err == nil {
		t.Fatal("expected an error for an unknown channel")
	}
}

func TestAppDirRejectsTraversalAndSymlinkEscape(t *testing.T) {
	m := newTestManager(t, "")
	m.cacheDir = t.TempDir()
	root := filepath.Join(m.cacheDir, "naslos", "prod")
	if err := os.MkdirAll(filepath.Join(root, "apps", "jellyfin"), 0755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	if _, err := m.AppDir("naslos", "Prod", ".."); err == nil {
		t.Fatal("expected traversal name to be rejected")
	}

	outside := t.TempDir()
	if err := os.Symlink(outside, filepath.Join(root, "apps", "evil")); err != nil {
		t.Fatalf("symlink: %v", err)
	}
	if _, err := m.AppDir("naslos", "Prod", "evil"); err == nil {
		t.Fatal("expected symlink escape to be rejected")
	}
}

func TestSafeJoinRejectsAbsoluteAndParent(t *testing.T) {
	root := t.TempDir()
	if _, err := safeJoin(root, "/etc/passwd"); err == nil {
		t.Fatal("absolute path should be rejected")
	}
	if _, err := safeJoin(root, "../secret"); err == nil {
		t.Fatal("parent path should be rejected")
	}
}

func TestEnsureFreshStaleFallback(t *testing.T) {
	origin := newOriginRepo(t, map[string]string{"apps/jellyfin/Chart.yaml": "name: jellyfin\n"})
	m := newTestManager(t, "")
	if _, err := m.AddSource(&Source{Name: "naslos", URL: origin}); err != nil {
		t.Fatalf("add source: %v", err)
	}
	if err := m.Refresh(context.Background(), "naslos", "Prod"); err != nil {
		t.Fatalf("refresh: %v", err)
	}

	// Remove the origin so any refresh fails, then pretend the TTL expired.
	if err := os.RemoveAll(origin); err != nil {
		t.Fatalf("remove origin: %v", err)
	}
	m.now = func() time.Time { return time.Now().Add(2 * time.Hour) }

	src, err := m.EnsureFresh(context.Background(), "naslos", "Prod")
	if err != nil {
		t.Fatalf("EnsureFresh should fall back to stale cache: %v", err)
	}
	if src == nil {
		t.Fatal("nil source")
	}
	if src.LastError == "" {
		t.Fatal("expected LastError to record the failed refresh")
	}
}

func TestEnsureFreshWithoutCacheFails(t *testing.T) {
	m := newTestManager(t, "")
	if _, err := m.AddSource(&Source{Name: "naslos", URL: filepath.Join(t.TempDir(), "missing")}); err != nil {
		t.Fatalf("add source: %v", err)
	}
	if _, err := m.EnsureFresh(context.Background(), "naslos", "Prod"); err == nil {
		t.Fatal("expected an error when there is no cache and the remote is unreachable")
	}
}

func TestSourceStorePersistence(t *testing.T) {
	path := filepath.Join(t.TempDir(), "sources.json")
	store := NewStore(path)
	if err := store.Load(); err != nil {
		t.Fatalf("load: %v", err)
	}
	if err := store.Upsert(&Source{Name: "mine", URL: "https://example.com/repo.git", Auth: AuthToken, CredentialsSecret: "creds"}); err != nil {
		t.Fatalf("upsert: %v", err)
	}

	reloaded := NewStore(path)
	if err := reloaded.Load(); err != nil {
		t.Fatalf("reload: %v", err)
	}
	src, err := reloaded.Get("mine")
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	if src.Auth != AuthToken || src.CredentialsSecret != "creds" {
		t.Fatalf("source round-trip mismatch: %+v", src)
	}
	if len(src.Channels) != 3 {
		t.Fatalf("default channels not applied: %v", src.Channels)
	}
}

func TestChannelsForDeclaredIsAuthoritative(t *testing.T) {
	src := &Source{Name: "naslos", URL: "u", Channels: map[string]string{"Prod": "main"}}
	channels := src.ChannelsFor()
	if len(channels) != 1 || channels["Prod"] != "main" {
		t.Fatalf("ChannelsFor() = %v, want only {Prod: main}", channels)
	}
	// No declared channels -> the standard defaults.
	def := (&Source{Name: "naslos", URL: "u"}).ChannelsFor()
	if len(def) != 3 {
		t.Fatalf("default ChannelsFor() = %v, want 3 channels", def)
	}
}

func TestSourceValidate(t *testing.T) {
	cases := []struct {
		name    string
		src     Source
		wantErr bool
	}{
		{"ok", Source{Name: "naslos", URL: "https://example.com/r.git"}, false},
		{"bad name", Source{Name: "Bad Name", URL: "https://example.com/r.git"}, true},
		{"missing url", Source{Name: "naslos"}, true},
		{"token needs secret", Source{Name: "naslos", URL: "u", Auth: AuthToken}, true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := tc.src.Validate()
			if (err != nil) != tc.wantErr {
				t.Fatalf("Validate() error = %v, wantErr %v", err, tc.wantErr)
			}
		})
	}
}
