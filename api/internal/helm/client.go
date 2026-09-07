// Package helm provides a Helm SDK wrapper for NasOS app lifecycle management.
package helm

import (
	"fmt"
	"os"
	"path/filepath"
	"time"

	"helm.sh/helm/v3/pkg/action"
	"helm.sh/helm/v3/pkg/cli"
	"k8s.io/cli-runtime/pkg/genericclioptions"
)

// Client provides Helm operations for app management.
type Client struct {
	settings  *cli.EnvSettings
	namespace string
	cacheDir  string
}

// NewClient creates a new Helm client.
func NewClient(namespace string) *Client {
	settings := cli.New()
	settings.SetNamespace(namespace)

	cacheDir := filepath.Join(os.TempDir(), "nasos-helm-cache")
	os.MkdirAll(cacheDir, 0755)
	settings.RepositoryConfig = filepath.Join(cacheDir, "repositories.yaml")
	settings.RepositoryCache = filepath.Join(cacheDir, "repository")

	return &Client{
		settings:  settings,
		namespace: namespace,
		cacheDir:  cacheDir,
	}
}

// getActionConfig creates an action.Configuration for the given namespace.
func (c *Client) getActionConfig(namespace string) (*action.Configuration, error) {
	config := &action.Configuration{}
	clientGetter := &genericclioptions.ConfigFlags{
		Namespace: &namespace,
	}
	if err := config.Init(clientGetter, namespace, "secret", func(format string, v ...interface{}) {}); err != nil {
		return nil, fmt.Errorf("init helm action config: %w", err)
	}
	return config, nil
}

// App represents an installed application.
type App struct {
	Name        string                 `json:"name"`
	Namespace   string                 `json:"namespace"`
	Chart       string                 `json:"chart"`
	Version     string                 `json:"version"`
	Values      map[string]interface{} `json:"values"`
	Status      string                 `json:"status"`
	UpdatedAt   time.Time              `json:"updatedAt"`
	Description string                 `json:"description"`
	Icon        string                 `json:"icon"`
	Category    string                 `json:"category"`
	Ports       []int                  `json:"ports"`
	URL         string                 `json:"url"`
}
