//! Operator client for Buddy Backup (port of `cmd/buddyctl`): it creates the key
//! that identifies an instance to its buddies, enrolls that key on a receiver,
//! and pushes or restores backups through the same protocol the UI uses.
//!
//! Everything it uploads is encrypted on this side of the wire, so the receiver
//! (a second Naslos instance or a plain Docker volume) stores ciphertext it
//! cannot read. See docs/buddy-backup.md.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use clap::{Parser, Subcommand};
use naslos_api::buddy::client::{
    ChainState, Client, PushOptions, PushProgress, PushResult, RestoreOptions,
};
use naslos_api::buddy::keys::{load_identity, new_identity, Identity};
use naslos_api::buddy::peers::validate_source;

#[derive(Parser)]
#[command(
    name = "buddyctl",
    about = "Buddy Backup client: identity, enroll, push, restore and prune",
    after_help = "Global identity file: --identity (default $BUDDY_IDENTITY or ~/.naslos/buddy-identity.json)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create (or show) this instance's key material
    Identity {
        #[arg(long)]
        identity: Option<String>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Authorize this key on a receiver (needs its token)
    Enroll {
        receiver: String,
        #[arg(long)]
        identity: Option<String>,
        #[arg(long)]
        token: String,
        #[arg(long)]
        name: Option<String>,
        /// Comma-separated source prefixes this key may write (default: any)
        #[arg(long)]
        sources: Option<String>,
    },
    /// Free space, quota and last backup on the receiver
    Status {
        receiver: String,
        #[arg(long)]
        identity: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// List the backups stored for this key
    Backups {
        receiver: String,
        #[arg(long)]
        identity: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Encrypt and upload a directory, file or stream (stdin)
    Push {
        receiver: String,
        #[arg(long)]
        identity: Option<String>,
        #[arg(long)]
        source: String,
        #[arg(long)]
        dir: Option<String>,
        #[arg(long)]
        file: Option<String>,
        /// Payload label: tar, zfs-send, raw (default: tar for --dir, raw otherwise)
        #[arg(long)]
        kind: Option<String>,
        /// Resume-state file (default: ~/.naslos/buddy-state-<source>.json)
        #[arg(long)]
        state: Option<String>,
        #[arg(long)]
        resume: bool,
        #[arg(long = "prune-keep", default_value_t = 0)]
        prune_keep: usize,
        #[arg(long)]
        quiet: bool,
    },
    /// Download, verify and decrypt a backup
    Restore {
        receiver: String,
        #[arg(long)]
        identity: Option<String>,
        #[arg(long)]
        source: String,
        #[arg(long)]
        chain: Option<String>,
        #[arg(long)]
        dir: Option<String>,
        #[arg(long)]
        out: Option<String>,
        #[arg(long = "verify-only")]
        verify_only: bool,
        #[arg(long)]
        quiet: bool,
    },
    /// Keep only the newest N chains of a source
    Prune {
        receiver: String,
        #[arg(long)]
        identity: Option<String>,
        #[arg(long)]
        source: String,
        #[arg(long)]
        keep: usize,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Identity {
            identity,
            name,
            json,
        } => cmd_identity(identity, name, json),
        Command::Enroll {
            receiver,
            identity,
            token,
            name,
            sources,
        } => cmd_enroll(receiver, identity, token, name, sources).await,
        Command::Status {
            receiver,
            identity,
            json,
        } => cmd_status(receiver, identity, json).await,
        Command::Backups {
            receiver,
            identity,
            json,
        } => cmd_backups(receiver, identity, json).await,
        Command::Push {
            receiver,
            identity,
            source,
            dir,
            file,
            kind,
            state,
            resume,
            prune_keep,
            quiet,
        } => {
            cmd_push(
                receiver, identity, source, dir, file, kind, state, resume, prune_keep, quiet,
            )
            .await
        }
        Command::Restore {
            receiver,
            identity,
            source,
            chain,
            dir,
            out,
            verify_only,
            quiet,
        } => {
            cmd_restore(
                receiver,
                identity,
                source,
                chain,
                dir,
                out,
                verify_only,
                quiet,
            )
            .await
        }
        Command::Prune {
            receiver,
            identity,
            source,
            keep,
        } => cmd_prune(receiver, identity, source, keep).await,
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn default_state_dir() -> PathBuf {
    home().join(".naslos")
}

fn default_identity_path() -> PathBuf {
    match std::env::var("BUDDY_IDENTITY") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => default_state_dir().join("buddy-identity.json"),
    }
}

fn default_state_path(source: &str) -> PathBuf {
    let name = source.replace(['/', '\\'], "_");
    default_state_dir().join(format!("buddy-state-{name}.json"))
}

/// Read the key file, creating it on first use so the first command an operator
/// runs is the one that produces their identity.
fn load_or_create(path: Option<&str>, name: Option<&str>) -> Result<(Identity, String), String> {
    let path = path
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(default_identity_path);
    let shown = path.to_string_lossy().to_string();
    if !path.exists() {
        let mut label = name.unwrap_or("").to_string();
        if label.is_empty() {
            label = std::env::var("HOSTNAME").unwrap_or_default();
        }
        if label.is_empty() {
            label = "naslos".to_string();
        }
        let identity = new_identity(&label.to_lowercase())?;
        identity.save(&shown)?;
        eprintln!("created identity {shown} ({})", identity.name);
    }
    Ok((load_identity(&shown)?, shown))
}

fn client(receiver: &str, identity: Identity) -> Client {
    let mut client = Client::new(receiver, Arc::new(identity));
    client.sequence_dir = default_state_dir()
        .join("buddy-sequences")
        .to_string_lossy()
        .to_string();
    client
}

fn human_bytes(n: i64) -> String {
    const UNITS: [&str; 6] = ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    println!("{text}");
    Ok(())
}

fn cmd_identity(identity: Option<String>, name: Option<String>, json: bool) -> Result<(), String> {
    let (id, path) = load_or_create(identity.as_deref(), name.as_deref())?;
    let fingerprint = id.fingerprint()?;
    if json {
        return print_json(&serde_json::json!({
            "name": id.name,
            "publicKey": id.public_key,
            "fingerprint": fingerprint,
            "identity": path,
        }));
    }
    println!("name:        {}", id.name);
    println!("fingerprint: {fingerprint}");
    println!("public key:  {}", id.public_key);
    println!();
    println!("Authorize this key on a receiver with:");
    println!("  buddyctl enroll <receiver-url> --token <token>");
    println!("or add the public key above to the receiver's peer list.");
    Ok(())
}

async fn cmd_enroll(
    receiver: String,
    identity: Option<String>,
    token: String,
    name: Option<String>,
    sources: Option<String>,
) -> Result<(), String> {
    let (id, _) = load_or_create(identity.as_deref(), None)?;
    let registered = match name.as_deref().filter(|n| !n.is_empty()) {
        Some(n) => n.to_string(),
        None => id.name.clone(),
    };
    let sources: Vec<String> = sources
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let fingerprint = client(&receiver, id)
        .enroll(&token, &registered, &sources)
        .await?;
    println!("enrolled as {registered} ({fingerprint})");
    Ok(())
}

async fn cmd_status(receiver: String, identity: Option<String>, json: bool) -> Result<(), String> {
    let (id, _) = load_or_create(identity.as_deref(), None)?;
    let status = client(&receiver, id).status().await?;
    if json {
        return print_json(&status);
    }
    println!(
        "receiver:      {} (protocol v{}, {})",
        status.receiver, status.protocol_version, status.version
    );
    println!("free space:    {}", human_bytes(status.free_bytes));
    print!("stored for me: {}", human_bytes(status.used_bytes));
    if status.quota_bytes > 0 {
        print!(" of {}", human_bytes(status.quota_bytes));
    }
    println!();
    match &status.last_backup {
        Some(t) => {
            let secs = (chrono::Utc::now() - *t).num_seconds().max(0);
            println!(
                "last backup:   {} ({secs}s ago)",
                t.format("%Y-%m-%d %H:%M:%S")
            );
        }
        None => println!("last backup:   never"),
    }
    println!("sources:       {}", status.sources.join(", "));
    println!("peers allowed: {}", status.peers.join(", "));
    if status.enroll_open {
        println!("enrollment:    open (a token is configured)");
    }
    Ok(())
}

async fn cmd_backups(receiver: String, identity: Option<String>, json: bool) -> Result<(), String> {
    let (id, _) = load_or_create(identity.as_deref(), None)?;
    let backups = client(&receiver, id).backups().await?;
    if json {
        return print_json(&backups);
    }
    if backups.is_empty() {
        println!("no backups stored for this key");
        return Ok(());
    }
    println!(
        "{:<28} {:<6} {:<20} STORED",
        "SOURCE", "KIND", "LAST BACKUP"
    );
    for b in &backups {
        println!(
            "{:<28} {:<6} {:<20} {} ({} chunks)",
            b.source,
            b.kind,
            b.created_at.format("%Y-%m-%d %H:%M:%S"),
            human_bytes(b.stored_bytes),
            b.chunks
        );
    }
    Ok(())
}

/// Walk a directory, returning its regular files in a stable order so the same
/// tree always produces the same stream.
fn collect_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
        let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            let meta = entry.metadata().map_err(|e| e.to_string())?;
            if meta.is_dir() {
                walk(&path, out)?;
            } else if meta.is_file() {
                out.push(path);
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(root, &mut files)?;
    files.sort();
    Ok(files)
}

fn parent_dirs(rel: &str) -> Vec<String> {
    let parts: Vec<&str> = rel.split('/').collect();
    (1..parts.len()).map(|i| parts[..i].join("/")).collect()
}

/// Write a tar archive of `files` (with their parent directories) to `out`.
fn write_tar(root: &Path, files: &[PathBuf], out: &mut dyn Write) -> Result<(), String> {
    let mut builder = tar::Builder::new(out);
    let mut written_dirs = std::collections::HashSet::new();
    for file in files {
        let rel = file
            .strip_prefix(root)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        for parent in parent_dirs(&rel) {
            if written_dirs.insert(parent.clone()) {
                let mut header = tar::Header::new_gnu();
                header.set_entry_type(tar::EntryType::Directory);
                header.set_mode(0o755);
                header.set_size(0);
                header.set_cksum();
                builder
                    .append_data(&mut header, format!("{parent}/"), std::io::empty())
                    .map_err(|e| e.to_string())?;
            }
        }
        builder
            .append_path_with_name(file, &rel)
            .map_err(|e| e.to_string())?;
    }
    builder.finish().map_err(|e| e.to_string())
}

/// Unpack a tar stream into `dest`, refusing anything that would escape it.
fn extract_tar(reader: &mut dyn Read, dest: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let mut archive = tar::Archive::new(reader);
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path().map_err(|e| e.to_string())?.to_path_buf();
        if path.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            return Err(format!(
                "refusing archive entry {:?}: it escapes the destination",
                path.display()
            ));
        }
        match entry.header().entry_type() {
            tar::EntryType::Directory => {
                std::fs::create_dir_all(dest.join(&path)).map_err(|e| e.to_string())?;
            }
            tar::EntryType::Regular => {
                entry.unpack_in(dest).map_err(|e| e.to_string())?;
            }
            _ => {}
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn cmd_push(
    receiver: String,
    identity: Option<String>,
    source: String,
    dir: Option<String>,
    file: Option<String>,
    kind: Option<String>,
    state_path: Option<String>,
    resume: bool,
    prune_keep: usize,
    quiet: bool,
) -> Result<(), String> {
    validate_source(&source)?;
    let (id, _) = load_or_create(identity.as_deref(), None)?;

    let mut kind = kind.unwrap_or_default();
    enum Source {
        Dir(PathBuf),
        File(PathBuf),
        Stdin,
    }
    let source_kind = match (&dir, &file) {
        (Some(d), _) => {
            if kind.is_empty() {
                kind = "tar".to_string();
            }
            Source::Dir(PathBuf::from(d))
        }
        (None, Some(f)) => {
            if kind.is_empty() {
                kind = "raw".to_string();
            }
            Source::File(PathBuf::from(f))
        }
        (None, None) => {
            if kind.is_empty() {
                kind = "raw".to_string();
            }
            Source::Stdin
        }
    };

    let mut reader: Box<dyn tokio::io::AsyncRead + Unpin + Send> = match source_kind {
        Source::Dir(root) => {
            let files = collect_files(&root)?;
            let (read_half, write_half) = tokio::io::duplex(64 << 10);
            let root_clone = root.clone();
            tokio::task::spawn_blocking(move || {
                let mut bridge = tokio_util::io::SyncIoBridge::new(write_half);
                write_tar(&root_clone, &files, &mut bridge)
            });
            Box::new(read_half)
        }
        Source::File(path) => Box::new(
            tokio::fs::File::open(&path)
                .await
                .map_err(|e| e.to_string())?,
        ),
        Source::Stdin => Box::new(tokio::io::stdin()),
    };

    let state_path = state_path
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| default_state_path(&source));
    let state_shown = state_path.to_string_lossy().to_string();

    let mut state = if resume {
        Some(ChainState::load(&state_shown).map_err(|e| format!("cannot resume: {e}"))?)
    } else {
        None
    };
    let state = match state.take() {
        Some(s) => s,
        None => ChainState::new(&source, &kind)?,
    };
    // The state carries the chain's data key, so it is written (0600) *before*
    // the first chunk goes out.
    state.save(&state_shown)?;

    let progress = |p: PushProgress| {
        if p.chunk % 32 == 0 || p.chunk == 0 {
            eprint!(
                "\r  {} sent, {} stored ({} chunks, {} already there)",
                human_bytes(p.plain_bytes),
                human_bytes(p.sealed_bytes),
                p.chunk + 1,
                p.skipped
            );
        }
    };
    let progress_ref = if quiet { None } else { Some(&progress) };

    let (result, error) = client(&receiver, id)
        .push(PushOptions {
            source: &source,
            kind: &kind,
            reader: reader.as_mut(),
            state: Some(state.clone()),
            from_snapshot: "",
            to_snapshot: "",
            from_guid: "",
            to_guid: "",
            prune_keep,
            progress: progress_ref.map(|f| f as &(dyn Fn(PushProgress) + Send + Sync)),
            before_publish: None,
            save_state: &|s| s.save(&state_shown),
        })
        .await;

    if let Some(err) = error {
        // Keep the generation the push persisted before transmitting; saving
        // the pre-push clone here would re-arm a nonce for the next attempt.
        let current = result.state.clone().unwrap_or_else(|| state.clone());
        if let Err(save_err) = current.save(&state_shown) {
            eprintln!("could not persist the resume state: {save_err}");
        }
        eprintln!(
            "\nthe push did not finish. Re-run with --resume to continue chain {}",
            current.chain
        );
        return Err(err);
    }

    // Published: the chain is complete, so the resume state is no longer needed.
    let _ = std::fs::remove_file(&state_path);
    if !quiet {
        eprintln!();
    }
    print_push_result(&result);
    Ok(())
}

fn print_push_result(result: &PushResult) {
    println!(
        "backed up {} to chain {}: {} in {} chunks ({} stored, {} uploaded, {} already present)",
        result.source,
        result.chain,
        human_bytes(result.plain_bytes),
        result.chunks,
        human_bytes(result.sealed_bytes),
        result.uploaded,
        result.skipped
    );
    if result.pruned_chains > 0 {
        println!("pruned {} older chains", result.pruned_chains);
    }
}

#[allow(clippy::too_many_arguments)]
async fn cmd_restore(
    receiver: String,
    identity: Option<String>,
    source: String,
    chain: Option<String>,
    dir: Option<String>,
    out: Option<String>,
    verify_only: bool,
    quiet: bool,
) -> Result<(), String> {
    validate_source(&source)?;
    let (id, _) = load_or_create(identity.as_deref(), None)?;
    let chain = chain.unwrap_or_default();

    enum Out {
        Sink,
        Dir(PathBuf),
        File(PathBuf),
        Stdout,
    }
    let dest = match (&dir, &out) {
        _ if verify_only => Some(Out::Sink),
        (Some(d), _) => Some(Out::Dir(PathBuf::from(d))),
        (None, Some(f)) => Some(Out::File(PathBuf::from(f))),
        _ => Some(Out::Stdout),
    };

    let progress = |plain: i64, chunks: usize, _sealed: i64| {
        eprint!("\r  {} verified of {chunks} chunks", human_bytes(plain));
    };
    let progress_ref = if quiet { None } else { Some(&progress) };

    let result = match dest {
        Some(Out::Sink) => {
            let mut sink = tokio::io::sink();
            let (result, error) = client(&receiver, id)
                .restore(RestoreOptions {
                    source: &source,
                    chain: &chain,
                    out: &mut sink,
                    progress: progress_ref.map(|f| f as &(dyn Fn(i64, usize, i64) + Send + Sync)),
                    skip_signature_check: false,
                    expected_sequence: None,
                })
                .await;
            if let Some(e) = error {
                return Err(e);
            }
            result
        }
        Some(Out::Stdout) => {
            let mut stdout = tokio::io::stdout();
            let (result, error) = client(&receiver, id)
                .restore(RestoreOptions {
                    source: &source,
                    chain: &chain,
                    out: &mut stdout,
                    progress: progress_ref.map(|f| f as &(dyn Fn(i64, usize, i64) + Send + Sync)),
                    skip_signature_check: false,
                    expected_sequence: None,
                })
                .await;
            if let Some(e) = error {
                return Err(e);
            }
            tokio::io::AsyncWriteExt::flush(&mut stdout)
                .await
                .map_err(|e| e.to_string())?;
            result
        }
        Some(Out::File(path)) => {
            let mut file = tokio::fs::File::create(&path)
                .await
                .map_err(|e| e.to_string())?;
            let (result, error) = client(&receiver, id)
                .restore(RestoreOptions {
                    source: &source,
                    chain: &chain,
                    out: &mut file,
                    progress: progress_ref.map(|f| f as &(dyn Fn(i64, usize, i64) + Send + Sync)),
                    skip_signature_check: false,
                    expected_sequence: None,
                })
                .await;
            if let Some(e) = error {
                return Err(e);
            }
            tokio::io::AsyncWriteExt::flush(&mut file)
                .await
                .map_err(|e| e.to_string())?;
            result
        }
        Some(Out::Dir(dest)) => {
            let (read_half, mut write_half) = tokio::io::duplex(64 << 10);
            let handle = tokio::task::spawn_blocking(move || {
                let mut bridge = tokio_util::io::SyncIoBridge::new(read_half);
                extract_tar(&mut bridge, &dest)
            });
            let (result, error) = client(&receiver, id)
                .restore(RestoreOptions {
                    source: &source,
                    chain: &chain,
                    out: &mut write_half,
                    progress: progress_ref.map(|f| f as &(dyn Fn(i64, usize, i64) + Send + Sync)),
                    skip_signature_check: false,
                    expected_sequence: None,
                })
                .await;
            // Signal EOF to the extractor and let it report its own error.
            drop(write_half);
            let extracted = handle.await.map_err(|e| e.to_string())?;
            if let Some(e) = error {
                return Err(e);
            }
            extracted?;
            result
        }
        None => unreachable!(),
    };

    if !quiet {
        eprintln!();
    }
    // When the payload itself goes to stdout (`... | zfs receive`), the summary
    // must not be mixed into it.
    let to_stdout = !verify_only && dir.is_none() && out.is_none();
    let print = |line: String| {
        if to_stdout {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    };
    let manifest = result.manifest.as_ref();
    print(format!(
        "restored {} from chain {}: {} in {} chunks (kind {}, taken {})",
        result.source,
        result.chain,
        human_bytes(result.plain_bytes),
        result.chunks,
        manifest.map(|m| m.kind.as_str()).unwrap_or(""),
        manifest
            .map(|m| m.created_at.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_default()
    ));
    if verify_only {
        print("the backup is intact: every chunk decrypted and matched the signed manifest".into());
    }
    Ok(())
}

async fn cmd_prune(
    receiver: String,
    identity: Option<String>,
    source: String,
    keep: usize,
) -> Result<(), String> {
    if keep < 1 {
        return Err("--keep must be at least 1".to_string());
    }
    let (id, _) = load_or_create(identity.as_deref(), None)?;
    let removed = client(&receiver, id).prune(&source, keep).await?;
    println!("kept the newest {keep} chains of {source}, removed {removed}");
    Ok(())
}
