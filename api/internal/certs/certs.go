// Package certs manages base domains and their cert-manager ACME DNS-01
// certificates. Domain records are persisted as JSON; the Issuer and wildcard
// Certificate CRs are rendered by the API and written with the dynamic client.
package certs

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	apierrors "k8s.io/apimachinery/pkg/api/errors"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/apis/meta/v1/unstructured"
	"k8s.io/apimachinery/pkg/runtime/schema"
	"k8s.io/apimachinery/pkg/types"
	"k8s.io/client-go/dynamic"

	"github.com/AessemOps/Naslos-Linux/api/internal/providers"
)

var (
	certificateGVR = schema.GroupVersionResource{Group: "cert-manager.io", Version: "v1", Resource: "certificates"}
	issuerGVR      = schema.GroupVersionResource{Group: "cert-manager.io", Version: "v1", Resource: "issuers"}
)

// dns1123Subdomain matches a DNS name / subdomain.
var dns1123Subdomain = regexp.MustCompile(`^[a-z0-9]([-a-z0-9]*[a-z0-9])?(\.[a-z0-9]([-a-z0-9]*[a-z0-9])?)*$`)

// Provider is the DNS-01 challenge provider. The set of valid values is the
// provider registry (built-in YAML plus an optional override directory); these
// constants remain for the built-ins so existing records and tests keep working.
type Provider string

const (
	ProviderCloudflare  Provider = "cloudflare"
	ProviderRFC2136     Provider = "rfc2136"
	ProviderPassthrough Provider = "passthrough"
	ProviderOVH         Provider = "ovh"
)

// providerRegistry is the registry used to validate domains and render solvers.
// It is the embedded set by default and is replaced once at startup with the
// server's registry (which also reads the override directory).
var providerRegistry atomic.Pointer[providers.Registry]

func init() {
	providerRegistry.Store(providers.Load(""))
}

// ConfigureRegistry replaces the provider registry used by certificates. It is
// called once at startup, before the API serves requests.
func ConfigureRegistry(r *providers.Registry) {
	if r != nil {
		providerRegistry.Store(r)
	}
}

func registry() *providers.Registry {
	return providerRegistry.Load()
}

// Domain is a base domain with an optional ACME certificate.
type Domain struct {
	BaseDomain string `json:"baseDomain"`
	// DNSProvider names a provider from the registry (e.g. cloudflare, ovh,
	// rfc2136, passthrough).
	DNSProvider Provider `json:"dnsProvider"`
	// CredentialsSecret names the Secret holding provider credentials.
	CredentialsSecret string `json:"credentialsSecret,omitempty"`
	// ProviderConfig holds a provider's non-secret fields (e.g. OVH's endpoint
	// and applicationKey).
	ProviderConfig map[string]string `json:"providerConfig,omitempty"`
	// Solver is the raw cert-manager DNS-01 solver for passthrough providers.
	Solver map[string]interface{} `json:"solver,omitempty"`
	// ACMEEmail is the ACME account email.
	ACMEEmail string `json:"acmeEmail,omitempty"`
	// Environment is staging or production.
	Environment string `json:"environment"`
	// Primary marks the Helm-owned domain serving UI + Authelia.
	Primary bool `json:"primary,omitempty"`
	// SSO marks a non-primary domain promoted to the Authelia SSO list at
	// runtime (the Domains page toggle). The primary domain is always SSO.
	SSO bool `json:"sso,omitempty"`

	CreatedAt time.Time `json:"createdAt"`
	UpdatedAt time.Time `json:"updatedAt"`
	LastError string    `json:"lastError,omitempty"`
}

// Validate checks the domain record.
func (d *Domain) Validate() error {
	if !dns1123Subdomain.MatchString(d.BaseDomain) {
		return fmt.Errorf("base domain %q is not a valid DNS name", d.BaseDomain)
	}
	p, ok := registry().Get(string(d.DNSProvider))
	if !ok {
		return fmt.Errorf("unsupported DNS provider %q", d.DNSProvider)
	}
	switch {
	case p.HasCertManager():
		if d.CredentialsSecret == "" {
			return fmt.Errorf("%s requires a credentials secret", p.Name)
		}
	case p.IsPassthrough():
		if len(d.Solver) == 0 {
			return fmt.Errorf("passthrough requires a solver")
		}
	default:
		return fmt.Errorf("provider %q does not support certificates", p.Name)
	}
	switch d.Environment {
	case "", "staging":
		d.Environment = "staging"
	case "production":
	default:
		return fmt.Errorf("environment must be staging or production")
	}
	return nil
}

// SecretName is the TLS secret the wildcard Certificate writes into.
func (d *Domain) SecretName() string {
	name := regexp.MustCompile(`[^a-z0-9-]`).ReplaceAllString(d.BaseDomain, "-")
	return "naslos-" + name + "-tls"
}

// IssuerName is the ClusterIssuer/Issuer name for the domain.
func (d *Domain) IssuerName() string {
	return "naslos-acme-" + regexp.MustCompile(`[^a-z0-9-]`).ReplaceAllString(d.BaseDomain, "-")
}

// Spec renders the Issuer and wildcard Certificate for a domain.
func Spec(d Domain, namespace string) (*unstructured.Unstructured, *unstructured.Unstructured, error) {
	if err := d.Validate(); err != nil {
		return nil, nil, err
	}
	server := "https://acme-staging-v02.api.letsencrypt.org/directory"
	if d.Environment == "production" {
		server = "https://acme-v02.api.letsencrypt.org/directory"
	}

	solver, err := solverFor(d)
	if err != nil {
		return nil, nil, err
	}

	issuer := &unstructured.Unstructured{Object: map[string]interface{}{
		"apiVersion": "cert-manager.io/v1",
		"kind":       "Issuer",
		"metadata": map[string]interface{}{
			"name":      d.IssuerName(),
			"namespace": namespace,
		},
		"spec": map[string]interface{}{
			"acme": map[string]interface{}{
				"server": server,
				"email":  d.ACMEEmail,
				"privateKeySecretRef": map[string]interface{}{
					"name": d.IssuerName() + "-account",
				},
				"solvers": []interface{}{map[string]interface{}{"dns01": solver}},
			},
		},
	}}

	certificate := &unstructured.Unstructured{Object: map[string]interface{}{
		"apiVersion": "cert-manager.io/v1",
		"kind":       "Certificate",
		"metadata": map[string]interface{}{
			"name":      d.SecretName(),
			"namespace": namespace,
		},
		"spec": map[string]interface{}{
			"secretName": d.SecretName(),
			"issuerRef": map[string]interface{}{
				"name":  d.IssuerName(),
				"kind":  "Issuer",
				"group": "cert-manager.io",
			},
			"dnsNames": []interface{}{d.BaseDomain, "*." + d.BaseDomain},
		},
	}}
	return issuer, certificate, nil
}

func solverFor(d Domain) (map[string]interface{}, error) {
	p, ok := registry().Get(string(d.DNSProvider))
	if !ok {
		return nil, fmt.Errorf("unsupported DNS provider %q", d.DNSProvider)
	}
	if p.IsPassthrough() {
		return d.Solver, nil
	}
	if !p.HasCertManager() {
		return nil, fmt.Errorf("provider %q does not support certificates", p.Name)
	}
	return p.Solver(d.CredentialsSecret, d.ProviderConfig)
}

// Store persists domain records.
type Store struct {
	path    string
	mu      sync.Mutex
	domains map[string]Domain
}

// NewStore creates a domain store.
func NewStore(path string) *Store {
	return &Store{path: path, domains: make(map[string]Domain)}
}

// Load reads the domains file.
func (s *Store) Load() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.path == "" {
		return nil
	}
	data, err := os.ReadFile(s.path)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return fmt.Errorf("reading domains file: %w", err)
	}
	var list []Domain
	if err := json.Unmarshal(data, &list); err != nil {
		return fmt.Errorf("parsing domains file: %w", err)
	}
	for _, d := range list {
		if d.BaseDomain == "" {
			continue
		}
		s.domains[d.BaseDomain] = d
	}
	return nil
}

// List returns domains ordered by name.
func (s *Store) List() []Domain {
	s.mu.Lock()
	defer s.mu.Unlock()
	out := make([]Domain, 0, len(s.domains))
	for _, d := range s.domains {
		out = append(out, d)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].BaseDomain < out[j].BaseDomain })
	return out
}

// Get returns a domain.
func (s *Store) Get(name string) (Domain, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	d, ok := s.domains[name]
	if !ok {
		return Domain{}, fmt.Errorf("domain %q not found", name)
	}
	return d, nil
}

// Upsert stores a domain.
func (s *Store) Upsert(d Domain) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.domains[d.BaseDomain] = d
	return s.saveLocked()
}

// Delete removes a domain.
func (s *Store) Delete(name string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if _, ok := s.domains[name]; !ok {
		return fmt.Errorf("domain %q not found", name)
	}
	delete(s.domains, name)
	return s.saveLocked()
}

// Update applies a mutation and persists.
func (s *Store) Update(name string, mutate func(*Domain)) (Domain, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	d, ok := s.domains[name]
	if !ok {
		return Domain{}, fmt.Errorf("domain %q not found", name)
	}
	mutate(&d)
	s.domains[name] = d
	return d, s.saveLocked()
}

func (s *Store) saveLocked() error {
	if s.path == "" {
		return nil
	}
	list := make([]Domain, 0, len(s.domains))
	for _, d := range s.domains {
		list = append(list, d)
	}
	sort.Slice(list, func(i, j int) bool { return list[i].BaseDomain < list[j].BaseDomain })
	data, err := json.MarshalIndent(list, "", "  ")
	if err != nil {
		return fmt.Errorf("encoding domains: %w", err)
	}
	if dir := filepath.Dir(s.path); dir != "" && dir != "." {
		if err := os.MkdirAll(dir, 0750); err != nil {
			return err
		}
	}
	tmp, err := os.CreateTemp(filepath.Dir(s.path), ".domains-*.tmp")
	if err != nil {
		return err
	}
	tmpName := tmp.Name()
	defer os.Remove(tmpName)
	if _, err := tmp.Write(append(data, '\n')); err != nil {
		tmp.Close()
		return err
	}
	if err := tmp.Close(); err != nil {
		return err
	}
	if err := os.Chmod(tmpName, 0600); err != nil {
		return err
	}
	return os.Rename(tmpName, s.path)
}

// Reconciler applies domain certificates to the cluster.
type Reconciler struct {
	dyn       dynamic.Interface
	namespace string
}

// NewReconciler creates a certs reconciler.
func NewReconciler(dyn dynamic.Interface, namespace string) *Reconciler {
	return &Reconciler{dyn: dyn, namespace: namespace}
}

// Available reports whether the cert-manager CRDs are installed.
func (r *Reconciler) Available(ctx context.Context) bool {
	_, err := r.dyn.Resource(certificateGVR).Namespace(r.namespace).List(ctx, metav1.ListOptions{Limit: 1})
	return err == nil || !apierrors.IsNotFound(err) && !isNoMatch(err)
}

// Apply renders and applies a domain's Issuer and Certificate.
func (r *Reconciler) Apply(ctx context.Context, d Domain) error {
	issuer, certificate, err := Spec(d, r.namespace)
	if err != nil {
		return err
	}
	for _, obj := range []*unstructured.Unstructured{issuer, certificate} {
		if err := r.applyObject(ctx, obj); err != nil {
			return err
		}
	}
	return nil
}

// Delete removes a domain's Issuer and Certificate.
func (r *Reconciler) Delete(ctx context.Context, d Domain) error {
	if err := r.deleteObject(ctx, certificateGVR, d.SecretName()); err != nil {
		return err
	}
	return r.deleteObject(ctx, issuerGVR, d.IssuerName())
}

// Status reports the readiness condition of a domain's Certificate.
func (r *Reconciler) Status(ctx context.Context, d Domain) (map[string]interface{}, error) {
	obj, err := r.dyn.Resource(certificateGVR).Namespace(r.namespace).Get(ctx, d.SecretName(), metav1.GetOptions{})
	if err != nil {
		if apierrors.IsNotFound(err) {
			return map[string]interface{}{"status": "pending"}, nil
		}
		return nil, err
	}
	conditions, _, _ := unstructured.NestedSlice(obj.Object, "status", "conditions")
	state := "pending"
	for _, c := range conditions {
		cond, ok := c.(map[string]interface{})
		if !ok {
			continue
		}
		if cond["type"] == "Ready" {
			if cond["status"] == "True" {
				state = "ready"
			} else {
				state = "not-ready"
			}
		}
	}
	return map[string]interface{}{
		"status":     state,
		"conditions": conditions,
		"secretName": d.SecretName(),
	}, nil
}

func (r *Reconciler) applyObject(ctx context.Context, obj *unstructured.Unstructured) error {
	gvr := certificateGVR
	if obj.GetKind() == "Issuer" {
		gvr = issuerGVR
	}
	data, err := obj.MarshalJSON()
	if err != nil {
		return err
	}
	_, err = r.dyn.Resource(gvr).Namespace(r.namespace).Patch(
		ctx, obj.GetName(), types.ApplyPatchType, data,
		metav1.PatchOptions{FieldManager: "naslos-api"},
	)
	return err
}

func (r *Reconciler) deleteObject(ctx context.Context, gvr schema.GroupVersionResource, name string) error {
	err := r.dyn.Resource(gvr).Namespace(r.namespace).Delete(ctx, name, metav1.DeleteOptions{})
	if err != nil && !apierrors.IsNotFound(err) {
		return err
	}
	return nil
}

func isNoMatch(err error) bool {
	return err != nil && (strings.Contains(err.Error(), "no matches for kind") ||
		strings.Contains(err.Error(), "the server could not find the requested resource"))
}
