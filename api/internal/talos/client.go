// Package talos provides a client for the Talos Linux API.
package talos

import (
	"context"
	"fmt"
	"os"
	"path/filepath"

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
func loadConfig(path string) (*clientconfig.Config, error) {
	if path != "" {
		return clientconfig.Open(path)
	}

	home, err := os.UserHomeDir()
	if err != nil {
		return nil, fmt.Errorf("getting home dir: %w", err)
	}
	return clientconfig.Open(filepath.Join(home, ".talos", "config"))
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
