//! The three ordered operations: Naslos -> Talos -> Kubernetes (D5).
//!
//! Each operation is idempotent enough for the Job's retry after a Talos
//! reboot: the entrypoint re-reads the running versions before doing anything
//! and treats "already at target" as success.

use crate::pack::PackMetadata;
use crate::semver::Version;
use crate::system::talos_installer_image;
use crate::upgrade::commands;
use crate::upgrade::status::Writer;
use crate::upgrade::Cli;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

const TALOS_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const HELM_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const KUBECTL_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(10 * 60);
// Authelia fatals when LDAP is not listening yet and restarts with container
// backoff; the old 5m window expired while OpenLDAP and Authelia were still
// settling on a real node (0.7.2 upgrade, 2026-11-23). The kubectl wrapper
// timeout is longer so the rollout timeout, not the wrapper, is authoritative.
const ROLLOUT_TIMEOUT: &str = "10m";

pub async fn naslos(cli: &Cli, writer: &mut Writer) -> Result<String, String> {
    writer
        .step(
            "pack",
            "running",
            "Downloading and verifying the install pack",
        )
        .await;
    let (meta, pack_dir) = download_and_extract(cli).await?;
    if meta.naslos_version != cli.version {
        return Err(format!(
            "pack naslosVersion {} != requested {}",
            meta.naslos_version, cli.version
        ));
    }
    let release = release_facts(&pack_dir)?;
    if release.profile != "installer" {
        return Err("the pack is not an installer-profile release; refusing to upgrade".into());
    }
    if Version::parse(&cli.from)
        .is_some_and(|from| Version::parse(&cli.version).is_some_and(|to| to <= from))
    {
        return Err(format!("{} is not newer than {}", cli.version, cli.from));
    }
    writer
        .step(
            "pack",
            "succeeded",
            &format!(
                "Verified pack {} ({})",
                meta.naslos_version,
                &cli.sha256[..12.min(cli.sha256.len())]
            ),
        )
        .await;

    writer
        .step("etcd-snapshot", "running", "Taking an etcd snapshot")
        .await;
    let snapshot = etcd_snapshot(cli).await?;
    writer
        .step(
            "etcd-snapshot",
            "succeeded",
            &format!("Snapshot written to {}", snapshot.display()),
        )
        .await;

    writer
        .step(
            "prerequisites",
            "running",
            "Converging local-path and cert-manager",
        )
        .await;
    converge_prerequisites(cli, &pack_dir).await?;
    writer
        .step("prerequisites", "succeeded", "Prerequisites are ready")
        .await;

    writer
        .step("helm", "running", "Upgrading the Naslos release")
        .await;
    let previous = helm_history_latest(cli).await.unwrap_or(0);
    let values = helm_current_values(cli).await?;
    let overrides = crate::upgrade::overrides::rebuild(&values)?;
    let overrides_path = Path::new(&cli.state_dir).join("upgrade-overrides.json");
    write_json(&overrides_path, &overrides)?;

    let chart = pack_dir.join("charts/naslos");
    let upgrade = helm_upgrade(cli, &chart, &overrides_path).await;
    if let Err(e) = upgrade {
        let rollback = rollback(cli, previous).await;
        return Err(match rollback {
            Ok(()) => format!("helm upgrade failed: {e}; rolled back to revision {previous}"),
            Err(r) => format!("helm upgrade failed: {e}; rollback failed: {r}"),
        });
    }
    writer
        .step("helm", "succeeded", "Helm upgraded the release")
        .await;

    writer
        .step(
            "workloads",
            "running",
            "Waiting for OpenLDAP, Authelia, API and UI",
        )
        .await;
    let waits = wait_for_workloads(cli).await;
    if let Err(e) = waits {
        let rollback = rollback(cli, previous).await;
        return Err(match rollback {
            Ok(()) => format!("{e}; rolled back to revision {previous}"),
            Err(r) => format!("{e}; rollback failed: {r}"),
        });
    }
    writer
        .step("workloads", "succeeded", "Core workloads are ready")
        .await;

    writer
        .step(
            "images",
            "running",
            "Checking the running images against the pack",
        )
        .await;
    // FR-UPD-06: a digest mismatch after the Helm upgrade is a failed upgrade
    // like any other, so it must take the same rollback path.
    if let Err(e) = verify_images(cli, &meta).await {
        let rollback = rollback(cli, previous).await;
        return Err(match rollback {
            Ok(()) => format!("{e}; rolled back to revision {previous}"),
            Err(r) => format!("{e}; rollback failed: {r}"),
        });
    }
    writer
        .step(
            "images",
            "succeeded",
            "Running images match the pack digests",
        )
        .await;
    Ok(format!("Naslos upgraded to {}", cli.version))
}

pub async fn talos(cli: &Cli, writer: &mut Writer) -> Result<String, String> {
    let (meta, _) = download_and_extract(cli).await?;
    let target = Version::parse(&meta.talos_version)
        .ok_or_else(|| format!("pack talosVersion {:?} is not semver", meta.talos_version))?;
    let current = current_talos_version_retry(cli).await?;
    writer.set_versions(&current, &meta.talos_version).await;
    let running = Version::parse(&current);

    if running.map(|v| v == target).unwrap_or(false) {
        writer
            .step(
                "verify",
                "succeeded",
                "Talos is already at the target version",
            )
            .await;
        align_install_image(cli, &meta).await?;
        wait_for_node(cli).await?;
        wait_for_agent(cli).await?;
        return Ok(format!("Talos is at {target}"));
    }
    if writer.attempts() > 0 {
        return Err(format!(
            "Talos is still {current} after a reboot; the upgrade rolled back to the previous slot"
        ));
    }
    if let Some(running) = running {
        if target < running {
            return Err(format!("Talos {target} is older than running {running}"));
        }
        if target.minor_distance(&running) > 1 {
            return Err(format!(
                "Talos {target} is more than one minor ahead of {running}"
            ));
        }
    }

    writer
        .step("etcd-snapshot", "running", "Taking an etcd snapshot")
        .await;
    let snapshot = etcd_snapshot(cli).await?;
    writer
        .step(
            "etcd-snapshot",
            "succeeded",
            &format!("Snapshot written to {}", snapshot.display()),
        )
        .await;

    let image = talos_installer_image(&meta.schematic_id, &meta.talos_version);
    writer
        .phase(
            "rebooting",
            &format!("Installing Talos {} and rebooting", meta.talos_version),
        )
        .await;
    // Non-blocking: the node (and this Job pod) reboots. A retry re-enters at
    // the top and either sees the target version or detects the A/B rollback.
    run_talos(
        cli,
        &[
            "upgrade".into(),
            "--image".into(),
            image.clone(),
            "--wait=false".into(),
            "--drain=false".into(),
        ],
        TALOS_TIMEOUT,
    )
    .await?;

    // If the node did not reboot fast enough to kill this pod, poll briefly.
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_secs(15)).await;
        if current_talos_version(cli)
            .await
            .ok()
            .and_then(|v| Version::parse(&v))
            == Some(target)
        {
            align_install_image(cli, &meta).await?;
            wait_for_node(cli).await?;
            wait_for_agent(cli).await?;
            return Ok(format!("Talos upgraded to {target}"));
        }
    }
    Err(format!(
        "Talos did not reach {} after the reboot",
        meta.talos_version
    ))
}

pub async fn kubernetes(cli: &Cli, writer: &mut Writer) -> Result<String, String> {
    let (meta, _) = download_and_extract(cli).await?;
    let target = Version::parse(&meta.kubernetes_version).ok_or_else(|| {
        format!(
            "pack kubernetesVersion {:?} is not semver",
            meta.kubernetes_version
        )
    })?;
    let talos = current_talos_version_retry(cli).await?;
    if Version::parse(&talos) != Version::parse(&meta.talos_version) {
        return Err(format!(
            "Talos is {talos}, the release pins {}; upgrade Talos first",
            meta.talos_version
        ));
    }
    let current = current_kubernetes_version(cli).await?;
    writer
        .set_versions(&current, &meta.kubernetes_version)
        .await;
    let running = Version::parse(&current);
    if running.map(|v| v == target).unwrap_or(false) {
        return Ok(format!("Kubernetes is already at {target}"));
    }
    if let Some(running) = running {
        if target < running {
            return Err(format!(
                "Kubernetes {target} is older than running {running}"
            ));
        }
        if target.minor_distance(&running) > 1 {
            return Err(format!(
                "Kubernetes {target} is more than one minor ahead of {running}"
            ));
        }
    }

    writer
        .step("etcd-snapshot", "running", "Taking an etcd snapshot")
        .await;
    let snapshot = etcd_snapshot(cli).await?;
    writer
        .step(
            "etcd-snapshot",
            "succeeded",
            &format!("Snapshot written to {}", snapshot.display()),
        )
        .await;

    writer
        .step("dry-run", "running", "Checking the upgrade-k8s plan")
        .await;
    run_talos(
        cli,
        &[
            "upgrade-k8s".into(),
            "--to".into(),
            meta.kubernetes_version.trim_start_matches('v').to_string(),
            "--dry-run".into(),
        ],
        KUBECTL_TIMEOUT,
    )
    .await?;
    writer
        .step("dry-run", "succeeded", "The upgrade plan is valid")
        .await;

    writer
        .step(
            "upgrade",
            "running",
            "Upgrading the Kubernetes control plane",
        )
        .await;
    run_talos(
        cli,
        &[
            "upgrade-k8s".into(),
            "--to".into(),
            meta.kubernetes_version.trim_start_matches('v').to_string(),
        ],
        Duration::from_secs(45 * 60),
    )
    .await?;
    writer
        .step("upgrade", "succeeded", "upgrade-k8s completed")
        .await;

    let after = current_kubernetes_version(cli).await?;
    if Version::parse(&after) != Some(target) {
        return Err(format!(
            "the API reports Kubernetes {after}, expected {}",
            meta.kubernetes_version
        ));
    }
    let kubelet = current_kubelet_version(cli).await?;
    if Version::parse(&kubelet) != Some(target) {
        return Err(format!(
            "the kubelet reports {kubelet}, expected {}; see docs/operations.md manual recovery",
            meta.kubernetes_version
        ));
    }
    Ok(format!("Kubernetes upgraded to {target}"))
}

async fn download_and_extract(cli: &Cli) -> Result<(PackMetadata, PathBuf), String> {
    // R-063: the URL is https-only and its host must be the configured update
    // source or a GitHub asset host. The sha256 is compared inside the Job
    // (never trusted from the API response), and an available signature is
    // verified against the configured key.
    let source = std::env::var("SYSTEM_UPDATES_SOURCE").unwrap_or_default();
    if cli.sha256.len() != 64 || !cli.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("--sha256 must be 64 hex characters".into());
    }
    // C-1c: the API verifies and caches the pack on the shared state volume
    // before it creates this Job, and the Job mounts the same claim at the
    // same path. Prefer that file: the namespace default-deny has no egress
    // for the Job except the fallback policy, and a Talos reboot retry must
    // not depend on a second download. A cached file that is present but does
    // not match the approved digest fails closed instead of falling back.
    let bytes = match read_cached_file(&cli.pack_file, "pack")? {
        Some(bytes) => {
            let got = crate::pack::hex_sha256(&bytes);
            if !got.eq_ignore_ascii_case(&cli.sha256) {
                return Err(format!(
                    "the cached pack {} does not match the approved sha256 (got {got}, approved {})",
                    cli.pack_file, cli.sha256
                ));
            }
            bytes
        }
        None => {
            // Fallback: the URL path. `download_capped` enforces the byte cap,
            // and the client re-validates every redirect hop.
            crate::system::pack_url_allowed(&cli.pack_url, &source)?;
            let client = crate::system::pack_client(Duration::from_secs(600), &source)?;
            crate::system::download_capped(
                &client,
                &cli.pack_url,
                "pack",
                crate::upgrade::MAX_PACK_BYTES,
                "naslos-upgrade",
            )
            .await?
        }
    };
    let got = crate::pack::hex_sha256(&bytes);
    if !got.eq_ignore_ascii_case(&cli.sha256) {
        return Err(format!(
            "pack sha256 mismatch: got {got}, approved {}",
            cli.sha256
        ));
    }
    // Signature first from the cached copy (no network), then from the release
    // URL as the fallback. The (url, key) combinations stay as strict as the
    // update check: a published signature requires a key, and a configured key
    // requires a signature.
    let signature = match read_cached_file(&cli.pack_sig_file, "pack signature")? {
        Some(bytes) => Some(
            String::from_utf8(bytes)
                .map_err(|_| "the cached pack signature is not UTF-8".to_string())?,
        ),
        None if cli.pack_sig_url.is_empty() => None,
        None => {
            crate::system::pack_url_allowed(&cli.pack_sig_url, &source)?;
            let client = crate::system::pack_client(Duration::from_secs(600), &source)?;
            Some(
                String::from_utf8(
                    crate::system::download_capped(
                        &client,
                        &cli.pack_sig_url,
                        "pack signature",
                        crate::upgrade::MAX_PACK_BYTES,
                        "naslos-upgrade",
                    )
                    .await?,
                )
                .map_err(|_| "the pack signature is not UTF-8".to_string())?,
            )
        }
    };
    match (signature.as_deref(), cli.pack_pubkey.is_empty()) {
        (Some(signature), false) => {
            crate::upgrade::verify_pack_signature(&bytes, signature, &cli.pack_pubkey)?;
        }
        (Some(_), true) => {
            return Err("a pack signature is available but no --pack-pubkey is configured".into())
        }
        (None, false) => {
            return Err("a pack public key is configured but no pack signature is available".into())
        }
        (None, true) => {}
    }
    let dest = Path::new(&cli.state_dir)
        .join("updates")
        .join(format!("pack-{}", cli.version));
    if dest.exists() {
        let _ = std::fs::remove_dir_all(&dest);
    }
    std::fs::create_dir_all(&dest).map_err(|e| format!("creating {}: {e}", dest.display()))?;
    let meta = crate::pack::extract_pack(&bytes, &dest)?;
    Ok((meta, dest))
}

/// Read a file the API cached on the shared state volume. A missing file is
/// `None` (the caller falls back to its URL); an unreadable or oversized file
/// is an error, because silently downloading instead could mask corruption.
/// The read itself is capped, so a file that grows after the metadata check
/// still cannot exhaust memory.
fn read_cached_file(path: &str, what: &str) -> Result<Option<Vec<u8>>, String> {
    use std::io::Read;

    if path.is_empty() {
        return Ok(None);
    }
    let path = Path::new(path);
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("reading the cached {what} {}: {e}", path.display())),
    };
    let mut bytes = Vec::new();
    file.take(crate::upgrade::MAX_PACK_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("reading the cached {what} {}: {e}", path.display()))?;
    if bytes.len() > crate::upgrade::MAX_PACK_BYTES {
        return Err(format!(
            "the cached {what} {} is larger than the {} byte cap",
            path.display(),
            crate::upgrade::MAX_PACK_BYTES
        ));
    }
    Ok(Some(bytes))
}

struct ReleaseFacts {
    profile: String,
}

fn release_facts(pack_dir: &Path) -> Result<ReleaseFacts, String> {
    let path = pack_dir.join("charts/naslos/values-installer.yaml");
    let raw =
        std::fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let value: serde_yaml::Value =
        serde_yaml::from_str(&raw).map_err(|e| format!("parsing {}: {e}", path.display()))?;
    let profile = value
        .get("release")
        .and_then(|r| r.get("profile"))
        .and_then(|p| p.as_str())
        .unwrap_or("")
        .to_string();
    Ok(ReleaseFacts { profile })
}

async fn converge_prerequisites(cli: &Cli, pack_dir: &Path) -> Result<(), String> {
    let local_path = pack_dir.join("manifests/local-path-v0.0.26.yaml");
    kubectl_apply(cli, &local_path).await?;
    let crds = pack_dir.join("manifests/cert-manager-crds.yaml");
    kubectl_apply(cli, &crds).await?;

    let cert_manager = single_chart(&pack_dir.join("charts/naslos/charts"), "cert-manager-")?;
    helm_upgrade_install(
        cli,
        "cert-manager",
        &cert_manager,
        "cert-manager",
        &["--set", "crds.enabled=false"],
    )
    .await?;
    let webhook = single_chart(&pack_dir.join("charts"), "cert-manager-webhook-ovh-")?;
    helm_upgrade_install(
        cli,
        "cert-manager-webhook-ovh",
        &webhook,
        "cert-manager",
        &[
            "--set",
            "groupName=ovh.naslos.local",
            "--set",
            "certManager.namespace=cert-manager",
            "--set",
            "certManager.serviceAccountName=cert-manager",
        ],
    )
    .await?;

    for (namespace, name) in [
        ("cert-manager", "cert-manager"),
        ("cert-manager", "cert-manager-cainjector"),
        ("cert-manager", "cert-manager-webhook"),
        ("cert-manager", "cert-manager-webhook-ovh"),
    ] {
        wait_rollout(cli, namespace, "deployment", name).await?;
    }
    Ok(())
}

fn single_chart(dir: &Path, prefix: &str) -> Result<PathBuf, String> {
    let mut matches: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("reading {}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with(prefix) && n.ends_with(".tgz"))
                .unwrap_or(false)
        })
        .collect();
    matches.sort();
    match matches.len() {
        1 => Ok(matches.remove(0)),
        n => Err(format!(
            "expected exactly one {prefix}*.tgz in {}, found {n}",
            dir.display()
        )),
    }
}

async fn helm_upgrade_install(
    cli: &Cli,
    release: &str,
    chart: &Path,
    namespace: &str,
    extra: &[&str],
) -> Result<(), String> {
    let mut args = vec![
        "upgrade".into(),
        "--install".into(),
        release.into(),
        chart.display().to_string(),
        "-n".into(),
        namespace.into(),
        "--create-namespace".into(),
        "--reset-values".into(),
        "--wait=false".into(),
        "--skip-schema-validation".into(),
        "--timeout".into(),
        "10m".into(),
    ];
    args.extend(extra.iter().map(|s| s.to_string()));
    run_helm(cli, &args, HELM_TIMEOUT).await.map(|_| ())
}

async fn kubectl_apply(cli: &Cli, path: &Path) -> Result<(), String> {
    run_kubectl(
        cli,
        &[
            "apply".into(),
            "--server-side".into(),
            "--force-conflicts".into(),
            "--field-manager".into(),
            "naslos-upgrade".into(),
            "-f".into(),
            path.display().to_string(),
        ],
        KUBECTL_TIMEOUT,
    )
    .await
    .map(|_| ())
}

async fn helm_current_values(cli: &Cli) -> Result<Value, String> {
    let out = run_helm(
        cli,
        &[
            "get".into(),
            "values".into(),
            cli.release.clone(),
            "-n".into(),
            cli.namespace.clone(),
            "-o".into(),
            "json".into(),
        ],
        KUBECTL_TIMEOUT,
    )
    .await?;
    serde_json::from_str(out.trim()).map_err(|e| format!("parsing helm values: {e}"))
}

async fn helm_history_latest(cli: &Cli) -> Result<i64, String> {
    let out = run_helm(
        cli,
        &[
            "history".into(),
            cli.release.clone(),
            "-n".into(),
            cli.namespace.clone(),
            "-o".into(),
            "json".into(),
        ],
        KUBECTL_TIMEOUT,
    )
    .await?;
    let history: Vec<Value> = serde_json::from_str(out.trim()).unwrap_or_default();
    Ok(history
        .iter()
        .filter_map(|r| r.get("revision").and_then(Value::as_i64))
        .max()
        .unwrap_or(0))
}

fn helm_upgrade_args(
    release: &str,
    chart: &Path,
    namespace: &str,
    overrides: &Path,
) -> Vec<String> {
    vec![
        "upgrade".into(),
        release.into(),
        chart.display().to_string(),
        "-n".into(),
        namespace.into(),
        "-f".into(),
        chart.join("values.yaml").display().to_string(),
        "-f".into(),
        chart.join("values-installer.yaml").display().to_string(),
        "-f".into(),
        overrides.display().to_string(),
        // The release stores the installer's fully merged values, including
        // old image digests. Reset to chart defaults + this pack's values so
        // every image and non-engine value comes from the new release.
        "--reset-values".into(),
        "--wait=false".into(),
        "--skip-schema-validation".into(),
        // The release was first installed by the desktop engine
        // (`naslos-install.exe`), whose server-side apply owns many fields.
        // Without this the upgrade fails with "Apply failed with 1 conflict"
        // on every field this release changes (network policies, RBAC,
        // Traefik args, Authelia env) and rolls back to the previous revision.
        "--force-conflicts".into(),
        "--timeout".into(),
        "10m".into(),
    ]
}

async fn helm_upgrade(cli: &Cli, chart: &Path, overrides: &Path) -> Result<(), String> {
    let args = helm_upgrade_args(&cli.release, chart, &cli.namespace, overrides);
    run_helm(cli, &args, HELM_TIMEOUT).await.map(|_| ())
}

async fn rollback(cli: &Cli, revision: i64) -> Result<(), String> {
    if revision <= 0 {
        return Err("no previous revision to roll back to".into());
    }
    run_helm(
        cli,
        &[
            "rollback".into(),
            cli.release.clone(),
            revision.to_string(),
            "-n".into(),
            cli.namespace.clone(),
            "--force-conflicts".into(),
            "--wait".into(),
            "--timeout".into(),
            "10m".into(),
        ],
        HELM_TIMEOUT,
    )
    .await
    .map(|_| ())
}

async fn wait_for_workloads(cli: &Cli) -> Result<(), String> {
    let workload_ns =
        std::env::var("NASLOS_WORKLOAD_NAMESPACE").unwrap_or_else(|_| "naslos-privileged".into());
    for (namespace, kind, name) in [
        (cli.namespace.as_str(), "statefulset", "naslos-openldap"),
        (cli.namespace.as_str(), "statefulset", "naslos-authelia"),
        (cli.namespace.as_str(), "deployment", "naslos-api"),
        (cli.namespace.as_str(), "deployment", "naslos-ui"),
        // The terminal is in the same image-check set below; waiting for it
        // too keeps the two lists identical, so a Recreate window (its
        // strategy) cannot make the image check see no running pod and fail a
        // good upgrade. A workload the chart did not render is skipped by
        // `wait_rollout`, like every optional workload here.
        (workload_ns.as_str(), "deployment", "naslos-terminal"),
    ] {
        wait_rollout(cli, namespace, kind, name).await?;
    }
    // I-5: the privileged DaemonSets carry the pack's digests too, so a
    // successful rollout there is part of the workload gate (and the source
    // for the running-image check below).
    for name in ["naslos-agent", "naslos-samba", "naslos-nfs"] {
        wait_rollout(cli, &workload_ns, "daemonset", name).await?;
    }
    Ok(())
}

async fn wait_rollout(cli: &Cli, namespace: &str, kind: &str, name: &str) -> Result<(), String> {
    match run_kubectl(
        cli,
        &[
            "rollout".into(),
            "status".into(),
            format!("{kind}/{name}"),
            "-n".into(),
            namespace.into(),
            format!("--timeout={ROLLOUT_TIMEOUT}"),
        ],
        KUBECTL_TIMEOUT,
    )
    .await
    {
        Ok(_) => Ok(()),
        // The chart renders SMB, NFS, the terminal and the DaemonSets
        // conditionally. A workload that is not part of this install has no
        // rollout to wait for; treating that as a failure rolled back a good
        // upgrade (I-5 follow-up).
        Err(e) if e.contains("NotFound") || e.contains("not found") => Ok(()),
        Err(e) => Err(e),
    }
}

async fn wait_for_node(cli: &Cli) -> Result<(), String> {
    run_kubectl(
        cli,
        &[
            "wait".into(),
            "--for=condition=Ready".into(),
            "node".into(),
            "--all".into(),
            "--timeout=10m".into(),
        ],
        KUBECTL_TIMEOUT,
    )
    .await
    .map(|_| ())
}

async fn wait_for_agent(cli: &Cli) -> Result<(), String> {
    let namespace = std::env::var("NASLOS_WORKLOAD_NAMESPACE")
        .unwrap_or_else(|_| "naslos-privileged".to_string());
    wait_rollout(cli, &namespace, "daemonset", "naslos-agent").await
}

async fn align_install_image(cli: &Cli, meta: &PackMetadata) -> Result<(), String> {
    let image = meta.talos_installer_image();
    let patch = serde_json::json!({"machine": {"install": {"image": image}}}).to_string();
    run_talos(
        cli,
        &[
            "patch".into(),
            "machineconfig".into(),
            "--patch".into(),
            patch,
            "--mode".into(),
            "no-reboot".into(),
        ],
        TALOS_TIMEOUT,
    )
    .await
    .map(|_| ())
}

async fn etcd_snapshot(cli: &Cli) -> Result<PathBuf, String> {
    let dir = Path::new(&cli.state_dir).join("backups");
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let path = dir.join(format!("etcd-{stamp}.db"));
    run_talos(
        cli,
        &["etcd".into(), "snapshot".into(), path.display().to_string()],
        SNAPSHOT_TIMEOUT,
    )
    .await?;
    prune_snapshots(&dir, 3)?;
    Ok(path)
}

fn prune_snapshots(dir: &Path, keep: usize) -> Result<(), String> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("reading {}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("etcd-") && n.ends_with(".db"))
                .unwrap_or(false)
        })
        .collect();
    files.sort();
    while files.len() > keep {
        let oldest = files.remove(0);
        let _ = std::fs::remove_file(oldest);
    }
    Ok(())
}

async fn verify_images(cli: &Cli, meta: &PackMetadata) -> Result<(), String> {
    let workload_ns =
        std::env::var("NASLOS_WORKLOAD_NAMESPACE").unwrap_or_else(|_| "naslos-privileged".into());
    let targets = [
        ("api", "deployment", "naslos-api", cli.namespace.as_str()),
        ("ui", "deployment", "naslos-ui", cli.namespace.as_str()),
        (
            "openldap",
            "statefulset",
            "naslos-openldap",
            cli.namespace.as_str(),
        ),
        ("agent", "daemonset", "naslos-agent", workload_ns.as_str()),
        ("samba", "daemonset", "naslos-samba", workload_ns.as_str()),
        ("nfs", "daemonset", "naslos-nfs", workload_ns.as_str()),
        (
            "terminal",
            "deployment",
            "naslos-terminal",
            workload_ns.as_str(),
        ),
    ];
    for (component, kind, name, namespace) in targets {
        let Some(expected) = meta.images.get(component) else {
            continue;
        };
        if !expected.contains("@sha256:") {
            continue;
        }
        // Verify the pods that are actually running, not the pod template: a
        // rollout can lag a template edit, and FR-UPD-06 is about what serves.
        let workload = match run_kubectl(
            cli,
            &[
                "get".into(),
                kind.into(),
                name.into(),
                "-n".into(),
                namespace.into(),
                "-o".into(),
                "json".into(),
            ],
            KUBECTL_TIMEOUT,
        )
        .await
        {
            Ok(out) => out,
            Err(e) if e.contains("NotFound") || e.contains("not found") => continue,
            Err(e) => return Err(e),
        };
        let workload: Value = serde_json::from_str(workload.trim())
            .map_err(|e| format!("parsing {kind}/{name}: {e}"))?;
        let selector = workload
            .pointer("/spec/selector/matchLabels")
            .and_then(Value::as_object)
            .map(|labels| {
                labels
                    .iter()
                    .filter_map(|(key, value)| value.as_str().map(|v| format!("{key}={v}")))
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();
        if selector.is_empty() {
            return Err(format!("{kind}/{name} has no matchLabels selector"));
        }
        let out = run_kubectl(
            cli,
            &[
                "get".into(),
                "pods".into(),
                "-n".into(),
                namespace.into(),
                "-l".into(),
                selector,
                "-o".into(),
                "json".into(),
            ],
            KUBECTL_TIMEOUT,
        )
        .await?;
        let pods: Value = serde_json::from_str(out.trim())
            .map_err(|e| format!("parsing {kind}/{name} pods: {e}"))?;
        let running: Vec<&Value> = pods
            .get("items")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter(|pod| is_live_running_pod(pod))
                    .collect()
            })
            .unwrap_or_default();
        if running.is_empty() {
            return Err(format!("{kind}/{name} has no running pods"));
        }
        // Only the component's own repository is compared: a pod may carry a
        // legitimate sidecar, but a stale digest of the same repository is the
        // drift the pack check must catch.
        let repository = expected.split('@').next().unwrap_or(expected);
        for pod in running {
            let pod_name = pod
                .pointer("/metadata/name")
                .and_then(Value::as_str)
                .unwrap_or("<unnamed>");
            let images = pod
                .pointer("/spec/containers")
                .and_then(Value::as_array)
                .map(|containers| {
                    containers
                        .iter()
                        .filter_map(|c| c.get("image").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if !images.iter().any(|image| *image == expected) {
                return Err(format!(
                    "{kind}/{name} pod {pod_name} runs {images:?}, the pack pins {expected}"
                ));
            }
            if let Some(stale) = images
                .iter()
                .find(|image| image.split('@').next() == Some(repository) && **image != expected)
            {
                return Err(format!(
                    "{kind}/{name} pod {pod_name} still runs {stale}, the pack pins {expected}"
                ));
            }
        }
    }
    Ok(())
}

/// Whether a pod is a live replica the image check should compare. A
/// terminating pod (`deletionTimestamp` set) is being replaced by the rollout,
/// so its old digest is not drift; this is the same live-pod rule the app
/// image check uses (FR-APP-27).
fn is_live_running_pod(pod: &Value) -> bool {
    pod.pointer("/status/phase").and_then(Value::as_str) == Some("Running")
        && !pod
            .pointer("/metadata/deletionTimestamp")
            .is_some_and(|stamp| !stamp.is_null())
}

/// After a reboot the Talos API can lag the kubelet; retry briefly instead of
/// failing the operation on the first refused connection.
async fn current_talos_version_retry(cli: &Cli) -> Result<String, String> {
    let mut last = String::new();
    for _ in 0..12 {
        match current_talos_version(cli).await {
            Ok(version) => return Ok(version),
            Err(e) => {
                last = e;
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
    Err(last)
}

async fn current_talos_version(cli: &Cli) -> Result<String, String> {
    let out = run_talos(
        cli,
        &["get".into(), "version".into(), "-o".into(), "json".into()],
        TALOS_TIMEOUT,
    )
    .await?;
    crate::talos::first_json(&out)
        .and_then(|v| {
            v.get("spec")
                .and_then(|s| s.get("version"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .ok_or_else(|| "talosctl get version returned no spec.version".to_string())
}

async fn current_kubelet_version(cli: &Cli) -> Result<String, String> {
    let out = run_kubectl(
        cli,
        &["get".into(), "nodes".into(), "-o".into(), "json".into()],
        KUBECTL_TIMEOUT,
    )
    .await?;
    let value: Value =
        serde_json::from_str(out.trim()).map_err(|e| format!("parsing kubectl nodes: {e}"))?;
    value
        .pointer("/items/0/status/nodeInfo/kubeletVersion")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "kubectl get nodes returned no kubeletVersion".to_string())
}

async fn current_kubernetes_version(cli: &Cli) -> Result<String, String> {
    let out = run_kubectl(
        cli,
        &["version".into(), "-o".into(), "json".into()],
        KUBECTL_TIMEOUT,
    )
    .await?;
    let value: Value =
        serde_json::from_str(out.trim()).map_err(|e| format!("parsing kubectl version: {e}"))?;
    value
        .pointer("/serverVersion/gitVersion")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "kubectl version returned no serverVersion.gitVersion".to_string())
}

/// `talosctl` gets the mounted talosconfig plus the node list from the Job
/// environment. The mounted file has endpoints but no nodes, and
/// `etcd snapshot`/`upgrade` refuse to run without `--nodes`.
fn talos_args(talosconfig: &str, nodes: &str, args: &[String]) -> Vec<String> {
    let mut full = vec!["--talosconfig".to_string(), talosconfig.to_string()];
    let nodes = nodes.trim();
    if !nodes.is_empty() {
        full.push("--nodes".to_string());
        full.push(nodes.to_string());
    }
    full.extend(args.iter().cloned());
    full
}

async fn run_talos(cli: &Cli, args: &[String], timeout: Duration) -> Result<String, String> {
    let nodes = std::env::var("TALOS_NODES").unwrap_or_default();
    let full = talos_args(&cli.talosconfig, &nodes, args);
    commands::run(&commands::talosctl(), &full, timeout).await
}

async fn run_kubectl(_cli: &Cli, args: &[String], timeout: Duration) -> Result<String, String> {
    let mut full = kubeconfig_args();
    full.extend(args.iter().cloned());
    commands::run(&commands::kubectl(), &full, timeout).await
}

async fn run_helm(_cli: &Cli, args: &[String], timeout: Duration) -> Result<String, String> {
    let mut full = kubeconfig_args();
    full.extend(args.iter().cloned());
    commands::run(&commands::helm(), &full, timeout).await
}

fn kubeconfig_args() -> Vec<String> {
    match commands::kubeconfig() {
        Some(kc) => vec!["--kubeconfig".into(), kc],
        None => Vec::new(),
    }
}

fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    let raw = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    std::fs::write(path, raw).map_err(|e| format!("writing {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command() -> Vec<String> {
        vec![
            "etcd".into(),
            "snapshot".into(),
            "/var/lib/naslos/backups/etcd.db".into(),
        ]
    }

    #[test]
    fn talos_args_put_nodes_after_the_talosconfig() {
        let full = talos_args(
            "/var/run/secrets/talos.dev/config",
            "10.0.0.1,10.0.0.2",
            &command(),
        );
        assert_eq!(
            full,
            [
                "--talosconfig",
                "/var/run/secrets/talos.dev/config",
                "--nodes",
                "10.0.0.1,10.0.0.2",
                "etcd",
                "snapshot",
                "/var/lib/naslos/backups/etcd.db",
            ]
        );
    }

    #[test]
    fn talos_args_omit_nodes_without_a_node_list() {
        let full = talos_args("/var/run/secrets/talos.dev/config", "  ", &command());
        assert!(!full.iter().any(|arg| arg == "--nodes"));
        assert_eq!(full[0], "--talosconfig");
        assert_eq!(full[2], "etcd");
    }

    #[test]
    fn terminating_pods_are_not_live_replicas() {
        let running = serde_json::json!({"status": {"phase": "Running"}});
        assert!(is_live_running_pod(&running));
        let terminating = serde_json::json!({
            "metadata": {"deletionTimestamp": "2026-01-01T00:00:00Z"},
            "status": {"phase": "Running"},
        });
        assert!(!is_live_running_pod(&terminating));
        let explicit_null = serde_json::json!({
            "metadata": {"deletionTimestamp": null},
            "status": {"phase": "Running"},
        });
        assert!(is_live_running_pod(&explicit_null));
        let pending = serde_json::json!({"status": {"phase": "Pending"}});
        assert!(!is_live_running_pod(&pending));
    }

    #[test]
    fn the_rollout_window_covers_the_authelia_ldap_backoff() {
        assert_eq!(ROLLOUT_TIMEOUT, "10m");
        assert!(
            KUBECTL_TIMEOUT > Duration::from_secs(10 * 60),
            "the kubectl wrapper must outlive the rollout timeout"
        );
    }

    #[test]
    fn the_platform_upgrade_forces_apply_conflicts() {
        let args = helm_upgrade_args(
            "naslos",
            Path::new("/var/lib/naslos/updates/pack-0.7.2/charts/naslos"),
            "naslos",
            Path::new("/var/lib/naslos/upgrade-overrides.json"),
        );
        assert!(args.contains(&"--force-conflicts".to_string()), "{args:?}");
        assert!(args.contains(&"--reset-values".to_string()), "{args:?}");
        assert_eq!(args.first().map(String::as_str), Some("upgrade"));
    }
}
