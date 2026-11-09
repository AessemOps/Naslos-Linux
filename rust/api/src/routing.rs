//! Traefik exposure layer for installed apps (port of `api/internal/routing`).
//! One IngressRoute per app plus the middlewares it references, in the apps
//! namespace, applied through the kube adapter.

use std::sync::Arc;

use crate::apps::{DiscoveredService, Record, Router};

pub const FORWARD_AUTH_NAME: &str = "naslos-app-forwardauth-authelia";
pub const SECURITY_HEADERS_NAME: &str = "naslos-app-security-headers";
pub const PORTAL_EXTERNAL_SERVICE_NAME: &str = "naslos-authelia-portal";
pub const PORTAL_LABEL_KEY: &str = "naslos.local/sso-portal";

/// Everything needed to render one app's route.
#[derive(Debug, Clone, Default)]
pub struct Spec {
    pub name: String,
    pub namespace: String,
    pub subdomain: String,
    pub base_domain: String,
    pub tls: bool,
    pub auth: bool,
    pub local_only: bool,
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
    let scheme = if spec.scheme.is_empty() {
        "http"
    } else {
        &spec.scheme
    };

    let mut middlewares = vec![serde_json::json!({
        "name": SECURITY_HEADERS_NAME, "namespace": spec.namespace
    })];
    if spec.auth {
        if spec.authelia_service.is_empty() {
            return Err(format!(
                "app {:?} requests auth but no Authelia service is configured",
                spec.name
            ));
        }
        middlewares.push(serde_json::json!({
            "name": FORWARD_AUTH_NAME, "namespace": spec.namespace
        }));
    }
    if spec.local_only {
        if spec.local_only_cidr.is_empty() {
            return Err(format!(
                "app {:?} is local-only but no LAN CIDR is configured",
                spec.name
            ));
        }
        middlewares.push(serde_json::json!({
            "name": format!("{}-ipallowlist", spec.name), "namespace": spec.namespace
        }));
    }

    let entry_point = if spec.tls { "websecure" } else { "web" };
    let mut objects = vec![security_headers_middleware(&spec.namespace)];
    if spec.auth {
        objects.push(forward_auth_middleware(spec));
    }
    if spec.local_only {
        objects.push(ip_allowlist_middleware(spec));
    }

    let mut route_spec = serde_json::json!({
        "entryPoints": [entry_point],
        "routes": [{
            "match": format!("Host(`{}`)", spec.host()),
            "kind": "Rule",
            "middlewares": middlewares,
            "services": [{ "name": spec.service, "port": spec.port, "scheme": scheme }],
        }],
    });
    if spec.tls && !spec.tls_secret.is_empty() {
        route_spec["tls"] = serde_json::json!({ "secretName": spec.tls_secret });
    }
    objects.push(serde_json::json!({
        "apiVersion": "traefik.io/v1alpha1",
        "kind": "IngressRoute",
        "metadata": { "name": spec.name, "namespace": spec.namespace },
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

fn security_headers_middleware(namespace: &str) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "traefik.io/v1alpha1",
        "kind": "Middleware",
        "metadata": { "name": SECURITY_HEADERS_NAME, "namespace": namespace },
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
        "metadata": { "name": FORWARD_AUTH_NAME, "namespace": spec.namespace },
        "spec": { "forwardAuth": {
            "address": address,
            "authResponseHeaders": ["Remote-User", "Remote-Groups", "Remote-Email", "Remote-Name"],
        }},
    })
}

fn ip_allowlist_middleware(spec: &Spec) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "traefik.io/v1alpha1",
        "kind": "Middleware",
        "metadata": { "name": format!("{}-ipallowlist", spec.name), "namespace": spec.namespace },
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
    /// Release namespace holding the platform TLS Secret.
    pub release_namespace: String,
    /// Platform TLS Secret used when no domain-specific Secret exists.
    pub fallback_tls_secret: String,
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
        let mut tls_secret = self.opts.tls_secret.clone();
        if let Some(resolver) = &self.opts.tls_secret_for {
            let resolved = resolver(base_domain);
            if !resolved.is_empty() {
                tls_secret = resolved;
            }
        }
        Spec {
            name: rec.name.clone(),
            namespace,
            subdomain: rec.exposure.subdomain.clone(),
            base_domain: base_domain.to_string(),
            tls: rec.exposure.tls,
            auth: rec.exposure.auth,
            local_only: rec.exposure.local_only,
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

    /// Copy the domain/default TLS Secret into the app namespace. An
    /// IngressRoute can only reference a Secret in its own namespace, and the
    /// domain Certificates live in the shared apps namespace.
    async fn copy_tls_secret(&self, target: &str, name: &str) -> Result<(), String> {
        let mut found = None;
        for (namespace, source_name) in [
            (self.opts.namespace.as_str(), name),
            (
                self.opts.release_namespace.as_str(),
                self.opts.fallback_tls_secret.as_str(),
            ),
        ] {
            if namespace.is_empty() || source_name.is_empty() {
                continue;
            }
            match self.kube.get_json("secret", source_name, namespace).await {
                Ok(Some(secret)) => {
                    found = Some(secret);
                    break;
                }
                Ok(None) => {}
                Err(e) => {
                    return Err(format!(
                        "reading TLS secret {source_name:?} in {namespace}: {e}"
                    ))
                }
            }
        }
        let Some(mut secret) = found else {
            return Err(format!(
                "no TLS secret {name:?} in {} or {}; cannot expose this app with TLS",
                self.opts.namespace, self.opts.release_namespace
            ));
        };
        if let Some(meta) = secret.get_mut("metadata").and_then(|m| m.as_object_mut()) {
            for key in [
                "resourceVersion",
                "uid",
                "creationTimestamp",
                "ownerReferences",
                "managedFields",
                "selfLink",
            ] {
                meta.remove(key);
            }
            meta.insert("namespace".to_string(), serde_json::json!(target));
            meta.insert("name".to_string(), serde_json::json!(name));
        }
        if let Some(obj) = secret.as_object_mut() {
            obj.remove("status");
        }
        self.kube
            .apply(&secret)
            .await
            .map_err(|e| format!("copying TLS secret {name:?} to {target}: {e}"))
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
                    if let Err(e) = self.delete_route(&spec.namespace, &rec.name).await {
                        errs.push(e);
                    }
                }
                Ok(objects) => {
                    if spec.tls
                        && !spec.tls_secret.is_empty()
                        && spec.namespace != self.opts.namespace
                    {
                        if let Err(e) = self
                            .copy_tls_secret(&spec.namespace, &spec.tls_secret)
                            .await
                        {
                            errs.push(e);
                        }
                    }
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
        self.delete_route(&namespace, name).await
    }
}

impl KubeRouter {
    async fn delete_route(&self, namespace: &str, name: &str) -> Result<(), String> {
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

    #[test]
    fn render_basic_route() {
        let objects = render(&spec()).unwrap();
        // security headers + route
        assert_eq!(objects.len(), 2);
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
        assert_eq!(objects.len(), 3);
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
