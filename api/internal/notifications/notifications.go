// Package notifications provides ntfy push notification management for Naslos.
package notifications

import (
	"net/http"
	"sync"
	"time"
)

// EventType is the type of event that triggers a notification.
type EventType string

const (
	EventZFSHealth     EventType = "zfs_health"
	EventZFSScrub      EventType = "zfs_scrub"
	EventAppStatus     EventType = "app_status"
	EventDiskFailure   EventType = "disk_failure"
	EventSystemUpdate  EventType = "system_update"
	EventShareAccess   EventType = "share_access"
)

// Severity is the notification severity level.
type Severity string

const (
	SeverityInfo     Severity = "info"
	SeverityWarning  Severity = "warning"
	SeverityError    Severity = "error"
	SeverityCritical Severity = "critical"
)

// Settings holds the ntfy notification configuration.
type Settings struct {
	Enabled       bool        `json:"enabled"`
	ServerURL     string      `json:"serverUrl"`
	Topic         string      `json:"topic"`
	AuthToken     string      `json:"authToken"`
	Email         string      `json:"email"`
	EnabledEvents []EventType `json:"enabledEvents"`
	MinSeverity   Severity   `json:"minSeverity"`
}

// Notification is a notification to be sent.
type Notification struct {
	Title    string   `json:"title"`
	Message  string   `json:"message"`
	Severity Severity `json:"severity"`
	Tags     []string `json:"tags"`
	Time     time.Time `json:"time"`
}

// Manager manages ntfy notifications.
type Manager struct {
	mu         sync.Mutex
	settings   Settings
	client     *http.Client
	configPath string
}

// NewManager creates a new notification manager.
func NewManager(configPath string) *Manager {
	m := &Manager{
		client:     &http.Client{Timeout: 10 * time.Second},
		configPath: configPath,
	}
	m.load()
	return m
}
