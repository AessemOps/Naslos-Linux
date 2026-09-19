// Package auth provides authentication middleware for Naslos API.
// It trusts Remote-User/Remote-Groups headers ONLY from Traefik's pod CIDR, and
// only when the request also carries the shared secret that Traefik's
// proxy-identity middleware injects. The CIDR check alone cannot distinguish
// Traefik from any other pod in the cluster; the secret can (see SEC-1/NAS-001).
package auth

import (
	"crypto/subtle"
	"fmt"
	"net"
	"net/http"
	"strings"
	"sync"
)

// ProxySecretHeader is the header Traefik's `proxy-identity` middleware injects
// with the shared value. A request without it (or with the wrong value) is not
// trusted, no matter where it comes from.
const ProxySecretHeader = "X-Naslos-Proxy-Secret"

// Middleware provides authentication and authorization.
type Middleware struct {
	trustedCIDRs []*net.IPNet
	proxySecret  string
	mu           sync.RWMutex
}

// NewMiddleware creates a new auth middleware. proxySecret is the value only the
// proxy knows; an empty one means no request can ever pass RequireAuth (fail
// closed) and callers should refuse to start instead.
func NewMiddleware(trustedCIDRs []string, proxySecret string) (*Middleware, error) {
	cidrs := make([]*net.IPNet, 0, len(trustedCIDRs))
	for _, cidr := range trustedCIDRs {
		_, ipNet, err := net.ParseCIDR(cidr)
		if err != nil {
			return nil, fmt.Errorf("parsing CIDR %q: %w", cidr, err)
		}
		cidrs = append(cidrs, ipNet)
	}

	return &Middleware{
		trustedCIDRs: cidrs,
		proxySecret:  proxySecret,
	}, nil
}

// isTrusted checks if the request comes from a trusted source.
func (m *Middleware) isTrusted(r *http.Request) bool {
	host, _, err := net.SplitHostPort(r.RemoteAddr)
	if err != nil {
		host = r.RemoteAddr
	}
	ip := net.ParseIP(host)
	if ip == nil {
		return false
	}

	m.mu.RLock()
	defer m.mu.RUnlock()

	for _, cidr := range m.trustedCIDRs {
		if cidr.Contains(ip) {
			return true
		}
	}
	return false
}

// RequireAuth middleware enforces authentication.
func (m *Middleware) RequireAuth(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		// An unconfigured secret must never authorize anything: without it the
		// middleware would accept an empty header as proof.
		if m.proxySecret == "" {
			http.Error(w, "Unauthorized", http.StatusUnauthorized)
			return
		}

		// Only trust auth headers from Traefik's network...
		if !m.isTrusted(r) {
			http.Error(w, "Unauthorized", http.StatusUnauthorized)
			return
		}

		// ...and only when the unforgeable proxy secret proves the request came
		// through the proxy. This is what a request from any other source cannot supply.
		if subtle.ConstantTimeCompare([]byte(r.Header.Get(ProxySecretHeader)), []byte(m.proxySecret)) != 1 {
			http.Error(w, "Unauthorized", http.StatusUnauthorized)
			return
		}

		// Extract user from Remote-User header
		username := r.Header.Get("Remote-User")
		if username == "" {
			http.Error(w, "Unauthorized", http.StatusUnauthorized)
			return
		}

		// Store user info in context
		ctx := WithUser(r.Context(), &UserInfo{
			Username:    username,
			Groups:      parseGroups(r.Header.Get("Remote-Groups")),
			Email:       r.Header.Get("Remote-Email"),
			DisplayName: r.Header.Get("Remote-Name"),
		})

		next.ServeHTTP(w, r.WithContext(ctx))
	})
}

// RequireAdmin middleware enforces admin group membership.
func (m *Middleware) RequireAdmin(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		user := UserFromContext(r.Context())
		if user == nil {
			http.Error(w, "Unauthorized", http.StatusUnauthorized)
			return
		}

		if !user.HasGroup("naslos_admins") {
			http.Error(w, "Forbidden", http.StatusForbidden)
			return
		}

		next.ServeHTTP(w, r)
	})
}

// parseGroups parses comma-separated groups.
func parseGroups(groups string) []string {
	if groups == "" {
		return nil
	}
	parts := strings.Split(groups, ",")
	for i, p := range parts {
		parts[i] = strings.TrimSpace(p)
	}
	return parts
}
