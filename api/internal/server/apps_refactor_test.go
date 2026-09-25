package server

import (
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

// adminRequest sends a request that passes the owner gate as an administrator.
func adminRequest(t *testing.T, s *Server, method, path, body string) *httptest.ResponseRecorder {
	t.Helper()
	var reader *strings.Reader
	if body == "" {
		reader = strings.NewReader("")
	} else {
		reader = strings.NewReader(body)
	}
	req := httptest.NewRequest(method, path, reader)
	req.Header.Set("Remote-User", "admin")
	req.Header.Set("Remote-Groups", "naslos_admins")
	req.Header.Set("X-Naslos-Proxy-Secret", "test-proxy-secret")
	if body != "" {
		req.Header.Set("Content-Type", "application/json")
	}
	rec := httptest.NewRecorder()
	s.router.ServeHTTP(rec, req)
	return rec
}

// TestInstallRequiresConfirmation pins FR-APP-12: a third-party chart install is
// refused unless the request carries an explicit confirmation.
func TestInstallRequiresConfirmation(t *testing.T) {
	s := newTestServer(t)

	rec := adminRequest(t, s, http.MethodPost, "/api/apps", `{"name":"jellyfin","values":{}}`)
	if rec.Code != http.StatusBadRequest {
		t.Fatalf("unconfirmed install: status = %d, want 400 (body %s)", rec.Code, rec.Body.String())
	}
	if !strings.Contains(rec.Body.String(), "confirmed") {
		t.Fatalf("error did not mention confirmation: %s", rec.Body.String())
	}
}

// TestExposureUnknownAppIs404 covers the app exposure endpoint contract.
func TestExposureUnknownAppIs404(t *testing.T) {
	s := newTestServer(t)

	rec := adminRequest(t, s, http.MethodGet, "/api/apps/does-not-exist/exposure", "")
	if rec.Code != http.StatusNotFound {
		t.Fatalf("exposure of an unknown app: status = %d, want 404", rec.Code)
	}
}

// TestSourcesListIsEmptyOnAFreshInstall pins the shape of GET /api/sources.
func TestSourcesListIsEmptyOnAFreshInstall(t *testing.T) {
	s := newTestServer(t)

	rec := adminRequest(t, s, http.MethodGet, "/api/sources", "")
	if rec.Code != http.StatusOK {
		t.Fatalf("GET /api/sources: status = %d, want 200", rec.Code)
	}
	if strings.TrimSpace(rec.Body.String()) != "[]" {
		t.Fatalf("GET /api/sources = %s, want []", rec.Body.String())
	}
}

// TestDomainValidationRejectsBadNames covers the domain endpoint contract.
func TestDomainValidationRejectsBadNames(t *testing.T) {
	s := newTestServer(t)

	rec := adminRequest(t, s, http.MethodPut, "/api/domains/not_a_domain",
		`{"baseDomain":"not_a_domain","dnsProvider":"cloudflare","credentialsSecret":"x","environment":"staging"}`)
	if rec.Code != http.StatusBadRequest {
		t.Fatalf("invalid domain: status = %d, want 400 (body %s)", rec.Code, rec.Body.String())
	}
}

// TestAppDetailRejectsNestedPath guards the app-detail router against a path that
// would otherwise be treated as an app name.
func TestAppDetailRejectsNestedPath(t *testing.T) {
	s := newTestServer(t)

	rec := adminRequest(t, s, http.MethodGet, "/api/apps/a/b/c", "")
	if rec.Code != http.StatusNotFound {
		t.Fatalf("nested app path: status = %d, want 404", rec.Code)
	}
}
