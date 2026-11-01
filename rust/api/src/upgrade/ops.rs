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
const KUBECTL_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(10 * 60);

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
    verify_images(cli, &meta).await?;
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
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|e| format!("building the download client: {e}"))?;
    let resp = client
        .get(&cli.pack_url)
        .header("User-Agent", "naslos-upgrade")
        .send()
        .await
        .map_err(|e| format!("GET {}: {e}", cli.pack_url))?;
    if !resp.status().is_success() {
        return Err(format!("GET {}: HTTP {}", cli.pack_url, resp.status()));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("reading the pack: {e}"))?;
    let got = crate::pack::hex_sha256(&bytes);
    if !got.eq_ignore_ascii_case(&cli.sha256) {
        return Err(format!(
            "pack sha256 mismatch: got {got}, approved {}",
            cli.sha256
        ));
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

async fn helm_upgrade(cli: &Cli, chart: &Path, overrides: &Path) -> Result<(), String> {
    run_helm(
        cli,
        &[
            "upgrade".into(),
            cli.release.clone(),
            chart.display().to_string(),
            "-n".into(),
            cli.namespace.clone(),
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
            "--timeout".into(),
            "10m".into(),
        ],
        HELM_TIMEOUT,
    )
    .await
    .map(|_| ())
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
    for (namespace, kind, name) in [
        (cli.namespace.as_str(), "statefulset", "naslos-openldap"),
        (cli.namespace.as_str(), "statefulset", "naslos-authelia"),
        (cli.namespace.as_str(), "deployment", "naslos-api"),
        (cli.namespace.as_str(), "deployment", "naslos-ui"),
    ] {
        wait_rollout(cli, namespace, kind, name).await?;
    }
    Ok(())
}

async fn wait_rollout(cli: &Cli, namespace: &str, kind: &str, name: &str) -> Result<(), String> {
    run_kubectl(
        cli,
        &[
            "rollout".into(),
            "status".into(),
            format!("{kind}/{name}"),
            "-n".into(),
            namespace.into(),
            "--timeout=5m".into(),
        ],
        KUBECTL_TIMEOUT,
    )
    .await
    .map(|_| ())
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
        let out = match run_kubectl(
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
        let value: Value =
            serde_json::from_str(out.trim()).map_err(|e| format!("parsing {kind}/{name}: {e}"))?;
        let images = value
            .pointer("/spec/template/spec/containers")
            .and_then(Value::as_array)
            .map(|containers| {
                containers
                    .iter()
                    .filter_map(|c| c.get("image").and_then(Value::as_str))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if !images.iter().any(|image| image == expected) {
            return Err(format!(
                "{kind}/{name} runs {images:?}, the pack pins {expected}"
            ));
        }
    }
    Ok(())
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

async fn run_talos(cli: &Cli, args: &[String], timeout: Duration) -> Result<String, String> {
    let mut full = vec!["--talosconfig".to_string(), cli.talosconfig.clone()];
    full.extend(args.iter().cloned());
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
