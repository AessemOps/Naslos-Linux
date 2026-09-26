package server

import (
	"context"
	"encoding/json"
	"fmt"
	"log"
	"net/http"
	"sort"
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
			"domains":           s.domains.List(),
			"certManager":       s.certs != nil,
			"baseDomain":        s.baseDomain,
			"selectableDomains": s.selectableDomains(),
			"ssoDomains":        s.effectiveSSODomains(),
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
	if strings.HasSuffix(path, "/sso") {
		s.handleDomainSSO(w, r, strings.TrimSuffix(path, "/sso"))
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
		// A deleted domain drops out of the effective SSO list; reconcile the
		// fragments so Authelia stops protecting it. Best-effort.
		if domain.SSO || name == s.baseDomain {
			if err := s.syncSSOState(r.Context()); err != nil {
				log.Printf("Warning: Authelia SSO sync after removing %s failed: %v", name, err)
			}
		}
		writeJSON(w, http.StatusOK, map[string]string{"status": "domain removed", "name": name})

	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// selectableDomains returns the base domains an app exposure may use: the
// primary domain first, then every registered domain record, de-duplicated and
// with empties dropped.
func (s *Server) selectableDomains() []string {
	out := make([]string, 0, 1)
	seen := make(map[string]bool)
	add := func(domain string) {
		if domain == "" || seen[domain] {
			return
		}
		seen[domain] = true
		out = append(out, domain)
	}
	add(s.baseDomain)
	if s.domains != nil {
		for _, d := range s.domains.List() {
			add(d.BaseDomain)
		}
	}
	return out
}

// effectiveSSODomains returns the domains Authelia protects: the primary domain
// plus the chart-declared SSO_DOMAINS env seed and any store domain promoted to
// SSO, de-duplicated. The chart list is a floor (it cannot be demoted from the
// UI); store-promoted domains can be toggled live.
func (s *Server) effectiveSSODomains() []string {
	out := make([]string, 0, 1)
	seen := make(map[string]bool)
	add := func(domain string) {
		if domain == "" || seen[domain] {
			return
		}
		seen[domain] = true
		out = append(out, domain)
	}
	add(s.baseDomain)
	for _, d := range s.ssoDomains {
		add(d)
	}
	if s.domains != nil {
		for _, d := range s.domains.List() {
			if d.SSO {
				add(d.BaseDomain)
			}
		}
	}
	return out
}

// baseDomainSelectable reports whether a non-empty base domain is one of the
// configured domains an exposure may point at.
func (s *Server) baseDomainSelectable(baseDomain string) bool {
	for _, d := range s.selectableDomains() {
		if d == baseDomain {
			return true
		}
	}
	return false
}

// handleDomainSSO promotes or demotes a registered domain in the effective
// Authelia SSO list. The primary domain is always SSO; a chart-declared
// SSO_DOMAINS entry is a floor that cannot be demoted from the UI. Demotion is
// refused while an installed app still requires auth on the domain, because its
// route would start failing the SSO gate.
func (s *Server) handleDomainSSO(w http.ResponseWriter, r *http.Request, name string) {
	if r.Method != http.MethodPost {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}
	if s.domains == nil {
		writeError(w, http.StatusServiceUnavailable, "domain management is not available")
		return
	}
	if name == "" || strings.Contains(name, "/") {
		writeError(w, http.StatusNotFound, "domain not found")
		return
	}
	var req struct {
		Enabled bool `json:"enabled"`
	}
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 4<<10)).Decode(&req); err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	if name == s.baseDomain {
		writeError(w, http.StatusBadRequest, "the primary domain is always an SSO domain")
		return
	}
	if _, err := s.domains.Get(name); err != nil {
		writeError(w, http.StatusNotFound, err.Error())
		return
	}
	if !req.Enabled {
		if containsDomain(s.ssoDomains, name) {
			writeError(w, http.StatusConflict,
				"this domain is declared in the chart's SSO list (SSO_DOMAINS); remove it from values before demoting")
			return
		}
		if apps := s.appsUsingAuth(name); len(apps) > 0 {
			writeError(w, http.StatusConflict,
				fmt.Sprintf("cannot disable SSO: installed apps still require auth on %s: %s", name, strings.Join(apps, ", ")))
			return
		}
	}
	now := time.Now().UTC()
	domain, err := s.domains.Update(name, func(d *certs.Domain) {
		d.SSO = req.Enabled
		d.UpdatedAt = now
	})
	if err != nil {
		writeError(w, http.StatusNotFound, err.Error())
		return
	}
	if err := s.syncSSOState(r.Context()); err != nil {
		// The flag is persisted; the fragments will be reconciled on the next
		// change (or on startup), so this is not fatal.
		log.Printf("Warning: Authelia SSO sync failed: %v", err)
	}
	writeJSON(w, http.StatusOK, map[string]interface{}{
		"domain":     domain,
		"domains":    s.domains.List(),
		"ssoDomains": s.effectiveSSODomains(),
	})
}

// appsUsingAuth returns the names of installed apps whose exposure requires
// Authelia auth on the given base domain (an empty record domain means the
// primary, which routing falls back to).
func (s *Server) appsUsingAuth(baseDomain string) []string {
	if s.appManager == nil {
		return nil
	}
	var names []string
	for _, rec := range s.appManager.Records() {
		if !rec.Exposure.Auth {
			continue
		}
		domain := rec.BaseDomain
		if domain == "" {
			domain = s.baseDomain
		}
		if domain == baseDomain {
			names = append(names, rec.Name)
		}
	}
	sort.Strings(names)
	return names
}

// containsDomain reports whether list contains value.
func containsDomain(list []string, value string) bool {
	for _, item := range list {
		if item == value {
			return true
		}
	}
	return false
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
		// The SSO flag is owned by POST /api/domains/{domain}/sso: the Domains
		// form never sends it, so a plain edit must not silently demote the
		// domain.
		domain.SSO = existing.SSO
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
