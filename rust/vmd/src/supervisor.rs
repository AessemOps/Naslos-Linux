//! The VM supervisor: vmd's reconciler between the desired config the API
//! pushed and the QEMU processes actually running.
//!
//! Invariants (mirrors the shares apply path):
//! - the API is the brain; vmd only applies what it validated itself too
//!   (SEC-1: a bad render never reaches a command line),
//! - one vmd pod per node, so two supervisors never fight over QEMU
//!   processes or VNC ports (`Recreate` strategy),
//! - `state` is always derived here, never persisted.

use crate::args::{detect_firmware, qemu_args, BuildContext};
use crate::cloudinit;
use crate::model::{validate_vm, vm_dir, DesiredConfig, Firmware, VmLive, VmSpec};
use crate::qmp::Qmp;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

/// How long a graceful stop waits before SIGTERM.
const GRACEFUL_STOP_SECS: u64 = 30;
/// How long SIGTERM waits before SIGKILL.
const TERM_STOP_SECS: u64 = 10;
/// How long a fresh VM's dataset dir is awaited (mount-propagation lag).
const DATASET_WAIT_SECS: u64 = 60;
/// VNC passwords are truncated to 8 characters by the RFB spec.
const VNC_PASSWORD_LEN: usize = 8;

struct Running {
    child: tokio::process::Child,
    spec: VmSpec,
    vnc_display: u16,
    vnc_password: String,
    started_at: std::time::Instant,
    exited: Option<std::process::ExitStatus>,
    /// The stop was requested by the reconciler (not an unexpected exit).
    stopping: bool,
    last_error: String,
    unexpected_exit: bool,
}

#[derive(Default)]
struct State {
    desired: DesiredConfig,
    /// The last applied config revision: identical re-pushes are no-ops.
    last_revision: String,
    running: HashMap<String, Running>,
    /// Last reported live state for VMs that are not running (or were removed
    /// from the desired config), so the API can still see exits and errors.
    ghosts: HashMap<String, VmLive>,
}

pub struct Supervisor {
    kvm: bool,
    vnc_first_display: u16,
    vnc_display_count: u16,
    run_dir: PathBuf,
    firmware: (Option<PathBuf>, Option<PathBuf>),
    state: Mutex<State>,
}

impl Supervisor {
    pub fn new(
        kvm: bool,
        vnc_first_display: u16,
        vnc_display_count: u16,
        run_dir: PathBuf,
    ) -> Self {
        Self {
            kvm,
            vnc_first_display,
            vnc_display_count,
            run_dir,
            firmware: detect_firmware(),
            state: Mutex::new(State {
                desired: DesiredConfig::default(),
                last_revision: String::new(),
                running: HashMap::new(),
                ghosts: HashMap::new(),
            }),
        }
    }

    /// Accept a new desired config, validate every VM, install it, and kick
    /// off a detached reconcile. Taking `Arc<Self>` is what allows the
    /// reconcile to outlive this request: axum drops the handler future when
    /// its client disconnects (a browser or API timeout), and a synchronous
    /// reconcile died with it mid-start/mid-stop — a cancelled stop left the
    /// QEMU process running with the desired state already recorded, and a
    /// cancelled first start left the revision installed so every later
    /// re-push was a no-op. Reconcile errors are recorded per VM in the live
    /// view (ghosts) instead of being returned to the caller.
    pub async fn apply(self: Arc<Self>, config: DesiredConfig) -> Result<(), String> {
        let mut seen = std::collections::HashSet::new();
        let mut host_ports = std::collections::HashSet::new();
        for vm in &config.vms {
            validate_vm(vm)?;
            if !seen.insert(vm.name.clone()) {
                return Err(format!("duplicate VM name {:?}", vm.name));
            }
            for fwd in &vm.net.forwards {
                if !host_ports.insert(fwd.host) {
                    return Err(format!(
                        "host port {} is forwarded by more than one VM",
                        fwd.host
                    ));
                }
            }
        }
        {
            let mut state = self.state.lock().await;
            if !config.revision.is_empty() && state.last_revision == config.revision {
                // The API re-renders the same config on watcher recoveries;
                // an identical revision is a no-op.
                return Ok(());
            }
            state.last_revision = config.revision.clone();
            state.desired = config;
        }
        let supervisor = Arc::clone(&self);
        tokio::spawn(async move {
            if let Err(errors) = supervisor.reconcile().await {
                tracing::warn!("reconcile errors: {errors}");
            }
        });
        Ok(())
    }

    /// Diff desired against running: start, stop and recreate as needed.
    pub async fn reconcile(&self) -> Result<(), String> {
        // Snapshot the desired list without holding the lock across the
        // per-VM work.
        let desired: Vec<VmSpec> = self.state.lock().await.desired.vms.clone();
        let mut errors: Vec<String> = Vec::new();

        for vm in &desired {
            match vm.desired {
                crate::model::DesiredState::Running => {
                    let needs_recreate = {
                        let state = self.state.lock().await;
                        match state.running.get(&vm.name) {
                            Some(r) => !running_matches(r, vm) || r.exited.is_some(),
                            None => false,
                        }
                    };
                    if needs_recreate {
                        if let Err(e) = self.stop_one(&vm.name, true).await {
                            errors.push(format!("recreating VM {}: {e}", vm.name));
                            continue;
                        }
                    }
                    let already = {
                        let state = self.state.lock().await;
                        state
                            .running
                            .get(&vm.name)
                            .map(|r| r.exited.is_none())
                            .unwrap_or(false)
                    };
                    if !already {
                        if let Err(e) = self.start(vm).await {
                            errors.push(format!("starting VM {}: {e}", vm.name));
                            let mut state = self.state.lock().await;
                            state
                                .ghosts
                                .entry(vm.name.clone())
                                .and_modify(|g| {
                                    g.state = "stopped".into();
                                    g.last_error.clone_from(&e);
                                })
                                .or_insert_with(|| VmLive {
                                    name: vm.name.clone(),
                                    state: "stopped".into(),
                                    last_error: e.clone(),
                                    ..Default::default()
                                });
                        }
                    }
                }
                crate::model::DesiredState::Stopped => {
                    let running = {
                        let state = self.state.lock().await;
                        state
                            .running
                            .get(&vm.name)
                            .map(|r| r.exited.is_none())
                            .unwrap_or(false)
                    };
                    if running {
                        if let Err(e) = self.stop_one(&vm.name, false).await {
                            errors.push(format!("stopping VM {}: {e}", vm.name));
                        }
                    }
                }
            }
        }

        // VMs removed from the config are stopped and forgotten.
        let mut to_remove: Vec<String> = Vec::new();
        {
            let state = self.state.lock().await;
            for name in state.running.keys() {
                if !desired.iter().any(|vm| &vm.name == name) {
                    to_remove.push(name.clone());
                }
            }
        }
        for name in &to_remove {
            if let Err(e) = self.stop_one(name, true).await {
                errors.push(format!("stopping removed VM {name}: {e}"));
            }
        }

        self.poll().await;
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    /// Force-stop one VM (the force-stop action): QEMU quit, then signals.
    pub async fn force_stop(&self, name: &str) -> Result<(), String> {
        let exists = self.state.lock().await.running.contains_key(name);
        if !exists {
            return Err(format!("VM {name:?} is not running"));
        }
        self.stop_one(name, true).await
    }

    /// Reap exited children, refresh per-VM state, and park ghosts.
    pub async fn poll(&self) {
        {
            let mut state = self.state.lock().await;
            let mut reaped: Vec<String> = Vec::new();
            for (name, running) in state.running.iter_mut() {
                if running.exited.is_some() {
                    continue;
                }
                if let Ok(Some(status)) = running.child.try_wait() {
                    running.exited = Some(status);
                    if !running.stopping {
                        running.unexpected_exit = true;
                        running.last_error = format!(
                            "QEMU exited unexpectedly with {status}; last log line: {}",
                            tail_qemu_log(&vm_dir(&running.spec.pool, name))
                        );
                        tracing::warn!("VM {name} exited unexpectedly: {status}");
                    } else {
                        running.last_error = String::new();
                    }
                    reaped.push(name.clone());
                }
            }
            // Park reaped VMs as ghosts so their exit reason stays visible.
            for name in reaped {
                if let Some(running) = state.running.remove(&name) {
                    state.ghosts.insert(
                        name.clone(),
                        VmLive {
                            name: name.clone(),
                            state: "stopped".into(),
                            pid: None,
                            vnc_port: None,
                            vnc_password: String::new(),
                            cpu_percent: 0.0,
                            mem_used_mib: 0,
                            unexpected_exit: running.unexpected_exit,
                            last_error: running.last_error.clone(),
                        },
                    );
                }
            }
        }
    }

    /// Live state for every VM vmd knows about (desired, running or ghost).
    /// The lock is held only to copy immutable snapshots; the per-VM QMP
    /// probes run after releasing it, so a wedged QEMU cannot stall
    /// reconciliation or lifecycle actions.
    pub async fn live(&self) -> Vec<VmLive> {
        self.poll().await;
        let (desired, running, ghosts) = {
            let state = self.state.lock().await;
            (
                state.desired.vms.clone(),
                state
                    .running
                    .iter()
                    .map(|(name, r)| {
                        (
                            name.clone(),
                            (
                                r.exited.is_none(),
                                RunningSnapshot {
                                    pid: r.pid(),
                                    vnc_display: r.vnc_display,
                                    vnc_password: r.vnc_password.clone(),
                                    started_at: r.started_at,
                                    unexpected_exit: r.unexpected_exit,
                                    last_error: r.last_error.clone(),
                                },
                            ),
                        )
                    })
                    .collect::<Vec<(String, (bool, RunningSnapshot))>>(),
                state.ghosts.clone(),
            )
        };
        let lookup = |name: &str| -> Option<&(bool, RunningSnapshot)> {
            running
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, snapshot)| snapshot)
        };

        let mut out: Vec<VmLive> = Vec::new();
        for vm in &desired {
            match lookup(&vm.name) {
                Some((alive, snapshot)) => {
                    let (cpu, mem) = if *alive {
                        let mem = Qmp::new(self.run_dir.join(format!("{}.qmp", vm.name)))
                            .balloon_actual_mib()
                            .await
                            .ok()
                            .flatten()
                            .unwrap_or(0);
                        (
                            cpu_percent(snapshot.pid, vm.cpu, snapshot.started_at),
                            mem as u32,
                        )
                    } else {
                        (0.0, 0)
                    };
                    out.push(VmLive {
                        name: vm.name.clone(),
                        state: if *alive {
                            "running".into()
                        } else {
                            "stopped".into()
                        },
                        pid: alive.then_some(snapshot.pid),
                        vnc_port: alive.then_some(5900 + snapshot.vnc_display),
                        vnc_password: if *alive {
                            snapshot.vnc_password.clone()
                        } else {
                            String::new()
                        },
                        cpu_percent: cpu,
                        mem_used_mib: mem,
                        unexpected_exit: snapshot.unexpected_exit,
                        last_error: snapshot.last_error.clone(),
                    });
                }
                None => {
                    if let Some(ghost) = ghosts.get(&vm.name) {
                        out.push(ghost.clone());
                    } else {
                        out.push(VmLive {
                            name: vm.name.clone(),
                            state: "stopped".into(),
                            ..Default::default()
                        });
                    }
                }
            }
        }
        // Ghosts of removed VMs (their exit reason was never reported).
        for ghost in ghosts.values() {
            if !desired.iter().any(|vm| vm.name == ghost.name) {
                out.push(ghost.clone());
            }
        }
        out
    }

    async fn start(&self, spec: &VmSpec) -> Result<(), String> {
        {
            let state = self.state.lock().await;
            if state.running.contains_key(&spec.name) {
                return Ok(());
            }
        }

        self.start_inner(spec).await
    }

    async fn start_inner(&self, spec: &VmSpec) -> Result<(), String> {
        let dir = vm_dir(&spec.pool, &spec.name);
        wait_for_dir(&dir).await?;

        // Disk: create blank, or seed from a library cloud image.
        let disk = std::path::Path::new(&dir).join("disk.qcow2");
        if !disk.exists() {
            if let Some(source) = &spec.disk.source_image {
                let bare = crate::model::library_file(source)
                    .map_err(|e| format!("disk sourceImage {e}"))?;
                let source_path = crate::model::library_path(&spec.pool, &bare);
                if !tokio::fs::try_exists(&source_path).await.unwrap_or(false) {
                    return Err(format!(
                        "source image {source:?} is not in the {} library",
                        spec.pool
                    ));
                }
                let format = match spec.disk.format {
                    crate::model::DiskFormat::Qcow2 => "qcow2",
                    crate::model::DiskFormat::Raw => "raw",
                };
                let output = tokio::process::Command::new(crate::model::qemu_img_binary())
                    .arg("convert")
                    .arg("-O")
                    .arg(format)
                    .arg(&source_path)
                    .arg(&disk)
                    .output()
                    .await
                    .map_err(|e| format!("running qemu-img: {e}"))?;
                if !output.status.success() {
                    return Err(format!(
                        "seeding the disk from {source:?}: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    ));
                }
                resize_disk(&disk, spec.disk.size_gb).await?;
            } else {
                let format = match spec.disk.format {
                    crate::model::DiskFormat::Qcow2 => "qcow2",
                    crate::model::DiskFormat::Raw => "raw",
                };
                let output = tokio::process::Command::new(crate::model::qemu_img_binary())
                    .arg("create")
                    .arg("-f")
                    .arg(format)
                    .arg(&disk)
                    .arg(format!("{}G", spec.disk.size_gb))
                    .output()
                    .await
                    .map_err(|e| format!("running qemu-img: {e}"))?;
                if !output.status.success() {
                    return Err(format!(
                        "creating the disk image: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    ));
                }
            }
        }

        // UEFI vars: a writable per-VM copy of the template, made on first
        // start only (the template itself is mounted read-only in the image).
        let vars_path = std::path::Path::new(&dir).join("vars.fd");
        if spec.firmware == Firmware::Uefi && !vars_path.exists() {
            let template = self
                .firmware
                .1
                .clone()
                .ok_or("UEFI firmware is not installed in the vmd image".to_string())?;
            let bytes = tokio::fs::read(&template)
                .await
                .map_err(|e| format!("reading the UEFI vars template: {e}"))?;
            tokio::fs::write(&vars_path, bytes)
                .await
                .map_err(|e| format!("writing the UEFI vars copy: {e}"))?;
        }

        // cloud-init seed (regenerated every start; idempotent for the guest).
        let seed = match &spec.cloud_init {
            Some(_) => Some(
                cloudinit::build_seed(spec, std::path::Path::new(&dir))
                    .await
                    .map_err(|e| format!("building the cloud-init seed: {e}"))?,
            ),
            None => None,
        };

        // VNC display + password.
        let vnc_display_num = self
            .allocate_display()
            .await
            .ok_or_else(|| "no free VNC display in the configured range".to_string())?;
        let password = vnc_password();

        let iso_path = match &spec.iso {
            Some(iso) => {
                let bare = crate::model::library_file(iso).map_err(|e| format!("iso {e}"))?;
                let path = crate::model::library_path(&spec.pool, &bare);
                if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
                    return Err(format!("ISO {iso:?} is not in the {} library", spec.pool));
                }
                Some(PathBuf::from(path))
            }
            None => None,
        };

        let qmp_socket = self.run_dir.join(format!("{}.qmp", spec.name));
        let _ = tokio::fs::remove_file(&qmp_socket).await;

        let ctx = BuildContext {
            kvm: self.kvm,
            vnc_display: vnc_display_num,
            vm_dir: PathBuf::from(&dir),
            qmp_socket: qmp_socket.clone(),
            uefi_code: self.firmware.0.clone(),
            uefi_vars: Some(vars_path),
            cloud_init_seed: seed,
            iso_path,
        };
        let argv = qemu_args(spec, &ctx)?;

        // QEMU's stderr lands in the dataset so a failed start is
        // diagnosable from the VM directory.
        let log_path = std::path::Path::new(&dir).join("qemu.log");
        let log = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&log_path)
            .map_err(|e| format!("opening {}: {e}", log_path.display()))?;

        let mut command = tokio::process::Command::new(crate::model::qemu_binary());
        command
            .args(&argv)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::from(log))
            .kill_on_drop(false);

        let child = command
            .spawn()
            .map_err(|e| format!("spawning QEMU for {}: {e}", spec.name))?;
        let pid = child.id().unwrap_or(0);

        // Wait for QMP, then set the VNC password before any console client
        // can connect.
        let mut qmp_ok = false;
        for _ in 0..30 {
            if Qmp::new(&qmp_socket)
                .set_vnc_password(&password)
                .await
                .is_ok()
            {
                qmp_ok = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        if !qmp_ok {
            // The process may still be starting; report but keep running.
            tracing::warn!(
                "could not set the VNC password for {} within 15s; the console may refuse connections",
                spec.name
            );
        }

        {
            let mut state = self.state.lock().await;
            state.running.insert(
                spec.name.clone(),
                Running {
                    child,
                    spec: spec.clone(),
                    vnc_display: vnc_display_num,
                    vnc_password: password,
                    started_at: std::time::Instant::now(),
                    exited: None,
                    stopping: false,
                    last_error: String::new(),
                    unexpected_exit: false,
                },
            );
            state.ghosts.remove(&spec.name);
        }
        tracing::info!(
            "started VM {} (pid {pid}, vnc display {vnc_display_num})",
            spec.name
        );
        Ok(())
    }

    /// Stop one VM. `force` skips the graceful ACPI shutdown.
    async fn stop_one(&self, name: &str, force: bool) -> Result<(), String> {
        let (qmp_socket, alive) = {
            let mut state = self.state.lock().await;
            let Some(running) = state.running.get_mut(name) else {
                return Ok(());
            };
            running.stopping = true;
            (
                self.run_dir.join(format!("{name}.qmp")),
                running.exited.is_none(),
            )
        };
        if !alive {
            return Ok(());
        }

        if !force {
            let _ = Qmp::new(&qmp_socket).system_powerdown().await;
            if self.wait_exit(name, GRACEFUL_STOP_SECS).await {
                return Ok(());
            }
        } else {
            // Force: ask QEMU to quit cleanly first (it flushes block writes),
            // then escalate to signals.
            let _ = Qmp::new(&qmp_socket).quit().await;
            if self.wait_exit(name, 5).await {
                return Ok(());
            }
        }

        // SIGTERM, then SIGKILL.
        if let Some(pid) = self.pid_of(name).await {
            let _ = kill(pid, libc::SIGTERM);
        }
        if self.wait_exit(name, TERM_STOP_SECS).await {
            return Ok(());
        }
        if let Some(pid) = self.pid_of(name).await {
            let _ = kill(pid, libc::SIGKILL);
        }
        if !self.wait_exit(name, 5).await {
            return Err(format!("VM {name} did not exit after SIGKILL"));
        }
        tracing::info!("stopped VM {name}");
        Ok(())
    }

    async fn wait_exit(&self, name: &str, secs: u64) -> bool {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(secs);
        loop {
            self.poll().await;
            let alive = {
                let state = self.state.lock().await;
                state
                    .running
                    .get(name)
                    .map(|r| r.exited.is_none())
                    .unwrap_or(false)
            };
            if !alive {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }

    async fn pid_of(&self, name: &str) -> Option<u32> {
        self.state.lock().await.running.get(name).map(|r| r.pid())
    }

    async fn allocate_display(&self) -> Option<u16> {
        let state = self.state.lock().await;
        for display in self.vnc_first_display..self.vnc_first_display + self.vnc_display_count {
            if !state
                .running
                .values()
                .any(|r| r.vnc_display == display && r.exited.is_none())
            {
                return Some(display);
            }
        }
        None
    }
}

impl Running {
    fn pid(&self) -> u32 {
        self.child.id().unwrap_or(0)
    }
}

/// A copy of the per-VM fields a live report needs, made under the
/// supervisor lock so the QMP work can happen after releasing it.
#[derive(Debug, Clone)]
struct RunningSnapshot {
    pid: u32,
    vnc_display: u16,
    vnc_password: String,
    started_at: std::time::Instant,
    unexpected_exit: bool,
    last_error: String,
}

/// Whether a running VM matches the desired spec (the recreate test).
fn running_matches(running: &Running, desired: &VmSpec) -> bool {
    running.spec.cpu == desired.cpu
        && running.spec.mem_mib == desired.mem_mib
        && running.spec.firmware == desired.firmware
        && running.spec.disk == desired.disk
        && running.spec.net == desired.net
        && running.spec.cloud_init == desired.cloud_init
        && running.spec.iso == desired.iso
        && running.spec.pool == desired.pool
}

fn kill(pid: u32, signal: i32) -> std::io::Result<()> {
    // Killing by pid is precise; QEMU children share the pod's process group
    // and die with the pod, so no group signal is ever needed here.
    let ret = unsafe { libc::kill(pid as libc::pid_t, signal) };
    if ret == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

async fn resize_disk(disk: &std::path::Path, size_gb: u32) -> Result<(), String> {
    let output = tokio::process::Command::new(crate::model::qemu_img_binary())
        .arg("resize")
        .arg(disk)
        .arg(format!("{size_gb}G"))
        .output()
        .await
        .map_err(|e| format!("running qemu-img: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "resizing the disk: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

/// Bounded wait for a fresh VM's dataset directory (the mount-propagation
/// trap: the agent mounts it on the host, vmd sees it a moment later).
async fn wait_for_dir(dir: &str) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(DATASET_WAIT_SECS);
    loop {
        if tokio::fs::metadata(dir)
            .await
            .map(|m| m.is_dir())
            .unwrap_or(false)
        {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "the VM dataset directory {dir} is not visible to vmd after {DATASET_WAIT_SECS}s (was the dataset created?)"
            ));
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

/// Average CPU since start, as a percent of the VM's vCPUs.
fn cpu_percent(pid: u32, cpus: u32, started_at: std::time::Instant) -> f64 {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return 0.0;
    };
    // The comm field can contain spaces and parens; parse after the last ')'.
    let Some(rest) = stat.rsplit_once(')') else {
        return 0.0;
    };
    let fields: Vec<&str> = rest.1.split_whitespace().collect();
    // fields[11] utime, fields[12] stime (0-based, after the state field).
    let utime: f64 = fields.get(11).and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let stime: f64 = fields.get(12).and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let cpu_secs = (utime + stime) / 100.0; // _SC_CLK_TCK on every supported Linux
    let wall = started_at.elapsed().as_secs_f64().max(1.0);
    let percent = (cpu_secs / wall) * 100.0;
    let cap = cpus.max(1) as f64 * 100.0;
    percent.min(cap)
}

fn tail_qemu_log(dir: &str) -> String {
    std::fs::read_to_string(format!("{dir}/qemu.log"))
        .map(|content| {
            content
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .chars()
                .take(200)
                .collect()
        })
        .unwrap_or_default()
}

fn vnc_password() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghjkmnpqrstuvwxyz23456789";
    let mut raw = [0u8; VNC_PASSWORD_LEN];
    let _ = getrandom::fill(&mut raw);
    raw.iter()
        .map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char)
        .collect()
}

/// The background watchdog: reaps children and keeps state fresh even when
/// nobody is calling `live()`.
pub async fn run_watchdog(supervisor: Arc<Supervisor>) {
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(30));
    ticker.tick().await;
    loop {
        ticker.tick().await;
        supervisor.poll().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CloudInitSpec, DesiredState, DiskFormat, DiskSpec, NetSpec, PortForward};

    fn spec(name: &str) -> VmSpec {
        VmSpec {
            name: name.into(),
            cpu: 1,
            mem_mib: 128,
            autostart: false,
            firmware: Firmware::Bios,
            desired: DesiredState::Stopped,
            disk: DiskSpec {
                size_gb: 1,
                format: DiskFormat::Qcow2,
                bus: crate::model::DiskBus::Virtio,
                source_image: None,
            },
            net: NetSpec {
                forwards: vec![PortForward {
                    host: 8081,
                    dst: 80,
                }],
                ..NetSpec::default()
            },
            cloud_init: Some(CloudInitSpec {
                hostname: name.into(),
                user: "admin".into(),
                ssh_keys: vec![],
            }),
            iso: None,
            pool: "tank".into(),
        }
    }

    fn supervisor() -> Arc<Supervisor> {
        let dir = tempfile::tempdir().unwrap();
        // Leak the tempdir: the supervisor outlives the test, but the OS
        // cleans /tmp anyway and no assertion needs the path afterwards.
        let run_dir = dir.keep();
        Arc::new(Supervisor::new(false, 1, 16, run_dir))
    }

    #[test]
    fn vnc_passwords_are_8_alphanumeric_chars() {
        let password = vnc_password();
        assert_eq!(password.len(), 8);
        assert!(
            password.chars().all(|c| c.is_ascii_alphanumeric()),
            "{password}"
        );
    }

    #[tokio::test]
    async fn apply_validates_and_rejects_duplicates() {
        let sup = supervisor();
        let mut config = DesiredConfig::default();
        config.vms.push(spec("dup"));
        config.vms.push(spec("dup"));
        assert!(sup.clone().apply(config).await.is_err());

        let mut config = DesiredConfig::default();
        let mut bad = spec("ok-vm");
        bad.cpu = 0;
        config.vms.push(bad);
        assert!(sup.clone().apply(config).await.is_err());
    }

    #[tokio::test]
    async fn apply_rejects_cross_vm_host_port_collisions() {
        let sup = supervisor();
        let mut config = DesiredConfig::default();
        config.vms.push(spec("vm-a"));
        let mut other = spec("vm-b");
        other.net.forwards = spec("vm-a").net.forwards.clone();
        config.vms.push(other);
        assert!(sup.clone().apply(config).await.is_err());
    }

    #[tokio::test]
    async fn apply_returns_before_reconcile_completes() {
        // A desired-running VM whose dataset directory does not exist makes
        // the reconcile block in its bounded 60 s dataset wait. Before the
        // reconcile was detached, apply blocked with it — and a client
        // disconnect (axum drops the handler future) aborted the start
        // mid-flight while the revision was already installed, so every
        // later re-push of the same revision was a no-op and the VM never
        // started. apply must install the config and return promptly.
        let sup = supervisor();
        let mut config = DesiredConfig::default();
        config.vms.push(spec("blocked-start"));
        let applied =
            tokio::time::timeout(std::time::Duration::from_secs(5), sup.clone().apply(config))
                .await;
        assert!(applied.is_ok(), "apply must not block on the reconcile");
        assert!(applied.unwrap().is_ok());
        // The desired state is installed synchronously; the detached
        // reconcile is what would eventually record the wait error.
        let live = sup.live().await;
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].name, "blocked-start");
    }

    #[tokio::test]
    async fn stopped_vms_reconcile_without_starting() {
        // Everything in the config is desired=stopped and nothing runs, so
        // reconcile is a no-op that succeeds without touching the host.
        let sup = supervisor();
        let mut config = DesiredConfig::default();
        config.vms.push(spec("idle"));
        sup.clone().apply(config).await.unwrap();
        let live = sup.live().await;
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].name, "idle");
        assert_eq!(live[0].state, "stopped");
    }

    #[tokio::test]
    async fn force_stop_of_a_stopped_vm_is_an_error() {
        let sup = supervisor();
        assert!(sup.force_stop("ghost").await.is_err());
    }

    #[tokio::test]
    async fn removed_vms_are_reported_as_ghosts_only_after_running() {
        // A VM never started leaves no ghost; live() must not invent one.
        let sup = supervisor();
        let live = sup.live().await;
        assert!(live.is_empty());
    }
}
