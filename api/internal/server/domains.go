package server

import (
	"encoding/json"
	"net/http"
	"strings"
	"time"

	"github.com/AessemOps/Naslos-Linux/api/internal/certs"
)

// handleDomains lists and creates base domains with ACME certificates.
func (s *Server) handleDomains(w http.ResponseWriter, r *http.Request) {
	if s.domains == nil {
		writeError(w, http.StatusServiceUnavailable, "domain management is not available")
		return
	}
	switch r.Method {
	case http.MethodGet:
		writeJSON(w, http.StatusOK, map[string]interface{}{
			"domains":     s.domains.List(),
			"certManager": s.certs != nil,
			"baseDomain":  s.baseDomain,
		})

	case http.MethodPost:
		var domain certs.Domain
		if err := json.NewDecoder(r.Body).Decode(&domain); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		domain.Primary = false
		s.upsertDomain(w, r, domain, http.StatusCreated)

	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// handleDomainDetail reads, updates, deletes a domain, or reports its certificate.
func (s *Server) handleDomainDetail(w http.ResponseWriter, r *http.Request) {
	if s.domains == nil {
		writeError(w, http.StatusServiceUnavailable, "domain management is not available")
		return
	}
	path := strings.TrimPrefix(r.URL.Path, "/api/domains/")
	if strings.HasSuffix(path, "/certificate") {
		s.handleDomainCertificate(w, r, strings.TrimSuffix(path, "/certificate"))
		return
	}
	name := path
	if name == "" || strings.Contains(name, "/") {
		writeError(w, http.StatusNotFound, "domain not found")
		return
	}

	switch r.Method {
	case http.MethodGet:
		domain, err := s.domains.Get(name)
		if err != nil {
			writeError(w, http.StatusNotFound, err.Error())
			return
		}
		writeJSON(w, http.StatusOK, domain)

	case http.MethodPut:
		var domain certs.Domain
		if err := json.NewDecoder(r.Body).Decode(&domain); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		if domain.BaseDomain == "" {
			domain.BaseDomain = name
		}
		s.upsertDomain(w, r, domain, http.StatusOK)

	case http.MethodDelete:
		domain, err := s.domains.Get(name)
		if err != nil {
			writeError(w, http.StatusNotFound, err.Error())
			return
		}
		if s.certs != nil {
			if err := s.certs.Delete(r.Context(), domain); err != nil {
				writeError(w, http.StatusBadGateway, err.Error())
				return
			}
		}
		if err := s.domains.Delete(name); err != nil {
			writeError(w, http.StatusNotFound, err.Error())
			return
		}
		writeJSON(w, http.StatusOK, map[string]string{"status": "domain removed", "name": name})

	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// upsertDomain validates, persists and applies a domain.
func (s *Server) upsertDomain(w http.ResponseWriter, r *http.Request, domain certs.Domain, status int) {
	if domain.Environment == "" {
		domain.Environment = "staging"
	}
	if err := domain.Validate(); err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	now := time.Now().UTC()
	if existing, err := s.domains.Get(domain.BaseDomain); err == nil {
		domain.CreatedAt = existing.CreatedAt
	} else {
		domain.CreatedAt = now
	}
	domain.UpdatedAt = now

	if s.certs != nil {
		if err := s.certs.Apply(r.Context(), domain); err != nil {
			domain.LastError = err.Error()
		} else {
			domain.LastError = ""
		}
	} else {
		domain.LastError = "cert-manager is not installed"
	}
	if err := s.domains.Upsert(domain); err != nil {
		writeError(w, http.StatusInternalServerError, err.Error())
		return
	}
	writeJSON(w, status, domain)
}

// handleDomainCertificate reports a domain's certificate status.
func (s *Server) handleDomainCertificate(w http.ResponseWriter, r *http.Request, name string) {
	if r.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}
	domain, err := s.domains.Get(name)
	if err != nil {
		writeError(w, http.StatusNotFound, err.Error())
		return
	}
	if s.certs == nil {
		writeJSON(w, http.StatusOK, map[string]interface{}{
			"status":     "unavailable",
			"reason":     "cert-manager is not installed",
			"secretName": domain.SecretName(),
		})
		return
	}
	status, err := s.certs.Status(r.Context(), domain)
	if err != nil {
		writeError(w, http.StatusBadGateway, err.Error())
		return
	}
	writeJSON(w, http.StatusOK, status)
}
