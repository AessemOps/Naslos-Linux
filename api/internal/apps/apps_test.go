package apps

import (
	"context"
	"path/filepath"
	"testing"

	"github.com/AessemOps/Naslos-Linux/api/internal/catalog"
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
	got := exposureFor(app, nil)
	if got.Service != "{{ .Release.Name }}" || got.Port != 8096 || !got.TLS || !got.Auth {
		t.Fatalf("defaults mismatch: %+v", got)
	}

	override := &Exposure{Subdomain: "media", TLS: false, Auth: false, LocalOnly: true}
	got = exposureFor(app, override)
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

func TestValidateReleaseName(t *testing.T) {
	if err := validateReleaseName("jellyfin"); err != nil {
		t.Fatalf("valid name rejected: %v", err)
	}
	if err := validateReleaseName("Bad Name"); err == nil {
		t.Fatal("invalid name accepted")
	}
}
