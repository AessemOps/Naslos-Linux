//! The vmd data model: the desired VM state the API renders, validates and
//! pushes (`PUT /api/v1/vms/config`), plus the live-state reports vmd answers
//! with. `state` is always derived from vmd and never persisted.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Firmware {
    #[default]
    Uefi,
    Bios,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DiskFormat {
    #[default]
    Qcow2,
    Raw,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DiskBus {
    #[default]
    Virtio,
    Sata,
}

/// Guest networking is NAT (slirp) only; a host-bridge LAN mode is planned
/// for a later release (see docs/spec.md FR-VMT-04).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum NetMode {
    #[default]
    Nat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DesiredState {
    Running,
    #[default]
    Stopped,
}

fn default_format() -> DiskFormat {
    DiskFormat::Qcow2
}

fn default_bus() -> DiskBus {
    DiskBus::Virtio
}

fn default_firmware() -> Firmware {
    Firmware::Uefi
}

fn default_cpu() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PortForward {
    pub host: u16,
    pub dst: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct DiskSpec {
    #[serde(rename = "sizeGB", default)]
    pub size_gb: u32,
    #[serde(default = "default_format")]
    pub format: DiskFormat,
    #[serde(default = "default_bus")]
    pub bus: DiskBus,
    /// Library file (`isos/<file>`) the disk is initialized from on first
    /// start (a cloud image). The copy is resized to `sizeGB`.
    #[serde(
        rename = "sourceImage",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub source_image: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct NetSpec {
    /// NAT is the only mode; the field stays on the wire for forward
    /// compatibility with a future host-bridge mode.
    #[serde(default)]
    pub mode: NetMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forwards: Vec<PortForward>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CloudInitSpec {
    #[serde(default)]
    pub hostname: String,
    #[serde(default)]
    pub user: String,
    #[serde(default, rename = "sshKeys", skip_serializing_if = "Vec::is_empty")]
    pub ssh_keys: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmSpec {
    pub name: String,
    #[serde(default = "default_cpu")]
    pub cpu: u32,
    #[serde(rename = "memMiB", default)]
    pub mem_mib: u32,
    #[serde(default)]
    pub autostart: bool,
    #[serde(default = "default_firmware")]
    pub firmware: Firmware,
    /// Boot with OVMF Secure Boot (UEFI only; default off so existing VMs
    /// keep booting the way they were created).
    #[serde(rename = "secureBoot", default)]
    pub secure_boot: bool,
    #[serde(default)]
    pub desired: DesiredState,
    #[serde(default)]
    pub disk: DiskSpec,
    #[serde(default)]
    pub net: NetSpec,
    #[serde(rename = "cloudInit", default, skip_serializing_if = "Option::is_none")]
    pub cloud_init: Option<CloudInitSpec>,
    /// Library file to attach as a CD-ROM (`isos/<file>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iso: Option<String>,
    /// ZFS pool the VM's dataset lives in.
    #[serde(default)]
    pub pool: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DesiredConfig {
    pub revision: String,
    #[serde(default)]
    pub vms: Vec<VmSpec>,
}

/// One VM's live state, as reported by `GET /api/v1/vms`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct VmLive {
    pub name: String,
    /// `running` or `stopped`.
    pub state: String,
    pub pid: Option<u32>,
    /// TCP port of the VNC server while running (5900 + display).
    #[serde(rename = "vncPort")]
    pub vnc_port: Option<u16>,
    /// The VNC password vmd set via QMP; empty while stopped.
    #[serde(rename = "vncPassword")]
    pub vnc_password: String,
    /// Live CPU usage percent (0 when not running).
    #[serde(rename = "cpuPercent")]
    pub cpu_percent: f64,
    /// Live guest memory usage in MiB from the balloon (0 when not running).
    #[serde(rename = "memUsedMiB")]
    pub mem_used_mib: u32,
    /// The QEMU exit was not requested by desired state.
    #[serde(rename = "unexpectedExit")]
    pub unexpected_exit: bool,
    /// Last start error / exit reason.
    #[serde(rename = "lastError")]
    pub last_error: String,
}

/// A file in the ISO/image library.
#[derive(Debug, Clone, Serialize)]
pub struct ImageEntry {
    pub name: String,
    #[serde(rename = "sizeBytes")]
    pub size_bytes: u64,
}

/// The pool datasets are mounted under on the host (`/var/mnt`), the datasets
/// root every share path already uses.
pub const DATASETS_ROOT: &str = "/var/mnt";

/// Host path of a VM's dataset directory. `root` is the datasets root
/// (`/var/mnt` in production) so tests never touch the host layout.
pub fn vm_dir(root: &Path, pool: &str, name: &str) -> PathBuf {
    root.join(pool).join("vms").join(name)
}

/// Host path of the image library of one pool.
pub fn isos_dir(root: &Path, pool: &str) -> PathBuf {
    root.join(pool).join("vms").join("isos")
}

/// A VM record references a library file as `isos/<file>` (the DM-7 shape);
/// this validates the reference and returns the bare file name.
pub fn library_file(reference: &str) -> Result<String, String> {
    let Some(name) = reference.strip_prefix("isos/") else {
        return Err(format!(
            "library reference {reference:?} must start with \"isos/\""
        ));
    };
    if !valid_library_name(name) {
        return Err(format!("{name:?} is not a valid library file name"));
    }
    Ok(name.to_string())
}

/// Host path of a bare library file name under one pool.
pub fn library_path(root: &Path, pool: &str, name: &str) -> PathBuf {
    isos_dir(root, pool).join(name)
}

/// Upload-session owner tokens: 1-128 printable ASCII characters (the Images
/// panel sends a UUID). Shared with the API so both layers reject the same
/// values before vmd has to explain itself.
pub fn valid_upload_owner(owner: &str) -> bool {
    !owner.is_empty() && owner.len() <= 128 && owner.chars().all(|c| c.is_ascii_graphic())
}

/// Library file names are flat basenames; no path traversal may reach the
/// command line or the filesystem (SEC-1).
pub fn valid_library_name(file: &str) -> bool {
    if file.is_empty() || file.len() > 200 {
        return false;
    }
    if file.starts_with('.') || file.contains('/') || file.contains('\\') {
        return false;
    }
    // `.part` is vmd's upload/import staging suffix; a library name ending in
    // it could collide with an in-flight staging file.
    if file.ends_with(".part") {
        return false;
    }
    file.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+'))
}

/// Validate one VM spec. The API validates first; vmd re-validates everything
/// so a bad render can never reach a QEMU command line (SEC-1 pattern).
pub fn validate_vm(spec: &VmSpec) -> Result<(), String> {
    validate_name(&spec.name)?;
    if spec.name == "isos" {
        // `<pool>/vms/isos` is the image library dataset: a VM with that name
        // would alias it, and deleting the VM would destroy the library.
        return Err("the name \"isos\" is reserved for the image library dataset".to_string());
    }
    if spec.pool.is_empty() || !valid_pool_component(&spec.pool) {
        return Err(format!(
            "pool {:?} is not a valid dataset component",
            spec.pool
        ));
    }
    if !(1..=64).contains(&spec.cpu) {
        return Err(format!("cpu {} is out of range (1-64)", spec.cpu));
    }
    if !(16..=1_048_576).contains(&spec.mem_mib) {
        return Err(format!(
            "memMiB {} is out of range (16-1048576)",
            spec.mem_mib
        ));
    }
    if spec.secure_boot && spec.firmware != Firmware::Uefi {
        return Err("secureBoot requires UEFI firmware".to_string());
    }
    if !(1..=16_384).contains(&spec.disk.size_gb) {
        return Err(format!(
            "disk sizeGB {} is out of range (1-16384)",
            spec.disk.size_gb
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for fwd in &spec.net.forwards {
        if fwd.host == 0 {
            return Err("port forward host must not be 0".to_string());
        }
        if fwd.dst == 0 {
            return Err("port forward dst must not be 0".to_string());
        }
        if !seen.insert(fwd.host) {
            return Err(format!("duplicate host port {}", fwd.host));
        }
    }
    if let Some(iso) = &spec.iso {
        library_file(iso).map_err(|e| format!("iso {e}"))?;
    }
    if let Some(source) = &spec.disk.source_image {
        library_file(source).map_err(|e| format!("disk sourceImage {e}"))?;
    }
    if let Some(ci) = &spec.cloud_init {
        validate_cloud_init(ci)?;
    }
    Ok(())
}

/// DNS-1123 label (the chart/catalog convention).
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 63 {
        return Err(format!("VM name {name:?} must be 1-63 characters"));
    }
    let bytes = name.as_bytes();
    if !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit() {
        return Err(format!(
            "VM name {name:?} must start with a lowercase letter or digit"
        ));
    }
    if !bytes[bytes.len() - 1].is_ascii_lowercase() && !bytes[bytes.len() - 1].is_ascii_digit() {
        return Err(format!(
            "VM name {name:?} must end with a lowercase letter or digit"
        ));
    }
    if !bytes
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
    {
        return Err(format!(
            "VM name {name:?} must be a lowercase DNS-1123 label"
        ));
    }
    Ok(())
}

/// A pool name is a single dataset component (flat, no '/', and never `.` or
/// `..` — the traversal pair would escape the datasets root).
pub fn valid_pool_component(pool: &str) -> bool {
    if pool.is_empty() || pool.len() > 63 || pool.contains('/') || pool == "." || pool == ".." {
        return false;
    }
    let bytes = pool.as_bytes();
    // A leading alphanumeric keeps `.hidden`-style and `..x` components out.
    if !bytes[0].is_ascii_alphanumeric() {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn validate_cloud_init(ci: &CloudInitSpec) -> Result<(), String> {
    if ci.hostname.is_empty() || ci.hostname.len() > 63 {
        return Err("cloud-init hostname is required (1-63 characters)".to_string());
    }
    if !ci
        .hostname
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.'))
    {
        return Err(format!(
            "cloud-init hostname {:?} has invalid characters",
            ci.hostname
        ));
    }
    let user = ci.user.as_str();
    if user.is_empty()
        || user.len() > 32
        || !user
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-'))
        || user
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(true)
    {
        return Err(format!(
            "cloud-init user {user:?} must be a lowercase POSIX user name (letters, digits, '_', '-')"
        ));
    }
    if ci.ssh_keys.len() > 20 {
        return Err("cloud-init accepts at most 20 SSH keys".to_string());
    }
    for key in &ci.ssh_keys {
        // YAML treats \r, \t and the Unicode line separators as structure, not
        // as data. A key is a single line; any control character could break
        // out of the scalar and inject another cloud-config directive.
        let has_control = key
            .chars()
            .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'));
        if key.len() > 8192 || has_control || !key.starts_with("ssh-") {
            let shown: String = key.chars().take(24).collect();
            return Err(format!(
                "SSH key {shown:?}… must be one public key line (ssh-ed25519/ssh-rsa/…), without control characters"
            ));
        }
    }
    Ok(())
}

/// Snapshot names are DNS-ish with an optional timestamp suffix
/// (`weekly-2026-11-19`). Flat, no `/`, no `@`.
pub fn validate_snapshot_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 63 {
        return Err(format!("snapshot name {name:?} must be 1-63 characters"));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(format!(
            "snapshot name {name:?} may only contain letters, digits, '-', '_' and '.'"
        ));
    }
    Ok(())
}

/// The QEMU binary resolution (overridable for tests).
pub fn qemu_binary() -> String {
    std::env::var("VMD_QEMU_BIN").unwrap_or_else(|_| "qemu-system-x86_64".to_string())
}

/// `qemu-img` resolution (overridable for tests).
pub fn qemu_img_binary() -> String {
    std::env::var("VMD_QEMU_IMG_BIN").unwrap_or_else(|_| "qemu-img".to_string())
}

/// `genisoimage` resolution (overridable for tests).
pub fn genisoimage_binary() -> String {
    std::env::var("VMD_GENISOIMAGE_BIN").unwrap_or_else(|_| "genisoimage".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vm() -> VmSpec {
        VmSpec {
            name: "test-vm".into(),
            cpu: 2,
            mem_mib: 2048,
            autostart: false,
            firmware: Firmware::Uefi,
            secure_boot: false,
            desired: DesiredState::Running,
            disk: DiskSpec {
                size_gb: 40,
                format: DiskFormat::Qcow2,
                bus: DiskBus::Virtio,
                source_image: None,
            },
            net: NetSpec {
                mode: NetMode::Nat,
                forwards: vec![PortForward {
                    host: 8081,
                    dst: 80,
                }],
            },
            cloud_init: Some(CloudInitSpec {
                hostname: "test-vm".into(),
                user: "admin".into(),
                ssh_keys: vec!["ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExample test@example".into()],
            }),
            iso: Some("isos/debian-13.iso".into()),
            pool: "tank".into(),
        }
    }

    #[test]
    fn a_valid_spec_passes() {
        assert!(validate_vm(&vm()).is_ok());
    }

    #[test]
    fn the_image_library_dataset_name_is_reserved() {
        let mut spec = vm();
        spec.name = "isos".into();
        assert!(validate_vm(&spec)
            .unwrap_err()
            .contains("reserved for the image library"));
    }

    #[test]
    fn pools_must_be_flat_components() {
        for bad in [
            "",
            ".",
            "..",
            "../tank",
            ".hidden",
            "-lead",
            "a/b",
            &"x".repeat(64),
        ] {
            assert!(
                !valid_pool_component(bad),
                "{bad:?} must not be a valid pool component"
            );
        }
        for good in ["tank", "Data", "ssd-storage", "pool2", "a.b"] {
            assert!(valid_pool_component(good), "{good:?} must be valid");
        }
        // A traversal pool is rejected at validation time, before any path is
        // built from it.
        let mut spec = vm();
        spec.pool = "..".into();
        assert!(validate_vm(&spec).is_err());
    }

    #[test]
    fn names_must_be_dns1123() {
        for bad in ["", "UPPER", "-x", "x-", "a_b", "a b", &"x".repeat(64)] {
            let mut spec = vm();
            spec.name = bad.to_string();
            assert!(validate_vm(&spec).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn secure_boot_requires_uefi() {
        let mut spec = vm();
        spec.secure_boot = true;
        assert!(validate_vm(&spec).is_ok());
        spec.firmware = Firmware::Bios;
        let err = validate_vm(&spec).unwrap_err();
        assert!(err.contains("secureBoot requires UEFI"), "{err}");
    }

    #[test]
    fn secure_boot_defaults_off_for_legacy_records() {
        // Records persisted before the field existed must load as off.
        let legacy = serde_json::json!({
            "name": "legacy",
            "cpu": 1,
            "memMiB": 512,
            "pool": "tank",
            "disk": { "sizeGB": 10 }
        });
        let spec: VmSpec = serde_json::from_value(legacy).unwrap();
        assert!(!spec.secure_boot);
    }

    #[test]
    fn ranges_are_enforced() {
        for (cpu, ok) in [(0, false), (1, true), (64, true), (65, false)] {
            let mut spec = vm();
            spec.cpu = cpu;
            assert_eq!(validate_vm(&spec).is_ok(), ok, "cpu {cpu}");
        }
        for (mib, ok) in [
            (8, false),
            (16, true),
            (1_048_576, true),
            (1_048_577, false),
        ] {
            let mut spec = vm();
            spec.mem_mib = mib;
            assert_eq!(validate_vm(&spec).is_ok(), ok, "mem {mib}");
        }
        for (gb, ok) in [(0, false), (1, true), (16_384, true), (16_385, false)] {
            let mut spec = vm();
            spec.disk.size_gb = gb;
            assert_eq!(validate_vm(&spec).is_ok(), ok, "size {gb}");
        }
    }

    #[test]
    fn upload_owner_tokens_are_printable_ascii() {
        assert!(valid_upload_owner("6c1f3a4e-0b2d-4c3f-9a8e-0123456789ab"));
        assert!(!valid_upload_owner(""));
        assert!(!valid_upload_owner("two words"));
        assert!(!valid_upload_owner("café"));
        assert!(!valid_upload_owner("line\nbreak"));
        assert!(!valid_upload_owner(&"x".repeat(129)));
        assert!(valid_upload_owner(&"x".repeat(128)));
    }

    #[test]
    fn library_names_reject_traversal() {
        assert!(valid_library_name("debian-13-netinst.iso"));
        assert!(valid_library_name("ubuntu-24.04.qcow2"));
        for bad in [
            "",
            ".hidden",
            "a/b",
            "..",
            "a\\b",
            "a b",
            "debian.iso.part",
            &"x".repeat(201),
        ] {
            assert!(!valid_library_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn duplicate_host_ports_are_rejected() {
        let mut spec = vm();
        spec.net.forwards.push(PortForward {
            host: 8081,
            dst: 443,
        });
        assert!(validate_vm(&spec).is_err());
    }

    #[test]
    fn cloud_init_users_and_keys_are_validated() {
        let mut spec = vm();
        let mut ci = spec.cloud_init.clone().unwrap();
        ci.user = "Root!".into();
        spec.cloud_init = Some(ci.clone());
        assert!(validate_vm(&spec).is_err());
        ci.user = "root".into();
        ci.ssh_keys = vec!["not-a-key".into()];
        spec.cloud_init = Some(ci.clone());
        assert!(validate_vm(&spec).is_err());
        ci.ssh_keys = (0..21).map(|i| format!("ssh-ed25519 AAAAkey{i}")).collect();
        spec.cloud_init = Some(ci.clone());
        assert!(validate_vm(&spec).is_err());

        // Every YAML line break, not just \n, must be refused: a key is one
        // scalar line and a control character could inject a directive.
        for bad in [
            "ssh-ed25519 AAAAkey\r",
            "ssh-ed25519 AAAAkey\tcomment",
            "ssh-ed25519 AAAAkey\u{0}b",
            "ssh-ed25519 AAAAkey\u{85}next",
            "ssh-ed25519 AAAAkey\u{2028}next",
            "ssh-ed25519 AAAAkey\u{2029}next",
        ] {
            ci.ssh_keys = vec![bad.to_string()];
            spec.cloud_init = Some(ci.clone());
            let err = validate_vm(&spec).expect_err(bad);
            assert!(err.contains("control characters"), "{bad}: {err}");
        }
    }
}
