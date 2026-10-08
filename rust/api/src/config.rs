//! Package config holds Naslos API server configuration (port of
//! `api/internal/config`).

/// The API server configuration.
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// The HTTP listen address (e.g. `:8080`).
    pub listen_addr: String,
    /// Path to a kubeconfig file. Empty means use in-cluster config.
    pub kubeconfig: String,
    /// Path to a talosconfig file. Empty means use the default resolution.
    pub talosconfig: String,
}
