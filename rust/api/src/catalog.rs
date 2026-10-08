//! The Naslos app catalog (port of `api/internal/catalog`).
//!
//! Each app is a chart directory with a sibling `naslos-app.yaml` install
//! config; user-added repositories override the official repository on name
//! collision.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

use crate::chartsrepo::{is_dns1123_label, DEFAULT_CHANNEL};

const RELEASE_NAME_TEMPLATE: &str = "{{ .Release.Name }}";
const MANIFEST_FILE: &str = "naslos-app.yaml";
const CHART_FILE: &str = "Chart.yaml";

/// A catalog entry for an installable app.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct App {
    pub name: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub description: String,
    pub category: String,
    pub icon: String,
    pub version: String,
    pub chart: String,
    pub repository: String,
    #[serde(default)]
    pub schema: serde_json::Value,
    #[serde(rename = "defaultValues")]
    pub default_values: serde_json::Value,
    #[serde(default)]
    pub ports: Vec<i64>,
    pub website: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub source: String,
    pub channel: String,
    #[serde(default)]
    pub channels: Vec<String>,
    #[serde(rename = "chartPath")]
    pub chart_path: String,
    #[serde(default)]
    pub services: Vec<Service>,
    #[serde(default)]
    pub exposure: ExposureDefaults,
    #[serde(default, skip_serializing_if = "is_false")]
    pub privileged: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// A route target declared by an app manifest.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Service {
    pub name: String,
    pub port: i64,
    #[serde(default)]
    pub scheme: String,
}

/// The exposure toggles shown by the install UI.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExposureDefaults {
    #[serde(default)]
    pub subdomain: String,
    #[serde(default)]
    pub tls: bool,
    #[serde(default)]
    pub auth: bool,
    #[serde(default, rename = "localOnly")]
    pub local_only: bool,
}

/// The catalog index response.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub name: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub description: String,
    pub category: String,
    pub icon: String,
    pub version: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub source: String,
    pub channel: String,
    #[serde(default)]
    pub channels: Vec<String>,
    #[serde(rename = "chartPath")]
    pub chart_path: String,
}

/// One repository/channel working tree to scan.
#[derive(Debug, Clone, Default)]
pub struct SourceRef {
    pub name: String,
    pub display_name: String,
    pub channel: String,
    pub dir: String,
    pub official: bool,
}

/// The parsed `naslos-app.yaml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    #[serde(default)]
    name: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    icon: String,
    #[serde(default)]
    website: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    schema: Option<serde_yaml::Value>,
    #[serde(default)]
    default_values: Option<serde_json::Value>,
    #[serde(default)]
    services: Vec<Service>,
    #[serde(default)]
    exposure: ExposureDefaults,
    #[serde(default)]
    privileged: bool,
}

#[derive(Clone)]
struct Candidate {
    app: App,
    ref_: SourceRef,
}

/// An immutable snapshot of apps from a set of repositories.
#[derive(Default)]
pub struct Catalog {
    apps: HashMap<String, App>,
}

impl Catalog {
    /// Scan every source/channel working tree and merge them into a catalog.
    pub fn load(refs: &[SourceRef]) -> Self {
        let mut by_name: HashMap<String, Vec<Candidate>> = HashMap::new();
        for ref_ in refs {
            for cand in scan_ref(ref_) {
                by_name.entry(cand.app.name.clone()).or_default().push(cand);
            }
        }

        let mut apps = HashMap::new();
        for (name, candidates) in by_name {
            let winner = pick_winner(&candidates);
            let mut app = winner.app.clone();

            let mut channels: Vec<String> = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for cand in &candidates {
                if cand.ref_.name != winner.ref_.name {
                    continue;
                }
                if !seen.insert(cand.ref_.channel.clone()) {
                    continue;
                }
                channels.push(cand.ref_.channel.clone());
            }
            channels.sort();
            app.channels = channels;

            apps.insert(name, app);
        }
        Self { apps }
    }

    /// All catalog entries as a summary list, sorted by name.
    pub fn list(&self) -> Vec<CatalogEntry> {
        let mut entries: Vec<CatalogEntry> = self
            .apps
            .values()
            .map(|app| CatalogEntry {
                name: app.name.clone(),
                display_name: app.display_name.clone(),
                description: app.description.clone(),
                category: app.category.clone(),
                icon: app.icon.clone(),
                version: app.version.clone(),
                tags: app.tags.clone(),
                source: app.source.clone(),
                channel: app.channel.clone(),
                channels: app.channels.clone(),
                chart_path: app.chart_path.clone(),
            })
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        entries
    }

    /// A full app definition by name (a deep copy).
    pub fn get(&self, name: &str) -> Result<App, String> {
        self.apps
            .get(name)
            .cloned()
            .ok_or_else(|| format!("app {name:?} not found in catalog"))
    }

    pub fn has(&self, name: &str) -> bool {
        self.apps.contains_key(name)
    }

    pub fn schema(&self, name: &str) -> Result<serde_json::Value, String> {
        Ok(self.get(name)?.schema)
    }

    pub fn default_values(&self, name: &str) -> Result<serde_json::Value, String> {
        Ok(self.get(name)?.default_values)
    }
}

/// User-over-official precedence, then Prod, then alphabetical.
fn pick_winner(candidates: &[Candidate]) -> Candidate {
    let official: Vec<&Candidate> = candidates.iter().filter(|c| c.ref_.official).collect();
    let user: Vec<&Candidate> = candidates.iter().filter(|c| !c.ref_.official).collect();
    let mut pool = if user.is_empty() { official } else { user };
    pool.sort_by(|a, b| {
        let (ri, rj) = (&a.ref_, &b.ref_);
        if ri.name != rj.name {
            return ri.name.cmp(&rj.name);
        }
        if ri.channel == DEFAULT_CHANNEL {
            return std::cmp::Ordering::Less;
        }
        if rj.channel == DEFAULT_CHANNEL {
            return std::cmp::Ordering::Greater;
        }
        ri.channel.cmp(&rj.channel)
    });
    (*pool[0]).clone()
}

fn scan_ref(ref_: &SourceRef) -> Vec<Candidate> {
    let Ok(entries) = std::fs::read_dir(Path::new(&ref_.dir).join("apps")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let folder = entry.file_name().to_string_lossy().into_owned();
        if let Ok(app) = load_app(ref_, &folder) {
            out.push(Candidate {
                app,
                ref_: ref_.clone(),
            });
        }
    }
    out
}

fn load_app(ref_: &SourceRef, folder: &str) -> Result<App, String> {
    let data = std::fs::read(
        Path::new(&ref_.dir)
            .join("apps")
            .join(folder)
            .join(MANIFEST_FILE),
    )
    .map_err(|e| format!("reading {MANIFEST_FILE}: {e}"))?;
    let m: Manifest =
        serde_yaml::from_slice(&data).map_err(|e| format!("parsing {MANIFEST_FILE}: {e}"))?;
    validate_manifest(&m, folder)?;

    let schema = match &m.schema {
        Some(s) => {
            serde_json::to_value(s).map_err(|e| format!("encoding schema for {:?}: {e}", m.name))?
        }
        None => serde_json::Value::Null,
    };

    let mut app = App {
        name: m.name.clone(),
        display_name: display_or(&m.display_name, &m.name),
        description: m.description.clone(),
        category: m.category.clone(),
        icon: m.icon.clone(),
        website: m.website.clone(),
        version: m.version.clone(),
        tags: normalize_list(&m.tags),
        default_values: m.default_values.clone().unwrap_or(serde_json::Value::Null),
        services: normalize_services(&m.services),
        exposure: m.exposure.clone(),
        privileged: m.privileged,
        repository: ref_.name.clone(),
        source: ref_.name.clone(),
        channel: ref_.channel.clone(),
        chart_path: format!("apps/{folder}"),
        chart: folder.to_string(),
        schema,
        ..Default::default()
    };
    if let Some(v) = read_chart_version(&Path::new(&ref_.dir).join("apps").join(folder)) {
        app.version = v;
    }
    app.ports = service_ports(&app.services);
    Ok(app)
}

fn read_chart_version(dir: &Path) -> Option<String> {
    let data = std::fs::read(dir.join(CHART_FILE)).ok()?;
    #[derive(Deserialize)]
    struct Meta {
        #[serde(default)]
        version: String,
    }
    serde_yaml::from_slice::<Meta>(&data)
        .ok()
        .map(|m| m.version)
}

fn validate_manifest(m: &Manifest, folder: &str) -> Result<(), String> {
    if m.name.is_empty() {
        return Err("app manifest is missing name".to_string());
    }
    if m.name != folder {
        return Err(format!(
            "app name {:?} does not match folder {folder:?}",
            m.name
        ));
    }
    if !is_dns1123_label(&m.name) {
        return Err(format!(
            "app name {:?} must be a lowercase DNS-1123 label",
            m.name
        ));
    }
    for svc in &m.services {
        if svc.port <= 0 || svc.port > 65535 {
            return Err(format!(
                "app {:?} service {:?} has invalid port {}",
                m.name, svc.name, svc.port
            ));
        }
        if !svc.scheme.is_empty() && svc.scheme != "http" && svc.scheme != "https" {
            return Err(format!(
                "app {:?} service {:?} has invalid scheme {:?}",
                m.name, svc.name, svc.scheme
            ));
        }
        if !valid_service_name_template(&svc.name) {
            return Err(format!(
                "app {:?} service name {:?} may only be a literal or {{ .Release.Name }}",
                m.name, svc.name
            ));
        }
    }
    Ok(())
}

/// Templating is limited to the release name.
fn valid_service_name_template(name: &str) -> bool {
    let name = name.trim();
    if name.is_empty() {
        return false;
    }
    if !name.contains("{{") {
        return is_dns1123_label(name);
    }
    let remainder = name.replace(RELEASE_NAME_TEMPLATE, "");
    if remainder.contains("{{") || remainder.contains("}}") {
        return false;
    }
    let remainder = remainder.trim();
    if remainder.is_empty() {
        return true;
    }
    is_dns1123_label(remainder)
}

fn normalize_services(input: &[Service]) -> Vec<Service> {
    input
        .iter()
        .map(|s| {
            let mut s = s.clone();
            if s.scheme.is_empty() {
                s.scheme = "http".to_string();
            }
            s
        })
        .collect()
}

fn service_ports(services: &[Service]) -> Vec<i64> {
    services
        .iter()
        .filter(|s| s.port > 0)
        .map(|s| s.port)
        .collect()
}

fn normalize_list(input: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for v in input {
        let v = v.trim();
        if v.is_empty() || !seen.insert(v.to_string()) {
            continue;
        }
        out.push(v.to_string());
    }
    out
}

fn display_or(value: &str, fallback: &str) -> String {
    if value.trim().is_empty() {
        fallback.to_string()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_app(root: &Path, folder: &str, manifest: &str, chart: &str) {
        let dir = root.join("apps").join(folder);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::File::create(dir.join(MANIFEST_FILE))
            .unwrap()
            .write_all(manifest.as_bytes())
            .unwrap();
        if !chart.is_empty() {
            std::fs::File::create(dir.join(CHART_FILE))
                .unwrap()
                .write_all(chart.as_bytes())
                .unwrap();
        }
    }

    fn ref_(name: &str, channel: &str, dir: &str, official: bool) -> SourceRef {
        SourceRef {
            name: name.into(),
            channel: channel.into(),
            dir: dir.into(),
            official,
            ..Default::default()
        }
    }

    #[test]
    fn loads_a_simple_app() {
        let dir = tempfile::tempdir().unwrap();
        write_app(
            dir.path(),
            "nginx",
            "name: nginx\ndisplayName: Nginx\ndescription: web\nservices:\n  - name: \"{{ .Release.Name }}\"\n    port: 80\n",
            "apiVersion: v2\nname: nginx\nversion: 1.2.3\n",
        );
        let c = Catalog::load(&[ref_(
            "official",
            "Prod",
            &dir.path().display().to_string(),
            true,
        )]);
        let app = c.get("nginx").unwrap();
        assert_eq!(app.display_name, "Nginx");
        assert_eq!(app.version, "1.2.3", "Chart.yaml version wins");
        assert_eq!(app.ports, vec![80]);
        assert_eq!(app.services[0].scheme, "http");
    }

    #[test]
    fn user_source_overrides_official() {
        let official = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        write_app(official.path(), "app", "name: app\nversion: 1.0.0\n", "");
        write_app(user.path(), "app", "name: app\nversion: 9.9.9\n", "");
        let c = Catalog::load(&[
            ref_(
                "official",
                "Prod",
                &official.path().display().to_string(),
                true,
            ),
            ref_("mine", "Prod", &user.path().display().to_string(), false),
        ]);
        assert_eq!(c.get("app").unwrap().version, "9.9.9");
        assert_eq!(c.get("app").unwrap().source, "mine");
    }

    #[test]
    fn manifest_name_must_match_folder() {
        let dir = tempfile::tempdir().unwrap();
        write_app(dir.path(), "nginx", "name: other\nversion: 1.0.0\n", "");
        let c = Catalog::load(&[ref_(
            "official",
            "Prod",
            &dir.path().display().to_string(),
            true,
        )]);
        assert!(!c.has("nginx"));
    }

    #[test]
    fn channels_are_collected_for_the_winning_source() {
        let dir = tempfile::tempdir().unwrap();
        write_app(dir.path(), "app", "name: app\nversion: 1.0.0\n", "");
        let c = Catalog::load(&[
            ref_("official", "Prod", &dir.path().display().to_string(), true),
            ref_(
                "official",
                "Develop",
                &dir.path().display().to_string(),
                true,
            ),
        ]);
        let app = c.get("app").unwrap();
        assert_eq!(app.channels, vec!["Develop", "Prod"]);
        assert_eq!(app.channel, "Prod", "Prod wins");
    }

    #[test]
    fn service_name_template_validation() {
        assert!(valid_service_name_template("{{ .Release.Name }}"));
        assert!(valid_service_name_template("my-app"));
        assert!(!valid_service_name_template("my-{{ .Release.Name }}"));
        assert!(!valid_service_name_template("{{ .Values.foo }}"));
        assert!(!valid_service_name_template(""));
        assert!(!valid_service_name_template("Bad Name"));
    }
}
