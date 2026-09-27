package server

import (
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"
)

// newTestServer builds a Server the way main does, with the owner auth gate armed
// (there is no opt-out any more) and every state file pointed at a temp dir so the
// test never touches the host. The trusted CIDR covers httptest's synthetic client
// address (192.0.2.1), which is how TRAEFIK_CIDR covers the real proxy.
func newTestServer(t *testing.T) *Server {
	t.Helper()
	return newSeededTestServer(t, "", "")
}

// newSeededTestServer builds a Server with optional apps.json / domains.json
// content written before New, because the stores load in the constructor: a test
// that needs a pre-existing record or domain must seed the file, not call the
// manager afterwards.
func newSeededTestServer(t *testing.T, appsJSON, domainsJSON string) *Server {
	t.Helper()
	return newSeededTestServerSSO(t, appsJSON, domainsJSON, "naslos.local")
}

// newSeededTestServerSSO is newSeededTestServer with an explicit SSO_DOMAINS env
// value (the chart-declared SSO seed).
func newSeededTestServerSSO(t *testing.T, appsJSON, domainsJSON, ssoEnv string) *Server {
	t.Helper()

	dir := t.TempDir()
	appsPath := filepath.Join(dir, "apps.json")
	domainsPath := filepath.Join(dir, "domains.json")
	if appsJSON != "" {
		if err := os.WriteFile(appsPath, []byte(appsJSON), 0o600); err != nil {
			t.Fatalf("seeding apps.json: %v", err)
		}
	}
	if domainsJSON != "" {
		if err := os.WriteFile(domainsPath, []byte(domainsJSON), 0o600); err != nil {
			t.Fatalf("seeding domains.json: %v", err)
		}
	}

	t.Setenv("TRAEFIK_CIDR", "192.0.2.0/24")
	t.Setenv("PROXY_SHARED_SECRET", "test-proxy-secret")
	// Pin the primary domain and the chart SSO seed so assertions are stable
	// regardless of the host environment.
	t.Setenv("NASLOS_DOMAIN", "naslos.local")
	t.Setenv("SSO_DOMAINS", ssoEnv)
	t.Setenv("BUDDY_PEERS", filepath.Join(dir, "peers.json"))
	t.Setenv("BUDDY_RECEIVE_PATH", filepath.Join(dir, "buddy"))
	t.Setenv("SHARES_CONFIG", filepath.Join(dir, "shares.json"))
	t.Setenv("SMB_USERS_CONFIG", filepath.Join(dir, "smbusers.json"))
	t.Setenv("NOTIFICATIONS_CONFIG", filepath.Join(dir, "notifications.json"))
	t.Setenv("APPS_CONFIG", appsPath)
	t.Setenv("SOURCES_CONFIG", filepath.Join(dir, "sources.json"))
	t.Setenv("DOMAINS_CONFIG", domainsPath)
	t.Setenv("CHARTS_CACHE_DIR", filepath.Join(dir, "charts"))
	t.Setenv("DDNS_CONFIG", filepath.Join(dir, "ddns.json"))
	t.Setenv("DDNS_ENABLED", "true")
	t.Setenv("DDNS_PROVIDERS_DIR", filepath.Join(dir, "ddns-providers"))

	return New("127.0.0.1:0", nil)
}

// ownerPaths is every owner-facing route the API registers. Adding a route to
// routes() without adding it here is exactly the mistake NAS-001 was, so the
// table is deliberately exhaustive.
var ownerPaths = []string{
	"/api/catalog", "/api/catalog/nginx",
	"/api/apps", "/api/apps/nginx", "/api/apps/nginx/exposure", "/api/apps/nginx/services",
	"/api/apps/jobs", "/api/apps/jobs/abc123",
	"/api/sources", "/api/sources/refresh", "/api/sources/mine",
	"/api/domains", "/api/domains/example.com", "/api/domains/example.com/certificate",
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
// header alone (a forged identity header) must not be enough.
func TestOwnerRoutesRequireAuth(t *testing.T) {
	s := newTestServer(t)

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
	s := newTestServer(t)

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
	s := newTestServer(t)

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

// TestNonAdminMayOnlyReachTheirIdentityAndDashboard pins CR-03 at the router
// level: an authenticated user without `naslos_admins` keeps the three routes the
// UI needs to render, and every administrator route answers 403 rather than
// letting any signed-in account manage users, wipe disks or open the terminal.
func TestNonAdminMayOnlyReachTheirIdentityAndDashboard(t *testing.T) {
	s := newTestServer(t)

	call := func(path, groups string) int {
		req := httptest.NewRequest(http.MethodGet, path, nil)
		req.Header.Set("Remote-User", "someone")
		req.Header.Set("X-Naslos-Proxy-Secret", "test-proxy-secret")
		if groups != "" {
			req.Header.Set("Remote-Groups", groups)
		}
		rec := httptest.NewRecorder()
		s.router.ServeHTTP(rec, req)
		return rec.Code
	}

	// `/api/auth/me` is registered in the same allow-list block, but calling it
	// here would drag in LDAP (the handler checks the identity backend), which is
	// slow and irrelevant to the gate.
	for _, path := range []string{"/api/dashboard", "/api/metrics"} {
		if code := call(path, "naslos_users"); code != http.StatusOK {
			t.Errorf("%s as a plain user: status = %d, want 200", path, code)
		}
	}

	adminOnly := []string{
		"/api/users", "/api/users/admin", "/api/groups", "/api/groups/admins",
		"/api/volumes", "/api/volumes/zfs", "/api/volumes/zfs/import", "/api/datasets",
		"/api/disks", "/api/disks/recommend",
		"/api/shares", "/api/shares/paths", "/api/shares/folders", "/api/shares/status",
		"/api/shares/apply", "/api/shares/config/samba", "/api/shares/config/nfs",
		"/api/notifications", "/api/notifications/test",
		"/api/apps", "/api/apps/nginx", "/api/catalog", "/api/catalog/nginx",
		"/api/apps/jobs", "/api/apps/jobs/abc123",
		"/api/sources", "/api/sources/refresh", "/api/sources/mine",
		"/api/domains", "/api/domains/example.com", "/api/domains/example.com/certificate",
		"/api/domains/example.com/sso",
		"/api/providers", "/api/ddns", "/api/ddns/x", "/api/ddns/x/run",
		"/api/apps/nginx/exposure",
		"/api/apps/nginx/services",
		"/api/pods", "/api/namespaces", "/api/ws/logs", "/api/ws/exec",
		"/api/buddy/status", "/api/buddy/peers", "/api/buddy/identity", "/api/buddy/send",
		"/api/buddy/restore", "/api/buddy/jobs", "/api/buddy/jobs/abc123", "/api/buddy/schedules",
	}
	for _, path := range adminOnly {
		if code := call(path, "naslos_users"); code != http.StatusForbidden {
			t.Errorf("%s as a plain user: status = %d, want 403", path, code)
		}
	}

	// An administrator passes the gate. Only the gate is asserted: the handler's
	// own status depends on its dependencies (no LDAP or cluster in this test),
	// and the routes chosen here are the cheap ones for that reason.
	for _, path := range []string{"/api/catalog", "/api/buddy/peers", "/api/pods"} {
		if code := call(path, "naslos_admins"); code == http.StatusForbidden || code == http.StatusUnauthorized {
			t.Errorf("%s as an admin: status = %d, want the request to reach the handler", path, code)
		}
	}
}
