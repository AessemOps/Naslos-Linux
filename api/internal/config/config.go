// Package config holds Naslos API server configuration.
package config

// Config is the API server configuration.
type Config struct {
	// ListenAddr is the HTTP listen address (e.g. ":8080").
	ListenAddr string

	// Kubeconfig is the path to a kubeconfig file. Empty means use in-cluster config.
	Kubeconfig string

	// TalosConfig is the path to a talosconfig file. Empty means use default.
	TalosConfig string
}
