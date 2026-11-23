//! System configuration import (FR-CFG-04).
//!
//! The import consumes the versioned, secret-free export envelope and only
//! writes declarative records through the existing stores. Every payload is
//! checked against an explicit allow-list; exported ids are never trusted, no
//! credential or Secret is written, and the default mode is a dry-run plan.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::apps;
use crate::certs::{self, Domain};
use crate::datasets::{self, ManagedDataset};
use crate::ddns;
use crate::providers::Provider;
use crate::schedule::{self, Cadence};
use crate::server::buddy_schedules::ScheduleEntry;
use crate::server::config_export;
use crate::server::storage_schedules::{self, ScheduleKind, StorageSchedule};
use crate::shares::{self, CreateShareRequest, Protocol, UpdateShareRequest};
use crate::state::AppState;

/// The import response discriminator.
pub const KIND: &str = "naslos-config-import";

const APPLY_CONFIRM: &str = "apply";
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportDocument {
    pub kind: String,
    pub schema_version: u32,
    pub generated_at: DateTime<Utc>,
    pub naslos_version: String,
    pub base_domain: String,
    pub shares: Vec<ShareRecord>,
    pub storage_schedules: Vec<StorageScheduleRecord>,
    pub buddy_schedules: Vec<BuddyScheduleRecord>,
    pub domains: Vec<DomainRecord>,
    pub ddns_entries: Vec<DdnsRecord>,
    pub apps: Vec<AppRecord>,
    pub dataset_registry: Vec<DatasetRecord>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShareRecord {
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
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StorageScheduleRecord {
    pub id: String,
    pub kind: ScheduleKind,
    pub target: String,
    pub cadence: Cadence,
    pub run_at: String,
    pub keep: usize,
    pub recursive: bool,
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuddyScheduleRecord {
    pub id: String,
    pub dataset: String,
    pub source: String,
    pub receivers: Vec<String>,
    pub cadence: Cadence,
    pub run_at: String,
    pub prune_keep: usize,
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DomainRecord {
    pub base_domain: String,
    pub dns_provider: String,
    pub provider_config: BTreeMap<String, String>,
    pub acme_email: String,
    pub environment: String,
    pub primary: bool,
    pub sso: bool,
    pub app_records_ddns_entry: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DdnsRecord {
    pub id: String,
    pub provider: String,
    pub zone: String,
    pub record: String,
    pub record_type: String,
    pub ttl: i64,
    pub enabled: bool,
    pub provider_config: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppRecord {
    pub name: String,
    pub source: String,
    pub channel: String,
    pub chart_version: String,
    pub base_domain: String,
    pub exposure: ExposureRecord,
    pub datasets: Vec<String>,
    pub privileged: bool,
    pub env: Vec<EnvRecord>,
    #[serde(default)]
    pub values: Option<BTreeMap<String, serde_json::Value>>,
    pub redacted_keys: Vec<String>,
    pub values_omitted: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExposureRecord {
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvRecord {
    pub name: String,
    pub secret: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DatasetRecord {
    pub name: String,
    pub host_path: String,
    pub shared: bool,
    pub warning_acknowledged: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportQuery {
    #[serde(default)]
    apply: Option<String>,
    #[serde(default)]
    confirm: String,
}

#[derive(Debug, Clone)]
enum Outcome {
    Created(Option<String>),
    Updated(Option<String>, Vec<String>),
    Unchanged,
    Skipped(String),
    Failed(String),
}

#[derive(Debug, Clone)]
enum DdnsRef {
    Existing(String),
    Imported {
        original: String,
        existing: Option<String>,
    },
}

#[derive(Debug)]
struct SharePlan {
    entry: String,
    create: CreateShareRequest,
    existing: Option<shares::Share>,
    outcome: Outcome,
}

#[derive(Debug)]
struct StoragePlan {
    entry: String,
    value: StorageSchedule,
    existing: Option<StorageSchedule>,
    changes: Vec<String>,
    outcome: Outcome,
}

#[derive(Debug)]
struct BuddyPlan {
    entry: String,
    value: ScheduleEntry,
    existing: Option<ScheduleEntry>,
    changes: Vec<String>,
    outcome: Outcome,
}

#[derive(Debug)]
struct DdnsPlan {
    entry: String,
    original_id: String,
    value: ddns::Entry,
    existing: Option<ddns::Entry>,
    changes: Vec<String>,
    requires_credentials: bool,
    needs_config: bool,
    outcome: Outcome,
}

#[derive(Debug)]
struct DomainPlan {
    entry: String,
    value: Domain,
    existing: Option<Domain>,
    ddns_ref: Option<DdnsRef>,
    changes: Vec<String>,
    requires_credentials: bool,
    outcome: Outcome,
}

#[derive(Debug)]
struct DatasetPlan {
    entry: String,
    value: ManagedDataset,
    existing: Option<ManagedDataset>,
    changes: Vec<String>,
    outcome: Outcome,
}

#[derive(Debug, Default)]
struct Plan {
    shares: Vec<SharePlan>,
    storage: Vec<StoragePlan>,
    buddy: Vec<BuddyPlan>,
    ddns: Vec<DdnsPlan>,
    domains: Vec<DomainPlan>,
    datasets: Vec<DatasetPlan>,
    apps: Vec<SkipReport>,
    warnings: Vec<String>,
    structural_only: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SkipReport {
    entry: String,
    reason: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreatedReport {
    entry: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdatedReport {
    entry: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    changed: Vec<String>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct SectionReport {
    created: Vec<CreatedReport>,
    updated: Vec<UpdatedReport>,
    unchanged: Vec<String>,
    skipped: Vec<SkipReport>,
    failed: Vec<SkipReport>,
    notes: Vec<String>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct Totals {
    created: usize,
    updated: usize,
    unchanged: usize,
    skipped: usize,
    failed: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportReport {
    kind: &'static str,
    schema_version: u32,
    dry_run: bool,
    applied: bool,
    partial: bool,
    structural_only: bool,
    sections: BTreeMap<String, SectionReport>,
    totals: Totals,
    warnings: Vec<String>,
}

/// `POST /api/system/config/import` (FR-CFG-04). Admin-only through the owner
/// router. The default is a dry-run plan; `?apply=true&confirm=apply` writes
/// the validated plan.
pub async fn import_config(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ImportQuery>,
    body: Body,
) -> Response {
    let apply = match parse_apply(query.apply.as_deref()) {
        Ok(apply) => apply,
        Err(e) => return bad_request(&e),
    };
    if apply && query.confirm.trim() != APPLY_CONFIRM {
        return bad_request("apply requires the explicit confirmation confirm=apply");
    }

    let bytes = match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(e) => {
            // axum reports the body cap as a length-limit error; an oversized
            // restore must be 413, not a generic 400.
            return json(
                StatusCode::PAYLOAD_TOO_LARGE,
                serde_json::json!({
                    "error": format!("the config import body exceeds the {} byte limit: {e}", MAX_BODY_BYTES),
                }),
            );
        }
    };
    let document: ImportDocument = match serde_json::from_slice(&bytes) {
        Ok(document) => document,
        Err(e) => {
            return bad_request(&format!("invalid config import document: {e}"));
        }
    };
    if let Err(e) = check_envelope(&document) {
        return bad_request(&e);
    }

    let mut plan = match build_plan(&state, &document).await {
        Ok(plan) => plan,
        Err(errors) => {
            return json(
                StatusCode::BAD_REQUEST,
                serde_json::json!({
                    "error": "config import rejected",
                    "details": errors,
                }),
            );
        }
    };
    if apply {
        apply_plan(&state, &mut plan).await;
        tracing::info!(
            created = plan_created(&plan),
            updated = plan_updated(&plan),
            "config import applied"
        );
    }
    let report = build_report(&plan, document.schema_version, !apply);
    json(StatusCode::OK, serde_json::to_value(report).unwrap())
}

fn parse_apply(raw: Option<&str>) -> Result<bool, String> {
    match raw {
        None => Ok(false),
        Some(value) if value.trim().is_empty() || value == "0" || value == "false" => Ok(false),
        Some(value) if value == "1" || value == "true" => Ok(true),
        Some(value) => Err(format!(
            "apply must be true or false, not {:?}",
            value.trim()
        )),
    }
}

fn check_envelope(document: &ImportDocument) -> Result<(), String> {
    if document.kind != config_export::KIND {
        return Err(format!(
            "kind must be {:?}, not {:?}",
            config_export::KIND,
            document.kind
        ));
    }
    if document.schema_version > config_export::SCHEMA_VERSION {
        return Err(format!(
            "schemaVersion {} is newer than this API supports ({})",
            document.schema_version,
            config_export::SCHEMA_VERSION
        ));
    }
    if document.schema_version != config_export::SCHEMA_VERSION {
        return Err(format!(
            "unsupported schemaVersion {} (expected {})",
            document.schema_version,
            config_export::SCHEMA_VERSION
        ));
    }
    Ok(())
}

async fn build_plan(state: &AppState, document: &ImportDocument) -> Result<Plan, Vec<String>> {
    let mut errors = Vec::new();
    let shares = plan_shares(document, state, &mut errors).await;
    let storage = plan_storage(document, state, &mut errors).await;
    let buddy = plan_buddy(document, state, &mut errors).await;
    let ddns = plan_ddns(document, state, &mut errors);
    let domains = plan_domains(document, state, &ddns, &mut errors);
    let datasets = plan_datasets(document, state, &mut errors).await;
    let apps = plan_apps(document, state, &mut errors);

    let mut warnings = Vec::new();
    if document.base_domain != state.base_domain {
        warnings.push(
            "baseDomain is informational and was not changed; the chart owns the primary domain"
                .to_string(),
        );
    }
    if document
        .domains
        .iter()
        .any(|domain| domain.primary && domain.base_domain != state.base_domain)
    {
        warnings.push("primary is server-owned and was ignored for imported domains".to_string());
    }
    let needs_live_validation = !(document.shares.is_empty()
        && document.storage_schedules.is_empty()
        && document.buddy_schedules.is_empty()
        && document.dataset_registry.is_empty());
    let mut structural_only = state.agent.is_none() && needs_live_validation;
    if let Some(agent) = &state.agent {
        if needs_live_validation && agent.list_datasets().await.is_err() {
            // A configured but unreachable agent must never degrade silently:
            // the planner could then accept a path or target that the node
            // does not have. Say so in the report and mark the plan structural.
            structural_only = true;
            warnings.push(
                "the configured agent is unreachable; share paths and dataset/pool targets were validated structurally only, not against the node"
                    .to_string(),
            );
        }
    } else if structural_only {
        warnings.push(
            "the agent is unavailable; share paths and dataset/pool targets were validated structurally only"
                .to_string(),
        );
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(Plan {
        shares,
        storage,
        buddy,
        ddns,
        domains,
        datasets,
        apps,
        warnings,
        structural_only,
    })
}

async fn plan_shares(
    document: &ImportDocument,
    state: &AppState,
    errors: &mut Vec<String>,
) -> Vec<SharePlan> {
    let (base, current) = {
        let manager = state.shares.lock().unwrap();
        (shares::clean_path(manager.zfs_base()), manager.list())
    };
    let mountpoints = live_mountpoints(state).await;
    let mut plans = Vec::new();
    let mut seen = HashSet::new();

    for (index, record) in document.shares.iter().enumerate() {
        let label = format!("shares[{index}] ({})", record.name);
        if !seen.insert(record.name.clone()) {
            plans.push(SharePlan {
                entry: record.name.clone(),
                create: CreateShareRequest {
                    name: record.name.clone(),
                    path: record.path.clone(),
                    description: record.description.clone(),
                    ..Default::default()
                },
                existing: None,
                outcome: Outcome::Skipped(
                    "another imported share already uses this name; only the first is imported"
                        .to_string(),
                ),
            });
            continue;
        }
        if let Err(e) = shares::validate_share_name(&record.name) {
            errors.push(format!("{label}: {e}"));
            continue;
        }
        let protocol = match record.protocol.as_str() {
            "smb" => Protocol::Smb,
            "nfs" => Protocol::Nfs,
            "afp" => {
                plans.push(SharePlan {
                    entry: record.name.clone(),
                    create: CreateShareRequest {
                        name: record.name.clone(),
                        path: record.path.clone(),
                        protocol: Protocol::Afp,
                        description: record.description.clone(),
                        read_only: record.read_only,
                        browseable: Some(record.browseable),
                        allowed_hosts: shares::normalize_list(&record.allowed_hosts),
                        valid_users: shares::normalize_list(&record.valid_users),
                        valid_groups: shares::normalize_list(&record.valid_groups),
                        time_machine: record.time_machine,
                        no_root_squash: record.no_root_squash,
                        shadow_copies: false,
                        audit: false,
                        enabled: Some(record.enabled),
                    },
                    existing: None,
                    outcome: Outcome::Skipped(
                        "AFP is no longer supported; recreate the share as SMB or NFS".to_string(),
                    ),
                });
                continue;
            }
            other => {
                errors.push(format!(
                    "{label}: unsupported protocol {other:?}; use \"smb\" or \"nfs\""
                ));
                continue;
            }
        };
        if record.path.trim() != record.path {
            errors.push(format!(
                "{label}: path must not start or end with whitespace"
            ));
            continue;
        }
        let path = shares::clean_path(&record.path);
        let prefix = format!("{base}/");
        if path == base || !path.starts_with(&prefix) {
            errors.push(format!("{label}: path must be a directory inside {base}"));
            continue;
        }
        if let Some(mountpoints) = &mountpoints {
            if !shares::path_on_dataset(&path, mountpoints).1 {
                errors.push(format!(
                    "{label}: path is not on a mounted ZFS dataset known to the agent"
                ));
                continue;
            }
        }
        if std::fs::metadata(&base).is_ok() {
            if let Err(e) = state.shares.lock().unwrap().validate_path(&path) {
                errors.push(format!("{label}: {e}"));
                continue;
            }
        }

        let description = record.description.clone();
        let allowed_hosts = shares::normalize_list(&record.allowed_hosts);
        let valid_users = shares::normalize_list(&record.valid_users);
        let valid_groups = shares::normalize_list(&record.valid_groups);
        let candidate = shares::Share {
            name: record.name.clone(),
            path: path.clone(),
            protocol,
            description: description.clone(),
            read_only: record.read_only,
            browseable: record.browseable,
            allowed_hosts: allowed_hosts.clone(),
            valid_users: valid_users.clone(),
            valid_groups: valid_groups.clone(),
            time_machine: record.time_machine,
            no_root_squash: record.no_root_squash,
            shadow_copies: false,
            audit: false,
            created_at: None,
            enabled: record.enabled,
        };
        if let Err(e) = shares::validate_share_fields(&candidate) {
            errors.push(format!("{label}: {e}"));
            continue;
        }

        let create = CreateShareRequest {
            name: record.name.clone(),
            path,
            protocol,
            description,
            read_only: record.read_only,
            browseable: Some(record.browseable),
            allowed_hosts,
            valid_users,
            valid_groups,
            time_machine: record.time_machine,
            no_root_squash: record.no_root_squash,
            shadow_copies: false,
            audit: false,
            enabled: Some(record.enabled),
        };
        let existing = current
            .iter()
            .find(|share| share.name == record.name)
            .cloned();
        let mut changes = Vec::new();
        let outcome = match &existing {
            None => Outcome::Created(None),
            Some(existing) => {
                if existing.protocol != protocol {
                    Outcome::Skipped(
                        "the share update endpoint cannot change a share's protocol; recreate the share manually"
                            .to_string(),
                    )
                } else {
                    if existing.path != create.path {
                        changes.push("path".to_string());
                    }
                    if existing.description != create.description {
                        changes.push("description".to_string());
                    }
                    if existing.read_only != create.read_only {
                        changes.push("readOnly".to_string());
                    }
                    if existing.browseable != record.browseable {
                        changes.push("browseable".to_string());
                    }
                    if existing.allowed_hosts != create.allowed_hosts {
                        changes.push("allowedHosts".to_string());
                    }
                    if existing.valid_users != create.valid_users {
                        changes.push("validUsers".to_string());
                    }
                    if existing.valid_groups != create.valid_groups {
                        changes.push("validGroups".to_string());
                    }
                    if existing.time_machine != create.time_machine {
                        changes.push("timeMachine".to_string());
                    }
                    if existing.no_root_squash != create.no_root_squash {
                        changes.push("noRootSquash".to_string());
                    }
                    if existing.enabled != record.enabled {
                        changes.push("enabled".to_string());
                    }
                    if changes.is_empty() {
                        Outcome::Unchanged
                    } else {
                        Outcome::Updated(None, changes.clone())
                    }
                }
            }
        };
        plans.push(SharePlan {
            entry: record.name.clone(),
            create,
            existing,
            outcome,
        });
    }
    plans
}

async fn plan_storage(
    document: &ImportDocument,
    state: &AppState,
    errors: &mut Vec<String>,
) -> Vec<StoragePlan> {
    let current = state.storage_schedules.list();
    let mut plans = Vec::new();
    let mut seen = HashSet::new();
    // Enabled snapshot schedules already accepted in this plan. The overlap
    // guard in `validate_schedule` only sees the persisted store, so two
    // imported schedules covering the same dataset would otherwise both be
    // accepted and fight over the same snapshot names.
    let mut planned_snapshots: Vec<StorageSchedule> = Vec::new();

    for (index, record) in document.storage_schedules.iter().enumerate() {
        let label = format!(
            "storageSchedules[{index}] ({} {})",
            kind_name(record.kind),
            record.target.trim()
        );
        if let Err(e) = validate_imported_id(&record.id, &label) {
            errors.push(e);
            continue;
        }
        let mut value = StorageSchedule {
            id: String::new(),
            kind: record.kind,
            target: record.target.trim().to_string(),
            cadence: record.cadence,
            run_at: record.run_at.trim().to_string(),
            keep: record.keep,
            recursive: record.recursive,
            enabled: record.enabled,
            ..Default::default()
        };
        let matches: Vec<StorageSchedule> = current
            .iter()
            .filter(|schedule| schedule.kind == value.kind && schedule.target == value.target)
            .cloned()
            .collect();
        if matches.len() > 1 {
            plans.push(StoragePlan {
                entry: label,
                value,
                existing: None,
                changes: Vec::new(),
                outcome: Outcome::Skipped(
                    "several existing schedules target this dataset or pool; remove the ambiguity first"
                        .to_string(),
                ),
            });
            continue;
        }
        let existing = matches.into_iter().next();
        if let Some(schedule) = &existing {
            // Validate an update as itself so the one-schedule-per-dataset
            // overlap guard does not treat the local schedule as a conflict.
            value.id = schedule.id.clone();
        }
        if let Err(e) = storage_schedules::validate_schedule(state, &mut value).await {
            // An imported snapshot schedule that overlaps a different existing
            // one is ambiguous, not malformed: skip it (the operator keeps the
            // local schedule) instead of aborting the whole import.
            if e.contains("already has snapshot schedule") {
                plans.push(StoragePlan {
                    entry: label,
                    value,
                    existing: None,
                    changes: Vec::new(),
                    outcome: Outcome::Skipped(e),
                });
                continue;
            }
            errors.push(format!("{label}: {e}"));
            continue;
        }
        let key = format!("{}|{}", kind_name(value.kind), value.target);
        if !seen.insert(key) {
            plans.push(StoragePlan {
                entry: label,
                value,
                existing: None,
                changes: Vec::new(),
                outcome: Outcome::Skipped(
                    "another imported schedule already uses this kind/target; only one can be restored by natural key"
                        .to_string(),
                ),
            });
            continue;
        }
        let changes = existing
            .as_ref()
            .map(|schedule| storage_changes(schedule, &value))
            .unwrap_or_default();
        let mut outcome = match &existing {
            None => Outcome::Created(None),
            Some(schedule) if changes.is_empty() => Outcome::Unchanged,
            Some(schedule) => Outcome::Updated(Some(schedule.id.clone()), changes.clone()),
        };
        if value.enabled && value.kind == ScheduleKind::Snapshot {
            if let Some(other) = planned_snapshots
                .iter()
                .find(|other| storage_schedules::snapshot_schedules_overlap(other, &value))
            {
                outcome = Outcome::Skipped(format!(
                    "another imported snapshot schedule ({}) already covers {:?}{}; only one enabled schedule per dataset is allowed",
                    kind_name(other.kind),
                    other.target,
                    if other.recursive { " recursively" } else { "" }
                ));
            } else {
                planned_snapshots.push(value.clone());
            }
        }
        plans.push(StoragePlan {
            entry: label,
            value,
            existing,
            changes,
            outcome,
        });
    }
    plans
}

async fn plan_buddy(
    document: &ImportDocument,
    state: &AppState,
    errors: &mut Vec<String>,
) -> Vec<BuddyPlan> {
    let current = state.buddy_schedules.list();
    let mut plans = Vec::new();
    let mut seen = HashSet::new();

    for (index, record) in document.buddy_schedules.iter().enumerate() {
        let label = format!(
            "buddySchedules[{index}] ({} -> {})",
            record.dataset, record.source
        );
        if let Err(e) = validate_imported_id(&record.id, &label) {
            errors.push(e);
            continue;
        }
        let mut value = ScheduleEntry {
            id: String::new(),
            dataset: record.dataset.trim().to_string(),
            source: record.source.trim().to_string(),
            receivers: record.receivers.clone(),
            cadence: record.cadence,
            run_at: record.run_at.trim().to_string(),
            prune_keep: record.prune_keep,
            enabled: record.enabled,
            ..Default::default()
        };
        if let Err(e) = crate::server::buddy_schedules::validate_schedule(state, &mut value).await {
            errors.push(format!("{label}: {e}"));
            continue;
        }
        let key = format!("{}|{}", value.dataset, value.source);
        if !seen.insert(key) {
            plans.push(BuddyPlan {
                entry: label,
                value,
                existing: None,
                changes: Vec::new(),
                outcome: Outcome::Skipped(
                    "another imported schedule already backs up this dataset/source; only one can be restored by natural key"
                        .to_string(),
                ),
            });
            continue;
        }
        let matches: Vec<ScheduleEntry> = current
            .iter()
            .filter(|entry| entry.dataset == value.dataset && entry.source == value.source)
            .cloned()
            .collect();
        if matches.len() > 1 {
            plans.push(BuddyPlan {
                entry: label,
                value,
                existing: None,
                changes: Vec::new(),
                outcome: Outcome::Skipped(
                    "several existing schedules back up this dataset/source; remove the ambiguity first"
                        .to_string(),
                ),
            });
            continue;
        }
        let existing = matches.into_iter().next();
        let changes = existing
            .as_ref()
            .map(|entry| buddy_changes(entry, &value))
            .unwrap_or_default();
        let outcome = match &existing {
            None => Outcome::Created(None),
            Some(entry) if changes.is_empty() => Outcome::Unchanged,
            Some(entry) => Outcome::Updated(Some(entry.id.clone()), changes.clone()),
        };
        plans.push(BuddyPlan {
            entry: label,
            value,
            existing,
            changes,
            outcome,
        });
    }
    plans
}

fn plan_ddns(
    document: &ImportDocument,
    state: &AppState,
    errors: &mut Vec<String>,
) -> Vec<DdnsPlan> {
    let live = state.ddns.as_ref().map(|manager| manager.store().list());
    let mut plans = Vec::new();
    let mut seen_ids = HashSet::new();
    let mut seen_keys = HashSet::new();

    for (index, record) in document.ddns_entries.iter().enumerate() {
        let label = format!(
            "ddnsEntries[{index}] ({} {}/{} {})",
            record.provider,
            record.zone.trim(),
            record.record.trim(),
            record.record_type.trim()
        );
        if let Err(e) = validate_imported_id(&record.id, &label) {
            errors.push(e);
            continue;
        }
        if !seen_ids.insert(record.id.clone()) {
            plans.push(DdnsPlan {
                entry: label,
                original_id: record.id.clone(),
                value: ddns::Entry::default(),
                existing: None,
                changes: Vec::new(),
                requires_credentials: false,
                needs_config: false,
                outcome: Outcome::Skipped(
                    "another imported DDNS entry already uses this exported id; only the first is imported"
                        .to_string(),
                ),
            });
            continue;
        }
        let Some(provider) = state.providers.get(record.provider.trim()) else {
            errors.push(format!("{label}: unknown DNS provider"));
            continue;
        };
        if !provider.has_ddns() {
            errors.push(format!("{label}: provider does not support dynamic DNS"));
            continue;
        }
        let zone = record.zone.trim().to_string();
        if !crate::server::ddns::is_dns_subdomain(&zone) {
            errors.push(format!("{label}: zone is not a valid DNS name"));
            continue;
        }
        let label_record = record.record.trim().to_string();
        if let Err(e) = crate::server::ddns::validate_record_label(&label_record) {
            errors.push(format!("{label}: {e}"));
            continue;
        }
        let record_type = record.record_type.trim().to_ascii_uppercase();
        if record_type != "A" && record_type != "AAAA" {
            errors.push(format!("{label}: recordType must be A or AAAA"));
            continue;
        }
        if record.ttl != 0 && !(60..=86400).contains(&record.ttl) {
            errors.push(format!(
                "{label}: ttl must be 0 (provider default) or between 60 and 86400"
            ));
            continue;
        }
        let provider_config =
            match provider_config(provider, "ddns", &record.provider_config, &label) {
                Ok(config) => config,
                Err(e) => {
                    errors.push(e);
                    continue;
                }
            };
        let key = format!(
            "{}|{zone}|{label_record}|{record_type}",
            record.provider.trim()
        );
        if !seen_keys.insert(key) {
            plans.push(DdnsPlan {
                entry: label,
                original_id: record.id.clone(),
                value: ddns::Entry::default(),
                existing: None,
                changes: Vec::new(),
                requires_credentials: false,
                needs_config: false,
                outcome: Outcome::Skipped(
                    "another imported DDNS entry already uses this provider/zone/record/type; only the first is imported"
                        .to_string(),
                ),
            });
            continue;
        }
        let existing_matches: Vec<ddns::Entry> = live
            .as_ref()
            .map(|entries| {
                entries
                    .iter()
                    .filter(|entry| {
                        entry.provider == record.provider.trim()
                            && entry.zone == zone
                            && entry.record == label_record
                            && entry.record_type == record_type
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        if existing_matches.len() > 1 {
            plans.push(DdnsPlan {
                entry: label,
                original_id: record.id.clone(),
                value: ddns::Entry::default(),
                existing: None,
                changes: Vec::new(),
                requires_credentials: false,
                needs_config: false,
                outcome: Outcome::Skipped(
                    "several existing entries use this provider/zone/record/type; remove the ambiguity first"
                        .to_string(),
                ),
            });
            continue;
        }
        let existing = existing_matches.into_iter().next();
        let mut value = existing.clone().unwrap_or_default();
        value.provider = record.provider.trim().to_string();
        value.zone = zone;
        value.record = label_record;
        value.record_type = record_type;
        value.ttl = record.ttl;
        value.enabled = record.enabled;
        value.provider_config = merged_config(
            existing.as_ref().map(|entry| &entry.provider_config),
            provider_config,
        );
        let needs_config = provider.fields_for("ddns").iter().any(|field| {
            field.required && !field.secret && !value.provider_config.contains_key(&field.key)
        });
        let changes = existing
            .as_ref()
            .map(|entry| ddns_changes(entry, &value))
            .unwrap_or_default();
        let outcome = if state.ddns.is_none() {
            Outcome::Skipped("dynamic DNS is disabled on this instance".to_string())
        } else {
            match &existing {
                None => Outcome::Created(None),
                Some(entry) if changes.is_empty() => Outcome::Unchanged,
                Some(entry) => Outcome::Updated(Some(entry.id.clone()), changes.clone()),
            }
        };
        plans.push(DdnsPlan {
            entry: label,
            original_id: record.id.clone(),
            value,
            existing,
            changes,
            requires_credentials: provider.secret_fields().iter().any(|field| field.required),
            needs_config,
            outcome,
        });
    }
    plans
}

fn plan_domains(
    document: &ImportDocument,
    state: &AppState,
    ddns: &[DdnsPlan],
    errors: &mut Vec<String>,
) -> Vec<DomainPlan> {
    let live_ddns = state.ddns.as_ref().map(|manager| manager.store().list());
    let imported_ddns: HashMap<&str, usize> =
        ddns.iter()
            .enumerate()
            .fold(HashMap::new(), |mut map, (index, plan)| {
                // Keep the first plan for a duplicated exported id: the second is
                // skipped, and a domain reference must resolve to the imported one.
                map.entry(plan.original_id.as_str()).or_insert(index);
                map
            });
    let mut plans = Vec::new();
    let mut seen = HashSet::new();

    for (index, record) in document.domains.iter().enumerate() {
        let base_domain = record.base_domain.trim();
        let label = format!("domains[{index}] ({base_domain})");
        if !seen.insert(base_domain.to_string()) {
            plans.push(DomainPlan {
                entry: base_domain.to_string(),
                value: Domain {
                    base_domain: base_domain.to_string(),
                    ..Default::default()
                },
                existing: None,
                ddns_ref: None,
                changes: Vec::new(),
                requires_credentials: false,
                outcome: Outcome::Skipped(
                    "another imported domain already uses this base domain; only the first is imported"
                        .to_string(),
                ),
            });
            continue;
        }
        if !certs::is_dns1123_subdomain(base_domain) {
            errors.push(format!("{label}: base domain is not a valid DNS name"));
            continue;
        }
        let Some(provider) = state.providers.get(record.dns_provider.trim()) else {
            errors.push(format!("{label}: unknown DNS provider"));
            continue;
        };
        if !provider.supports_certificates() {
            errors.push(format!("{label}: provider does not support certificates"));
            continue;
        }
        if provider.is_passthrough() {
            plans.push(DomainPlan {
                entry: base_domain.to_string(),
                value: Domain {
                    base_domain: base_domain.to_string(),
                    ..Default::default()
                },
                existing: None,
                ddns_ref: None,
                changes: Vec::new(),
                requires_credentials: false,
                outcome: Outcome::Skipped(
                    "a passthrough solver is not exported; re-enter the domain through the UI"
                        .to_string(),
                ),
            });
            continue;
        }
        let environment = if record.environment.trim().is_empty() {
            "staging".to_string()
        } else {
            record.environment.trim().to_string()
        };
        if environment != "staging" && environment != "production" {
            errors.push(format!(
                "{label}: environment must be staging or production"
            ));
            continue;
        }
        let existing = state.domains.get(base_domain).ok();
        let provider_changed = existing
            .as_ref()
            .is_some_and(|domain| domain.dns_provider != record.dns_provider.trim());
        let imported_config =
            match provider_config(provider, "cert", &record.provider_config, &label) {
                Ok(config) => config,
                Err(e) => {
                    errors.push(e);
                    continue;
                }
            };
        let provider_config = if provider_changed {
            imported_config
        } else {
            merged_config(
                existing.as_ref().map(|domain| &domain.provider_config),
                imported_config,
            )
        };

        let mut skip: Option<String> = None;
        let mut ddns_ref: Option<DdnsRef> = None;
        if let Some(reference) = record
            .app_records_ddns_entry
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
        {
            if !crate::dnsrecords::provider_supported(record.dns_provider.trim()) {
                errors.push(format!(
                    "{label}: provider cannot create app DNS records; use ovh or cloudflare"
                ));
                continue;
            }
            match imported_ddns.get(reference) {
                Some(&ddns_index) => {
                    let ddns_plan = &ddns[ddns_index];
                    if let Outcome::Skipped(reason) = &ddns_plan.outcome {
                        skip = Some(format!(
                            "the referenced DDNS entry is not imported: {reason}"
                        ));
                    } else if ddns_plan.value.record.trim().starts_with("*.") {
                        skip = Some(
                            "a wildcard DDNS entry cannot be an app-record target".to_string(),
                        );
                    } else {
                        ddns_ref = Some(DdnsRef::Imported {
                            original: reference.to_string(),
                            existing: ddns_plan.existing.as_ref().map(|entry| entry.id.clone()),
                        });
                    }
                }
                None => {
                    let live_entry = live_ddns
                        .as_ref()
                        .and_then(|entries| entries.iter().find(|entry| entry.id == reference))
                        .cloned();
                    match live_entry {
                        Some(entry) if entry.record.trim().starts_with("*.") => {
                            skip = Some(
                                "a wildcard DDNS entry cannot be an app-record target".to_string(),
                            )
                        }
                        Some(entry) => {
                            ddns_ref = Some(DdnsRef::Existing(entry.id));
                        }
                        None => {
                            skip = Some(format!(
                                "the referenced DDNS entry {reference:?} does not exist on this node"
                            ));
                        }
                    }
                }
            }
        }

        let mut value = existing.clone().unwrap_or_default();
        if provider_changed {
            value.credentials_secret.clear();
            value.solver = None;
            value.last_error.clear();
        }
        value.base_domain = base_domain.to_string();
        value.dns_provider = record.dns_provider.trim().to_string();
        value.provider_config = provider_config;
        value.acme_email = record.acme_email.trim().to_string();
        value.environment = environment;
        // The primary domain is always an SSO domain; an imported record must
        // not be able to demote it.
        value.sso = record.sso || base_domain == state.base_domain;
        value.app_records_ddns_entry = match &ddns_ref {
            None => None,
            Some(DdnsRef::Existing(id)) => Some(id.clone()),
            Some(DdnsRef::Imported { existing, .. }) => existing.clone(),
        };
        let mut changes = Vec::new();
        if let Some(existing) = &existing {
            if existing.dns_provider != value.dns_provider {
                changes.push("dnsProvider".to_string());
            }
            if sorted_config(&existing.provider_config) != sorted_config(&value.provider_config) {
                changes.push("providerConfig".to_string());
            }
            if existing.acme_email != value.acme_email {
                changes.push("acmeEmail".to_string());
            }
            if existing.environment != value.environment {
                changes.push("environment".to_string());
            }
            if existing.sso != value.sso {
                changes.push("sso".to_string());
            }
            let imported_new_ddns =
                matches!(&ddns_ref, Some(DdnsRef::Imported { existing: None, .. }));
            if existing.app_records_ddns_entry != value.app_records_ddns_entry || imported_new_ddns
            {
                changes.push("appRecordsDdnsEntry".to_string());
            }
        }
        let outcome = if let Some(reason) = skip {
            Outcome::Skipped(reason)
        } else {
            match &existing {
                None => Outcome::Created(None),
                Some(_) if changes.is_empty() => Outcome::Unchanged,
                Some(_) => Outcome::Updated(None, changes.clone()),
            }
        };
        let requires_credentials =
            provider.has_cert_manager() && (existing.is_none() || provider_changed);
        plans.push(DomainPlan {
            entry: base_domain.to_string(),
            value,
            existing,
            ddns_ref,
            changes,
            requires_credentials,
            outcome,
        });
    }
    plans
}

async fn plan_datasets(
    document: &ImportDocument,
    state: &AppState,
    errors: &mut Vec<String>,
) -> Vec<DatasetPlan> {
    let live = live_datasets(state).await;
    let current = state.datasets_registry.list();
    let mut plans = Vec::new();
    let mut seen = HashSet::new();

    for (index, record) in document.dataset_registry.iter().enumerate() {
        let label = format!("datasetRegistry[{index}] ({})", record.name);
        if !seen.insert(record.name.clone()) {
            plans.push(DatasetPlan {
                entry: record.name.clone(),
                value: ManagedDataset {
                    name: record.name.clone(),
                    host_path: record.host_path.clone(),
                    shared: false,
                    warning_acknowledged: false,
                    created_at: None,
                    attached_to: Vec::new(),
                },
                existing: None,
                changes: Vec::new(),
                outcome: Outcome::Skipped(
                    "another imported dataset already uses this name; only the first is imported"
                        .to_string(),
                ),
            });
            continue;
        }
        let candidate = ManagedDataset {
            name: record.name.clone(),
            host_path: record.host_path.clone(),
            shared: record.shared,
            warning_acknowledged: record.warning_acknowledged,
            created_at: None,
            attached_to: Vec::new(),
        };
        let value = match state.datasets_registry.check(&candidate) {
            Ok(value) => value,
            Err(e) => {
                errors.push(format!("{label}: {e}"));
                continue;
            }
        };
        let host = value.host_path.clone();
        if let Some(datasets) = &live {
            if let Some(problem) = dataset_live_problem(datasets, &host) {
                errors.push(format!("{label}: {problem}"));
                continue;
            }
        }
        let existing = current
            .iter()
            .find(|dataset| dataset.name == record.name)
            .cloned();
        let mut changes = Vec::new();
        let outcome = match &existing {
            None => Outcome::Created(None),
            Some(existing) => {
                if existing.host_path != value.host_path && !existing.attached_to.is_empty() {
                    Outcome::Skipped(
                        "the dataset is attached to an app; detach it before changing its host path"
                            .to_string(),
                    )
                } else if existing.shared && !value.shared && existing.attached_to.len() > 1 {
                    Outcome::Skipped(
                        "the dataset is shared by several apps; detach them before making it private"
                            .to_string(),
                    )
                } else {
                    if existing.host_path != value.host_path {
                        changes.push("hostPath".to_string());
                    }
                    if existing.shared != value.shared {
                        changes.push("shared".to_string());
                    }
                    if existing.warning_acknowledged != value.warning_acknowledged {
                        changes.push("warningAcknowledged".to_string());
                    }
                    if changes.is_empty() {
                        Outcome::Unchanged
                    } else {
                        Outcome::Updated(None, changes.clone())
                    }
                }
            }
        };
        plans.push(DatasetPlan {
            entry: record.name.clone(),
            value,
            existing,
            changes,
            outcome,
        });
    }
    plans
}

fn plan_apps(
    document: &ImportDocument,
    state: &AppState,
    errors: &mut Vec<String>,
) -> Vec<SkipReport> {
    let installed: HashSet<String> = state
        .app_manager
        .as_ref()
        .map(|manager| {
            manager
                .records()
                .into_iter()
                .map(|record| record.name)
                .collect()
        })
        .unwrap_or_default();
    let mut skipped = Vec::new();
    let mut seen = HashSet::new();

    for (index, record) in document.apps.iter().enumerate() {
        let label = format!("apps[{index}] ({})", record.name);
        if !seen.insert(record.name.clone()) {
            skipped.push(SkipReport {
                entry: record.name.clone(),
                reason: "another imported app already uses this name; only the first is reported"
                    .to_string(),
            });
            continue;
        }
        if let Err(e) = validate_app(record) {
            errors.push(format!("{label}: {e}"));
            continue;
        }
        let reason = if state.app_manager.is_none() {
            "the app manager is unavailable; apps are not restored by config import"
        } else if installed.contains(&record.name) {
            "already installed; config import does not reconfigure installed apps"
        } else {
            "not installed; reinstall it from the catalog and re-enter secret values"
        };
        skipped.push(SkipReport {
            entry: record.name.clone(),
            reason: reason.to_string(),
        });
    }
    skipped
}

fn validate_app(record: &AppRecord) -> Result<(), String> {
    apps::validate_release_name(&record.name)?;
    validate_text("source", &record.source, 256)?;
    validate_text("channel", &record.channel, 128)?;
    validate_text("chartVersion", &record.chart_version, 128)?;
    if !record.base_domain.is_empty() && !certs::is_dns1123_subdomain(&record.base_domain) {
        return Err("baseDomain is not a valid DNS name".to_string());
    }
    apps::validate_subdomain(&record.exposure.subdomain)?;
    crate::routing::validate_protected_paths(&record.exposure.protected_paths)?;
    if record.exposure.port != 0 && !(1..=65535).contains(&record.exposure.port) {
        return Err("exposure.port must be 0 or 1..65535".to_string());
    }
    if !record.exposure.scheme.is_empty()
        && record.exposure.scheme != "http"
        && record.exposure.scheme != "https"
    {
        return Err("exposure.scheme must be http or https".to_string());
    }
    if !record.exposure.service.is_empty()
        && !crate::chartsrepo::is_dns1123_label(&record.exposure.service)
    {
        return Err("exposure.service must be a lowercase DNS-1123 label".to_string());
    }
    for dataset in &record.datasets {
        if !crate::chartsrepo::is_dns1123_label(dataset) {
            return Err(format!(
                "dataset {dataset:?} must be a lowercase DNS-1123 label"
            ));
        }
    }
    let mut names = HashSet::new();
    for env in &record.env {
        if !crate::app_env::valid_name(&env.name) {
            return Err(format!(
                "environment variable {:?} has an invalid name",
                env.name
            ));
        }
        if !names.insert(env.name.as_str()) {
            return Err(format!("environment variable {:?} is duplicated", env.name));
        }
    }
    for key in &record.redacted_keys {
        validate_text("redactedKeys entry", key, 256)?;
    }
    if record.values_omitted && record.values.is_some() {
        return Err("values must be omitted when valuesOmitted is true".to_string());
    }
    if let Some(values) = &record.values {
        for (key, value) in values {
            if config_export::is_sensitive_key(key) {
                return Err(format!("values.{key} is credential-shaped"));
            }
            reject_sensitive_keys(value, &format!("values.{key}"))?;
        }
    }
    Ok(())
}

fn reject_sensitive_keys(value: &serde_json::Value, path: &str) -> Result<(), String> {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                if config_export::is_sensitive_key(key) {
                    return Err(format!("{path}.{key} is credential-shaped"));
                }
                reject_sensitive_keys(child, &format!("{path}.{key}"))?;
            }
        }
        serde_json::Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                reject_sensitive_keys(item, &format!("{path}[{index}]"))?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn provider_config(
    provider: &Provider,
    scope: &str,
    submitted: &BTreeMap<String, String>,
    label: &str,
) -> Result<HashMap<String, String>, String> {
    let fields = provider.fields_for(scope);
    let mut out = HashMap::new();
    for (key, value) in submitted {
        if config_export::is_free_form_value_key(key) {
            return Err(format!(
                "{label}: provider field {key:?} can carry a credential and cannot be imported"
            ));
        }
        if config_export::is_sensitive_key(key) {
            return Err(format!(
                "{label}: provider field {key:?} is credential-shaped"
            ));
        }
        let Some(field) = fields
            .iter()
            .find(|field| field.key.as_str() == key.as_str())
        else {
            return Err(format!(
                "{label}: provider field {key:?} is not declared by {:?}",
                provider.name
            ));
        };
        if field.secret {
            return Err(format!(
                "{label}: provider field {key:?} is a secret and cannot be imported"
            ));
        }
        crate::providers::validate_field(field, value).map_err(|e| format!("{label}: {e}"))?;
        if config_export::value_looks_sensitive(value) {
            return Err(format!(
                "{label}: provider field {key:?} looks like it carries a credential"
            ));
        }
        out.insert(key.clone(), value.clone());
    }
    for field in fields.iter().filter(|field| !field.secret) {
        if !out.contains_key(&field.key) && !field.default.is_empty() {
            out.insert(field.key.clone(), field.default.clone());
        }
    }
    Ok(out)
}

fn storage_changes(existing: &StorageSchedule, value: &StorageSchedule) -> Vec<String> {
    let mut changes = Vec::new();
    if existing.kind != value.kind {
        changes.push("kind".to_string());
    }
    if existing.target != value.target {
        changes.push("target".to_string());
    }
    if existing.cadence != value.cadence {
        changes.push("cadence".to_string());
    }
    if existing.run_at != value.run_at {
        changes.push("runAt".to_string());
    }
    if existing.keep != value.keep {
        changes.push("keep".to_string());
    }
    if existing.recursive != value.recursive {
        changes.push("recursive".to_string());
    }
    if existing.enabled != value.enabled {
        changes.push("enabled".to_string());
    }
    changes
}

fn buddy_changes(existing: &ScheduleEntry, value: &ScheduleEntry) -> Vec<String> {
    let mut changes = Vec::new();
    if existing.dataset != value.dataset {
        changes.push("dataset".to_string());
    }
    if existing.source != value.source {
        changes.push("source".to_string());
    }
    if existing.receiver_list() != value.receiver_list() {
        changes.push("receivers".to_string());
    }
    if existing.cadence != value.cadence {
        changes.push("cadence".to_string());
    }
    if existing.run_at != value.run_at {
        changes.push("runAt".to_string());
    }
    if existing.prune_keep != value.prune_keep {
        changes.push("pruneKeep".to_string());
    }
    if existing.enabled != value.enabled {
        changes.push("enabled".to_string());
    }
    changes
}

fn ddns_changes(existing: &ddns::Entry, value: &ddns::Entry) -> Vec<String> {
    let mut changes = Vec::new();
    if existing.provider != value.provider {
        changes.push("provider".to_string());
    }
    if existing.zone != value.zone {
        changes.push("zone".to_string());
    }
    if existing.record != value.record {
        changes.push("record".to_string());
    }
    if existing.record_type != value.record_type {
        changes.push("recordType".to_string());
    }
    if existing.ttl != value.ttl {
        changes.push("ttl".to_string());
    }
    if existing.enabled != value.enabled {
        changes.push("enabled".to_string());
    }
    if sorted_config(&existing.provider_config) != sorted_config(&value.provider_config) {
        changes.push("providerConfig".to_string());
    }
    changes
}

async fn apply_plan(state: &Arc<AppState>, plan: &mut Plan) {
    // The store writes are synchronous and fsync a file per record; keep them
    // off the async worker. The plan moves into the blocking task and comes
    // back enriched with the per-entry outcomes.
    let staged = std::mem::take(plan);
    let state_for_writes = state.clone();
    let written = tokio::task::spawn_blocking(move || {
        let mut staged = staged;
        let mut ddns_ids = HashMap::new();
        apply_shares(&state_for_writes, &mut staged.shares);
        apply_storage(&state_for_writes, &mut staged.storage);
        apply_buddy(&state_for_writes, &mut staged.buddy);
        apply_ddns(&state_for_writes, &mut staged.ddns, &mut ddns_ids);
        apply_datasets(&state_for_writes, &mut staged.datasets);
        (staged, ddns_ids)
    })
    .await;
    let (mut staged, ddns_ids) = match written {
        Ok(written) => written,
        Err(e) => {
            // A panicked writer must not look like a clean no-op import.
            plan.warnings
                .push(format!("the config import writer task failed: {e}"));
            plan.warnings.push(
                "no record outcomes were reported for this apply; re-run the dry-run plan before retrying"
                    .to_string(),
            );
            return;
        }
    };

    let mut warnings = Vec::new();
    apply_domains(state, &mut staged.domains, &ddns_ids, &mut warnings).await;
    staged.warnings.extend(warnings);
    *plan = staged;
}

fn apply_shares(state: &AppState, plans: &mut [SharePlan]) {
    let mut manager = state.shares.lock().unwrap();
    for plan in plans {
        if !is_write(&plan.outcome) {
            continue;
        }
        // The plan validated this path without holding the share lock. Check it
        // again immediately before the write so a symlink planted between plan
        // and apply cannot be exported (TOCTOU, R-052).
        if std::fs::metadata(manager.zfs_base()).is_ok() {
            if let Err(e) = manager.validate_path(&plan.create.path) {
                plan.outcome = Outcome::Failed(format!("revalidating the share path: {e}"));
                continue;
            }
        }
        let result = match &plan.existing {
            Some(_) => {
                let request = UpdateShareRequest {
                    path: Some(plan.create.path.clone()),
                    description: Some(plan.create.description.clone()),
                    read_only: Some(plan.create.read_only),
                    browseable: plan.create.browseable,
                    allowed_hosts: Some(plan.create.allowed_hosts.clone()),
                    valid_users: Some(plan.create.valid_users.clone()),
                    valid_groups: Some(plan.create.valid_groups.clone()),
                    time_machine: Some(plan.create.time_machine),
                    no_root_squash: Some(plan.create.no_root_squash),
                    shadow_copies: None,
                    audit: None,
                    enabled: plan.create.enabled,
                };
                manager.update(&plan.create.name, &request).map(|_| ())
            }
            None => manager.create(&plan.create).map(|_| ()),
        };
        if let Err(e) = result {
            plan.outcome = Outcome::Failed(format!("saving the share: {e}"));
        }
    }
}

fn apply_storage(state: &AppState, plans: &mut [StoragePlan]) {
    for plan in plans {
        if !is_write(&plan.outcome) {
            continue;
        }
        match &plan.existing {
            Some(existing) => {
                // Capture the persisted record (including scheduler runtime
                // state) so a failed save can be rolled back instead of being
                // reported as applied.
                let previous = state
                    .storage_schedules
                    .get(&existing.id)
                    .unwrap_or_else(|| existing.clone());
                let mut value = plan.value.clone();
                value.id = existing.id.clone();
                value.next_run = Some(schedule::next_run(value.cadence, &value.run_at, Utc::now()));
                let id = value.id.clone();
                match state.storage_schedules.update_config(value) {
                    Ok(_) => {
                        plan.outcome = Outcome::Updated(Some(id), plan.changes.clone());
                    }
                    Err(e) => {
                        let rollback = state.storage_schedules.put(previous);
                        plan.outcome = match rollback {
                            Ok(()) => Outcome::Failed(format!("saving the schedule: {e}")),
                            Err(rollback_error) => Outcome::Failed(format!(
                                "saving the schedule: {e}; rolling back the previous schedule also failed: {rollback_error}"
                            )),
                        };
                    }
                }
            }
            None => {
                let mut value = plan.value.clone();
                value.id = new_server_id();
                value.next_run = Some(schedule::next_run(value.cadence, &value.run_at, Utc::now()));
                let id = value.id.clone();
                match state.storage_schedules.put(value) {
                    Ok(()) => plan.outcome = Outcome::Created(Some(id)),
                    Err(e) => {
                        let rollback = state.storage_schedules.remove(&id);
                        plan.outcome = match rollback {
                            Ok(()) => Outcome::Failed(format!("saving the schedule: {e}")),
                            Err(rollback_error) => Outcome::Failed(format!(
                                "saving the schedule: {e}; removing the partial record also failed: {rollback_error}"
                            )),
                        };
                    }
                }
            }
        }
    }
}

fn apply_buddy(state: &AppState, plans: &mut [BuddyPlan]) {
    for plan in plans {
        if !is_write(&plan.outcome) {
            continue;
        }
        match &plan.existing {
            Some(existing) => {
                let previous = state
                    .buddy_schedules
                    .get(&existing.id)
                    .unwrap_or_else(|| existing.clone());
                let mut value = existing.clone();
                value.dataset = plan.value.dataset.clone();
                value.source = plan.value.source.clone();
                value.receiver = plan.value.receiver.clone();
                value.receivers = plan.value.receivers.clone();
                value.cadence = plan.value.cadence;
                value.run_at = plan.value.run_at.clone();
                value.prune_keep = plan.value.prune_keep;
                value.enabled = plan.value.enabled;
                value.next_run = Some(crate::server::buddy_schedules::compute_next_run(
                    &value,
                    Utc::now(),
                ));
                let id = value.id.clone();
                match state.buddy_schedules.put(value) {
                    Ok(()) => {
                        plan.outcome = Outcome::Updated(Some(id), plan.changes.clone());
                    }
                    Err(e) => {
                        let rollback = state.buddy_schedules.put(previous);
                        plan.outcome = match rollback {
                            Ok(()) => Outcome::Failed(format!("saving the schedule: {e}")),
                            Err(rollback_error) => Outcome::Failed(format!(
                                "saving the schedule: {e}; rolling back the previous schedule also failed: {rollback_error}"
                            )),
                        };
                    }
                }
            }
            None => {
                let mut value = plan.value.clone();
                value.id = new_server_id();
                value.next_run = Some(crate::server::buddy_schedules::compute_next_run(
                    &value,
                    Utc::now(),
                ));
                let id = value.id.clone();
                match state.buddy_schedules.put(value) {
                    Ok(()) => plan.outcome = Outcome::Created(Some(id)),
                    Err(e) => {
                        let rollback = state.buddy_schedules.remove(&id);
                        plan.outcome = match rollback {
                            Ok(()) => Outcome::Failed(format!("saving the schedule: {e}")),
                            Err(rollback_error) => Outcome::Failed(format!(
                                "saving the schedule: {e}; removing the partial record also failed: {rollback_error}"
                            )),
                        };
                    }
                }
            }
        }
    }
}

fn apply_ddns(state: &AppState, plans: &mut [DdnsPlan], ids: &mut HashMap<String, String>) {
    let Some(manager) = &state.ddns else {
        return;
    };
    for plan in plans {
        if let Some(existing) = &plan.existing {
            ids.insert(plan.original_id.clone(), existing.id.clone());
        }
        if !is_write(&plan.outcome) {
            continue;
        }
        match &plan.existing {
            Some(existing) => {
                let previous = manager.store().get(&existing.id).ok();
                let mut value = plan.value.clone();
                value.id = existing.id.clone();
                let id = value.id.clone();
                match manager.store().put(value) {
                    Ok(()) => {
                        ids.insert(plan.original_id.clone(), id.clone());
                        plan.outcome = Outcome::Updated(Some(id), plan.changes.clone());
                    }
                    Err(e) => {
                        let rollback = match previous {
                            Some(previous) => manager.store().put(previous),
                            None => Ok(()),
                        };
                        plan.outcome = match rollback {
                            Ok(()) => Outcome::Failed(format!("saving the DDNS entry: {e}")),
                            Err(rollback_error) => Outcome::Failed(format!(
                                "saving the DDNS entry: {e}; rolling back the previous entry also failed: {rollback_error}"
                            )),
                        };
                    }
                }
            }
            None => {
                let mut value = plan.value.clone();
                value.id = ddns::new_id();
                let id = value.id.clone();
                match manager.store().put(value) {
                    Ok(()) => {
                        ids.insert(plan.original_id.clone(), id.clone());
                        plan.outcome = Outcome::Created(Some(id));
                    }
                    Err(e) => {
                        let rollback = manager.store().delete(&id);
                        plan.outcome = match rollback {
                            Ok(()) => Outcome::Failed(format!("saving the DDNS entry: {e}")),
                            Err(rollback_error) => Outcome::Failed(format!(
                                "saving the DDNS entry: {e}; removing the partial entry also failed: {rollback_error}"
                            )),
                        };
                    }
                }
            }
        }
    }
}

async fn apply_domains(
    state: &Arc<AppState>,
    plans: &mut [DomainPlan],
    ddns_ids: &HashMap<String, String>,
    warnings: &mut Vec<String>,
) {
    for plan in plans {
        if !is_write(&plan.outcome) {
            continue;
        }
        let reference = match &plan.ddns_ref {
            None => None,
            Some(DdnsRef::Existing(id)) => Some(id.clone()),
            Some(DdnsRef::Imported { original, .. }) => match ddns_ids.get(original) {
                Some(id) => Some(id.clone()),
                None => {
                    plan.outcome =
                        Outcome::Skipped("the referenced DDNS entry was not imported".to_string());
                    continue;
                }
            },
        };
        let mut value = plan.value.clone();
        value.app_records_ddns_entry = reference.clone();
        let mut changes = plan.changes.clone();
        if let Some(existing) = &plan.existing {
            if existing.app_records_ddns_entry == reference {
                changes.retain(|change| change != "appRecordsDdnsEntry");
            } else if !changes.iter().any(|change| change == "appRecordsDdnsEntry") {
                changes.push("appRecordsDdnsEntry".to_string());
            }
            if changes.is_empty() {
                plan.outcome = Outcome::Unchanged;
                continue;
            }
        }
        let now = Utc::now();
        value.updated_at = Some(now);
        if plan.existing.is_none() {
            value.created_at = Some(now);
        }
        let base_domain = value.base_domain.clone();
        let previous = plan.existing.clone();
        if let Err(e) = state.domains.upsert(value.clone()) {
            plan.outcome = Outcome::Failed(format!("saving the domain: {e}"));
            continue;
        }

        // Enforce the SSO invariants through the same handler the UI uses.
        // `sync_sso_state` is private to `server::domains`, so calling the
        // handler is the shared hook that also runs the Authelia sync.
        if base_domain != state.base_domain {
            let response = crate::server::domains::domain_sso_post(
                State(state.clone()),
                Path(base_domain.clone()),
                Body::from(serde_json::json!({ "enabled": value.sso }).to_string()),
            )
            .await;
            if !response.status().is_success() {
                let status = response.status();
                let detail = axum::body::to_bytes(response.into_body(), 64 * 1024)
                    .await
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                    .and_then(|body| {
                        body.get("error")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                    })
                    .unwrap_or_else(|| status.to_string());
                let rollback = match &previous {
                    Some(previous) => state.domains.upsert(previous.clone()),
                    None => state.domains.delete(&base_domain),
                };
                plan.outcome = match rollback {
                    Ok(()) => Outcome::Failed(format!(
                        "saving the domain: the imported sso value was rejected: {detail}"
                    )),
                    Err(rollback_error) => Outcome::Failed(format!(
                        "saving the domain: the imported sso value was rejected: {detail}; rolling back the previous domain also failed: {rollback_error}"
                    )),
                };
                continue;
            }
        }

        // Re-render the Issuer/Certificate for the imported provider config
        // and reconcile the app DNS records, exactly like PUT /api/domains.
        let domain = state.domains.get(&base_domain).unwrap_or(value);
        if let Some(certs) = &state.certs {
            match certs.apply(&domain).await {
                Ok(()) => {
                    let _ = state.domains.update(&base_domain, |d| d.last_error.clear());
                }
                Err(e) => {
                    let error = e.clone();
                    let _ = state.domains.update(&base_domain, |d| {
                        d.last_error = error.clone();
                        d.updated_at = Some(Utc::now());
                    });
                    warnings.push(format!(
                        "domain {base_domain:?}: certificate reconciliation failed: {e}"
                    ));
                }
            }
        }
        if let Some(manager) = &state.app_manager {
            manager.reconcile_dns_for_domain(&base_domain).await;
        }
        plan.outcome = match &plan.existing {
            None => Outcome::Created(None),
            Some(_) => Outcome::Updated(None, changes),
        };
    }
}

fn apply_datasets(state: &AppState, plans: &mut [DatasetPlan]) {
    for plan in plans {
        if !is_write(&plan.outcome) {
            continue;
        }
        let previous = plan
            .existing
            .as_ref()
            .and_then(|existing| state.datasets_registry.get(&existing.name));
        let result = match &plan.existing {
            Some(_) => state
                .datasets_registry
                .update_config(plan.value.clone())
                .map(|_| ()),
            None => state.datasets_registry.add(plan.value.clone()).map(|_| ()),
        };
        match result {
            Ok(()) => {
                plan.outcome = match &plan.existing {
                    None => Outcome::Created(None),
                    Some(_) => Outcome::Updated(None, plan.changes.clone()),
                };
            }
            Err(e) => {
                let rollback = match previous {
                    Some(previous) => state.datasets_registry.update_config(previous).map(|_| ()),
                    None => state.datasets_registry.remove(&plan.value.name),
                };
                plan.outcome = match rollback {
                    Ok(()) => Outcome::Failed(format!("saving the dataset: {e}")),
                    Err(rollback_error) => Outcome::Failed(format!(
                        "saving the dataset: {e}; rolling back the previous record also failed: {rollback_error}"
                    )),
                };
            }
        }
    }
}

fn is_write(outcome: &Outcome) -> bool {
    matches!(outcome, Outcome::Created(_) | Outcome::Updated(_, _))
}

fn build_report(plan: &Plan, schema_version: u32, dry_run: bool) -> ImportReport {
    let mut sections = BTreeMap::new();
    let mut shares = section_report(
        plan.shares
            .iter()
            .map(|entry| (entry.entry.as_str(), &entry.outcome)),
    );
    if plan
        .shares
        .iter()
        .any(|entry| matches!(entry.outcome, Outcome::Created(_) | Outcome::Updated(_, _)))
    {
        shares.notes.push(
            "share records were written; POST /api/shares/apply pushes them to Samba and NFS"
                .to_string(),
        );
    }
    sections.insert("shares".to_string(), shares);

    sections.insert(
        "storageSchedules".to_string(),
        section_report(
            plan.storage
                .iter()
                .map(|entry| (entry.entry.as_str(), &entry.outcome)),
        ),
    );
    sections.insert(
        "buddySchedules".to_string(),
        section_report(
            plan.buddy
                .iter()
                .map(|entry| (entry.entry.as_str(), &entry.outcome)),
        ),
    );

    let mut ddns = section_report(
        plan.ddns
            .iter()
            .map(|entry| (entry.entry.as_str(), &entry.outcome)),
    );
    if plan
        .ddns
        .iter()
        .any(|entry| entry.requires_credentials && matches!(entry.outcome, Outcome::Created(_)))
    {
        ddns.notes.push(
            "provider credentials are not exported; enter them with PUT /api/ddns/{id} before the entries can update"
                .to_string(),
        );
    }
    if plan
        .ddns
        .iter()
        .any(|entry| entry.needs_config && matches!(entry.outcome, Outcome::Created(_)))
    {
        ddns.notes.push(
            "required provider fields that can embed a credential (for example the generic updateUrl/body) are not exported; re-enter them with PUT /api/ddns/{id}"
                .to_string(),
        );
    }
    sections.insert("ddnsEntries".to_string(), ddns);

    let mut domains = section_report(
        plan.domains
            .iter()
            .map(|entry| (entry.entry.as_str(), &entry.outcome)),
    );
    if plan.domains.iter().any(|entry| {
        entry.requires_credentials
            && matches!(entry.outcome, Outcome::Created(_) | Outcome::Updated(_, _))
    }) {
        domains.notes.push(
            "provider credentials are not exported; re-enter them with PUT /api/domains/{baseDomain} so certificates can reconcile"
                .to_string(),
        );
    }
    sections.insert("domains".to_string(), domains);

    sections.insert(
        "datasetRegistry".to_string(),
        section_report(
            plan.datasets
                .iter()
                .map(|entry| (entry.entry.as_str(), &entry.outcome)),
        ),
    );

    let mut apps = SectionReport::default();
    apps.skipped = plan.apps.clone();
    if !plan.apps.is_empty() {
        apps.notes.push(
            "apps are installed from the catalog, not by config import; secret values and environment values must be re-entered"
                .to_string(),
        );
    }
    sections.insert("apps".to_string(), apps);

    let mut totals = Totals::default();
    for section in sections.values() {
        totals.created += section.created.len();
        totals.updated += section.updated.len();
        totals.unchanged += section.unchanged.len();
        totals.skipped += section.skipped.len();
        totals.failed += section.failed.len();
    }
    // An apply that saved nothing new is still applied (idempotent re-run); a
    // partially saved plan is reported as partial, never as fully applied.
    let wrote = totals.created + totals.updated > 0;
    let applied = !dry_run && totals.failed == 0;
    let partial = !dry_run && totals.failed > 0 && wrote;
    ImportReport {
        kind: KIND,
        schema_version,
        dry_run,
        applied,
        partial,
        structural_only: plan.structural_only,
        sections,
        totals,
        warnings: plan.warnings.clone(),
    }
}

fn section_report<'a>(items: impl Iterator<Item = (&'a str, &'a Outcome)>) -> SectionReport {
    let mut section = SectionReport::default();
    for (entry, outcome) in items {
        match outcome {
            Outcome::Created(id) => section.created.push(CreatedReport {
                entry: entry.to_string(),
                id: id.clone(),
            }),
            Outcome::Updated(id, changed) => section.updated.push(UpdatedReport {
                entry: entry.to_string(),
                id: id.clone(),
                changed: changed.clone(),
            }),
            Outcome::Unchanged => section.unchanged.push(entry.to_string()),
            Outcome::Skipped(reason) => section.skipped.push(SkipReport {
                entry: entry.to_string(),
                reason: reason.clone(),
            }),
            Outcome::Failed(reason) => section.failed.push(SkipReport {
                entry: entry.to_string(),
                reason: reason.clone(),
            }),
        }
    }
    section
}

fn plan_created(plan: &Plan) -> usize {
    plan.shares
        .iter()
        .filter(|entry| matches!(entry.outcome, Outcome::Created(_)))
        .count()
        + plan
            .storage
            .iter()
            .filter(|entry| matches!(entry.outcome, Outcome::Created(_)))
            .count()
        + plan
            .buddy
            .iter()
            .filter(|entry| matches!(entry.outcome, Outcome::Created(_)))
            .count()
        + plan
            .ddns
            .iter()
            .filter(|entry| matches!(entry.outcome, Outcome::Created(_)))
            .count()
        + plan
            .domains
            .iter()
            .filter(|entry| matches!(entry.outcome, Outcome::Created(_)))
            .count()
        + plan
            .datasets
            .iter()
            .filter(|entry| matches!(entry.outcome, Outcome::Created(_)))
            .count()
}

fn plan_updated(plan: &Plan) -> usize {
    plan.shares
        .iter()
        .filter(|entry| matches!(entry.outcome, Outcome::Updated(_, _)))
        .count()
        + plan
            .storage
            .iter()
            .filter(|entry| matches!(entry.outcome, Outcome::Updated(_, _)))
            .count()
        + plan
            .buddy
            .iter()
            .filter(|entry| matches!(entry.outcome, Outcome::Updated(_, _)))
            .count()
        + plan
            .ddns
            .iter()
            .filter(|entry| matches!(entry.outcome, Outcome::Updated(_, _)))
            .count()
        + plan
            .domains
            .iter()
            .filter(|entry| matches!(entry.outcome, Outcome::Updated(_, _)))
            .count()
        + plan
            .datasets
            .iter()
            .filter(|entry| matches!(entry.outcome, Outcome::Updated(_, _)))
            .count()
}

fn kind_name(kind: ScheduleKind) -> &'static str {
    match kind {
        ScheduleKind::Snapshot => "snapshot",
        ScheduleKind::Scrub => "scrub",
    }
}

fn merged_config(
    existing: Option<&HashMap<String, String>>,
    imported: HashMap<String, String>,
) -> HashMap<String, String> {
    let mut out = existing.cloned().unwrap_or_default();
    out.extend(imported);
    out
}

fn sorted_config(config: &HashMap<String, String>) -> BTreeMap<String, String> {
    config
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

async fn live_datasets(state: &AppState) -> Option<Vec<crate::agent::Dataset>> {
    let agent = state.agent.as_ref()?;
    agent.list_datasets().await.ok()
}

async fn live_mountpoints(state: &AppState) -> Option<Vec<String>> {
    live_datasets(state).await.map(|datasets| {
        datasets
            .into_iter()
            .map(|dataset| dataset.mountpoint)
            .collect()
    })
}

fn dataset_live_problem(datasets: &[crate::agent::Dataset], host: &str) -> Option<String> {
    let mounted = datasets.iter().any(|dataset| {
        datasets::normalize_host_path(&dataset.mountpoint)
            .ok()
            .as_deref()
            == Some(host)
    });
    if !mounted {
        return Some("host path is not a mounted ZFS dataset known to the agent".to_string());
    }
    let buddy_paths: Vec<String> = datasets
        .iter()
        .filter(|dataset| dataset.name == "naslos-buddy" || dataset.name.ends_with("/naslos-buddy"))
        .filter_map(|dataset| datasets::normalize_host_path(&dataset.mountpoint).ok())
        .collect();
    if datasets::overlaps_buddy(host, &buddy_paths) {
        return Some(
            "the Buddy receive store (or a parent path) cannot be registered as an app dataset"
                .to_string(),
        );
    }
    None
}

fn validate_imported_id(id: &str, label: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err(format!("{label}: id is required"));
    }
    if id.len() > 128 {
        return Err(format!("{label}: id must be 128 characters or fewer"));
    }
    if !id.chars().all(|c| c.is_ascii_graphic()) {
        return Err(format!(
            "{label}: id must contain only visible ASCII characters"
        ));
    }
    Ok(())
}

fn validate_text(label: &str, value: &str, max: usize) -> Result<(), String> {
    if value.chars().any(char::is_control) {
        return Err(format!("{label} must not contain control characters"));
    }
    if value.len() > max {
        return Err(format!("{label} must be {max} characters or fewer"));
    }
    Ok(())
}

fn new_server_id() -> String {
    let mut buffer = [0u8; 16];
    if getrandom::fill(&mut buffer).is_ok() {
        return buffer.iter().map(|byte| format!("{byte:02x}")).collect();
    }
    use std::sync::atomic::{AtomicU64, Ordering};
    static FALLBACK: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let sequence = FALLBACK.fetch_add(1, Ordering::Relaxed);
    format!("fallback-{nanos:x}-{sequence:x}")
}

fn bad_request(message: &str) -> Response {
    json(
        StatusCode::BAD_REQUEST,
        serde_json::json!({ "error": message }),
    )
}

fn json(status: StatusCode, value: serde_json::Value) -> Response {
    crate::json(status, value)
}

#[cfg(test)]
mod tests;
