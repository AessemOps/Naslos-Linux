package catalog

import (
	"os"
	"path/filepath"
	"testing"
)

// writeApp writes a chart + manifest pair under dir/apps/<name>.
func writeApp(t *testing.T, dir, name, chartVersion, manifest string) {
	t.Helper()
	base := filepath.Join(dir, "apps", name)
	if err := os.MkdirAll(base, 0755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	if err := os.WriteFile(filepath.Join(base, "Chart.yaml"), []byte("name: "+name+"\nversion: "+chartVersion+"\n"), 0644); err != nil {
		t.Fatalf("write chart: %v", err)
	}
	if err := os.WriteFile(filepath.Join(base, manifestFile), []byte(manifest), 0644); err != nil {
		t.Fatalf("write manifest: %v", err)
	}
}

const jellyfinManifest = `
name: jellyfin
displayName: Jellyfin
description: Free media system
category: media
icon: "tv"
version: 10.9.0
tags: [media, streaming]
schema:
  type: object
  properties:
    timezone: {type: string, title: Timezone, default: UTC}
defaultValues:
  timezone: UTC
services:
  - name: "{{ .Release.Name }}"
    port: 8096
    scheme: http
exposure:
  subdomain: jellyfin
  tls: true
  auth: true
  localOnly: false
`

func TestLoadParsesManifest(t *testing.T) {
	official := t.TempDir()
	writeApp(t, official, "jellyfin", "10.9.1", jellyfinManifest)

	c := Load([]SourceRef{{Name: "naslos", Channel: "Prod", Dir: official, Official: true}})

	app, err := c.Get("jellyfin")
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	if app.DisplayName != "Jellyfin" || app.Category != "media" {
		t.Fatalf("metadata mismatch: %+v", app)
	}
	// Chart.yaml version wins over the manifest's informational version.
	if app.Version != "10.9.1" {
		t.Fatalf("version = %q, want the chart version 10.9.1", app.Version)
	}
	if app.Source != "naslos" || app.ChartPath != "apps/jellyfin" {
		t.Fatalf("source/chartPath mismatch: %+v", app)
	}
	if len(app.Services) != 1 || app.Services[0].Port != 8096 || app.Services[0].Scheme != "http" {
		t.Fatalf("services mismatch: %+v", app.Services)
	}
	if len(app.Ports) != 1 || app.Ports[0] != 8096 {
		t.Fatalf("ports mismatch: %v", app.Ports)
	}
	if !app.Exposure.TLS || !app.Exposure.Auth || app.Exposure.Subdomain != "jellyfin" {
		t.Fatalf("exposure mismatch: %+v", app.Exposure)
	}
	if len(app.Schema) == 0 {
		t.Fatal("schema was not decoded")
	}
}

func TestUserRepoOverridesOfficial(t *testing.T) {
	official := t.TempDir()
	writeApp(t, official, "jellyfin", "10.9.1", jellyfinManifest)

	user := t.TempDir()
	writeApp(t, user, "jellyfin", "11.0.0", `
name: jellyfin
displayName: My Jellyfin
category: custom
services:
  - name: "{{ .Release.Name }}"
    port: 9096
`)

	c := Load([]SourceRef{
		{Name: "naslos", Channel: "Prod", Dir: official, Official: true},
		{Name: "mine", Channel: "Prod", Dir: user},
	})

	app, err := c.Get("jellyfin")
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	if app.Source != "mine" || app.DisplayName != "My Jellyfin" || app.Version != "11.0.0" {
		t.Fatalf("user repo did not win: %+v", app)
	}
}

func TestChannelsAggregatedPerWinningSource(t *testing.T) {
	official := t.TempDir()
	writeApp(t, official, "plex", "1.0.0", "name: plex\n")
	officialDev := t.TempDir()
	writeApp(t, officialDev, "plex", "1.1.0", "name: plex\n")

	c := Load([]SourceRef{
		{Name: "naslos", Channel: "Prod", Dir: official, Official: true},
		{Name: "naslos", Channel: "Develop", Dir: officialDev, Official: true},
	})
	app, err := c.Get("plex")
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	if app.Channel != "Prod" {
		t.Fatalf("default channel = %q, want Prod", app.Channel)
	}
	if len(app.Channels) != 2 {
		t.Fatalf("channels = %v, want [Develop Prod]", app.Channels)
	}
}

func TestLoadSkipsInvalidManifest(t *testing.T) {
	dir := t.TempDir()
	// name does not match the folder -> skipped.
	writeApp(t, dir, "good", "1.0.0", "name: good\n")
	if err := os.MkdirAll(filepath.Join(dir, "apps", "bad"), 0755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	if err := os.WriteFile(filepath.Join(dir, "apps", "bad", manifestFile), []byte("name: other\n"), 0644); err != nil {
		t.Fatalf("write: %v", err)
	}

	c := Load([]SourceRef{{Name: "naslos", Channel: "Prod", Dir: dir, Official: true}})
	if !c.Has("good") {
		t.Fatal("valid app was not loaded")
	}
	if c.Has("bad") || c.Has("other") {
		t.Fatal("invalid manifest was loaded")
	}
}

func TestLoadMissingDirectoryIsNotFatal(t *testing.T) {
	c := Load([]SourceRef{{Name: "naslos", Channel: "Prod", Dir: filepath.Join(t.TempDir(), "nope")}})
	if len(c.List()) != 0 {
		t.Fatalf("expected an empty catalog, got %d entries", len(c.List()))
	}
}

func TestGetReturnsDeepCopy(t *testing.T) {
	dir := t.TempDir()
	writeApp(t, dir, "jellyfin", "10.9.1", jellyfinManifest)
	c := Load([]SourceRef{{Name: "naslos", Channel: "Prod", Dir: dir, Official: true}})

	first, err := c.Get("jellyfin")
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	first.DefaultValues["timezone"] = "mutated"

	second, err := c.Get("jellyfin")
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	if second.DefaultValues["timezone"] != "UTC" {
		t.Fatalf("defaults leaked between reads: %v", second.DefaultValues["timezone"])
	}
}

func TestValidServiceNameTemplate(t *testing.T) {
	cases := map[string]bool{
		"{{ .Release.Name }}":   true,
		"my-app":                 true,
		"my-{{ .Release.Name }}": false,
		"{{ .Values.foo }}":      false,
		"":                       false,
	}
	for name, want := range cases {
		if got := validServiceNameTemplate(name); got != want {
			t.Errorf("validServiceNameTemplate(%q) = %v, want %v", name, got, want)
		}
	}
}
