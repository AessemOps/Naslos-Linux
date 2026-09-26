package server

import (
	"encoding/json"
	"net/http"
	"testing"
)

// seededApps is one record on a secondary base domain.
const seededApps = `[{"name":"jellyfin","exposure":{"subdomain":"jellyfin","tls":true,"auth":false},"baseDomain":"media.example.com","createdAt":"2026-01-01T00:00:00Z","updatedAt":"2026-01-01T00:00:00Z"}]`

// seededDomains is one registered secondary domain, not SSO.
const seededDomains = `[{"baseDomain":"media.example.com","dnsProvider":"cloudflare","credentialsSecret":"x","environment":"staging"}]`

// TestDomainsExposeSelectableAndSSO pins the added GET /api/domains fields: the
// primary domain first, then registered domains, and the effective SSO list.
func TestDomainsExposeSelectableAndSSO(t *testing.T) {
	s := newSeededTestServer(t, "", seededDomains)

	rec := adminRequest(t, s, http.MethodGet, "/api/domains", "")
	if rec.Code != http.StatusOK {
		t.Fatalf("GET /api/domains: status = %d, want 200 (body %s)", rec.Code, rec.Body.String())
	}
	var body struct {
		BaseDomain        string   `json:"baseDomain"`
		SelectableDomains []string `json:"selectableDomains"`
		SSODomains        []string `json:"ssoDomains"`
	}
	if err := json.Unmarshal(rec.Body.Bytes(), &body); err != nil {
		t.Fatalf("decode: %v (%s)", err, rec.Body.String())
	}
	if body.BaseDomain != "naslos.local" {
		t.Errorf("baseDomain = %q, want naslos.local", body.BaseDomain)
	}
	if !containsString(body.SelectableDomains, "naslos.local") || !containsString(body.SelectableDomains, "media.example.com") {
		t.Errorf("selectableDomains = %v, want the primary and the registered domain", body.SelectableDomains)
	}
	if !containsString(body.SSODomains, "naslos.local") {
		t.Errorf("ssoDomains = %v, want the primary", body.SSODomains)
	}
	if containsString(body.SSODomains, "media.example.com") {
		t.Errorf("ssoDomains = %v, must not contain an unpromoted domain", body.SSODomains)
	}
}

// TestExposureGetReturnsRecordBaseDomain is the regression test for the revert
// bug: the endpoint used to hard-code the primary domain, so the UI silently
// rewrote a chosen secondary domain back to it on save.
func TestExposureGetReturnsRecordBaseDomain(t *testing.T) {
	s := newSeededTestServer(t, seededApps, seededDomains)

	rec := adminRequest(t, s, http.MethodGet, "/api/apps/jellyfin/exposure", "")
	if rec.Code != http.StatusOK {
		t.Fatalf("GET exposure: status = %d, want 200 (body %s)", rec.Code, rec.Body.String())
	}
	var body struct {
		BaseDomain        string   `json:"baseDomain"`
		PrimaryDomain     string   `json:"primaryDomain"`
		SelectableDomains []string `json:"selectableDomains"`
		SSODomains        []string `json:"ssoDomains"`
		AuthAllowed       bool     `json:"authAllowed"`
	}
	if err := json.Unmarshal(rec.Body.Bytes(), &body); err != nil {
		t.Fatalf("decode: %v (%s)", err, rec.Body.String())
	}
	if body.BaseDomain != "media.example.com" {
		t.Errorf("baseDomain = %q, want the record's own domain", body.BaseDomain)
	}
	if body.PrimaryDomain != "naslos.local" {
		t.Errorf("primaryDomain = %q, want naslos.local", body.PrimaryDomain)
	}
	if body.AuthAllowed {
		t.Errorf("authAllowed = true for a non-SSO domain, want false")
	}
	if !containsString(body.SelectableDomains, "media.example.com") {
		t.Errorf("selectableDomains = %v, want the record's domain", body.SelectableDomains)
	}
}

// TestExposurePutRejectsUnknownBaseDomain pins the 400 guard: an exposure may
// only point at a configured domain.
func TestExposurePutRejectsUnknownBaseDomain(t *testing.T) {
	s := newSeededTestServer(t, seededApps, seededDomains)

	rec := adminRequest(t, s, http.MethodPut, "/api/apps/jellyfin/exposure",
		`{"exposure":{"subdomain":"jellyfin","tls":true},"baseDomain":"evil.example.net"}`)
	if rec.Code != http.StatusBadRequest {
		t.Fatalf("PUT unknown base domain: status = %d, want 400 (body %s)", rec.Code, rec.Body.String())
	}
}

// TestInstallRejectsUnknownBaseDomain pins the same guard on install.
func TestInstallRejectsUnknownBaseDomain(t *testing.T) {
	s := newSeededTestServer(t, "", seededDomains)

	rec := adminRequest(t, s, http.MethodPost, "/api/apps",
		`{"name":"jellyfin","values":{},"baseDomain":"evil.example.net","confirmed":true}`)
	if rec.Code != http.StatusBadRequest {
		t.Fatalf("POST unknown base domain: status = %d, want 400 (body %s)", rec.Code, rec.Body.String())
	}
}

func containsString(list []string, want string) bool {
	for _, item := range list {
		if item == want {
			return true
		}
	}
	return false
}
