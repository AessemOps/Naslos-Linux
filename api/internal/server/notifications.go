package server

import (
	"encoding/json"
	"net/http"

	"github.com/AessemOps/Naslos-Linux/api/internal/notifications"
)

// notificationSettingsResponse is the browser-facing view of the ntfy settings.
// It deliberately carries only `hasAuthToken`, never the token itself: the token
// is a publish credential for the operator's topic and must not reach the page.
type notificationSettingsResponse struct {
	Enabled       bool                      `json:"enabled"`
	ServerURL     string                    `json:"serverUrl"`
	Topic         string                    `json:"topic"`
	HasAuthToken  bool                      `json:"hasAuthToken"`
	Email         string                    `json:"email"`
	EnabledEvents []notifications.EventType `json:"enabledEvents"`
	MinSeverity   notifications.Severity    `json:"minSeverity"`
}

// handleNotifications handles ntfy notification settings.
func (s *Server) handleNotifications(w http.ResponseWriter, r *http.Request) {
	switch r.Method {
	case http.MethodGet:
		settings := s.notifications.GetSettings()
		writeJSON(w, http.StatusOK, notificationSettingsResponse{
			Enabled:       settings.Enabled,
			ServerURL:     settings.ServerURL,
			Topic:         settings.Topic,
			HasAuthToken:  settings.AuthToken != "",
			Email:         settings.Email,
			EnabledEvents: settings.EnabledEvents,
			MinSeverity:   settings.MinSeverity,
		})

	case http.MethodPut:
		// AuthToken is a pointer so an ordinary save that omits it keeps the
		// stored token; only an explicit "" clears it.
		var req struct {
			Enabled       bool                      `json:"enabled"`
			ServerURL     string                    `json:"serverUrl"`
			Topic         string                    `json:"topic"`
			AuthToken     *string                   `json:"authToken"`
			Email         string                    `json:"email"`
			EnabledEvents []notifications.EventType `json:"enabledEvents"`
			MinSeverity   notifications.Severity    `json:"minSeverity"`
		}
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		settings := notifications.Settings{
			Enabled:       req.Enabled,
			ServerURL:     req.ServerURL,
			Topic:         req.Topic,
			Email:         req.Email,
			EnabledEvents: req.EnabledEvents,
			MinSeverity:   req.MinSeverity,
		}
		if err := s.notifications.UpdateSettings(settings, req.AuthToken); err != nil {
			writeError(w, http.StatusInternalServerError, err.Error())
			return
		}
		writeJSON(w, http.StatusOK, map[string]string{"status": "notification settings updated"})

	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// handleNotificationTest sends a test notification.
func (s *Server) handleNotificationTest(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}

	if err := s.notifications.SendTest(); err != nil {
		writeError(w, http.StatusInternalServerError, err.Error())
		return
	}

	writeJSON(w, http.StatusOK, map[string]string{"status": "test notification sent"})
}
