//! The ISO/disk-image library (FR-VMT): one flat directory per pool at
//! `<pool>/vms/isos`.
//!
//! Three writers reach it: the chunked browser upload (relayed by the API),
//! the server-side URL importer, and deletion. The URL importer carries the
//! SSRF guard instead of a network policy: vmd is hostNetwork, whose traffic
//! is exempt from pod egress policy, so the target list is validated here —
//! public addresses only, http(s) only, redirects re-validated hop by hop
//! (FR-DNS-07 pattern).
//!
//! Every write is staged in a `<name>.part` file created with `create_new`
//! and `O_NOFOLLOW`, then renamed into place. That makes the size cap a real
//! cap (an over-cap upload never becomes a library file), keeps a failed
//! upload from destroying the previous image, and refuses a pre-existing
//! symlink instead of following it. One client (identified by an owner token)
//! owns a upload name at a time, chunks must arrive at exactly the current end
//! of the staged file, and replacing an existing image needs an explicit
//! `confirm=<name>`.

use crate::model::{
    library_path, valid_library_name, valid_pool_component, valid_upload_owner, ImageEntry,
};
use futures::StreamExt;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Largest accepted image (imports and total uploads).
pub const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024 * 1024;

/// An abandoned upload (client gone, API restarted) must not block a new
/// upload of the same name forever.
const UPLOAD_SESSION_TTL: Duration = Duration::from_secs(2 * 60 * 60);

/// Which writer owns a library name: a chunked upload or a URL import. The
/// distinction matters because an import's staging file must never be
/// appended to by an upload chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionKind {
    Upload,
    Import,
}

#[derive(Debug, Clone)]
struct Session {
    kind: SessionKind,
    /// The client token that owns an upload; empty for a server-side import.
    owner: String,
    started: Instant,
}

#[derive(Default)]
struct UploadRegistry {
    /// Name → session, keyed by the final path so two pools with the same
    /// file name are independent.
    active: HashMap<PathBuf, Session>,
    /// One async mutex per final path: sequential chunks are serialized while
    /// different images still upload concurrently.
    locks: HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>,
}

fn registry() -> &'static Mutex<UploadRegistry> {
    static REGISTRY: OnceLock<Mutex<UploadRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(UploadRegistry::default()))
}

fn registry_guard() -> std::sync::MutexGuard<'static, UploadRegistry> {
    registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

async fn upload_lock(path: &Path) -> tokio::sync::OwnedMutexGuard<()> {
    path_lock(path).lock_owned().await
}

fn path_lock(path: &Path) -> Arc<tokio::sync::Mutex<()>> {
    let mut registry = registry_guard();
    registry
        .locks
        .entry(path.to_path_buf())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

/// A non-blocking per-name lock: `None` means another request is actively
/// writing that name. Used by the orphan sweep so it never waits on (or
/// deadlocks with) a concurrent upload while holding its own name's lock.
fn try_upload_lock(path: &Path) -> Option<tokio::sync::OwnedMutexGuard<()>> {
    path_lock(path).try_lock_owned().ok()
}

fn begin_upload(path: &Path, kind: SessionKind, owner: &str) -> Result<(), String> {
    let mut registry = registry_guard();
    registry
        .active
        .retain(|_, session| session.started.elapsed() < UPLOAD_SESSION_TTL);
    if registry.active.contains_key(path) {
        return Err(format!(
            "another upload or import of {} is already in progress",
            label(path)
        ));
    }
    registry.active.insert(
        path.to_path_buf(),
        Session {
            kind,
            owner: owner.to_string(),
            started: Instant::now(),
        },
    );
    Ok(())
}

/// The live (non-expired) session for a name, if any.
///
/// An abandoned upload (client gone, API restarted) must not hold the name
/// forever; pruning on every read keeps the TTL effective even when no new
/// upload starts.
fn live_session(path: &Path) -> Option<Session> {
    let mut registry = registry_guard();
    if registry
        .active
        .get(path)
        .is_some_and(|session| session.started.elapsed() >= UPLOAD_SESSION_TTL)
    {
        registry.active.remove(path);
        return None;
    }
    registry.active.get(path).cloned()
}

fn session_kind(path: &Path) -> Option<SessionKind> {
    live_session(path).map(|session| session.kind)
}

fn upload_active(path: &Path) -> bool {
    session_kind(path) == Some(SessionKind::Upload)
}

/// Refresh an active session so the TTL is an idle timeout, not a hard cap on
/// a legitimately slow multi-gigabyte transfer.
fn touch_session(path: &Path) {
    if let Some(session) = registry_guard().active.get_mut(path) {
        session.started = Instant::now();
    }
}

fn finish_upload(path: &Path) {
    registry_guard().active.remove(path);
}

/// Releases a URL import's session on every exit path, including the `?`
/// early returns.
struct ImportSession {
    path: PathBuf,
}

impl ImportSession {
    fn begin(path: &Path) -> Result<Self, String> {
        begin_upload(path, SessionKind::Import, "")?;
        Ok(Self {
            path: path.to_path_buf(),
        })
    }
}

impl Drop for ImportSession {
    fn drop(&mut self) {
        finish_upload(&self.path);
    }
}

/// Whether an upload owns this library name right now (the import handler
/// refuses to race it).
pub fn upload_in_progress(root: &Path, pool: &str, name: &str) -> bool {
    if !valid_pool_component(pool) || !valid_library_name(name) {
        return false;
    }
    upload_active(&library_path(root, pool, name))
}

fn label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Walk `<root>/<pool>/vms/isos` and refuse a symlink at any component, so a
/// share account cannot redirect the library itself outside the dataset.
/// `create` creates the missing components (one level at a time, never
/// following a link); a read-only caller gets the path back even when a
/// component is absent (the caller then sees an empty library).
async fn library_dir(root: &Path, pool: &str, create: bool) -> Result<PathBuf, String> {
    let mut path = root.to_path_buf();
    for component in [pool, "vms", "isos"] {
        path = path.join(component);
        match tokio::fs::symlink_metadata(&path).await {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(format!(
                    "refusing to use the symlink at {}: the image library path must be real directories",
                    path.display()
                ))
            }
            Ok(meta) if !meta.is_dir() => {
                return Err(format!(
                    "{} exists but is not a directory",
                    path.display()
                ))
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if !create {
                    return Ok(path);
                }
                match tokio::fs::create_dir(&path).await {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        // A concurrent request created it; re-check below on
                        // the next loop iteration or on the next call.
                    }
                    Err(e) => return Err(format!("creating {}: {e}", path.display())),
                }
            }
            Err(e) => return Err(format!("checking {}: {e}", path.display())),
        }
    }
    Ok(path)
}

fn part_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    name.push(".part");
    path.with_file_name(name)
}

/// Refuse a target that exists as a symlink or is not a regular file, so a
/// share-writable directory cannot redirect an image write. Returns whether a
/// regular file already exists.
async fn refuse_symlink(path: &Path, what: &str) -> Result<bool, String> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(meta) if meta.file_type().is_symlink() => Err(format!(
            "refusing to follow the symlink at {}: {what} must be a regular file",
            path.display()
        )),
        Ok(meta) if !meta.is_file() => Err(format!(
            "{} exists but is not a regular file",
            path.display()
        )),
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("checking {}: {e}", path.display())),
    }
}

/// Remove a stale `.part` file (no active session) after refusing a symlink.
async fn clear_stale_part(part: &Path) -> Result<(), String> {
    match tokio::fs::symlink_metadata(part).await {
        Ok(meta) if meta.file_type().is_symlink() => Err(format!(
            "refusing to follow the symlink at {}",
            part.display()
        )),
        Ok(meta) if !meta.is_file() => Err(format!(
            "{} exists but is not a regular file",
            part.display()
        )),
        Ok(_) => tokio::fs::remove_file(part)
            .await
            .map_err(|e| format!("removing the stale {}: {e}", part.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("checking {}: {e}", part.display())),
    }
}

/// Remove `<name>.part` files that no live session owns (a crashed upload or
/// a failed publish). Active uploads and imports are keyed by their final path
/// and are left untouched; symlinks and non-files are left for the explicit
/// path checks to report. Best effort: a sweep failure must not fail the
/// caller's upload.
async fn sweep_orphan_parts(dir: &Path) {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(base) = name.strip_suffix(".part") else {
            continue;
        };
        let final_path = dir.join(base);
        // The liveness check and the unlink must be atomic with respect to an
        // upload of this name: a concurrent offset-0 restart can recreate the
        // staged file between the check and the unlink, and that writer would
        // then append to an unlinked inode and lose the transfer. Skip busy
        // names entirely (try-lock, never wait): waiting here while holding
        // the caller's own name lock could deadlock two uploads that sweep
        // each other.
        let Some(_guard) = try_upload_lock(&final_path) else {
            continue;
        };
        if live_session(&final_path).is_some() {
            continue;
        }
        let Ok(meta) = tokio::fs::symlink_metadata(entry.path()).await else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        if tokio::fs::remove_file(entry.path()).await.is_ok() {
            tracing::info!(
                "removed the orphaned staged image upload {}",
                entry.path().display()
            );
        }
    }
}

async fn open_new_part(part: &Path) -> Result<tokio::fs::File, String> {
    tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(part)
        .await
        .map_err(|e| format!("creating {}: {e}", part.display()))
}

async fn open_existing_part(part: &Path) -> Result<tokio::fs::File, String> {
    tokio::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(part)
        .await
        .map_err(|e| format!("opening {} at the requested offset: {e}", part.display()))
}

/// List the library of one pool. A missing directory is an empty library,
/// not an error (the API bootstraps it on first use). An invalid pool
/// component yields an empty list: the handlers reject it with 400 first.
/// Symlinks, directories and in-flight `.part` files are never listed.
pub async fn list(root: &Path, pool: &str) -> Vec<ImageEntry> {
    if !valid_pool_component(pool) {
        return Vec::new();
    }
    let Ok(dir) = library_dir(root, pool, false).await else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let Ok(entries) = tokio::fs::read_dir(&dir).await else {
        return out;
    };
    let mut entries = entries;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        if !valid_library_name(&name) || name.ends_with(".part") {
            continue;
        }
        let Ok(meta) = tokio::fs::symlink_metadata(entry.path()).await else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        out.push(ImageEntry {
            name,
            size_bytes: meta.len(),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Write one upload chunk at `offset`. Chunks must be contiguous: `offset`
/// has to equal the current staged size. `final_chunk` fsyncs, renames the
/// staged file into place and returns the total size.
///
/// `confirm_overwrite` is the caller's explicit `confirm=<name>`: replacing an
/// existing library image otherwise fails. The existing image is untouched
/// until the final rename, so a failed replace does not destroy it.
#[allow(clippy::too_many_arguments)]
pub async fn write_chunk(
    root: &Path,
    pool: &str,
    name: &str,
    offset: u64,
    final_chunk: bool,
    confirm_overwrite: bool,
    owner: &str,
    body: axum::body::Body,
) -> Result<u64, String> {
    write_chunk_capped(
        root,
        pool,
        name,
        offset,
        final_chunk,
        confirm_overwrite,
        owner,
        body,
        MAX_IMAGE_BYTES,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn write_chunk_capped(
    root: &Path,
    pool: &str,
    name: &str,
    offset: u64,
    final_chunk: bool,
    confirm_overwrite: bool,
    owner: &str,
    body: axum::body::Body,
    max_bytes: u64,
) -> Result<u64, String> {
    use tokio::io::AsyncWriteExt;

    if !valid_pool_component(pool) {
        return Err(format!("pool {pool:?} is not a valid dataset component"));
    }
    if !valid_library_name(name) {
        return Err(format!("{name:?} is not a valid library file name"));
    }
    library_dir(root, pool, true).await?;
    let path = library_path(root, pool, name);
    let part = part_path(&path);
    let _guard = upload_lock(&path).await;

    if offset > max_bytes {
        return Err(format!(
            "offset {offset} is past the {max_bytes}-byte library limit"
        ));
    }
    if !valid_upload_owner(owner) {
        return Err("a 1-128 character upload owner token is required on every chunk".to_string());
    }
    let parent = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| root.to_path_buf());

    let mut file = if offset == 0 {
        // Refusal checks come first: a refused overwrite (missing confirm, a
        // symlinked target, …) must leave any live session and its staged
        // file untouched.
        let exists = refuse_symlink(&path, "the image").await?;
        if exists && !confirm_overwrite {
            return Err(format!(
                "{name:?} already exists in the library; pass confirm={name} to replace it"
            ));
        }
        match live_session(&path) {
            Some(session) if session.kind == SessionKind::Import => {
                return Err(format!(
                    "an import of {name:?} is in progress; wait for it to finish"
                ));
            }
            Some(session) if session.owner != owner => {
                // Another client owns a live upload of this name. Its staged
                // file is not ours to discard; only an abandoned (expired)
                // session may be superseded.
                return Err(format!(
                    "another upload of {name:?} is in progress from a different client; wait for it to finish"
                ));
            }
            Some(_) => {
                // The same owner restarting at zero is the UI's recovery path
                // after a stale-offset error: replace its own session.
                finish_upload(&path);
            }
            None => {}
        }
        sweep_orphan_parts(&parent).await;
        clear_stale_part(&part).await?;
        begin_upload(&path, SessionKind::Upload, owner)?;
        match open_new_part(&part).await {
            Ok(file) => file,
            Err(e) => {
                finish_upload(&path);
                return Err(e);
            }
        }
    } else {
        let Some(session) = live_session(&path) else {
            return Err(format!(
                "no upload of {name:?} is in progress; start at offset 0"
            ));
        };
        if session.kind == SessionKind::Import {
            return Err(format!(
                "an import of {name:?} is in progress; wait for it to finish"
            ));
        }
        if session.owner != owner {
            return Err(format!(
                "another upload of {name:?} is in progress from a different client; wait for it to finish"
            ));
        }
        touch_session(&path);
        let meta = match tokio::fs::symlink_metadata(&part).await {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(format!(
                    "the staged upload of {name:?} is gone; start at offset 0"
                ))
            }
            Err(e) => return Err(format!("checking {}: {e}", part.display())),
        };
        if meta.file_type().is_symlink() || !meta.is_file() {
            return Err(format!(
                "refusing to append to {}: it is not a regular staged file",
                part.display()
            ));
        }
        if meta.len() != offset {
            return Err(format!(
                "offset {offset} does not match the {} bytes uploaded so far; restart from offset 0",
                meta.len()
            ));
        }
        open_existing_part(&part).await?
    };
    if offset > 0 {
        use tokio::io::AsyncSeekExt;
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|e| format!("seeking {} to offset {offset}: {e}", part.display()))?;
    }

    let mut written = 0u64;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(e) => {
                // Roll the staged file back to the request's start offset so
                // the same offset can be retried.
                let _ = file.set_len(offset).await;
                return Err(format!("reading the upload chunk: {e}"));
            }
        };
        if offset
            .saturating_add(written)
            .saturating_add(chunk.len() as u64)
            > max_bytes
        {
            drop(file);
            let _ = tokio::fs::remove_file(&part).await;
            finish_upload(&path);
            return Err(format!(
                "the image exceeds the {max_bytes}-byte library limit; the staged upload was removed"
            ));
        }
        if let Err(e) = file.write_all(&chunk).await {
            let _ = file.set_len(offset).await;
            return Err(format!("writing {}: {e}", path.display()));
        }
        written += chunk.len() as u64;
    }

    let total = offset.saturating_add(written);
    if !final_chunk {
        return Ok(total);
    }
    if total > max_bytes {
        drop(file);
        let _ = tokio::fs::remove_file(&part).await;
        finish_upload(&path);
        return Err(format!(
            "the image exceeds the {max_bytes}-byte library limit; the staged upload was removed"
        ));
    }
    if let Err(e) = file.sync_all().await {
        drop(file);
        // Every other terminal failure removes the staged file; a sync error
        // must not leave a full-size invisible part behind.
        let _ = tokio::fs::remove_file(&part).await;
        finish_upload(&path);
        return Err(format!("syncing {}: {e}", part.display()));
    }
    drop(file);
    if let Err(e) = tokio::fs::rename(&part, &path).await {
        // An invisible staged file would still occupy the dataset; publish
        // failures must not leave it behind.
        let _ = tokio::fs::remove_file(&part).await;
        finish_upload(&path);
        return Err(format!("installing {}: {e}", path.display()));
    }
    finish_upload(&path);
    Ok(total)
}

/// Abandon an upload session owned by `owner`, deleting its staged file.
///
/// Only the owner may release a live session; a different client gets a
/// conflict (the same rule that prevents chunk stealing), and a URL import
/// cannot be cancelled from the upload path. With no live session the call is
/// an idempotent success and removes any orphaned staged file, so a client
/// retrying a cancel after a crash converges.
pub async fn cancel_upload(root: &Path, pool: &str, name: &str, owner: &str) -> Result<(), String> {
    if !valid_pool_component(pool) {
        return Err(format!("pool {pool:?} is not a valid dataset component"));
    }
    if !valid_library_name(name) {
        return Err(format!("{name:?} is not a valid library file name"));
    }
    if !valid_upload_owner(owner) {
        return Err(
            "a 1-128 character upload owner token is required to cancel an upload".to_string(),
        );
    }
    let path = library_path(root, pool, name);
    let part = part_path(&path);
    // The same lock every chunk takes: cancellation cannot interleave with a
    // streaming chunk of this name.
    let _guard = upload_lock(&path).await;
    match live_session(&path) {
        None => clear_stale_part(&part).await,
        Some(session) if session.kind == SessionKind::Import => Err(format!(
            "an import of {name:?} is in progress; wait for it to finish"
        )),
        Some(session) if session.owner != owner => Err(format!(
            "another upload of {name:?} is in progress from a different client; wait for it to finish"
        )),
        Some(_) => {
            finish_upload(&path);
            clear_stale_part(&part).await
        }
    }
}

/// Import an image from a URL. The target is re-resolved and re-checked on
/// every redirect hop; an optional sha256 pins the content.
///
/// `before_publish` runs immediately before the staged file replaces the
/// library entry. The caller uses it to re-check that no VM started using the
/// image while the (possibly long) download ran; returning an error discards
/// the staged file and leaves the previous image untouched.
pub async fn import_url<F, Fut>(
    root: &Path,
    pool: &str,
    name: &str,
    url: &str,
    sha256: Option<&str>,
    confirm_overwrite: bool,
    before_publish: F,
) -> Result<u64, String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    use tokio::io::AsyncWriteExt;

    if !valid_pool_component(pool) {
        return Err(format!("pool {pool:?} is not a valid dataset component"));
    }
    if !valid_library_name(name) {
        return Err(format!("{name:?} is not a valid library file name"));
    }
    let expected = sha256.map(|h| {
        let clean = h.trim().trim_start_matches("sha256:").to_ascii_lowercase();
        clean
    });
    if let Some(h) = &expected {
        if h.len() != 64 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("{h:?} is not a sha256 hex digest"));
        }
    }

    library_dir(root, pool, true).await?;
    let path = library_path(root, pool, name);
    let exists = refuse_symlink(&path, "the image").await?;
    if exists && !confirm_overwrite {
        return Err(format!(
            "{name:?} already exists in the library; pass confirm={name} to replace it"
        ));
    }
    let part = part_path(&path);
    // Hold the per-name lock only while claiming the name and preparing the
    // staging file; the download itself runs outside it. The session outlives
    // the block (and is released by its Drop on every exit path).
    let _session = {
        let _guard = upload_lock(&path).await;
        let session = ImportSession::begin(&path)?;
        if let Some(parent) = path.parent() {
            sweep_orphan_parts(parent).await;
        }
        clear_stale_part(&part).await?;
        session
    };

    let mut target = url.to_string();
    let mut redirects = 0;
    let response = loop {
        let (safe_url, host, pinned) = validate_target(&target)?;
        // Pin the connection to the vetted address so the host cannot be
        // rebound between validation and the request (the ntfy client uses
        // the same defense). reqwest only resolves overrides at client
        // level, so each hop gets its own pinned client.
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(10))
            .read_timeout(std::time::Duration::from_secs(120))
            .resolve(&host, pinned)
            .build()
            .map_err(|e| format!("building the importer client: {e}"))?;
        let resp = client
            .get(&safe_url)
            .send()
            .await
            .map_err(|e| format!("fetching {target}: {e}"))?;
        if resp.status().is_redirection() {
            let location = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            if location.is_empty() {
                return Err(format!("{} redirected without a Location", resp.status()));
            }
            redirects += 1;
            if redirects > 3 {
                return Err("the URL redirects more than 3 times".to_string());
            }
            target = absolutize(&target, &location)?;
            continue;
        }
        if let Err(e) = resp.error_for_status_ref() {
            return Err(format!("fetching {target}: {e}"));
        }
        break resp;
    };

    let mut file = open_new_part(&part).await?;
    let mut hasher = Sha256::new();
    let mut written = 0u64;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(e) => {
                let _ = tokio::fs::remove_file(&part).await;
                return Err(format!("downloading {target}: {e}"));
            }
        };
        written += chunk.len() as u64;
        if written > MAX_IMAGE_BYTES {
            drop(file);
            let _ = tokio::fs::remove_file(&part).await;
            return Err(format!(
                "the image exceeds the {MAX_IMAGE_BYTES}-byte library limit"
            ));
        }
        touch_session(&path);
        hasher.update(&chunk);
        if let Err(e) = file.write_all(&chunk).await {
            drop(file);
            let _ = tokio::fs::remove_file(&part).await;
            return Err(format!("writing {}: {e}", part.display()));
        }
    }
    if let Err(e) = file.sync_all().await {
        drop(file);
        let _ = tokio::fs::remove_file(&part).await;
        return Err(format!("syncing {}: {e}", part.display()));
    }
    drop(file);
    if let Some(expected) = expected {
        let actual: String = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if actual != expected {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(format!(
                "sha256 mismatch: expected {expected}, got {actual}; the file was removed"
            ));
        }
    }
    // A VM may have started using this image while the download ran. Publish
    // is the last point before the library entry changes, so the attachment
    // check and the rename are adjacent (vmd's supervisor state is the source
    // of truth, and a start can only race this narrow window).
    publish_import(&part, &path, before_publish()).await?;
    Ok(written)
}

/// Publish a fully downloaded staging file after the caller's last-moment
/// check. A refused check removes the staged file and leaves the library
/// entry untouched.
async fn publish_import(
    part: &Path,
    path: &Path,
    before_publish: impl std::future::Future<Output = Result<(), String>>,
) -> Result<(), String> {
    if let Err(e) = before_publish.await {
        let _ = tokio::fs::remove_file(part).await;
        return Err(e);
    }
    if let Err(e) = tokio::fs::rename(part, path).await {
        // The staged file is invisible to `list()` but still occupies the
        // dataset; a failed rename must not leave it behind to be counted
        // against the library cap forever.
        let _ = tokio::fs::remove_file(part).await;
        return Err(format!("installing {}: {e}", path.display()));
    }
    Ok(())
}

/// Resolve a relative redirect against the current URL.
fn absolutize(current: &str, location: &str) -> Result<String, String> {
    let base = reqwest::Url::parse(current).map_err(|e| format!("parsing {current}: {e}"))?;
    let joined = base
        .join(location)
        .map_err(|e| format!("resolving redirect {location:?} against {current}: {e}"))?;
    Ok(joined.to_string())
}

/// Validate one fetch target: http(s) only, resolvable, and every resolved
/// address must be public. Returns the normalized URL, its host and the
/// pinned address the request must use.
fn validate_target(url: &str) -> Result<(String, String, SocketAddr), String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err("the import URL is empty".to_string());
    }
    if trimmed.starts_with('-') || trimmed.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("the import URL contains invalid characters".to_string());
    }
    let parsed = reqwest::Url::parse(trimmed)
        .map_err(|e| format!("the import URL {trimmed:?} is not valid: {e}"))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(format!(
            "the import URL must use http or https, got {:?}",
            parsed.scheme()
        ));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| "the import URL has no host".to_string())?
        .to_string();
    let port = parsed.port_or_known_default().unwrap_or(443);
    let addrs: Vec<SocketAddr> = if let Ok(ip) = host.parse::<IpAddr>() {
        if blocked_ip(&ip) {
            return Err(format!(
                "refusing to import from the non-public address {host}"
            ));
        }
        vec![SocketAddr::new(ip, port)]
    } else {
        (host.as_str(), port)
            .to_socket_addrs()
            .map_err(|e| format!("resolving the import host {host:?}: {e}"))?
            .collect()
    };
    if addrs.is_empty() {
        return Err(format!("the import host {host:?} did not resolve"));
    }
    for addr in &addrs {
        if blocked_ip(&addr.ip()) {
            return Err(format!(
                "refusing to import from {host} (resolves to the non-public address {})",
                addr.ip()
            ));
        }
    }
    Ok((parsed.to_string(), host, addrs[0]))
}

/// Non-public address classes, mirroring the API's DDNS/ntfy guard
/// (ddns::drivers::blocked_ip) so the importer accepts exactly the same
/// target space the platform already deems safe.
pub fn blocked_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || o[0] == 0
                || (o[0] == 100 && (o[1] & 0xc0) == 64)
                || (o[0] == 192 && o[1] == 0 && o[2] == 0)
                || (o[0] == 198 && (o[1] == 18 || o[1] == 19))
                || o[0] >= 240
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return blocked_ip(&IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            v6.is_loopback()
                || v6.is_multicast()
                || v6.is_unspecified()
                || (first & 0xffc0) == 0xfe80 // link-local fe80::/10
                || (first & 0xffc0) == 0xfec0 // deprecated site-local fec0::/10
                || (first & 0xfe00) == 0xfc00 // ULA fc00::/7
                || (first == 0x0064 && v6.segments()[1] == 0xff9b) // NAT64
                || first == 0x2002
                || (first == 0x2001 && v6.segments()[1] == 0x0db8)
        }
    }
}

/// Delete a library file. A symlink is refused (never followed); the caller
/// must refuse first when a running VM attaches the file.
pub async fn delete(root: &Path, pool: &str, name: &str) -> Result<(), String> {
    if !valid_pool_component(pool) {
        return Err(format!("pool {pool:?} is not a valid dataset component"));
    }
    if !valid_library_name(name) {
        return Err(format!("{name:?} is not a valid library file name"));
    }
    library_dir(root, pool, false).await?;
    let path = library_path(root, pool, name);
    match tokio::fs::symlink_metadata(&path).await {
        Ok(meta) if meta.file_type().is_symlink() => Err(format!(
            "refusing to follow the symlink at {}; remove it manually",
            path.display()
        )),
        Ok(meta) if !meta.is_file() => Err(format!(
            "{} exists but is not a regular file",
            path.display()
        )),
        Ok(_) => tokio::fs::remove_file(&path)
            .await
            .map_err(|e| format!("deleting {}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(format!("{name:?} is not in the library"))
        }
        Err(e) => Err(format!("deleting {}: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;

    fn root() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    /// The owner token most tests use; the mismatch tests pass their own.
    const TEST_OWNER: &str = "test-owner";

    fn body(bytes: &[u8]) -> Body {
        Body::from(bytes.to_vec())
    }

    #[test]
    fn private_targets_are_blocked() {
        for bad in [
            "127.0.0.1",
            "10.0.0.5",
            "192.168.1.10",
            "172.16.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "::1",
            "fe80::1",
            "fc00::1",
            "fec0::1",
        ] {
            let ip: IpAddr = bad.parse().unwrap();
            assert!(blocked_ip(&ip), "{bad} must be blocked");
        }
        for good in [
            "1.1.1.1",
            "93.184.216.34",
            "2606:2800:220:1:248:1893:25c8:1946",
        ] {
            let ip: IpAddr = good.parse().unwrap();
            assert!(!blocked_ip(&ip), "{good} must be allowed");
        }
        // IPv4-mapped IPv6 must be judged as IPv4.
        let mapped: IpAddr = "::ffff:127.0.0.1".parse().unwrap();
        assert!(blocked_ip(&mapped));
    }

    #[test]
    fn schemes_and_targets_are_validated() {
        assert!(validate_target("https://example.com/debian.iso").is_ok());
        assert!(validate_target("http://example.com/debian.iso").is_ok());
        assert!(validate_target("ftp://example.com/debian.iso").is_err());
        assert!(validate_target("https://127.0.0.1/x").is_err());
        assert!(validate_target("https://10.0.0.5/x").is_err());
        assert!(validate_target("file:///etc/passwd").is_err());
        assert!(validate_target("https:///nohost").is_err());
    }

    #[tokio::test]
    async fn chunked_uploads_assemble_in_order() {
        let dir = root();
        let total = write_chunk(
            dir.path(),
            "tank",
            "debian.iso",
            0,
            false,
            false,
            TEST_OWNER,
            body(b"hello "),
        )
        .await
        .expect("first chunk");
        assert_eq!(total, 6);
        let total = write_chunk(
            dir.path(),
            "tank",
            "debian.iso",
            6,
            true,
            false,
            TEST_OWNER,
            body(b"world"),
        )
        .await
        .expect("final chunk");
        assert_eq!(total, 11);
        let stored = tokio::fs::read(dir.path().join("tank/vms/isos/debian.iso"))
            .await
            .unwrap();
        assert_eq!(stored, b"hello world");
        let listed = list(dir.path(), "tank").await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].size_bytes, 11);
    }

    #[tokio::test]
    async fn an_offset_must_equal_the_uploaded_size() {
        let dir = root();
        write_chunk(
            dir.path(),
            "tank",
            "a.iso",
            0,
            false,
            false,
            TEST_OWNER,
            body(b"1234"),
        )
        .await
        .unwrap();
        let err = write_chunk(
            dir.path(),
            "tank",
            "a.iso",
            2,
            false,
            false,
            TEST_OWNER,
            body(b"xx"),
        )
        .await
        .unwrap_err();
        assert!(err.contains("does not match"), "{err}");
        assert!(err.contains("restart from offset 0"), "{err}");
        // The staged file is unchanged, so the correct offset still works.
        write_chunk(
            dir.path(),
            "tank",
            "a.iso",
            4,
            true,
            false,
            TEST_OWNER,
            body(b"56"),
        )
        .await
        .unwrap();
        assert_eq!(
            tokio::fs::read(dir.path().join("tank/vms/isos/a.iso"))
                .await
                .unwrap(),
            b"123456"
        );
    }

    #[tokio::test]
    async fn a_fresh_upload_at_zero_recovers_from_a_stale_offset() {
        let dir = root();
        write_chunk(
            dir.path(),
            "tank",
            "a.iso",
            0,
            false,
            false,
            TEST_OWNER,
            body(b"1234"),
        )
        .await
        .unwrap();
        // A stale offset tells the UI to restart from zero; that restart must
        // be accepted even though the abandoned session is still registered.
        let total = write_chunk(
            dir.path(),
            "tank",
            "a.iso",
            0,
            true,
            false,
            TEST_OWNER,
            body(b"fresh"),
        )
        .await
        .expect("restart from zero");
        assert_eq!(total, 5);
        assert_eq!(
            tokio::fs::read(dir.path().join("tank/vms/isos/a.iso"))
                .await
                .unwrap(),
            b"fresh"
        );
        // The superseded stream cannot keep appending.
        let err = write_chunk(
            dir.path(),
            "tank",
            "a.iso",
            4,
            false,
            false,
            TEST_OWNER,
            body(b"x"),
        )
        .await
        .unwrap_err();
        assert!(err.contains("no upload"), "{err}");
    }

    #[tokio::test]
    async fn only_the_owner_can_cancel_a_live_upload() {
        let dir = root();
        write_chunk(
            dir.path(),
            "tank",
            "a.iso",
            0,
            false,
            false,
            "owner-a",
            body(b"1234"),
        )
        .await
        .unwrap();

        let err = cancel_upload(dir.path(), "tank", "a.iso", "owner-b")
            .await
            .unwrap_err();
        assert!(err.contains("different client"), "{err}");
        assert!(
            dir.path().join("tank/vms/isos/a.iso.part").exists(),
            "a different client must not be able to delete the staged file"
        );

        // The owner can still finish, and a repeated cancel of the finished
        // session is an idempotent success.
        write_chunk(
            dir.path(),
            "tank",
            "a.iso",
            4,
            true,
            false,
            "owner-a",
            body(b"56"),
        )
        .await
        .unwrap();
        assert_eq!(
            tokio::fs::read(dir.path().join("tank/vms/isos/a.iso"))
                .await
                .unwrap(),
            b"123456"
        );
        cancel_upload(dir.path(), "tank", "a.iso", "owner-a")
            .await
            .expect("an already released session is a no-op");
    }

    #[tokio::test]
    async fn cancelling_an_upload_removes_its_staged_file() {
        let dir = root();
        write_chunk(
            dir.path(),
            "tank",
            "a.iso",
            0,
            false,
            false,
            "owner-a",
            body(b"partial"),
        )
        .await
        .unwrap();
        assert!(
            dir.path().join("tank/vms/isos/a.iso.part").exists(),
            "the staged file exists before the cancel"
        );

        cancel_upload(dir.path(), "tank", "a.iso", "owner-a")
            .await
            .expect("the owner cancels");
        assert!(!dir.path().join("tank/vms/isos/a.iso.part").exists());
        assert!(list(dir.path(), "tank").await.is_empty());
        assert!(live_session(&library_path(dir.path(), "tank", "a.iso")).is_none());

        // The cancelled owner can start a new upload at offset 0.
        write_chunk(
            dir.path(),
            "tank",
            "a.iso",
            0,
            true,
            false,
            "owner-a",
            body(b"fresh"),
        )
        .await
        .expect("a new upload after a cancel");
    }

    #[tokio::test]
    async fn an_import_session_cannot_be_cancelled_as_an_upload() {
        let dir = root();
        let path = library_path(dir.path(), "tank", "a.iso");
        begin_upload(&path, SessionKind::Import, "").unwrap();
        let err = cancel_upload(dir.path(), "tank", "a.iso", "owner-a")
            .await
            .unwrap_err();
        assert!(err.contains("import"), "{err}");
        finish_upload(&path);
    }

    #[tokio::test]
    async fn a_sweep_skips_a_busy_upload_name() {
        let dir = root();
        let isos = dir.path().join("tank/vms/isos");
        tokio::fs::create_dir_all(&isos).await.unwrap();
        tokio::fs::write(isos.join("busy.iso.part"), b"streaming")
            .await
            .unwrap();
        let path = library_path(dir.path(), "tank", "busy.iso");

        // Simulate an in-flight chunk: it holds the per-name lock but the
        // session may look expired to a concurrent liveness check.
        let guard = upload_lock(&path).await;
        sweep_orphan_parts(&isos).await;
        assert!(
            isos.join("busy.iso.part").exists(),
            "a busy name must never be swept"
        );
        drop(guard);

        // Once no writer is active and no session owns it, the same sweep
        // removes the orphan.
        sweep_orphan_parts(&isos).await;
        assert!(!isos.join("busy.iso.part").exists());
    }

    #[tokio::test]
    async fn a_second_client_cannot_steal_a_live_upload() {
        let dir = root();
        write_chunk(
            dir.path(),
            "tank",
            "a.iso",
            0,
            false,
            false,
            "owner-a",
            body(b"1234"),
        )
        .await
        .unwrap();

        // A different client at offset 0 must not reset the live session.
        let err = write_chunk(
            dir.path(),
            "tank",
            "a.iso",
            0,
            false,
            false,
            "owner-b",
            body(b"zz"),
        )
        .await
        .unwrap_err();
        assert!(err.contains("different client"), "{err}");
        assert_eq!(
            tokio::fs::read(dir.path().join("tank/vms/isos/a.iso.part"))
                .await
                .unwrap(),
            b"1234",
            "the first client's staged bytes must survive"
        );

        // The owner is checked on continuations too.
        let err = write_chunk(
            dir.path(),
            "tank",
            "a.iso",
            4,
            true,
            false,
            "owner-b",
            body(b"56"),
        )
        .await
        .unwrap_err();
        assert!(err.contains("different client"), "{err}");

        // The original owner can finish normally.
        write_chunk(
            dir.path(),
            "tank",
            "a.iso",
            4,
            true,
            false,
            "owner-a",
            body(b"56"),
        )
        .await
        .unwrap();
        assert_eq!(
            tokio::fs::read(dir.path().join("tank/vms/isos/a.iso"))
                .await
                .unwrap(),
            b"123456"
        );
    }

    #[tokio::test]
    async fn a_refused_overwrite_leaves_a_live_session_intact() {
        let dir = root();
        write_chunk(
            dir.path(),
            "tank",
            "a.iso",
            0,
            true,
            false,
            "owner-a",
            body(b"old"),
        )
        .await
        .unwrap();
        // Start an explicit replacement (confirm) with one chunk.
        write_chunk(
            dir.path(),
            "tank",
            "a.iso",
            0,
            false,
            true,
            "owner-a",
            body(b"new-"),
        )
        .await
        .unwrap();

        // A refused overwrite (no confirm) must not cancel that upload.
        let err = write_chunk(
            dir.path(),
            "tank",
            "a.iso",
            0,
            true,
            false,
            "owner-a",
            body(b"x"),
        )
        .await
        .unwrap_err();
        assert!(err.contains("confirm=a.iso"), "{err}");
        assert_eq!(
            tokio::fs::read(dir.path().join("tank/vms/isos/a.iso.part"))
                .await
                .unwrap(),
            b"new-",
            "the refused request must not clear the staged file"
        );

        write_chunk(
            dir.path(),
            "tank",
            "a.iso",
            4,
            true,
            true,
            "owner-a",
            body(b"body"),
        )
        .await
        .unwrap();
        assert_eq!(
            tokio::fs::read(dir.path().join("tank/vms/isos/a.iso"))
                .await
                .unwrap(),
            b"new-body"
        );
    }

    #[tokio::test]
    async fn an_abandoned_session_may_be_superseded_by_another_owner() {
        let dir = root();
        write_chunk(
            dir.path(),
            "tank",
            "a.iso",
            0,
            false,
            false,
            "owner-a",
            body(b"1234"),
        )
        .await
        .unwrap();
        // Age the session past the idle TTL, as a crashed client would.
        let path = library_path(dir.path(), "tank", "a.iso");
        {
            let mut registry = registry_guard();
            if let Some(session) = registry.active.get_mut(&path) {
                session.started = Instant::now() - UPLOAD_SESSION_TTL - Duration::from_secs(1);
            }
        }
        write_chunk(
            dir.path(),
            "tank",
            "a.iso",
            0,
            true,
            false,
            "owner-b",
            body(b"fresh"),
        )
        .await
        .expect("an abandoned session may be superseded");
        assert_eq!(
            tokio::fs::read(dir.path().join("tank/vms/isos/a.iso"))
                .await
                .unwrap(),
            b"fresh"
        );
    }

    #[tokio::test]
    async fn an_upload_without_an_owner_is_refused_before_any_state() {
        let dir = root();
        let err = write_chunk(dir.path(), "tank", "a.iso", 0, true, false, "", body(b"x"))
            .await
            .unwrap_err();
        assert!(err.contains("owner token"), "{err}");
        assert!(list(dir.path(), "tank").await.is_empty());
        assert!(live_session(&library_path(dir.path(), "tank", "a.iso")).is_none());
    }

    #[tokio::test]
    async fn a_later_upload_cleans_orphaned_part_files() {
        let dir = root();
        let isos = dir.path().join("tank/vms/isos");
        tokio::fs::create_dir_all(&isos).await.unwrap();
        // A crashed previous upload of this name and of another name.
        tokio::fs::write(isos.join("a.iso.part"), b"junk")
            .await
            .unwrap();
        tokio::fs::write(isos.join("crashed.iso.part"), b"orphan")
            .await
            .unwrap();

        write_chunk(
            dir.path(),
            "tank",
            "a.iso",
            0,
            true,
            false,
            TEST_OWNER,
            body(b"fresh"),
        )
        .await
        .expect("the retry starts clean");
        assert_eq!(tokio::fs::read(isos.join("a.iso")).await.unwrap(), b"fresh");
        assert!(!isos.join("crashed.iso.part").exists());
        assert!(!isos.join("a.iso.part").exists());
    }

    #[tokio::test]
    async fn a_failed_publish_rename_removes_the_staged_file() {
        let dir = root();
        let part = dir.path().join("new.part");
        let live = dir.path().join("live.iso");
        tokio::fs::write(&part, b"new").await.unwrap();
        // Renaming a file over a directory fails on Linux.
        tokio::fs::create_dir(&live).await.unwrap();
        let err = publish_import(&part, &live, async { Ok(()) })
            .await
            .unwrap_err();
        assert!(err.contains("installing"), "{err}");
        assert!(
            !part.exists(),
            "a failed publish must not leave the staged file behind"
        );
    }

    #[tokio::test]
    async fn a_lost_staged_file_tells_the_client_to_start_at_zero() {
        let dir = root();
        write_chunk(
            dir.path(),
            "tank",
            "a.iso",
            0,
            false,
            false,
            TEST_OWNER,
            body(b"1234"),
        )
        .await
        .unwrap();
        tokio::fs::remove_file(dir.path().join("tank/vms/isos/a.iso.part"))
            .await
            .unwrap();
        let err = write_chunk(
            dir.path(),
            "tank",
            "a.iso",
            4,
            false,
            false,
            TEST_OWNER,
            body(b"x"),
        )
        .await
        .unwrap_err();
        assert!(err.contains("start at offset 0"), "{err}");
        // The lingering session must not block the recovery upload.
        write_chunk(
            dir.path(),
            "tank",
            "a.iso",
            0,
            true,
            false,
            TEST_OWNER,
            body(b"ok"),
        )
        .await
        .expect("restart after the staged file was lost");
    }

    #[tokio::test]
    async fn an_appending_request_needs_an_active_upload() {
        let dir = root();
        tokio::fs::create_dir_all(dir.path().join("tank/vms/isos"))
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("tank/vms/isos/a.iso.part"), b"1234")
            .await
            .unwrap();
        let err = write_chunk(
            dir.path(),
            "tank",
            "a.iso",
            4,
            true,
            false,
            TEST_OWNER,
            body(b"56"),
        )
        .await
        .unwrap_err();
        assert!(err.contains("no upload"), "{err}");
    }

    #[tokio::test]
    async fn replacing_an_image_needs_explicit_confirmation() {
        let dir = root();
        write_chunk(
            dir.path(),
            "tank",
            "a.iso",
            0,
            true,
            false,
            TEST_OWNER,
            body(b"old"),
        )
        .await
        .unwrap();
        let err = write_chunk(
            dir.path(),
            "tank",
            "a.iso",
            0,
            true,
            false,
            TEST_OWNER,
            body(b"new"),
        )
        .await
        .unwrap_err();
        assert!(err.contains("confirm=a.iso"), "{err}");
        assert_eq!(
            tokio::fs::read(dir.path().join("tank/vms/isos/a.iso"))
                .await
                .unwrap(),
            b"old",
            "a refused replace must keep the existing image"
        );
        write_chunk(
            dir.path(),
            "tank",
            "a.iso",
            0,
            true,
            true,
            TEST_OWNER,
            body(b"new"),
        )
        .await
        .unwrap();
        assert_eq!(
            tokio::fs::read(dir.path().join("tank/vms/isos/a.iso"))
                .await
                .unwrap(),
            b"new"
        );
    }

    #[tokio::test]
    async fn a_planted_symlink_is_refused_not_followed() {
        let dir = root();
        let isos = dir.path().join("tank/vms/isos");
        tokio::fs::create_dir_all(&isos).await.unwrap();
        let outside = dir.path().join("outside.img");
        tokio::fs::write(&outside, b"do not touch").await.unwrap();
        std::os::unix::fs::symlink(&outside, isos.join("a.iso")).unwrap();

        let err = write_chunk(
            dir.path(),
            "tank",
            "a.iso",
            0,
            true,
            true,
            TEST_OWNER,
            body(b"x"),
        )
        .await
        .unwrap_err();
        assert!(err.contains("symlink"), "{err}");
        assert_eq!(tokio::fs::read(&outside).await.unwrap(), b"do not touch");

        let err = delete(dir.path(), "tank", "a.iso").await.unwrap_err();
        assert!(err.contains("symlink"), "{err}");
        assert!(outside.exists(), "the symlink target must survive");
        assert!(list(dir.path(), "tank").await.is_empty());
    }

    #[tokio::test]
    async fn a_failed_replace_keeps_the_previous_image_and_no_part_file() {
        let dir = root();
        write_chunk(
            dir.path(),
            "tank",
            "a.iso",
            0,
            true,
            false,
            TEST_OWNER,
            body(b"good"),
        )
        .await
        .unwrap();
        // A replacement that exceeds the cap (a tiny cap here) must leave the
        // previous image untouched and must not publish a partial file.
        let err = write_chunk_capped(
            dir.path(),
            "tank",
            "a.iso",
            0,
            true,
            true,
            TEST_OWNER,
            body(b"too large"),
            4,
        )
        .await
        .unwrap_err();
        assert!(err.contains("limit"), "{err}");
        assert_eq!(
            tokio::fs::read(dir.path().join("tank/vms/isos/a.iso"))
                .await
                .unwrap(),
            b"good"
        );
        assert!(
            !dir.path().join("tank/vms/isos/a.iso.part").exists(),
            "the failed upload must not leave a staged file"
        );
        let listed = list(dir.path(), "tank").await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "a.iso");
    }

    #[tokio::test]
    async fn an_over_cap_upload_is_removed_and_releases_the_session() {
        let dir = root();
        let err = write_chunk_capped(
            dir.path(),
            "tank",
            "a.iso",
            0,
            true,
            false,
            TEST_OWNER,
            body(b"0123456789"),
            8,
        )
        .await
        .unwrap_err();
        assert!(err.contains("limit"), "{err}");
        assert!(list(dir.path(), "tank").await.is_empty());
        assert!(!dir.path().join("tank/vms/isos/a.iso.part").exists());
        // The session must be released: a new upload can start at offset 0.
        write_chunk_capped(
            dir.path(),
            "tank",
            "a.iso",
            0,
            true,
            false,
            TEST_OWNER,
            body(b"ok"),
            8,
        )
        .await
        .expect("a new upload after a cap failure");
    }

    #[tokio::test]
    async fn the_cap_counts_the_whole_upload_not_one_request() {
        let dir = root();
        let total = write_chunk_capped(
            dir.path(),
            "tank",
            "a.iso",
            0,
            false,
            false,
            TEST_OWNER,
            body(b"12345"),
            8,
        )
        .await
        .expect("first chunk");
        assert_eq!(total, 5);
        // The old code capped only the bytes of the current request, so this
        // second chunk would have passed and left a 9-byte file.
        let err = write_chunk_capped(
            dir.path(),
            "tank",
            "a.iso",
            5,
            false,
            false,
            TEST_OWNER,
            body(b"1234"),
            8,
        )
        .await
        .unwrap_err();
        assert!(err.contains("limit"), "{err}");
        assert!(list(dir.path(), "tank").await.is_empty());
        assert!(!dir.path().join("tank/vms/isos/a.iso.part").exists());
    }

    #[tokio::test]
    async fn a_refused_publish_check_keeps_the_previous_image() {
        let dir = root();
        let part = dir.path().join("new.part");
        let live = dir.path().join("live.iso");
        tokio::fs::write(&part, b"new").await.unwrap();
        tokio::fs::write(&live, b"old").await.unwrap();

        let err = publish_import(&part, &live, async {
            Err("cannot replace \"live.iso\": VM vm1 is running attaches it".to_string())
        })
        .await
        .unwrap_err();
        assert!(err.contains("vm1"), "{err}");
        assert_eq!(tokio::fs::read(&live).await.unwrap(), b"old");
        assert!(!part.exists(), "a refused publish removes the staged file");

        // With no refusal, the staged file replaces the entry atomically.
        tokio::fs::write(&part, b"new").await.unwrap();
        publish_import(&part, &live, async { Ok(()) })
            .await
            .unwrap();
        assert_eq!(tokio::fs::read(&live).await.unwrap(), b"new");
        assert!(!part.exists());
    }

    #[tokio::test]
    async fn a_symlinked_library_directory_is_refused() {
        let dir = root();
        tokio::fs::create_dir_all(dir.path().join("tank/vms"))
            .await
            .unwrap();
        let outside = dir.path().join("outside");
        tokio::fs::create_dir_all(&outside).await.unwrap();
        std::os::unix::fs::symlink(&outside, dir.path().join("tank/vms/isos")).unwrap();

        let err = write_chunk(
            dir.path(),
            "tank",
            "a.iso",
            0,
            true,
            false,
            TEST_OWNER,
            body(b"x"),
        )
        .await
        .unwrap_err();
        assert!(err.contains("symlink"), "{err}");
        assert!(list(dir.path(), "tank").await.is_empty());
        let err = delete(dir.path(), "tank", "a.iso").await.unwrap_err();
        assert!(err.contains("symlink"), "{err}");
        assert!(
            std::fs::read_dir(&outside).unwrap().next().is_none(),
            "nothing may be written through the library symlink"
        );
    }

    #[test]
    fn an_expired_session_is_pruned() {
        let dir = root();
        let path = library_path(dir.path(), "tank", "a.iso");
        {
            let mut registry = registry_guard();
            registry.active.insert(
                path.clone(),
                Session {
                    kind: SessionKind::Upload,
                    owner: "old-owner".to_string(),
                    started: Instant::now() - UPLOAD_SESSION_TTL - Duration::from_secs(1),
                },
            );
        }
        assert!(session_kind(&path).is_none());
        assert!(!upload_active(&path));
        assert!(begin_upload(&path, SessionKind::Upload, TEST_OWNER).is_ok());
        finish_upload(&path);
    }

    #[test]
    fn an_import_session_refuses_upload_chunks() {
        let dir = root();
        let path = library_path(dir.path(), "tank", "a.iso");
        begin_upload(&path, SessionKind::Import, "").unwrap();
        assert_eq!(session_kind(&path), Some(SessionKind::Import));
        assert!(
            !upload_active(&path),
            "an import's staging file is not an upload target"
        );
        assert!(begin_upload(&path, SessionKind::Upload, TEST_OWNER).is_err());
        finish_upload(&path);
        assert!(session_kind(&path).is_none());
        begin_upload(&path, SessionKind::Upload, TEST_OWNER).unwrap();
        finish_upload(&path);
    }

    #[test]
    fn redirects_resolve_relative_locations() {
        assert_eq!(
            absolutize("https://example.com/a/", "b.iso").unwrap(),
            "https://example.com/a/b.iso"
        );
        assert_eq!(
            absolutize("https://example.com/a/", "https://mirror.example.org/c.iso").unwrap(),
            "https://mirror.example.org/c.iso"
        );
    }
}
