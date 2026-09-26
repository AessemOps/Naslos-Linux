// Package helm provides a Helm SDK wrapper for Naslos app lifecycle management.
package helm

import (
	"fmt"
	"time"

	"helm.sh/helm/v3/pkg/action"
	"k8s.io/cli-runtime/pkg/genericclioptions"
)

// Client provides Helm operations for app management, scoped to one namespace.
type Client struct {
	namespace string
}

// NewClient creates a new Helm client for a namespace.
func NewClient(namespace string) *Client {
	return &Client{namespace: namespace}
}

// Namespace returns the namespace the client operates in.
func (c *Client) Namespace() string { return c.namespace }

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
