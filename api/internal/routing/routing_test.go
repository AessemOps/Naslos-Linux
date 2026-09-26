package routing

import (
	"context"
	"strings"
	"testing"

	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/apis/meta/v1/unstructured"
	"k8s.io/apimachinery/pkg/runtime"
	"k8s.io/apimachinery/pkg/runtime/schema"
	dynamicfake "k8s.io/client-go/dynamic/fake"

	"github.com/AessemOps/Naslos-Linux/api/internal/apps"
)

func newFakeReconciler(t *testing.T) (*Reconciler, *dynamicfake.FakeDynamicClient) {
	t.Helper()
	client := dynamicfake.NewSimpleDynamicClientWithCustomListKinds(runtime.NewScheme(), map[schema.GroupVersionResource]string{
		ingressRouteGVR: "IngressRouteList",
		middlewareGVR:   "MiddlewareList",
		serviceGVR:      "ServiceList",
	})
	r := NewReconciler(client, Options{
		Namespace:         "naslos-apps",
		TLSSecret:         "naslos-apps-tls",
		AutheliaService:   "naslos-authelia",
		AutheliaPort:      80,
		AutheliaNamespace: "naslos",
	})
	return r, client
}

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

func TestPortalDomainsSkipsPrimary(t *testing.T) {
	got := portalDomains([]string{"naslos.local", "", "media.example.com", "naslos.local"}, "naslos.local")
	if len(got) != 1 || got[0] != "media.example.com" {
		t.Fatalf("portalDomains = %v, want [media.example.com]", got)
	}
}

func TestPortalObjectsRenderAliasAndRoute(t *testing.T) {
	r, _ := newFakeReconciler(t)
	objects := r.portalObjects("media.example.com", "naslos-media-example-com-tls")
	if len(objects) != 2 {
		t.Fatalf("expected service + route, got %d", len(objects))
	}
	svc, route := objects[0], objects[1]
	if svc.GetKind() != "Service" || route.GetKind() != "IngressRoute" {
		t.Fatalf("kinds = %s, %s", svc.GetKind(), route.GetKind())
	}
	if got, _, _ := unstructured.NestedString(svc.Object, "spec", "externalName"); got != "naslos-authelia.naslos.svc.cluster.local" {
		t.Fatalf("externalName = %q", got)
	}
	if got, _, _ := unstructured.NestedString(svc.Object, "metadata", "labels", portalLabelKey); got != "true" {
		t.Fatalf("service label missing: %v", svc.GetLabels())
	}
	routes, _, _ := unstructured.NestedSlice(route.Object, "spec", "routes")
	match, _ := routes[0].(map[string]interface{})["match"].(string)
	if match != "Host(`media.example.com`) && PathPrefix(`/authelia`)" {
		t.Fatalf("match = %q", match)
	}
	if secret, _, _ := unstructured.NestedString(route.Object, "spec", "tls", "secretName"); secret != "naslos-media-example-com-tls" {
		t.Fatalf("tls secret = %q", secret)
	}
	if entries, _, _ := unstructured.NestedStringSlice(route.Object, "spec", "entryPoints"); len(entries) != 1 || entries[0] != "websecure" {
		t.Fatalf("entryPoints = %v", entries)
	}
}

func TestPortalNameDeterministicAndBounded(t *testing.T) {
	long := strings.Repeat("a", 80) + ".example.com"
	name := portalName(long)
	if len(name) > 63 {
		t.Fatalf("portal name %q is %d chars, want <= 63", name, len(name))
	}
	if name != portalName(long) {
		t.Fatal("portalName is not deterministic")
	}
	// Domains that sanitize to the same label must stay distinct.
	if portalName("a.b.example") == portalName("a-b.example") {
		t.Fatal("sanitized label collision produced the same portal name")
	}
}

func TestReconcilePortalsDeletesStaleRouteAndAlias(t *testing.T) {
	r, client := newFakeReconciler(t)
	ctx := context.Background()

	// Seed a stale portal route and the alias service.
	stale := &unstructured.Unstructured{Object: map[string]interface{}{
		"apiVersion": "traefik.io/v1alpha1",
		"kind":       "IngressRoute",
		"metadata": map[string]interface{}{
			"name":      portalName("old.example.com"),
			"namespace": "naslos-apps",
			"labels":    map[string]interface{}{portalLabelKey: "true"},
		},
	}}
	if _, err := client.Resource(ingressRouteGVR).Namespace("naslos-apps").Create(ctx, stale, metav1.CreateOptions{}); err != nil {
		t.Fatalf("seed route: %v", err)
	}
	svc := &unstructured.Unstructured{Object: map[string]interface{}{
		"apiVersion": "v1",
		"kind":       "Service",
		"metadata":   map[string]interface{}{"name": portalExternalServiceName, "namespace": "naslos-apps"},
	}}
	if _, err := client.Resource(serviceGVR).Namespace("naslos-apps").Create(ctx, svc, metav1.CreateOptions{}); err != nil {
		t.Fatalf("seed service: %v", err)
	}

	// Only the primary remains SSO: the stale route and the alias must go.
	if err := r.ReconcilePortals(ctx, []string{"naslos.local"}, "naslos.local", nil); err != nil {
		t.Fatalf("reconcile: %v", err)
	}
	if _, err := client.Resource(ingressRouteGVR).Namespace("naslos-apps").Get(ctx, portalName("old.example.com"), metav1.GetOptions{}); err == nil {
		t.Fatal("stale portal route was not removed")
	}
	if _, err := client.Resource(serviceGVR).Namespace("naslos-apps").Get(ctx, portalExternalServiceName, metav1.GetOptions{}); err == nil {
		t.Fatal("alias service was not removed once no promoted domain remained")
	}
}

func TestReconcilePortalsNoopWithoutDynamicClient(t *testing.T) {
	r := NewReconciler(nil, Options{Namespace: "naslos-apps", AutheliaService: "naslos-authelia"})
	if err := r.ReconcilePortals(context.Background(), []string{"naslos.local", "x.example.com"}, "naslos.local", nil); err != nil {
		t.Fatalf("nil client must be a no-op, got %v", err)
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
