package server

import (
	"context"
	"encoding/json"
	"net/http"
	"strings"
	"testing"

	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/client-go/kubernetes/fake"

	"github.com/AessemOps/Naslos-Linux/api/internal/ddns"
)

func TestProvidersListIncludesBuiltins(t *testing.T) {
	s := newTestServer(t)

	rec := adminRequest(t, s, http.MethodGet, "/api/providers", "")
	if rec.Code != http.StatusOK {
		t.Fatalf("GET /api/providers: status = %d (body %s)", rec.Code, rec.Body.String())
	}
	var payload struct {
		Providers []struct {
			Name        string   `json:"name"`
			CertManager bool     `json:"certManager"`
			DDNS        bool     `json:"ddns"`
			Driver      string   `json:"driver"`
			APIRights   []string `json:"apiRights"`
		} `json:"providers"`
		Errors []string `json:"errors"`
	}
	if err := json.Unmarshal(rec.Body.Bytes(), &payload); err != nil {
		t.Fatalf("decoding providers: %v", err)
	}
	byName := map[string]struct {
		CertManager bool
		DDNS        bool
		Driver      string
		APIRights   []string
	}{}
	for _, p := range payload.Providers {
		byName[p.Name] = struct {
			CertManager bool
			DDNS        bool
			Driver      string
			APIRights   []string
		}{p.CertManager, p.DDNS, p.Driver, p.APIRights}
	}
	ovh, ok := byName["ovh"]
	if !ok || !ovh.CertManager || !ovh.DDNS || ovh.Driver != "ovh" {
		t.Fatalf("ovh provider view = %+v (all %+v)", ovh, byName)
	}
	if generic, ok := byName["generic"]; !ok || generic.CertManager || !generic.DDNS || generic.Driver != "http" {
		t.Fatalf("generic provider view = %+v", generic)
	}
	for _, name := range []string{"cloudflare", "ovh", "rfc2136", "passthrough"} {
		p, ok := byName[name]
		if !ok || len(p.APIRights) == 0 {
			t.Fatalf("cert-capable provider %q is missing apiRights: %+v", name, p)
		}
	}
}

func TestDdnsUnknownProviderIs400(t *testing.T) {
	s := newTestServer(t)

	rec := adminRequest(t, s, http.MethodPost, "/api/ddns",
		`{"provider":"nope","zone":"example.com","record":"home","recordType":"A"}`)
	if rec.Code != http.StatusBadRequest {
		t.Fatalf("unknown provider: status = %d, want 400 (body %s)", rec.Code, rec.Body.String())
	}
}

func TestDdnsUnknownEntryIs404(t *testing.T) {
	s := newTestServer(t)

	rec := adminRequest(t, s, http.MethodGet, "/api/ddns/does-not-exist", "")
	if rec.Code != http.StatusNotFound {
		t.Fatalf("unknown entry: status = %d, want 404", rec.Code)
	}
}

func TestDdnsValidationRejectsBadInput(t *testing.T) {
	s := newTestServer(t)
	s.kubeClient = fake.NewSimpleClientset()

	cases := []struct {
		name string
		body string
	}{
		{"bad zone", `{"provider":"generic","zone":"not_a_domain","record":"home","recordType":"A","fields":{"updateUrl":"https://example.com"}}`},
		{"bad type", `{"provider":"generic","zone":"example.com","record":"home","recordType":"TXT","fields":{"updateUrl":"https://example.com"}}`},
		{"bad ttl", `{"provider":"generic","zone":"example.com","record":"home","recordType":"A","ttl":5,"fields":{"updateUrl":"https://example.com"}}`},
		{"missing required", `{"provider":"generic","zone":"example.com","record":"home","recordType":"A","fields":{}}`},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			rec := adminRequest(t, s, http.MethodPost, "/api/ddns", tc.body)
			if rec.Code != http.StatusBadRequest {
				t.Fatalf("status = %d, want 400 (body %s)", rec.Code, rec.Body.String())
			}
		})
	}
}

// TestDdnsNeverReturnsCredentialValues is the FR-DNS-02 regression: the create
// response and the list view must not contain a submitted token, and the token
// must land in the namespaced Secret instead.
func TestDdnsNeverReturnsCredentialValues(t *testing.T) {
	s := newTestServer(t)
	s.kubeClient = fake.NewSimpleClientset()

	rec := adminRequest(t, s, http.MethodPost, "/api/ddns",
		`{"provider":"generic","zone":"example.com","record":"home","recordType":"A","enabled":true,
		  "fields":{"updateUrl":"https://api.example.com/update?ip={{.ip}}","method":"GET","authType":"bearer","token":"s3cr3t-value"}}`)
	if rec.Code != http.StatusCreated {
		t.Fatalf("create: status = %d, want 201 (body %s)", rec.Code, rec.Body.String())
	}
	if strings.Contains(rec.Body.String(), "s3cr3t-value") {
		t.Fatalf("create response leaked the token: %s", rec.Body.String())
	}

	var entry struct {
		ID                string   `json:"id"`
		CredentialsSecret string   `json:"credentialsSecret"`
		CredentialFields  []string `json:"credentialFields"`
	}
	if err := json.Unmarshal(rec.Body.Bytes(), &entry); err != nil {
		t.Fatalf("decoding entry: %v", err)
	}
	if entry.CredentialsSecret == "" {
		t.Fatal("entry has no credentialsSecret")
	}
	if len(entry.CredentialFields) != 1 || entry.CredentialFields[0] != "token" {
		t.Fatalf("credentialFields = %v", entry.CredentialFields)
	}

	secret, err := s.kubeClient.CoreV1().Secrets("naslos-apps").Get(context.Background(), entry.CredentialsSecret, metav1.GetOptions{})
	if err != nil {
		t.Fatalf("reading the credential Secret: %v", err)
	}
	if string(secret.Data["token"]) != "s3cr3t-value" {
		t.Fatalf("Secret token = %q", secret.Data["token"])
	}

	list := adminRequest(t, s, http.MethodGet, "/api/ddns", "")
	if list.Code != http.StatusOK {
		t.Fatalf("list: status = %d", list.Code)
	}
	if strings.Contains(list.Body.String(), "s3cr3t-value") {
		t.Fatalf("list view leaked the token: %s", list.Body.String())
	}
	if !strings.Contains(list.Body.String(), entry.ID) {
		t.Fatalf("list view is missing the entry: %s", list.Body.String())
	}
}

func TestDdnsRunUnknownEntryIs404(t *testing.T) {
	s := newTestServer(t)

	rec := adminRequest(t, s, http.MethodPost, "/api/ddns/missing/run", "")
	if rec.Code != http.StatusNotFound {
		t.Fatalf("run of an unknown entry: status = %d, want 404", rec.Code)
	}
}

// TestDdnsEditResetsConvergedStatus covers the edit path: changing an entry must
// clear the old "ok" status so the next reconcile re-applies even when the
// public IP is unchanged.
func TestDdnsEditResetsConvergedStatus(t *testing.T) {
	s := newTestServer(t)
	s.kubeClient = fake.NewSimpleClientset()

	created := adminRequest(t, s, http.MethodPost, "/api/ddns",
		`{"provider":"generic","zone":"example.com","record":"home","recordType":"A","enabled":true,"fields":{"updateUrl":"https://api.example.com/update"}}`)
	if created.Code != http.StatusCreated {
		t.Fatalf("create: status = %d (body %s)", created.Code, created.Body.String())
	}
	var entry struct {
		ID string `json:"id"`
	}
	if err := json.Unmarshal(created.Body.Bytes(), &entry); err != nil {
		t.Fatalf("decode: %v", err)
	}
	if _, err := s.ddns.Store().Update(entry.ID, func(e *ddns.Entry) {
		e.LastStatus = "ok"
		e.LastIP = "203.0.113.7"
	}); err != nil {
		t.Fatalf("seed status: %v", err)
	}

	updated := adminRequest(t, s, http.MethodPut, "/api/ddns/"+entry.ID,
		`{"provider":"generic","zone":"example.com","record":"newhome","recordType":"A","enabled":true,"fields":{"updateUrl":"https://api.example.com/update"}}`)
	if updated.Code != http.StatusOK {
		t.Fatalf("update: status = %d (body %s)", updated.Code, updated.Body.String())
	}
	var got struct {
		LastStatus string `json:"lastStatus"`
	}
	if err := json.Unmarshal(updated.Body.Bytes(), &got); err != nil {
		t.Fatalf("decode update: %v", err)
	}
	if got.LastStatus != "" {
		t.Fatalf("edited entry kept status %q, want it cleared", got.LastStatus)
	}
}

// TestDomainRejectsInvalidProviderEnum covers provider field validation on the
// domains path (parity with /api/ddns).
func TestDomainRejectsInvalidProviderEnum(t *testing.T) {
	s := newTestServer(t)
	s.kubeClient = fake.NewSimpleClientset()

	rec := adminRequest(t, s, http.MethodPut, "/api/domains/example.com",
		`{"baseDomain":"example.com","dnsProvider":"ovh","environment":"staging",
		  "fields":{"endpoint":"bogus","applicationKey":"AK","applicationSecret":"AS","consumerKey":"CK"}}`)
	if rec.Code != http.StatusBadRequest {
		t.Fatalf("invalid enum: status = %d, want 400 (body %s)", rec.Code, rec.Body.String())
	}
}
