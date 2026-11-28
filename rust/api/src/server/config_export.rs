//! System configuration export (FR-CFG).
//!
//! The export is a versioned, allow-listed projection of the declarative
//! records, not a dump of the store files. Every section is written through an
//! explicit struct so a newly added secret-bearing field cannot leak by
//! accident. Secret material is excluded on purpose:
//!
//! - no `credentialsSecret` / `credentialFields` / rendered domain `solver`;
//! - no app value the catalog marks as a secret (or any key that looks like a
//!   credential), and no custom env-var value;
//! - no Buddy identity, KEK or peer key material;
//! - no Kubernetes Secret is read, so no Secret value can be included;
//! - no runtime result/timestamp field (`lastRun`, `lastError`, ...).
//!
//! Import/restore is deliberately not implemented (docs/spec.md §3.15). The
//! validation rules an importer must follow are documented there and in
//! `docs/api.md`.

use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashSet};

use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

use crate::app_env::EnvMeta;
use crate::apps::{Exposure, Record};
use crate::catalog::App;
use crate::providers::Provider;
use crate::state::AppState;

/// The export envelope discriminator.
pub const KIND: &str = "naslos-config-export";

/// The only schema version this API writes. An importer MUST reject a newer
/// version rather than guess.
pub const SCHEMA_VERSION: u32 = 1;

/// The envelope and its sections, in a stable order.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Export {
    pub kind: &'static str,
    pub schema_version: u32,
    pub generated_at: DateTime<Utc>,
    pub naslos_version: String,
    pub base_domain: String,
    pub shares: Vec<ShareExport>,
    pub storage_schedules: Vec<StorageScheduleExport>,
    pub buddy_schedules: Vec<BuddyScheduleExport>,
    pub cloud_destinations: Vec<CloudDestinationExport>,
    pub cloud_schedules: Vec<CloudScheduleExport>,
    pub domains: Vec<DomainExport>,
    pub ddns_entries: Vec<DdnsEntryExport>,
    pub apps: Vec<AppExport>,
    pub dataset_registry: Vec<DatasetExport>,
}

/// A share definition without the server-owned `createdAt` timestamp.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareExport {
    pub name: String,
    pub path: String,
    pub protocol: String,
    pub description: String,
    pub read_only: bool,
    pub browseable: bool,
    pub allowed_hosts: Vec<String>,
    pub valid_users: Vec<String>,
    pub valid_groups: Vec<String>,
    pub time_machine: bool,
    pub no_root_squash: bool,
    pub shadow_copies: bool,
    pub audit: bool,
    pub enabled: bool,
}

impl ShareExport {
    fn project(share: &crate::shares::Share) -> Self {
        Self {
            name: share.name.clone(),
            path: share.path.clone(),
            protocol: share.protocol.as_str().to_string(),
            description: share.description.clone(),
            read_only: share.read_only,
            browseable: share.browseable,
            allowed_hosts: share.allowed_hosts.clone(),
            valid_users: share.valid_users.clone(),
            valid_groups: share.valid_groups.clone(),
            time_machine: share.time_machine,
            no_root_squash: share.no_root_squash,
            shadow_copies: share.shadow_copies,
            audit: share.audit,
            enabled: share.enabled,
        }
    }
}

/// A scheduled storage task without its runtime result fields.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageScheduleExport {
    pub id: String,
    pub kind: crate::server::storage_schedules::ScheduleKind,
    pub target: String,
    pub cadence: crate::schedule::Cadence,
    pub run_at: String,
    pub keep: usize,
    pub recursive: bool,
    pub enabled: bool,
}

impl StorageScheduleExport {
    fn project(schedule: &crate::server::storage_schedules::StorageSchedule) -> Self {
        Self {
            id: schedule.id.clone(),
            kind: schedule.kind,
            target: schedule.target.clone(),
            cadence: schedule.cadence,
            run_at: schedule.run_at.clone(),
            keep: schedule.keep,
            recursive: schedule.recursive,
            enabled: schedule.enabled,
        }
    }
}

/// A scheduled buddy backup with its destinations and without the per-receiver
/// run results (peer names are configuration, results are runtime state).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuddyScheduleExport {
    pub id: String,
    pub dataset: String,
    pub source: String,
    pub receivers: Vec<String>,
    pub cadence: crate::schedule::Cadence,
    pub run_at: String,
    pub prune_keep: usize,
    pub enabled: bool,
}

impl BuddyScheduleExport {
    fn project(schedule: &crate::server::buddy_schedules::ScheduleEntry) -> Self {
        Self {
            id: schedule.id.clone(),
            dataset: schedule.dataset.clone(),
            source: schedule.source.clone(),
            receivers: schedule.receiver_list(),
            cadence: schedule.cadence,
            run_at: schedule.run_at.clone(),
            prune_keep: schedule.prune_keep,
            enabled: schedule.enabled,
        }
    }
}

/// A cloud destination without credentials, the Secret name or any capability
/// or runtime result (FR-CFG; DM-9).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudDestinationExport {
    pub id: String,
    pub name: String,
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub prefix: String,
    pub path_style: bool,
    pub allow_insecure_http: bool,
    pub enabled: bool,
}

impl CloudDestinationExport {
    fn project(destination: &crate::cloud::Destination) -> Self {
        Self {
            id: destination.id.clone(),
            name: destination.name.clone(),
            endpoint: destination.endpoint.clone(),
            region: destination.region.clone(),
            bucket: destination.bucket.clone(),
            prefix: destination.prefix.clone(),
            path_style: destination.path_style,
            allow_insecure_http: destination.allow_insecure_http,
            enabled: destination.enabled,
        }
    }
}

/// A cloud backup schedule without its runtime results.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudScheduleExport {
    pub id: String,
    pub dataset: String,
    pub destinations: Vec<String>,
    pub cadence: crate::schedule::Cadence,
    pub run_at: String,
    pub keep_lineages: usize,
    pub enabled: bool,
}

impl CloudScheduleExport {
    fn project(schedule: &crate::cloud::schedules::ScheduleEntry) -> Self {
        Self {
            id: schedule.id.clone(),
            dataset: schedule.dataset.clone(),
            destinations: schedule.destination_list(),
            cadence: schedule.cadence,
            run_at: schedule.run_at.clone(),
            keep_lineages: schedule.keep_lineages,
            enabled: schedule.enabled,
        }
    }
}

/// A base domain without its credential Secret name, rendered solver or
/// runtime error.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DomainExport {
    pub base_domain: String,
    pub dns_provider: String,
    pub provider_config: BTreeMap<String, String>,
    pub acme_email: String,
    pub environment: String,
    pub primary: bool,
    pub sso: bool,
    pub app_records_ddns_entry: Option<String>,
}

impl DomainExport {
    fn project(domain: &crate::certs::Domain, provider: Option<&Provider>) -> Self {
        Self {
            base_domain: domain.base_domain.clone(),
            dns_provider: domain.dns_provider.clone(),
            provider_config: safe_provider_config(&domain.provider_config, provider, "cert"),
            acme_email: domain.acme_email.clone(),
            environment: domain.environment.clone(),
            primary: domain.primary,
            sso: domain.sso,
            app_records_ddns_entry: domain.app_records_ddns_entry.clone(),
        }
    }
}

/// A DDNS entry without its credential Secret name, credential field list or
/// last-run state.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DdnsEntryExport {
    pub id: String,
    pub provider: String,
    pub zone: String,
    pub record: String,
    pub record_type: String,
    pub ttl: i64,
    pub enabled: bool,
    pub provider_config: BTreeMap<String, String>,
}

impl DdnsEntryExport {
    fn project(entry: &crate::ddns::Entry, provider: Option<&Provider>) -> Self {
        Self {
            id: entry.id.clone(),
            provider: entry.provider.clone(),
            zone: entry.zone.clone(),
            record: entry.record.clone(),
            record_type: entry.record_type.clone(),
            ttl: entry.ttl,
            enabled: entry.enabled,
            provider_config: safe_provider_config(&entry.provider_config, provider, "ddns"),
        }
    }
}

/// An installed app record. `values` has the catalog's secret-marked keys (and
/// any key that looks like a credential) removed; it is omitted entirely and
/// `valuesOmitted` is true when the catalog was unavailable and the values had
/// to be dropped whole.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppExport {
    pub name: String,
    pub source: String,
    pub channel: String,
    pub chart_version: String,
    pub base_domain: String,
    pub exposure: ExposureExport,
    pub datasets: Vec<String>,
    pub privileged: bool,
    pub env: Vec<EnvExport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<BTreeMap<String, serde_json::Value>>,
    pub redacted_keys: Vec<String>,
    pub values_omitted: bool,
}

/// The app exposure settings. A dedicated allow-list struct keeps the DM-9
/// shape stable even though the stored `Exposure` skips empty fields.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExposureExport {
    pub subdomain: String,
    pub tls: bool,
    pub auth: bool,
    pub local_only: bool,
    pub protected_paths: Vec<String>,
    pub internet: bool,
    pub service: String,
    pub port: i64,
    pub scheme: String,
}

impl ExposureExport {
    fn project(exposure: &Exposure) -> Self {
        Self {
            subdomain: exposure.subdomain.clone(),
            tls: exposure.tls,
            auth: exposure.auth,
            local_only: exposure.local_only,
            protected_paths: exposure.protected_paths.clone(),
            internet: exposure.internet,
            service: exposure.service.clone(),
            port: exposure.port,
            scheme: exposure.scheme.clone(),
        }
    }
}

/// A custom environment variable's metadata only: the value lives in a Secret.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvExport {
    pub name: String,
    pub secret: bool,
}

impl EnvExport {
    fn project(meta: &EnvMeta) -> Self {
        Self {
            name: meta.name.clone(),
            secret: meta.secret,
        }
    }
}

impl AppExport {
    fn project(record: &Record, app: Option<&App>) -> Self {
        let (values, redacted_keys) = match app {
            Some(app) => {
                // The current catalog schema is authoritative for today's
                // secret-marked properties, but a value that was secret when the
                // app was installed must stay redacted even if the catalog has
                // drifted (R-020). Union both sources of truth.
                let mut secret_keys = crate::apps::secret_value_keys(&app.schema);
                secret_keys.extend(
                    record
                        .redacted_keys
                        .iter()
                        .filter_map(|pointer| pointer_key_name(pointer)),
                );
                let (values, keys) = redact_values(&record.values, &secret_keys);
                (Some(values), keys)
            }
            None => (None, Vec::new()),
        };
        Self {
            name: record.name.clone(),
            source: record.source.clone(),
            channel: record.channel.clone(),
            chart_version: record.chart_version.clone(),
            base_domain: record.base_domain.clone(),
            exposure: ExposureExport::project(&record.exposure),
            datasets: record.datasets.clone(),
            privileged: record.privileged,
            env: record.env.iter().map(EnvExport::project).collect(),
            values,
            redacted_keys,
            // A missing catalog means secret-marked keys cannot be identified,
            // so the whole value map is dropped instead of guessed at. An empty
            // map still exports as `{}` because there is nothing to hide.
            values_omitted: app.is_none() && !record.values.is_empty(),
        }
    }
}

/// The property name at the end of a persisted JSON-pointer (`/auth/password`
/// → `password`, `/licenseData` → `licenseData`). The config export redacts by
/// name at any depth, so a nested pointer's leaf is the safe key to remove.
fn pointer_key_name(pointer: &str) -> Option<String> {
    let key = pointer
        .rsplit('/')
        .next()?
        .replace("~1", "/")
        .replace("~0", "~");
    if key.is_empty() {
        None
    } else {
        Some(key)
    }
}

/// A registered app dataset without its server-owned attachment list.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatasetExport {
    pub name: String,
    pub host_path: String,
    pub shared: bool,
    pub warning_acknowledged: bool,
}

impl DatasetExport {
    fn project(dataset: &crate::datasets::ManagedDataset) -> Self {
        Self {
            name: dataset.name.clone(),
            host_path: dataset.host_path.clone(),
            shared: dataset.shared,
            warning_acknowledged: dataset.warning_acknowledged,
        }
    }
}

/// `GET /api/system/config/export` (FR-CFG-01). Admin-only through the owner
/// router; returns a downloadable JSON document. Serialization cannot include
/// a Secret because the projection never reads one.
pub async fn export(State(state): State<Arc<AppState>>) -> Response {
    let export = build(&state, Utc::now());
    let mut body = match serde_json::to_vec_pretty(&export) {
        Ok(body) => body,
        Err(e) => {
            return crate::json(
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({ "error": format!("encoding config export: {e}") }),
            );
        }
    };
    body.push(b'\n');

    let filename = format!(
        "naslos-config-export-{}.json",
        export.generated_at.format("%Y%m%dT%H%M%SZ")
    );
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename=\"{filename}\"")) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    (StatusCode::OK, headers, body).into_response()
}

/// Build the export from the live stores. This never contacts the Kubernetes
/// API and never reads a Secret.
pub fn build(state: &AppState, now: DateTime<Utc>) -> Export {
    let shares = state.shares.lock().unwrap().list();
    let catalog = state.catalog.load();

    Export {
        kind: KIND,
        schema_version: SCHEMA_VERSION,
        generated_at: now,
        naslos_version: state.system.release.naslos_version.clone(),
        base_domain: state.base_domain.clone(),
        shares: shares.iter().map(ShareExport::project).collect(),
        storage_schedules: state
            .storage_schedules
            .list()
            .iter()
            .map(StorageScheduleExport::project)
            .collect(),
        buddy_schedules: state
            .buddy_schedules
            .list()
            .iter()
            .map(BuddyScheduleExport::project)
            .collect(),
        cloud_destinations: state
            .cloud
            .as_ref()
            .map(|cloud| {
                cloud
                    .destinations
                    .list()
                    .iter()
                    .map(CloudDestinationExport::project)
                    .collect()
            })
            .unwrap_or_default(),
        cloud_schedules: state
            .cloud
            .as_ref()
            .map(|cloud| {
                cloud
                    .schedules
                    .list()
                    .iter()
                    .map(CloudScheduleExport::project)
                    .collect()
            })
            .unwrap_or_default(),
        domains: state
            .domains
            .list()
            .iter()
            .map(|domain| DomainExport::project(domain, state.providers.get(&domain.dns_provider)))
            .collect(),
        ddns_entries: state
            .ddns
            .as_ref()
            .map(|manager| {
                manager
                    .store()
                    .list()
                    .iter()
                    .map(|entry| {
                        DdnsEntryExport::project(entry, state.providers.get(&entry.provider))
                    })
                    .collect()
            })
            .unwrap_or_default(),
        apps: state
            .app_manager
            .as_ref()
            .map(|manager| {
                manager
                    .records()
                    .iter()
                    .map(|record| {
                        let app = catalog
                            .as_ref()
                            .and_then(|catalog| catalog.get(&record.name).ok());
                        AppExport::project(record, app.as_ref())
                    })
                    .collect()
            })
            .unwrap_or_default(),
        dataset_registry: state
            .datasets_registry
            .list()
            .iter()
            .map(DatasetExport::project)
            .collect(),
    }
}

/// Project a provider config to the provider schema's non-secret fields.
///
/// A key-name filter alone is not enough: provider configs can carry values
/// under free-form keys (the generic DDNS `updateUrl`/`body`) and older records
/// may still hold fields that later became secret (the OVH `applicationKey`
/// fallback). Only keys the provider declares non-secret are exported, and
/// free-form values that can embed a credential are dropped.
fn safe_provider_config(
    input: &std::collections::HashMap<String, String>,
    provider: Option<&Provider>,
    scope: &str,
) -> BTreeMap<String, String> {
    let Some(provider) = provider else {
        // An unknown/removed provider definition cannot tell us which keys are
        // safe, so fail closed rather than fall back to a key-name heuristic.
        return BTreeMap::new();
    };
    let allowed: HashSet<String> = provider
        .fields_for(scope)
        .into_iter()
        .filter(|field| !field.secret)
        .map(|field| field.key)
        .collect();
    input
        .iter()
        .filter(|(key, value)| {
            allowed.contains(*key)
                && !is_sensitive_key(key)
                && !is_free_form_value_key(key)
                && !value_looks_sensitive(value)
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// Free-form provider fields whose value can legitimately contain a credential
/// (the generic DDNS driver substitutes tokens into these templates). They have
/// no safe projection, so they are left out and must be re-entered.
pub(crate) fn is_free_form_value_key(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    matches!(normalized.as_str(), "updateurl" | "body")
}

/// Whether a provider-config value itself contains something credential-like.
/// This catches a token pasted into a URL or body even when the key looks safe.
pub(crate) fn value_looks_sensitive(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    if let Some((_, rest)) = lower.split_once("://") {
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        if authority.contains('@') {
            // `scheme://user:password@host` carries userinfo.
            return true;
        }
    }
    [
        "password",
        "passwd",
        "passphrase",
        "secret",
        "token",
        "credential",
        "apikey",
        "api_key",
        "access_key",
        "consumer_key",
        "private_key",
    ]
    .iter()
    .any(|marker| lower.contains(&format!("{marker}=")) || lower.contains(&format!("{marker}:")))
}

/// Redact an app value map recursively. The catalog's secret-marked property
/// names are removed at any depth, and any key that looks like a credential is
/// removed even when the catalog forgot to mark it.
fn redact_values(
    values: &serde_json::Map<String, serde_json::Value>,
    secret_keys: &HashSet<String>,
) -> (BTreeMap<String, serde_json::Value>, Vec<String>) {
    let mut redacted = BTreeSet::new();
    let mut out = BTreeMap::new();
    for (key, value) in values {
        if secret_keys.contains(key) || is_sensitive_key(key) {
            if !value.is_null() {
                redacted.insert(key.clone());
            }
            continue;
        }
        out.insert(key.clone(), redact_value(value, secret_keys, &mut redacted));
    }
    (out, redacted.into_iter().collect())
}

fn redact_value(
    value: &serde_json::Value,
    secret_keys: &HashSet<String>,
    redacted: &mut BTreeSet<String>,
) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, value) in map {
                if secret_keys.contains(key) || is_sensitive_key(key) {
                    if !value.is_null() {
                        redacted.insert(key.clone());
                    }
                    continue;
                }
                out.insert(key.clone(), redact_value(value, secret_keys, redacted));
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .iter()
                .map(|item| redact_value(item, secret_keys, redacted))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Whether a free-form key is credential-like. The export applies this to
/// app values and provider configs so a field the catalog does not mark still
/// cannot carry a password, token, Secret reference or key material.
pub fn is_sensitive_key(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    [
        "password",
        "passwd",
        "passphrase",
        "secret",
        "token",
        "credential",
        "privatekey",
        "tlskey",
        "apikey",
        "accesskey",
        "consumerkey",
        "applicationkey",
        "secretkey",
        "bearer",
        "kek",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::App;
    use crate::certs::Domain;
    use crate::datasets::ManagedDataset;
    use crate::ddns::Entry;
    use crate::server::buddy_schedules::ScheduleEntry;
    use crate::server::storage_schedules::{ScheduleKind, StorageSchedule};
    use crate::shares::{CreateShareRequest, Protocol};
    use serde_json::{json, Value};
    use std::collections::HashMap;

    const CANARY: &str = "CANARY-SECRET-MUST-NOT-EXPORT";

    const ENVELOPE_KEYS: &[&str] = &[
        "kind",
        "schemaVersion",
        "generatedAt",
        "naslosVersion",
        "baseDomain",
        "shares",
        "storageSchedules",
        "buddySchedules",
        "cloudDestinations",
        "cloudSchedules",
        "domains",
        "ddnsEntries",
        "apps",
        "datasetRegistry",
    ];
    const SHARE_KEYS: &[&str] = &[
        "name",
        "path",
        "protocol",
        "description",
        "readOnly",
        "browseable",
        "allowedHosts",
        "validUsers",
        "validGroups",
        "timeMachine",
        "noRootSquash",
        "shadowCopies",
        "audit",
        "enabled",
    ];
    const STORAGE_SCHEDULE_KEYS: &[&str] = &[
        "id",
        "kind",
        "target",
        "cadence",
        "runAt",
        "keep",
        "recursive",
        "enabled",
    ];
    const BUDDY_SCHEDULE_KEYS: &[&str] = &[
        "id",
        "dataset",
        "source",
        "receivers",
        "cadence",
        "runAt",
        "pruneKeep",
        "enabled",
    ];
    const CLOUD_DESTINATION_KEYS: &[&str] = &[
        "id",
        "name",
        "endpoint",
        "region",
        "bucket",
        "prefix",
        "pathStyle",
        "allowInsecureHttp",
        "enabled",
    ];
    const CLOUD_SCHEDULE_KEYS: &[&str] = &[
        "id",
        "dataset",
        "destinations",
        "cadence",
        "runAt",
        "keepLineages",
        "enabled",
    ];
    const DOMAIN_KEYS: &[&str] = &[
        "baseDomain",
        "dnsProvider",
        "providerConfig",
        "acmeEmail",
        "environment",
        "primary",
        "sso",
        "appRecordsDdnsEntry",
    ];
    const DDNS_KEYS: &[&str] = &[
        "id",
        "provider",
        "zone",
        "record",
        "recordType",
        "ttl",
        "enabled",
        "providerConfig",
    ];
    const APP_KEYS: &[&str] = &[
        "name",
        "source",
        "channel",
        "chartVersion",
        "baseDomain",
        "exposure",
        "datasets",
        "privileged",
        "env",
        "values",
        "redactedKeys",
        "valuesOmitted",
    ];
    const DATASET_KEYS: &[&str] = &["name", "hostPath", "shared", "warningAcknowledged"];

    fn keys(value: &Value) -> BTreeSet<&str> {
        value
            .as_object()
            .map(|map| map.keys().map(String::as_str).collect())
            .unwrap_or_default()
    }

    fn assert_allowed(section: &str, record: &Value, allowed: &[&str]) {
        let actual = keys(record);
        let allowed: BTreeSet<&str> = allowed.iter().copied().collect();
        assert_eq!(
            actual, allowed,
            "{section} fields must match the export allow-list"
        );
    }

    fn walk(value: &Value, visit: &mut impl FnMut(&str, &Value)) {
        match value {
            Value::Object(map) => {
                for (key, value) in map {
                    visit(key, value);
                    walk(value, visit);
                }
            }
            Value::Array(items) => {
                for item in items {
                    walk(item, visit);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn export_shape_uses_an_allow_list_and_deterministic_arrays() {
        let state = AppState::for_test("test-secret");
        state
            .shares
            .lock()
            .unwrap()
            .create(&CreateShareRequest {
                name: "media".into(),
                path: "/var/mnt/tank/media".into(),
                protocol: Protocol::Smb,
                description: "Media".into(),
                browseable: Some(true),
                shadow_copies: true,
                audit: true,
                enabled: Some(true),
                ..Default::default()
            })
            .unwrap();
        state
            .storage_schedules
            .put(StorageSchedule {
                id: "snap-1".into(),
                kind: ScheduleKind::Snapshot,
                target: "tank/media".into(),
                cadence: crate::schedule::Cadence::Daily,
                run_at: "02:30".into(),
                keep: 3,
                recursive: true,
                enabled: true,
                ..Default::default()
            })
            .unwrap();
        state
            .buddy_schedules
            .put(ScheduleEntry {
                id: "buddy-1".into(),
                dataset: "tank/media".into(),
                source: "tank/media".into(),
                receiver: "peer-a".into(),
                cadence: crate::schedule::Cadence::Weekly,
                run_at: "Sun 03:00".into(),
                prune_keep: 5,
                enabled: true,
                ..Default::default()
            })
            .unwrap();
        state
            .domains
            .upsert(Domain {
                base_domain: "example.test".into(),
                dns_provider: "cloudflare".into(),
                provider_config: HashMap::from([("zone".into(), "example.test".into())]),
                acme_email: "ops@example.test".into(),
                app_records_ddns_entry: Some("ddns-1".into()),
                ..Default::default()
            })
            .unwrap();
        state
            .datasets_registry
            .add(ManagedDataset {
                name: "media".into(),
                host_path: "/var/mnt/tank/media".into(),
                shared: false,
                warning_acknowledged: false,
                created_at: None,
                attached_to: Vec::new(),
            })
            .unwrap();
        let cloud = state.cloud.as_ref().unwrap();
        cloud
            .destinations
            .put(crate::cloud::Destination {
                id: "dest-1".into(),
                name: "offsite".into(),
                endpoint: "https://s3.us-west-004.backblazeb2.com".into(),
                region: "us-west-004".into(),
                bucket: "naslos-backups".into(),
                prefix: "host1".into(),
                enabled: true,
                credentials_managed: true,
                credentials_secret: "naslos-cloud-cred-dest-1".into(),
                has_access_key: true,
                has_secret_key: true,
                ..Default::default()
            })
            .unwrap();
        cloud
            .schedules
            .put(crate::cloud::schedules::ScheduleEntry {
                id: "cs-1".into(),
                dataset: "tank/media".into(),
                destinations: vec!["dest-1".into()],
                cadence: crate::schedule::Cadence::Daily,
                run_at: "02:30".into(),
                keep_lineages: 2,
                enabled: true,
                ..Default::default()
            })
            .unwrap();

        let now = Utc::now();
        let export = build(&state, now);
        let value = serde_json::to_value(&export).unwrap();

        assert_eq!(keys(&value), ENVELOPE_KEYS.iter().copied().collect());
        assert_eq!(value["kind"], KIND);
        assert_eq!(value["schemaVersion"], SCHEMA_VERSION);
        assert_eq!(value["generatedAt"], json!(now));
        assert_eq!(value["baseDomain"], "");

        assert_allowed("share", &value["shares"][0], SHARE_KEYS);
        assert_allowed(
            "storage schedule",
            &value["storageSchedules"][0],
            STORAGE_SCHEDULE_KEYS,
        );
        assert_allowed(
            "buddy schedule",
            &value["buddySchedules"][0],
            BUDDY_SCHEDULE_KEYS,
        );
        assert_allowed(
            "cloud destination",
            &value["cloudDestinations"][0],
            CLOUD_DESTINATION_KEYS,
        );
        assert_allowed(
            "cloud schedule",
            &value["cloudSchedules"][0],
            CLOUD_SCHEDULE_KEYS,
        );
        assert_allowed("domain", &value["domains"][0], DOMAIN_KEYS);
        assert_allowed(
            "dataset registry",
            &value["datasetRegistry"][0],
            DATASET_KEYS,
        );

        assert_eq!(value["shares"][0]["name"], "media");
        assert_eq!(value["shares"][0]["protocol"], "smb");
        assert_eq!(value["shares"][0]["shadowCopies"], true);
        assert_eq!(value["shares"][0]["audit"], true);
        assert_eq!(value["storageSchedules"][0]["kind"], "snapshot");
        assert_eq!(value["storageSchedules"][0]["cadence"], "daily");
        assert_eq!(value["buddySchedules"][0]["receivers"], json!(["peer-a"]));
        assert_eq!(value["cloudDestinations"][0]["name"], "offsite");
        assert_eq!(value["cloudDestinations"][0]["allowInsecureHttp"], false);
        assert_eq!(
            value["cloudSchedules"][0]["destinations"],
            json!(["dest-1"])
        );
        assert_eq!(value["ddnsEntries"], json!([]));
        assert_eq!(value["apps"], json!([]));
        assert_eq!(value["datasetRegistry"][0]["name"], "media");
    }

    #[test]
    fn secret_material_is_never_exported() {
        let providers = crate::providers::Registry::load("");
        let domain = Domain {
            base_domain: "example.test".into(),
            dns_provider: "cloudflare".into(),
            credentials_secret: format!("domain-{CANARY}"),
            provider_config: HashMap::from([
                // Declared non-secret by the provider schema.
                ("proxied".into(), "false".into()),
                // Client-supplied/legacy keys not in the schema must not pass.
                ("zone".into(), "example.test".into()),
                ("apiKey".into(), CANARY.into()),
            ]),
            solver: Some(json!({"password": CANARY})),
            acme_email: "ops@example.test".into(),
            last_error: CANARY.into(),
            ..Default::default()
        };
        let ddns = Entry {
            id: "ddns-1".into(),
            provider: "ovh".into(),
            zone: "example.test".into(),
            record: "home".into(),
            record_type: "A".into(),
            credentials_secret: format!("ddns-{CANARY}"),
            credential_fields: vec![CANARY.into()],
            provider_config: HashMap::from([
                ("endpoint".into(), "ovh-eu".into()),
                ("mode".into(), "api".into()),
                // Legacy OVH fallback: a field that is secret in the schema.
                ("applicationKey".into(), CANARY.into()),
                ("consumerKey".into(), CANARY.into()),
            ]),
            last_ip: CANARY.into(),
            last_status: CANARY.into(),
            last_error: CANARY.into(),
            ..Default::default()
        };
        let buddy = ScheduleEntry {
            id: "buddy-1".into(),
            dataset: "tank/media".into(),
            source: "tank/media".into(),
            receiver: "peer-a".into(),
            receiver_results: HashMap::from([("peer-a".into(), CANARY.into())]),
            last_result: CANARY.into(),
            last_error: CANARY.into(),
            ..Default::default()
        };
        let storage = StorageSchedule {
            id: "snap-1".into(),
            kind: ScheduleKind::Snapshot,
            target: "tank/media".into(),
            last_result: CANARY.into(),
            last_error: CANARY.into(),
            ..Default::default()
        };

        let app = App {
            name: "demo".into(),
            schema: json!({
                "type": "object",
                "properties": {
                    "adminPassword": {"type": "string", "format": "password"},
                    "nested": {
                        "type": "object",
                        "properties": {
                            "apiToken": {"type": "string", "writeOnly": true},
                            "level": {"type": "string"}
                        }
                    }
                }
            }),
            ..Default::default()
        };
        let mut record = Record {
            name: "demo".into(),
            ..Default::default()
        };
        record.values = json!({
            "adminPassword": CANARY,
            "replicas": 2,
            "nested": {"apiToken": CANARY, "level": "debug"},
            "plainToken": CANARY,
            "existingSecret": CANARY
        })
        .as_object()
        .unwrap()
        .clone();
        record.env = vec![EnvMeta {
            name: "API_TOKEN".into(),
            secret: true,
        }];
        let app_export = AppExport::project(&record, Some(&app));

        let value = json!({
            "domain": DomainExport::project(&domain, providers.get("cloudflare")),
            "ddns": DdnsEntryExport::project(&ddns, providers.get("ovh")),
            "buddy": BuddyScheduleExport::project(&buddy),
            "storage": StorageScheduleExport::project(&storage),
            "app": app_export,
        });
        assert_allowed("ddns entry", &value["ddns"], DDNS_KEYS);
        assert_allowed("app", &value["app"], APP_KEYS);

        let text = serde_json::to_string(&value).unwrap();
        assert!(!text.contains(CANARY), "secret canary leaked: {text}");

        assert_eq!(
            value["domain"]["providerConfig"],
            json!({"proxied": "false"})
        );
        assert!(value["domain"].get("credentialsSecret").is_none());
        assert!(value["domain"].get("solver").is_none());
        assert!(value["domain"].get("lastError").is_none());

        assert_eq!(
            value["ddns"]["providerConfig"],
            json!({"endpoint": "ovh-eu", "mode": "api"})
        );
        for key in [
            "credentialsSecret",
            "credentialFields",
            "lastIP",
            "lastStatus",
            "lastError",
        ] {
            assert!(value["ddns"].get(key).is_none(), "ddns leaked {key}");
        }

        assert!(value["buddy"].get("receiverResults").is_none());
        assert!(value["buddy"].get("lastError").is_none());
        assert!(value["storage"].get("lastError").is_none());

        assert_eq!(value["app"]["values"]["replicas"], 2);
        assert_eq!(
            value["app"]["values"]["nested"]["level"],
            Value::String("debug".into())
        );
        for key in ["adminPassword", "plainToken", "existingSecret"] {
            assert!(
                value["app"]["values"].get(key).is_none(),
                "app leaked {key}"
            );
        }
        assert!(value["app"]["values"]["nested"].get("apiToken").is_none());
        assert_eq!(
            value["app"]["redactedKeys"],
            json!(["adminPassword", "apiToken", "existingSecret", "plainToken"])
        );
        assert_eq!(
            value["app"]["env"],
            json!([{"name": "API_TOKEN", "secret": true}])
        );
    }

    #[test]
    fn app_values_are_omitted_without_a_catalog() {
        let record = Record {
            name: "demo".into(),
            values: json!({"replicas": 2}).as_object().unwrap().clone(),
            ..Default::default()
        };
        let export = AppExport::project(&record, None);
        assert!(export.values.is_none());
        assert!(export.values_omitted);
        let value = serde_json::to_value(export).unwrap();
        assert_eq!(value["valuesOmitted"], true);
        assert!(value.get("values").is_none(), "{value}");
    }

    #[test]
    fn persisted_redacted_keys_are_unioned_with_the_catalog_schema() {
        // The current schema does not mark licenseData at all; only the record
        // remembers it was secret when the app was installed (R-020 / I-9).
        let app = App {
            name: "demo".into(),
            schema: json!({
                "type": "object",
                "properties": { "replicas": {"type": "integer"} }
            }),
            ..Default::default()
        };
        let mut record = Record {
            name: "demo".into(),
            redacted_keys: vec!["/licenseData".into()],
            ..Default::default()
        };
        record.values = json!({
            "replicas": 2,
            "licenseData": CANARY,
            "nested": {"licenseData": CANARY}
        })
        .as_object()
        .unwrap()
        .clone();

        let value = serde_json::to_value(AppExport::project(&record, Some(&app))).unwrap();
        assert!(value["values"].get("licenseData").is_none(), "{value}");
        assert!(
            value["values"]["nested"].get("licenseData").is_none(),
            "{value}"
        );
        assert_eq!(value["values"]["replicas"], 2);
        assert_eq!(value["redactedKeys"], json!(["licenseData"]));
        assert!(!value.to_string().contains(CANARY), "{value}");
    }

    #[test]
    fn exposure_projection_always_has_every_dm9_field() {
        // The stored Exposure skips empty fields, so exporting it directly
        // omitted protectedPaths/service/port/scheme.
        let record = Record {
            name: "demo".into(),
            exposure: Exposure::default(),
            ..Default::default()
        };
        let value = serde_json::to_value(AppExport::project(&record, None)).unwrap();
        let exposure = keys(&value["exposure"]);
        for field in [
            "subdomain",
            "tls",
            "auth",
            "localOnly",
            "protectedPaths",
            "internet",
            "service",
            "port",
            "scheme",
        ] {
            assert!(
                exposure.contains(field),
                "exposure missing {field}: {value}"
            );
        }
    }

    #[test]
    fn free_form_provider_values_are_not_exported() {
        let providers = crate::providers::Registry::load("");
        let entry = Entry {
            id: "ddns-1".into(),
            provider: "generic".into(),
            provider_config: HashMap::from([
                (
                    "updateUrl".into(),
                    format!("https://example.test/update?token={CANARY}"),
                ),
                ("body".into(), format!("{{\"token\":\"{CANARY}\"}}")),
                ("method".into(), "GET".into()),
            ]),
            ..Default::default()
        };
        let export = DdnsEntryExport::project(&entry, providers.get("generic"));
        assert_eq!(
            export.provider_config,
            BTreeMap::from([("method".to_string(), "GET".to_string())])
        );
        // An unknown provider definition fails closed.
        let unknown = DdnsEntryExport::project(&entry, None);
        assert!(unknown.provider_config.is_empty());
    }

    #[test]
    fn absent_subsystems_export_empty_arrays_not_null() {
        let state = AppState::for_test("test-secret");
        let value = serde_json::to_value(build(&state, Utc::now())).unwrap();
        for section in [
            "shares",
            "storageSchedules",
            "buddySchedules",
            "domains",
            "ddnsEntries",
            "apps",
            "datasetRegistry",
        ] {
            assert_eq!(
                value[section],
                json!([]),
                "{section} must be an empty array"
            );
        }
    }

    #[test]
    fn every_free_form_key_in_the_payload_is_allow_listed() {
        let providers = crate::providers::Registry::load("");
        // A catalog app with an empty schema: no secret-marked properties, so
        // only the credential-key heuristic stands between values and export.
        let app = App {
            name: "demo".into(),
            schema: json!({"type": "object", "properties": {}}),
            ..Default::default()
        };
        let mut record = Record {
            name: "demo".into(),
            ..Default::default()
        };
        record.values = json!({
            "passphrase": CANARY,
            "accessKey": CANARY,
            "tls.key": CANARY,
            "replicas": 2
        })
        .as_object()
        .unwrap()
        .clone();
        let domain = Domain {
            dns_provider: "ovh".into(),
            provider_config: HashMap::from([
                ("endpoint".into(), "ovh-eu".into()),
                ("applicationKey".into(), CANARY.into()),
            ]),
            ..Default::default()
        };

        let value = json!({
            "domain": DomainExport::project(&domain, providers.get("ovh")),
            "app": AppExport::project(&record, Some(&app)),
        });
        let mut visited = 0usize;
        let mut forbidden = Vec::new();
        walk(&value, &mut |key, _| {
            visited += 1;
            if is_sensitive_key(key) {
                forbidden.push(key.to_string());
            }
        });
        assert!(visited > 0, "the assertion must not be vacuous: {value}");
        assert!(
            forbidden.is_empty(),
            "credential-looking key in export: {forbidden:?}"
        );
        assert_eq!(value["app"]["values"]["replicas"], 2);
    }
}
