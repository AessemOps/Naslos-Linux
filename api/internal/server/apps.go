package server

import (
	"encoding/json"
	"net/http"
)

// handleApps handles app catalog operations.
func (s *Server) handleApps(w http.ResponseWriter, r *http.Request) {
	switch r.Method {
	case http.MethodGet:
		// List installed apps
		writeJSON(w, http.StatusOK, map[string]string{"status": "listing apps"})
	case http.MethodPost:
		// Install app from catalog
		var req struct {
			Name      string            `json:"name"`
			Namespace string            `json:"namespace"`
			Values    map[string]string `json:"values"`
		}
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		writeJSON(w, http.StatusCreated, map[string]string{
			"status": "app install requested",
			"name":   req.Name,
		})
	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// handleAppDetail handles individual app operations (start/stop/configure).
func (s *Server) handleAppDetail(w http.ResponseWriter, r *http.Request) {
	app := r.URL.Path[len("/api/apps/"):]
	switch r.Method {
	case http.MethodGet:
		writeJSON(w, http.StatusOK, map[string]string{"status": "getting app", "app": app})
	case http.MethodPut:
		// Update/configure app
		writeJSON(w, http.StatusOK, map[string]string{"status": "updating app", "app": app})
	case http.MethodDelete:
		// Uninstall app
		writeJSON(w, http.StatusOK, map[string]string{"status": "uninstalling app", "app": app})
	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}
