//! Buddy receiver HTTP endpoints (port of `api/internal/buddy/http.go`).
//!
//! Peers authenticate with their own Ed25519 key; these routes are NOT behind
//! the owner/proxy auth (they are mounted on the public router).

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::buddy::auth::{body_digest, Authenticator, RequestAuth};
use crate::buddy::envelope::{validate_manifest_shape, Manifest, ENVELOPE_VERSION};
use crate::buddy::keys::public_key_of;
use crate::buddy::peers::{validate_source, PeerStore};
use crate::buddy::store::{Store, StoreError};
use crate::json;
use crate::state::AppState;

const MAX_MANIFEST_BYTES: usize = 16 << 20;
const DEFAULT_ENROLL_QUOTA_BYTES: i64 = 1i64 << 40;

/// The quota an enrolling peer gets: its request, capped by the receiver's
/// configured default (0/negative means "use the built-in 1 TiB default").
fn enrollment_quota(requested: i64, configured_default: i64) -> i64 {
    let ceiling = if configured_default > 0 {
        configured_default
    } else {
        DEFAULT_ENROLL_QUOTA_BYTES
    };
    if requested <= 0 {
        ceiling
    } else {
        requested.min(ceiling)
    }
}

/// The receiver's configuration.
pub struct Receiver {
    pub store: Arc<Store>,
    pub peers: Arc<PeerStore>,
    pub auth: Arc<Authenticator>,
    pub name: String,
    /// Audience that signed peer requests must name. It was the operator-chosen
    /// `name`, which is not unique, so two receivers called "naslos" accepted
    /// each other's replayed requests (R-017). A per-instance random id makes
    /// it unique; it is published by the read-only `/status` discovery call.
    pub audience: String,
    pub version: String,
    pub enroll_token: String,
    pub enroll_open: bool,
    pub default_quota_bytes: i64,
    enroll_used: Mutex<bool>,
}

impl Receiver {
    pub fn new(
        store: Arc<Store>,
        peers: Arc<PeerStore>,
        auth: Arc<Authenticator>,
        name: &str,
    ) -> Self {
        Self {
            store,
            peers,
            auth,
            name: name.to_string(),
            audience: instance_audience(name),
            version: std::env::var("BUDDY_VERSION").unwrap_or_else(|_| "0.1.0".to_string()),
            enroll_token: String::new(),
            enroll_open: false,
            default_quota_bytes: 0,
            enroll_used: Mutex::new(false),
        }
    }
}

/// Build the signing audience for this receiver instance. The receiver has no
/// mandatory keypair of its own, so use a per-process random id rather than the
/// non-unique display name (R-017). The name is kept as a readable prefix.
fn instance_audience(name: &str) -> String {
    let name = match name.trim() {
        "" => "receiver",
        name => name,
    };
    let mut id = [0u8; 16];
    if getrandom::fill(&mut id).is_err() {
        return format!(
            "naslos-buddy:{name}:{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        );
    }
    let id: String = id.iter().map(|b| format!("{b:02x}")).collect();
    format!("naslos-buddy:{name}:{id}")
}

fn bad_request(msg: &str) -> Response {
    json(StatusCode::BAD_REQUEST, serde_json::json!({ "error": msg }))
}

fn store_status(err: &StoreError) -> StatusCode {
    match err {
        StoreError::Quota { .. } => StatusCode::PAYLOAD_TOO_LARGE,
        StoreError::NoSpace => StatusCode::INSUFFICIENT_STORAGE,
        StoreError::Other(_) => StatusCode::CONFLICT,
    }
}

fn auth_from_headers(headers: &HeaderMap) -> RequestAuth {
    let get = |k: &str| {
        headers
            .get(k)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    };
    RequestAuth {
        key_id: get(crate::buddy::auth::HEADER_KEY_ID),
        timestamp: get(crate::buddy::auth::HEADER_TIMESTAMP),
        nonce: get(crate::buddy::auth::HEADER_NONCE),
        signature: get(crate::buddy::auth::HEADER_SIGNATURE),
    }
}

/// Authenticate a request against the receiver's own identity, returning the
/// peer (or a ready-to-send 401).
#[allow(clippy::result_large_err)]
fn authenticate(
    receiver: &Receiver,
    headers: &HeaderMap,
    body_digest: &str,
    method: &str,
    uri: &str,
) -> Result<crate::buddy::Peer, Response> {
    let auth = auth_from_headers(headers);
    receiver
        .auth
        .verify(
            &auth,
            body_digest,
            method,
            uri,
            &receiver.audience,
            chrono::Utc::now(),
        )
        .map_err(|e| json(StatusCode::UNAUTHORIZED, serde_json::json!({ "error": e })))
}

/// Authenticate and check the key may write `source`.
#[allow(clippy::result_large_err)]
fn peer_for(
    receiver: &Receiver,
    headers: &HeaderMap,
    body_digest: &str,
    method: &str,
    uri: &str,
    source: &str,
) -> Result<crate::buddy::Peer, Response> {
    let peer = authenticate(receiver, headers, body_digest, method, uri)?;
    if !peer.allows(source) {
        return Err(json(
            StatusCode::FORBIDDEN,
            serde_json::json!({ "error": format!("key {:?} is not allowed to write {source}", peer.name) }),
        ));
    }
    receiver.peers.touch_seen(&peer.name, chrono::Utc::now());
    Ok(peer)
}

#[allow(clippy::result_large_err)]
async fn read_limited(body: Body, limit: usize) -> Result<bytes::Bytes, Response> {
    match axum::body::to_bytes(body, limit).await {
        Ok(b) => Ok(b),
        Err(_) => Err(json(
            StatusCode::PAYLOAD_TOO_LARGE,
            serde_json::json!({ "error": format!("request body exceeds {limit} bytes") }),
        )),
    }
}

fn declared_body_len(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse().ok())
}

fn uri_path(uri: &axum::http::Uri) -> String {
    match uri.path_and_query() {
        Some(pq) => pq.as_str().to_string(),
        None => uri.path().to_string(),
    }
}

/// `GET /api/buddy/v1/status`
pub async fn status(
    State(receiver): State<BuddyState>,
    headers: HeaderMap,
    uri: axum::http::Uri,
) -> Response {
    let digest = body_digest(b"");
    let peer = match authenticate(&receiver, &headers, &digest, "GET", &uri_path(&uri)) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    receiver.peers.touch_seen(&peer.name, chrono::Utc::now());
    let free = match receiver.store.free_space() {
        Ok(v) => v,
        Err(e) => {
            return json(
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({ "error": format!("cannot read free space: {e}") }),
            )
        }
    };
    let used = match receiver.store.usage(&peer.fingerprint) {
        Ok(v) => v,
        Err(e) => {
            return json(
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({ "error": format!("cannot read usage: {e}") }),
            )
        }
    };
    let last = receiver.store.last_backup(&peer.fingerprint).ok().flatten();
    let sources = receiver
        .store
        .sources(&peer.fingerprint)
        .unwrap_or_default();
    let mut chains = serde_json::Map::new();
    for source in &sources {
        if let Ok(m) = receiver.store.manifest(&peer.fingerprint, source) {
            chains.insert(source.clone(), serde_json::Value::String(m.chain));
        }
    }
    let peer_names: Vec<String> = receiver.peers.list().into_iter().map(|p| p.name).collect();
    json(
        StatusCode::OK,
        serde_json::json!({
            "version": receiver.version,
            "receiver": receiver.name,
            "audience": receiver.audience,
            "keyId": peer.fingerprint,
            "freeBytes": free,
            "usedBytes": used,
            "quotaBytes": peer.quota_bytes,
            "lastBackup": last,
            "sources": sources,
            "chains": chains,
            "enrollOpen": !receiver.enroll_token.is_empty(),
            "peers": peer_names,
            "serverTime": chrono::Utc::now(),
            "protocolVersion": ENVELOPE_VERSION,
        }),
    )
}

/// `POST /api/buddy/v1/enroll`
pub async fn enroll(State(receiver): State<BuddyState>, body: Body) -> Response {
    if receiver.enroll_token.is_empty() {
        return json(
            StatusCode::FORBIDDEN,
            serde_json::json!({ "error": "enrollment is disabled on this receiver: authorize the key on the receiver instead" }),
        );
    }
    let body = match read_limited(body, 64 << 10).await {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    #[derive(Default, serde::Deserialize)]
    struct EnrollRequest {
        #[serde(default)]
        token: String,
        #[serde(default)]
        name: String,
        #[serde(default, rename = "publicKey")]
        public_key: String,
        #[serde(default)]
        sources: Vec<String>,
        #[serde(default, rename = "quotaBytes")]
        quota_bytes: i64,
    }
    let req: EnrollRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return bad_request(&format!("invalid enrollment request: {e}")),
    };
    use subtle::ConstantTimeEq;
    if req
        .token
        .as_bytes()
        .ct_eq(receiver.enroll_token.as_bytes())
        .unwrap_u8()
        != 1
    {
        return json(
            StatusCode::UNAUTHORIZED,
            serde_json::json!({ "error": "invalid enrollment token" }),
        );
    }
    if req.name.trim().is_empty() || req.public_key.trim().is_empty() {
        return bad_request("name and publicKey are required");
    }
    if let Err(e) = public_key_of(&req.public_key) {
        return bad_request(&e);
    }
    for source in &req.sources {
        if let Err(e) = validate_source(source) {
            return bad_request(&e);
        }
    }

    let mut used = receiver.enroll_used.lock().unwrap();
    if (*used || receiver.store.enrollment_used()) && !receiver.enroll_open {
        return json(
            StatusCode::FORBIDDEN,
            serde_json::json!({ "error": "this enrollment token has already been used" }),
        );
    }
    // A peer may not grant itself more than the receiver's configured default
    // (PF-M10 / SEC-2026-C9).
    let quota = enrollment_quota(req.quota_bytes, receiver.default_quota_bytes);
    let actor = match public_key_of(&req.public_key) {
        Ok((_, fingerprint)) => fingerprint,
        Err(e) => return bad_request(&e),
    };
    let pending = match receiver.begin_mutation(
        &actor,
        "buddy.enroll",
        Some(req.name.clone()),
        serde_json::json!({ "quotaBytes": quota, "sources": req.sources }),
    ) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let peer = crate::buddy::Peer {
        name: req.name.clone(),
        public_key: req.public_key.clone(),
        allowed_sources: req.sources.clone(),
        quota_bytes: quota,
        enabled: true,
        ..Default::default()
    };
    if !receiver.enroll_open {
        // Record single use before the key is authorized. If the marker cannot
        // be written the enrollment fails closed (SEC-2026-C21).
        if let Err(e) = receiver.store.mark_enrollment_used() {
            let _ = receiver.finish_mutation(pending, StatusCode::INTERNAL_SERVER_ERROR);
            return json(
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({ "error": format!("cannot record the enrollment token as used: {e}") }),
            );
        }
        *used = true;
    }
    match receiver.peers.add(peer) {
        Ok(()) => {
            if let Err(resp) = receiver.finish_mutation(pending, StatusCode::CREATED) {
                return resp;
            }
            let added = receiver.peers.get(&req.name).unwrap_or_default();
            json(
                StatusCode::CREATED,
                serde_json::json!({ "status": "enrolled", "name": added.name, "fingerprint": added.fingerprint }),
            )
        }
        Err(e) => {
            let _ = receiver.finish_mutation(pending, StatusCode::BAD_REQUEST);
            bad_request(&e)
        }
    }
}

/// `GET /api/buddy/v1/backups`
pub async fn backups(
    State(receiver): State<BuddyState>,
    headers: HeaderMap,
    uri: axum::http::Uri,
) -> Response {
    let digest = body_digest(b"");
    let peer = match authenticate(&receiver, &headers, &digest, "GET", &uri_path(&uri)) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    receiver.peers.touch_seen(&peer.name, chrono::Utc::now());
    match receiver.store.summary(&peer.fingerprint) {
        Ok(backups) => json(StatusCode::OK, serde_json::json!({ "backups": backups })),
        Err(e) => json(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "error": e.to_string() }),
        ),
    }
}

/// `GET|PUT /api/buddy/v1/chunks/{source}`
pub async fn chunks(
    State(receiver): State<BuddyState>,
    Path(source): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    method: axum::http::Method,
    uri: axum::http::Uri,
    body: Body,
) -> Response {
    let chain = query.get("chain").cloned().unwrap_or_default();
    let index_param = query.get("index").cloned().unwrap_or_default();
    if method == axum::http::Method::GET {
        let digest = body_digest(b"");
        let peer = match peer_for(
            &receiver,
            &headers,
            &digest,
            "GET",
            &uri_path(&uri),
            &source,
        ) {
            Ok(p) => p,
            Err(resp) => return resp,
        };
        if chain.is_empty() {
            return bad_request("chain is required");
        }
        if !index_param.is_empty() {
            let index: i64 = match index_param.parse() {
                Ok(i) if i >= 0 => i,
                _ => return bad_request(&format!("invalid chunk index {index_param:?}")),
            };
            return match receiver
                .store
                .chunk(&peer.fingerprint, &source, &chain, index)
            {
                Ok(sealed) => (
                    StatusCode::OK,
                    [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
                    sealed,
                )
                    .into_response(),
                Err(e) => json(
                    StatusCode::NOT_FOUND,
                    serde_json::json!({ "error": e.to_string() }),
                ),
            };
        }
        let stored = match receiver
            .store
            .chunk_digests(&peer.fingerprint, &source, &chain)
        {
            Ok(s) => s,
            Err(e) => {
                return json(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    serde_json::json!({ "error": e.to_string() }),
                )
            }
        };
        let published = receiver
            .store
            .is_chain_published(&peer.fingerprint, &source, &chain)
            .unwrap_or(false);
        let mut list: Vec<(i64, String, u16)> = stored
            .into_iter()
            .map(|(i, c)| (i, c.digest, c.generation))
            .collect();
        list.sort_by_key(|(i, _, _)| *i);
        let chunks: Vec<serde_json::Value> = list
            .into_iter()
            .map(|(i, d, g)| serde_json::json!({ "index": i, "digest": d, "generation": g }))
            .collect();
        return json(
            StatusCode::OK,
            serde_json::json!({ "source": source, "chain": chain, "published": published, "chunks": chunks }),
        );
    }

    // PUT
    let body = match read_limited(body, crate::buddy::envelope::MAX_SEALED_CHUNK_SIZE).await {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let digest = body_digest(&body);
    let peer = match peer_for(
        &receiver,
        &headers,
        &digest,
        "PUT",
        &uri_path(&uri),
        &source,
    ) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let index: i64 = match index_param.parse() {
        Ok(i) if i >= 0 => i,
        _ => return bad_request(&format!("invalid chunk index {index_param:?}")),
    };
    if chain.is_empty() {
        return bad_request("chain is required");
    }
    let pending = match receiver.pending_mutation(
        &peer.fingerprint,
        "buddy.chunk.put",
        Some(format!("{source}@{chain}#{index}")),
        serde_json::json!({ "index": index, "sealedBytes": body.len() }),
    ) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    match receiver.store.put_chunk_within(
        &peer.fingerprint,
        &source,
        &chain,
        index,
        &body,
        peer.quota_bytes,
    ) {
        Ok(()) => {
            if let Err(resp) = receiver.record_mutation(pending, StatusCode::CREATED) {
                return resp;
            }
            json(
                StatusCode::CREATED,
                serde_json::json!({ "source": source, "chain": chain, "index": index, "storedBytes": body.len() }),
            )
        }
        Err(e) => {
            let status = store_status(&e);
            let _ = receiver.record_mutation(pending, status);
            let msg = match &e {
                StoreError::Quota { .. } | StoreError::NoSpace => e.to_string(),
                StoreError::Other(m) => m.clone(),
            };
            json(status, serde_json::json!({ "error": msg }))
        }
    }
}

use axum::response::IntoResponse;

fn chain_summaries(manifests: &[Manifest]) -> Vec<serde_json::Value> {
    manifests
        .iter()
        .map(|m| {
            serde_json::json!({
                "chain": m.chain, "kind": m.kind, "createdAt": m.created_at,
                "fromSnapshot": m.from_snapshot, "toSnapshot": m.to_snapshot,
                "fromGUID": m.from_guid, "toGUID": m.to_guid, "chunks": m.chunks.len(),
                "sequence": m.sequence,
            })
        })
        .collect()
}

/// `GET /api/buddy/v1/chains/{source}`
pub async fn chains(
    State(receiver): State<BuddyState>,
    Path(source): Path<String>,
    headers: HeaderMap,
    uri: axum::http::Uri,
) -> Response {
    let digest = body_digest(b"");
    let peer = match peer_for(
        &receiver,
        &headers,
        &digest,
        "GET",
        &uri_path(&uri),
        &source,
    ) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let manifests = match receiver
        .store
        .chains_limited(&peer.fingerprint, &source, 500)
    {
        Ok(m) => m,
        Err(e) => {
            return json(
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({ "error": e.to_string() }),
            )
        }
    };
    let total = receiver
        .store
        .chain_count(&peer.fingerprint, &source)
        .unwrap_or(0);
    json(
        StatusCode::OK,
        serde_json::json!({
            "source": source, "chains": chain_summaries(&manifests),
            "total": total, "truncated": total > manifests.len(),
        }),
    )
}

/// `GET /api/buddy/v1/sequence/{source}`
pub async fn sequence(
    State(receiver): State<BuddyState>,
    Path(source): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    uri: axum::http::Uri,
) -> Response {
    let digest = body_digest(b"");
    let peer = match peer_for(
        &receiver,
        &headers,
        &digest,
        "GET",
        &uri_path(&uri),
        &source,
    ) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let chain = query.get("chain").cloned().unwrap_or_default();
    match receiver.store.sequence(&peer.fingerprint, &source, &chain) {
        Ok(m) => json(
            StatusCode::OK,
            serde_json::json!({ "source": source, "chains": chain_summaries(&m) }),
        ),
        Err(e) => json(
            StatusCode::NOT_FOUND,
            serde_json::json!({ "error": e.to_string() }),
        ),
    }
}

/// `GET|PUT /api/buddy/v1/manifest/{source}`
pub async fn manifest(
    State(receiver): State<BuddyState>,
    Path(source): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    method: axum::http::Method,
    uri: axum::http::Uri,
    body: Body,
) -> Response {
    if let Err(e) = validate_source(&source) {
        return bad_request(&e);
    }
    let chain = query.get("chain").cloned().unwrap_or_default();
    if method == axum::http::Method::GET {
        let digest = body_digest(b"");
        let peer = match peer_for(
            &receiver,
            &headers,
            &digest,
            "GET",
            &uri_path(&uri),
            &source,
        ) {
            Ok(p) => p,
            Err(resp) => return resp,
        };
        let current = match receiver.store.manifest(&peer.fingerprint, &source) {
            Ok(m) => m,
            Err(e) => {
                return json(
                    StatusCode::NOT_FOUND,
                    serde_json::json!({ "error": e.to_string() }),
                )
            }
        };
        if !chain.is_empty() && chain != current.chain {
            return match receiver
                .store
                .manifest_for_chain(&peer.fingerprint, &source, &chain)
            {
                Ok(m) => json(StatusCode::OK, serde_json::to_value(m).unwrap()),
                Err(_) => json(
                    StatusCode::NOT_FOUND,
                    serde_json::json!({ "error": format!("chain {chain} is not stored for {source} (current: {})", current.chain) }),
                ),
            };
        }
        return json(StatusCode::OK, serde_json::to_value(current).unwrap());
    }

    // PUT
    if declared_body_len(&headers).is_some_and(|len| len > MAX_MANIFEST_BYTES as u64) {
        return json(
            StatusCode::PAYLOAD_TOO_LARGE,
            serde_json::json!({ "error": format!("request body exceeds {MAX_MANIFEST_BYTES} bytes") }),
        );
    }
    let body = match read_limited(body, MAX_MANIFEST_BYTES).await {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let digest = body_digest(&body);
    let peer = match peer_for(
        &receiver,
        &headers,
        &digest,
        "PUT",
        &uri_path(&uri),
        &source,
    ) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let manifest: Manifest = match serde_json::from_slice(&body) {
        Ok(m) => m,
        Err(e) => return bad_request(&format!("invalid manifest: {e}")),
    };
    if manifest.version != ENVELOPE_VERSION {
        return bad_request(&format!(
            "protocol version mismatch: the manifest is v{}, this receiver speaks v{ENVELOPE_VERSION}",
            manifest.version
        ));
    }
    if manifest.source != source {
        return bad_request(&format!(
            "manifest is for {:?} but was sent to {source:?}",
            manifest.source
        ));
    }
    if manifest.chain.is_empty() {
        return bad_request("manifest has no chain id");
    }
    if manifest.chunks.is_empty() {
        return bad_request("manifest lists no chunks");
    }
    if let Err(e) = manifest.verify_signature(&peer.public_key) {
        return json(StatusCode::FORBIDDEN, serde_json::json!({ "error": e }));
    }
    if let Err(e) = validate_manifest_shape(&manifest) {
        return bad_request(&e);
    }
    let stored = match receiver
        .store
        .list_chunks(&peer.fingerprint, &source, &manifest.chain)
    {
        Ok(s) => s,
        Err(e) => {
            return json(
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({ "error": e.to_string() }),
            )
        }
    };
    let have: std::collections::HashSet<i64> = stored.into_iter().collect();
    let mut missing = Vec::new();
    for chunk in &manifest.chunks {
        if !have.contains(&chunk.index) {
            missing.push(chunk.index);
            if missing.len() >= 20 {
                break;
            }
        }
    }
    if !missing.is_empty() {
        return json(
            StatusCode::CONFLICT,
            serde_json::json!({
                "error": format!("{} of {} chunks of chain {} are missing", missing.len(), manifest.chunks.len(), manifest.chain),
                "missingChunks": missing,
            }),
        );
    }
    let pending = match receiver.begin_mutation(
        &peer.fingerprint,
        "buddy.manifest.put",
        Some(format!("{source}@{}", manifest.chain)),
        serde_json::json!({
            "sequence": manifest.sequence,
            "chunks": manifest.chunks.len(),
            "fromSnapshot": manifest.from_snapshot,
            "toSnapshot": manifest.to_snapshot,
        }),
    ) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    if let Err(e) = receiver
        .store
        .put_manifest(&peer.fingerprint, &source, &manifest)
    {
        let _ = receiver.finish_mutation(pending, StatusCode::INTERNAL_SERVER_ERROR);
        return json(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({ "error": e.to_string() }),
        );
    }
    receiver.store.invalidate_usage(&peer.fingerprint);
    let sealed: i64 = manifest.chunks.iter().map(|c| c.sealed_bytes).sum();
    if let Err(resp) = receiver.finish_mutation(pending, StatusCode::OK) {
        return resp;
    }
    json(
        StatusCode::OK,
        serde_json::json!({ "source": source, "chain": manifest.chain, "chunks": manifest.chunks.len(), "storedByte": sealed }),
    )
}

/// `POST /api/buddy/v1/prune/{source}`
pub async fn prune(
    State(receiver): State<BuddyState>,
    Path(source): Path<String>,
    headers: HeaderMap,
    uri: axum::http::Uri,
    body: Body,
) -> Response {
    if let Err(e) = validate_source(&source) {
        return bad_request(&e);
    }
    let body = match read_limited(body, 64 << 10).await {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let digest = body_digest(&body);
    let peer = match peer_for(
        &receiver,
        &headers,
        &digest,
        "POST",
        &uri_path(&uri),
        &source,
    ) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    #[derive(Default, serde::Deserialize)]
    struct PruneRequest {
        #[serde(default)]
        keep: usize,
    }
    let req: PruneRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return bad_request(&format!("invalid prune request: {e}")),
    };
    if req.keep < 1 {
        return bad_request("keep must be at least 1");
    }
    let pending = match receiver.begin_mutation(
        &peer.fingerprint,
        "buddy.prune",
        Some(source.clone()),
        serde_json::json!({ "keep": req.keep }),
    ) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    match receiver.store.prune(&peer.fingerprint, &source, req.keep) {
        Ok(removed) => {
            if let Err(resp) = receiver.finish_mutation(pending, StatusCode::OK) {
                return resp;
            }
            json(
                StatusCode::OK,
                serde_json::json!({ "source": source, "keep": req.keep, "removedChains": removed }),
            )
        }
        Err(e) => {
            let _ = receiver.finish_mutation(pending, StatusCode::INTERNAL_SERVER_ERROR);
            json(
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({ "error": e.to_string() }),
            )
        }
    }
}

/// Per-router state for the buddy handlers.
///
/// The API mounts them under the full `AppState`; the standalone receiver
/// mounts them with just the receiver and has no audit log. A local wrapper
/// keeps the `FromRef` impl legal (both `Arc` and axum's trait are foreign).
#[derive(Clone)]
pub struct BuddyState {
    pub receiver: Arc<Receiver>,
    app: Option<Arc<AppState>>,
}

impl BuddyState {
    pub fn new(receiver: Arc<Receiver>) -> Self {
        Self {
            receiver,
            app: None,
        }
    }

    fn audit(&self) -> Option<&crate::audit::AuditLog> {
        self.app.as_ref().map(|state| &state.audit)
    }

    /// Refuse a receiver mutation before it happens when the audit log cannot
    /// record it (R-027). The standalone receiver has no audit log, so it is
    /// unaffected.
    fn begin_mutation(
        &self,
        actor: &str,
        action: &str,
        target: Option<String>,
        params: serde_json::Value,
    ) -> Result<Option<crate::audit::Pending>, Response> {
        let Some(audit) = self.audit() else {
            return Ok(None);
        };
        if let Some(reason) = audit.degraded_reason() {
            return Err(json(
                StatusCode::SERVICE_UNAVAILABLE,
                serde_json::json!({ "error": format!("the audit log is degraded ({reason}); refusing the receiver mutation") }),
            ));
        }
        let pending = crate::audit::Pending::system(actor, action, target, params);
        audit.begin(pending.clone()).map_err(|e| {
            audit.mark_degraded(&e);
            json(
                StatusCode::SERVICE_UNAVAILABLE,
                serde_json::json!({ "error": format!("audit unavailable: {e}") }),
            )
        })?;
        Ok(Some(pending))
    }

    /// Like `begin_mutation` but for high-volume mutations (chunk uploads):
    /// check the log before the write and record one end record afterwards.
    fn pending_mutation(
        &self,
        actor: &str,
        action: &str,
        target: Option<String>,
        params: serde_json::Value,
    ) -> Result<Option<crate::audit::Pending>, Response> {
        let Some(audit) = self.audit() else {
            return Ok(None);
        };
        if let Some(reason) = audit.degraded_reason() {
            return Err(json(
                StatusCode::SERVICE_UNAVAILABLE,
                serde_json::json!({ "error": format!("the audit log is degraded ({reason}); refusing the receiver mutation") }),
            ));
        }
        Ok(Some(crate::audit::Pending::system(
            actor, action, target, params,
        )))
    }

    fn record_mutation(
        &self,
        pending: Option<crate::audit::Pending>,
        status: StatusCode,
    ) -> Result<(), Response> {
        let (Some(audit), Some(pending)) = (self.audit(), pending) else {
            return Ok(());
        };
        audit.record_once(&pending, status.as_u16()).map_err(|e| {
            audit.mark_degraded(&e);
            json(
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({ "error": format!("the audit record failed ({e}); the mutation may have applied") }),
            )
        })
    }

    fn finish_mutation(
        &self,
        pending: Option<crate::audit::Pending>,
        status: StatusCode,
    ) -> Result<(), Response> {
        let (Some(audit), Some(pending)) = (self.audit(), pending) else {
            return Ok(());
        };
        audit.finish(&pending, status.as_u16()).map_err(|e| {
            audit.mark_degraded(&e);
            json(
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({ "error": format!("the audit end record failed ({e}); the mutation may have applied") }),
            )
        })
    }
}

impl std::ops::Deref for BuddyState {
    type Target = Receiver;
    fn deref(&self) -> &Self::Target {
        &self.receiver
    }
}

impl axum::extract::FromRef<Arc<AppState>> for BuddyState {
    fn from_ref(state: &Arc<AppState>) -> Self {
        BuddyState {
            receiver: state
                .buddy
                .clone()
                .expect("buddy routes are only mounted when the receiver is configured"),
            app: Some(state.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{enrollment_quota, instance_audience, Receiver};
    use std::sync::Arc;

    fn receiver_named(name: &str) -> Receiver {
        let peers = Arc::new(crate::buddy::peers::PeerStore::new(""));
        let auth = Arc::new(crate::buddy::auth::Authenticator::new(peers.clone()));
        let store = Arc::new(crate::buddy::store::Store::new("/nonexistent-buddy-test"));
        Receiver::new(store, peers, auth, name)
    }

    #[test]
    fn two_receivers_with_the_same_name_get_distinct_audiences() {
        let a = receiver_named("naslos");
        let b = receiver_named("naslos");
        assert_eq!(a.name, b.name);
        assert_ne!(
            a.audience, b.audience,
            "equal operator names must not share a signing audience"
        );
        assert!(!a.audience.is_empty());
        assert_eq!(instance_audience("  ").matches(':').count(), 2);
    }

    #[test]
    fn enrollment_cannot_raise_its_own_quota() {
        assert_eq!(enrollment_quota(0, 100), 100);
        assert_eq!(enrollment_quota(50, 100), 50);
        assert_eq!(enrollment_quota(10_000, 100), 100);
        assert_eq!(enrollment_quota(i64::MAX, 0), 1i64 << 40);
    }
}
