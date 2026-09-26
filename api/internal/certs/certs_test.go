package certs

import (
	"path/filepath"
	"testing"
)

func TestSpecCloudflareProduction(t *testing.T) {
	d := Domain{
		BaseDomain:        "example.com",
		DNSProvider:       ProviderCloudflare,
		CredentialsSecret: "cloudflare-creds",
		ACMEEmail:         "admin@example.com",
		Environment:       "production",
	}
	issuer, certificate, err := Spec(d, "naslos-apps")
	if err != nil {
		t.Fatalf("spec: %v", err)
	}
	if issuer.GetKind() != "Issuer" || certificate.GetKind() != "Certificate" {
		t.Fatalf("kinds = %s/%s", issuer.GetKind(), certificate.GetKind())
	}
	acme := issuer.Object["spec"].(map[string]interface{})["acme"].(map[string]interface{})
	if acme["server"] != "https://acme-v02.api.letsencrypt.org/directory" {
		t.Fatalf("server = %v", acme["server"])
	}
	dnsNames := certificate.Object["spec"].(map[string]interface{})["dnsNames"].([]interface{})
	if len(dnsNames) != 2 || dnsNames[1] != "*.example.com" {
		t.Fatalf("dnsNames = %v", dnsNames)
	}
}

func TestSpecStagingDefault(t *testing.T) {
	d := Domain{BaseDomain: "example.com", DNSProvider: ProviderCloudflare, CredentialsSecret: "c"}
	issuer, _, err := Spec(d, "ns")
	if err != nil {
		t.Fatalf("spec: %v", err)
	}
	acme := issuer.Object["spec"].(map[string]interface{})["acme"].(map[string]interface{})
	if acme["server"] != "https://acme-staging-v02.api.letsencrypt.org/directory" {
		t.Fatalf("server = %v", acme["server"])
	}
}

func TestDomainValidate(t *testing.T) {
	cases := []struct {
		name    string
		domain  Domain
		wantErr bool
	}{
		{"ok", Domain{BaseDomain: "example.com", DNSProvider: ProviderCloudflare, CredentialsSecret: "c"}, false},
		{"bad domain", Domain{BaseDomain: "not a domain", DNSProvider: ProviderCloudflare, CredentialsSecret: "c"}, true},
		{"missing secret", Domain{BaseDomain: "example.com", DNSProvider: ProviderCloudflare}, true},
		{"passthrough needs solver", Domain{BaseDomain: "example.com", DNSProvider: ProviderPassthrough}, true},
		{"bad env", Domain{BaseDomain: "example.com", DNSProvider: ProviderCloudflare, CredentialsSecret: "c", Environment: "prod"}, true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if err := tc.domain.Validate(); (err != nil) != tc.wantErr {
				t.Fatalf("Validate() error = %v, wantErr %v", err, tc.wantErr)
			}
		})
	}
}

func TestStoreRoundTrip(t *testing.T) {
	path := filepath.Join(t.TempDir(), "domains.json")
	store := NewStore(path)
	if err := store.Load(); err != nil {
		t.Fatalf("load: %v", err)
	}
	if err := store.Upsert(Domain{BaseDomain: "example.com", DNSProvider: ProviderCloudflare, CredentialsSecret: "c", Environment: "production"}); err != nil {
		t.Fatalf("upsert: %v", err)
	}
	reloaded := NewStore(path)
	if err := reloaded.Load(); err != nil {
		t.Fatalf("reload: %v", err)
	}
	got, err := reloaded.Get("example.com")
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	if got.Environment != "production" || got.SecretName() != "naslos-example-com-tls" {
		t.Fatalf("round trip mismatch: %+v (secret %s)", got, got.SecretName())
	}
}
