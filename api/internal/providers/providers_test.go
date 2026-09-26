package providers

import (
	"os"
	"path/filepath"
	"testing"
)

func TestLoadBuiltins(t *testing.T) {
	r := Load("")
	if errs := r.Errors(); len(errs) != 0 {
		t.Fatalf("built-in load errors: %v", errs)
	}
	for _, name := range []string{"ovh", "cloudflare", "rfc2136", "passthrough", "generic"} {
		p, ok := r.Get(name)
		if !ok {
			t.Fatalf("missing built-in provider %q", name)
		}
		if p.DisplayName == "" {
			t.Errorf("provider %q has no display name", name)
		}
	}
	if p, _ := r.Get("ovh"); !p.HasCertManager() || !p.HasDDNS() {
		t.Errorf("ovh must support certificates and DDNS")
	}
	if p, _ := r.Get("cloudflare"); !p.HasCertManager() || !p.HasDDNS() {
		t.Errorf("cloudflare must support certificates and DDNS")
	}
	if p, _ := r.Get("generic"); p.HasCertManager() || !p.HasDDNS() || p.DDNS.Driver != "http" {
		t.Errorf("generic must be DDNS-only with the http driver")
	}
	if p, _ := r.Get("passthrough"); !p.IsPassthrough() || !p.SupportsCertificates() {
		t.Errorf("passthrough must be a certificate provider with an inline solver")
	}
}

func TestOVHSolverShape(t *testing.T) {
	p, _ := Load("").Get("ovh")
	solver, err := p.Solver("naslos-domain-example-com-creds", map[string]string{"applicationKey": "AK"})
	if err != nil {
		t.Fatalf("solver: %v", err)
	}
	ovh, ok := solver["ovh"].(map[string]interface{})
	if !ok {
		t.Fatalf("solver has no ovh mapping: %#v", solver)
	}
	if ovh["endpoint"] != "ovh-eu" {
		t.Errorf("endpoint default not applied: %v", ovh["endpoint"])
	}
	if ovh["applicationKey"] != "AK" {
		t.Errorf("applicationKey not substituted: %v", ovh["applicationKey"])
	}
	for _, key := range []string{"applicationSecretSecretRef", "consumerKeySecretRef"} {
		ref, ok := ovh[key].(map[string]interface{})
		if !ok {
			t.Fatalf("%s is not a mapping: %#v", key, ovh[key])
		}
		if ref["name"] != "naslos-domain-example-com-creds" {
			t.Errorf("%s name = %v", key, ref["name"])
		}
	}
}

func TestSolverMissingConfigIsAnError(t *testing.T) {
	p, _ := Load("").Get("ovh")
	if _, err := p.Solver("s", map[string]string{}); err == nil {
		t.Fatal("expected an error when a ${cred.*} placeholder has no value")
	}
}

func TestSolverRequiresSecretName(t *testing.T) {
	p, _ := Load("").Get("cloudflare")
	if _, err := p.Solver("", nil); err == nil {
		t.Fatal("expected an error when ${secret} is not set")
	}
}

func TestOverrideReplacesBuiltinAndSkipsInvalid(t *testing.T) {
	dir := t.TempDir()
	override := `name: ovh
displayName: OVH Override
fields:
  - key: applicationKey
    label: Application key
    type: string
    required: true
`
	if err := os.WriteFile(filepath.Join(dir, "ovh.yaml"), []byte(override), 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "broken.yaml"), []byte("name: Bad_Name\n"), 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "ignored.txt"), []byte("not yaml"), 0600); err != nil {
		t.Fatal(err)
	}

	r := Load(dir)
	if len(r.Errors()) == 0 {
		t.Fatal("expected the invalid override to be recorded")
	}
	p, ok := r.Get("ovh")
	if !ok {
		t.Fatal("ovh should still exist")
	}
	if p.DisplayName != "OVH Override" {
		t.Errorf("override did not replace the built-in: %q", p.DisplayName)
	}
	if p.HasDDNS() {
		t.Errorf("override should have dropped DDNS support")
	}
}

func TestResolveFieldsSplitsConfigAndSecrets(t *testing.T) {
	p, _ := Load("").Get("ovh")

	resolved, err := p.ResolveFields(map[string]string{
		"applicationKey":    "AK",
		"applicationSecret": "AS",
		"consumerKey":       "CK",
	}, nil, nil, true)
	if err != nil {
		t.Fatalf("resolve: %v", err)
	}
	if resolved.Config["endpoint"] != "ovh-eu" || resolved.Config["applicationKey"] != "AK" {
		t.Fatalf("config = %#v", resolved.Config)
	}
	if resolved.SecretValues["applicationSecret"] != "AS" || resolved.SecretValues["consumerKey"] != "CK" {
		t.Fatalf("secret values = %#v", resolved.SecretValues)
	}
	if len(resolved.CredentialFields) != 2 {
		t.Fatalf("credential fields = %v", resolved.CredentialFields)
	}

	// A missing required secret on create fails.
	if _, err := p.ResolveFields(map[string]string{"applicationKey": "AK", "applicationSecret": "AS"}, nil, nil, true); err == nil {
		t.Fatal("expected a missing required secret to fail on create")
	}

	// On update, a stored config value is kept when the field is omitted, and an
	// invalid enum is rejected.
	existing := map[string]string{"endpoint": "ovh-ca", "applicationKey": "AK"}
	resolved, err = p.ResolveFields(map[string]string{"consumerKey": "CK"}, existing, []string{"applicationSecret"}, false)
	if err != nil {
		t.Fatalf("update resolve: %v", err)
	}
	if resolved.Config["endpoint"] != "ovh-ca" {
		t.Fatalf("existing config not kept: %#v", resolved.Config)
	}
	if _, err := p.ResolveFields(map[string]string{"endpoint": "bogus", "applicationKey": "AK"}, existing, []string{"applicationSecret"}, false); err == nil {
		t.Fatal("expected an invalid enum to be rejected")
	}
}

func TestParseRejectsDuplicateFieldsAndBadEnums(t *testing.T) {
	cases := map[string]string{
		"duplicate":  "name: x\nfields:\n  - {key: a, label: A, type: string}\n  - {key: a, label: A2, type: string}\n",
		"bad enum":   "name: x\nfields:\n  - key: a\n    label: A\n    type: enum\n    enum: [one]\n    default: two\n",
		"bad name":   "name: Bad_Name\n",
		"bad driver": "name: x\nddns:\n  driver: nope\n",
	}
	for name, doc := range cases {
		t.Run(name, func(t *testing.T) {
			if _, err := Parse([]byte(doc)); err == nil {
				t.Fatalf("expected parse to fail for %s", name)
			}
		})
	}
}
