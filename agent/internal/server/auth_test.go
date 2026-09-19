package server

import (
	"net/http"
	"net/http/httptest"
	"testing"
)

// TestRequireAuth pins the gate in front of a privileged, host-networked agent
// (NAS-002). The agent destroys pools and receives datasets, so only a caller
// holding the shared token may reach anything but /health.
func TestRequireAuth(t *testing.T) {
	gated := &Server{router: http.NewServeMux(), authToken: "agent-secret"}
	gated.routes()

	// /health stays public: the DaemonSet's liveness/readiness probes cannot
	// carry a token.
	rec := httptest.NewRecorder()
	gated.router.ServeHTTP(rec, httptest.NewRequest(http.MethodGet, "/health", nil))
	if rec.Code != http.StatusOK {
		t.Errorf("GET /health: status = %d, want 200", rec.Code)
	}

	for _, tc := range []struct{ name, header string }{
		{"no header", ""},
		{"wrong token", "Bearer not-the-token"},
		{"raw token without the scheme", "agent-secret"},
		{"empty bearer", "Bearer "},
		{"prefix of the token", "Bearer agent"},
	} {
		req := httptest.NewRequest(http.MethodGet, "/api/v1/pools", nil)
		if tc.header != "" {
			req.Header.Set("Authorization", tc.header)
		}
		rec := httptest.NewRecorder()
		gated.router.ServeHTTP(rec, req)
		if rec.Code != http.StatusUnauthorized {
			t.Errorf("%s: status = %d, want 401", tc.name, rec.Code)
		}
	}

	// The real token reaches the handler; a degraded agent then answers 503 for
	// a ZFS route, which is what proves the gate was passed.
	req := httptest.NewRequest(http.MethodGet, "/api/v1/pools", nil)
	req.Header.Set("Authorization", "Bearer agent-secret")
	rec = httptest.NewRecorder()
	gated.router.ServeHTTP(rec, req)
	if rec.Code == http.StatusUnauthorized {
		t.Error("with the correct token: status = 401, want the request to reach the handler")
	}

	// No configured token and no opt-out: fail closed rather than serve anyone.
	closed := &Server{router: http.NewServeMux()}
	closed.routes()
	req = httptest.NewRequest(http.MethodGet, "/api/v1/pools", nil)
	req.Header.Set("Authorization", "Bearer ")
	rec = httptest.NewRecorder()
	closed.router.ServeHTTP(rec, req)
	if rec.Code != http.StatusUnauthorized {
		t.Errorf("unconfigured token: status = %d, want 401", rec.Code)
	}
}
