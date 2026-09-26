package server

import (
	"context"
	"encoding/json"
	"fmt"
	"log"
	"net/http"
	"regexp"
	"strings"
	"time"

	"github.com/AessemOps/Naslos-Linux/api/internal/ddns"
	"github.com/AessemOps/Naslos-Linux/api/internal/providers"
)

// dnsLabel matches one DNS-1123 label (a zone or subdomain component).
var dnsLabel = regexp.MustCompile(`^[a-z0-9]([-a-z0-9]*[a-z0-9])?$`)

// dnsSubdomain matches a DNS name / subdomain.
var dnsSubdomain = regexp.MustCompile(`^[a-z0-9]([-a-z0-9]*[a-z0-9])?(\.[a-z0-9]([-a-z0-9]*[a-z0-9])?)*$`)

// providerView is one provider as returned by GET /api/providers. It contains no
// credential values.
type providerView struct {
	Name        string            `json:"name"`
	DisplayName string            `json:"displayName"`
	Description string            `json:"description,omitempty"`
	Icon        string            `json:"icon,omitempty"`
	Fields      []providers.Field `json:"fields"`
	CertManager bool              `json:"certManager"`
	DDNS        bool              `json:"ddns"`
	Driver      string            `json:"driver,omitempty"`
}

// handleProviders lists the declarative DNS providers (FR-DNS-01). Load errors
// for override files are reported, not fatal.
func (s *Server) handleProviders(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}
	views := []providerView{}
	loadErrors := []string{}
	if s.providers != nil {
		for _, p := range s.providers.List() {
			view := providerView{
				Name:        p.Name,
				DisplayName: p.DisplayName,
				Description: p.Description,
				Icon:        p.Icon,
				Fields:      p.Fields,
				CertManager: p.SupportsCertificates(),
				DDNS:        p.HasDDNS(),
			}
			if p.HasDDNS() {
				view.Driver = p.DDNS.Driver
			}
			if view.Fields == nil {
				view.Fields = []providers.Field{}
			}
			views = append(views, view)
		}
		loadErrors = append(loadErrors, s.providers.Errors()...)
	}
	writeJSON(w, http.StatusOK, map[string]interface{}{
		"providers": views,
		"errors":    loadErrors,
	})
}

// ddnsRequest is the create/update body. `fields` carries every credential field
// the provider declares; the server splits it into a Secret and providerConfig.
type ddnsRequest struct {
	Provider   string            `json:"provider"`
	Zone       string            `json:"zone"`
	Record     string            `json:"record"`
	RecordType string            `json:"recordType"`
	TTL        int               `json:"ttl"`
	Enabled    *bool             `json:"enabled"`
	Fields     map[string]string `json:"fields"`
}

// handleDdns lists and creates dynamic-DNS entries (FR-DNS-02).
func (s *Server) handleDdns(w http.ResponseWriter, r *http.Request) {
	switch r.Method {
	case http.MethodGet:
		if s.ddns == nil {
			writeJSON(w, http.StatusOK, map[string]interface{}{"entries": []ddns.Entry{}, "enabled": false})
			return
		}
		writeJSON(w, http.StatusOK, map[string]interface{}{
			"entries":         s.ddns.Store().List(),
			"enabled":         true,
			"intervalSeconds": int(s.ddns.Interval().Seconds()),
		})

	case http.MethodPost:
		var req ddnsRequest
		if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 64<<10)).Decode(&req); err != nil {
			writeError(w, http.StatusBadRequest, "invalid request: "+err.Error())
			return
		}
		s.upsertDdns(w, r, nil, req)

	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// handleDdnsDetail reads, updates, deletes an entry or forces a run.
func (s *Server) handleDdnsDetail(w http.ResponseWriter, r *http.Request) {
	path := strings.TrimPrefix(r.URL.Path, "/api/ddns/")
	if strings.HasSuffix(path, "/run") {
		s.handleDdnsRun(w, r, strings.TrimSuffix(path, "/run"))
		return
	}
	id := path
	if id == "" || strings.Contains(id, "/") {
		writeError(w, http.StatusNotFound, "ddns entry not found")
		return
	}
	if s.ddns == nil {
		writeError(w, http.StatusNotFound, "ddns entry not found")
		return
	}
	switch r.Method {
	case http.MethodGet:
		entry, err := s.ddns.Store().Get(id)
		if err != nil {
			writeError(w, http.StatusNotFound, err.Error())
			return
		}
		writeJSON(w, http.StatusOK, entry)

	case http.MethodPut:
		existing, err := s.ddns.Store().Get(id)
		if err != nil {
			writeError(w, http.StatusNotFound, err.Error())
			return
		}
		var req ddnsRequest
		if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 64<<10)).Decode(&req); err != nil {
			writeError(w, http.StatusBadRequest, "invalid request: "+err.Error())
			return
		}
		s.upsertDdns(w, r, &existing, req)

	case http.MethodDelete:
		entry, err := s.ddns.Store().Get(id)
		if err != nil {
			writeError(w, http.StatusNotFound, err.Error())
			return
		}
		if err := s.ddns.Store().Delete(id); err != nil {
			writeError(w, http.StatusNotFound, err.Error())
			return
		}
		if client, err := s.kubernetesClient(); err == nil && entry.CredentialsSecret != "" {
			if err := ddns.DeleteSecret(r.Context(), client, s.appsNamespace, entry.CredentialsSecret); err != nil {
				log.Printf("Warning: could not delete DDNS Secret %s: %v", entry.CredentialsSecret, err)
			}
		}
		writeJSON(w, http.StatusOK, map[string]string{"status": "ddns entry removed", "id": id})

	default:
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
	}
}

// handleDdnsRun forces one reconcile of an entry (FR-DNS-06).
func (s *Server) handleDdnsRun(w http.ResponseWriter, r *http.Request, id string) {
	if r.Method != http.MethodPost {
		writeError(w, http.StatusMethodNotAllowed, "method not allowed")
		return
	}
	if s.ddns == nil {
		writeError(w, http.StatusServiceUnavailable, "dynamic DNS is not enabled")
		return
	}
	if id == "" || strings.Contains(id, "/") {
		writeError(w, http.StatusNotFound, "ddns entry not found")
		return
	}
	if _, err := s.ddns.Store().Get(id); err != nil {
		writeError(w, http.StatusNotFound, err.Error())
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), 30*time.Second)
	defer cancel()
	if err := s.ddns.Run(ctx, id, true); err != nil {
		entry, _ := s.ddns.Store().Get(id)
		writeJSON(w, http.StatusBadGateway, map[string]interface{}{"error": err.Error(), "entry": entry})
		return
	}
	entry, _ := s.ddns.Store().Get(id)
	writeJSON(w, http.StatusOK, entry)
}

// upsertDdns validates, splits credentials, persists and returns an entry.
func (s *Server) upsertDdns(w http.ResponseWriter, r *http.Request, existing *ddns.Entry, req ddnsRequest) {
	if s.ddns == nil {
		writeError(w, http.StatusServiceUnavailable, "dynamic DNS is not enabled")
		return
	}
	if s.providers == nil {
		writeError(w, http.StatusServiceUnavailable, "no DNS provider registry is loaded")
		return
	}

	providerName := strings.TrimSpace(req.Provider)
	if providerName == "" && existing != nil {
		providerName = existing.Provider
	}
	p, ok := s.providers.Get(providerName)
	if !ok {
		writeError(w, http.StatusBadRequest, fmt.Sprintf("unknown provider %q", providerName))
		return
	}
	if !p.HasDDNS() {
		writeError(w, http.StatusBadRequest, fmt.Sprintf("provider %q does not support dynamic DNS", providerName))
		return
	}

	zone := strings.TrimSpace(req.Zone)
	if zone == "" && existing != nil {
		zone = existing.Zone
	}
	if !dnsSubdomain.MatchString(zone) {
		writeError(w, http.StatusBadRequest, fmt.Sprintf("zone %q is not a valid DNS name", zone))
		return
	}
	record := strings.TrimSpace(req.Record)
	if record == "" && existing != nil {
		record = existing.Record
	}
	if err := validateRecordLabel(record); err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}
	recordType := strings.ToUpper(strings.TrimSpace(req.RecordType))
	if recordType == "" && existing != nil {
		recordType = existing.RecordType
	}
	if recordType != "A" && recordType != "AAAA" {
		writeError(w, http.StatusBadRequest, "recordType must be A or AAAA")
		return
	}
	ttl := req.TTL
	if ttl == 0 && existing != nil {
		ttl = existing.TTL
	}
	if ttl != 0 && (ttl < 60 || ttl > 86400) {
		writeError(w, http.StatusBadRequest, "ttl must be 0 (provider default) or between 60 and 86400")
		return
	}

	// Resolve non-secret config: submitted value, else existing, else default.
	config := map[string]string{}
	for _, f := range p.ConfigFields() {
		value, submitted := req.Fields[f.Key]
		switch {
		case submitted:
			config[f.Key] = value
		case existing != nil && existing.ProviderConfig[f.Key] != "":
			config[f.Key] = existing.ProviderConfig[f.Key]
		case f.Default != "":
			config[f.Key] = f.Default
		}
		if f.Required && strings.TrimSpace(config[f.Key]) == "" {
			writeError(w, http.StatusBadRequest, fmt.Sprintf("field %q is required", f.Key))
			return
		}
		if err := validateFieldValue(f, config[f.Key]); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
	}

	// Resolve secret fields. A non-empty submitted value wins; otherwise the
	// existing Secret is kept (only rewritten when something new is submitted).
	updates := map[string]string{}
	credentialFields := []string{}
	if existing != nil {
		credentialFields = append(credentialFields, existing.CredentialFields...)
	}
	for _, f := range p.SecretFields() {
		if value, ok := req.Fields[f.Key]; ok && strings.TrimSpace(value) != "" {
			updates[f.SecretKeyOr()] = value
			credentialFields = addField(credentialFields, f.Key)
			continue
		}
		if existing == nil && f.Required {
			writeError(w, http.StatusBadRequest, fmt.Sprintf("field %q is required", f.Key))
			return
		}
		if err := validateFieldValue(f, req.Fields[f.Key]); err != nil {
			writeError(w, http.StatusBadRequest, err.Error())
			return
		}
	}

	now := time.Now().UTC()
	entry := ddns.Entry{
		ID:         ddns.NewID(),
		Provider:   providerName,
		Zone:       zone,
		Record:     record,
		RecordType: recordType,
		TTL:        ttl,
		Enabled:    true,
		CreatedAt:  now,
		UpdatedAt:  now,
	}
	if req.Enabled != nil {
		entry.Enabled = *req.Enabled
	}
	if existing != nil {
		entry = *existing
		entry.Provider = providerName
		entry.Zone = zone
		entry.Record = record
		entry.RecordType = recordType
		entry.TTL = ttl
		entry.UpdatedAt = now
		if req.Enabled != nil {
			entry.Enabled = *req.Enabled
		}
		// The configuration changed, so the old "converged" status no longer
		// applies: clear it so the next reconcile re-applies even when the
		// public IP is unchanged.
		entry.LastStatus = ""
		entry.LastError = ""
	}
	entry.ProviderConfig = config
	entry.CredentialFields = credentialFields

	if len(updates) > 0 {
		secretName := entry.CredentialsSecret
		if secretName == "" {
			secretName = entry.SecretName()
		}
		client, err := s.kubernetesClient()
		if err != nil {
			writeError(w, http.StatusServiceUnavailable, "cannot reach the Kubernetes API to store credentials: "+err.Error())
			return
		}
		ctx, cancel := context.WithTimeout(r.Context(), 15*time.Second)
		defer cancel()
		if err := ddns.WriteSecret(ctx, client, s.appsNamespace, secretName, updates); err != nil {
			writeError(w, http.StatusBadGateway, "storing credentials: "+err.Error())
			return
		}
		entry.CredentialsSecret = secretName
	}
	if len(p.SecretFields()) > 0 && entry.CredentialsSecret == "" {
		for _, f := range p.SecretFields() {
			if f.Required {
				writeError(w, http.StatusBadRequest, fmt.Sprintf("provider %q requires credentials", p.Name))
				return
			}
		}
	}

	if err := s.ddns.Store().Put(entry); err != nil {
		writeError(w, http.StatusInternalServerError, err.Error())
		return
	}
	status := http.StatusCreated
	if existing != nil {
		status = http.StatusOK
	}
	writeJSON(w, status, entry)
}

// validateRecordLabel checks a subdomain label ("@"/empty = apex).
func validateRecordLabel(record string) error {
	if record == "" || record == "@" {
		return nil
	}
	stripped := strings.TrimPrefix(record, "*.")
	if !dnsLabel.MatchString(stripped) {
		return fmt.Errorf("record %q is not a valid DNS label", record)
	}
	return nil
}

func validateFieldValue(f providers.Field, value string) error {
	if value == "" {
		return nil
	}
	switch f.Type {
	case providers.FieldBool:
		if value != "true" && value != "false" {
			return fmt.Errorf("field %q must be true or false", f.Key)
		}
	case providers.FieldEnum:
		for _, allowed := range f.Enum {
			if value == allowed {
				return nil
			}
		}
		return fmt.Errorf("field %q must be one of %s", f.Key, strings.Join(f.Enum, ", "))
	}
	return nil
}

func addField(list []string, key string) []string {
	for _, item := range list {
		if item == key {
			return list
		}
	}
	return append(list, key)
}
