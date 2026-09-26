// Package routing renders and reconciles the Traefik exposure layer for
// installed apps. The API owns one IngressRoute per app plus the middlewares it
// references, all in the apps namespace.
package routing

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"strings"

	apierrors "k8s.io/apimachinery/pkg/api/errors"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/apis/meta/v1/unstructured"
	"k8s.io/apimachinery/pkg/runtime/schema"
	"k8s.io/apimachinery/pkg/types"
	"k8s.io/client-go/dynamic"

	"github.com/AessemOps/Naslos-Linux/api/internal/apps"
)

// GroupVersionResource for the Traefik CRDs (and the ExternalName Service the
// per-domain Authelia portal routes point at).
var (
	ingressRouteGVR = schema.GroupVersionResource{Group: "traefik.io", Version: "v1alpha1", Resource: "ingressroutes"}
	middlewareGVR   = schema.GroupVersionResource{Group: "traefik.io", Version: "v1alpha1", Resource: "middlewares"}
	serviceGVR      = schema.GroupVersionResource{Group: "", Version: "v1", Resource: "services"}
)

const (
	forwardAuthName     = "naslos-app-forwardauth-authelia"
	securityHeadersName = "naslos-app-security-headers"

	// portalExternalServiceName is the ExternalName Service in the apps
	// namespace that aliases the Authelia Service in the release namespace, so
	// the API can render portal routes without write access in the release
	// namespace.
	portalExternalServiceName = "naslos-authelia-portal"
	// portalLabelKey marks the router objects the API owns for SSO portals.
	portalLabelKey = "naslos.local/sso-portal"
)

// Spec is everything needed to render one app's route.
type Spec struct {
	Name      string
	Namespace string

	Subdomain  string
	BaseDomain string
	TLS        bool
	Auth       bool
	LocalOnly  bool

	Service string
	Port    int
	Scheme  string

	TLSSecret string

	AutheliaService string
	AutheliaPort    int
	// AutheliaNamespace is the namespace the Authelia Service lives in (the
	// platform namespace), which differs from the app route's namespace.
	AutheliaNamespace string

	LocalOnlyCIDR string
}

// Host returns the fully-qualified host, or "" when there is no subdomain.
func (s Spec) Host() string {
	if s.Subdomain == "" || s.BaseDomain == "" {
		return ""
	}
	return s.Subdomain + "." + s.BaseDomain
}

// Render returns the IngressRoute and middlewares for a spec. An empty
// subdomain means no route at all (cluster-internal only).
func Render(spec Spec) ([]*unstructured.Unstructured, error) {
	if spec.Host() == "" {
		return nil, nil
	}
	if spec.Service == "" || spec.Port <= 0 {
		return nil, fmt.Errorf("app %q has no route target service/port", spec.Name)
	}
	if spec.Scheme == "" {
		spec.Scheme = "http"
	}

	middlewares := []interface{}{
		map[string]interface{}{"name": securityHeadersName, "namespace": spec.Namespace},
	}
	if spec.Auth {
		if spec.AutheliaService == "" {
			return nil, fmt.Errorf("app %q requests auth but no Authelia service is configured", spec.Name)
		}
		middlewares = append(middlewares, map[string]interface{}{
			"name": forwardAuthName, "namespace": spec.Namespace,
		})
	}
	if spec.LocalOnly {
		if spec.LocalOnlyCIDR == "" {
			return nil, fmt.Errorf("app %q is local-only but no LAN CIDR is configured", spec.Name)
		}
		middlewares = append(middlewares, map[string]interface{}{
			"name": spec.Name + "-ipallowlist", "namespace": spec.Namespace,
		})
	}

	entryPoint := "web"
	if spec.TLS {
		entryPoint = "websecure"
	}

	objects := []*unstructured.Unstructured{
		securityHeadersMiddleware(spec.Namespace),
	}
	if spec.Auth {
		objects = append(objects, forwardAuthMiddleware(spec))
	}
	if spec.LocalOnly {
		objects = append(objects, ipAllowListMiddleware(spec))
	}

	route := &unstructured.Unstructured{Object: map[string]interface{}{
		"apiVersion": "traefik.io/v1alpha1",
		"kind":       "IngressRoute",
		"metadata": map[string]interface{}{
			"name":      spec.Name,
			"namespace": spec.Namespace,
		},
		"spec": map[string]interface{}{
			"entryPoints": []interface{}{entryPoint},
			"routes": []interface{}{map[string]interface{}{
				"match":       fmt.Sprintf("Host(`%s`)", spec.Host()),
				"kind":        "Rule",
				"middlewares": middlewares,
				"services": []interface{}{map[string]interface{}{
					"name":   spec.Service,
					"port":   int64(spec.Port),
					"scheme": spec.Scheme,
				}},
			}},
		},
	}}
	if spec.TLS && spec.TLSSecret != "" {
		route.Object["spec"].(map[string]interface{})["tls"] = map[string]interface{}{
			"secretName": spec.TLSSecret,
		}
	}
	return append(objects, route), nil
}

func securityHeadersMiddleware(namespace string) *unstructured.Unstructured {
	return &unstructured.Unstructured{Object: map[string]interface{}{
		"apiVersion": "traefik.io/v1alpha1",
		"kind":       "Middleware",
		"metadata": map[string]interface{}{
			"name":      securityHeadersName,
			"namespace": namespace,
		},
		"spec": map[string]interface{}{
			"headers": map[string]interface{}{
				"browserXssFilter":        true,
				"contentTypeNosniff":      true,
				"frameDeny":               true,
				"customFrameOptionsValue": "DENY",
				"referrerPolicy":          "strict-origin-when-cross-origin",
				"stsSeconds":              int64(31536000),
				// Deliberately no stsIncludeSubdomains: a subdomain serves a
				// TLS-off app and must not be HSTS-forced by the main UI.
			},
		},
	}}
}

func forwardAuthMiddleware(spec Spec) *unstructured.Unstructured {
	namespace := spec.AutheliaNamespace
	if namespace == "" {
		namespace = spec.Namespace
	}
	address := fmt.Sprintf(
		"http://%s.%s.svc.cluster.local:%d/api/authz/forward-auth?authelia_url=https://%s/authelia/",
		spec.AutheliaService, namespace, spec.AutheliaPort, spec.BaseDomain,
	)
	return &unstructured.Unstructured{Object: map[string]interface{}{
		"apiVersion": "traefik.io/v1alpha1",
		"kind":       "Middleware",
		"metadata": map[string]interface{}{
			"name":      forwardAuthName,
			"namespace": spec.Namespace,
		},
		"spec": map[string]interface{}{
			"forwardAuth": map[string]interface{}{
				"address":             address,
				"authResponseHeaders": []interface{}{"Remote-User", "Remote-Groups", "Remote-Email", "Remote-Name"},
			},
		},
	}}
}

func ipAllowListMiddleware(spec Spec) *unstructured.Unstructured {
	return &unstructured.Unstructured{Object: map[string]interface{}{
		"apiVersion": "traefik.io/v1alpha1",
		"kind":       "Middleware",
		"metadata": map[string]interface{}{
			"name":      spec.Name + "-ipallowlist",
			"namespace": spec.Namespace,
		},
		"spec": map[string]interface{}{
			"ipAllowList": map[string]interface{}{
				"sourceRange": []interface{}{spec.LocalOnlyCIDR},
			},
		},
	}}
}

// Reconciler applies rendered objects to the cluster.
type Reconciler struct {
	dyn       dynamic.Interface
	namespace string

	tlsSecret       string
	tlsSecretFor    func(baseDomain string) string
	autheliaService string
	autheliaPort    int
	autheliaNS      string
	localOnlyCIDR   string
}

// Options configures a Reconciler.
type Options struct {
	Namespace       string
	TLSSecret       string
	AutheliaService string
	AutheliaPort    int
	// AutheliaNamespace is the namespace the Authelia Service lives in.
	AutheliaNamespace string
	LocalOnlyCIDR     string
	// TLSSecretFor resolves the TLS Secret for a base domain (the wildcard
	// Certificate's secretName). When nil or empty, TLSSecret is used.
	TLSSecretFor func(baseDomain string) string
}

// NewReconciler creates a routing reconciler.
func NewReconciler(dyn dynamic.Interface, opts Options) *Reconciler {
	return &Reconciler{
		dyn:             dyn,
		namespace:       opts.Namespace,
		tlsSecret:       opts.TLSSecret,
		tlsSecretFor:    opts.TLSSecretFor,
		autheliaService: opts.AutheliaService,
		autheliaPort:    opts.AutheliaPort,
		autheliaNS:      opts.AutheliaNamespace,
		localOnlyCIDR:   opts.LocalOnlyCIDR,
	}
}

// SpecFor builds a routing spec from an app record. namespace is where the
// route and its middlewares live (the managed or privileged apps namespace).
func (r *Reconciler) SpecFor(rec apps.Record, namespace, baseDomain string) Spec {
	if namespace == "" {
		namespace = r.namespace
	}
	tlsSecret := r.tlsSecret
	if r.tlsSecretFor != nil {
		if resolved := r.tlsSecretFor(baseDomain); resolved != "" {
			tlsSecret = resolved
		}
	}
	return Spec{
		Name:              rec.Name,
		Namespace:         namespace,
		Subdomain:         rec.Exposure.Subdomain,
		BaseDomain:        baseDomain,
		TLS:               rec.Exposure.TLS,
		Auth:              rec.Exposure.Auth,
		LocalOnly:         rec.Exposure.LocalOnly,
		Service:           rec.Exposure.Service,
		Port:              rec.Exposure.Port,
		Scheme:            rec.Exposure.Scheme,
		TLSSecret:         tlsSecret,
		AutheliaService:   r.autheliaService,
		AutheliaPort:      r.autheliaPort,
		AutheliaNamespace: r.autheliaNS,
		LocalOnlyCIDR:     r.localOnlyCIDR,
	}
}

// Apply renders and server-side-applies an app's route and middlewares in
// namespace. Auth is only rendered when the base domain is in the SSO list.
func (r *Reconciler) Apply(ctx context.Context, rec apps.Record, namespace, baseDomain string, ssoDomains []string) error {
	spec := r.SpecFor(rec, namespace, baseDomain)
	if spec.Auth && !contains(ssoDomains, baseDomain) {
		return fmt.Errorf("base domain %q is not in the SSO domain list; auth is unavailable", baseDomain)
	}
	objects, err := Render(spec)
	if err != nil {
		return err
	}
	if objects == nil {
		// No subdomain: make sure a previously-applied route is gone.
		return r.Delete(ctx, namespace, rec.Name)
	}
	for _, obj := range objects {
		if err := r.applyObject(ctx, obj, spec.Namespace); err != nil {
			return err
		}
	}
	return nil
}

// Delete removes an app's IngressRoute and its per-app ipallowlist middleware
// from namespace.
func (r *Reconciler) Delete(ctx context.Context, namespace, name string) error {
	if namespace == "" {
		namespace = r.namespace
	}
	if err := r.deleteObject(ctx, namespace, ingressRouteGVR, name); err != nil {
		return err
	}
	return r.deleteObject(ctx, namespace, middlewareGVR, name+"-ipallowlist")
}

// TLSSecretFor exposes the per-domain TLS Secret resolver.
func (r *Reconciler) TLSSecretFor() func(baseDomain string) string { return r.tlsSecretFor }

// ReconcilePortals makes the Authelia portal reachable on every SSO domain
// except the primary (the chart serves the primary on the release namespace).
// Authelia's session cookie is per domain, so a promoted domain needs its own
// portal or the forwardAuth redirect 404s. Routes live in the apps namespace,
// where the API already has Service/IngressRoute rights, and target an
// ExternalName Service aliasing the Authelia Service, so no write access is
// needed in the release namespace. Best-effort: the caller logs the error.
func (r *Reconciler) ReconcilePortals(ctx context.Context, domains []string, primary string, tlsSecretFor func(string) string) error {
	if r.dyn == nil || r.autheliaService == "" {
		return nil
	}
	want := map[string]bool{}
	var errs []error
	for _, domain := range portalDomains(domains, primary) {
		secret := ""
		if tlsSecretFor != nil {
			secret = tlsSecretFor(domain)
		}
		if secret == "" {
			secret = r.tlsSecret
		}
		for _, obj := range r.portalObjects(domain, secret) {
			if err := r.applyObject(ctx, obj, r.namespace); err != nil {
				errs = append(errs, err)
			}
		}
		want[portalName(domain)] = true
	}
	if len(want) == 0 {
		// No promoted domains: drop the alias Service. Routes are removed by the
		// list below in the same pass.
		if err := r.deleteObject(ctx, r.namespace, serviceGVR, portalExternalServiceName); err != nil {
			errs = append(errs, err)
		}
	}
	list, err := r.dyn.Resource(ingressRouteGVR).Namespace(r.namespace).List(ctx, metav1.ListOptions{LabelSelector: portalLabelKey + "=true"})
	if err != nil {
		errs = append(errs, fmt.Errorf("listing portal routes: %w", err))
		return errors.Join(errs...)
	}
	for i := range list.Items {
		name := list.Items[i].GetName()
		if !want[name] {
			if err := r.deleteObject(ctx, r.namespace, ingressRouteGVR, name); err != nil {
				errs = append(errs, err)
			}
		}
	}
	return errors.Join(errs...)
}

// portalDomains filters the effective SSO list down to the domains that need an
// API-rendered portal (everything except the primary, which the chart serves).
func portalDomains(domains []string, primary string) []string {
	out := make([]string, 0, len(domains))
	for _, domain := range domains {
		if domain == "" || domain == primary {
			continue
		}
		out = append(out, domain)
	}
	return out
}

// portalObjects renders the ExternalName Service and the IngressRoute for one
// non-primary SSO domain.
func (r *Reconciler) portalObjects(domain, tlsSecret string) []*unstructured.Unstructured {
	externalName := fmt.Sprintf("%s.%s.svc.cluster.local", r.autheliaService, r.autheliaNS)
	labels := map[string]interface{}{portalLabelKey: "true"}
	svc := &unstructured.Unstructured{Object: map[string]interface{}{
		"apiVersion": "v1",
		"kind":       "Service",
		"metadata": map[string]interface{}{
			"name":      portalExternalServiceName,
			"namespace": r.namespace,
			"labels":    labels,
		},
		"spec": map[string]interface{}{
			"type":         "ExternalName",
			"externalName": externalName,
			"ports": []interface{}{map[string]interface{}{
				"name": "http", "port": int64(r.autheliaPort), "targetPort": int64(r.autheliaPort), "protocol": "TCP",
			}},
		},
	}}
	spec := map[string]interface{}{
		"entryPoints": []interface{}{"websecure"},
		"routes": []interface{}{map[string]interface{}{
			"match": fmt.Sprintf("Host(`%s`) && PathPrefix(`/authelia`)", domain),
			"kind":  "Rule",
			"services": []interface{}{map[string]interface{}{
				"name": portalExternalServiceName,
				"port": int64(r.autheliaPort),
			}},
		}},
	}
	if tlsSecret != "" {
		spec["tls"] = map[string]interface{}{"secretName": tlsSecret}
	}
	route := &unstructured.Unstructured{Object: map[string]interface{}{
		"apiVersion": "traefik.io/v1alpha1",
		"kind":       "IngressRoute",
		"metadata": map[string]interface{}{
			"name":      portalName(domain),
			"namespace": r.namespace,
			"labels":    labels,
		},
		"spec": spec,
	}}
	return []*unstructured.Unstructured{svc, route}
}

// portalName is a deterministic, DNS-label-safe IngressRoute name for a domain.
// A short hash suffix keeps two domains that sanitize to the same label apart
// and keeps the name within 63 characters.
func portalName(domain string) string {
	var b strings.Builder
	for _, r := range strings.ToLower(domain) {
		if (r >= 'a' && r <= 'z') || (r >= '0' && r <= '9') {
			b.WriteRune(r)
		} else {
			b.WriteByte('-')
		}
	}
	label := strings.Trim(b.String(), "-")
	if label == "" {
		label = "domain"
	}
	sum := sha256.Sum256([]byte(domain))
	suffix := hex.EncodeToString(sum[:])[:8]
	name := "naslos-sso-portal-" + label
	if len(name)+len(suffix)+1 > 63 {
		name = name[:63-len(suffix)-1]
	}
	return name + "-" + suffix
}

func (r *Reconciler) applyObject(ctx context.Context, obj *unstructured.Unstructured, namespace string) error {
	gvr := gvrFor(obj.GetKind())
	if gvr == nil {
		return fmt.Errorf("unknown routing kind %q", obj.GetKind())
	}
	data, err := obj.MarshalJSON()
	if err != nil {
		return err
	}
	_, err = r.dyn.Resource(*gvr).Namespace(namespace).Patch(
		ctx, obj.GetName(), types.ApplyPatchType, data,
		metav1.PatchOptions{FieldManager: "naslos-api"},
	)
	if err != nil {
		return fmt.Errorf("applying %s/%s: %w", obj.GetKind(), obj.GetName(), err)
	}
	return nil
}

func (r *Reconciler) deleteObject(ctx context.Context, namespace string, gvr schema.GroupVersionResource, name string) error {
	err := r.dyn.Resource(gvr).Namespace(namespace).Delete(ctx, name, metav1.DeleteOptions{})
	if err != nil && !apierrors.IsNotFound(err) {
		return fmt.Errorf("deleting %s/%s: %w", gvr.Resource, name, err)
	}
	return nil
}

func gvrFor(kind string) *schema.GroupVersionResource {
	switch obj := strings.ToLower(kind); obj {
	case "ingressroute":
		return &ingressRouteGVR
	case "middleware":
		return &middlewareGVR
	case "service":
		return &serviceGVR
	default:
		return nil
	}
}

func contains(list []string, value string) bool {
	for _, item := range list {
		if item == value {
			return true
		}
	}
	return false
}
