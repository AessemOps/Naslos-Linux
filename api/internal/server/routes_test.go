package server

import (
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"testing"
)

// newTestServer builds a Server the way main does, with the owner auth gate armed
// and every state file pointed at a temp dir so the test never touches the host.
// The trusted CIDR covers httptest's synthetic client address (192.0.2.1), which
// is how TRAEFIK_CIDR covers the real proxy.
func newTestServer(t *testing.T, authDisabled bool) *Server {
	t.Helper()

	t.Setenv("TRAEFIK_CIDR", "192.0.2.0/24")
	t.Setenv("BUDDY_PEERS", filepath.Join(t.TempDir(), "peers.json"))
	t.Setenv("BUDDY_RECEIVE_PATH", filepath.Join(t.TempDir(), "buddy"))
	t.Setenv("SHARES_CONFIG", filepath.Join(t.TempDir(), "shares.json"))
	t.Setenv("SMB_USERS_CONFIG", filepath.Join(t.TempDir(), "smbusers.json"))

	if authDisabled {
		t.Setenv("AUTH_DISABLED", "true")
		t.Setenv("PROXY_SHARED_SECRET", "")
	} else {
		t.Setenv("AUTH_DISABLED", "false")
		t.Setenv("PROXY_SHARED_SECRET", "test-proxy-secret")
	}

	return New("127.0.0.1:0", nil)
}

// ownerPaths is every owner-facing route the API registers. Adding a route to
// routes() without adding it here is exactly the mistake NAS-001 was, so the
// table is deliberately exhaustive.
var ownerPaths = []string{
	"/api/catalog", "/api/catalog/nginx",
	"/api/apps", "/api/apps/nginx",
	"/api/disks", "/api/disks/recommend",
	"/api/volumes", "/api/volumes/zfs", "/api/volumes/zfs/import", "/api/volumes/zfs/test",
	"/api/datasets",
	"/api/ws/logs", "/api/pods", "/api/namespaces", "/api/ws/exec",
	"/api/shares", "/api/shares/paths", "/api/shares/folders", "/api/shares/status",
	"/api/shares/apply", "/api/shares/config/samba", "/api/shares/config/nfs", "/api/shares/media",
	"/api/notifications", "/api/notifications/test",
	"/api/buddy/status", "/api/buddy/peers", "/api/buddy/identity", "/api/buddy/send",
	"/api/buddy/restore", "/api/buddy/jobs", "/api/buddy/jobs/abc123", "/api/buddy/schedules",
	"/api/metrics", "/api/dashboard",
	"/api/users", "/api/users/admin", "/api/groups", "/api/groups/admins", "/api/auth/me",
}

// TestOwnerRoutesRequireAuth is the regression test for NAS-001/NAS-004/NAS-005:
// no owner route may be reachable without the proxy secret, and the identity
// header alone (the NodePort forgery) must not be enough.
func TestOwnerRoutesRequireAuth(t *testing.T) {
	s := newTestServer(t, false)

	for _, path := range ownerPaths {
		for _, headers := range []map[string]string{
			nil,
			{"Remote-User": "admin"},
			{"Remote-User": "admin", "X-Naslos-Proxy-Secret": "wrong"},
		} {
			req := httptest.NewRequest(http.MethodGet, path, nil)
			for k, v := range headers {
				req.Header.Set(k, v)
			}
			rec := httptest.NewRecorder()
			s.router.ServeHTTP(rec, req)
			if rec.Code != http.StatusUnauthorized {
				t.Errorf("GET %s with %v: status = %d, want 401", path, headers, rec.Code)
			}
		}
	}

	// Mutating methods are gated the same way (the check must not depend on the
	// handler's own method handling).
	for _, path := range []string{"/api/volumes/zfs", "/api/users", "/api/buddy/send", "/api/shares"} {
		req := httptest.NewRequest(http.MethodPost, path, nil)
		req.Header.Set("Remote-User", "admin")
		rec := httptest.NewRecorder()
		s.router.ServeHTTP(rec, req)
		if rec.Code != http.StatusUnauthorized {
			t.Errorf("POST %s with a forged header: status = %d, want 401", path, rec.Code)
		}
	}
}

// TestOwnerRoutesPassWithTheProxySecret proves the gate is passable: with the
// secret and an identity the request reaches the handler (a non-401 then depends
// on the handler and its dependencies, which is not what this test is about).
func TestOwnerRoutesPassWithTheProxySecret(t *testing.T) {
	s := newTestServer(t, false)

	req := httptest.NewRequest(http.MethodGet, "/api/pods", nil)
	req.Header.Set("Remote-User", "admin")
	req.Header.Set("X-Naslos-Proxy-Secret", "test-proxy-secret")
	rec := httptest.NewRecorder()
	s.router.ServeHTTP(rec, req)

	if rec.Code == http.StatusUnauthorized {
		t.Errorf("GET /api/pods with the proxy secret: status = 401, want it to reach the handler")
	}
}

// TestPublicRoutesStayPublic pins the allow-list: probes cannot require a login,
// the buddy peer API is authenticated by the peers' own keys (a peer cannot
// complete an interactive login), and the static UI is served as-is.
func TestPublicRoutesStayPublic(t *testing.T) {
	s := newTestServer(t, false)

	for _, path := range []string{"/api/health", "/api/ready"} {
		req := httptest.NewRequest(http.MethodGet, path, nil)
		rec := httptest.NewRecorder()
		s.router.ServeHTTP(rec, req)
		if rec.Code != http.StatusOK {
			t.Errorf("GET %s: status = %d, want 200", path, rec.Code)
		}
	}

	// The peer API answers JSON (its own Ed25519 auth), not the owner
	// middleware's plain-text 401 - that is what proves it is not owner-gated.
	req := httptest.NewRequest(http.MethodGet, "/api/buddy/v1/status", nil)
	rec := httptest.NewRecorder()
	s.router.ServeHTTP(rec, req)
	if ct := rec.Header().Get("Content-Type"); ct != "application/json" {
		t.Errorf("GET /api/buddy/v1/status: content-type = %q (status %d), want the peer handler's JSON",
			ct, rec.Code)
	}
}

// TestAuthDisabledPassthrough covers the explicit development opt-out.
func TestAuthDisabledPassthrough(t *testing.T) {
	s := newTestServer(t, true)

	req := httptest.NewRequest(http.MethodGet, "/api/health", nil)
	rec := httptest.NewRecorder()
	s.router.ServeHTTP(rec, req)
	if rec.Code != http.StatusOK {
		t.Errorf("AUTH_DISABLED health: status = %d, want 200", rec.Code)
	}

	// No headers, no secret: the owner route is served anyway (and the handler
	// answers for itself - what matters is that it is not the auth gate).
	req = httptest.NewRequest(http.MethodGet, "/api/pods", nil)
	rec = httptest.NewRecorder()
	s.router.ServeHTTP(rec, req)
	if rec.Code == http.StatusUnauthorized {
		t.Errorf("AUTH_DISABLED owner route: status = 401, want the auth gate bypassed")
	}
}
