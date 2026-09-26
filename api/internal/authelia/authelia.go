// Package authelia renders the dynamic Authelia configuration fragments (the
// session cookies and the access-control rules) for the effective SSO domain
// list, and pushes them into a ConfigMap the Authelia pod mounts. Authelia reads
// its configuration only at startup and validates access_control as one file, so
// a change also restarts the workload.
package authelia

import (
	"context"
	"fmt"
	"reflect"

	"gopkg.in/yaml.v3"
	apierrors "k8s.io/apimachinery/pkg/api/errors"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/client-go/kubernetes"
)

// Keys are the ConfigMap keys the chart mounts at /config-sso.
const (
	CookiesKey = "cookies.yml"
	RulesKey   = "rules.yml"
)

// cookie is one session.cookies entry: a domain, the portal URL Authelia sends a
// redirected user to, and the default redirection target for that domain. The
// default_redirection_url MUST share a cookie scope with the domain and must
// differ from authelia_url, so it is the domain's own apex; with a non-empty
// `cookies:` list Authelia rejects the legacy global default_redirection_url.
type cookie struct {
	Domain                string `yaml:"domain"`
	AutheliaURL           string `yaml:"authelia_url"`
	DefaultRedirectionURL string `yaml:"default_redirection_url"`
}

// rule is one access_control.rules entry. The dynamic rules are always
// one_factor; the static chart config keeps the primary domain's bypass and
// two_factor rules.
type rule struct {
	Domain string `yaml:"domain"`
	Policy string `yaml:"policy"`
}

// Fragments renders cookies.yml and rules.yml for the effective SSO list. The
// first domain is the primary: its apex rules already come from the static chart
// config, so only non-primary apexes are emitted here, while the wildcard rule is
// emitted for every domain (that is what protects app subdomains).
func Fragments(domains []string) (cookiesYAML, rulesYAML []byte, err error) {
	cookies := make([]cookie, 0, len(domains))
	for _, domain := range domains {
		cookies = append(cookies, cookie{
			Domain:                domain,
			AutheliaURL:           "https://" + domain + "/authelia/",
			DefaultRedirectionURL: "https://" + domain + "/",
		})
	}
	rules := make([]rule, 0, len(domains)*2)
	primary := ""
	if len(domains) > 0 {
		primary = domains[0]
	}
	for _, domain := range domains {
		if domain != primary {
			rules = append(rules, rule{Domain: domain, Policy: "one_factor"})
		}
	}
	for _, domain := range domains {
		rules = append(rules, rule{Domain: "*." + domain, Policy: "one_factor"})
	}
	if cookiesYAML, err = yaml.Marshal(cookies); err != nil {
		return nil, nil, err
	}
	if rulesYAML, err = yaml.Marshal(rules); err != nil {
		return nil, nil, err
	}
	return cookiesYAML, rulesYAML, nil
}

// Reconciler owns the SSO fragment ConfigMap and the Authelia pod restart.
type Reconciler struct {
	client    func() (kubernetes.Interface, error)
	namespace string
	configMap string
	pod       string
}

// Options configures a Reconciler.
type Options struct {
	// Client returns the clientset; it is called on every Sync so a test can
	// inject a fake after construction.
	Client    func() (kubernetes.Interface, error)
	Namespace string
	// ConfigMap is the fragment ConfigMap name (default naslos-authelia-sso).
	ConfigMap string
	// Pod is the Authelia pod to delete so the controller recreates it with the
	// new fragments (default naslos-authelia-0, the first StatefulSet ordinal).
	// Deleting the pod rather than patching the workload keeps the API's write
	// scope to the fragment ConfigMap: it can restart Authelia but cannot change
	// the pod spec (which mounts the jwt/LDAP secrets).
	Pod string
}

// NewReconciler creates an SSO reconciler.
func NewReconciler(opts Options) *Reconciler {
	configMap := opts.ConfigMap
	if configMap == "" {
		configMap = "naslos-authelia-sso"
	}
	pod := opts.Pod
	if pod == "" {
		pod = "naslos-authelia-0"
	}
	return &Reconciler{
		client:    opts.Client,
		namespace: opts.Namespace,
		configMap: configMap,
		pod:       pod,
	}
}

// Sync writes the fragments for domains and restarts Authelia when they changed.
// It is a no-op when the ConfigMap already holds equivalent fragments, so a
// startup reconcile does not interrupt auth.
func (r *Reconciler) Sync(ctx context.Context, domains []string) error {
	cookies, rules, err := Fragments(domains)
	if err != nil {
		return fmt.Errorf("rendering SSO fragments: %w", err)
	}
	client, err := r.client()
	if err != nil {
		return fmt.Errorf("no Kubernetes client for SSO sync: %w", err)
	}
	configMaps := client.CoreV1().ConfigMaps(r.namespace)
	current, err := configMaps.Get(ctx, r.configMap, metav1.GetOptions{})
	if apierrors.IsNotFound(err) {
		// The chart seeds this ConfigMap; do not create it here. Creating it would
		// need an unscoped `create` on configmaps (resourceNames does not apply to
		// create), and Authelia already cannot start without the mounted volume.
		return fmt.Errorf("SSO ConfigMap %s/%s is missing; reinstall the chart to seed it", r.namespace, r.configMap)
	}
	if err != nil {
		return fmt.Errorf("reading SSO ConfigMap: %w", err)
	}
	if fragmentsEqual(current.Data[CookiesKey], cookies) && fragmentsEqual(current.Data[RulesKey], rules) {
		return nil
	}
	updated := current.DeepCopy()
	if updated.Data == nil {
		updated.Data = map[string]string{}
	}
	updated.Data[CookiesKey] = string(cookies)
	updated.Data[RulesKey] = string(rules)
	if _, err := configMaps.Update(ctx, updated, metav1.UpdateOptions{}); err != nil {
		return fmt.Errorf("updating SSO ConfigMap: %w", err)
	}
	return r.restart(ctx, client)
}

// restart deletes the Authelia pod so its controller recreates it against the
// freshly written fragments. A missing pod is not an error: a concurrent restart
// already removed it, or the controller has not created it yet.
func (r *Reconciler) restart(ctx context.Context, client kubernetes.Interface) error {
	err := client.CoreV1().Pods(r.namespace).Delete(ctx, r.pod, metav1.DeleteOptions{})
	if err != nil && !apierrors.IsNotFound(err) {
		return fmt.Errorf("restarting Authelia: %w", err)
	}
	return nil
}

// fragmentsEqual reports whether the stored fragment is semantically the same as
// the desired one. The seed ConfigMap is rendered by Helm, so a byte comparison
// would be brittle; unmarshalling both sides normalises quoting and ordering.
func fragmentsEqual(existing string, desired []byte) bool {
	if existing == "" {
		return false
	}
	var got, want []map[string]string
	if err := yaml.Unmarshal([]byte(existing), &got); err != nil {
		return false
	}
	if err := yaml.Unmarshal(desired, &want); err != nil {
		return false
	}
	return reflect.DeepEqual(got, want)
}
