package auth

import (
	"net/http"
	"net/http/httptest"
	"testing"
)

// The middleware is the only thing standing between the cluster network and
// every owner route (NAS-001), so each rejection reason is pinned here.
func TestRequireAuthRejectsUnlessTrustedAndProven(t *testing.T) {
	// httptest requests come from 192.0.2.1 and the test router treats that as
	// the trusted proxy range, exactly like TRAEFIK_CIDR does in the chart.
	m, err := NewMiddleware([]string{"192.0.2.0/24"}, "proxy-secret")
	if err != nil {
		t.Fatalf("NewMiddleware: %v", err)
	}

	next := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		user := UserFromContext(r.Context())
		if user == nil {
			t.Error("handler reached without a user in the context")
			w.WriteHeader(http.StatusInternalServerError)
			return
		}
		if user.Username != "admin" {
			t.Errorf("context user = %q, want admin", user.Username)
		}
		if !user.HasGroup("naslos_admins") {
			t.Errorf("context groups = %v, want naslos_admins", user.Groups)
		}
		w.WriteHeader(http.StatusOK)
	})

	call := func(headers map[string]string, remoteAddr string) *httptest.ResponseRecorder {
		req := httptest.NewRequest(http.MethodGet, "/api/users", nil)
		for k, v := range headers {
			req.Header.Set(k, v)
		}
		if remoteAddr != "" {
			req.RemoteAddr = remoteAddr
		}
		rec := httptest.NewRecorder()
		m.RequireAuth(next).ServeHTTP(rec, req)
		return rec
	}

	// Nothing at all.
	if rec := call(nil, ""); rec.Code != http.StatusUnauthorized {
		t.Errorf("no headers: status = %d, want 401", rec.Code)
	}

	// The identity header alone is exactly the forgery the secret exists to stop.
	if rec := call(map[string]string{"Remote-User": "admin"}, ""); rec.Code != http.StatusUnauthorized {
		t.Errorf("user without secret: status = %d, want 401", rec.Code)
	}

	// The secret alone is not an identity either.
	if rec := call(map[string]string{ProxySecretHeader: "proxy-secret"}, ""); rec.Code != http.StatusUnauthorized {
		t.Errorf("secret without user: status = %d, want 401", rec.Code)
	}

	// A wrong secret must fail even with a plausible user.
	if rec := call(map[string]string{
		ProxySecretHeader: "not-the-secret",
		"Remote-User":     "admin",
	}, ""); rec.Code != http.StatusUnauthorized {
		t.Errorf("wrong secret: status = %d, want 401", rec.Code)
	}

	// A request that did not come from the trusted range cannot pass, even with
	// both headers (a peer pod, or anything else on the cluster network).
	if rec := call(map[string]string{
		ProxySecretHeader: "proxy-secret",
		"Remote-User":     "admin",
	}, "203.0.113.9:5555"); rec.Code != http.StatusUnauthorized {
		t.Errorf("untrusted source: status = %d, want 401", rec.Code)
	}

	// The real thing.
	if rec := call(map[string]string{
		ProxySecretHeader: "proxy-secret",
		"Remote-User":     "admin",
		"Remote-Groups":   "naslos_admins,users",
	}, ""); rec.Code != http.StatusOK {
		t.Errorf("secret + user + trusted: status = %d, want 200 (%s)", rec.Code, rec.Body.String())
	}
}

// An unconfigured secret must never authorize: otherwise an empty header would
// compare equal to an empty expectation and every route would be open.
func TestRequireAuthFailsClosedWithoutAConfiguredSecret(t *testing.T) {
	m, err := NewMiddleware([]string{"192.0.2.0/24"}, "")
	if err != nil {
		t.Fatalf("NewMiddleware: %v", err)
	}

	reached := false
	next := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		reached = true
	})

	req := httptest.NewRequest(http.MethodGet, "/api/users", nil)
	req.Header.Set("Remote-User", "admin")
	rec := httptest.NewRecorder()
	m.RequireAuth(next).ServeHTTP(rec, req)

	if reached {
		t.Error("handler was reached with no configured secret")
	}
	if rec.Code != http.StatusUnauthorized {
		t.Errorf("status = %d, want 401", rec.Code)
	}
}

// TestNewMiddlewareRejectsBadCIDR keeps a typo in TRAEFIK_CIDR from becoming a
// silently-open or silently-broken deployment.
func TestNewMiddlewareRejectsBadCIDR(t *testing.T) {
	if _, err := NewMiddleware([]string{"not-a-cidr"}, "secret"); err == nil {
		t.Error("NewMiddleware(bad CIDR) = nil error, want a rejection")
	}
}
