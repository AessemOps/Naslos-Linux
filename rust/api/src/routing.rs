//! Traefik exposure layer for installed apps (port of `api/internal/routing`).
//! One IngressRoute per app plus the middlewares it references, rendered in the
//! platform apps namespace (next to the domain TLS Secrets) and targeting the
//! app Service in its own namespace. The per-app Internet-egress NetworkPolicy
//! stays in the app namespace.

use std::sync::Arc;

use crate::apps::{DiscoveredService, Record, Router};

pub const FORWARD_AUTH_NAME: &str = "naslos-app-forwardauth-authelia";
pub const SECURITY_HEADERS_NAME: &str = "naslos-app-security-headers";
/// Deletes the parent-domain Authelia session cookie before the app backend
/// sees it (R-003). The plugin is shipped by the chart as a Traefik local
/// plugin; the middleware is rendered next to the route.
pub const STRIP_AUTH_COOKIE_NAME: &str = "naslos-app-strip-authelia-cookie";
/// Key of the chart-shipped Traefik local plugin used by the middleware.
pub const STRIP_AUTH_COOKIE_PLUGIN: &str = "naslos-cookie-strip";
pub const PORTAL_EXTERNAL_SERVICE_NAME: &str = "naslos-authelia-portal";
pub const PORTAL_LABEL_KEY: &str = "naslos.local/sso-portal";

/// Everything needed to render one app's route.
#[derive(Debug, Clone, Default)]
pub struct Spec {
    pub name: String,
    /// Namespace the app's workloads run in (the Service target).
    pub namespace: String,
    /// Namespace the API's IngressRoute and middlewares are written to. It is
    /// the platform apps namespace, where the domain TLS Secrets live, so the
    /// wildcard private key is never copied into an app namespace (R-002).
    /// Empty means "same as `namespace`".
    pub route_namespace: String,
    pub subdomain: String,
    pub base_domain: String,
    pub tls: bool,
    pub auth: bool,
    pub local_only: bool,
    /// Path prefixes that require forwardAuth even when `auth` is false.
    pub protected_paths: Vec<String>,
    /// Whether the app may reach the public Internet. The base namespace
    /// policy grants only DNS + app-to-app; this adds an additive allow.
    pub internet: bool,
    pub service: String,
    pub port: i64,
    pub scheme: String,
    pub tls_secret: String,
    pub authelia_service: String,
    pub authelia_port: i64,
    pub authelia_namespace: String,
    pub local_only_cidr: String,
}

impl Spec {
    /// The fully-qualified host, or `""` when there is no subdomain.
    pub fn host(&self) -> String {
        if self.subdomain.is_empty() || self.base_domain.is_empty() {
            String::new()
        } else {
            format!("{}.{}", self.subdomain, self.base_domain)
        }
    }

    /// The namespace that holds the route and its middlewares.
    pub fn route_namespace(&self) -> &str {
        if self.route_namespace.is_empty() {
            &self.namespace
        } else {
            &self.route_namespace
        }
    }
}

/// Validate the path prefixes that may be guarded by forwardAuth. They are
/// interpolated into a Traefik rule, so only a conservative path alphabet is
/// allowed.
pub fn validate_protected_paths(paths: &[String]) -> Result<(), String> {
    for path in paths {
        if !path.starts_with('/') || path.len() < 2 {
            return Err(format!(
                "protected path {path:?} must be a path starting with '/'"
            ));
        }
        if !path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '.' | '~'))
        {
            return Err(format!(
                "protected path {path:?} contains characters that are not allowed"
            ));
        }
    }
    Ok(())
}

/// Render the IngressRoute and middlewares for a spec.
pub fn render(spec: &Spec) -> Result<Vec<serde_json::Value>, String> {
    if spec.host().is_empty() {
        return Ok(Vec::new());
    }
    if spec.service.is_empty() || spec.port <= 0 {
        return Err(format!(
            "app {:?} has no route target service/port",
            spec.name
        ));
    }
    validate_protected_paths(&spec.protected_paths)?;
    let scheme = if spec.scheme.is_empty() {
        "http"
    } else {
        &spec.scheme
    };

    // A public app can still have specific paths behind Authelia. `auth`
    // protects the whole host; `protected_paths` protects only those prefixes
    // and leaves search/static assets open.
    let forward_auth = spec.auth || !spec.protected_paths.is_empty();
    if forward_auth && spec.authelia_service.is_empty() {
        return Err(format!(
            "app {:?} requests auth but no Authelia service is configured",
            spec.name
        ));
    }
    if spec.local_only && spec.local_only_cidr.is_empty() {
        return Err(format!(
            "app {:?} is local-only but no LAN CIDR is configured",
            spec.name
        ));
    }

    let route_ns = spec.route_namespace().to_string();
    let security = serde_json::json!({
        "name": SECURITY_HEADERS_NAME, "namespace": route_ns
    });
    let allowlist = serde_json::json!({
        "name": format!("{}-ipallowlist", spec.name), "namespace": route_ns
    });
    let strip_cookie = serde_json::json!({
        "name": STRIP_AUTH_COOKIE_NAME, "namespace": route_ns
    });

    let mut public_middlewares = vec![security.clone()];
    if spec.auth {
        public_middlewares.push(serde_json::json!({
            "name": FORWARD_AUTH_NAME, "namespace": route_ns
        }));
    }
    if spec.local_only {
        public_middlewares.push(allowlist.clone());
    }
    // Always last: forwardAuth must still see the session cookie, and the app
    // backend must never see it (R-003).
    public_middlewares.push(strip_cookie.clone());

    let entry_point = if spec.tls { "websecure" } else { "web" };
    let mut objects = vec![security_headers_middleware(spec)];
    if forward_auth {
        objects.push(forward_auth_middleware(spec));
    }
    if spec.local_only {
        objects.push(ip_allowlist_middleware(spec));
    }
    objects.push(strip_cookie_middleware(spec));

    let mut service = serde_json::json!({
        "name": spec.service, "port": spec.port, "scheme": scheme
    });
    if spec.namespace != route_ns {
        // The route lives in the platform apps namespace, where the domain TLS
        // Secret was issued; Traefik proxies to the app's Service in its
        // isolated namespace (allowCrossNamespace). The wildcard private key is
        // never written into the app namespace (R-002).
        service["namespace"] = serde_json::json!(spec.namespace);
    }
    let mut routes = Vec::new();

    if !spec.auth && !spec.protected_paths.is_empty() {
        let paths = spec
            .protected_paths
            .iter()
            .map(|path| format!("PathPrefix(`{path}`)"))
            .collect::<Vec<_>>()
            .join(" || ");
        let mut protected_middlewares = vec![
            security,
            serde_json::json!({ "name": FORWARD_AUTH_NAME, "namespace": route_ns }),
        ];
        if spec.local_only {
            protected_middlewares.push(allowlist);
        }
        protected_middlewares.push(strip_cookie);
        routes.push(serde_json::json!({
            "match": format!("Host(`{}`) && ({paths})", spec.host()),
            "kind": "Rule",
            // Explicit priority: the public catch-all must not shadow the
            // protected prefix route.
            "priority": 100,
            "middlewares": protected_middlewares,
            "services": [service.clone()],
        }));
    }

    routes.push(serde_json::json!({
        "match": format!("Host(`{}`)", spec.host()),
        "kind": "Rule",
        "priority": 1,
        "middlewares": public_middlewares,
        "services": [service],
    }));

    let mut route_spec = serde_json::json!({
        "entryPoints": [entry_point],
        "routes": routes,
    });
    if spec.tls {
        // With no domain-specific Secret the route still requests TLS and
        // Traefik serves its default certificate; nothing secret is copied.
        if spec.tls_secret.is_empty() {
            route_spec["tls"] = serde_json::json!({});
        } else {
            route_spec["tls"] = serde_json::json!({ "secretName": spec.tls_secret });
        }
    }
    objects.push(serde_json::json!({
        "apiVersion": "traefik.io/v1alpha1",
        "kind": "IngressRoute",
        "metadata": { "name": spec.name, "namespace": route_ns },
        "spec": route_spec,
    }));
    Ok(objects)
}

/// Name of the per-app Internet-egress NetworkPolicy.
pub fn internet_policy_name(name: &str) -> String {
    format!("{name}-internet")
}

/// Label marking a policy the API created, so a sweep can find and remove one
/// a failed install left behind.
pub const MANAGED_BY_LABEL: &str = "naslos.local/managed-by";
/// Value of `MANAGED_BY_LABEL` on API-created policies.
pub const MANAGED_BY_VALUE: &str = "naslos-api";
/// App (release) the policy belongs to.
pub const APP_LABEL: &str = "naslos.local/app";

/// The additive Egress policy that grants public Internet access. It never
/// opens the cluster, LAN or link-local ranges, so an app cannot use it to
/// reach the API, kubelet, the host or a metadata service. It also repeats the
/// base DNS and app-to-app allows so it stays correct where the chart's base
/// policies are disabled (`networkPolicy.enabled=false`).
pub fn internet_policy(spec: &Spec) -> serde_json::Value {
    internet_policy_for(&spec.name, &spec.namespace)
}

/// The same policy built from the release name and namespace alone, so the API
/// can grant egress before the Helm release (and its Record) exists.
pub fn internet_policy_for(name: &str, namespace: &str) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "networking.k8s.io/v1",
        "kind": "NetworkPolicy",
        "metadata": {
            "name": internet_policy_name(name),
            "namespace": namespace,
            "labels": {
                MANAGED_BY_LABEL: MANAGED_BY_VALUE,
                APP_LABEL: name,
            },
        },
        "spec": {
            "podSelector": {},
            "policyTypes": ["Egress"],
            "egress": [
                {
                    "to": [
                        { "ipBlock": { "cidr": "0.0.0.0/0", "except": [
                            "10.0.0.0/8",
                            "172.16.0.0/12",
                            "192.168.0.0/16",
                            "169.254.0.0/16"
                        ]}},
                        { "ipBlock": { "cidr": "::/0", "except": [
                            "fc00::/7",
                            "fe80::/10"
                        ]}}
                    ]
                },
                {
                    "to": [{ "namespaceSelector": { "matchLabels": {
                        "kubernetes.io/metadata.name": "kube-system"
                    }}}],
                    "ports": [
                        { "port": 53, "protocol": "UDP" },
                        { "port": 53, "protocol": "TCP" }
                    ]
                },
                {
                    "to": [{ "namespaceSelector": { "matchLabels": {
                        "naslos.local/apps": "true"
                    }}}]
                }
            ]
        }
    })
}

fn security_headers_middleware(spec: &Spec) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "traefik.io/v1alpha1",
        "kind": "Middleware",
        "metadata": { "name": SECURITY_HEADERS_NAME, "namespace": spec.route_namespace() },
        "spec": { "headers": {
            "browserXssFilter": true,
            "contentTypeNosniff": true,
            "frameDeny": true,
            "customFrameOptionsValue": "DENY",
            "referrerPolicy": "strict-origin-when-cross-origin",
            "stsSeconds": 31536000,
            // This middleware is first on every app route. Removing client
            // supplied identity headers means an app reached with auth=false
            // cannot be told who the browser claims to be; with auth=true,
            // forwardAuth's authResponseHeaders then set the real values.
            "customRequestHeaders": {
                "Remote-User": "",
                "Remote-Groups": "",
                "Remote-Email": "",
                "Remote-Name": "",
            },
        }},
    })
}

fn forward_auth_middleware(spec: &Spec) -> serde_json::Value {
    let namespace = if spec.authelia_namespace.is_empty() {
        &spec.namespace
    } else {
        &spec.authelia_namespace
    };
    let address = format!(
        "http://{}.{}.svc.cluster.local:{}/api/authz/forward-auth?authelia_url=https://{}/authelia/",
        spec.authelia_service, namespace, spec.authelia_port, spec.base_domain
    );
    serde_json::json!({
        "apiVersion": "traefik.io/v1alpha1",
        "kind": "Middleware",
        "metadata": { "name": FORWARD_AUTH_NAME, "namespace": spec.route_namespace() },
        "spec": { "forwardAuth": {
            "address": address,
            "authResponseHeaders": ["Remote-User", "Remote-Groups", "Remote-Email", "Remote-Name"],
        }},
    })
}

fn strip_cookie_middleware(spec: &Spec) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "traefik.io/v1alpha1",
        "kind": "Middleware",
        "metadata": { "name": STRIP_AUTH_COOKIE_NAME, "namespace": spec.route_namespace() },
        "spec": { "plugin": { STRIP_AUTH_COOKIE_PLUGIN: {} } },
    })
}

fn ip_allowlist_middleware(spec: &Spec) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "traefik.io/v1alpha1",
        "kind": "Middleware",
        "metadata": { "name": format!("{}-ipallowlist", spec.name), "namespace": spec.route_namespace() },
        "spec": { "ipAllowList": { "sourceRange": [spec.local_only_cidr] } },
    })
}

/// Resolves the TLS Secret name for a base domain.
pub type TlsSecretResolver = Arc<dyn Fn(&str) -> String + Send + Sync>;

/// Options for the routing reconciler.
#[derive(Clone, Default)]
pub struct Options {
    pub namespace: String,
    pub tls_secret: String,
    pub authelia_service: String,
    pub authelia_port: i64,
    pub authelia_namespace: String,
    pub local_only_cidr: String,
    /// Resolves the TLS Secret for a base domain (the wildcard Certificate's
    /// secretName). When absent, `tls_secret` is used.
    pub tls_secret_for: Option<TlsSecretResolver>,
}

/// A routing reconciler over the kube adapter.
pub struct KubeRouter {
    kube: Arc<crate::kube::Client>,
    opts: Options,
}

impl KubeRouter {
    pub fn new(kube: Arc<crate::kube::Client>, opts: Options) -> Self {
        Self { kube, opts }
    }

    fn spec_for(&self, rec: &Record, namespace: &str, base_domain: &str) -> Spec {
        let namespace = if namespace.is_empty() {
            self.opts.namespace.clone()
        } else {
            namespace.to_string()
        };
        // The domain record is authoritative: its wildcard Secret is in the
        // platform apps namespace, beside the route. With no record the route
        // asks for TLS and Traefik serves its default certificate; nothing is
        // copied from another namespace (R-002).
        let tls_secret = match &self.opts.tls_secret_for {
            Some(resolver) => resolver(base_domain),
            None => self.opts.tls_secret.clone(),
        };
        Spec {
            name: rec.name.clone(),
            namespace,
            route_namespace: self.opts.namespace.clone(),
            subdomain: rec.exposure.subdomain.clone(),
            base_domain: base_domain.to_string(),
            tls: rec.exposure.tls,
            auth: rec.exposure.auth,
            local_only: rec.exposure.local_only,
            protected_paths: rec.exposure.protected_paths.clone(),
            internet: rec.exposure.internet,
            service: rec.exposure.service.clone(),
            port: rec.exposure.port,
            scheme: rec.exposure.scheme.clone(),
            tls_secret,
            authelia_service: self.opts.authelia_service.clone(),
            authelia_port: self.opts.authelia_port,
            authelia_namespace: self.opts.authelia_namespace.clone(),
            local_only_cidr: self.opts.local_only_cidr.clone(),
        }
    }

    async fn apply_object(&self, obj: &serde_json::Value) -> Result<(), String> {
        let kind = obj.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        let name = obj
            .get("metadata")
            .and_then(|m| m.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        self.kube
            .apply(obj)
            .await
            .map_err(|e| format!("applying {kind}/{name}: {e}"))
    }

    /// Reconcile the Authelia portal routes for non-primary SSO domains.
    pub async fn sync_portals(&self, domains: &[String], primary: &str) -> Result<(), String> {
        if self.opts.authelia_service.is_empty() {
            return Ok(());
        }
        let mut want = std::collections::HashSet::new();
        let mut errs = Vec::new();
        for domain in domains {
            if domain.is_empty() || domain == primary {
                continue;
            }
            let secret = self
                .opts
                .tls_secret_for
                .as_ref()
                .map(|f| f(domain))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| self.opts.tls_secret.clone());
            for obj in self.portal_objects(domain, &secret) {
                if let Err(e) = self.apply_object(&obj).await {
                    errs.push(e);
                }
            }
            want.insert(portal_name(domain));
        }
        if want.is_empty() {
            let _ = self
                .kube
                .delete(
                    "service",
                    PORTAL_EXTERNAL_SERVICE_NAME,
                    &self.opts.namespace,
                )
                .await;
        }
        // Prune portal routes that are no longer wanted.
        match self
            .kube
            .list_json(
                "ingressroutes",
                &self.opts.namespace,
                Some(&format!("{PORTAL_LABEL_KEY}=true")),
            )
            .await
        {
            Ok(list) => {
                if let Some(items) = list.get("items").and_then(|v| v.as_array()) {
                    for item in items {
                        if let Some(name) = item
                            .get("metadata")
                            .and_then(|m| m.get("name"))
                            .and_then(|v| v.as_str())
                        {
                            if !want.contains(name) {
                                if let Err(e) = self
                                    .kube
                                    .delete("ingressroute", name, &self.opts.namespace)
                                    .await
                                {
                                    errs.push(e);
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => errs.push(format!("listing portal routes: {e}")),
        }
        if errs.is_empty() {
            Ok(())
        } else {
            Err(errs.join("; "))
        }
    }

    fn portal_objects(&self, domain: &str, tls_secret: &str) -> Vec<serde_json::Value> {
        let external_name = format!(
            "{}.{}.svc.cluster.local",
            self.opts.authelia_service, self.opts.authelia_namespace
        );
        let labels = serde_json::json!({ PORTAL_LABEL_KEY: "true" });
        let svc = serde_json::json!({
            "apiVersion": "v1",
            "kind": "Service",
            "metadata": {
                "name": PORTAL_EXTERNAL_SERVICE_NAME,
                "namespace": self.opts.namespace,
                "labels": labels,
            },
            "spec": {
                "type": "ExternalName",
                "externalName": external_name,
                "ports": [{ "name": "http", "port": self.opts.authelia_port, "targetPort": self.opts.authelia_port, "protocol": "TCP" }],
            },
        });
        let mut spec = serde_json::json!({
            "entryPoints": ["websecure"],
            "routes": [{
                "match": format!("Host(`{domain}`) && PathPrefix(`/authelia`)"),
                "kind": "Rule",
                "services": [{ "name": PORTAL_EXTERNAL_SERVICE_NAME, "port": self.opts.authelia_port }],
            }],
        });
        if !tls_secret.is_empty() {
            spec["tls"] = serde_json::json!({ "secretName": tls_secret });
        }
        let route = serde_json::json!({
            "apiVersion": "traefik.io/v1alpha1",
            "kind": "IngressRoute",
            "metadata": { "name": portal_name(domain), "namespace": self.opts.namespace, "labels": labels },
            "spec": spec,
        });
        vec![svc, route]
    }
}

#[async_trait::async_trait]
impl Router for KubeRouter {
    async fn reconcile_portals(&self, domains: &[String], primary: &str) -> Result<(), String> {
        self.sync_portals(domains, primary).await
    }

    async fn apply(
        &self,
        rec: &Record,
        namespace: &str,
        base_domain: &str,
        sso_domains: &[String],
    ) -> Result<(), String> {
        let spec = self.spec_for(rec, namespace, base_domain);
        let mut errs = Vec::new();
        // Internet access is independent of routing: an app can be
        // cluster-internal and still need outbound access. A policy failure
        // must not stop the route from converging (and vice versa).
        if spec.internet {
            if let Err(e) = self.apply_object(&internet_policy(&spec)).await {
                errs.push(e);
            }
        } else if let Err(e) = self
            .kube
            .delete(
                "networkpolicy",
                &internet_policy_name(&rec.name),
                &spec.namespace,
            )
            .await
        {
            errs.push(e);
        }
        if spec.auth && !sso_domains.iter().any(|d| d == base_domain) {
            errs.push(format!(
                "base domain {base_domain:?} is not in the SSO domain list; auth is unavailable"
            ));
        } else {
            match render(&spec) {
                Ok(objects) if objects.is_empty() => {
                    if let Err(e) = self.delete_route(&spec, &rec.name).await {
                        errs.push(e);
                    }
                }
                Ok(objects) => {
                    // No TLS Secret is copied: the route lives in the platform
                    // apps namespace, next to the Secret cert-manager issued
                    // for the base domain (R-002).
                    for obj in &objects {
                        if let Err(e) = self.apply_object(obj).await {
                            errs.push(e);
                        }
                    }
                }
                Err(e) => errs.push(e),
            }
        }
        if errs.is_empty() {
            Ok(())
        } else {
            Err(errs.join("; "))
        }
    }

    async fn ensure_internet(&self, name: &str, namespace: &str) -> Result<(), String> {
        self.apply_object(&internet_policy_for(name, namespace))
            .await
    }

    async fn delete_internet(&self, name: &str, namespace: &str) -> Result<(), String> {
        self.kube
            .delete("networkpolicy", &internet_policy_name(name), namespace)
            .await
    }

    async fn delete(&self, namespace: &str, name: &str) -> Result<(), String> {
        let namespace = if namespace.is_empty() {
            self.opts.namespace.clone()
        } else {
            namespace.to_string()
        };
        self.kube
            .delete("networkpolicy", &internet_policy_name(name), &namespace)
            .await?;
        self.delete_route_in(&namespace, name).await?;
        // The route now lives in the platform apps namespace; remove any route
        // an older release rendered into the app namespace as well.
        if namespace != self.opts.namespace {
            self.delete_route_in(&self.opts.namespace, name).await?;
        }
        Ok(())
    }
}

impl KubeRouter {
    async fn delete_route(&self, spec: &Spec, name: &str) -> Result<(), String> {
        self.delete_route_in(spec.route_namespace(), name).await?;
        if spec.route_namespace() != spec.namespace {
            self.delete_route_in(&spec.namespace, name).await?;
        }
        Ok(())
    }

    async fn delete_route_in(&self, namespace: &str, name: &str) -> Result<(), String> {
        self.kube.delete("ingressroute", name, namespace).await?;
        self.kube
            .delete("middleware", &format!("{name}-ipallowlist"), namespace)
            .await
    }
}

/// A deterministic, DNS-label-safe IngressRoute name for a domain.
pub fn portal_name(domain: &str) -> String {
    let label: String = domain
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                c
            } else {
                '-'
            }
        })
        .collect();
    let mut label = label.trim_matches('-').to_string();
    if label.is_empty() {
        label = "domain".to_string();
    }
    let sum = sha256_hex(domain);
    let suffix = &sum[..8];
    let mut name = format!("naslos-sso-portal-{label}");
    if name.len() + suffix.len() + 1 > 63 {
        name.truncate(63 - suffix.len() - 1);
    }
    format!("{name}-{suffix}")
}

fn sha256_hex(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// A Service discoverer over the kube adapter (port of `server/discovery.go`).
pub struct KubeDiscoverer {
    kube: Arc<crate::kube::Client>,
}

impl KubeDiscoverer {
    pub fn new(kube: Arc<crate::kube::Client>) -> Self {
        Self { kube }
    }
}

#[async_trait::async_trait]
impl crate::apps::ServiceDiscoverer for KubeDiscoverer {
    async fn services_for_release(
        &self,
        namespace: &str,
        release: &str,
    ) -> Result<Vec<DiscoveredService>, String> {
        let list = self.kube.list_json("services", namespace, None).await?;
        let mut out = Vec::new();
        if let Some(items) = list.get("items").and_then(|v| v.as_array()) {
            for svc in items {
                let name = svc
                    .get("metadata")
                    .and_then(|m| m.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let ann = svc
                    .get("metadata")
                    .and_then(|m| m.get("annotations"))
                    .and_then(|a| a.get("meta.helm.sh/release-name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let label = svc
                    .get("metadata")
                    .and_then(|m| m.get("labels"))
                    .and_then(|a| a.get("app.kubernetes.io/instance"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if ann != release && label != release {
                    continue;
                }
                if let Some(ports) = svc
                    .get("spec")
                    .and_then(|s| s.get("ports"))
                    .and_then(|p| p.as_array())
                {
                    for port in ports {
                        let pnum = port.get("port").and_then(|v| v.as_i64()).unwrap_or(0);
                        let pname = port
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        out.push(DiscoveredService {
                            name: name.clone(),
                            port: pnum,
                            scheme: port_scheme(&pname, pnum).to_string(),
                            port_name: pname,
                        });
                    }
                }
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name).then(a.port.cmp(&b.port)));
        Ok(out)
    }
}

fn port_scheme(name: &str, port: i64) -> &'static str {
    let name = name.to_lowercase();
    if name.contains("https") || name.contains("tls") || port == 443 {
        "https"
    } else {
        "http"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> Spec {
        Spec {
            name: "nginx".into(),
            namespace: "naslos-apps".into(),
            subdomain: "nginx".into(),
            base_domain: "naslos.local".into(),
            tls: true,
            service: "nginx".into(),
            port: 80,
            tls_secret: "naslos-naslos-local-tls".into(),
            ..Default::default()
        }
    }

    fn middleware_names(route: &serde_json::Value, index: usize) -> Vec<String> {
        route["spec"]["routes"][index]["middlewares"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["name"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn render_basic_route() {
        let objects = render(&spec()).unwrap();
        // security headers + cookie strip + route
        assert_eq!(objects.len(), 3);
        let route = objects.last().unwrap();
        assert_eq!(route["kind"], "IngressRoute");
        assert_eq!(
            route["spec"]["routes"][0]["match"],
            "Host(`nginx.naslos.local`)"
        );
        assert_eq!(route["spec"]["routes"][0]["services"][0]["port"], 80);
        assert_eq!(route["spec"]["entryPoints"][0], "websecure");
        assert_eq!(
            route["spec"]["tls"]["secretName"],
            "naslos-naslos-local-tls"
        );
    }

    #[test]
    fn app_routes_strip_client_identity() {
        let objects = render(&spec()).unwrap();
        let headers = objects
            .iter()
            .find(|o| o["metadata"]["name"] == SECURITY_HEADERS_NAME)
            .expect("security headers middleware");
        let stripped = &headers["spec"]["headers"]["customRequestHeaders"];
        for header in [
            "Remote-User",
            "Remote-Groups",
            "Remote-Email",
            "Remote-Name",
        ] {
            assert_eq!(stripped[header], "", "{header} must be removed");
        }
        // The middleware is first on the route, so forwardAuth cannot run
        // before the client value is removed.
        let route = objects.last().unwrap();
        assert_eq!(
            route["spec"]["routes"][0]["middlewares"][0]["name"],
            SECURITY_HEADERS_NAME
        );
    }

    #[test]
    fn render_auth_adds_forwardauth() {
        let mut s = spec();
        s.auth = true;
        s.authelia_service = "naslos-authelia".into();
        s.authelia_port = 80;
        s.authelia_namespace = "naslos".into();
        let objects = render(&s).unwrap();
        assert_eq!(objects.len(), 4);
        let fwd = objects
            .iter()
            .find(|o| o["metadata"]["name"] == FORWARD_AUTH_NAME);
        assert!(fwd.is_some());
        assert!(fwd.unwrap()["spec"]["forwardAuth"]["address"]
            .as_str()
            .unwrap()
            .contains("naslos-authelia.naslos.svc.cluster.local:80"));
    }

    #[test]
    fn render_public_app_with_protected_paths() {
        let mut s = spec();
        s.authelia_service = "naslos-authelia".into();
        s.authelia_port = 80;
        s.authelia_namespace = "naslos".into();
        s.protected_paths = vec!["/preferences".into(), "/stats".into()];
        let objects = render(&s).unwrap();
        // security headers + forwardAuth + cookie strip + route
        assert_eq!(objects.len(), 4);
        let route = objects.last().unwrap();
        let routes = route["spec"]["routes"].as_array().unwrap();
        assert_eq!(routes.len(), 2);
        assert_eq!(routes[0]["priority"], 100);
        assert_eq!(
            routes[0]["match"],
            "Host(`nginx.naslos.local`) && (PathPrefix(`/preferences`) || PathPrefix(`/stats`))"
        );
        let protected = middleware_names(route, 0);
        assert_eq!(protected[0], SECURITY_HEADERS_NAME);
        assert_eq!(protected[1], FORWARD_AUTH_NAME);
        assert_eq!(protected.last().unwrap(), STRIP_AUTH_COOKIE_NAME);
        // The catch-all stays public: search and static assets need no login.
        assert_eq!(routes[1]["match"], "Host(`nginx.naslos.local`)");
        let public = middleware_names(route, 1);
        assert_eq!(public, vec![SECURITY_HEADERS_NAME, STRIP_AUTH_COOKIE_NAME]);
    }

    #[test]
    fn exposure_never_renders_a_tls_secret_or_key() {
        let mut s = spec();
        s.namespace = "naslos-app-4".into();
        s.route_namespace = "naslos-apps".into();
        let objects = render(&s).unwrap();
        for obj in &objects {
            assert_ne!(obj["kind"], "Secret", "render must not create Secrets");
        }
        let rendered = serde_json::to_string(&objects).unwrap();
        assert!(!rendered.contains("tls.key"));
        assert!(!rendered.contains("kubernetes.io/tls"));
    }

    #[test]
    fn app_route_lives_in_the_platform_namespace() {
        let mut s = spec();
        s.namespace = "naslos-app-4".into();
        s.route_namespace = "naslos-apps".into();
        let objects = render(&s).unwrap();
        for obj in &objects {
            assert_eq!(
                obj["metadata"]["namespace"], "naslos-apps",
                "{} must live beside the domain TLS Secret",
                obj["metadata"]["name"]
            );
        }
        let route = objects.last().unwrap();
        assert_eq!(
            route["spec"]["routes"][0]["services"][0]["namespace"],
            "naslos-app-4"
        );
        assert_eq!(
            route["spec"]["tls"]["secretName"], "naslos-naslos-local-tls",
            "the platform-issued Secret is referenced, not copied"
        );
    }

    #[test]
    fn tls_without_a_platform_secret_uses_the_default_certificate() {
        let mut s = spec();
        s.tls_secret = String::new();
        let objects = render(&s).unwrap();
        let route = objects.last().unwrap();
        assert_eq!(route["spec"]["tls"], serde_json::json!({}));
        assert!(objects.iter().all(|o| o["kind"] != "Secret"));
    }

    #[test]
    fn cookie_strip_runs_after_forwardauth_on_every_app_route() {
        let mut s = spec();
        s.auth = true;
        s.authelia_service = "naslos-authelia".into();
        s.authelia_port = 80;
        s.protected_paths = vec!["/preferences".into()];
        s.protected_paths.clear();
        let objects = render(&s).unwrap();
        let route = objects.last().unwrap();
        let names = middleware_names(route, 0);
        let auth = names.iter().position(|n| n == FORWARD_AUTH_NAME);
        let strip = names.iter().position(|n| n == STRIP_AUTH_COOKIE_NAME);
        assert!(strip.is_some(), "the route must strip the cookie");
        assert!(
            auth.is_none() || strip > auth,
            "forwardAuth must still receive the cookie before it is stripped"
        );
        assert_eq!(names.last().unwrap(), STRIP_AUTH_COOKIE_NAME);

        // The protected-prefix route (auth off) strips after forwardAuth too.
        s.auth = false;
        s.protected_paths = vec!["/preferences".into()];
        let objects = render(&s).unwrap();
        let route = objects.last().unwrap();
        let names = middleware_names(route, 0);
        let auth = names.iter().position(|n| n == FORWARD_AUTH_NAME);
        let strip = names.iter().position(|n| n == STRIP_AUTH_COOKIE_NAME);
        assert!(strip.is_some() && auth.is_some());
        assert!(strip > auth);
        assert_eq!(names.last().unwrap(), STRIP_AUTH_COOKIE_NAME);
        let strip = objects
            .iter()
            .find(|o| o["metadata"]["name"] == STRIP_AUTH_COOKIE_NAME)
            .expect("strip middleware");
        assert_eq!(
            strip["spec"]["plugin"][STRIP_AUTH_COOKIE_PLUGIN],
            serde_json::json!({})
        );
    }

    #[test]
    fn render_rejects_protected_path_rule_injection() {
        let mut s = spec();
        s.authelia_service = "naslos-authelia".into();
        s.authelia_port = 80;
        s.protected_paths = vec!["/preferences`) || Host(`evil".into()];
        assert!(render(&s).is_err());
        s.protected_paths = vec!["preferences".into()];
        assert!(render(&s).is_err());
    }

    #[test]
    fn internet_policy_allows_public_but_not_private_ranges() {
        let p = internet_policy(&spec());
        assert_eq!(p["kind"], "NetworkPolicy");
        assert_eq!(p["metadata"]["name"], "nginx-internet");
        assert_eq!(p["metadata"]["namespace"], "naslos-apps");
        assert_eq!(p["spec"]["policyTypes"][0], "Egress");
        let egress = &p["spec"]["egress"];
        let to = &egress[0]["to"];
        assert_eq!(to[0]["ipBlock"]["cidr"], "0.0.0.0/0");
        let except: Vec<&str> = to[0]["ipBlock"]["except"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        for cidr in [
            "10.0.0.0/8",
            "172.16.0.0/12",
            "192.168.0.0/16",
            "169.254.0.0/16",
        ] {
            assert!(except.contains(&cidr), "must exclude {cidr}");
        }
        let v6: Vec<&str> = to[1]["ipBlock"]["except"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(v6.contains(&"fc00::/7"));
        assert!(v6.contains(&"fe80::/10"));
        // The policy repeats the base DNS and app-to-app allows so it stays
        // correct when `networkPolicy.enabled=false`.
        assert_eq!(
            egress[1]["to"][0]["namespaceSelector"]["matchLabels"]["kubernetes.io/metadata.name"],
            "kube-system"
        );
        assert_eq!(
            egress[2]["to"][0]["namespaceSelector"]["matchLabels"]["naslos.local/apps"],
            "true"
        );
    }

    #[test]
    fn internet_policy_for_builds_the_same_egress_before_a_release_exists() {
        let p = internet_policy_for("bazarr", "naslos-app-8");
        assert_eq!(p["kind"], "NetworkPolicy");
        assert_eq!(p["metadata"]["name"], "bazarr-internet");
        assert_eq!(p["metadata"]["namespace"], "naslos-app-8");
        assert_eq!(p["metadata"]["labels"][MANAGED_BY_LABEL], MANAGED_BY_VALUE);
        assert_eq!(p["metadata"]["labels"][APP_LABEL], "bazarr");
        assert_eq!(p["spec"], internet_policy(&spec())["spec"]);
    }

    #[test]
    fn render_no_subdomain_is_empty() {
        let mut s = spec();
        s.subdomain = String::new();
        assert!(render(&s).unwrap().is_empty());
    }

    #[test]
    fn portal_name_is_stable_and_safe() {
        let n = portal_name("Media.Example.com");
        assert!(n.starts_with("naslos-sso-portal-media-example-com-"));
        assert!(n.len() <= 63);
        assert_eq!(n, portal_name("Media.Example.com"));
    }
}
