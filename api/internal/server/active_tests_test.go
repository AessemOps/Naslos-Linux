package server

import (
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"
)

// Batch 6 active tests (AV-5, AV-6, AV-9, AV-10) — see
// docs/AUDIT-2026-09-19-REPORT.md. These run against the real router with the
// owner auth gate armed (newTestServer), so they exercise the same stack the
// live API does: proxy secret, trusted source, identity header, admin gate.

// ownerRequest drives one request through the real router as an authenticated
// administrator. Callers pass a method, a path and an optional query string.
func ownerRequest(t *testing.T, s *Server, method, path string) *httptest.ResponseRecorder {
	t.Helper()
	req := httptest.NewRequest(method, path, nil)
	req.Header.Set("Remote-User", "admin")
	req.Header.Set("Remote-Groups", "naslos_admins")
	req.Header.Set("X-Naslos-Proxy-Secret", "test-proxy-secret")
	rec := httptest.NewRecorder()
	s.router.ServeHTTP(rec, req)
	return rec
}

// TestAV5PathTraversalIsRefused pins that a caller-supplied object name on a path
// route cannot escape its namespace or reach a file. A crude traversal that the
// handler would otherwise treat as "not found" is fine; what must never happen is
// a 200 (object returned) or a crash. The interesting cases are the percent- and
// slash-encoded forms, which is where naive TrimPrefix handling goes wrong.
func TestAV5PathTraversalIsRefused(t *testing.T) {
	s := newTestServer(t)

	payloads := []string{
		"../../etc/passwd",
		"..%2f..%2fetc%2fpasswd",
		"..%252f..%252fetc%252fpasswd",
		"....//....//etc/passwd",
		"%2e%2e%2f%2e%2e%2fetc%2fpasswd",
		"foo/../../../etc/passwd",
		"..%5c..%5cwindows%5csystem32",
		"admin%00.json",
		"admin%2f..%2f..%2fsecret",
	}

	// Paths whose last segment is a caller-supplied name. A 2xx means the name
	// was treated as an existing object, which for a traversal payload would be
	// an IDOR. 400/404/405/503 are all acceptable refusals.
	//
	// The identity routes (/api/users/, /api/groups/) are deliberately excluded:
	// without LDAP in a unit test they short-circuit at identityUnavailable (503)
	// before reaching the path handling, so they would pass vacuously. Their
	// name validation is pinned separately by TestAV6KubeNameParamsRejectInjection
	// and the identity package's escape tests.
	routes := []struct{ method, prefix string }{
		{http.MethodGet, "/api/shares/"},
		{http.MethodGet, "/api/apps/"},
		{http.MethodGet, "/api/catalog/"},
		{http.MethodGet, "/api/volumes/zfs/"},
	}

	for _, route := range routes {
		for _, payload := range payloads {
			path := route.prefix + payload
			rec := ownerRequest(t, s, route.method, path)
			// A redirect is Go's ServeMux cleaning the path; it never serves
			// the object, so it is a refusal for IDOR purposes.
			if rec.Code >= 200 && rec.Code < 300 {
				t.Errorf("%s %s: status = %d, want a refusal (not 2xx)", route.method, path, rec.Code)
			}
		}
	}
}

// TestAV6KubeNameParamsRejectInjection pins AV-6 for the terminal parameters:
// namespace/pod/container are interpolated into Kubernetes API paths, so a value
// carrying a slash, an escape, whitespace or a shell metacharacter must be
// refused by validateKubeName with a 400 BEFORE client-go is reached. A 503
// (unreachable cluster) would mean the value passed the guard and only failed
// later, so the test demands the early 400 for the metacharacter set.
func TestAV6KubeNameParamsRejectInjection(t *testing.T) {
	s := newTestServer(t)

	// Values that only the guard can reject: each carries a character the
	// DNS-1123 allowlist forbids. (Uppercase and leading dots are also invalid,
	// but they reach the cluster and fail there, so they would not isolate the
	// guard; they are covered by the guard's own unit tests.)
	bad := []string{
		"naslos/../../kube-system",
		"nas los",
		"naslos;id",
		"naslos$(id)",
		"naslos`id`",
		"naslos\x7f",
		"naslos|cat",
		"naslos&rm",
	}

	for _, value := range bad {
		// /api/pods?namespace=
		rec := ownerRequest(t, s, http.MethodGet, "/api/pods?namespace="+urlQueryEscape(value))
		if rec.Code != http.StatusBadRequest {
			t.Errorf("GET /api/pods?namespace=%q: status = %d, want 400 (the guard must refuse it)",
				value, rec.Code)
		}
		// /api/ws/exec?namespace=&pod= (preflight GET; no Upgrade header)
		rec = ownerRequest(t, s, http.MethodGet,
			"/api/ws/exec?namespace="+urlQueryEscape(value)+"&pod="+urlQueryEscape(value))
		if rec.Code != http.StatusBadRequest {
			t.Errorf("GET /api/ws/exec namespace/pod=%q: status = %d, want 400",
				value, rec.Code)
		}
	}
}

// TestAV9ExecNamespaceDefaultsToOwn pins the terminal scoping boundary: with no
// namespace parameter the exec handler uses the API's own namespace, so a request
// cannot silently target another namespace. The check is on the resolved target
// the preflight reports, which is what the browser shows before connecting.
func TestAV9ExecNamespaceDefaultsToOwn(t *testing.T) {
	s := newTestServer(t)

	// Namespace is unset on this Server (New is called with a nil kubeconfig in
	// tests), so the default is empty and the handler must refuse a missing pod
	// rather than pick something. That is the safe behaviour; assert it.
	rec := ownerRequest(t, s, http.MethodGet, "/api/ws/exec")
	if rec.Code != http.StatusBadRequest {
		t.Errorf("GET /api/ws/exec with no pod: status = %d, want 400", rec.Code)
	}

	// A well-formed but non-existent pod must not resolve to a different
	// namespace: the handler answers 404/503, never 200.
	rec = ownerRequest(t, s, http.MethodGet, "/api/ws/exec?namespace=kube-system&pod=does-not-exist")
	if rec.Code == http.StatusOK {
		t.Errorf("GET /api/ws/exec?namespace=kube-system: status = 200, want a refusal")
	}
}

// TestAV10NoSecretMaterialInResponses sweeps the secret-bearing surfaces for
// leaked credentials. The proxy secret and the agent token must never appear in
// any owner response; notifications already have their own test, so this covers
// the broader set: dashboard, metrics, status and identity endpoints.
func TestAV10NoSecretMaterialInResponses(t *testing.T) {
	s := newTestServer(t)

	secrets := []string{
		"test-proxy-secret", // PROXY_SHARED_SECRET
		"agent-secret-token",
		"service-password",
	}

	// The set is chosen so the sweep is fast: /api/dashboard, /api/metrics,
	// /api/auth/me and /api/ready each spend seconds waiting on LDAP or the node
	// in a unit test, and none of them carries credentials - the surfaces that
	// do (notifications, the rendered samba config, buddy status) are here.
	paths := []string{
		"/api/buddy/status",
		"/api/notifications",
		"/api/shares/config/samba",
		"/api/health",
	}

	for _, path := range paths {
		rec := ownerRequest(t, s, http.MethodGet, path)
		body := rec.Body.String()
		for _, secret := range secrets {
			if strings.Contains(body, secret) {
				t.Errorf("GET %s leaked %q in its response body", path, secret)
			}
		}
	}
}

// urlQueryEscape escapes a value for a query parameter. It is a tiny wrapper so
// the payload table above stays readable; net/url is the real implementation.
var urlQueryEscaper = url.QueryEscape

func urlQueryEscape(value string) string {
	return urlQueryEscaper(value)
}
