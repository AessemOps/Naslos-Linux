package server

import (
	"encoding/json"
	"net/http"
	"strings"

	"github.com/AessemOps/Naslos-Linux/api/internal/chartsrepo"
)

// handleSources lists and adds chart repositories.
func (s *Server) handleSources(w http.ResponseWriter, r *http.Request) {
	if s.charts == nil {
		writeError(w, http.StatusServiceUnavailable, "chart repositories are not available")
		return
	}
	switch r.Method {
	case http.MethodGet:
		writeJSON(w, http.StatusOK, s.charts.Sources())

	case http.MethodPost:
		var src chartsrepo.Source
		if err := json.NewDecoder(r.Body).Decode(&src); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		// Official is reserved for the chart-seeded source.
		src.Official = false
		added, err := s.charts.AddSource(&src)
		if err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		writeJSON(w, http.StatusCreated, added)

	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// handleSourcesRefresh refreshes one or all sources and rebuilds the catalog.
func (s *Server) handleSourcesRefresh(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}
	if s.charts == nil {
		writeError(w, http.StatusServiceUnavailable, "chart repositories are not available")
		return
	}
	name := strings.TrimSpace(r.URL.Query().Get("name"))
	if name != "" {
		src, err := s.charts.GetSource(name)
		if err != nil {
			writeError(w, http.StatusNotFound, err.Error())
			return
		}
		var errs []string
		for _, channel := range src.ChannelNames() {
			if err := s.charts.Refresh(r.Context(), name, channel); err != nil {
				errs = append(errs, err.Error())
			}
		}
		s.catalog.Store(s.buildCatalog())
		if len(errs) > 0 {
			writeJSON(w, http.StatusBadGateway, map[string]interface{}{
				"status": "partial", "errors": errs, "catalog": len(s.catalog.Load().List()),
			})
			return
		}
	} else if err := s.refreshSources(r.Context()); err != nil {
		writeJSON(w, http.StatusBadGateway, map[string]interface{}{
			"status": "partial", "errors": []string{err.Error()}, "catalog": len(s.catalog.Load().List()),
		})
		return
	}
	writeJSON(w, http.StatusOK, map[string]interface{}{
		"status":  "refreshed",
		"catalog": len(s.catalog.Load().List()),
	})
}

// handleSourceDetail reads or deletes a source.
func (s *Server) handleSourceDetail(w http.ResponseWriter, r *http.Request) {
	if s.charts == nil {
		writeError(w, http.StatusServiceUnavailable, "chart repositories are not available")
		return
	}
	name := strings.TrimPrefix(r.URL.Path, "/api/sources/")
	if name == "" || strings.Contains(name, "/") {
		writeError(w, http.StatusNotFound, "source not found")
		return
	}
	switch r.Method {
	case http.MethodGet:
		src, err := s.charts.GetSource(name)
		if err != nil {
			writeError(w, http.StatusNotFound, err.Error())
			return
		}
		writeJSON(w, http.StatusOK, src)

	case http.MethodDelete:
		if err := s.charts.DeleteSource(name); err != nil {
			writeError(w, http.StatusNotFound, err.Error())
			return
		}
		s.catalog.Store(s.buildCatalog())
		writeJSON(w, http.StatusOK, map[string]string{"status": "source removed", "name": name})

	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}
