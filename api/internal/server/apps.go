package server

import (
	"encoding/json"
	"fmt"
	"net/http"
	"strings"

	"github.com/AessemOps/Naslos-Linux/api/internal/apps"
)

// handleCatalog returns the list of available apps in the catalog.
func (s *Server) handleCatalog(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}
	snapshot := s.catalog.Load()
	if snapshot == nil {
		writeJSON(w, http.StatusOK, []any{})
		return
	}
	writeJSON(w, http.StatusOK, snapshot.List())
}

// handleCatalogApp returns details of a specific catalog app, including its schema.
func (s *Server) handleCatalogApp(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}
	name := strings.TrimPrefix(r.URL.Path, "/api/catalog/")
	snapshot := s.catalog.Load()
	if snapshot == nil {
		writeError(w, http.StatusNotFound, "catalog is not available")
		return
	}
	app, err := snapshot.Get(name)
	if err != nil {
		writeError(w, http.StatusNotFound, err.Error())
		return
	}
	writeJSON(w, http.StatusOK, app)
}

// handleApps handles installed app operations.
func (s *Server) handleApps(w http.ResponseWriter, r *http.Request) {
	if s.appManager == nil {
		writeError(w, http.StatusServiceUnavailable, "app management is not available")
		return
	}

	switch r.Method {
	case http.MethodGet:
		views, err := s.appManager.List(r.Context())
		if err != nil {
			writeError(w, http.StatusInternalServerError, err.Error())
			return
		}
		writeJSON(w, http.StatusOK, views)

	case http.MethodPost:
		var req struct {
			Name       string                 `json:"name"`
			Values     map[string]interface{} `json:"values"`
			Exposure   *apps.Exposure         `json:"exposure,omitempty"`
			BaseDomain string                 `json:"baseDomain,omitempty"`
			Confirmed  bool                   `json:"confirmed"`
		}
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		if !req.Confirmed {
			writeError(w, http.StatusBadRequest, "install must be explicitly confirmed")
			return
		}
		if req.BaseDomain != "" && !s.baseDomainSelectable(req.BaseDomain) {
			writeError(w, http.StatusBadRequest, fmt.Sprintf("base domain %q is not configured", req.BaseDomain))
			return
		}
		if conflict := s.ensureAppJobs().conflicting(req.Name); conflict != nil {
			writeAppJobConflict(w, conflict)
			return
		}
		job := s.enqueueAppJob(appJobInstall, req.Name, req.BaseDomain, apps.InstallRequest{
			Name:       req.Name,
			Values:     req.Values,
			Exposure:   req.Exposure,
			BaseDomain: req.BaseDomain,
		}, nil)
		writeJSON(w, http.StatusAccepted, map[string]any{"jobId": job.ID, "state": appJobRunning})

	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// handleAppDetail handles individual app operations (configure/exposure/uninstall).
func (s *Server) handleAppDetail(w http.ResponseWriter, r *http.Request) {
	if s.appManager == nil {
		writeError(w, http.StatusServiceUnavailable, "app management is not available")
		return
	}
	path := strings.TrimPrefix(r.URL.Path, "/api/apps/")
	// Defensive: the dedicated jobs routes match first, but a reorganization
	// must not turn `jobs` into an app name.
	if path == "jobs" {
		s.handleAppJobs(w, r)
		return
	}
	if strings.HasPrefix(path, "jobs/") {
		s.handleAppJobDetail(w, r)
		return
	}
	if strings.HasSuffix(path, "/exposure") {
		s.handleAppExposure(w, r, strings.TrimSuffix(path, "/exposure"))
		return
	}
	if strings.HasSuffix(path, "/services") {
		s.handleAppServices(w, r, strings.TrimSuffix(path, "/services"))
		return
	}
	name := path
	if name == "" || strings.Contains(name, "/") {
		writeError(w, http.StatusNotFound, "app not found")
		return
	}

	switch r.Method {
	case http.MethodGet:
		view, err := s.appManager.GetView(r.Context(), name)
		if err != nil {
			writeError(w, http.StatusNotFound, err.Error())
			return
		}
		writeJSON(w, http.StatusOK, view)

	case http.MethodPut:
		var req struct {
			Values map[string]interface{} `json:"values"`
		}
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		if _, err := s.appManager.Get(name); err != nil {
			writeError(w, http.StatusNotFound, err.Error())
			return
		}
		if conflict := s.ensureAppJobs().conflicting(name); conflict != nil {
			writeAppJobConflict(w, conflict)
			return
		}
		job := s.enqueueAppJob(appJobUpgrade, name, "", apps.InstallRequest{}, req.Values)
		writeJSON(w, http.StatusAccepted, map[string]any{"jobId": job.ID, "state": appJobRunning})

	case http.MethodDelete:
		if _, err := s.appManager.Get(name); err != nil {
			writeError(w, http.StatusNotFound, err.Error())
			return
		}
		if conflict := s.ensureAppJobs().conflicting(name); conflict != nil {
			writeAppJobConflict(w, conflict)
			return
		}
		job := s.enqueueAppJob(appJobUninstall, name, "", apps.InstallRequest{}, nil)
		writeJSON(w, http.StatusAccepted, map[string]any{"jobId": job.ID, "state": appJobRunning})

	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// handleAppServices lists the Services a release rendered, for the exposure
// UI's route-target picker.
func (s *Server) handleAppServices(w http.ResponseWriter, r *http.Request, name string) {
	if r.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}
	if name == "" || strings.Contains(name, "/") {
		writeError(w, http.StatusNotFound, "app not found")
		return
	}
	services, err := s.appManager.DiscoverServices(r.Context(), name)
	if err != nil {
		writeError(w, http.StatusNotFound, err.Error())
		return
	}
	writeJSON(w, http.StatusOK, services)
}

// handleAppExposure reads or updates an app's exposure settings.
func (s *Server) handleAppExposure(w http.ResponseWriter, r *http.Request, name string) {
	switch r.Method {
	case http.MethodGet:
		rec, err := s.appManager.Get(name)
		if err != nil {
			writeError(w, http.StatusNotFound, err.Error())
			return
		}
		baseDomain := rec.BaseDomain
		if baseDomain == "" {
			baseDomain = s.baseDomain
		}
		writeJSON(w, http.StatusOK, map[string]interface{}{
			"exposure":          rec.Exposure,
			"baseDomain":        baseDomain,
			"primaryDomain":     s.baseDomain,
			"selectableDomains": s.selectableDomains(),
			"ssoDomains":        s.effectiveSSODomains(),
			"authAllowed":       s.appManager.AuthAllowed(rec.BaseDomain),
			"lastError":         rec.LastError,
		})

	case http.MethodPut:
		var req struct {
			Exposure   apps.Exposure `json:"exposure"`
			BaseDomain string        `json:"baseDomain,omitempty"`
		}
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		if req.BaseDomain != "" && !s.baseDomainSelectable(req.BaseDomain) {
			writeError(w, http.StatusBadRequest, fmt.Sprintf("base domain %q is not configured", req.BaseDomain))
			return
		}
		view, err := s.appManager.SetExposure(r.Context(), name, req.Exposure, req.BaseDomain)
		if err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		writeJSON(w, http.StatusOK, view)

	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}
