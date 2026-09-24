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
	if err := first.UpdateSettings(want, &want.AuthToken); err != nil {
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
	if err := m.UpdateSettings(Settings{Enabled: true, Topic: "x"}, nil); err != nil {
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

// TestUpdateSettingsTokenSemantics pins CR-07's write side: a nil token keeps
// the stored one (an ordinary settings save omits it), an empty string clears
// it, and a value replaces it.
func TestUpdateSettingsTokenSemantics(t *testing.T) {
	m := NewManager("")
	token := "tk_secret"
	if err := m.UpdateSettings(Settings{Enabled: true, Topic: "a"}, &token); err != nil {
		t.Fatalf("setting the token: %v", err)
	}

	// nil keeps it.
	if err := m.UpdateSettings(Settings{Enabled: true, Topic: "b"}, nil); err != nil {
		t.Fatalf("keeping the token: %v", err)
	}
	if got := m.GetSettings(); got.AuthToken != token || got.Topic != "b" {
		t.Errorf("after a nil token: %+v, want topic b and the stored token", got)
	}

	// A value replaces it.
	replacement := "tk_new"
	if err := m.UpdateSettings(Settings{Enabled: true, Topic: "c"}, &replacement); err != nil {
		t.Fatalf("replacing the token: %v", err)
	}
	if got := m.GetSettings(); got.AuthToken != replacement {
		t.Errorf("after a replacement: token = %q, want %q", got.AuthToken, replacement)
	}

	// An explicit empty string clears it.
	empty := ""
	if err := m.UpdateSettings(Settings{Enabled: true, Topic: "d"}, &empty); err != nil {
		t.Fatalf("clearing the token: %v", err)
	}
	if got := m.GetSettings(); got.AuthToken != "" {
		t.Errorf("after clearing: token = %q, want empty", got.AuthToken)
	}
}

// TestNotificationURLHardening is the PF-L8 guard: only http/https is accepted,
// metadata/link-local targets are refused, and the topic is escaped.
func TestNotificationURLHardening(t *testing.T) {
	good := []struct {
		server, topic, want string
	}{
		{"https://ntfy.sh", "naslos-alerts", "https://ntfy.sh/naslos-alerts"},
		// A self-hosted ntfy on the LAN is a supported deployment.
		{"http://192.168.1.10:8080", "alerts", "http://192.168.1.10:8080/alerts"},
		// A base path is preserved.
		{"https://example.com/ntfy", "a-b", "https://example.com/ntfy/a-b"},
	}
	for _, tc := range good {
		got, err := notificationURL(tc.server, tc.topic)
		if err != nil {
			t.Errorf("notificationURL(%q, %q) = %v, want nil", tc.server, tc.topic, err)
			continue
		}
		if got != tc.want {
			t.Errorf("notificationURL(%q, %q) = %q, want %q", tc.server, tc.topic, got, tc.want)
		}
	}

	bad := []string{
		"file:///etc/passwd",
		"gopher://ntfy.sh",
		"http://169.254.169.254", // cloud metadata
		"http://[fe80::1]",       // link-local
	}
	for _, server := range bad {
		if _, err := notificationURL(server, "topic"); err == nil {
			t.Errorf("notificationURL(%q) = nil error, want a rejection", server)
		}
	}

	// The topic cannot climb out of the publish path: only ntfy's charset is
	// accepted.
	for _, topic := range []string{"../admin", "a/b", "a b", ""} {
		if _, err := notificationURL("https://ntfy.sh", topic); err == nil {
			t.Errorf("notificationURL(topic=%q) = nil error, want a rejection", topic)
		}
	}
}
