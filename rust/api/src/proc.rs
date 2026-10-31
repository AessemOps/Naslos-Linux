//! Child-process environment hygiene.
//!
//! The API runs with secrets in its environment (`BUDDY_KEK`,
//! `BUDDY_ENROLL_TOKEN`, `AGENT_TOKEN`, `PROXY_SHARED_SECRET`, the LDAP
//! password file path). By default every forked `helm`/`kubectl`/`talosctl`/
//! `git` inherits all of them. The LDAP password was already moved to a file
//! for this reason (SEC-2026-F16); the rest of the secret material is closed
//! here by clearing the environment and re-adding only what the tools need.

/// Variables a child may inherit. Deliberately excludes every secret-bearing
/// name the API uses.
const ALLOWED: &[&str] = &[
    "PATH",
    "HOME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TZ",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "KUBECONFIG",
    "TALOSCONFIG",
    "HELM_CACHE_HOME",
    "HELM_CONFIG_HOME",
    "HELM_DATA_HOME",
    "HELM_PLUGINS",
    "XDG_CACHE_HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
];

/// The allowlist, for callers that build their own command type (the PTY).
pub fn allowlist() -> &'static [&'static str] {
    ALLOWED
}

/// Whether a variable may be inherited by a child process.
pub fn allowed(key: &str) -> bool {
    ALLOWED.contains(&key)
}

/// Clear the child's environment and re-add the allowlist from the API's own
/// environment. Call this **before** any command-specific `.env(...)` calls so
/// those are not dropped.
pub fn scrub(cmd: &mut tokio::process::Command) {
    cmd.env_clear();
    for key in ALLOWED {
        if let Ok(value) = std::env::var(key) {
            if !value.is_empty() {
                cmd.env(key, value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::allowed;

    #[test]
    fn secrets_are_not_inherited() {
        for key in [
            "BUDDY_KEK",
            "BUDDY_ENROLL_TOKEN",
            "AGENT_TOKEN",
            "PROXY_SHARED_SECRET",
            "LDAP_BIND_PASS",
            "LDAP_BIND_PASS_FILE",
            "NASLOS_ADMIN_PASSWORD",
        ] {
            assert!(!allowed(key), "{key} must not reach a child");
        }
    }

    #[test]
    fn tool_inputs_are_inherited() {
        for key in ["PATH", "HOME", "KUBECONFIG", "TALOSCONFIG", "HTTPS_PROXY"] {
            assert!(allowed(key), "{key} is needed by the tools");
        }
    }
}
