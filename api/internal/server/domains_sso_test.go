package server

import (
	"encoding/json"
	"net/http"
	"strings"
	"testing"

	corev1 "k8s.io/api/core/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/client-go/kubernetes/fake"
)

// seededAppsWithAuth is an app requiring Authelia auth on a secondary domain.
const seededAppsWithAuth = `[{"name":"jellyfin","exposure":{"subdomain":"jellyfin","tls":true,"auth":true},"baseDomain":"media.example.com","createdAt":"2026-01-01T00:00:00Z","updatedAt":"2026-01-01T00:00:00Z"}]`

// seededDomainsSSO is one registered secondary domain already promoted to SSO.
const seededDomainsSSO = `[{"baseDomain":"media.example.com","dnsProvider":"cloudflare","credentialsSecret":"x","environment":"staging","sso":true}]`

// withFakeCluster installs a fake clientset holding the Authelia pod and the
// fragment ConfigMap so the SSO endpoint's sync path has targets.
func withFakeCluster(s *Server) *fake.Clientset {
	client := fake.NewSimpleClientset(
		&corev1.Pod{ObjectMeta: metav1.ObjectMeta{Name: "naslos-authelia-0", Namespace: "naslos"}},
		&corev1.ConfigMap{
			ObjectMeta: metav1.ObjectMeta{Name: "naslos-authelia-sso", Namespace: "naslos"},
			Data:       map[string]string{},
		},
	)
	s.kubeClient = client
	return client
}

// TestDomainSSOPromotionTakesEffect covers enable: the flag is persisted, the
// effective list grows, and the app manager's auth gate follows immediately.
func TestDomainSSOPromotionTakesEffect(t *testing.T) {
	s := newSeededTestServer(t, "", seededDomains)
	withFakeCluster(s)

	rec := adminRequest(t, s, http.MethodPost, "/api/domains/media.example.com/sso", `{"enabled":true}`)
	if rec.Code != http.StatusOK {
		t.Fatalf("promote: status = %d, want 200 (body %s)", rec.Code, rec.Body.String())
	}
	var body struct {
		SSODomains []string `json:"ssoDomains"`
	}
	if err := json.Unmarshal(rec.Body.Bytes(), &body); err != nil {
		t.Fatalf("decode: %v (%s)", err, rec.Body.String())
	}
	if !containsString(body.SSODomains, "media.example.com") {
		t.Errorf("ssoDomains = %v, want the promoted domain", body.SSODomains)
	}
	if !s.appManager.AuthAllowed("media.example.com") {
		t.Error("auth gate did not follow the promotion")
	}
}

// TestDomainSSOPrimaryIsRejected pins that the primary is always SSO.
func TestDomainSSOPrimaryIsRejected(t *testing.T) {
	s := newSeededTestServer(t, "", seededDomains)
	withFakeCluster(s)

	rec := adminRequest(t, s, http.MethodPost, "/api/domains/naslos.local/sso", `{"enabled":true}`)
	if rec.Code != http.StatusBadRequest {
		t.Fatalf("primary promote: status = %d, want 400 (body %s)", rec.Code, rec.Body.String())
	}
}

// TestDomainSSODemoteRefusedWhileAuthAppExists is the safety guard: demoting a
// domain whose apps use auth would break their routes. The 409 names the app.
func TestDomainSSODemoteRefusedWhileAuthAppExists(t *testing.T) {
	s := newSeededTestServer(t, seededAppsWithAuth, seededDomainsSSO)
	withFakeCluster(s)

	rec := adminRequest(t, s, http.MethodPost, "/api/domains/media.example.com/sso", `{"enabled":false}`)
	if rec.Code != http.StatusConflict {
		t.Fatalf("demote with an auth app: status = %d, want 409 (body %s)", rec.Code, rec.Body.String())
	}
	if !strings.Contains(rec.Body.String(), "jellyfin") {
		t.Errorf("409 did not name the blocking app: %s", rec.Body.String())
	}
}

// TestDomainSSODemoteRefusedForEnvListedDomain pins the chart list as a floor:
// an SSO_DOMAINS entry cannot be demoted from the UI.
func TestDomainSSODemoteRefusedForEnvListedDomain(t *testing.T) {
	s := newSeededTestServerSSO(t, "", seededDomains, "naslos.local,media.example.com")
	withFakeCluster(s)

	rec := adminRequest(t, s, http.MethodPost, "/api/domains/media.example.com/sso", `{"enabled":false}`)
	if rec.Code != http.StatusConflict {
		t.Fatalf("demote an env-listed domain: status = %d, want 409 (body %s)", rec.Code, rec.Body.String())
	}
}

// TestDomainEditPreservesSSO is a regression guard: the Domains form never
// sends `sso`, so a plain edit must not silently demote a promoted domain.
func TestDomainEditPreservesSSO(t *testing.T) {
	s := newSeededTestServer(t, "", seededDomainsSSO)
	withFakeCluster(s)

	rec := adminRequest(t, s, http.MethodPut, "/api/domains/media.example.com",
		`{"baseDomain":"media.example.com","dnsProvider":"cloudflare","credentialsSecret":"x","environment":"staging"}`)
	if rec.Code != http.StatusOK {
		t.Fatalf("edit: status = %d, want 200 (body %s)", rec.Code, rec.Body.String())
	}
	if !s.appManager.AuthAllowed("media.example.com") {
		t.Error("editing the domain dropped the SSO flag")
	}
}

// TestDomainSSODemoteSucceedsWithoutAuthApps covers the happy demotion path.
func TestDomainSSODemoteSucceedsWithoutAuthApps(t *testing.T) {
	s := newSeededTestServer(t, "", seededDomainsSSO)
	withFakeCluster(s)

	rec := adminRequest(t, s, http.MethodPost, "/api/domains/media.example.com/sso", `{"enabled":false}`)
	if rec.Code != http.StatusOK {
		t.Fatalf("demote: status = %d, want 200 (body %s)", rec.Code, rec.Body.String())
	}
	if s.appManager.AuthAllowed("media.example.com") {
		t.Error("auth gate still allows a demoted domain")
	}
}
