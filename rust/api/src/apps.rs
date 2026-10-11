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
    #[serde(skip)]
    pub namespace: String,
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
    _mu: Mutex<()>,
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
            _mu: Mutex::new(()),
        })
    }

    fn helm_for(&self, privileged: bool) -> &Arc<helm::Client> {
        if privileged {
            if let Some(h) = &self.helm_priv {
                return h;
            }
        }
        &self.helm
    }

    fn target_namespace(&self, rec: &Record) -> String {
        if rec.privileged && !self.priv_ns.is_empty() {
            self.priv_ns.clone()
        } else {
            self.helm.namespace().to_string()
        }
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
        let chart_dir = self.resolve_chart(&app).await?;

        let values = merge_values(&app.default_values, req.values.as_ref());
        let mut exposure = exposure_for(&app, req.exposure.as_ref(), &req.name);

        report(
            progress,
            STAGE_INSTALLING,
            &format!("Installing {}", req.name),
        );
        self.helm_for(app.privileged)
            .install_dir(
                &req.name,
                &chart_dir,
                &serde_json::Value::Object(values.clone()),
            )
            .await?;

        report(
            progress,
            STAGE_FINALIZING,
            "Recording the install and applying routing",
        );
        exposure = self
            .fill_discovered_service(&req.name, app.privileged, exposure)
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
        let merged = merge_values(&serde_json::Value::Object(rec.values.clone()), values);

        report(progress, STAGE_INSTALLING, &format!("Upgrading {name}"));
        self.helm_for(rec.privileged)
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
        exposure: &Exposure,
        base_domain: &str,
    ) -> Result<View, String> {
        let mut rec = self.store.get(name)?;
        validate_subdomain(&exposure.subdomain)?;
        let base_domain = if base_domain.is_empty() {
            if rec.base_domain.is_empty() {
                self.base_domain.clone()
            } else {
                rec.base_domain.clone()
            }
        } else {
            base_domain.to_string()
        };
        if exposure.auth && !self.auth_allowed(&base_domain) {
            return Err("auth requires the domain to be in the SSO domain list".to_string());
        }
        let mut merged = rec.exposure.clone();
        merged.subdomain = exposure.subdomain.clone();
        merged.tls = exposure.tls;
        merged.auth = exposure.auth;
        merged.local_only = exposure.local_only;
        if !exposure.service.is_empty() {
            merged.service = render_service_name(&exposure.service, &rec.name);
        }
        if exposure.port != 0 {
            merged.port = exposure.port;
        }
        if !exposure.scheme.is_empty() {
            merged.scheme = exposure.scheme.clone();
        }
        if merged.service.is_empty() {
            merged = self
                .fill_discovered_service(&rec.name, rec.privileged, merged)
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
            self.helm_for(rec.privileged).uninstall(name).await?;
        }
        report(progress, STAGE_FINALIZING, "Removing the route and record");
        if let Some(router) = &self.router {
            router.delete(&self.target_namespace(&rec), name).await?;
        }
        self.store.delete(name)
    }

    /// All records with live status, merging the managed and privileged
    /// namespaces.
    pub async fn list(&self) -> Result<Vec<View>, String> {
        let mut releases: HashMap<String, helm::App> = HashMap::new();
        let mut clients: Vec<&Arc<helm::Client>> = vec![&self.helm];
        if let Some(hp) = &self.helm_priv {
            if hp.namespace() != self.helm.namespace() {
                clients.push(hp);
            }
        }
        let mut first_err: Option<String> = None;
        for client in clients {
            match client.list().await {
                Ok(list) => {
                    for rel in list {
                        releases.insert(format!("{}/{}", rel.namespace, rel.name), rel);
                    }
                }
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
        }
        if releases.is_empty() {
            if let Some(e) = first_err {
                return Err(e);
            }
        }

        let mut views = Vec::new();
        for rec in self.store.list() {
            let ns = self.display_namespace(&rec);
            let mut view = View {
                namespace: ns.clone(),
                chart: rec.chart_path.clone(),
                status: "missing".to_string(),
                url: self.url_for(&rec),
                record: rec.clone(),
            };
            if let Some(rel) = releases.get(&format!("{ns}/{}", rec.name)) {
                view.status = rel.status.clone();
                if !rel.chart.is_empty() {
                    view.chart = rel.chart.clone();
                }
            }
            views.push(view);
        }
        views.sort_by(|a, b| a.record.name.cmp(&b.record.name));
        Ok(views)
    }

    fn display_namespace(&self, rec: &Record) -> String {
        if rec.orphaned && !rec.namespace.is_empty() {
            return rec.namespace.clone();
        }
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

    async fn fill_discovered_service(
        &self,
        release: &str,
        privileged: bool,
        e: Exposure,
    ) -> Exposure {
        if !e.service.is_empty() || self.discoverer.is_none() {
            return e;
        }
        let mut namespace = self.helm.namespace().to_string();
        if privileged && !self.priv_ns.is_empty() {
            namespace = self.priv_ns.clone();
        }
        let d = self.discoverer.as_ref().unwrap();
        match d.services_for_release(&namespace, release).await {
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
        };
        if let Ok(rel) = self.helm_for(rec.privileged).get(name).await {
            view.status = rel.status.clone();
            if !rel.chart.is_empty() {
                view.chart = rel.chart;
            }
        }
        Ok(view)
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
