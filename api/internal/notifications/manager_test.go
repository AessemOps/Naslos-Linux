package notifications

import (
	"path/filepath"
	"testing"
)

// TestSettingsSurviveARestart pins the persistence fix: the manager used to be
// constructed with an empty path, which it treats as "memory only", so every API
// restart silently reset the operator's topic, token and enabled events.
func TestSettingsSurviveARestart(t *testing.T) {
	path := filepath.Join(t.TempDir(), "notifications.json")

	first := NewManager(path)
	want := Settings{
		Enabled:       true,
		ServerURL:     "https://ntfy.example",
		Topic:         "naslos-prod",
		AuthToken:     "tk_secret",
		EnabledEvents: []EventType{EventBackupFailure, EventDiskFailure},
		MinSeverity:   SeverityError,
	}
	if err := first.UpdateSettings(want); err != nil {
		t.Fatalf("UpdateSettings: %v", err)
	}

	// A second manager on the same path is what a restart looks like.
	second := NewManager(path)
	got := second.GetSettings()

	if !got.Enabled || got.ServerURL != want.ServerURL || got.Topic != want.Topic || got.AuthToken != want.AuthToken {
		t.Errorf("settings after restart = %+v, want %+v", got, want)
	}
	if len(got.EnabledEvents) != 2 || got.EnabledEvents[0] != EventBackupFailure {
		t.Errorf("enabled events = %v, want the configured list", got.EnabledEvents)
	}
	if got.MinSeverity != SeverityError {
		t.Errorf("minSeverity = %q, want error", got.MinSeverity)
	}
}

// TestMemoryOnlyManagerStillWorks keeps the empty-path behaviour intentional: a
// manager with no path must not write anything, and must fall back to defaults.
func TestMemoryOnlyManagerStillWorks(t *testing.T) {
	m := NewManager("")
	if err := m.UpdateSettings(Settings{Enabled: true, Topic: "x"}); err != nil {
		t.Fatalf("UpdateSettings with no path: %v", err)
	}
	if got := m.GetSettings(); !got.Enabled || got.Topic != "x" {
		t.Errorf("settings = %+v, want the in-memory value", got)
	}

	fresh := NewManager("")
	if got := fresh.GetSettings(); got.Enabled {
		t.Errorf("a memory-only manager should start from defaults, got %+v", got)
	}
}
