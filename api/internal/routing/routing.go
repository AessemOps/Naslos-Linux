// Package routing renders and reconciles the Traefik exposure layer for
// installed apps. The API owns one IngressRoute per app plus the middlewares it
// references, all in the apps namespace.
package routing

import (
	"context"
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

// GroupVersionResource for the Traefik CRDs.
var (
	ingressRouteGVR = schema.GroupVersionResource{Group: "traefik.io", Version: "v1alpha1", Resource: "ingressroutes"}
	middlewareGVR   = schema.GroupVersionResource{Group: "traefik.io", Version: "v1alpha1", Resource: "middlewares"}
)

const (
	forwardAuthName     = "naslos-app-forwardauth-authelia"
	securityHeadersName = "naslos-app-security-headers"
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
	address := fmt.Sprintf(
		"http://%s.%s.svc.cluster.local:%d/api/authz/forward-auth?authelia_url=https://%s/authelia/",
		spec.AutheliaService, spec.Namespace, spec.AutheliaPort, spec.BaseDomain,
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
	autheliaService string
	autheliaPort    int
	localOnlyCIDR   string
}

// Options configures a Reconciler.
type Options struct {
	Namespace       string
	TLSSecret       string
	AutheliaService string
	AutheliaPort    int
	LocalOnlyCIDR   string
}

// NewReconciler creates a routing reconciler.
func NewReconciler(dyn dynamic.Interface, opts Options) *Reconciler {
	return &Reconciler{
		dyn:             dyn,
		namespace:       opts.Namespace,
		tlsSecret:       opts.TLSSecret,
		autheliaService: opts.AutheliaService,
		autheliaPort:    opts.AutheliaPort,
		localOnlyCIDR:   opts.LocalOnlyCIDR,
	}
}

// SpecFor builds a routing spec from an app record.
func (r *Reconciler) SpecFor(rec apps.Record, baseDomain string) Spec {
	return Spec{
		Name:            rec.Name,
		Namespace:       r.namespace,
		Subdomain:       rec.Exposure.Subdomain,
		BaseDomain:      baseDomain,
		TLS:             rec.Exposure.TLS,
		Auth:            rec.Exposure.Auth,
		LocalOnly:       rec.Exposure.LocalOnly,
		Service:         rec.Exposure.Service,
		Port:            rec.Exposure.Port,
		Scheme:          rec.Exposure.Scheme,
		TLSSecret:       r.tlsSecret,
		AutheliaService: r.autheliaService,
		AutheliaPort:    r.autheliaPort,
		LocalOnlyCIDR:   r.localOnlyCIDR,
	}
}

// Apply renders and server-side-applies an app's route and middlewares. Auth is
// only rendered when the base domain is in the SSO list.
func (r *Reconciler) Apply(ctx context.Context, rec apps.Record, baseDomain string, ssoDomains []string) error {
	spec := r.SpecFor(rec, baseDomain)
	if spec.Auth && !contains(ssoDomains, baseDomain) {
		return fmt.Errorf("base domain %q is not in the SSO domain list; auth is unavailable", baseDomain)
	}
	objects, err := Render(spec)
	if err != nil {
		return err
	}
	if objects == nil {
		// No subdomain: make sure a previously-applied route is gone.
		return r.Delete(ctx, rec.Name)
	}
	for _, obj := range objects {
		if err := r.applyObject(ctx, obj); err != nil {
			return err
		}
	}
	return nil
}

// Delete removes an app's IngressRoute and its per-app ipallowlist middleware.
func (r *Reconciler) Delete(ctx context.Context, name string) error {
	if err := r.deleteObject(ctx, ingressRouteGVR, name); err != nil {
		return err
	}
	return r.deleteObject(ctx, middlewareGVR, name+"-ipallowlist")
}

func (r *Reconciler) applyObject(ctx context.Context, obj *unstructured.Unstructured) error {
	gvr := gvrFor(obj.GetKind())
	if gvr == nil {
		return fmt.Errorf("unknown routing kind %q", obj.GetKind())
	}
	data, err := obj.MarshalJSON()
	if err != nil {
		return err
	}
	_, err = r.dyn.Resource(*gvr).Namespace(r.namespace).Patch(
		ctx, obj.GetName(), types.ApplyPatchType, data,
		metav1.PatchOptions{FieldManager: "naslos-api"},
	)
	if err != nil {
		return fmt.Errorf("applying %s/%s: %w", obj.GetKind(), obj.GetName(), err)
	}
	return nil
}

func (r *Reconciler) deleteObject(ctx context.Context, gvr schema.GroupVersionResource, name string) error {
	err := r.dyn.Resource(gvr).Namespace(r.namespace).Delete(ctx, name, metav1.DeleteOptions{})
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
