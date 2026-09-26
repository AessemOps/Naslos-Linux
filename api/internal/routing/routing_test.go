package routing

import (
	"strings"
	"testing"

	"github.com/AessemOps/Naslos-Linux/api/internal/apps"
)

func baseSpec() Spec {
	return Spec{
		Name:            "jellyfin",
		Namespace:       "naslos-apps",
		Subdomain:       "jellyfin",
		BaseDomain:      "example.com",
		TLS:             true,
		Auth:            true,
		Service:         "jellyfin",
		Port:            8096,
		Scheme:          "http",
		TLSSecret:       "naslos-tls",
		AutheliaService: "naslos-authelia",
		AutheliaPort:    80,
		// Authelia lives in the platform namespace, not the apps namespace.
		AutheliaNamespace: "naslos",
	}
}

func TestRenderTLSAndAuthRoute(t *testing.T) {
	objects, err := Render(baseSpec())
	if err != nil {
		t.Fatalf("render: %v", err)
	}
	var route, forwardAuth, headers map[string]interface{}
	for _, obj := range objects {
		switch obj.GetKind() {
		case "IngressRoute":
			route = obj.Object
		case "Middleware":
			if obj.GetName() == forwardAuthName {
				forwardAuth = obj.Object
			}
			if obj.GetName() == securityHeadersName {
				headers = obj.Object
			}
		}
	}
	if route == nil || forwardAuth == nil || headers == nil {
		t.Fatalf("missing rendered objects: route=%v auth=%v headers=%v", route != nil, forwardAuth != nil, headers != nil)
	}
	spec := route["spec"].(map[string]interface{})
	if entries := spec["entryPoints"].([]interface{}); entries[0] != "websecure" {
		t.Fatalf("entryPoint = %v, want websecure", entries[0])
	}
	if _, ok := spec["tls"]; !ok {
		t.Fatal("TLS route has no tls block")
	}
	routes := spec["routes"].([]interface{})
	first := routes[0].(map[string]interface{})
	if first["match"] != "Host(`jellyfin.example.com`)" {
		t.Fatalf("match = %v", first["match"])
	}
	middlewares := first["middlewares"].([]interface{})
	if len(middlewares) != 2 {
		t.Fatalf("middlewares = %v, want security-headers + forwardauth", middlewares)
	}
	// Security headers must NOT include HSTS subdomains.
	hspec := headers["spec"].(map[string]interface{})["headers"].(map[string]interface{})
	if _, ok := hspec["stsIncludeSubdomains"]; ok {
		t.Fatal("stsIncludeSubdomains must not be set")
	}
	// The forwardAuth address must target the Authelia Service in its own
	// (platform) namespace, not the apps namespace.
	addr := forwardAuth["spec"].(map[string]interface{})["forwardAuth"].(map[string]interface{})["address"].(string)
	if !strings.Contains(addr, "naslos-authelia.naslos.svc.cluster.local:80") {
		t.Fatalf("forwardAuth address = %q, want the Authelia service in the platform namespace", addr)
	}
}

func TestRenderNoSubdomainMeansNoRoute(t *testing.T) {
	spec := baseSpec()
	spec.Subdomain = ""
	objects, err := Render(spec)
	if err != nil {
		t.Fatalf("render: %v", err)
	}
	if objects != nil {
		t.Fatalf("expected no objects, got %d", len(objects))
	}
}

func TestRenderHTTPNoTLS(t *testing.T) {
	spec := baseSpec()
	spec.TLS = false
	spec.Auth = false
	objects, err := Render(spec)
	if err != nil {
		t.Fatalf("render: %v", err)
	}
	route := objects[len(objects)-1]
	specObj := route.Object["spec"].(map[string]interface{})
	if entries := specObj["entryPoints"].([]interface{}); entries[0] != "web" {
		t.Fatalf("entryPoint = %v, want web", entries[0])
	}
	if _, ok := specObj["tls"]; ok {
		t.Fatal("TLS-off route must not have a tls block")
	}
}

func TestRenderLocalOnlyRequiresCIDR(t *testing.T) {
	spec := baseSpec()
	spec.Auth = false
	spec.LocalOnly = true
	if _, err := Render(spec); err == nil {
		t.Fatal("expected an error when localOnly has no CIDR")
	}
	spec.LocalOnlyCIDR = "192.168.1.0/24"
	objects, err := Render(spec)
	if err != nil {
		t.Fatalf("render: %v", err)
	}
	found := false
	for _, obj := range objects {
		if obj.GetKind() == "Middleware" && obj.GetName() == "jellyfin-ipallowlist" {
			found = true
		}
	}
	if !found {
		t.Fatal("ipallowlist middleware not rendered")
	}
}

func TestRenderMissingTarget(t *testing.T) {
	spec := baseSpec()
	spec.Service = ""
	if _, err := Render(spec); err == nil {
		t.Fatal("expected an error when the route target is missing")
	}
}

func TestSpecForResolvesPerDomainTLSSecret(t *testing.T) {
	r := NewReconciler(nil, Options{
		Namespace: "naslos-apps",
		TLSSecret: "naslos-apps-tls",
		TLSSecretFor: func(baseDomain string) string {
			if baseDomain == "example.com" {
				return "naslos-example-com-tls"
			}
			return ""
		},
	})
	rec := apps.Record{Name: "jellyfin", Exposure: apps.Exposure{Subdomain: "jellyfin", TLS: true}}

	if got := r.SpecFor(rec, "naslos-apps", "example.com").TLSSecret; got != "naslos-example-com-tls" {
		t.Fatalf("TLS secret = %q, want the per-domain secret", got)
	}
	if got := r.SpecFor(rec, "naslos-apps", "other.example").TLSSecret; got != "naslos-apps-tls" {
		t.Fatalf("TLS secret fallback = %q, want naslos-apps-tls", got)
	}
}
