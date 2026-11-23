//! The VM supervisor: vmd's reconciler between the desired config the API
//! pushed and the QEMU processes actually running.
//!
//! Invariants (mirrors the shares apply path):
//! - the API is the brain; vmd only applies what it validated itself too
//!   (SEC-1: a bad render never reaches a command line),
//! - one vmd pod per node, so two supervisors never fight over QEMU
//!   processes or VNC ports (`Recreate` strategy),
//! - `state` is always derived here, never persisted.

use crate::args::{detect_firmware, qemu_args, BuildContext, FirmwareImages};
use crate::cloudinit;
use crate::model::{validate_vm, vm_dir, DesiredConfig, Firmware, VmLive, VmSpec, DATASETS_ROOT};
use crate::qmp::Qmp;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// How long a graceful stop waits before SIGTERM.
const GRACEFUL_STOP_SECS: u64 = 30;
/// How long SIGTERM waits before SIGKILL.
const TERM_STOP_SECS: u64 = 10;
/// How long a fresh VM's dataset dir is awaited (mount-propagation lag).
const DATASET_WAIT_SECS: u64 = 60;
/// VNC passwords are truncated to 8 characters by the RFB spec.
const VNC_PASSWORD_LEN: usize = 8;
/// How long a start waits for QEMU's QMP socket before giving up on the VNC
/// password (the process is kept running, as before).
const QMP_WAIT: Duration = Duration::from_secs(15);
/// A reservation older than this has no live start task (a panic would leave
/// one behind); it is dropped so the name can be started again. A normal
/// start holds a reservation only for the bounded dataset wait plus the
/// qemu-img conversion, both far below this.
const STALE_START: Duration = Duration::from_secs(600);

struct Running {
    /// `None` while the name/display is reserved but QEMU has not spawned.
    child: Option<tokio::process::Child>,
    spec: VmSpec,
    vnc_display: u16,
    vnc_password: String,
    started_at: Instant,
    exited: Option<std::process::ExitStatus>,
    /// The start reserved the name and is still preparing; a second reconcile
    /// must not start the same VM. Cleared once the process is installed.
    starting: bool,
    /// Identifies this reservation, so a cancelled start cannot install a
    /// process into a newer reservation of the same name.
    token: u64,
    /// The stop was requested by the reconciler (not an unexpected exit).
    stopping: bool,
    last_error: String,
    unexpected_exit: bool,
}

impl Running {
    fn pid(&self) -> u32 {
        self.child
            .as_ref()
            .and_then(|child| child.id())
            .unwrap_or(0)
    }

    /// A live QEMU process. A reservation is not alive yet; a reaped child is
    /// no longer alive even before `poll` moves it to a ghost.
    fn alive(&self) -> bool {
        self.child.is_some() && self.exited.is_none()
    }

    /// The name and display are held by an in-flight start.
    fn reserved(&self) -> bool {
        self.child.is_none() && self.exited.is_none()
    }
}

#[derive(Default)]
struct State {
    desired: DesiredConfig,
    /// The last applied config revision: identical re-pushes are no-ops.
    last_revision: String,
    /// Bumped on every accepted config. A detached reconcile only runs when
    /// its generation is still the newest, so a slow older push cannot build
    /// on top of a newer one.
    generation: u64,
    /// Monotonic reservation id (see [`Running::token`]).
    next_start_token: u64,
    running: HashMap<String, Running>,
    /// Last reported live state for VMs that are not running (or were removed
    /// from the desired config), so the API can still see exits and errors.
    ghosts: HashMap<String, VmLive>,
}

pub struct Supervisor {
    kvm: bool,
    vnc_first_display: u16,
    vnc_display_count: u16,
    /// Root the pool datasets are mounted under (`/var/mnt`); overridable so
    /// tests never touch the host layout.
    datasets_root: PathBuf,
    run_dir: PathBuf,
    firmware: FirmwareImages,
    qemu_bin: String,
    /// How long a start waits for QMP before warning (tests shorten it).
    qmp_wait: Duration,
    state: Mutex<State>,
    /// Serializes every reconcile. `apply` detaches its reconcile so a client
    /// disconnect cannot cancel a start/stop mid-flight, but two detached
    /// reconciles must never run together: both would see "not running" and
    /// spawn two QEMU processes for one VM.
    reconcile_lock: Mutex<()>,
}

impl Supervisor {
    pub fn new(
        kvm: bool,
        vnc_first_display: u16,
        vnc_display_count: u16,
        run_dir: PathBuf,
    ) -> Self {
        Self::with_datasets_root(
            kvm,
            vnc_first_display,
            vnc_display_count,
            run_dir,
            PathBuf::from(DATASETS_ROOT),
        )
    }

    pub fn with_datasets_root(
        kvm: bool,
        vnc_first_display: u16,
        vnc_display_count: u16,
        run_dir: PathBuf,
        datasets_root: PathBuf,
    ) -> Self {
        Self {
            kvm,
            vnc_first_display,
            vnc_display_count,
            datasets_root,
            run_dir,
            firmware: detect_firmware(),
            qemu_bin: crate::model::qemu_binary(),
            qmp_wait: QMP_WAIT,
            state: Mutex::new(State {
                desired: DesiredConfig::default(),
                last_revision: String::new(),
                generation: 0,
                next_start_token: 0,
                running: HashMap::new(),
                ghosts: HashMap::new(),
            }),
            reconcile_lock: Mutex::new(()),
        }
    }

    /// Test constructor: fake QEMU binary, a bounded QMP wait and a temp
    /// datasets root. Not part of the production surface.
    #[cfg(test)]
    pub fn for_test(
        run_dir: PathBuf,
        datasets_root: PathBuf,
        qemu_bin: String,
        qmp_wait: Duration,
    ) -> Self {
        let mut supervisor = Self::with_datasets_root(false, 1, 16, run_dir, datasets_root);
        supervisor.qemu_bin = qemu_bin;
        supervisor.qmp_wait = qmp_wait;
        supervisor
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
        // Install the config and bump the generation under the lock, then
        // detach exactly one reconcile. The generation check makes a slow
        // older push a no-op instead of building on a newer config.
        let generation = {
            let mut state = self.state.lock().await;
            if !config.revision.is_empty() && state.last_revision == config.revision {
                // The API re-renders the same config on watcher recoveries;
                // an identical revision is a no-op.
                return Ok(());
            }
            state.last_revision = config.revision.clone();
            state.desired = config;
            state.generation = state.generation.wrapping_add(1);
            state.generation
        };
        let supervisor = Arc::clone(&self);
        tokio::spawn(async move {
            let _serialized = supervisor.reconcile_lock.lock().await;
            let current = supervisor.state.lock().await.generation == generation;
            if !current {
                // A newer apply is queued or already reconciling; that pass
                // reads the newest desired state.
                return;
            }
            if let Err(errors) = supervisor.reconcile_locked().await {
                tracing::warn!("reconcile errors: {errors}");
            }
        });
        Ok(())
    }

    /// Diff desired against running: start, stop and recreate as needed.
    /// Public for the watchdog; every call is serialized.
    pub async fn reconcile(&self) -> Result<(), String> {
        let _serialized = self.reconcile_lock.lock().await;
        self.reconcile_locked().await
    }

    async fn reconcile_locked(&self) -> Result<(), String> {
        // Reap before diffing: a crashed child must look absent (and its exit
        // reason must be parked) before the decision to (re)start.
        self.poll().await;
        // Drop reservations whose start task is gone (a panic would otherwise
        // hold the name and its display forever).
        let stale: Vec<String> = {
            let state = self.state.lock().await;
            state
                .running
                .iter()
                .filter(|(_, running)| {
                    running.reserved() && running.started_at.elapsed() > STALE_START
                })
                .map(|(name, _)| name.clone())
                .collect()
        };
        if !stale.is_empty() {
            let mut state = self.state.lock().await;
            for name in &stale {
                tracing::warn!("dropping the stale start reservation of VM {name}");
                state.running.remove(name);
            }
        }

        // Snapshot the desired list without holding the lock across the
        // per-VM work.
        let desired: Vec<VmSpec> = self.state.lock().await.desired.vms.clone();
        let mut errors: Vec<String> = Vec::new();

        for vm in &desired {
            match vm.desired {
                crate::model::DesiredState::Running => {
                    let (held, needs_recreate) = {
                        let state = self.state.lock().await;
                        match state.running.get(&vm.name) {
                            // A reaped entry is treated as absent; `start`
                            // removes it before reserving.
                            Some(r) if r.exited.is_some() => (false, false),
                            // Alive or an in-flight reservation. A start
                            // whose spec changed while it was preparing is
                            // cancelled and restarted rather than booting the
                            // old definition.
                            Some(r) => (true, !running_matches(r, vm)),
                            None => (false, false),
                        }
                    };
                    if needs_recreate {
                        if let Err(e) = self.stop_one(&vm.name, true).await {
                            errors.push(format!("recreating VM {}: {e}", vm.name));
                            continue;
                        }
                    }
                    if !held || needs_recreate {
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
                    // A reservation counts: removing it cancels the start
                    // before QEMU spawns (otherwise a stop that races a start
                    // would let the VM come up despite desired=stopped).
                    let present = {
                        let state = self.state.lock().await;
                        state
                            .running
                            .get(&vm.name)
                            .map(|r| r.alive() || r.reserved())
                            .unwrap_or(false)
                    };
                    if present {
                        if let Err(e) = self.stop_one(&vm.name, false).await {
                            errors.push(format!("stopping VM {}: {e}", vm.name));
                        }
                    }
                }
            }
        }

        // VMs removed from the config are stopped and forgotten. This also
        // cancels a reservation whose start is still preparing.
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
    /// A VM whose start is still preparing is cancelled before QEMU spawns.
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
                let Some(child) = running.child.as_mut() else {
                    // Reservation: no process exists yet.
                    continue;
                };
                if let Ok(Some(status)) = child.try_wait() {
                    running.exited = Some(status);
                    if !running.stopping {
                        running.unexpected_exit = true;
                        running.last_error = format!(
                            "QEMU exited unexpectedly with {status}; last log line: {}",
                            tail_qemu_log(&vm_dir(&self.datasets_root, &running.spec.pool, name))
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
    ///
    /// A reservation (start in flight, no process yet) is not reported as
    /// running. If it replaced a crashed VM, the ghost's exit reason stays
    /// visible until the new process actually exists.
    pub async fn live(&self) -> Vec<VmLive> {
        self.poll().await;
        let (desired, running, ghosts) = {
            let state = self.state.lock().await;
            (
                state.desired.vms.clone(),
                state
                    .running
                    .iter()
                    .filter(|(_, r)| r.alive())
                    .map(|(name, r)| {
                        (
                            name.clone(),
                            RunningSnapshot {
                                pid: r.pid(),
                                vnc_display: r.vnc_display,
                                vnc_password: r.vnc_password.clone(),
                                started_at: r.started_at,
                                unexpected_exit: r.unexpected_exit,
                                last_error: r.last_error.clone(),
                            },
                        )
                    })
                    .collect::<Vec<(String, RunningSnapshot)>>(),
                state.ghosts.clone(),
            )
        };
        let lookup = |name: &str| -> Option<&RunningSnapshot> {
            running
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, snapshot)| snapshot)
        };

        let mut out: Vec<VmLive> = Vec::new();
        for vm in &desired {
            match lookup(&vm.name) {
                Some(snapshot) => {
                    let mem = Qmp::new(self.run_dir.join(format!("{}.qmp", vm.name)))
                        .balloon_actual_mib()
                        .await
                        .ok()
                        .flatten()
                        .unwrap_or(0);
                    out.push(VmLive {
                        name: vm.name.clone(),
                        state: "running".into(),
                        pid: Some(snapshot.pid),
                        vnc_port: Some(5900 + snapshot.vnc_display),
                        vnc_password: snapshot.vnc_password.clone(),
                        cpu_percent: cpu_percent(snapshot.pid, vm.cpu, snapshot.started_at),
                        mem_used_mib: mem as u32,
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
        let Some((token, display)) = self.reserve_start(spec).await? else {
            // Already running or a start is already preparing this VM.
            return Ok(());
        };
        match self.start_inner(spec, token, display).await {
            Ok(()) => Ok(()),
            Err(e) => {
                self.cancel_reservation(&spec.name, token).await;
                Err(e)
            }
        }
    }

    /// Reserve the VM name and a VNC display under the state lock before any
    /// slow work (the dataset wait). Without the placeholder two serialized
    /// pushes cannot race, but a start that takes 60 s would otherwise leave
    /// the name free for a concurrent force-stop → re-push sequence.
    ///
    /// `Ok(None)` means the VM is already running or starting.
    async fn reserve_start(&self, spec: &VmSpec) -> Result<Option<(u64, u16)>, String> {
        let mut state = self.state.lock().await;
        if let Some(running) = state.running.get(&spec.name) {
            if running.exited.is_none() {
                return Ok(None);
            }
            // A reaped entry is absent; drop it before reserving the name.
            state.running.remove(&spec.name);
        }
        let display = (self.vnc_first_display..self.vnc_first_display + self.vnc_display_count)
            .find(|display| {
                !state
                    .running
                    .values()
                    .any(|r| r.vnc_display == *display && r.exited.is_none())
            })
            .ok_or_else(|| "no free VNC display in the configured range".to_string())?;
        state.next_start_token = state.next_start_token.wrapping_add(1);
        let token = state.next_start_token;
        state.running.insert(
            spec.name.clone(),
            Running {
                child: None,
                spec: spec.clone(),
                vnc_display: display,
                vnc_password: String::new(),
                started_at: Instant::now(),
                exited: None,
                starting: true,
                token,
                stopping: false,
                last_error: String::new(),
                unexpected_exit: false,
            },
        );
        Ok(Some((token, display)))
    }

    async fn cancel_reservation(&self, name: &str, token: u64) {
        let mut state = self.state.lock().await;
        if state
            .running
            .get(name)
            .is_some_and(|running| running.token == token)
        {
            // Dropping a spawned child here is safe: QEMU is started with
            // kill_on_drop(true).
            state.running.remove(name);
        }
    }

    async fn start_inner(
        &self,
        spec: &VmSpec,
        token: u64,
        vnc_display_num: u16,
    ) -> Result<(), String> {
        let dir = vm_dir(&self.datasets_root, &spec.pool, &spec.name);
        wait_for_dir(&dir).await?;

        // Disk: create blank, or seed from a library cloud image. A symlink at
        // any of these paths would let a share account redirect QEMU's open
        // (and qemu-img's truncate) outside the dataset.
        let disk = dir.join("disk.qcow2");
        let disk_exists = match tokio::fs::symlink_metadata(&disk).await {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(format!(
                    "refusing to use the symlink at {}: the disk image must be a regular file",
                    disk.display()
                ))
            }
            Ok(meta) if meta.is_file() => true,
            Ok(_) => {
                return Err(format!(
                    "{} exists but is not a regular file",
                    disk.display()
                ))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(format!("checking {}: {e}", disk.display())),
        };
        let format = match spec.disk.format {
            crate::model::DiskFormat::Qcow2 => "qcow2",
            crate::model::DiskFormat::Raw => "raw",
        };
        if !disk_exists {
            if let Some(source) = &spec.disk.source_image {
                let bare = crate::model::library_file(source)
                    .map_err(|e| format!("disk sourceImage {e}"))?;
                let source_path =
                    crate::model::library_path(&self.datasets_root, &spec.pool, &bare);
                require_library_file(&source_path, "source image").await?;
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
        // Secure Boot gets its own NVRAM file and key-enrolled template so a
        // non-Secure-Boot var store can never be booted as Secure Boot.
        let vars_name = if spec.secure_boot {
            "vars.secboot.fd"
        } else {
            "vars.fd"
        };
        let vars_path = dir.join(vars_name);
        if spec.firmware == Firmware::Uefi {
            match tokio::fs::symlink_metadata(&vars_path).await {
                Ok(meta) if meta.file_type().is_symlink() => {
                    let message = format!(
                        "refusing to use the symlink at {}: the UEFI vars store must be a regular file",
                        vars_path.display()
                    );
                    return Err(message);
                }
                Ok(meta) if !meta.is_file() => {
                    return Err(format!(
                        "{} exists but is not a regular file",
                        vars_path.display()
                    ))
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    let template = if spec.secure_boot {
                        self.firmware.secure_boot_vars.as_ref().ok_or(
                            "Secure Boot firmware is not installed in the vmd image".to_string(),
                        )?
                    } else {
                        self.firmware
                            .vars
                            .as_ref()
                            .ok_or("UEFI firmware is not installed in the vmd image".to_string())?
                    };
                    let bytes = tokio::fs::read(template)
                        .await
                        .map_err(|e| format!("reading the UEFI vars template: {e}"))?;
                    write_new_file(&vars_path, &bytes)
                        .await
                        .map_err(|e| format!("writing the UEFI vars copy: {e}"))?;
                }
                Err(e) => return Err(format!("checking {}: {e}", vars_path.display())),
            }
        }

        // cloud-init seed (regenerated every start; idempotent for the guest).
        let seed = match &spec.cloud_init {
            Some(_) => Some(
                cloudinit::build_seed(spec, &dir)
                    .await
                    .map_err(|e| format!("building the cloud-init seed: {e}"))?,
            ),
            None => None,
        };

        let password = vnc_password()?;

        let iso_path = match &spec.iso {
            Some(iso) => {
                let bare = crate::model::library_file(iso).map_err(|e| format!("iso {e}"))?;
                let path = crate::model::library_path(&self.datasets_root, &spec.pool, &bare);
                require_library_file(&path, "ISO").await?;
                Some(path)
            }
            None => None,
        };

        let qmp_socket = self.run_dir.join(format!("{}.qmp", spec.name));
        let _ = tokio::fs::remove_file(&qmp_socket).await;

        let ctx = BuildContext {
            kvm: self.kvm,
            vnc_display: vnc_display_num,
            vm_dir: dir.clone(),
            qmp_socket: qmp_socket.clone(),
            uefi_code: self.firmware.code.clone(),
            uefi_vars: Some(vars_path),
            secure_boot_code: self.firmware.secure_boot_code.clone(),
            cloud_init_seed: seed,
            iso_path,
        };
        let argv = qemu_args(spec, &ctx)?;

        // QEMU's stderr lands in the dataset so a failed start is
        // diagnosable from the VM directory. O_NOFOLLOW refuses a planted
        // symlink instead of truncating its target.
        let log_path = dir.join("qemu.log");
        let log = open_qemu_log(&log_path)?;

        let mut command = tokio::process::Command::new(&self.qemu_bin);
        command
            .args(&argv)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::from(log))
            // vmd is the only owner of this process tree; a dropped Child
            // (cancelled start, supervisor shutdown) must not orphan QEMU.
            .kill_on_drop(true);

        let pid = {
            let mut state = self.state.lock().await;
            let Some(entry) = state.running.get_mut(&spec.name) else {
                return Ok(());
            };
            if entry.token != token || !entry.starting || entry.stopping {
                return Err(format!("starting VM {} was cancelled", spec.name));
            }
            // Spawning while the reservation is still held is what makes the
            // reservation → process transition atomic for a concurrent stop.
            let child = command
                .spawn()
                .map_err(|e| format!("spawning QEMU for {}: {e}", spec.name))?;
            let pid = child.id().unwrap_or(0);
            entry.started_at = Instant::now();
            entry.child = Some(child);
            pid
        };

        // Wait for QMP, then set the VNC password before any console client
        // can connect. Stop early if the process is gone.
        let mut qmp_ok = false;
        let deadline = Instant::now() + self.qmp_wait;
        loop {
            if Qmp::new(&qmp_socket)
                .set_vnc_password(&password)
                .await
                .is_ok()
            {
                qmp_ok = true;
                break;
            }
            self.poll().await;
            let still_starting = {
                let state = self.state.lock().await;
                state
                    .running
                    .get(&spec.name)
                    .is_some_and(|running| running.token == token && running.alive())
            };
            if !still_starting || Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if !qmp_ok {
            // The process may still be starting; report but keep running.
            tracing::warn!(
                "could not set the VNC password for {} within {}s; the console may refuse connections",
                spec.name,
                self.qmp_wait.as_secs()
            );
        }

        {
            let mut state = self.state.lock().await;
            let Some(entry) = state.running.get_mut(&spec.name) else {
                // A stop cancelled the start; the child was dropped (and
                // killed) with the entry.
                return Ok(());
            };
            if entry.token != token {
                return Err(format!("starting VM {} was cancelled", spec.name));
            }
            if !entry.alive() {
                let error = if entry.last_error.is_empty() {
                    format!("QEMU for {} exited during startup", spec.name)
                } else {
                    entry.last_error.clone()
                };
                state.running.remove(&spec.name);
                return Err(error);
            }
            entry.starting = false;
            entry.vnc_password = password;
            entry.last_error.clear();
            entry.unexpected_exit = false;
            state.ghosts.remove(&spec.name);
        }
        tracing::info!(
            "started VM {} (pid {pid}, vnc display {vnc_display_num})",
            spec.name
        );
        Ok(())
    }

    /// Stop one VM. `force` skips the graceful ACPI shutdown.
    ///
    /// A reaped entry is treated as absent (removed, not signalled). A
    /// reservation is cancelled by removing it; its start notices before
    /// spawning QEMU (or the spawned child is killed on drop).
    async fn stop_one(&self, name: &str, force: bool) -> Result<(), String> {
        let qmp_socket = {
            let mut state = self.state.lock().await;
            match state.running.get(name) {
                None => return Ok(()),
                Some(running) if running.exited.is_some() => {
                    state.running.remove(name);
                    return Ok(());
                }
                Some(running) if running.reserved() => {
                    if let Some(running) = state.running.get_mut(name) {
                        running.stopping = true;
                    }
                    state.running.remove(name);
                    return Ok(());
                }
                Some(_) => {}
            }
            if let Some(running) = state.running.get_mut(name) {
                running.stopping = true;
            }
            self.run_dir.join(format!("{name}.qmp"))
        };

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
                state.running.get(name).map(|r| r.alive()).unwrap_or(false)
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

    /// Whether `port` is one of the VNC TCP ports this supervisor may
    /// allocate. The tunnel route refuses anything outside the configured
    /// display range, so a valid API token cannot turn it into a general
    /// loopback port proxy.
    pub fn vnc_port_in_range(&self, port: u16) -> bool {
        let first = 5900u32 + u32::from(self.vnc_first_display);
        let end = first + u32::from(self.vnc_display_count);
        (first..end).contains(&u32::from(port))
    }

    /// One watchdog pass, split out so the restart path is testable.
    pub async fn watchdog_tick(&self) {
        self.poll().await;
        if !self.needs_reconcile().await {
            return;
        }
        if let Err(e) = self.reconcile().await {
            tracing::warn!("watchdog reconcile: {e}");
        }
    }

    /// Whether any desired-running VM has no live process (a crash, or a
    /// process that never started). A reservation remains in flight and is
    /// skipped; so is a reservation that is clearly wedged past its own
    /// start deadline.
    async fn needs_reconcile(&self) -> bool {
        let state = self.state.lock().await;
        state.desired.vms.iter().any(|vm| {
            if vm.desired != crate::model::DesiredState::Running {
                return false;
            }
            match state.running.get(&vm.name) {
                None => true,
                Some(running) if running.alive() => false,
                Some(running) if running.reserved() => running.started_at.elapsed() > STALE_START,
                Some(_) => true,
            }
        })
    }

    /// The VNC TCP port a *running* VM is bound to, for the console tunnel.
    /// The name → display binding is the supervisor's, not the caller's: a
    /// valid token must not turn the tunnel into a proxy for another VM's
    /// (or an arbitrary loopback) port.
    pub async fn vnc_port_for(&self, name: &str) -> Option<u16> {
        let state = self.state.lock().await;
        state
            .running
            .get(name)
            .filter(|running| running.alive())
            .map(|running| 5900 + running.vnc_display)
    }

    /// Which running VM (if any) attaches a library file, for the image
    /// delete/overwrite guard.
    pub async fn image_in_use(&self, pool: &str, name: &str) -> Option<String> {
        let reference = format!("isos/{name}");
        let state = self.state.lock().await;
        state.running.iter().find_map(|(vm, running)| {
            if running.exited.is_some() || running.spec.pool != pool {
                return None;
            }
            if running.spec.iso.as_deref() == Some(reference.as_str())
                || running.spec.disk.source_image.as_deref() == Some(reference.as_str())
            {
                let state = if running.reserved() {
                    "starting"
                } else {
                    "running"
                };
                return Some(format!("VM {vm} is {state}"));
            }
            None
        })
    }

    #[cfg(test)]
    pub async fn test_register_running(&self, name: &str, vnc_display: u16) {
        let child = tokio::process::Command::new("sleep")
            .arg("60")
            .kill_on_drop(true)
            .spawn()
            .expect("spawn the test process");
        let spec = VmSpec {
            name: name.to_string(),
            cpu: 1,
            mem_mib: 16,
            autostart: false,
            firmware: Firmware::Bios,
            secure_boot: false,
            desired: crate::model::DesiredState::Stopped,
            disk: crate::model::DiskSpec::default(),
            net: crate::model::NetSpec::default(),
            cloud_init: None,
            iso: None,
            pool: "tank".to_string(),
        };
        self.state.lock().await.running.insert(
            name.to_string(),
            Running {
                child: Some(child),
                spec,
                vnc_display,
                vnc_password: String::new(),
                started_at: Instant::now(),
                exited: None,
                starting: false,
                token: 0,
                stopping: false,
                last_error: String::new(),
                unexpected_exit: false,
            },
        );
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
        && running.spec.secure_boot == desired.secure_boot
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

/// Refuse a library file that is a symlink (or not a regular file); a
/// share-writable library directory must not redirect QEMU/qemu-img outside
/// the pool.
async fn require_library_file(path: &Path, what: &str) -> Result<(), String> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(meta) if meta.file_type().is_symlink() => Err(format!(
            "refusing to use the symlink at {}: the {what} must be a regular file",
            path.display()
        )),
        Ok(meta) if meta.is_file() => Ok(()),
        Ok(_) => Err(format!(
            "{} exists but is not a regular file",
            path.display()
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(format!(
            "the {what} {} is not in the library",
            path.display()
        )),
        Err(e) => Err(format!("checking {}: {e}", path.display())),
    }
}

/// Create a file that must not already exist, without following a symlink.
async fn write_new_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .await
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                format!(
                    "{} already exists (refusing to overwrite it)",
                    path.display()
                )
            } else {
                format!("creating {}: {e}", path.display())
            }
        })?;
    file.write_all(bytes)
        .await
        .map_err(|e| format!("writing {}: {e}", path.display()))?;
    file.sync_all()
        .await
        .map_err(|e| format!("syncing {}: {e}", path.display()))
}

/// Open `qemu.log` without following a symlink. The log is truncated on
/// every start, so a planted link would otherwise truncate an arbitrary file.
fn open_qemu_log(path: &Path) -> Result<std::fs::File, String> {
    use std::os::unix::fs::OpenOptionsExt;
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(format!(
                "refusing to use the symlink at {}: the QEMU log must be a regular file",
                path.display()
            ))
        }
        Ok(meta) if !meta.is_file() => {
            return Err(format!(
                "{} exists but is not a regular file",
                path.display()
            ))
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("checking {}: {e}", path.display())),
    }
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| format!("opening {}: {e}", path.display()))
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
async fn wait_for_dir(dir: &Path) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(DATASET_WAIT_SECS);
    loop {
        match tokio::fs::symlink_metadata(dir).await {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(format!(
                    "refusing to use the symlink at {}: the VM dataset directory must be a real directory",
                    dir.display()
                ))
            }
            Ok(meta) if meta.is_dir() => return Ok(()),
            _ => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "the VM dataset directory {} is not visible to vmd after {DATASET_WAIT_SECS}s (was the dataset created?)",
                dir.display()
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

fn tail_qemu_log(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("qemu.log"))
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

fn vnc_password() -> Result<String, String> {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghjkmnpqrstuvwxyz23456789";
    let mut raw = [0u8; VNC_PASSWORD_LEN];
    // Fail closed: a predictable password would make the console's only
    // authentication layer worthless, so a start that cannot get entropy is
    // refused instead of falling back to zeros.
    getrandom::fill(&mut raw).map_err(|e| format!("generating the VNC password: {e}"))?;
    Ok(raw
        .iter()
        .map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char)
        .collect())
}

/// The background watchdog: reaps children, keeps state fresh even when
/// nobody is calling `live()`, and reconciles a desired-running VM that has
/// no live process (a crashed QEMU would otherwise stay down until the next
/// config push).
pub async fn run_watchdog(supervisor: Arc<Supervisor>) {
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(30));
    ticker.tick().await;
    loop {
        ticker.tick().await;
        supervisor.watchdog_tick().await;
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
            secure_boot: false,
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

    /// A hermetic supervisor: a temp datasets root with a pre-created disk (so
    /// no qemu-img runs), a fake QEMU script that records each spawn, and a
    /// short QMP wait.
    struct TestEnv {
        _root: tempfile::TempDir,
        datasets: PathBuf,
        run_dir: PathBuf,
        starts_log: PathBuf,
        qemu: String,
    }

    fn test_env(name: &str, script_body: &str) -> TestEnv {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let datasets = root.path().join("mnt");
        let run_dir = root.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        let vm_dir = datasets.join("tank/vms").join(name);
        std::fs::create_dir_all(&vm_dir).unwrap();
        // A pre-existing disk skips qemu-img; the fake QEMU never opens it.
        std::fs::write(vm_dir.join("disk.qcow2"), b"").unwrap();
        let starts_log = root.path().join("starts.log");
        let script = root.path().join("fake-qemu.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$$\" >> '{}'\n{script_body}\n",
                starts_log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        TestEnv {
            _root: root,
            datasets,
            run_dir,
            starts_log,
            qemu: script.to_string_lossy().into_owned(),
        }
    }

    fn started_count(log: &Path) -> usize {
        std::fs::read_to_string(log)
            .map(|content| content.lines().filter(|l| !l.trim().is_empty()).count())
            .unwrap_or(0)
    }

    fn running_spec(name: &str) -> VmSpec {
        let mut vm = spec(name);
        vm.desired = DesiredState::Running;
        vm.firmware = Firmware::Bios;
        vm.cloud_init = None;
        vm
    }

    async fn wait_for_attempts(log: &Path, wanted: usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while started_count(log) < wanted {
            assert!(
                Instant::now() < deadline,
                "expected {wanted} QEMU starts, saw {}",
                started_count(log)
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn concurrent_pushes_start_a_vm_once() {
        let env = test_env("slow-vm", "exec sleep 5");
        let sup = Arc::new(Supervisor::for_test(
            env.run_dir.clone(),
            env.datasets.clone(),
            env.qemu.clone(),
            Duration::from_millis(100),
        ));
        let first = DesiredConfig {
            revision: "r1".into(),
            vms: vec![running_spec("slow-vm")],
        };
        let second = DesiredConfig {
            revision: "r2".into(),
            vms: vec![running_spec("slow-vm")],
        };

        // Two pushes of the same desired-running VM. Each detaches a
        // reconcile; the reservation and the reconcile lock must still
        // produce exactly one QEMU process.
        let (a, b) = tokio::join!(sup.clone().apply(first), sup.clone().apply(second));
        assert!(a.is_ok(), "{a:?}");
        assert!(b.is_ok(), "{b:?}");
        wait_for_attempts(&env.starts_log, 1).await;
        // Give a second (buggy) start time to spawn.
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(
            started_count(&env.starts_log),
            1,
            "two pushes must start the VM once"
        );
        let live = sup.live().await;
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].state, "running");
    }

    async fn wait_for_ghost_error(sup: &Supervisor, name: &str, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let live = sup.live().await;
            if let Some(vm) = live.iter().find(|vm| vm.name == name) {
                if vm.state == "stopped" && vm.last_error.contains(needle) {
                    return;
                }
            }
            assert!(
                Instant::now() < deadline,
                "no stopped {name} with {needle:?}: {live:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn a_symlinked_disk_or_log_refuses_the_start() {
        // A planted disk symlink must be refused before qemu-img/QEMU touch
        // it; the target stays intact.
        let env = test_env("sym-vm", "exec sleep 5");
        let vm_dir = env.datasets.join("tank/vms/sym-vm");
        std::fs::remove_file(vm_dir.join("disk.qcow2")).unwrap();
        let outside = env._root.path().join("outside.img");
        std::fs::write(&outside, b"keep").unwrap();
        std::os::unix::fs::symlink(&outside, vm_dir.join("disk.qcow2")).unwrap();
        let sup = Arc::new(Supervisor::for_test(
            env.run_dir.clone(),
            env.datasets.clone(),
            env.qemu.clone(),
            Duration::from_millis(50),
        ));
        let config = DesiredConfig {
            revision: "sym-1".into(),
            vms: vec![running_spec("sym-vm")],
        };
        sup.clone().apply(config).await.unwrap();
        wait_for_ghost_error(&sup, "sym-vm", "symlink").await;
        assert_eq!(started_count(&env.starts_log), 0, "QEMU must not start");
        assert_eq!(std::fs::read(&outside).unwrap(), b"keep");

        // The same for the log, which is opened with O_TRUNC on every start.
        let env = test_env("sym-log-vm", "exec sleep 5");
        let vm_dir = env.datasets.join("tank/vms/sym-log-vm");
        let outside = env._root.path().join("outside.log");
        std::fs::write(&outside, b"keep").unwrap();
        std::os::unix::fs::symlink(&outside, vm_dir.join("qemu.log")).unwrap();
        let sup = Arc::new(Supervisor::for_test(
            env.run_dir.clone(),
            env.datasets.clone(),
            env.qemu.clone(),
            Duration::from_millis(50),
        ));
        let config = DesiredConfig {
            revision: "sym-2".into(),
            vms: vec![running_spec("sym-log-vm")],
        };
        sup.clone().apply(config).await.unwrap();
        wait_for_ghost_error(&sup, "sym-log-vm", "symlink").await;
        assert_eq!(started_count(&env.starts_log), 0, "QEMU must not start");
        assert_eq!(std::fs::read(&outside).unwrap(), b"keep");
    }

    #[tokio::test]
    async fn a_running_vm_blocks_its_library_image() {
        let env = test_env("iso-vm", "exec sleep 5");
        let isos = env.datasets.join("tank/vms/isos");
        std::fs::create_dir_all(&isos).unwrap();
        std::fs::write(isos.join("debian.iso"), b"iso").unwrap();
        let sup = Arc::new(Supervisor::for_test(
            env.run_dir.clone(),
            env.datasets.clone(),
            env.qemu.clone(),
            Duration::from_millis(50),
        ));
        let mut vm = running_spec("iso-vm");
        vm.iso = Some("isos/debian.iso".into());
        let config = DesiredConfig {
            revision: "iso-1".into(),
            vms: vec![vm],
        };
        sup.clone().apply(config).await.unwrap();
        wait_for_attempts(&env.starts_log, 1).await;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if sup.live().await.iter().any(|vm| vm.state == "running") {
                break;
            }
            assert!(Instant::now() < deadline, "VM never reached running");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            sup.image_in_use("tank", "debian.iso").await.as_deref(),
            Some("VM iso-vm is running")
        );
        assert!(sup.image_in_use("tank", "other.iso").await.is_none());
        assert!(sup.image_in_use("other-pool", "debian.iso").await.is_none());
    }

    #[tokio::test]
    async fn the_watchdog_restarts_a_crashed_vm() {
        let env = test_env("crash-vm", "exit 7");
        let sup = Arc::new(Supervisor::for_test(
            env.run_dir.clone(),
            env.datasets.clone(),
            env.qemu.clone(),
            Duration::from_millis(50),
        ));
        let config = DesiredConfig {
            revision: "crash-1".into(),
            vms: vec![running_spec("crash-vm")],
        };
        sup.clone().apply(config).await.unwrap();

        // The first start fails and is parked as a ghost. Before the fix a
        // reaped entry looked alive to start()/stop_one and the VM stayed
        // down forever.
        wait_for_attempts(&env.starts_log, 1).await;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let live = sup.live().await;
            if live.len() == 1 && live[0].state == "stopped" && !live[0].last_error.is_empty() {
                break;
            }
            assert!(Instant::now() < deadline, "no exit reason was recorded");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let before = started_count(&env.starts_log);
        assert_eq!(before, 1);
        // One watchdog pass must notice the desired-running VM has no live
        // process and start it again (which crashes again, cleanly).
        sup.watchdog_tick().await;
        wait_for_attempts(&env.starts_log, 2).await;
        assert_eq!(started_count(&env.starts_log), 2);
        let live = sup.live().await;
        assert_eq!(live[0].state, "stopped");
        assert!(
            live[0].last_error.contains("exited"),
            "{:?}",
            live[0].last_error
        );
    }

    #[test]
    fn the_vnc_tunnel_range_is_exactly_the_configured_displays() {
        let sup = supervisor(); // first display 1, 16 displays
        assert!(sup.vnc_port_in_range(5901));
        assert!(sup.vnc_port_in_range(5916));
        assert!(!sup.vnc_port_in_range(5900));
        assert!(!sup.vnc_port_in_range(5917));
        assert!(!sup.vnc_port_in_range(22));
    }

    #[test]
    fn vnc_passwords_are_8_alphanumeric_chars() {
        let password = vnc_password().unwrap();
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

    #[test]
    fn cpu_percent_of_a_nonexistent_pid_is_zero() {
        let percent = cpu_percent(u32::MAX, 4, std::time::Instant::now());
        assert_eq!(
            percent, 0.0,
            "/proc must not exist for an impossible pid, so cpu_percent must be 0.0"
        );
    }

    #[test]
    fn cpu_percent_of_this_process_stays_within_the_cpu_cap() {
        let percent = cpu_percent(std::process::id(), 4, std::time::Instant::now());
        assert!(
            (0.0..=400.0).contains(&percent),
            "cpu_percent with 4 vCPUs must stay within 0..=400, got {percent}"
        );
        assert!(
            percent.is_finite(),
            "cpu_percent must be finite, got {percent}"
        );
    }

    #[test]
    fn cpu_percent_with_zero_cpus_caps_at_one_cpu() {
        // The cap is max(1) * 100, so a zero vCPU count may not exceed 100%.
        let percent = cpu_percent(std::process::id(), 0, std::time::Instant::now());
        assert!(
            (0.0..=100.0).contains(&percent),
            "cpu_percent with 0 vCPUs must stay within 0..=100, got {percent}"
        );
    }
}
