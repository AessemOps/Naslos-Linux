use super::*;
use crate::server::config_export;
use serde_json::{json, Value};

fn base_doc() -> Value {
    json!({
        "kind": "naslos-config-export",
        "schemaVersion": 1,
        "generatedAt": "2026-11-22T00:00:00Z",
        "naslosVersion": "0.6.0",
        "baseDomain": "",
        "shares": [],
        "storageSchedules": [],
        "buddySchedules": [],
        "domains": [],
        "ddnsEntries": [],
        "apps": [],
        "datasetRegistry": []
    })
}

fn parse(value: Value) -> ImportDocument {
    serde_json::from_value(value).unwrap()
}

fn share(name: &str, path: &str) -> Value {
    json!({
        "name": name,
        "path": path,
        "protocol": "smb",
        "description": "",
        "readOnly": false,
        "browseable": true,
        "allowedHosts": [],
        "validUsers": [],
        "validGroups": [],
        "timeMachine": false,
        "noRootSquash": false,
        "enabled": true
    })
}

fn app(name: &str, values: Value) -> Value {
    json!({
        "name": name,
        "source": "official",
        "channel": "stable",
        "chartVersion": "1.0.0",
        "baseDomain": "",
        "exposure": {
            "subdomain": "",
            "tls": false,
            "auth": false,
            "localOnly": false,
            "protectedPaths": [],
            "internet": true,
            "service": "",
            "port": 0,
            "scheme": ""
        },
        "datasets": [],
        "privileged": false,
        "env": [],
        "values": values,
        "redactedKeys": [],
        "valuesOmitted": false
    })
}

fn ddns_manager(state: &AppState) -> ddns::Manager {
    ddns::Manager::new(ddns::Options {
        registry: state.providers.clone(),
        store_path: String::new(),
        apps_namespace: "naslos-apps".into(),
        kube: None,
        ip_sources: Vec::new(),
        ipv6_sources: Vec::new(),
        interval: std::time::Duration::ZERO,
        cooldown: std::time::Duration::ZERO,
    })
}

#[test]
fn unknown_top_level_and_record_fields_are_rejected() {
    let mut value = base_doc();
    value["extra"] = json!(true);
    assert!(serde_json::from_value::<ImportDocument>(value).is_err());

    let mut value = base_doc();
    value["shares"] = json!([{
        "name": "media",
        "path": "/var/mnt/tank/media",
        "protocol": "smb",
        "description": "",
        "readOnly": false,
        "browseable": true,
        "allowedHosts": [],
        "validUsers": [],
        "validGroups": [],
        "timeMachine": false,
        "noRootSquash": false,
        "enabled": true,
        "password": "must-not-pass"
    }]);
    assert!(serde_json::from_value::<ImportDocument>(value).is_err());
}

#[test]
fn envelope_version_and_kind_are_strict() {
    let mut value = base_doc();
    value["schemaVersion"] = json!(2);
    let error = check_envelope(&parse(value)).unwrap_err();
    assert!(error.contains("newer"), "{error}");

    let mut value = base_doc();
    value["schemaVersion"] = json!(0);
    let error = check_envelope(&parse(value)).unwrap_err();
    assert!(error.contains("unsupported"), "{error}");

    let mut value = base_doc();
    value["kind"] = json!("something-else");
    let error = check_envelope(&parse(value)).unwrap_err();
    assert!(error.contains("kind"), "{error}");
}

#[tokio::test]
async fn secret_shaped_app_values_are_rejected() {
    let state = AppState::for_test("test-secret");
    let mut value = base_doc();
    value["apps"] = json!([app(
        "demo",
        json!({"nested": {"apiToken": "must-not-pass"}})
    )]);
    let errors = build_plan(&state, &parse(value)).await.unwrap_err();
    assert!(
        errors.iter().any(|error| error.contains("apiToken")),
        "{errors:?}"
    );

    let mut value = base_doc();
    value["apps"] = json!([app("demo", json!({"plainToken": "must-not-pass"}))]);
    let errors = build_plan(&state, &parse(value)).await.unwrap_err();
    assert!(
        errors.iter().any(|error| error.contains("plainToken")),
        "{errors:?}"
    );
}

#[tokio::test]
async fn secret_and_free_form_provider_fields_are_rejected() {
    let state = AppState::for_test("test-secret");
    let mut value = base_doc();
    value["domains"] = json!([{
        "baseDomain": "example.test",
        "dnsProvider": "cloudflare",
        "providerConfig": {"apiToken": "must-not-pass"},
        "acmeEmail": "ops@example.test",
        "environment": "staging",
        "primary": false,
        "sso": false,
        "appRecordsDdnsEntry": null
    }]);
    let errors = build_plan(&state, &parse(value)).await.unwrap_err();
    assert!(
        errors.iter().any(|error| error.contains("apiToken")),
        "{errors:?}"
    );

    let mut value = base_doc();
    value["ddnsEntries"] = json!([{
        "id": "ddns-1",
        "provider": "generic",
        "zone": "example.test",
        "record": "home",
        "recordType": "A",
        "ttl": 300,
        "enabled": true,
        "providerConfig": {"updateUrl": "https://example.test/update?token=must-not-pass"}
    }]);
    let errors = build_plan(&state, &parse(value)).await.unwrap_err();
    assert!(
        errors.iter().any(|error| error.contains("updateUrl")),
        "{errors:?}"
    );

    let mut value = base_doc();
    value["ddnsEntries"] = json!([{
        "id": "ddns-1",
        "provider": "generic",
        "zone": "example.test",
        "record": "home",
        "recordType": "A",
        "ttl": 300,
        "enabled": true,
        "providerConfig": {"successContains": "token=must-not-pass"}
    }]);
    let errors = build_plan(&state, &parse(value)).await.unwrap_err();
    assert!(
        errors.iter().any(|error| error.contains("successContains")),
        "{errors:?}"
    );
}

#[tokio::test]
async fn invalid_paths_datasets_pools_and_providers_are_rejected() {
    let state = AppState::for_test("test-secret");

    let mut value = base_doc();
    value["shares"] = json!([share("outside", "/etc/outside")]);
    let errors = build_plan(&state, &parse(value)).await.unwrap_err();
    assert!(
        errors.iter().any(|error| error.contains("/var/mnt")),
        "{errors:?}"
    );

    let mut value = base_doc();
    value["domains"] = json!([{
        "baseDomain": "example.test",
        "dnsProvider": "not-a-provider",
        "providerConfig": {},
        "acmeEmail": "ops@example.test",
        "environment": "staging",
        "primary": false,
        "sso": false,
        "appRecordsDdnsEntry": null
    }]);
    let errors = build_plan(&state, &parse(value)).await.unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.contains("unknown DNS provider")),
        "{errors:?}"
    );

    let mut value = base_doc();
    value["storageSchedules"] = json!([{
        "id": "snap-1",
        "kind": "scrub",
        "target": "bad/name",
        "cadence": "daily",
        "runAt": "02:30",
        "keep": 0,
        "recursive": false,
        "enabled": true
    }]);
    let errors = build_plan(&state, &parse(value)).await.unwrap_err();
    assert!(
        errors.iter().any(|error| error.contains("pool")),
        "{errors:?}"
    );

    let mut value = base_doc();
    value["datasetRegistry"] = json!([{
        "name": "Bad_Name",
        "hostPath": "/var/mnt/tank/data",
        "shared": false,
        "warningAcknowledged": false
    }]);
    let errors = build_plan(&state, &parse(value)).await.unwrap_err();
    assert!(
        errors.iter().any(|error| error.contains("DNS-1123")),
        "{errors:?}"
    );
}

#[tokio::test]
async fn dry_run_plan_is_pure_and_apply_is_idempotent() {
    let state = AppState::for_test("test-secret");
    state
        .shares
        .lock()
        .unwrap()
        .create(&CreateShareRequest {
            name: "keep".into(),
            path: "/var/mnt/tank/keep".into(),
            protocol: Protocol::Smb,
            enabled: Some(true),
            ..Default::default()
        })
        .unwrap();

    let mut value = base_doc();
    value["shares"] = json!([share("added", "/var/mnt/tank/added")]);
    let document = parse(value);

    let mut plan = build_plan(&state, &document).await.unwrap();
    assert_eq!(plan.shares.len(), 1);
    assert!(matches!(plan.shares[0].outcome, Outcome::Created(None)));
    assert!(state.shares.lock().unwrap().get("added").is_err());

    apply_plan(&state, &mut plan);
    assert!(state.shares.lock().unwrap().get("added").is_ok());
    assert!(state.shares.lock().unwrap().get("keep").is_ok());

    let plan = build_plan(&state, &document).await.unwrap();
    assert!(matches!(plan.shares[0].outcome, Outcome::Unchanged));
}

#[tokio::test]
async fn the_same_zone_record_and_type_can_use_different_providers() {
    let mut state = AppState::for_test("test-secret");
    state.ddns = Some(Arc::new(ddns_manager(&state)));
    let mut value = base_doc();
    value["ddnsEntries"] = json!([
        {
            "id": "ddns-cloudflare",
            "provider": "cloudflare",
            "zone": "example.test",
            "record": "home",
            "recordType": "A",
            "ttl": 300,
            "enabled": true,
            "providerConfig": {}
        },
        {
            "id": "ddns-duckdns",
            "provider": "duckdns",
            "zone": "example.test",
            "record": "home",
            "recordType": "A",
            "ttl": 300,
            "enabled": true,
            "providerConfig": {}
        }
    ]);
    let plan = build_plan(&state, &parse(value)).await.unwrap();
    assert_eq!(plan.ddns.len(), 2);
    assert!(plan
        .ddns
        .iter()
        .all(|entry| matches!(entry.outcome, Outcome::Created(_))));
}

#[tokio::test]
async fn exported_ids_are_never_reused_for_new_records() {
    let mut state = AppState::for_test("test-secret");
    state.ddns = Some(Arc::new(ddns_manager(&state)));
    let mut value = base_doc();
    value["storageSchedules"] = json!([{
        "id": "evil-storage-id",
        "kind": "snapshot",
        "target": "tank/media",
        "cadence": "daily",
        "runAt": "02:30",
        "keep": 3,
        "recursive": false,
        "enabled": true
    }]);
    value["buddySchedules"] = json!([{
        "id": "evil-buddy-id",
        "dataset": "tank/media",
        "source": "tank/media",
        "receivers": ["https://buddy.example.test"],
        "cadence": "weekly",
        "runAt": "Sun 03:00",
        "pruneKeep": 5,
        "enabled": true
    }]);
    value["ddnsEntries"] = json!([{
        "id": "evil-ddns-id",
        "provider": "cloudflare",
        "zone": "example.test",
        "record": "home",
        "recordType": "A",
        "ttl": 300,
        "enabled": true,
        "providerConfig": {}
    }]);

    let mut plan = build_plan(&state, &parse(value)).await.unwrap();
    apply_plan(&state, &mut plan);

    let storage = state.storage_schedules.list();
    assert_eq!(storage.len(), 1);
    assert_ne!(storage[0].id, "evil-storage-id");
    assert_eq!(storage[0].id.len(), 32);

    let buddy = state.buddy_schedules.list();
    assert_eq!(buddy.len(), 1);
    assert_ne!(buddy[0].id, "evil-buddy-id");
    assert_eq!(buddy[0].id.len(), 32);

    let ddns = state.ddns.as_ref().unwrap().store().list();
    assert_eq!(ddns.len(), 1);
    assert_ne!(ddns[0].id, "evil-ddns-id");
    assert_eq!(ddns[0].id.len(), 16);
    assert!(ddns[0].credentials_secret.is_empty());
}

#[tokio::test]
async fn domain_references_follow_the_server_generated_ddns_id() {
    let mut state = AppState::for_test("test-secret");
    state.ddns = Some(Arc::new(ddns_manager(&state)));
    let mut value = base_doc();
    value["ddnsEntries"] = json!([{
        "id": "exported-ddns",
        "provider": "cloudflare",
        "zone": "example.test",
        "record": "home",
        "recordType": "A",
        "ttl": 300,
        "enabled": true,
        "providerConfig": {}
    }]);
    value["domains"] = json!([{
        "baseDomain": "example.test",
        "dnsProvider": "cloudflare",
        "providerConfig": {"proxied": "false"},
        "acmeEmail": "ops@example.test",
        "environment": "staging",
        "primary": false,
        "sso": false,
        "appRecordsDdnsEntry": "exported-ddns"
    }]);

    let mut plan = build_plan(&state, &parse(value)).await.unwrap();
    apply_plan(&state, &mut plan);

    let ddns_id = state.ddns.as_ref().unwrap().store().list()[0].id.clone();
    let domain = state.domains.get("example.test").unwrap();
    assert_eq!(
        domain.app_records_ddns_entry.as_deref(),
        Some(ddns_id.as_str())
    );
    assert!(domain.credentials_secret.is_empty());
}

#[tokio::test]
async fn ambiguous_existing_schedules_are_skipped() {
    let state = AppState::for_test("test-secret");
    for id in ["one", "two"] {
        state
            .storage_schedules
            .put(StorageSchedule {
                id: id.into(),
                kind: ScheduleKind::Snapshot,
                target: "tank/media".into(),
                cadence: Cadence::Daily,
                run_at: "02:30".into(),
                keep: 1,
                enabled: true,
                ..Default::default()
            })
            .unwrap();
    }
    let mut value = base_doc();
    value["storageSchedules"] = json!([{
        "id": "imported",
        "kind": "snapshot",
        "target": "tank/media",
        "cadence": "daily",
        "runAt": "02:30",
        "keep": 2,
        "recursive": false,
        "enabled": true
    }]);
    let plan = build_plan(&state, &parse(value)).await.unwrap();
    assert!(matches!(plan.storage[0].outcome, Outcome::Skipped(_)));
    assert_eq!(state.storage_schedules.list().len(), 2);
}

#[tokio::test]
async fn apps_are_validated_then_reported_as_skipped() {
    let state = AppState::for_test("test-secret");
    let mut value = base_doc();
    value["apps"] = json!([app("demo", json!({"replicas": 2}))]);
    let document = parse(value);
    let plan = build_plan(&state, &document).await.unwrap();
    assert_eq!(plan.apps.len(), 1);
    let report = build_report(&plan, 1, true);
    assert_eq!(report.sections["apps"].skipped.len(), 1);
    assert_eq!(report.totals.skipped, 1);
}

#[tokio::test]
async fn an_app_without_values_is_valid_even_when_the_catalog_was_missing() {
    let state = AppState::for_test("test-secret");
    let mut value = base_doc();
    let mut record = app("demo", json!({}));
    record.as_object_mut().unwrap().remove("values");
    value["apps"] = json!([record]);
    let plan = build_plan(&state, &parse(value)).await.unwrap();
    assert_eq!(plan.apps.len(), 1);
}

#[tokio::test]
async fn existing_free_form_provider_config_is_preserved() {
    let mut state = AppState::for_test("test-secret");
    state.ddns = Some(Arc::new(ddns_manager(&state)));
    state
        .ddns
        .as_ref()
        .unwrap()
        .store()
        .put(ddns::Entry {
            id: "generic-existing".into(),
            provider: "generic".into(),
            zone: "example.test".into(),
            record: "home".into(),
            record_type: "A".into(),
            ttl: 300,
            enabled: true,
            provider_config: std::collections::HashMap::from([
                ("updateUrl".into(), "https://example.test/update".into()),
                ("body".into(), "ip=${ip}".into()),
                ("method".into(), "GET".into()),
            ]),
            credentials_secret: "existing-secret".into(),
            credential_fields: vec!["token".into()],
            ..Default::default()
        })
        .unwrap();

    let mut value = base_doc();
    value["ddnsEntries"] = json!([{
        "id": "exported-generic",
        "provider": "generic",
        "zone": "example.test",
        "record": "home",
        "recordType": "A",
        "ttl": 300,
        "enabled": true,
        "providerConfig": {"method": "GET"}
    }]);
    let mut plan = build_plan(&state, &parse(value)).await.unwrap();
    apply_plan(&state, &mut plan);

    let stored = state.ddns.as_ref().unwrap().store().list();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].id, "generic-existing");
    assert_eq!(
        stored[0]
            .provider_config
            .get("updateUrl")
            .map(String::as_str),
        Some("https://example.test/update")
    );
    assert_eq!(
        stored[0].provider_config.get("body").map(String::as_str),
        Some("ip=${ip}")
    );
    assert_eq!(stored[0].credentials_secret, "existing-secret");
}

#[tokio::test]
async fn an_exported_document_round_trips_through_import() {
    let mut source = AppState::for_test("test-secret");
    source.ddns = Some(Arc::new(ddns_manager(&source)));
    source
        .shares
        .lock()
        .unwrap()
        .create(&CreateShareRequest {
            name: "media".into(),
            path: "/var/mnt/tank/media".into(),
            protocol: Protocol::Smb,
            description: "Media".into(),
            enabled: Some(true),
            ..Default::default()
        })
        .unwrap();
    source
        .storage_schedules
        .put(StorageSchedule {
            id: "snap-source".into(),
            kind: ScheduleKind::Snapshot,
            target: "tank/media".into(),
            cadence: Cadence::Daily,
            run_at: "02:30".into(),
            keep: 3,
            recursive: true,
            enabled: true,
            ..Default::default()
        })
        .unwrap();
    source
        .buddy_schedules
        .put(ScheduleEntry {
            id: "buddy-source".into(),
            dataset: "tank/media".into(),
            source: "tank/media".into(),
            receiver: "https://buddy.example.test".into(),
            cadence: Cadence::Weekly,
            run_at: "Sun 03:00".into(),
            prune_keep: 5,
            enabled: true,
            ..Default::default()
        })
        .unwrap();
    source
        .domains
        .upsert(Domain {
            base_domain: "example.test".into(),
            dns_provider: "cloudflare".into(),
            credentials_secret: "must-not-export".into(),
            provider_config: std::collections::HashMap::from([("proxied".into(), "false".into())]),
            acme_email: "ops@example.test".into(),
            environment: "staging".into(),
            ..Default::default()
        })
        .unwrap();
    source
        .ddns
        .as_ref()
        .unwrap()
        .store()
        .put(ddns::Entry {
            id: "ddns-source".into(),
            provider: "cloudflare".into(),
            zone: "example.test".into(),
            record: "home".into(),
            record_type: "A".into(),
            ttl: 300,
            enabled: true,
            provider_config: std::collections::HashMap::from([("proxied".into(), "false".into())]),
            ..Default::default()
        })
        .unwrap();
    source
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
    source
        .domains
        .update("example.test", |domain| {
            domain.app_records_ddns_entry = Some("ddns-source".into());
        })
        .unwrap();

    let exported = serde_json::to_value(config_export::build(&source, Utc::now())).unwrap();
    let document: ImportDocument = serde_json::from_value(exported).unwrap();

    let mut target = AppState::for_test("test-secret");
    target.ddns = Some(Arc::new(ddns_manager(&target)));
    let mut plan = build_plan(&target, &document).await.unwrap();
    apply_plan(&target, &mut plan);

    let reexported = serde_json::to_value(config_export::build(&target, Utc::now())).unwrap();
    assert_eq!(reexported["shares"][0]["name"], "media");
    assert_eq!(reexported["shares"][0]["path"], "/var/mnt/tank/media");
    assert_eq!(
        reexported["datasetRegistry"],
        json!([{
            "name": "media",
            "hostPath": "/var/mnt/tank/media",
            "shared": false,
            "warningAcknowledged": false
        }])
    );

    let target_schedule = &reexported["storageSchedules"][0];
    assert_eq!(target_schedule["kind"], "snapshot");
    assert_eq!(target_schedule["target"], "tank/media");
    assert_eq!(target_schedule["cadence"], "daily");
    assert_eq!(target_schedule["runAt"], "02:30");
    assert_eq!(target_schedule["keep"], 3);

    let target_buddy = &reexported["buddySchedules"][0];
    assert_eq!(target_buddy["dataset"], "tank/media");
    assert_eq!(
        target_buddy["receivers"],
        json!(["https://buddy.example.test"])
    );

    let target_ddns = &reexported["ddnsEntries"][0];
    assert_eq!(target_ddns["provider"], "cloudflare");
    assert_eq!(target_ddns["record"], "home");
    let target_ddns_id = target_ddns["id"].as_str().unwrap();

    let target_domain = &reexported["domains"][0];
    assert_eq!(target_domain["baseDomain"], "example.test");
    assert_eq!(target_domain["dnsProvider"], "cloudflare");
    assert_eq!(target_domain["appRecordsDdnsEntry"], target_ddns_id);

    let second = build_plan(&target, &document).await.unwrap();
    assert!(second
        .shares
        .iter()
        .all(|entry| matches!(entry.outcome, Outcome::Unchanged)));
    assert!(second
        .storage
        .iter()
        .all(|entry| matches!(entry.outcome, Outcome::Unchanged)));
    assert!(second
        .buddy
        .iter()
        .all(|entry| matches!(entry.outcome, Outcome::Unchanged)));
    assert!(second
        .ddns
        .iter()
        .all(|entry| matches!(entry.outcome, Outcome::Unchanged)));
    assert!(second
        .domains
        .iter()
        .all(|entry| matches!(entry.outcome, Outcome::Unchanged)));
    assert!(second
        .datasets
        .iter()
        .all(|entry| matches!(entry.outcome, Outcome::Unchanged)));
}
