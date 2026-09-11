// Package talos provides a client for the Talos Linux API.
package talos

import (
	"context"
	"fmt"

	"github.com/siderolabs/talos/pkg/machinery/client"
	clientconfig "github.com/siderolabs/talos/pkg/machinery/client/config"
)

// Client wraps the Talos API client.
type Client struct {
	client *client.Client
	ctx    context.Context
}

// NewClient creates a new Talos client from a talosconfig file.
// If talosConfig is empty, it uses the default config path (~/.talos/config).
func NewClient(ctx context.Context, talosConfig string) (*Client, error) {
	cfg, err := loadConfig(talosConfig)
	if err != nil {
		return nil, fmt.Errorf("loading talosconfig: %w", err)
	}

	c, err := client.New(ctx,
		client.WithConfig(cfg),
	)
	if err != nil {
		return nil, fmt.Errorf("creating Talos client: %w", err)
	}

	return &Client{
		client: c,
		ctx:    ctx,
	}, nil
}

// loadConfig loads a talosconfig from the given path or the default location.
//
// When path is empty, this delegates to clientconfig.Open("") so the
// machinery library's own default-path resolution runs: it checks the
// TALOSCONFIG environment variable first, then ~/.talos/config, then the
// in-cluster service-account mount (/var/run/secrets/talos.dev/config).
// A previous version of this function hardcoded ~/.talos/config directly,
// which silently skipped the TALOSCONFIG env var and made the API pod fail
// at startup with "failed to determine endpoints" even when a talosconfig
// was mounted and TALOSCONFIG was set.
func loadConfig(path string) (*clientconfig.Config, error) {
	return clientconfig.Open(path)
}

// Close closes the Talos client connection.
func (c *Client) Close() error {
	return c.client.Close()
}

// Context returns the client's context.
func (c *Client) Context() context.Context {
	return c.ctx
}

// Client returns the underlying Talos client.
func (c *Client) Client() *client.Client {
	return c.client
}
