package server

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"strings"
	"time"

	"github.com/AessemOps/Naslos-Linux/api/internal/certs"
	"github.com/AessemOps/Naslos-Linux/api/internal/ddns"
)

// domainRequest is a domain create/update body. `fields` carries the selected
// provider's credential fields; the server splits them into a Secret (secret
// fields) and the domain's providerConfig (the rest).
type domainRequest struct {
	certs.Domain
	Fields map[string]string `json:"fields"`
}

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
		var req domainRequest
		if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 64<<10)).Decode(&req); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		req.Primary = false
		s.upsertDomain(w, r, req, http.StatusCreated)

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
		var req domainRequest
		if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 64<<10)).Decode(&req); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
		if req.BaseDomain == "" {
			req.BaseDomain = name
		}
		s.upsertDomain(w, r, req, http.StatusOK)

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
func (s *Server) upsertDomain(w http.ResponseWriter, r *http.Request, req domainRequest, status int) {
	domain := req.Domain
	if domain.Environment == "" {
		domain.Environment = "staging"
	}
	existing, getErr := s.domains.Get(domain.BaseDomain)
	existed := getErr == nil

	if req.Fields != nil {
		var prior *certs.Domain
		if existed {
			prior = &existing
		}
		if err := s.applyDomainFields(r, &domain, req.Fields, prior); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
	}
	if err := domain.Validate(); err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	now := time.Now().UTC()
	if existed {
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

// applyDomainFields splits a submitted provider field map into the non-secret
// providerConfig and a credential Secret, writing the Secret in the apps
// namespace. When no secret value is submitted the existing Secret is kept.
func (s *Server) applyDomainFields(r *http.Request, domain *certs.Domain, fields map[string]string, existing *certs.Domain) error {
	if s.providers == nil {
		return fmt.Errorf("no DNS provider registry is loaded")
	}
	p, ok := s.providers.Get(string(domain.DNSProvider))
	if !ok {
		return fmt.Errorf("unsupported DNS provider %q", domain.DNSProvider)
	}
	if !p.SupportsCertificates() {
		return fmt.Errorf("provider %q does not support certificates", p.Name)
	}
	if p.IsPassthrough() {
		// Passthrough takes a raw solver; there are no provider fields.
		return nil
	}
	create := existing == nil

	var existingConfig map[string]string
	if existing != nil {
		existingConfig = existing.ProviderConfig
	}
	resolved, err := p.ResolveFields("cert", fields, existingConfig, nil, create)
	if err != nil {
		return err
	}
	domain.ProviderConfig = resolved.Config
	secretData := resolved.SecretValues

	secretName := domain.CredentialsSecret
	if secretName == "" {
		secretName = "naslos-domain-" + sanitizeName(domain.BaseDomain) + "-creds"
	}
	// Reject a domain whose credentials cannot render a solver (a missing
	// app key, for example) rather than storing one that never issues.
	if err := p.ValidateSolver(secretName, resolved.Config); err != nil {
		return err
	}
	if len(secretData) == 0 {
		// Nothing new to store: keep whatever Secret the domain already names.
		return nil
	}
	client, err := s.kubernetesClient()
	if err != nil {
		return fmt.Errorf("cannot reach the Kubernetes API to store credentials: %w", err)
	}
	ctx, cancel := context.WithTimeout(r.Context(), 15*time.Second)
	defer cancel()
	if err := ddns.WriteSecret(ctx, client, s.appsNamespace, secretName, secretData); err != nil {
		return fmt.Errorf("storing credentials: %w", err)
	}
	domain.CredentialsSecret = secretName
	return nil
}

// sanitizeName reduces a domain to a DNS-1123-safe component.
func sanitizeName(name string) string {
	return strings.Trim(strings.Map(func(r rune) rune {
		switch {
		case r >= 'a' && r <= 'z', r >= '0' && r <= '9':
			return r
		case r == '.':
			return '-'
		default:
			return '-'
		}
	}, strings.ToLower(name)), "-")
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
