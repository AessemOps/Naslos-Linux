package server

import (
	"encoding/json"
	"net/http"

	"github.com/AessemOps/Naslos-Linux/api/internal/helm"
)

// handleCatalog returns the list of available apps in the catalog.
func (s *Server) handleCatalog(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}
	writeJSON(w, http.StatusOK, s.catalog.List())
}

// handleCatalogApp returns details of a specific catalog app, including its schema.
func (s *Server) handleCatalogApp(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}

	name := r.URL.Path[len("/api/catalog/"):]
	app, err := s.catalog.Get(name)
	if err != nil {
		writeError(w, http.StatusNotFound, err.Error())
		return
	}

	writeJSON(w, http.StatusOK, app)
}

// handleApps handles installed app operations.
func (s *Server) handleApps(w http.ResponseWriter, r *http.Request) {
	switch r.Method {
	case http.MethodGet:
		// List installed apps
		apps, err := s.helm.List(r.Context())
		if err != nil {
			writeError(w, http.StatusInternalServerError, err.Error())
			return
		}
		writeJSON(w, http.StatusOK, apps)

	case http.MethodPost:
		// Install app from catalog
		var req struct {
			Name   string                 `json:"name"`
			Values map[string]interface{} `json:"values"`
		}
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}

		// Get catalog entry for chart info
		catalogApp, err := s.catalog.Get(req.Name)
		if err != nil {
			writeError(w, http.StatusNotFound, err.Error())
			return
		}

		// Merge default values with user values
		values := catalogApp.DefaultValues
		for k, v := range req.Values {
			values[k] = v
		}

		// Install via Helm
		rel, err := s.helm.Install(r.Context(), req.Name, catalogApp.Chart, values)
		if err != nil {
			writeError(w, http.StatusInternalServerError, err.Error())
			return
		}

		writeJSON(w, http.StatusCreated, map[string]string{
			"status":  "app installed",
			"name":    rel.Name,
			"chart":   rel.Chart.Name(),
			"version": rel.Chart.Metadata.Version,
		})

	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// handleAppDetail handles individual app operations (configure/start/stop/uninstall).
func (s *Server) handleAppDetail(w http.ResponseWriter, r *http.Request) {
	name := r.URL.Path[len("/api/apps/"):]

	switch r.Method {
	case http.MethodGet:
		// Get app details
		app, err := s.helm.Get(r.Context(), name)
		if err != nil {
			writeError(w, http.StatusNotFound, err.Error())
			return
		}
		writeJSON(w, http.StatusOK, app)

	case http.MethodPut:
		// Upgrade/configure app
		var req struct {
			Values map[string]interface{} `json:"values"`
		}
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}

		// Get catalog entry for chart reference
		catalogApp, err := s.catalog.Get(name)
		if err != nil {
			writeError(w, http.StatusNotFound, err.Error())
			return
		}

		rel, err := s.helm.Upgrade(r.Context(), name, catalogApp.Chart, req.Values)
		if err != nil {
			writeError(w, http.StatusInternalServerError, err.Error())
			return
		}

		writeJSON(w, http.StatusOK, map[string]string{
			"status": "app updated",
			"name":   rel.Name,
		})

	case http.MethodDelete:
		// Uninstall app
		if err := s.helm.Uninstall(r.Context(), name); err != nil {
			writeError(w, http.StatusInternalServerError, err.Error())
			return
		}
		writeJSON(w, http.StatusOK, map[string]string{
			"status": "app uninstalled",
			"name":   name,
		})

	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// Removed: handleAppStartStop was a stub that returned "not yet implemented",
// had no route registered and no caller in the UI (staticcheck U1000, Batch 6).
// App start/stop would be a real feature (scale replicas or a Helm suspend
// value) and should be added with its route and a test when it is built.

// ensure helm import is used
var _ = helm.ReleaseStatus
