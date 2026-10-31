//! Installed-app records and lifecycle orchestration (port of
//! `api/internal/apps`).
//!
//! Records are persisted as JSON beside the other state files and carry the
//! exposure settings the routing layer renders from.

use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::catalog::Catalog;
use crate::chartsrepo::is_dns1123_label;
use crate::helm;

const RELEASE_NAME_TEMPLATE: &str = "{{ .Release.Name }}";

/// Resolve the release-name template in a declared service.
pub fn render_service_name(name: &str, release: &str) -> String {
    name.trim()
        .replace(RELEASE_NAME_TEMPLATE, release)
        .trim()
        .to_string()
}

/// The exposure configuration of an installed app.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Exposure {
    #[serde(default)]
    pub subdomain: String,
    #[serde(default)]
    pub tls: bool,
    #[serde(default)]
    pub auth: bool,
    #[serde(default, rename = "localOnly")]
    pub local_only: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub service: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub port: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub scheme: String,
}

fn is_zero(n: &i64) -> bool {
    *n == 0
}

/// A partial exposure update. Omitted fields keep their stored value instead
/// of defaulting to `false`, which used to silently disable TLS/auth on a
/// partial PUT (SEC-2026-D11).
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct ExposurePatch {
    #[serde(default)]
    pub subdomain: Option<String>,
    #[serde(default)]
    pub tls: Option<bool>,
    #[serde(default)]
    pub auth: Option<bool>,
    #[serde(default, rename = "localOnly")]
    pub local_only: Option<bool>,
    #[serde(default)]
    pub service: Option<String>,
    #[serde(default)]
    pub port: Option<i64>,
    #[serde(default)]
    pub scheme: Option<String>,
}

impl ExposurePatch {
    pub fn apply(&self, current: &Exposure) -> Exposure {
        let mut merged = current.clone();
        if let Some(v) = &self.subdomain {
            merged.subdomain = v.clone();
        }
        if let Some(v) = self.tls {
            merged.tls = v;
        }
        if let Some(v) = self.auth {
            merged.auth = v;
        }
        if let Some(v) = self.local_only {
            merged.local_only = v;
        }
        if let Some(v) = &self.service {
            merged.service = v.clone();
        }
        if let Some(v) = self.port {
            if v != 0 {
                merged.port = v;
            }
        }
        if let Some(v) = &self.scheme {
            merged.scheme = v.clone();
        }
        merged
    }
}

/// A persisted installed app.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Record {
    pub name: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub channel: String,
    #[serde(default, rename = "chartPath")]
    pub chart_path: String,
    #[serde(default, rename = "chartVersion")]
    pub chart_version: String,
    #[serde(default)]
    pub values: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub exposure: Exposure,
    #[serde(
        default,
        rename = "baseDomain",
        skip_serializing_if = "String::is_empty"
    )]
    pub base_domain: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub namespace: String,
    /// Registry datasets attached to this app (D3/F1).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub datasets: Vec<String>,
    #[serde(rename = "createdAt")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(rename = "updatedAt")]
    pub updated_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub orphaned: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub privileged: bool,
    #[serde(
        default,
        rename = "lastError",
        skip_serializing_if = "String::is_empty"
    )]
    pub last_error: String,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// A record enriched with live release status for API responses.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct View {
    #[serde(flatten)]
    pub record: Record,
    pub namespace: String,
    pub status: String,
    pub chart: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub url: String,
    /// Values masked on read because the catalog marks them as secrets. A
    /// client echoes them back as `null` to keep the stored value.
    #[serde(
        default,
        rename = "redactedKeys",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub redacted_keys: Vec<String>,
}

/// A Service rendered by an installed release.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct DiscoveredService {
    pub name: String,
    pub port: i64,
    pub scheme: String,
    #[serde(default, rename = "portName", skip_serializing_if = "String::is_empty")]
    pub port_name: String,
}

/// Returns the current catalog snapshot.
pub type CatalogProvider = Arc<dyn Fn() -> Option<Arc<Catalog>> + Send + Sync>;

/// Returns the effective SSO domain list.
pub type SsoDomainsFn = Arc<dyn Fn() -> Vec<String> + Send + Sync>;

/// Finds the Services a release rendered.
#[async_trait::async_trait]
pub trait ServiceDiscoverer: Send + Sync {
    async fn services_for_release(
        &self,
        namespace: &str,
        release: &str,
    ) -> Result<Vec<DiscoveredService>, String>;
}

/// Renders and applies the exposure layer for a record.
#[async_trait::async_trait]
pub trait Router: Send + Sync {
    async fn apply(
        &self,
        rec: &Record,
        namespace: &str,
        base_domain: &str,
        sso_domains: &[String],
    ) -> Result<(), String>;
    async fn delete(&self, namespace: &str, name: &str) -> Result<(), String>;
}

/// A synchronous progress observer.
pub type ProgressFn = dyn Fn(&str, &str) + Send + Sync;

/// Lifecycle stages reported by the operations.
pub const STAGE_PREPARING: &str = "preparing";
pub const STAGE_INSTALLING: &str = "installing";
pub const STAGE_FINALIZING: &str = "finalizing";

/// Bounds the manager's external dependencies.
pub struct Config {
    pub store_path: String,
    pub helm: Arc<helm::Client>,
    pub helm_privileged: Option<Arc<helm::Client>>,
    pub privileged_namespace: String,
    pub charts: Arc<crate::chartsrepo::Manager>,
    pub catalog: CatalogProvider,
    pub router: Option<Arc<dyn Router>>,
    pub discoverer: Option<Arc<dyn ServiceDiscoverer>>,
    pub base_domain: String,
    pub sso_domains: Option<SsoDomainsFn>,
    /// Per-app namespace pool and dataset provisioning (D3/F1). Empty pools
    /// keep the legacy shared-namespace behaviour.
    pub namespace_pool: Vec<String>,
    pub privileged_namespace_pool: Vec<String>,
    pub kube: Option<Arc<crate::kube::Client>>,
    pub datasets: Option<Arc<crate::datasets::Registry>>,
}

/// Coordinates records and lifecycle operations.
pub struct Manager {
    store: Store,
    helm: Arc<helm::Client>,
    helm_priv: Option<Arc<helm::Client>>,
    priv_ns: String,
    charts: Arc<crate::chartsrepo::Manager>,
    catalog: CatalogProvider,
    router: Option<Arc<dyn Router>>,
    discoverer: Option<Arc<dyn ServiceDiscoverer>>,
    base_domain: String,
    sso_domains: Option<SsoDomainsFn>,
    namespace_pool: Vec<String>,
    privileged_namespace_pool: Vec<String>,
    kube: Option<Arc<crate::kube::Client>>,
    datasets: Option<Arc<crate::datasets::Registry>>,
    /// Namespaces reserved by installs that have not recorded a release yet.
    /// `_mu` was never taken, so two concurrent installs picked the same
    /// "isolated" namespace; reservations are the fix.
    reservations: Arc<Mutex<std::collections::HashSet<String>>>,
}

/// Frees a namespace reservation when the install finishes, succeeds or not.
struct NamespaceReservation {
    reserved: Arc<Mutex<std::collections::HashSet<String>>>,
    namespace: String,
}

impl Drop for NamespaceReservation {
    fn drop(&mut self) {
        if let Ok(mut set) = self.reserved.lock() {
            set.remove(&self.namespace);
        }
    }
}

impl Manager {
    pub fn new(cfg: Config) -> Result<Self, String> {
        let mut store = Store::new(&cfg.store_path);
        store.load()?;
        Ok(Self {
            store,
            helm: cfg.helm,
            helm_priv: cfg.helm_privileged,
            priv_ns: cfg.privileged_namespace,
            charts: cfg.charts,
            catalog: cfg.catalog,
            router: cfg.router,
            discoverer: cfg.discoverer,
            base_domain: cfg.base_domain,
            sso_domains: cfg.sso_domains,
            namespace_pool: cfg.namespace_pool,
            privileged_namespace_pool: cfg.privileged_namespace_pool,
            kube: cfg.kube,
            datasets: cfg.datasets,
            reservations: Arc::new(Mutex::new(std::collections::HashSet::new())),
        })
    }

    fn helm_for(&self, rec: &Record) -> helm::Client {
        self.helm_client(rec.privileged, &self.target_namespace(rec))
    }

    fn helm_client(&self, privileged: bool, namespace: &str) -> helm::Client {
        let base = if privileged {
            self.helm_priv.as_ref().unwrap_or(&self.helm)
        } else {
            &self.helm
        };
        base.for_namespace(namespace)
    }

    fn target_namespace(&self, rec: &Record) -> String {
        if !rec.namespace.is_empty() {
            rec.namespace.clone()
        } else if rec.privileged && !self.priv_ns.is_empty() {
            self.priv_ns.clone()
        } else {
            self.helm.namespace().to_string()
        }
    }

    /// Pick a free namespace from the pool (or the legacy shared namespace
    /// when no pool is configured) and reserve it for this install.
    ///
    /// The record is only written after Helm finishes, so reading persisted
    /// records alone lets two concurrent installs claim the same namespace.
    fn reserve_namespace(
        &self,
        privileged: bool,
    ) -> Result<(String, Option<NamespaceReservation>), String> {
        let pool = if privileged {
            &self.privileged_namespace_pool
        } else {
            &self.namespace_pool
        };
        if pool.is_empty() {
            return Ok((
                if privileged && !self.priv_ns.is_empty() {
                    self.priv_ns.clone()
                } else {
                    self.helm.namespace().to_string()
                },
                None,
            ));
        }

        let mut reservations = self
            .reservations
            .lock()
            .map_err(|_| "app namespace reservations are poisoned".to_string())?;
        let mut used: std::collections::HashSet<String> = self
            .store
            .list()
            .iter()
            .map(|r| self.display_namespace(r))
            .collect();
        used.extend(reservations.iter().cloned());

        let namespace = pick_namespace(pool, &used).ok_or_else(|| {
            "no free per-app namespace is available; raise apps.isolation.namespaces".to_string()
        })?;
        reservations.insert(namespace.clone());
        Ok((
            namespace.clone(),
            Some(NamespaceReservation {
                reserved: self.reservations.clone(),
                namespace,
            }),
        ))
    }

    pub fn records(&self) -> Vec<Record> {
        self.store.list()
    }

    pub fn get(&self, name: &str) -> Result<Record, String> {
        self.store.get(name)
    }

    pub fn base_domain(&self) -> &str {
        &self.base_domain
    }

    pub fn sso_domains(&self) -> Vec<String> {
        self.sso_domains.as_ref().map(|f| f()).unwrap_or_default()
    }

    pub fn auth_allowed(&self, base_domain: &str) -> bool {
        let base = if base_domain.is_empty() {
            self.base_domain.as_str()
        } else {
            base_domain
        };
        if base.is_empty() {
            return false;
        }
        self.sso_domains().iter().any(|d| d == base)
    }

    /// Install resolves the chart from the catalog, installs it, records the
    /// exposure and applies routing.
    pub async fn install(
        &self,
        req: InstallRequest,
        progress: Option<&ProgressFn>,
    ) -> Result<View, String> {
        validate_release_name(&req.name)?;
        report(
            progress,
            STAGE_PREPARING,
            &format!("Resolving {} from the catalog", req.name),
        );
        let catalog = (self.catalog)().ok_or("catalog is not available")?;
        let app = catalog.get(&req.name)?;
        // The HTTP layer checked the catalog snapshot it showed the operator.
        // The job resolves the (swappable) catalog again, so re-check here: a
        // refresh between confirm and execution must not install privileged
        // with only the generic confirmation.
        if app.privileged && !req.privileged_confirmed {
            return Err(
                "this app now requires privileged mode; review that and confirm the install again"
                    .to_string(),
            );
        }
        let chart_dir = self.resolve_chart(&app).await?;
        crate::chartsrepo::assert_regular_tree(&chart_dir)?;

        let mut values = merge_values(&app.default_values, req.values.as_ref());
        validate_install_values(&app, &values)?;
        let (namespace, _reservation) = self.reserve_namespace(app.privileged)?;
        let dataset_values = self
            .provision_datasets(&req.name, &namespace, app.privileged, &req.datasets)
            .await?;
        for (key, value) in dataset_values {
            values.insert(key, value);
        }
        let mut exposure = exposure_for(&app, req.exposure.as_ref(), &req.name);

        report(
            progress,
            STAGE_INSTALLING,
            &format!("Installing {}", req.name),
        );
        if let Err(e) = self
            .helm_client(app.privileged, &namespace)
            .install_dir(
                &req.name,
                &chart_dir,
                &serde_json::Value::Object(values.clone()),
            )
            .await
        {
            // Do not leak a PV/PVC when Helm fails before the record exists.
            self.delete_claims(&req.name, &namespace, &req.datasets)
                .await;
            return Err(e);
        }

        report(
            progress,
            STAGE_FINALIZING,
            "Recording the install and applying routing",
        );
        exposure = self
            .fill_discovered_service(&req.name, &namespace, exposure)
            .await;

        let now = Utc::now();
        let base_domain = if req.base_domain.is_empty() {
            self.base_domain.clone()
        } else {
            req.base_domain.clone()
        };
        let rec = Record {
            name: req.name.clone(),
            source: app.source.clone(),
            channel: app.channel.clone(),
            chart_path: app.chart_path.clone(),
            chart_version: app.version.clone(),
            values,
            exposure,
            base_domain: base_domain.clone(),
            namespace,
            datasets: req.datasets.clone(),
            privileged: app.privileged,
            created_at: Some(now),
            updated_at: Some(now),
            ..Default::default()
        };
        self.store.upsert(rec.clone())?;
        self.apply_route(&rec, &req.base_domain).await;
        self.view(&rec.name).await
    }

    /// Upgrade reconciles an installed app against its chart, replacing values.
    pub async fn upgrade(
        &self,
        name: &str,
        values: Option<&serde_json::Map<String, serde_json::Value>>,
        progress: Option<&ProgressFn>,
    ) -> Result<View, String> {
        report(
            progress,
            STAGE_PREPARING,
            &format!("Resolving the chart for {name}"),
        );
        let mut rec = self.store.get(name)?;
        if rec.orphaned {
            return Err(format!(
                "app {name:?} is orphaned and cannot be reconfigured"
            ));
        }
        let catalog = (self.catalog)().ok_or("catalog is not available")?;
        let app = catalog.get(name)?;
        let chart_dir = self.resolve_chart(&app).await?;
        crate::chartsrepo::assert_regular_tree(&chart_dir)?;
        let mut merged = merge_values(&serde_json::Value::Object(rec.values.clone()), values);

        // A redacted secret comes back as `null`; keep the stored value.
        for key in secret_value_keys(&app.schema) {
            if merged.get(&key).map(|v| v.is_null()).unwrap_or(false) {
                if let Some(previous) = rec.values.get(&key) {
                    merged.insert(key, previous.clone());
                }
            }
        }
        // `install` validated the catalog schema; `upgrade` did not.
        validate_install_values(&app, &merged)?;

        report(progress, STAGE_INSTALLING, &format!("Upgrading {name}"));
        self.helm_for(&rec)
            .upgrade_dir(name, &chart_dir, &serde_json::Value::Object(merged.clone()))
            .await?;

        report(
            progress,
            STAGE_FINALIZING,
            "Recording the new configuration",
        );
        rec.values = merged;
        rec.chart_version = app.version.clone();
        rec.updated_at = Some(Utc::now());
        rec.last_error.clear();
        self.store.upsert(rec)?;
        self.view(name).await
    }

    /// SetExposure updates the exposure settings and re-renders the route.
    pub async fn set_exposure(
        &self,
        name: &str,
        patch: &ExposurePatch,
        base_domain: &str,
    ) -> Result<View, String> {
        let mut rec = self.store.get(name)?;
        let mut merged = patch.apply(&rec.exposure);
        validate_subdomain(&merged.subdomain)?;
        let base_domain = if base_domain.is_empty() {
            if rec.base_domain.is_empty() {
                self.base_domain.clone()
            } else {
                rec.base_domain.clone()
            }
        } else {
            base_domain.to_string()
        };
        if merged.auth && !self.auth_allowed(&base_domain) {
            return Err("auth requires the domain to be in the SSO domain list".to_string());
        }
        if let Some(service) = &patch.service {
            if !service.is_empty() {
                merged.service = render_service_name(service, &rec.name);
            }
        }
        if merged.service.is_empty() {
            let namespace = self.target_namespace(&rec);
            merged = self
                .fill_discovered_service(&rec.name, &namespace, merged)
                .await;
        }
        rec.exposure = merged;
        rec.base_domain = base_domain.clone();
        rec.updated_at = Some(Utc::now());
        rec.last_error.clear();
        self.store.upsert(rec.clone())?;
        self.apply_route(&rec, &base_domain).await;
        self.view(name).await
    }

    /// Uninstall removes the release, its route and its record.
    pub async fn uninstall(&self, name: &str, progress: Option<&ProgressFn>) -> Result<(), String> {
        report(
            progress,
            STAGE_PREPARING,
            &format!("Loading the record for {name}"),
        );
        let rec = self.store.get(name)?;
        if !rec.orphaned {
            report(
                progress,
                STAGE_INSTALLING,
                &format!("Removing the {name} release"),
            );
            self.helm_for(&rec).uninstall(name).await?;
        }
        report(progress, STAGE_FINALIZING, "Removing the route and record");
        if let Some(router) = &self.router {
            router.delete(&self.target_namespace(&rec), name).await?;
        }
        if let Err(e) = self.delete_datasets(&rec).await {
            tracing::warn!("app {name}: dataset cleanup failed: {e}");
        }
        self.store.delete(name)
    }

    /// All records with live status, looked up in each record's namespace.
    pub async fn list(&self) -> Result<Vec<View>, String> {
        let mut views = Vec::new();
        let mut first_err: Option<String> = None;
        for rec in self.store.list() {
            let ns = self.display_namespace(&rec);
            let mut view = View {
                namespace: ns.clone(),
                chart: rec.chart_path.clone(),
                status: "missing".to_string(),
                url: self.url_for(&rec),
                record: rec.clone(),
                redacted_keys: Vec::new(),
            };
            view.redacted_keys = self.redact_record_values(&mut view.record);
            match self.helm_for(&rec).get(&rec.name).await {
                Ok(rel) => {
                    view.status = rel.status.clone();
                    if !rel.chart.is_empty() {
                        view.chart = rel.chart;
                    }
                }
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
            views.push(view);
        }
        if views.is_empty() {
            if let Some(e) = first_err {
                return Err(e);
            }
        }
        views.sort_by(|a, b| a.record.name.cmp(&b.record.name));
        Ok(views)
    }

    fn display_namespace(&self, rec: &Record) -> String {
        self.target_namespace(rec)
    }

    /// Create orphaned records for releases that predate the refactor.
    pub fn backfill(&self, platform_release: &str, releases: &[helm::App]) -> Result<(), String> {
        for rel in releases {
            if rel.name == platform_release {
                continue;
            }
            if self.store.get(&rel.name).is_ok() {
                continue;
            }
            let rec = Record {
                name: rel.name.clone(),
                chart_version: rel.version.clone(),
                values: match &rel.values {
                    serde_json::Value::Object(m) => m.clone(),
                    _ => Default::default(),
                },
                namespace: rel.namespace.clone(),
                orphaned: true,
                created_at: Some(Utc::now()),
                updated_at: Some(Utc::now()),
                ..Default::default()
            };
            self.store.upsert(rec)?;
        }
        Ok(())
    }

    /// Re-apply routing for every non-orphaned record.
    pub async fn reconcile_routes(&self) -> Result<(), String> {
        let Some(router) = &self.router else {
            return Ok(());
        };
        let sso = self.sso_domains();
        let mut errs = Vec::new();
        for rec in self.store.list() {
            if rec.orphaned {
                continue;
            }
            let base = if rec.base_domain.is_empty() {
                self.base_domain.clone()
            } else {
                rec.base_domain.clone()
            };
            if let Err(e) = router
                .apply(&rec, &self.target_namespace(&rec), &base, &sso)
                .await
            {
                errs.push(format!("app {:?}: {e}", rec.name));
            }
        }
        if errs.is_empty() {
            Ok(())
        } else {
            Err(errs.join("; "))
        }
    }

    async fn apply_route(&self, rec: &Record, base_domain: &str) {
        let Some(router) = &self.router else {
            return;
        };
        let base = if base_domain.is_empty() {
            if rec.base_domain.is_empty() {
                self.base_domain.clone()
            } else {
                rec.base_domain.clone()
            }
        } else {
            base_domain.to_string()
        };
        if let Err(e) = router
            .apply(rec, &self.target_namespace(rec), &base, &self.sso_domains())
            .await
        {
            let _ = self.store.update(&rec.name, |r| r.last_error = e.clone());
        }
    }

    /// The Services a release rendered.
    pub async fn discover_services(&self, name: &str) -> Result<Vec<DiscoveredService>, String> {
        let rec = self.store.get(name)?;
        let Some(d) = &self.discoverer else {
            return Ok(Vec::new());
        };
        d.services_for_release(&self.target_namespace(&rec), name)
            .await
    }

    fn isolation_enabled(&self) -> bool {
        !self.namespace_pool.is_empty() || !self.privileged_namespace_pool.is_empty()
    }

    /// Create the per-app PV/PVCs for the selected registry datasets and return
    /// the Helm values the chart needs. Baseline apps get a claim bound to the
    /// dataset; privileged apps get the specific host path (they can mount it).
    async fn provision_datasets(
        &self,
        app: &str,
        namespace: &str,
        privileged: bool,
        selected: &[String],
    ) -> Result<serde_json::Map<String, serde_json::Value>, String> {
        let mut values = serde_json::Map::new();
        if selected.is_empty() {
            return Ok(values);
        }
        if selected.len() > 1 {
            return Err(
                "installed apps currently support one dataset; attach the others with a future chart that reads datasetsClaims"
                    .to_string(),
            );
        }
        if !self.isolation_enabled() {
            return Err(
                "attaching datasets requires per-app namespaces (apps.isolation.enabled=true)"
                    .to_string(),
            );
        }
        let Some(registry) = &self.datasets else {
            return Err("the dataset registry is not configured".to_string());
        };
        let Some(kube) = &self.kube else {
            return Err("the Kubernetes client is not configured".to_string());
        };
        // Reserve each dataset before creating a claim. The reservation is
        // persisted, so two concurrent installs cannot both claim a private
        // dataset.
        let mut attached: Vec<String> = Vec::new();
        for name in selected {
            if let Err(e) = registry.attach(name, app) {
                for done in &attached {
                    let _ = registry.detach(done, app);
                }
                return Err(e);
            }
            attached.push(name.clone());
        }
        let mut claims = Vec::new();
        let mut created: Vec<(String, String)> = Vec::new();
        for name in selected {
            let record = registry.get(name).unwrap();
            let object_name = format!("naslos-ds-{app}-{name}");
            if object_name.len() > 253 {
                return Err(format!("dataset claim name {object_name:?} is too long"));
            }
            if privileged {
                // Privileged charts mount the specific dataset path directly;
                // no claim is needed.
                values.insert("hostPath".to_string(), serde_json::json!(record.host_path));
                continue;
            }
            let pv = serde_json::json!({
                "apiVersion": "v1",
                "kind": "PersistentVolume",
                "metadata": {
                    "name": object_name,
                    "labels": {
                        "naslos.local/managed-dataset": name,
                        "naslos.local/app": app,
                    },
                },
                "spec": {
                    "capacity": { "storage": "10Ti" },
                    "accessModes": ["ReadWriteOnce"],
                    "persistentVolumeReclaimPolicy": "Retain",
                    "storageClassName": "naslos-app-data",
                    "hostPath": { "path": record.host_path, "type": "Directory" },
                },
            });
            let pvc = serde_json::json!({
                "apiVersion": "v1",
                "kind": "PersistentVolumeClaim",
                "metadata": {
                    "name": object_name,
                    "namespace": namespace,
                    "labels": {
                        "naslos.local/managed-dataset": name,
                        "naslos.local/app": app,
                    },
                },
                "spec": {
                    "accessModes": ["ReadWriteOnce"],
                    "storageClassName": "naslos-app-data",
                    "volumeName": object_name,
                    "resources": { "requests": { "storage": "10Ti" } },
                },
            });
            if let Err(e) = kube.apply(&pv).await {
                self.rollback_claims(&created).await;
                for done in &attached {
                    let _ = registry.detach(done, app);
                }
                return Err(format!("creating dataset PV {object_name}: {e}"));
            }
            if let Err(e) = kube.apply(&pvc).await {
                let _ = kube.delete("pv", &object_name, "").await;
                self.rollback_claims(&created).await;
                for done in &attached {
                    let _ = registry.detach(done, app);
                }
                return Err(format!("creating dataset PVC {object_name}: {e}"));
            }
            created.push((object_name.clone(), namespace.to_string()));
            claims.push(serde_json::json!(object_name));
        }
        if !claims.is_empty() {
            values.insert(
                "datasetsClaim".to_string(),
                claims.first().cloned().unwrap_or_default(),
            );
            values.insert(
                "datasetsClaims".to_string(),
                serde_json::Value::Array(claims),
            );
        }
        Ok(values)
    }

    async fn rollback_claims(&self, created: &[(String, String)]) {
        if let Some(kube) = &self.kube {
            for (name, namespace) in created {
                let _ = kube.delete("pvc", name, namespace).await;
                let _ = kube.delete("pv", name, "").await;
            }
        }
    }

    async fn delete_claims(&self, app: &str, namespace: &str, datasets: &[String]) {
        if let Some(registry) = &self.datasets {
            for name in datasets {
                let _ = registry.detach(name, app);
            }
        }
        let Some(kube) = &self.kube else {
            return;
        };
        for name in datasets {
            let object_name = format!("naslos-ds-{app}-{name}");
            let _ = kube.delete("pvc", &object_name, namespace).await;
            let _ = kube.delete("pv", &object_name, "").await;
        }
    }

    async fn delete_datasets(&self, rec: &Record) -> Result<(), String> {
        self.delete_claims(&rec.name, &self.target_namespace(rec), &rec.datasets)
            .await;
        Ok(())
    }

    async fn fill_discovered_service(
        &self,
        release: &str,
        namespace: &str,
        e: Exposure,
    ) -> Exposure {
        if !e.service.is_empty() || self.discoverer.is_none() {
            return e;
        }
        let d = self.discoverer.as_ref().unwrap();
        match d.services_for_release(namespace, release).await {
            Ok(services) if !services.is_empty() => {
                let mut e = e;
                e.service = services[0].name.clone();
                e.port = services[0].port;
                e.scheme = services[0].scheme.clone();
                e
            }
            _ => e,
        }
    }

    async fn resolve_chart(&self, app: &crate::catalog::App) -> Result<String, String> {
        self.charts.ensure_fresh(&app.source, &app.channel).await?;
        self.charts.app_dir(&app.source, &app.channel, &app.name)
    }

    async fn view(&self, name: &str) -> Result<View, String> {
        let rec = self.store.get(name)?;
        let ns = self.display_namespace(&rec);
        let mut view = View {
            namespace: ns,
            chart: rec.chart_path.clone(),
            status: "missing".to_string(),
            url: self.url_for(&rec),
            record: rec.clone(),
            redacted_keys: Vec::new(),
        };
        view.redacted_keys = self.redact_record_values(&mut view.record);
        if let Ok(rel) = self.helm_for(&rec).get(name).await {
            view.status = rel.status.clone();
            if !rel.chart.is_empty() {
                view.chart = rel.chart;
            }
        }
        Ok(view)
    }

    /// Mask secret values in a record for API responses (returns the keys).
    pub fn redact_record_values(&self, record: &mut Record) -> Vec<String> {
        let keys = match (self.catalog)().and_then(|c| c.get(&record.name).ok()) {
            Some(app) => secret_value_keys(&app.schema),
            None => return Vec::new(),
        };
        let mut redacted: Vec<String> = keys
            .into_iter()
            .filter(|key| {
                record
                    .values
                    .get(key)
                    .map(|v| !v.is_null())
                    .unwrap_or(false)
            })
            .collect();
        for key in &redacted {
            record.values.insert(key.clone(), serde_json::Value::Null);
        }
        redacted.sort();
        redacted
    }

    /// A record ready for `GET /api/apps/{name}` (secrets masked).
    pub fn public_record(&self, rec: &Record) -> serde_json::Value {
        let mut masked = rec.clone();
        let redacted = self.redact_record_values(&mut masked);
        let mut value = serde_json::to_value(&masked).unwrap_or_default();
        if !redacted.is_empty() {
            value["redactedKeys"] = serde_json::json!(redacted);
        }
        value
    }

    fn url_for(&self, rec: &Record) -> String {
        if rec.exposure.subdomain.is_empty() {
            return String::new();
        }
        let base = if rec.base_domain.is_empty() {
            self.base_domain.clone()
        } else {
            rec.base_domain.clone()
        };
        if base.is_empty() {
            return String::new();
        }
        let host = format!("{}.{}", rec.exposure.subdomain, base);
        if rec.exposure.tls {
            format!("https://{host}")
        } else {
            format!("http://{host}")
        }
    }
}

/// `Install` request input.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct InstallRequest {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub values: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(default)]
    pub exposure: Option<Exposure>,
    #[serde(default, rename = "baseDomain")]
    pub base_domain: String,
    /// Registered datasets to attach (D3/F1); empty keeps the app claim-less.
    #[serde(default)]
    pub datasets: Vec<String>,
    /// The operator reviewed the privileged warning. Re-checked at run time.
    #[serde(default, rename = "privilegedConfirmed")]
    pub privileged_confirmed: bool,
}

/// First namespace in `pool` that is neither recorded nor reserved.
fn pick_namespace(pool: &[String], used: &std::collections::HashSet<String>) -> Option<String> {
    pool.iter().find(|ns| !used.contains(*ns)).cloned()
}

/// Property names the catalog marks as secrets (`format: password` or
/// `writeOnly: true`), at any nesting depth.
pub fn secret_value_keys(schema: &serde_json::Value) -> std::collections::HashSet<String> {
    fn walk(node: &serde_json::Value, out: &mut std::collections::HashSet<String>) {
        match node {
            serde_json::Value::Object(map) => {
                if let Some(serde_json::Value::Object(props)) = map.get("properties") {
                    for (key, prop) in props {
                        let secret = prop
                            .get("format")
                            .and_then(|f| f.as_str())
                            .map(|f| f == "password")
                            .unwrap_or(false)
                            || prop
                                .get("writeOnly")
                                .and_then(|w| w.as_bool())
                                .unwrap_or(false);
                        if secret {
                            out.insert(key.clone());
                        }
                    }
                }
                for value in map.values() {
                    walk(value, out);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    walk(item, out);
                }
            }
            _ => {}
        }
    }
    let mut out = std::collections::HashSet::new();
    walk(schema, &mut out);
    out
}

fn report(progress: Option<&ProgressFn>, stage: &str, message: &str) {
    if let Some(p) = progress {
        p(stage, message);
    }
}

/// Derive exposure from the catalog defaults, the declared service and the
/// caller's overrides.
pub fn exposure_for(
    app: &crate::catalog::App,
    override_: Option<&Exposure>,
    release: &str,
) -> Exposure {
    let mut e = Exposure {
        subdomain: app.exposure.subdomain.clone(),
        tls: app.exposure.tls,
        auth: app.exposure.auth,
        local_only: app.exposure.local_only,
        ..Default::default()
    };
    if !app.services.is_empty() {
        e.service = render_service_name(&app.services[0].name, release);
        e.port = app.services[0].port;
        e.scheme = app.services[0].scheme.clone();
    }
    if let Some(o) = override_ {
        if !o.subdomain.is_empty() {
            e.subdomain = o.subdomain.clone();
        }
        e.tls = o.tls;
        e.auth = o.auth;
        e.local_only = o.local_only;
        if !o.service.is_empty() {
            e.service = render_service_name(&o.service, release);
        }
        if o.port != 0 {
            e.port = o.port;
        }
        if !o.scheme.is_empty() {
            e.scheme = o.scheme.clone();
        }
    }
    if e.scheme.is_empty() {
        e.scheme = "http".to_string();
    }
    e
}

/// Deep-merge overlay into base and return a new map.
/// Validate the merged install values against the catalog's JSON schema
/// (SEC-2026-D7). A missing/null schema means the catalog does not constrain
/// the values; an invalid schema is a catalog defect and fails closed.
fn validate_install_values(
    app: &crate::catalog::App,
    values: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    if app.schema.is_null() {
        return Ok(());
    }
    let validator = jsonschema::validator_for(&app.schema)
        .map_err(|e| format!("app {:?} has an invalid values schema: {e}", app.name))?;
    let instance = serde_json::Value::Object(values.clone());
    let errors: Vec<String> = validator
        .iter_errors(&instance)
        .map(|e| e.to_string())
        .take(8)
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "invalid values for {:?}: {}",
            app.name,
            errors.join("; ")
        ))
    }
}

pub fn merge_values(
    base: &serde_json::Value,
    overlay: Option<&serde_json::Map<String, serde_json::Value>>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut out = match base {
        serde_json::Value::Object(m) => m.clone(),
        _ => serde_json::Map::new(),
    };
    if let Some(overlay) = overlay {
        for (k, v) in overlay {
            if let (
                Some(serde_json::Value::Object(existing)),
                serde_json::Value::Object(incoming),
            ) = (out.get(k), v)
            {
                out.insert(
                    k.clone(),
                    serde_json::Value::Object(merge_values(
                        &serde_json::Value::Object(existing.clone()),
                        Some(incoming),
                    )),
                );
                continue;
            }
            out.insert(k.clone(), v.clone());
        }
    }
    out
}

pub fn validate_release_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("app name is required".to_string());
    }
    if name.len() > 53 {
        return Err(format!("app name {name:?} must be 53 characters or fewer"));
    }
    if !is_dns1123_label(name) {
        return Err(format!(
            "app name {name:?} must be a lowercase DNS-1123 label"
        ));
    }
    Ok(())
}

pub fn validate_subdomain(subdomain: &str) -> Result<(), String> {
    if subdomain.is_empty() || is_dns1123_label(subdomain) {
        Ok(())
    } else {
        Err(format!(
            "subdomain {subdomain:?} must be a lowercase DNS-1123 label"
        ))
    }
}

/// Persists records as JSON.
pub struct Store {
    path: String,
    records: Mutex<HashMap<String, Record>>,
}

impl Store {
    pub fn new(path: &str) -> Self {
        Self {
            path: path.to_string(),
            records: Mutex::new(HashMap::new()),
        }
    }

    pub fn load(&mut self) -> Result<(), String> {
        if self.path.is_empty() {
            return Ok(());
        }
        let data = match std::fs::read(&self.path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(format!("reading apps file: {e}")),
        };
        let list: Vec<Record> =
            serde_json::from_slice(&data).map_err(|e| format!("parsing apps file: {e}"))?;
        let mut guard = self.records.lock().unwrap();
        for mut rec in list {
            if rec.name.is_empty() {
                continue;
            }
            rec.values = merge_values(&serde_json::Value::Object(rec.values), None);
            guard.insert(rec.name.clone(), rec);
        }
        Ok(())
    }

    pub fn list(&self) -> Vec<Record> {
        let guard = self.records.lock().unwrap();
        let mut out: Vec<Record> = guard.values().map(clone_record).collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    pub fn get(&self, name: &str) -> Result<Record, String> {
        self.records
            .lock()
            .unwrap()
            .get(name)
            .map(clone_record)
            .ok_or_else(|| format!("app {name:?} not found"))
    }

    pub fn upsert(&self, rec: Record) -> Result<(), String> {
        let mut guard = self.records.lock().unwrap();
        guard.insert(rec.name.clone(), clone_record(&rec));
        self.save_locked(&guard)
    }

    pub fn update(&self, name: &str, mutate: impl FnOnce(&mut Record)) -> Result<Record, String> {
        let mut guard = self.records.lock().unwrap();
        let rec = guard
            .get_mut(name)
            .ok_or_else(|| format!("app {name:?} not found"))?;
        mutate(rec);
        let out = clone_record(rec);
        self.save_locked(&guard)?;
        Ok(out)
    }

    pub fn delete(&self, name: &str) -> Result<(), String> {
        let mut guard = self.records.lock().unwrap();
        if guard.remove(name).is_none() {
            return Err(format!("app {name:?} not found"));
        }
        self.save_locked(&guard)
    }

    fn save_locked(&self, guard: &HashMap<String, Record>) -> Result<(), String> {
        if self.path.is_empty() {
            return Ok(());
        }
        let mut list: Vec<&Record> = guard.values().collect();
        list.sort_by(|a, b| a.name.cmp(&b.name));
        let mut data =
            serde_json::to_vec_pretty(&list).map_err(|e| format!("encoding apps: {e}"))?;
        data.push(b'\n');
        crate::chartsrepo::write_atomic(&self.path, &data, 0o600, 0o750)
    }
}

fn clone_record(rec: &Record) -> Record {
    let mut dup = rec.clone();
    dup.values = merge_values(&serde_json::Value::Object(rec.values.clone()), None);
    dup
}

/// The default apps state path (overridable by `APPS_CONFIG`).
pub fn default_store_path() -> String {
    std::env::var("APPS_CONFIG").unwrap_or_else(|_| "/var/lib/naslos/apps.json".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_name_validation() {
        assert!(validate_release_name("nginx").is_ok());
        assert!(validate_release_name("").is_err());
        assert!(validate_release_name("Bad_Name").is_err());
        assert!(validate_release_name(&"a".repeat(54)).is_err());
    }

    #[test]
    fn subdomain_validation() {
        assert!(validate_subdomain("").is_ok());
        assert!(validate_subdomain("media").is_ok());
        assert!(validate_subdomain("Bad").is_err());
    }

    #[test]
    fn merge_values_deep_merges() {
        let base = serde_json::json!({ "a": 1, "nested": { "x": 1, "y": 2 } });
        let overlay = serde_json::json!({ "b": 2, "nested": { "y": 9, "z": 3 } });
        let merged = merge_values(&base, overlay.as_object());
        assert_eq!(merged["a"], 1);
        assert_eq!(merged["b"], 2);
        assert_eq!(merged["nested"]["x"], 1);
        assert_eq!(merged["nested"]["y"], 9);
        assert_eq!(merged["nested"]["z"], 3);
    }

    #[test]
    fn render_service_name_resolves_template() {
        assert_eq!(render_service_name("{{ .Release.Name }}", "nginx"), "nginx");
        assert_eq!(render_service_name("web", "nginx"), "web");
    }

    #[test]
    fn store_round_trips_and_sorts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("apps.json");
        let store = Store::new(&path.display().to_string());
        for name in ["zeta", "alpha"] {
            store
                .upsert(Record {
                    name: name.into(),
                    ..Default::default()
                })
                .unwrap();
        }
        let names: Vec<String> = store.list().into_iter().map(|r| r.name).collect();
        assert_eq!(names, vec!["alpha", "zeta"]);

        let reloaded = Store::new(&path.display().to_string());
        let mut reloaded = reloaded;
        reloaded.load().unwrap();
        assert_eq!(reloaded.list().len(), 2);
    }
}

#[cfg(test)]
mod exposure_patch_tests {
    use super::{Exposure, ExposurePatch};

    #[test]
    fn omitted_fields_keep_the_stored_value() {
        let current = Exposure {
            subdomain: "plex".into(),
            tls: true,
            auth: true,
            local_only: false,
            service: "plex".into(),
            port: 32400,
            scheme: "http".into(),
        };
        let patch: ExposurePatch = serde_json::from_str(r#"{"subdomain":"media"}"#).unwrap();
        let merged = patch.apply(&current);
        assert_eq!(merged.subdomain, "media");
        assert!(merged.tls, "TLS must not be disabled by an omitted field");
        assert!(merged.auth, "auth must not be disabled by an omitted field");
        assert_eq!(merged.port, 32400);
    }
}

#[cfg(test)]
mod value_schema_tests {
    use super::{pick_namespace, secret_value_keys, validate_install_values};
    use crate::catalog::App;
    use serde_json::json;

    fn app_with_schema() -> App {
        App {
            name: "demo".into(),
            schema: json!({
                "type": "object",
                "properties": { "port": { "type": "integer", "minimum": 1 } },
                "required": ["port"]
            }),
            ..Default::default()
        }
    }

    #[test]
    fn valid_values_pass() {
        let mut values = serde_json::Map::new();
        values.insert("port".into(), json!(8080));
        assert!(validate_install_values(&app_with_schema(), &values).is_ok());
    }

    #[test]
    fn missing_or_wrong_type_is_refused() {
        let empty = serde_json::Map::new();
        assert!(validate_install_values(&app_with_schema(), &empty).is_err());
        let mut bad = serde_json::Map::new();
        bad.insert("port".into(), json!("eighty"));
        let err = validate_install_values(&app_with_schema(), &bad).unwrap_err();
        assert!(err.contains("invalid values"), "{err}");
    }

    #[test]
    fn a_null_schema_is_unconstrained() {
        let app = App {
            name: "demo".into(),
            schema: serde_json::Value::Null,
            ..Default::default()
        };
        assert!(validate_install_values(&app, &serde_json::Map::new()).is_ok());
    }

    #[test]
    fn catalog_secret_fields_are_detected() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "password": {"type": "string", "format": "password"},
                "nested": {"type": "object", "properties": {"token": {"writeOnly": true}}},
                "user": {"type": "string"}
            }
        });
        let keys = secret_value_keys(&schema);
        assert!(keys.contains("password"));
        assert!(keys.contains("token"));
        assert!(!keys.contains("user"));
    }

    #[test]
    fn namespace_selection_skips_records_and_reservations() {
        let pool = vec![
            "naslos-apps-1".to_string(),
            "naslos-apps-2".to_string(),
            "naslos-apps-3".to_string(),
        ];
        let mut used = std::collections::HashSet::new();
        used.insert("naslos-apps-1".to_string());
        used.insert("naslos-apps-2".to_string());
        assert_eq!(
            pick_namespace(&pool, &used).as_deref(),
            Some("naslos-apps-3")
        );
        used.insert("naslos-apps-3".to_string());
        assert_eq!(pick_namespace(&pool, &used), None);
    }
}
