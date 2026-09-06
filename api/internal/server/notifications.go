package server

import (
	"encoding/json"
	"net/http"
)

// handleNotifications handles ntfy notification settings.
func (s *Server) handleNotifications(w http.ResponseWriter, r *http.Request) {
	switch r.Method {
	case http.MethodGet:
		// Get notification settings
		writeJSON(w, http.StatusOK, map[string]string{"status": "getting notification settings"})
	case http.MethodPut:
		// Update notification settings
		var req struct {
			Enabled    bool   `json:"enabled"`
			ServerURL  string `json:"serverUrl"`
			Topic      string `json:"topic"`
			AuthToken  string `json:"authToken"`
		}
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		writeJSON(w, http.StatusOK, map[string]string{"status": "notification settings updated"})
	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}
