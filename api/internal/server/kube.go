package server

import (
	"context"
	"os"
	"strings"
	"sync"

	"k8s.io/client-go/dynamic"

	"github.com/AessemOps/Naslos-Linux/api/internal/chartsrepo"
)

var (
	dynamicOnce   sync.Once
	dynamicClient dynamic.Interface
	dynamicErr    error
)

// dynamicKubernetesClient returns a cached dynamic client, for the Traefik and
// cert-manager CRDs.
func (s *Server) dynamicKubernetesClient() (dynamic.Interface, error) {
	dynamicOnce.Do(func() {
		config, err := getKubeConfig(s.kubeconfig)
		if err != nil {
			dynamicErr = err
			return
		}
		dynamicClient, dynamicErr = dynamic.NewForConfig(config)
	})
	return dynamicClient, dynamicErr
}

// serverCredentials resolves chart-repository credentials from Secrets using the
// API's own ServiceAccount.
type serverCredentials struct {
	s         *Server
	defaultNS string
}

// Resolve implements chartsrepo.CredentialsProvider.
func (c serverCredentials) Resolve(ctx context.Context, src *chartsrepo.Source) (chartsrepo.Credentials, error) {
	client, err := c.s.kubernetesClient()
	if err != nil {
		return chartsrepo.Credentials{}, err
	}
	provider := chartsrepo.K8sCredentialsProvider{Client: client, DefaultNamespace: c.defaultNS}
	return provider.Resolve(ctx, src)
}

// getEnvMap parses a comma-separated `key=value` environment variable.
func getEnvMap(key string) map[string]string {
	raw, ok := os.LookupEnv(key)
	if !ok || strings.TrimSpace(raw) == "" {
		return nil
	}
	out := make(map[string]string)
	for _, pair := range strings.Split(raw, ",") {
		name, value, found := strings.Cut(pair, "=")
		name = strings.TrimSpace(name)
		value = strings.TrimSpace(value)
		if !found || name == "" || value == "" {
			continue
		}
		out[name] = value
	}
	return out
}

// getEnvList parses a comma-separated environment variable, trimmed and
// de-duplicated, falling back to the defaults when unset.
func getEnvList(key string, defaults []string) []string {
	raw, ok := os.LookupEnv(key)
	if !ok || raw == "" {
		return defaults
	}
	out := make([]string, 0)
	seen := make(map[string]struct{})
	for _, part := range strings.Split(raw, ",") {
		part = strings.TrimSpace(part)
		if part == "" {
			continue
		}
		if _, dup := seen[part]; dup {
			continue
		}
		seen[part] = struct{}{}
		out = append(out, part)
	}
	if len(out) == 0 {
		return defaults
	}
	return out
}
