package apps

import (
	"context"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/AessemOps/Naslos-Linux/api/internal/catalog"
	"github.com/AessemOps/Naslos-Linux/api/internal/chartsrepo"
	"github.com/AessemOps/Naslos-Linux/api/internal/helm"
)

func TestStoreRoundTrip(t *testing.T) {
	path := filepath.Join(t.TempDir(), "apps.json")
	store := NewStore(path)
	if err := store.Load(); err != nil {
		t.Fatalf("load: %v", err)
	}
	rec := Record{
		Name: "jellyfin", Source: "naslos", Channel: "Prod", ChartPath: "apps/jellyfin",
		Values:   map[string]interface{}{"timezone": "UTC", "nested": map[string]interface{}{"a": "b"}},
		Exposure: Exposure{Subdomain: "jellyfin", TLS: true, Auth: true, Port: 8096},
	}
	if err := store.Upsert(rec); err != nil {
		t.Fatalf("upsert: %v", err)
	}

	reloaded := NewStore(path)
	if err := reloaded.Load(); err != nil {
		t.Fatalf("reload: %v", err)
	}
	got, err := reloaded.Get("jellyfin")
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	if got.Exposure.Subdomain != "jellyfin" || !got.Exposure.TLS {
		t.Fatalf("exposure mismatch: %+v", got.Exposure)
	}
	nested, ok := got.Values["nested"].(map[string]interface{})
	if !ok || nested["a"] != "b" {
		t.Fatalf("values mismatch: %+v", got.Values)
	}
}

func TestMergeValuesDeep(t *testing.T) {
	base := map[string]interface{}{
		"timezone": "UTC",
		"nested":   map[string]interface{}{"a": 1, "b": 2},
	}
	overlay := map[string]interface{}{
		"timezone": "Europe/Paris",
		"nested":   map[string]interface{}{"b": 3},
	}
	out := mergeValues(base, overlay)
	if out["timezone"] != "Europe/Paris" {
		t.Fatalf("top-level override failed: %v", out["timezone"])
	}
	nested := out["nested"].(map[string]interface{})
	if nested["a"] != 1 || nested["b"] != 3 {
		t.Fatalf("deep merge failed: %+v", nested)
	}
	// The base must not be mutated.
	if base["nested"].(map[string]interface{})["b"] != 2 {
		t.Fatal("mergeValues mutated the base map")
	}
}

func TestExposureForUsesCatalogDefaultsAndOverrides(t *testing.T) {
	app := &catalog.App{
		Exposure: catalog.ExposureDefaults{Subdomain: "jellyfin", TLS: true, Auth: true},
		Services: []catalog.Service{{Name: "{{ .Release.Name }}", Port: 8096, Scheme: "http"}},
	}
	got := exposureFor(app, nil, "jellyfin")
	if got.Service != "jellyfin" || got.Port != 8096 || !got.TLS || !got.Auth {
		t.Fatalf("defaults mismatch: %+v", got)
	}

	override := &Exposure{Subdomain: "media", TLS: false, Auth: false, LocalOnly: true}
	got = exposureFor(app, override, "jellyfin")
	if got.Subdomain != "media" || got.TLS || got.Auth || !got.LocalOnly {
		t.Fatalf("overrides mismatch: %+v", got)
	}
	if got.Port != 8096 {
		t.Fatalf("port should default from the declared service: %+v", got)
	}
}

func TestBackfillSkipsPlatformAndExisting(t *testing.T) {
	path := filepath.Join(t.TempDir(), "apps.json")
	store := NewStore(path)
	if err := store.Load(); err != nil {
		t.Fatalf("load: %v", err)
	}
	if err := store.Upsert(Record{Name: "existing"}); err != nil {
		t.Fatalf("upsert: %v", err)
	}
	m := &Manager{store: store, helm: helm.NewClient("naslos")}

	releases := []helm.App{
		{Name: "naslos"},   // platform release: skipped
		{Name: "existing"}, // already recorded: skipped
		{Name: "legacy-app", Version: "1.2.3"},
	}
	if err := m.Backfill(context.Background(), "naslos", releases); err != nil {
		t.Fatalf("backfill: %v", err)
	}
	rec, err := store.Get("legacy-app")
	if err != nil {
		t.Fatalf("legacy app not backfilled: %v", err)
	}
	if !rec.Orphaned || rec.ChartVersion != "1.2.3" {
		t.Fatalf("backfilled record mismatch: %+v", rec)
	}
	if _, err := store.Get("naslos"); err == nil {
		t.Fatal("platform release must not be backfilled")
	}
}

func TestURLForUsesRecordBaseDomain(t *testing.T) {
	m := &Manager{baseDomain: "naslos.local"}
	rec := Record{Exposure: Exposure{Subdomain: "jellyfin", TLS: true}, BaseDomain: "media.example.com"}
	if got := m.urlFor(rec); got != "https://jellyfin.media.example.com" {
		t.Fatalf("urlFor with a record domain = %q", got)
	}
	rec.BaseDomain = ""
	if got := m.urlFor(rec); got != "https://jellyfin.naslos.local" {
		t.Fatalf("urlFor fallback to primary = %q", got)
	}
	plain := &Manager{baseDomain: "naslos.local"}
	if got := plain.urlFor(Record{Exposure: Exposure{Subdomain: "x"}}); got != "http://x.naslos.local" {
		t.Fatalf("urlFor non-TLS = %q", got)
	}
}

// TestAuthAllowedFollowsTheSSOProvider proves a live SSO promotion (the provider
// list changing) is reflected without recreating the manager.
func TestAuthAllowedFollowsTheSSOProvider(t *testing.T) {
	domains := []string{"naslos.local"}
	m := &Manager{baseDomain: "naslos.local", ssoDomains: func() []string { return domains }}
	if m.AuthAllowed("media.example.com") {
		t.Fatal("auth allowed on an unpromoted domain")
	}
	domains = append(domains, "media.example.com")
	if !m.AuthAllowed("media.example.com") {
		t.Fatal("auth not allowed after promotion")
	}
	// An empty domain falls back to the primary, which is always SSO.
	if !m.AuthAllowed("") {
		t.Fatal("auth not allowed on the primary fallback")
	}
}

func TestValidateReleaseName(t *testing.T) {
	if err := validateReleaseName("jellyfin"); err != nil {
		t.Fatalf("valid name rejected: %v", err)
	}
	if err := validateReleaseName("Bad Name"); err == nil {
		t.Fatal("invalid name accepted")
	}
}

// TestInstallWithProgressStagesInOrder pins FR-APP-18's progress contract: the
// install reports `preparing` before `installing`, and a Helm failure stops
// there (never `finalizing`). It runs against a fixture clone so resolveChart
// succeeds; the chart directory has no Chart.yaml, so the Helm load fails
// deterministically without a cluster.
func TestInstallWithProgressStagesInOrder(t *testing.T) {
	cacheDir := t.TempDir()
	cloneDir := filepath.Join(cacheDir, "test", "prod")
	appDir := filepath.Join(cloneDir, "apps", "demo")
	if err := os.MkdirAll(filepath.Join(cloneDir, ".git"), 0o755); err != nil {
		t.Fatalf("creating clone: %v", err)
	}
	if err := os.MkdirAll(appDir, 0o755); err != nil {
		t.Fatalf("creating app dir: %v", err)
	}
	writeFile(t, filepath.Join(appDir, "naslos-app.yaml"), "name: demo\nversion: 0.1.0\n")

	sources := chartsrepo.NewStore("")
	if err := sources.Upsert(&chartsrepo.Source{
		Name:     "test",
		URL:      "https://example.invalid/repo.git",
		Auth:     chartsrepo.AuthPublic,
		Channels: map[string]string{"Prod": "main"},
		LastSync: time.Now().UTC(),
	}); err != nil {
		t.Fatalf("seeding source: %v", err)
	}
	charts := chartsrepo.NewManager(sources, cacheDir, time.Hour, nil)
	cat := catalog.Load([]catalog.SourceRef{{Name: "test", Channel: "Prod", Dir: cloneDir}})
	store := NewStore(filepath.Join(t.TempDir(), "apps.json"))
	if err := store.Load(); err != nil {
		t.Fatalf("load: %v", err)
	}
	m := &Manager{
		store:      store,
		helm:       helm.NewClient("naslos-apps"),
		charts:     charts,
		catalog:    func() *catalog.Catalog { return cat },
		baseDomain: "naslos.local",
	}

	var stages []string
	_, err := m.InstallWithProgress(context.Background(), InstallRequest{Name: "demo"}, func(stage, _ string) {
		stages = append(stages, stage)
	})
	if err == nil {
		t.Fatal("expected the Helm install to fail without a cluster")
	}
	want := []string{StagePreparing, StageInstalling}
	if len(stages) != len(want) || stages[0] != want[0] || stages[1] != want[1] {
		t.Fatalf("stages = %v, want %v", stages, want)
	}
}

// TestUninstallWithProgressStagesInOrder covers the uninstall callback: an
// orphaned record skips Helm, so it reports `preparing` then `finalizing` and
// succeeds without a cluster.
func TestUninstallWithProgressStagesInOrder(t *testing.T) {
	path := filepath.Join(t.TempDir(), "apps.json")
	store := NewStore(path)
	if err := store.Load(); err != nil {
		t.Fatalf("load: %v", err)
	}
	if err := store.Upsert(Record{Name: "legacy", Orphaned: true, Values: map[string]interface{}{}}); err != nil {
		t.Fatalf("upsert: %v", err)
	}
	m := &Manager{store: store, helm: helm.NewClient("naslos-apps")}

	var stages []string
	if err := m.UninstallWithProgress(context.Background(), "legacy", func(stage, _ string) {
		stages = append(stages, stage)
	}); err != nil {
		t.Fatalf("uninstall: %v", err)
	}
	want := []string{StagePreparing, StageFinalizing}
	if len(stages) != len(want) || stages[0] != want[0] || stages[1] != want[1] {
		t.Fatalf("stages = %v, want %v", stages, want)
	}
	if _, err := store.Get("legacy"); err == nil {
		t.Fatal("record was not removed")
	}
}

func writeFile(t *testing.T, path, content string) {
	t.Helper()
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatalf("writing %s: %v", path, err)
	}
}
