//! Per-app CPU, memory and network usage for the Installed apps view.
//!
//! The appliance runs no metrics-server and no Prometheus (docs/monitoring.md),
//! so usage comes from the dependency-free kubelet read the operator docs
//! already name: `/api/v1/nodes/<node>/proxy/stats/summary` for per-pod CPU,
//! working set and cumulative network counters. It is fetched once per node
//! (the node is a single Talos host) and aggregated in memory. The caller polls
//! at 5s; the report is cached ~4s and collections are single-flight so a
//! polling tab cannot hammer the API server.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::Utc;

use crate::kube;

type PodKey = (String, String);
type PodAppMap = HashMap<PodKey, String>;
const NODE_TTL: Duration = Duration::from_secs(60);
const POD_TTL: Duration = Duration::from_secs(30);
const REPORT_TTL: Duration = Duration::from_secs(4);
/// A counter sample older than this is not a usable rate baseline (the pod was
/// probably rescheduled), so the next sample starts a new baseline.
const RATE_GAP: Duration = Duration::from_secs(30);
const ERROR_LOG_INTERVAL: Duration = Duration::from_secs(60);

/// One installed app's usage row.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppUsage {
    pub name: String,
    pub namespace: String,
    /// Running pods attributed to the app (not rate samples).
    pub pods: usize,
    pub cpu_millicores: Option<f64>,
    pub memory_bytes: Option<u64>,
    pub network_rx_bytes_per_sec: Option<f64>,
    pub network_tx_bytes_per_sec: Option<f64>,
}

/// The bulk `GET /api/apps/usage` response. Always HTTP 200: `available:false`
/// carries the reason so the UI hides the metrics column instead of erroring.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageReport {
    pub available: bool,
    pub sampled_at: String,
    pub error: String,
    pub apps: Vec<AppUsage>,
}

impl Default for UsageReport {
    fn default() -> Self {
        Self {
            available: false,
            sampled_at: Utc::now().to_rfc3339(),
            error: String::new(),
            apps: Vec::new(),
        }
    }
}

impl UsageReport {
    pub fn unavailable(error: impl Into<String>) -> Self {
        Self {
            available: false,
            sampled_at: Utc::now().to_rfc3339(),
            error: error.into(),
            apps: Vec::new(),
        }
    }
}

/// Per-pod values from one `stats/summary` response.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PodStats {
    pub cpu_nano_cores: Option<f64>,
    pub working_set_bytes: Option<u64>,
    /// Pod-level cumulative counters from the same summary (`network.*Bytes`).
    pub rx_bytes: Option<u64>,
    pub tx_bytes: Option<u64>,
}

/// Parse `stats/summary` into `(namespace, pod) -> PodStats`. Missing usage
/// fields stay `None` rather than 0 so an idle-but-unknown pod is not reported
/// as measured.
pub fn parse_summary_pods(value: &serde_json::Value) -> HashMap<PodKey, PodStats> {
    let mut out = HashMap::new();
    let Some(pods) = value.get("pods").and_then(|v| v.as_array()) else {
        return out;
    };
    for pod in pods {
        let Some(name) = pod.pointer("/podRef/name").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(namespace) = pod.pointer("/podRef/namespace").and_then(|v| v.as_str()) else {
            continue;
        };
        let cpu = pod
            .pointer("/cpu/usageNanoCores")
            .and_then(|v| v.as_f64())
            .filter(|v| v.is_finite() && *v >= 0.0);
        let memory = pod
            .pointer("/memory/workingSetBytes")
            .and_then(|v| v.as_u64());
        let rx = pod.pointer("/network/rxBytes").and_then(|v| v.as_u64());
        let tx = pod.pointer("/network/txBytes").and_then(|v| v.as_u64());
        out.insert(
            (namespace.to_string(), name.to_string()),
            PodStats {
                cpu_nano_cores: cpu,
                working_set_bytes: memory,
                rx_bytes: rx,
                tx_bytes: tx,
            },
        );
    }
    out
}

/// Bytes/second between two counter samples. `None` when there is no usable
/// baseline: first sample, a reset (`current < previous`) or a stale gap.
pub fn compute_rates(
    previous: Option<(u64, u64, Instant)>,
    current: (u64, u64),
    now: Instant,
) -> Option<(f64, f64)> {
    let (prev_rx, prev_tx, at) = previous?;
    let dt = now.checked_duration_since(at)?;
    if dt.is_zero() || dt > RATE_GAP {
        return None;
    }
    if current.0 < prev_rx || current.1 < prev_tx {
        return None;
    }
    let secs = dt.as_secs_f64();
    Some((
        (current.0 - prev_rx) as f64 / secs,
        (current.1 - prev_tx) as f64 / secs,
    ))
}

/// Attribute pods to app records. A namespace with exactly one recorded app
/// maps directly (every per-app isolation namespace, plus a single legacy
/// app); a shared namespace needs the official-chart `app.kubernetes.io/instance`
/// label. Unattributed pods are omitted rather than guessed.
pub fn map_pods_to_apps(pods: &serde_json::Value, targets: &[(String, String)]) -> PodAppMap {
    let mut by_namespace: HashMap<&str, Vec<&str>> = HashMap::new();
    for (app, namespace) in targets {
        if namespace.is_empty() {
            continue;
        }
        let apps = by_namespace.entry(namespace.as_str()).or_default();
        if !apps.contains(&app.as_str()) {
            apps.push(app.as_str());
        }
    }

    let mut out = HashMap::new();
    let Some(items) = pods.get("items").and_then(|v| v.as_array()) else {
        return out;
    };
    for item in items {
        let Some(namespace) = item.pointer("/metadata/namespace").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(name) = item.pointer("/metadata/name").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(apps) = by_namespace.get(namespace) else {
            continue;
        };
        let app = if apps.len() == 1 {
            apps[0]
        } else {
            let instance = item
                .pointer("/metadata/labels")
                .and_then(|v| v.get("app.kubernetes.io/instance"))
                .and_then(|v| v.as_str());
            match instance {
                Some(instance) if apps.contains(&instance) => instance,
                _ => continue,
            }
        };
        out.insert((namespace.to_string(), name.to_string()), app.to_string());
    }
    out
}

/// Sum the per-pod values into one row per recorded app. Apps with no pods
/// keep their row with null usage so the UI can distinguish "idle" from
/// "metrics unavailable".
pub fn aggregate(
    targets: &[(String, String)],
    pod_map: &PodAppMap,
    summary: &HashMap<PodKey, PodStats>,
) -> Vec<AppUsage> {
    let mut rows: HashMap<String, AppUsage> = HashMap::new();
    for (app, namespace) in targets {
        rows.entry(app.clone()).or_insert_with(|| AppUsage {
            name: app.clone(),
            namespace: namespace.clone(),
            ..Default::default()
        });
    }
    let mut cpu_sums: HashMap<String, (f64, bool)> = HashMap::new();
    let mut mem_sums: HashMap<String, (u64, bool)> = HashMap::new();
    for (pod, app) in pod_map {
        let Some(row) = rows.get_mut(app) else {
            continue;
        };
        row.pods += 1;
        if let Some(stats) = summary.get(pod) {
            if let Some(cpu) = stats.cpu_nano_cores {
                let entry = cpu_sums.entry(app.clone()).or_insert((0.0, false));
                entry.0 += cpu;
                entry.1 = true;
            }
            if let Some(mem) = stats.working_set_bytes {
                let entry = mem_sums.entry(app.clone()).or_insert((0, false));
                entry.0 = entry.0.saturating_add(mem);
                entry.1 = true;
            }
        }
    }
    for row in rows.values_mut() {
        if let Some((cpu, seen)) = cpu_sums.get(&row.name) {
            if *seen {
                row.cpu_millicores = Some(cpu / 1_000_000.0);
            }
        }
        if let Some((mem, seen)) = mem_sums.get(&row.name) {
            if *seen {
                row.memory_bytes = Some(*mem);
            }
        }
    }
    let mut out: Vec<AppUsage> = rows.into_values().collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Report cache and per-pod network baselines, shared through the app Manager.
#[derive(Default)]
pub struct UsageCollector {
    inner: Mutex<Inner>,
    /// Serialises collections so overlapping polls share one set of kubectl
    /// reads instead of racing on the per-pod network baseline.
    collect_mu: tokio::sync::Mutex<()>,
}

#[derive(Default)]
struct Inner {
    nodes: Option<(Instant, Vec<String>)>,
    pod_map: Option<(Instant, PodAppMap)>,
    pod_map_targets: Vec<(String, String)>,
    prev_network: HashMap<PodKey, (u64, u64, Instant)>,
    report: Option<(Instant, UsageReport)>,
    last_error_log: Option<Instant>,
}

impl UsageCollector {
    pub async fn collect(&self, kube: &kube::Client, targets: &[(String, String)]) -> UsageReport {
        if let Ok(inner) = self.inner.lock() {
            if let Some((at, report)) = &inner.report {
                if at.elapsed() < REPORT_TTL {
                    return report.clone();
                }
            }
        }

        // Single-flight: a concurrent poll waits here and then re-checks the
        // cache, so overlapping clients share one collection.
        let _guard = self.collect_mu.lock().await;
        if let Ok(inner) = self.inner.lock() {
            if let Some((at, report)) = &inner.report {
                if at.elapsed() < REPORT_TTL {
                    return report.clone();
                }
            }
        }

        match self.collect_inner(kube, targets).await {
            Ok(report) => {
                if let Ok(mut inner) = self.inner.lock() {
                    inner.report = Some((Instant::now(), report.clone()));
                    inner.last_error_log = None;
                }
                report
            }
            Err(e) => {
                let report = UsageReport::unavailable(e.clone());
                if let Ok(mut inner) = self.inner.lock() {
                    // A refused RBAC grant or a down kubelet would otherwise
                    // log on every 5s poll.
                    let should_log = inner
                        .last_error_log
                        .map(|at| at.elapsed() >= ERROR_LOG_INTERVAL)
                        .unwrap_or(true);
                    if should_log {
                        tracing::warn!("app usage collection failed: {e}");
                        inner.last_error_log = Some(Instant::now());
                    }
                    inner.report = Some((Instant::now(), report.clone()));
                }
                report
            }
        }
    }

    async fn collect_inner(
        &self,
        kube: &kube::Client,
        targets: &[(String, String)],
    ) -> Result<UsageReport, String> {
        let nodes = self.nodes(kube).await?;
        let pod_map = self.pod_map(kube, targets).await?;

        let mut summary = HashMap::new();
        for node in &nodes {
            let path = format!("/api/v1/nodes/{node}/proxy/stats/summary");
            let body = kube.get_raw(&path).await?;
            let value: serde_json::Value = serde_json::from_str(body.trim())
                .map_err(|e| format!("parsing stats/summary: {e}"))?;
            summary.extend(parse_summary_pods(&value));
        }

        // Network comes from the same summary: the pod-level cumulative
        // rxBytes/txBytes, converted to a rate against the previous sample.
        let now = Instant::now();
        let counters: HashMap<PodKey, (u64, u64)> = summary
            .iter()
            .filter(|(key, _)| pod_map.contains_key(*key))
            .filter_map(|(key, stats)| Some((key.clone(), (stats.rx_bytes?, stats.tx_bytes?))))
            .collect();
        let mut prev = self
            .inner
            .lock()
            .map(|mut inner| std::mem::take(&mut inner.prev_network))
            .unwrap_or_default();
        let mut pod_rates: HashMap<PodKey, (f64, f64)> = HashMap::new();
        for (key, current) in &counters {
            if let Some(rate) = compute_rates(prev.get(key).copied(), *current, now) {
                pod_rates.insert(key.clone(), rate);
            }
        }
        prev.clear();
        prev.extend(
            counters
                .into_iter()
                .map(|(key, (rx, tx))| (key, (rx, tx, now))),
        );
        let rates = rates_by_app(&pod_map, &pod_rates);
        if let Ok(mut inner) = self.inner.lock() {
            inner.prev_network = prev;
        }

        let mut apps = aggregate(targets, &pod_map, &summary);
        for app in &mut apps {
            if let Some((rx, tx)) = rates.get(&app.name) {
                app.network_rx_bytes_per_sec = *rx;
                app.network_tx_bytes_per_sec = *tx;
            }
        }

        Ok(UsageReport {
            available: true,
            sampled_at: Utc::now().to_rfc3339(),
            error: String::new(),
            apps,
        })
    }

    async fn nodes(&self, kube: &kube::Client) -> Result<Vec<String>, String> {
        if let Ok(inner) = self.inner.lock() {
            if let Some((at, nodes)) = &inner.nodes {
                if at.elapsed() < NODE_TTL {
                    return Ok(nodes.clone());
                }
            }
        }
        let value = kube.list_cluster("nodes").await?;
        let nodes: Vec<String> = value
            .get("items")
            .and_then(|v| v.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        item.pointer("/metadata/name")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                    })
                    .filter(|name| kube::is_dns1123_subdomain(name))
                    .collect()
            })
            .unwrap_or_default();
        if nodes.is_empty() {
            return Err("no Kubernetes nodes found".to_string());
        }
        if let Ok(mut inner) = self.inner.lock() {
            inner.nodes = Some((Instant::now(), nodes.clone()));
        }
        Ok(nodes)
    }

    async fn pod_map(
        &self,
        kube: &kube::Client,
        targets: &[(String, String)],
    ) -> Result<PodAppMap, String> {
        if let Ok(inner) = self.inner.lock() {
            if let Some((at, map)) = &inner.pod_map {
                if at.elapsed() < POD_TTL && inner.pod_map_targets == targets {
                    return Ok(map.clone());
                }
            }
        }
        // List per recorded namespace instead of `get pods -A`: the API holds
        // only namespaced pod Roles, so a cluster-scoped list is Forbidden.
        let mut namespaces: Vec<String> = targets
            .iter()
            .map(|(_, namespace)| namespace.clone())
            .filter(|namespace| !namespace.is_empty())
            .collect();
        namespaces.sort();
        namespaces.dedup();
        let mut items = Vec::new();
        for namespace in &namespaces {
            let list = kube.list_pods(namespace).await?;
            if let Some(pods) = list.get("items").and_then(|v| v.as_array()) {
                items.extend(pods.iter().cloned());
            }
        }
        let pods = serde_json::json!({ "items": items });
        let map = map_pods_to_apps(&pods, targets);
        if let Ok(mut inner) = self.inner.lock() {
            inner.pod_map = Some((Instant::now(), map.clone()));
            inner.pod_map_targets = targets.to_vec();
        }
        Ok(map)
    }
}

fn rates_by_app(
    pod_map: &PodAppMap,
    pod_rates: &HashMap<PodKey, (f64, f64)>,
) -> HashMap<String, (Option<f64>, Option<f64>)> {
    let mut rx: HashMap<String, f64> = HashMap::new();
    let mut tx: HashMap<String, f64> = HashMap::new();
    for (pod, app) in pod_map {
        if let Some((pod_rx, pod_tx)) = pod_rates.get(pod) {
            *rx.entry(app.clone()).or_insert(0.0) += pod_rx;
            *tx.entry(app.clone()).or_insert(0.0) += pod_tx;
        }
    }
    let mut out = HashMap::new();
    for app in pod_map.values() {
        out.insert(app.clone(), (rx.get(app).copied(), tx.get(app).copied()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary_fixture() -> serde_json::Value {
        serde_json::json!({
            "pods": [
                {
                    "podRef": { "name": "jellyfin-abc", "namespace": "naslos-app-4" },
                    "cpu": { "usageNanoCores": 42_500_000 },
                    "memory": { "workingSetBytes": 734_003_200 },
                    "network": { "rxBytes": 12000, "txBytes": 3000 }
                },
                {
                    "podRef": { "name": "jellyfin-def", "namespace": "naslos-app-4" },
                    "cpu": { "usageNanoCores": 7_500_000 },
                    "memory": { "workingSetBytes": 26_214_400 },
                    "network": { "rxBytes": 1000, "txBytes": 500 }
                }
            ]
        })
    }

    #[test]
    fn parses_summary_pods_without_inventing_zeroes() {
        let parsed = parse_summary_pods(&summary_fixture());
        let first = parsed
            .get(&("naslos-app-4".to_string(), "jellyfin-abc".to_string()))
            .unwrap();
        assert_eq!(first.cpu_nano_cores, Some(42_500_000.0));
        assert_eq!(first.working_set_bytes, Some(734_003_200));
        assert_eq!(first.rx_bytes, Some(12_000));
        assert_eq!(first.tx_bytes, Some(3_000));
        assert!(parsed.contains_key(&("naslos-app-4".to_string(), "jellyfin-def".to_string())));
        assert!(!parsed.contains_key(&("naslos-app-4".to_string(), "missing".to_string())));
    }

    #[test]
    fn summary_network_counters_are_optional() {
        let parsed = parse_summary_pods(&serde_json::json!({
            "pods": [
                {
                    "podRef": { "name": "no-network", "namespace": "naslos-app-4" },
                    "cpu": { "usageNanoCores": 1_000_000 }
                }
            ]
        }));
        let stats = parsed
            .get(&("naslos-app-4".to_string(), "no-network".to_string()))
            .unwrap();
        // hostNetwork pods (and older kubelets) may omit the counters.
        assert!(stats.rx_bytes.is_none());
        assert!(stats.tx_bytes.is_none());
    }

    #[test]
    fn rates_need_two_samples_and_reject_resets_and_gaps() {
        let now = Instant::now();
        assert!(compute_rates(None, (100, 50), now).is_none());
        let one_second = now.checked_sub(Duration::from_secs(1)).unwrap();
        let rate = compute_rates(Some((100, 50, one_second)), (300, 150), now).unwrap();
        assert!((rate.0 - 200.0).abs() < 1e-6);
        assert!((rate.1 - 100.0).abs() < 1e-6);
        // Counter reset.
        assert!(compute_rates(Some((300, 150, one_second)), (10, 20), now).is_none());
        // Stale baseline.
        let stale = now.checked_sub(Duration::from_secs(31)).unwrap();
        assert!(compute_rates(Some((100, 50, stale)), (300, 150), now).is_none());
    }

    #[test]
    fn maps_pods_by_namespace_and_shared_label() {
        let pods = serde_json::json!({
            "items": [
                { "metadata": { "name": "jellyfin-1", "namespace": "naslos-app-4" } },
                { "metadata": { "name": "radarr-1", "namespace": "naslos-apps",
                                "labels": { "app.kubernetes.io/instance": "radarr" } } },
                { "metadata": { "name": "unknown-1", "namespace": "naslos-apps" } },
                { "metadata": { "name": "other-1", "namespace": "kube-system" } }
            ]
        });
        let targets = vec![
            ("jellyfin".to_string(), "naslos-app-4".to_string()),
            ("radarr".to_string(), "naslos-apps".to_string()),
            ("sonarr".to_string(), "naslos-apps".to_string()),
        ];
        let map = map_pods_to_apps(&pods, &targets);
        assert_eq!(
            map.get(&("naslos-app-4".to_string(), "jellyfin-1".to_string())),
            Some(&"jellyfin".to_string())
        );
        assert_eq!(
            map.get(&("naslos-apps".to_string(), "radarr-1".to_string())),
            Some(&"radarr".to_string())
        );
        assert!(!map.contains_key(&("naslos-apps".to_string(), "unknown-1".to_string())));
        assert!(!map.contains_key(&("kube-system".to_string(), "other-1".to_string())));
    }

    #[test]
    fn aggregates_pods_into_one_row_per_app() {
        let targets = vec![
            ("jellyfin".to_string(), "naslos-app-4".to_string()),
            ("radarr".to_string(), "naslos-app-5".to_string()),
        ];
        let mut pod_map = HashMap::new();
        pod_map.insert(
            ("naslos-app-4".to_string(), "jellyfin-abc".to_string()),
            "jellyfin".to_string(),
        );
        pod_map.insert(
            ("naslos-app-4".to_string(), "jellyfin-def".to_string()),
            "jellyfin".to_string(),
        );
        let summary = parse_summary_pods(&summary_fixture());
        let rows = aggregate(&targets, &pod_map, &summary);
        let jellyfin = rows.iter().find(|r| r.name == "jellyfin").unwrap();
        assert_eq!(jellyfin.pods, 2);
        assert_eq!(jellyfin.cpu_millicores, Some(50.0));
        assert_eq!(jellyfin.memory_bytes, Some(760_217_600));
        let radarr = rows.iter().find(|r| r.name == "radarr").unwrap();
        assert_eq!(radarr.pods, 0);
        assert!(radarr.cpu_millicores.is_none());
        assert!(radarr.memory_bytes.is_none());
    }
}
