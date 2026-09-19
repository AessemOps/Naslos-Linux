package notifications

import (
	"bytes"
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"time"
)

// load loads notification settings from the config file.
func (m *Manager) load() {
	if m.configPath == "" {
		m.settings = defaultSettings()
		return
	}

	data, err := os.ReadFile(m.configPath)
	if err != nil {
		m.settings = defaultSettings()
		return
	}

	var settings Settings
	if err := json.Unmarshal(data, &settings); err != nil {
		m.settings = defaultSettings()
		return
	}
	m.settings = settings
}

// save persists notification settings to the config file.
func (m *Manager) save() error {
	if m.configPath == "" {
		return nil
	}

	dir := filepath.Dir(m.configPath)
	if err := os.MkdirAll(dir, 0750); err != nil {
		return fmt.Errorf("creating config dir: %w", err)
	}

	data, err := json.MarshalIndent(m.settings, "", "  ")
	if err != nil {
		return fmt.Errorf("marshaling settings: %w", err)
	}

	// 0600: this file holds the ntfy auth token. The container runs as a single
	// dedicated user, so nothing else needs to read it.
	if err := os.WriteFile(m.configPath, data, 0600); err != nil {
		return fmt.Errorf("writing settings: %w", err)
	}

	return nil
}

// defaultSettings returns the default notification settings.
func defaultSettings() Settings {
	return Settings{
		Enabled:   false,
		ServerURL: "https://ntfy.sh",
		Topic:     "naslos-alerts",
		EnabledEvents: []EventType{
			EventZFSHealth,
			EventAppStatus,
			EventDiskFailure,
		},
		MinSeverity: SeverityWarning,
	}
}

// GetSettings returns the current notification settings.
func (m *Manager) GetSettings() Settings {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.settings
}

// UpdateSettings updates the notification settings. The stored auth token is
// only touched when authToken is non-nil: a nil pointer keeps the existing
// token, an empty string clears it, and any other value replaces it. The API
// layer never returns the token, so a normal settings save omits the field and
// must not wipe the operator's token.
func (m *Manager) UpdateSettings(settings Settings, authToken *string) error {
	m.mu.Lock()
	defer m.mu.Unlock()

	if authToken != nil {
		settings.AuthToken = *authToken
	} else {
		settings.AuthToken = m.settings.AuthToken
	}
	m.settings = settings
	return m.save()
}

// Send sends a notification via ntfy.
func (m *Manager) Send(notif Notification) error {
	m.mu.Lock()
	defer m.mu.Unlock()

	if !m.settings.Enabled {
		return nil
	}

	if !severityMeetsThreshold(notif.Severity, m.settings.MinSeverity) {
		return nil
	}

	serverURL := m.settings.ServerURL
	if serverURL == "" {
		serverURL = "https://ntfy.sh"
	}
	url := fmt.Sprintf("%s/%s", strings.TrimSuffix(serverURL, "/"), m.settings.Topic)

	req, err := http.NewRequest("POST", url, bytes.NewBufferString(notif.Message))
	if err != nil {
		return fmt.Errorf("creating request: %w", err)
	}

	req.Header.Set("Title", notif.Title)
	req.Header.Set("X-Priority", severityToPriority(notif.Severity))

	if len(notif.Tags) > 0 {
		req.Header.Set("Tags", strings.Join(notif.Tags, ","))
	}

	if m.settings.AuthToken != "" {
		req.Header.Set("Authorization", "Bearer "+m.settings.AuthToken)
	}

	if m.settings.Email != "" {
		req.Header.Set("X-Email", m.settings.Email)
	}

	resp, err := m.client.Do(req)
	if err != nil {
		return fmt.Errorf("sending notification: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode >= 400 {
		return fmt.Errorf("ntfy returned status %d", resp.StatusCode)
	}

	return nil
}

// SendTest sends a test notification.
func (m *Manager) SendTest() error {
	return m.Send(Notification{
		Title:    "Naslos Test",
		Message:  "This is a test notification from Naslos.",
		Severity: SeverityInfo,
		Tags:     []string{"white_check_mark", "naslos"},
		Time:     time.Now(),
	})
}

// severityMeetsThreshold checks if the notification severity meets the minimum threshold.
func severityMeetsThreshold(severity, threshold Severity) bool {
	levels := map[Severity]int{
		SeverityInfo:     0,
		SeverityWarning:  1,
		SeverityError:    2,
		SeverityCritical: 3,
	}
	return levels[severity] >= levels[threshold]
}

// severityToPriority converts severity to ntfy priority.
func severityToPriority(severity Severity) string {
	switch severity {
	case SeverityInfo:
		return "2"
	case SeverityWarning:
		return "3"
	case SeverityError:
		return "4"
	case SeverityCritical:
		return "5"
	default:
		return "3"
	}
}
