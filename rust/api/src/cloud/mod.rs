//! S3-compatible encrypted ZFS-stream cloud backup (FR-REP-09/11/12/13).
//!
//! The bucket holds only sealed chunks and a domain-separated signed manifest.
//! Everything that decides *what* is sealed is shared with Buddy through
//! [`crate::buddy::transfer`]; this module adds the destination records, the
//! connect-time endpoint policy, the S3 transport, the lineage/retention model,
//! the jobs, and the scheduler.

pub mod jobs;
pub mod net;
pub mod restore;
pub mod s3;
pub mod schedules;
pub mod send;
pub mod store;
pub mod verify;

use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::buddy::envelope::Manifest;
use crate::buddy::keys::Identity;
use crate::schedule::{JsonStore, JsonStoreItem};
use crate::state::AppState;

/// The default state files on the state PVC (chart-rendered env).
pub const DEFAULT_DESTINATIONS: &str = "/var/lib/naslos/cloud-destinations.json";
pub const DEFAULT_SCHEDULES: &str = "/var/lib/naslos/cloud-schedules.json";
pub const DEFAULT_SEQUENCES_DIR: &str = "/var/lib/naslos/cloud-sequences";

/// The largest serialized cloud manifest. A chunk is ~1 MiB and a manifest
/// entry ~160 B, so this is roughly a 1 TiB stream; the send preflight refuses
/// anything larger instead of failing at publish after the upload.
pub const MAX_MANIFEST_BYTES: usize = 128 << 20;
/// The agent's hard cap for one receive body (a single link must fit).
pub const MAX_STREAM_BODY_BYTES: i64 = 1i64 << 40;

/// The cloud manifest signature domain. A Buddy manifest never verifies here
/// and a cloud manifest never verifies in Buddy.
pub const SIGNATURE_DOMAIN: &[u8] = b"NCLOUD1|";

/// The identity file, shared with Buddy (same signing key and KEK).
pub fn identity_path() -> String {
    std::env::var("BUDDY_IDENTITY")
        .unwrap_or_else(|_| "/var/lib/naslos/buddy-identity.json".to_string())
}

/// The key-custody namespace holding the destination credential Secrets.
pub fn keys_namespace() -> String {
    crate::dataset_encryption::keys_namespace()
}

/// The Secret name Naslos writes for a destination it created.
pub fn managed_secret_name(id: &str) -> String {
    format!("naslos-cloud-cred-{id}")
}

/// A stored destination. Never serialized to an API response directly: the
/// credential Secret name is internal and the credential values live only in
/// Kubernetes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Destination {
    pub id: String,
    pub name: String,
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    #[serde(default)]
    pub prefix: String,
    #[serde(default, rename = "pathStyle")]
    pub path_style: bool,
    #[serde(default, rename = "allowInsecureHttp")]
    pub allow_insecure_http: bool,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, rename = "credentialsManaged")]
    pub credentials_managed: bool,
    #[serde(default, rename = "credentialsSecret")]
    pub credentials_secret: String,
    #[serde(default, rename = "conditionalCreate")]
    pub conditional_create: bool,
    #[serde(default, rename = "hasAccessKey")]
    pub has_access_key: bool,
    #[serde(default, rename = "hasSecretKey")]
    pub has_secret_key: bool,
    #[serde(default, rename = "hasSessionToken")]
    pub has_session_token: bool,
    #[serde(default, rename = "createdAt")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default, rename = "lastTestAt")]
    pub last_test_at: Option<DateTime<Utc>>,
    #[serde(default, rename = "lastError")]
    pub last_error: String,
}

impl JsonStoreItem for Destination {
    fn id(&self) -> &str {
        &self.id
    }
}

/// The API response shape (DM-11). It exposes only `has*` credential flags.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DestinationView {
    pub id: String,
    pub name: String,
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub prefix: String,
    pub path_style: bool,
    pub allow_insecure_http: bool,
    pub enabled: bool,
    pub credentials_managed: bool,
    pub has_access_key: bool,
    pub has_secret_key: bool,
    pub has_session_token: bool,
    pub conditional_create: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_test_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub last_error: String,
}

impl From<&Destination> for DestinationView {
    fn from(d: &Destination) -> Self {
        Self {
            id: d.id.clone(),
            name: d.name.clone(),
            endpoint: d.endpoint.clone(),
            region: d.region.clone(),
            bucket: d.bucket.clone(),
            prefix: d.prefix.clone(),
            path_style: d.path_style,
            allow_insecure_http: d.allow_insecure_http,
            enabled: d.enabled,
            credentials_managed: d.credentials_managed,
            has_access_key: d.has_access_key,
            has_secret_key: d.has_secret_key,
            has_session_token: d.has_session_token,
            conditional_create: d.conditional_create,
            created_at: d.created_at,
            last_test_at: d.last_test_at,
            last_error: d.last_error.clone(),
        }
    }
}

/// The API `Chain` shape (docs/api.md): one published link.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainView {
    pub chain: String,
    pub lineage: String,
    pub kind: String,
    pub from_snapshot: String,
    pub to_snapshot: String,
    pub from_guid: String,
    pub to_guid: String,
    pub sequence: u64,
    pub created_at: Option<DateTime<Utc>>,
    pub chunks: usize,
    pub plain_bytes: i64,
    pub raw: bool,
}

/// The create/update body. Blank credential fields mean "keep the stored
/// value"; there is deliberately no clear-token flag in WP-1 (documented).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DestinationInput {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub region: String,
    #[serde(default)]
    pub bucket: String,
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub path_style: bool,
    #[serde(default)]
    pub allow_insecure_http: bool,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub access_key_id: Option<String>,
    #[serde(default)]
    pub secret_access_key: Option<String>,
    #[serde(default)]
    pub session_token: Option<String>,
    #[serde(default)]
    pub credentials_secret: Option<String>,
}

impl DestinationInput {
    fn non_empty(value: Option<&String>) -> Option<String> {
        value
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    }

    pub fn session_token_value(&self) -> Option<String> {
        Self::non_empty(self.session_token.as_ref())
    }

    pub fn access_key(&self) -> Option<String> {
        Self::non_empty(self.access_key_id.as_ref())
    }

    pub fn secret_key(&self) -> Option<String> {
        Self::non_empty(self.secret_access_key.as_ref())
    }

    pub fn credentials_secret_name(&self) -> Option<String> {
        Self::non_empty(self.credentials_secret.as_ref())
    }
}

/// S3 static credentials, read from a Secret. `Debug` is redacted.
#[derive(Clone, Default)]
pub struct Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"******")
            .field(
                "session_token",
                &self.session_token.as_ref().map(|_| "******"),
            )
            .finish()
    }
}

/// A destination store backed by the corrupt-preserving [`JsonStore`].
pub struct DestinationStore {
    inner: JsonStore<Destination>,
}

impl DestinationStore {
    pub fn new(path: &str) -> Self {
        Self {
            inner: JsonStore::new(path),
        }
    }

    pub fn from_env() -> Self {
        Self::new(
            &std::env::var("CLOUD_DESTINATIONS")
                .unwrap_or_else(|_| DEFAULT_DESTINATIONS.to_string()),
        )
    }

    pub fn load(&self) -> Result<(), String> {
        self.inner.load()
    }

    pub fn loaded(&self) -> bool {
        self.inner.loaded()
    }

    pub fn load_error(&self) -> Option<String> {
        self.inner.load_error()
    }

    pub fn require_loaded(&self) -> Result<(), String> {
        self.inner.require_loaded()
    }

    pub fn list(&self) -> Vec<Destination> {
        self.inner.list()
    }

    pub fn get(&self, id: &str) -> Option<Destination> {
        self.inner.get(id)
    }

    pub fn put(&self, destination: Destination) -> Result<(), String> {
        self.inner.put(destination)
    }

    pub fn remove(&self, id: &str) -> Result<(), String> {
        self.inner.remove(id)
    }

    pub fn update<F>(&self, id: &str, mutate: F) -> Result<Destination, String>
    where
        F: FnOnce(&mut Destination) -> Result<(), String>,
    {
        self.inner.update(id, mutate)
    }
}

/// The live cloud subsystem. `None` in `AppState` means it failed to
/// initialise; every route then answers 503.
pub struct Cloud {
    pub destinations: DestinationStore,
    pub schedules: schedules::CloudScheduleStore,
    pub jobs: jobs::CloudJobManager,
    pub sequences_dir: String,
    pub scheduler_interval: std::time::Duration,
}

impl Cloud {
    pub fn from_env() -> Self {
        let destinations = DestinationStore::from_env();
        if let Err(e) = destinations.load() {
            tracing::warn!("could not load the cloud destination store: {e}");
        }
        let schedules = schedules::CloudScheduleStore::from_env();
        if let Err(e) = schedules.load() {
            tracing::warn!("could not load the cloud schedule store: {e}");
        }
        let interval_ms = std::env::var("CLOUD_SCHEDULER_INTERVAL_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|ms| *ms > 0)
            .unwrap_or(60_000);
        Self {
            destinations,
            schedules,
            jobs: jobs::CloudJobManager::new(),
            sequences_dir: std::env::var("CLOUD_SEQUENCES_DIR")
                .unwrap_or_else(|_| DEFAULT_SEQUENCES_DIR.to_string()),
            scheduler_interval: std::time::Duration::from_millis(interval_ms),
        }
    }

    /// In-memory stores for tests.
    pub fn for_test() -> Self {
        Self {
            destinations: DestinationStore::new(""),
            schedules: schedules::CloudScheduleStore::new(""),
            jobs: jobs::CloudJobManager::new(),
            sequences_dir: String::new(),
            scheduler_interval: std::time::Duration::from_secs(60),
        }
    }

    /// The per-destination/source resume and freshness-anchor directory.
    pub fn source_state_dir(&self, destination: &str) -> std::path::PathBuf {
        std::path::Path::new(&self.sequences_dir).join(destination)
    }

    pub fn source_state_paths(
        &self,
        destination: &str,
        source: &str,
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = self.source_state_dir(destination);
        let digest = sha256_hex(source.as_bytes());
        (
            dir.join(format!("{digest}.json")),
            dir.join(format!("{digest}.seq")),
        )
    }
}

/// The destination store, or the 503 message when the subsystem is absent.
pub fn store_available(state: &AppState) -> Result<&Arc<Cloud>, String> {
    state
        .cloud
        .as_ref()
        .ok_or_else(|| "cloud backup is not enabled".to_string())
}

/// Load a destination's credentials from its Secret.
pub async fn load_credentials(
    state: &AppState,
    destination: &Destination,
) -> Result<Credentials, String> {
    if destination.credentials_secret.is_empty() {
        return Err("this destination has no stored credentials".to_string());
    }
    let Some(kube) = state.kube.as_ref() else {
        return Err(
            "the Kubernetes client is unavailable, so the credential Secret cannot be read"
                .to_string(),
        );
    };
    let namespace = keys_namespace();
    let secret = kube
        .get_json("secret", &destination.credentials_secret, &namespace)
        .await?
        .ok_or_else(|| {
            format!(
                "the credential Secret {} is missing",
                destination.credentials_secret
            )
        })?;
    let values = secret_string_data(&secret);
    credentials_from_values(&values)
}

/// Load by an explicit operator-supplied Secret name (destination create/update).
pub async fn load_named_credentials(
    state: &AppState,
    secret_name: &str,
) -> Result<(Credentials, (bool, bool, bool)), String> {
    let Some(kube) = state.kube.as_ref() else {
        return Err(
            "the Kubernetes client is unavailable, so the credential Secret cannot be read"
                .to_string(),
        );
    };
    let namespace = keys_namespace();
    let secret = kube
        .get_json("secret", secret_name, &namespace)
        .await?
        .ok_or_else(|| {
            format!("the credential Secret {secret_name} does not exist in {namespace}")
        })?;
    let values = secret_string_data(&secret);
    let flags = (
        values
            .get("accessKeyId")
            .is_some_and(|v| !v.trim().is_empty()),
        values.get("secretAccessKey").is_some_and(|v| !v.is_empty()),
        values
            .get("sessionToken")
            .is_some_and(|v| !v.trim().is_empty()),
    );
    let creds = credentials_from_values(&values)?;
    Ok((creds, flags))
}

fn credentials_from_values(values: &HashMap<String, String>) -> Result<Credentials, String> {
    let access = values.get("accessKeyId").cloned().unwrap_or_default();
    let secret = values.get("secretAccessKey").cloned().unwrap_or_default();
    if access.trim().is_empty() || secret.is_empty() {
        return Err("the credential Secret must hold accessKeyId and secretAccessKey".to_string());
    }
    Ok(Credentials {
        access_key_id: access.trim().to_string(),
        secret_access_key: secret,
        session_token: values
            .get("sessionToken")
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty()),
    })
}

/// Decode a Kubernetes Secret's `data` object into string values.
pub fn secret_string_data(secret: &serde_json::Value) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if let Some(data) = secret.get("data").and_then(|v| v.as_object()) {
        for (key, value) in data {
            let Some(encoded) = value.as_str() else {
                continue;
            };
            if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(encoded) {
                out.insert(key.clone(), String::from_utf8_lossy(&bytes).into_owned());
            }
        }
    }
    if let Some(data) = secret.get("stringData").and_then(|v| v.as_object()) {
        for (key, value) in data {
            if let Some(text) = value.as_str() {
                out.insert(key.clone(), text.to_string());
            }
        }
    }
    out
}

/// Write the Naslos-managed credential Secret, merging over existing keys.
/// `session_token: None` keeps a stored token (there is no clear flag).
pub async fn write_managed_credentials(
    state: &AppState,
    destination_id: &str,
    access_key_id: &str,
    secret_access_key: &str,
    session_token: Option<&str>,
) -> Result<(), String> {
    let Some(kube) = state.kube.as_ref() else {
        return Err(
            "the Kubernetes client is unavailable, so credentials cannot be stored".to_string(),
        );
    };
    let namespace = keys_namespace();
    let name = managed_secret_name(destination_id);
    let existing = kube.get_json("secret", &name, &namespace).await?;
    let mut data: HashMap<String, String> = existing
        .as_ref()
        .map(secret_string_data)
        .unwrap_or_default();
    data.insert("accessKeyId".to_string(), access_key_id.to_string());
    data.insert("secretAccessKey".to_string(), secret_access_key.to_string());
    if let Some(token) = session_token.map(str::trim).filter(|t| !t.is_empty()) {
        data.insert("sessionToken".to_string(), token.to_string());
    }
    let object = serde_json::json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": name,
            "namespace": namespace,
            "labels": {
                "app.kubernetes.io/managed-by": "naslos-api",
                "naslos.io/cloud-destination": destination_id,
            },
        },
        "type": "Opaque",
        "stringData": data,
    });
    kube.apply(&object)
        .await
        .map_err(|e| format!("storing credentials: {e}"))
}

/// Delete a Naslos-created Secret, but only when it still carries the
/// managed-by label. An operator-managed Secret is never deleted.
pub async fn delete_managed_credentials(state: &AppState, destination: &Destination) {
    if !destination.credentials_managed || destination.credentials_secret.is_empty() {
        return;
    }
    let Some(kube) = state.kube.as_ref() else {
        return;
    };
    let namespace = keys_namespace();
    match kube
        .get_json("secret", &destination.credentials_secret, &namespace)
        .await
    {
        Ok(Some(secret)) => {
            let managed = secret
                .get("metadata")
                .and_then(|m| m.get("labels"))
                .and_then(|l| l.get("app.kubernetes.io/managed-by"))
                .and_then(|v| v.as_str())
                == Some("naslos-api");
            if !managed {
                tracing::warn!(
                    "not deleting cloud credential Secret {}: it is not labelled managed-by=naslos-api",
                    destination.credentials_secret
                );
                return;
            }
        }
        Ok(None) => return,
        Err(e) => {
            tracing::warn!(
                "not deleting cloud credential Secret {}: it could not be read: {e}",
                destination.credentials_secret
            );
            return;
        }
    }
    if let Err(e) = kube
        .delete("secret", &destination.credentials_secret, &namespace)
        .await
    {
        tracing::warn!(
            "could not delete cloud credential Secret {}: {e}",
            destination.credentials_secret
        );
    }
}

/// The credential flags of a Secret name, for responses.
pub async fn credential_flags(state: &AppState, destination: &Destination) -> (bool, bool, bool) {
    let fallback = (
        destination.has_access_key,
        destination.has_secret_key,
        destination.has_session_token,
    );
    if destination.credentials_secret.is_empty() {
        return fallback;
    }
    load_named_credentials(state, &destination.credentials_secret)
        .await
        .map(|(_, flags)| flags)
        .unwrap_or(fallback)
}

/// Validate and normalize the editable fields of a destination.
pub fn normalize_destination(mut destination: Destination) -> Result<Destination, String> {
    destination.name = destination.name.trim().to_string();
    destination.endpoint = destination.endpoint.trim().to_string();
    destination.region = destination.region.trim().to_string();
    destination.bucket = destination.bucket.trim().to_string();
    destination.prefix = normalize_prefix(&destination.prefix)?;
    validate_destination(&destination)?;
    Ok(destination)
}

fn validate_destination(destination: &Destination) -> Result<(), String> {
    if destination.name.is_empty() || destination.name.len() > 64 {
        return Err("name must be 1-64 characters".to_string());
    }
    if destination.name.chars().any(char::is_control) {
        return Err("name must not contain control characters".to_string());
    }
    net::parse_endpoint(&destination.endpoint, destination.allow_insecure_http)?;
    if destination.region.is_empty() || destination.region.len() > 64 {
        return Err("region must be 1-64 characters (use \"auto\" for Cloudflare R2)".to_string());
    }
    if !destination
        .region
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err("region may only contain letters, digits, '-', '_' and '.'".to_string());
    }
    validate_bucket(&destination.bucket)?;
    Ok(())
}

/// A conservative S3 bucket name check (path-style and virtual-host-safe).
pub fn validate_bucket(bucket: &str) -> Result<(), String> {
    if !(3..=63).contains(&bucket.len()) {
        return Err("bucket must be 3-63 characters".to_string());
    }
    if !bucket
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '.'))
    {
        return Err("bucket may only contain lowercase letters, digits, '-' and '.'".to_string());
    }
    if !bucket
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric())
        || !bucket
            .chars()
            .last()
            .is_some_and(|c| c.is_ascii_alphanumeric())
    {
        return Err("bucket must start and end with a letter or digit".to_string());
    }
    if bucket.contains("..") || bucket.parse::<std::net::IpAddr>().is_ok() {
        return Err("bucket must not look like an IP address or contain '..'".to_string());
    }
    Ok(())
}

/// Trim slashes and reject traversal in the object-key prefix.
pub fn normalize_prefix(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim().trim_matches('/');
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    if trimmed.len() > 200 {
        return Err("prefix must be 200 characters or fewer".to_string());
    }
    for segment in trimmed.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err("prefix must not contain empty, '.' or '..' segments".to_string());
        }
        if !segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return Err(
                "prefix segments may only contain letters, digits, '-', '_' and '.'".to_string(),
            );
        }
    }
    Ok(trimmed.to_string())
}

/// The logical source for a dataset. It is the dataset name when that already
/// passes the source grammar (the normal case); a mixed-case or otherwise
/// unusual name is mapped to a stable hashed alias so the key stays safe and
/// the UI can still ask for it by dataset name.
pub fn source_for_dataset(dataset: &str) -> Result<String, String> {
    crate::server::zfs::validate_dataset_path(dataset)?;
    if crate::buddy::peers::validate_source(dataset).is_ok() {
        return Ok(dataset.to_string());
    }
    let digest = sha256_hex(dataset.as_bytes());
    Ok(format!("cloud-h{}", &digest[..40]))
}

/// `naslos-cloud-<pair token>-` snapshot prefix (already in the agent's owned
/// `naslos-` prefix, and per destination so retention cannot cross receivers).
pub fn snapshot_prefix(source: &str, destination_id: &str) -> String {
    format!(
        "naslos-cloud-{}-",
        crate::server::replication::replication_token(source, destination_id)
    )
}

/// `naslos-cloud-<pair token>-<UTC timestamp>`.
pub fn snapshot_name(source: &str, destination_id: &str, now: DateTime<Utc>) -> String {
    format!(
        "{}{}",
        snapshot_prefix(source, destination_id),
        now.format("%Y%m%dT%H%M%SZ")
    )
}

/// The key of one link directory:
/// `<root>/<source>/lineages/<lineage>/links/<sequence>-<chain>`.
pub fn link_prefix(root: &str, source: &str, lineage: &str, sequence: u64, chain: &str) -> String {
    let base = if root.is_empty() {
        source.to_string()
    } else {
        format!("{root}/{source}")
    };
    format!("{base}/lineages/{lineage}/links/{sequence}-{chain}")
}

/// `<link>/chunks/<index>-g<generation>.nbc`.
pub fn chunk_key(link: &str, index: i64, generation: u16) -> String {
    format!("{link}/chunks/{index}-g{generation}.nbc")
}

/// `<link>/manifest.json`.
pub fn manifest_key(link: &str) -> String {
    format!("{link}/manifest.json")
}

/// One parsed object key under a source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedLinkKey {
    pub lineage: String,
    pub sequence: u64,
    pub chain: String,
    pub kind: ParsedKeyKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedKeyKind {
    Manifest,
    Chunk { index: i64, generation: u16 },
    Other,
}

/// Parse `<source_root>/lineages/<lineage>/links/<seq>-<chain>/…`.
pub fn parse_link_key(source_root: &str, key: &str) -> Option<ParsedLinkKey> {
    let rest = key.strip_prefix(source_root)?.strip_prefix('/')?;
    let rest = rest.strip_prefix("lineages/")?;
    let (lineage, rest) = rest.split_once("/links/")?;
    let (seq_chain, rest) = rest.split_once('/')?;
    let (sequence, chain) = seq_chain.split_once('-')?;
    let sequence: u64 = sequence.parse().ok()?;
    if lineage.is_empty() || chain.is_empty() {
        return None;
    }
    let kind = if rest == "manifest.json" {
        ParsedKeyKind::Manifest
    } else if let Some(name) = rest.strip_prefix("chunks/") {
        let name = name.strip_suffix(".nbc")?;
        let (index, generation) = name.split_once("-g")?;
        ParsedKeyKind::Chunk {
            index: index.parse().ok()?,
            generation: generation.parse().ok()?,
        }
    } else {
        ParsedKeyKind::Other
    };
    Some(ParsedLinkKey {
        lineage: lineage.to_string(),
        sequence,
        chain: chain.to_string(),
        kind,
    })
}

/// Sign a cloud manifest with the `NCLOUD1|` domain prefix. The manifest is
/// not bound to a destination id on purpose: re-adding a destination on a
/// fresh NAS changes the id and every chain must still verify.
pub fn sign_manifest(manifest: &mut Manifest, identity: &Identity) -> Result<(), String> {
    manifest.key_id = identity.fingerprint()?;
    let canonical = manifest.canonical()?;
    let mut message = SIGNATURE_DOMAIN.to_vec();
    message.extend_from_slice(&canonical);
    let signature = identity.sign(&message)?;
    manifest.signature = base64::engine::general_purpose::STANDARD.encode(signature);
    Ok(())
}

/// Verify a cloud manifest. There is no legacy fallback and no
/// `skip_signature_check`: a cloud manifest is always domain-separated.
pub fn verify_manifest(manifest: &Manifest, identity: &Identity) -> Result<(), String> {
    if manifest.signature.is_empty() {
        return Err("manifest is not signed".to_string());
    }
    let (_, fingerprint) = crate::buddy::keys::public_key_of(&identity.public_key)?;
    if manifest.key_id != fingerprint {
        return Err(format!(
            "manifest was signed by {fingerprint} but claims {}",
            manifest.key_id
        ));
    }
    let signature = base64::engine::general_purpose::STANDARD
        .decode(&manifest.signature)
        .map_err(|e| format!("decoding manifest signature: {e}"))?;
    let canonical = manifest.canonical()?;
    let mut message = SIGNATURE_DOMAIN.to_vec();
    message.extend_from_slice(&canonical);
    crate::buddy::keys::verify(&identity.public_key, &message, &signature)
        .map_err(|_| "the manifest was not signed by this NAS (or it was modified)".to_string())
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn destination() -> Destination {
        Destination {
            id: "abc".into(),
            name: "b2".into(),
            endpoint: "https://s3.us-west-004.backblazeb2.com".into(),
            region: "us-west-004".into(),
            bucket: "naslos-backups".into(),
            prefix: "host1".into(),
            enabled: true,
            ..Default::default()
        }
    }

    #[test]
    fn the_key_layout_is_the_documented_lineage_link_path() {
        let link = link_prefix("host1", "tank/data", "lin1", 7, "abcd");
        assert_eq!(link, "host1/tank/data/lineages/lin1/links/7-abcd");
        assert_eq!(
            chunk_key(&link, 3, 2),
            "host1/tank/data/lineages/lin1/links/7-abcd/chunks/3-g2.nbc"
        );
        assert_eq!(
            manifest_key(&link),
            "host1/tank/data/lineages/lin1/links/7-abcd/manifest.json"
        );
        let parsed = parse_link_key("host1/tank/data", &chunk_key(&link, 3, 2)).unwrap();
        assert_eq!(parsed.lineage, "lin1");
        assert_eq!(parsed.sequence, 7);
        assert_eq!(parsed.chain, "abcd");
        assert_eq!(
            parsed.kind,
            ParsedKeyKind::Chunk {
                index: 3,
                generation: 2
            }
        );
        assert_eq!(
            parse_link_key("host1/tank/data", &manifest_key(&link))
                .unwrap()
                .kind,
            ParsedKeyKind::Manifest
        );
    }

    #[test]
    fn the_snapshot_name_uses_the_per_destination_token() {
        let now = DateTime::parse_from_rfc3339("2026-01-05T10:15:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let name = snapshot_name("tank/data", "dest-1", now);
        assert!(name.starts_with("naslos-cloud-"), "{name}");
        assert!(name.ends_with("-20260105T101500Z"), "{name}");
        assert_ne!(name, snapshot_name("tank/data", "dest-2", now));
    }

    #[test]
    fn destination_validation_refuses_unsafe_shapes() {
        let ok = normalize_destination(destination()).unwrap();
        assert_eq!(ok.prefix, "host1");
        let mut bad = destination();
        bad.bucket = "UPPER".into();
        assert!(normalize_destination(bad).is_err());
        let mut bad = destination();
        bad.prefix = "a/../b".into();
        assert!(normalize_destination(bad).is_err());
        let mut bad = destination();
        bad.endpoint = "http://8.8.8.8".into();
        bad.allow_insecure_http = true;
        assert!(normalize_destination(bad).is_err());
        let mut bad = destination();
        bad.endpoint = "https://user:pw@example.test".into();
        assert!(normalize_destination(bad).is_err());
    }

    #[test]
    fn prefix_normalization_trims_but_keeps_segments() {
        assert_eq!(
            normalize_prefix("/backups/host1/").unwrap(),
            "backups/host1"
        );
        assert_eq!(normalize_prefix("").unwrap(), "");
        assert!(normalize_prefix("a//b").is_err());
    }

    #[test]
    fn the_response_view_never_carries_a_secret_or_its_name() {
        let mut d = destination();
        d.credentials_secret = "naslos-cloud-cred-abc".into();
        d.has_access_key = true;
        d.has_secret_key = true;
        let value = serde_json::to_value(DestinationView::from(&d)).unwrap();
        let text = value.to_string();
        assert!(value["hasAccessKey"].as_bool().unwrap());
        assert!(value["hasSecretKey"].as_bool().unwrap());
        assert!(value["hasSessionToken"].as_bool().is_some());
        assert!(!text.contains("credentialsSecret"));
        assert!(!text.contains("naslos-cloud-cred"));
        assert!(!text.contains("accessKeyId"));
    }

    #[test]
    fn a_dataset_name_is_the_source_unless_it_needs_a_safe_alias() {
        assert_eq!(source_for_dataset("tank/data").unwrap(), "tank/data");
        let alias = source_for_dataset("tank/Data").unwrap();
        assert!(alias.starts_with("cloud-h"), "{alias}");
        assert_eq!(alias, source_for_dataset("tank/Data").unwrap());
        assert_ne!(alias, source_for_dataset("tank/data").unwrap());
        assert!(source_for_dataset("not-a-dataset").is_err());
    }

    #[test]
    fn cloud_manifests_are_domain_separated_from_buddy() {
        let identity = crate::buddy::keys::new_identity("cloud-test").unwrap();
        let mut manifest = Manifest {
            version: crate::buddy::envelope::ENVELOPE_VERSION,
            source: "tank/data".into(),
            chain: "abcd".into(),
            kind: "zfs-send-raw".into(),
            from_snapshot: String::new(),
            to_snapshot: "naslos-cloud-x-1".into(),
            from_guid: String::new(),
            to_guid: "g1".into(),
            created_at: Utc::now(),
            key_id: String::new(),
            sequence: 1,
            generation: 1,
            stream_prefix: "AAAAAA==".into(),
            chunk_plain_size: crate::buddy::envelope::CHUNK_PLAIN_SIZE as i64,
            chunks: Vec::new(),
            dek_wrapped: "AAAA".into(),
            signature: String::new(),
        };
        sign_manifest(&mut manifest, &identity).unwrap();
        verify_manifest(&manifest, &identity).unwrap();
        // The Buddy verifier (no domain) must refuse it.
        assert!(manifest.verify_signature(&identity.public_key).is_err());
        let mut tampered = manifest.clone();
        tampered.sequence = 2;
        assert!(verify_manifest(&tampered, &identity).is_err());
    }

    #[test]
    fn the_secret_data_reader_decodes_base64_and_string_data() {
        let secret = serde_json::json!({
            "data": {
                "accessKeyId": base64::engine::general_purpose::STANDARD.encode("AKIA"),
                "secretAccessKey": base64::engine::general_purpose::STANDARD.encode("shh"),
            }
        });
        let values = secret_string_data(&secret);
        let creds = credentials_from_values(&values).unwrap();
        assert_eq!(creds.access_key_id, "AKIA");
        assert_eq!(creds.secret_access_key, "shh");
        assert!(format!("{creds:?}").contains("******"));
        assert!(!format!("{creds:?}").contains("shh"));
    }

    #[test]
    fn bug_an_empty_key_is_refused_before_building_the_client() {
        let destination = destination();
        let creds = Credentials::default();
        let err = match s3::S3Storage::build(&destination, &creds, &net::NetConfig::default()) {
            Ok(_) => panic!("empty credentials must be refused"),
            Err(error) => error,
        };
        assert_eq!(err.category, s3::Category::Auth);
    }
}
