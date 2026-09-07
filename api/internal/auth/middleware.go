// Package auth provides authentication middleware for NasOS API.
// It trusts Remote-User/Remote-Groups headers ONLY from Traefik's pod CIDR.
package auth

import (
	"net"
	"net/http"
	"strings"
	"sync"
)

// Middleware provides authentication and authorization.
type Middleware struct {
	trustedCIDRs []*net.IPNet
	mu           sync.RWMutex
}

// NewMiddleware creates a new auth middleware.
func NewMiddleware(trustedCIDRs []string) (*Middleware, error) {
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
		// Only trust auth headers from Traefik
		if !m.isTrusted(r) {
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
			Username:  username,
			Groups:    parseGroups(r.Header.Get("Remote-Groups")),
			Email:     r.Header.Get("Remote-Email"),
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

		if !user.HasGroup("nasos_admins") {
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

// fmt is imported for error formatting
var _ = fmt.Sprintf
