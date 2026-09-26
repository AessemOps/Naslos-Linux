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

// solverOf extracts dns01 solver from a rendered Issuer.
func solverOf(t *testing.T, d Domain) map[string]interface{} {
	t.Helper()
	issuer, _, err := Spec(d, "naslos-apps")
	if err != nil {
		t.Fatalf("spec: %v", err)
	}
	acme := issuer.Object["spec"].(map[string]interface{})["acme"].(map[string]interface{})
	solvers := acme["solvers"].([]interface{})
	return solvers[0].(map[string]interface{})["dns01"].(map[string]interface{})
}

// TestSpecCloudflareSolverShape pins the exact solver output the existing
// deployments rely on (FR-APP-13 regression).
func TestSpecCloudflareSolverShape(t *testing.T) {
	solver := solverOf(t, Domain{BaseDomain: "example.com", DNSProvider: ProviderCloudflare, CredentialsSecret: "cf"})
	ref := solver["cloudflare"].(map[string]interface{})["apiTokenSecretRef"].(map[string]interface{})
	if ref["name"] != "cf" || ref["key"] != "api-token" {
		t.Fatalf("cloudflare solver = %#v", solver)
	}
}

// TestSpecRFC2136SolverShape pins the RFC2136 solver output.
func TestSpecRFC2136SolverShape(t *testing.T) {
	solver := solverOf(t, Domain{BaseDomain: "example.com", DNSProvider: ProviderRFC2136, CredentialsSecret: "rfc"})
	rfc := solver["rfc2136"].(map[string]interface{})
	if rfc["nameserver"] != "rfc" || rfc["tsigAlgorithm"] != "HMACSHA256" || rfc["tsigKeyName"] != "" {
		t.Fatalf("rfc2136 solver = %#v", solver)
	}
	ref := rfc["tsigSecretSecretRef"].(map[string]interface{})
	if ref["name"] != "rfc" || ref["key"] != "tsig-secret" {
		t.Fatalf("rfc2136 tsig ref = %#v", ref)
	}
}

// TestSpecPassthroughIsUnchanged pins the raw-solver passthrough path.
func TestSpecPassthroughIsUnchanged(t *testing.T) {
	raw := map[string]interface{}{"acme-dns": map[string]interface{}{"host": "ns.example.com"}}
	solver := solverOf(t, Domain{BaseDomain: "example.com", DNSProvider: ProviderPassthrough, Solver: raw})
	if solver["acme-dns"].(map[string]interface{})["host"] != "ns.example.com" {
		t.Fatalf("passthrough solver = %#v", solver)
	}
}

// TestSpecOVH proves OVH is supported through the registry, with its
// non-secret config substituted and its secret keys read from the Secret.
func TestSpecOVH(t *testing.T) {
	d := Domain{
		BaseDomain:        "example.com",
		DNSProvider:       ProviderOVH,
		CredentialsSecret: "naslos-domain-example-com-creds",
		ProviderConfig:    map[string]string{"endpoint": "ovh-ca", "applicationKey": "AK"},
	}
	if err := d.Validate(); err != nil {
		t.Fatalf("validate: %v", err)
	}
	solver := solverOf(t, d)
	ovh := solver["ovh"].(map[string]interface{})
	if ovh["endpoint"] != "ovh-ca" || ovh["applicationKey"] != "AK" {
		t.Fatalf("ovh config not substituted: %#v", ovh)
	}
	ref := ovh["applicationSecretSecretRef"].(map[string]interface{})
	if ref["name"] != "naslos-domain-example-com-creds" || ref["key"] != "applicationSecret" {
		t.Fatalf("ovh secret ref = %#v", ref)
	}
}
