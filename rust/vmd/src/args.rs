//! The QEMU command-line builder (SEC-1 pattern).
//!
//! Every argument is generated here from a validated `VmSpec`; no user-supplied
//! string ever reaches the command line unvalidated. The golden tests pin the
//! exact argv for the reference VMs so a render change is a reviewable diff.

use crate::model::{DiskBus, DiskFormat, Firmware, VmSpec};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// Everything the builder needs that is not part of the VM model.
#[derive(Debug, Clone, Default)]
pub struct BuildContext {
    /// KVM acceleration is available (`/dev/kvm`).
    pub kvm: bool,
    /// VNC display number; QEMU listens on 5900 + display.
    pub vnc_display: u16,
    /// Directory of the VM's dataset (`/var/mnt/<pool>/vms/<name>`).
    pub vm_dir: PathBuf,
    /// QMP unix socket path for this VM.
    pub qmp_socket: PathBuf,
    /// UEFI code image (pflash, read-only).
    pub uefi_code: Option<PathBuf>,
    /// UEFI vars image (pflash, writable). For a combined OVMF.fd image this
    /// is a per-VM copy made on first start.
    pub uefi_vars: Option<PathBuf>,
    /// OVMF Secure Boot code image (pflash, read-only, `*.secboot.fd`).
    pub secure_boot_code: Option<PathBuf>,
    /// Secure Boot vars template with the Microsoft keys pre-enrolled
    /// (`OVMF_VARS_*.ms.fd`); the supervisor copies it per VM.
    pub secure_boot_vars: Option<PathBuf>,
    /// Generated cloud-init NoCloud seed ISO, when the VM has cloud-init.
    pub cloud_init_seed: Option<PathBuf>,
    /// Absolute path of the install ISO, resolved by the supervisor.
    pub iso_path: Option<PathBuf>,
}

/// The firmware images shipped in the vmd image. Secure Boot is a distinct
/// code/vars pair (SMM-enabled OVMF with pre-enrolled keys); a plain UEFI or
/// combined image cannot be substituted for it.
#[derive(Debug, Clone, Default)]
pub struct FirmwareImages {
    pub code: Option<PathBuf>,
    pub vars: Option<PathBuf>,
    pub secure_boot_code: Option<PathBuf>,
    pub secure_boot_vars: Option<PathBuf>,
}

impl FirmwareImages {
    /// Whether this image can boot a Secure Boot guest.
    pub fn secure_boot_available(&self) -> bool {
        self.secure_boot_code.is_some() && self.secure_boot_vars.is_some()
    }
}

/// Derive a stable MAC from the VM name so DHCP assigns the same address
/// across restarts. Locally administered, QEMU OUI 52:54:00.
pub fn stable_mac(name: &str) -> String {
    let hash = Sha256::digest(name.as_bytes());
    format!("52:54:00:{:02x}:{:02x}:{:02x}", hash[0], hash[1], hash[2])
}

/// Build the full `qemu-system-x86_64` argv for one VM.
pub fn qemu_args(spec: &VmSpec, ctx: &BuildContext) -> Result<Vec<String>, String> {
    if spec.secure_boot && spec.firmware != Firmware::Uefi {
        // validate_vm rejects this; the builder refuses it too so a bad
        // render can never reach QEMU (SEC-1).
        return Err("secureBoot requires UEFI firmware".to_string());
    }
    let disk_path = ctx.vm_dir.join("disk.qcow2");
    let mut args: Vec<String> = Vec::new();

    args.push("-name".into());
    args.push(format!("naslos-vm-{}", spec.name));

    // q35: modern defaults plus the ich9 AHCI controller the `sata` bus maps
    // to. TCG keeps the feature usable for evaluation when KVM is missing.
    // Secure Boot also needs SMM, which OVMF's secboot firmware requires and
    // refuses to boot without.
    args.push("-machine".into());
    args.push(format!(
        "q35,accel={}{}",
        if ctx.kvm { "kvm" } else { "tcg" },
        if spec.secure_boot { ",smm=on" } else { "" }
    ));

    args.push("-cpu".into());
    args.push(if ctx.kvm { "host".into() } else { "max".into() });

    args.push("-smp".into());
    args.push(spec.cpu.to_string());

    args.push("-m".into());
    args.push(spec.mem_mib.to_string());

    args.push("-vga".into());
    args.push("std".into());

    // Balloon device: the memory-usage report comes from query-balloon.
    args.push("-device".into());
    args.push("virtio-balloon-pci".into());

    // Primary disk. QEMU maps `if=ide` on the q35 machine onto the built-in
    // ich9 AHCI (SATA) controller, which is the point of the `sata` bus
    // option: Windows guests without virtio drivers.
    args.push("-drive".into());
    let bus = match spec.disk.bus {
        DiskBus::Virtio => "virtio",
        DiskBus::Sata => "ide",
    };
    args.push(format!(
        "file={},if={},format={},cache=writeback,discard=unmap",
        disk_path.display(),
        bus,
        match spec.disk.format {
            DiskFormat::Qcow2 => "qcow2",
            DiskFormat::Raw => "raw",
        }
    ));

    // Firmware. UEFI boots from pflash; BIOS is QEMU's built-in SeaBIOS.
    match spec.firmware {
        Firmware::Uefi => {
            let (code, vars) = if spec.secure_boot {
                let code = ctx.secure_boot_code.as_ref().ok_or_else(|| {
                    "Secure Boot firmware (OVMF secboot) is not installed in the vmd image"
                        .to_string()
                })?;
                let vars = ctx.secure_boot_vars.as_ref().ok_or_else(|| {
                    "Secure Boot vars template (OVMF ms) is not installed in the vmd image"
                        .to_string()
                })?;
                // SMM must protect the pflash from guest writes for Secure
                // Boot to be meaningful; ICH9 S3 resume is incompatible with
                // the SMM OVMF build and is disabled as the edk2 reference
                // command line does.
                args.push("-global".into());
                args.push("driver=cfi.pflash01,property=secure,value=on".into());
                args.push("-global".into());
                args.push("ICH9-LPC.disable_s3=1".into());
                (code, vars)
            } else {
                let code = ctx
                    .uefi_code
                    .as_ref()
                    .ok_or_else(|| "UEFI firmware is not installed in the vmd image".to_string())?;
                let vars = ctx
                    .uefi_vars
                    .as_ref()
                    .ok_or_else(|| "UEFI vars image is not configured".to_string())?;
                (code, vars)
            };
            let (code_unit, vars_unit) = if spec.secure_boot {
                ("unit=0,", "unit=1,")
            } else {
                ("", "")
            };
            args.push("-drive".into());
            args.push(format!(
                "if=pflash,{code_unit}format=raw,readonly=on,file={}",
                code.display()
            ));
            args.push("-drive".into());
            args.push(format!(
                "if=pflash,{vars_unit}format=raw,file={}",
                vars.display()
            ));
        }
        Firmware::Bios => {}
    }

    // Networking: NAT (slirp) with the VM's TCP port forwards.
    let mac = stable_mac(&spec.name);
    let mut netdev = String::from("user,id=net0");
    for fwd in &spec.net.forwards {
        netdev.push_str(&format!(",hostfwd=tcp::{}-:{}", fwd.host, fwd.dst));
    }
    args.push("-netdev".into());
    args.push(netdev);
    args.push("-device".into());
    args.push(format!("virtio-net-pci,netdev=net0,mac={mac}"));

    // VNC with a per-VM password vmd sets over QMP after start.
    args.push("-vnc".into());
    args.push(format!("0.0.0.0:{},password=on", ctx.vnc_display));

    args.push("-qmp".into());
    args.push(format!("unix:{},server,nowait", ctx.qmp_socket.display()));

    // Install ISO as a CD-ROM.
    if let Some(iso) = &ctx.iso_path {
        args.push("-drive".into());
        args.push(format!("file={},media=cdrom,readonly=on", iso.display()));
    }

    // cloud-init NoCloud seed stays attached; re-attaching is idempotent and
    // the seed carries no secrets.
    if let Some(seed) = &ctx.cloud_init_seed {
        args.push("-drive".into());
        args.push(format!("file={},format=raw,if=virtio", seed.display()));
    }

    args.push("-display".into());
    args.push("none".into());
    args.push("-serial".into());
    args.push("none".into());

    Ok(args)
}

/// Detect the firmware images shipped in the vmd image. Prefers the split
/// code/vars layout; falls back to the combined image (copied per VM on
/// first start by the supervisor). Secure Boot additionally needs the
/// SMM-enabled `*.secboot.fd` code and a vars template with keys enrolled
/// (`*.ms.fd`, or a distro's `*.secboot.fd` vars); a plain UEFI pair is not
/// a Secure Boot pair and is never advertised as one.
pub fn detect_firmware() -> FirmwareImages {
    let (mut code, mut vars) = (None, None);
    if let (Ok(c), Ok(v)) = (
        std::env::var("VMD_UEFI_CODE"),
        std::env::var("VMD_UEFI_VARS"),
    ) {
        if !c.is_empty() && !v.is_empty() {
            code = Some(PathBuf::from(c));
            vars = Some(PathBuf::from(v));
        }
    }
    let candidates: [(&str, &str); 6] = [
        (
            "/usr/share/OVMF/OVMF_CODE_4M.fd",
            "/usr/share/OVMF/OVMF_VARS_4M.fd",
        ),
        (
            "/usr/share/OVMF/OVMF_CODE_2M.fd",
            "/usr/share/OVMF/OVMF_VARS_2M.fd",
        ),
        (
            "/usr/share/OVMF/OVMF_CODE.fd",
            "/usr/share/OVMF/OVMF_VARS.fd",
        ),
        (
            "/usr/share/ovmf/OVMF_CODE_4M.fd",
            "/usr/share/ovmf/OVMF_VARS_4M.fd",
        ),
        (
            "/usr/share/ovmf/OVMF_CODE_2M.fd",
            "/usr/share/ovmf/OVMF_VARS_2M.fd",
        ),
        (
            "/usr/share/ovmf/OVMF_CODE.fd",
            "/usr/share/ovmf/OVMF_VARS.fd",
        ),
    ];
    if code.is_none() {
        for (c, v) in candidates {
            if std::path::Path::new(c).exists() && std::path::Path::new(v).exists() {
                code = Some(PathBuf::from(c));
                vars = Some(PathBuf::from(v));
                break;
            }
        }
    }
    // Combined image: code is the image itself, vars is a per-VM copy of it.
    if code.is_none() {
        for combined in ["/usr/share/ovmf/OVMF.fd", "/usr/share/OVMF/OVMF.fd"] {
            if std::path::Path::new(combined).exists() {
                code = Some(PathBuf::from(combined));
                vars = Some(PathBuf::from(combined));
                break;
            }
        }
    }

    let mut secure_boot_code = None;
    let mut secure_boot_vars = None;
    if let (Ok(c), Ok(v)) = (
        std::env::var("VMD_UEFI_SECBOOT_CODE"),
        std::env::var("VMD_UEFI_SECBOOT_VARS"),
    ) {
        if !c.is_empty() && !v.is_empty() {
            secure_boot_code = Some(PathBuf::from(c));
            secure_boot_vars = Some(PathBuf::from(v));
        }
    }
    if secure_boot_code.is_none() {
        // Debian's ovmf ships OVMF_CODE_4M.secboot.fd + OVMF_VARS_4M.ms.fd
        // on bookworm; the 2M pair and the lowercase directory are covered
        // for other distro layouts. Vars without enrolled keys would leave
        // Secure Boot in setup mode, so only the ms/secboot templates count.
        let candidates: [(&str, &str); 8] = [
            (
                "/usr/share/OVMF/OVMF_CODE_4M.secboot.fd",
                "/usr/share/OVMF/OVMF_VARS_4M.ms.fd",
            ),
            (
                "/usr/share/OVMF/OVMF_CODE_4M.secboot.fd",
                "/usr/share/OVMF/OVMF_VARS_4M.secboot.fd",
            ),
            (
                "/usr/share/OVMF/OVMF_CODE.secboot.fd",
                "/usr/share/OVMF/OVMF_VARS.ms.fd",
            ),
            (
                "/usr/share/OVMF/OVMF_CODE.secboot.fd",
                "/usr/share/OVMF/OVMF_VARS.secboot.fd",
            ),
            (
                "/usr/share/ovmf/OVMF_CODE_4M.secboot.fd",
                "/usr/share/ovmf/OVMF_VARS_4M.ms.fd",
            ),
            (
                "/usr/share/ovmf/OVMF_CODE_4M.secboot.fd",
                "/usr/share/ovmf/OVMF_VARS_4M.secboot.fd",
            ),
            (
                "/usr/share/ovmf/OVMF_CODE.secboot.fd",
                "/usr/share/ovmf/OVMF_VARS.ms.fd",
            ),
            (
                "/usr/share/ovmf/OVMF_CODE.secboot.fd",
                "/usr/share/ovmf/OVMF_VARS.secboot.fd",
            ),
        ];
        for (c, v) in candidates {
            if std::path::Path::new(c).exists() && std::path::Path::new(v).exists() {
                secure_boot_code = Some(PathBuf::from(c));
                secure_boot_vars = Some(PathBuf::from(v));
                break;
            }
        }
    }

    FirmwareImages {
        code,
        vars,
        secure_boot_code,
        secure_boot_vars,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CloudInitSpec, DesiredState, DiskFormat, DiskSpec, NetSpec, PortForward};

    fn ctx() -> BuildContext {
        BuildContext {
            kvm: true,
            vnc_display: 3,
            vm_dir: PathBuf::from("/var/mnt/tank/vms/test-vm"),
            qmp_socket: PathBuf::from("/run/vmd/test-vm.qmp"),
            uefi_code: Some(PathBuf::from("/usr/share/ovmf/OVMF_CODE_4M.fd")),
            uefi_vars: Some(PathBuf::from("/var/mnt/tank/vms/test-vm/vars.fd")),
            secure_boot_code: Some(PathBuf::from("/usr/share/ovmf/OVMF_CODE_4M.secboot.fd")),
            secure_boot_vars: Some(PathBuf::from("/usr/share/ovmf/OVMF_VARS_4M.ms.fd")),
            cloud_init_seed: Some(PathBuf::from("/var/mnt/tank/vms/test-vm/seed.iso")),
            iso_path: Some(PathBuf::from("/var/mnt/tank/vms/isos/debian-13.iso")),
        }
    }

    fn spec(name: &str) -> VmSpec {
        VmSpec {
            name: name.into(),
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
                forwards: vec![PortForward {
                    host: 8081,
                    dst: 80,
                }],
                ..NetSpec::default()
            },
            cloud_init: Some(CloudInitSpec {
                hostname: "test-vm".into(),
                user: "admin".into(),
                ssh_keys: vec!["ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExample t@e".into()],
            }),
            iso: None,
            pool: "tank".into(),
        }
    }

    #[test]
    fn golden_args_uefi_nat() {
        let argv = qemu_args(&spec("test-vm"), &ctx()).unwrap();
        let mac = stable_mac("test-vm");
        let expected: Vec<String> = vec![
            "-name".into(),
            "naslos-vm-test-vm".into(),
            "-machine".into(),
            "q35,accel=kvm".into(),
            "-cpu".into(),
            "host".into(),
            "-smp".into(),
            "2".into(),
            "-m".into(),
            "2048".into(),
            "-vga".into(),
            "std".into(),
            "-device".into(),
            "virtio-balloon-pci".into(),
            "-drive".into(),
            "file=/var/mnt/tank/vms/test-vm/disk.qcow2,if=virtio,format=qcow2,cache=writeback,discard=unmap".into(),
            "-drive".into(),
            "if=pflash,format=raw,readonly=on,file=/usr/share/ovmf/OVMF_CODE_4M.fd".into(),
            "-drive".into(),
            "if=pflash,format=raw,file=/var/mnt/tank/vms/test-vm/vars.fd".into(),
            "-netdev".into(),
            "user,id=net0,hostfwd=tcp::8081-:80".into(),
            "-device".into(),
            format!("virtio-net-pci,netdev=net0,mac={mac}"),
            "-vnc".into(),
            "0.0.0.0:3,password=on".into(),
            "-qmp".into(),
            "unix:/run/vmd/test-vm.qmp,server,nowait".into(),
            "-drive".into(),
            "file=/var/mnt/tank/vms/isos/debian-13.iso,media=cdrom,readonly=on".into(),
            "-drive".into(),
            "file=/var/mnt/tank/vms/test-vm/seed.iso,format=raw,if=virtio".into(),
            "-display".into(),
            "none".into(),
            "-serial".into(),
            "none".into(),
        ];
        assert_eq!(argv, expected);
    }

    #[test]
    fn golden_args_bios_sata() {
        let mut s = spec("bios-vm");
        s.firmware = Firmware::Bios;
        s.disk.bus = DiskBus::Sata;
        s.disk.format = DiskFormat::Raw;
        s.net.forwards.clear();
        s.cloud_init = None;
        let mut c = ctx();
        c.vm_dir = PathBuf::from("/var/mnt/tank/vms/bios-vm");
        c.cloud_init_seed = None;
        c.iso_path = None;
        c.vnc_display = 7;
        let argv = qemu_args(&s, &c).unwrap();
        let joined = argv.join(" ");
        assert!(joined.contains("-machine q35,accel=kvm"), "{joined}");
        // No pflash drives in BIOS mode.
        assert!(!joined.contains("pflash"), "{joined}");
        assert!(joined.contains("if=ide,format=raw"), "{joined}");
        assert!(joined.contains("-netdev user,id=net0"), "{joined}");
        assert!(joined.contains("mac=52:54:00:"), "{joined}");
        assert!(joined.contains("-vnc 0.0.0.0:7,password=on"), "{joined}");
        // No cloud-init seed without cloud-init.
        assert!(!joined.contains("seed.iso"), "{joined}");
    }

    #[test]
    fn tcg_is_rendered_without_kvm() {
        let mut c = ctx();
        c.kvm = false;
        let argv = qemu_args(&spec("test-vm"), &c).unwrap();
        let joined = argv.join(" ");
        assert!(joined.contains("-machine q35,accel=tcg"), "{joined}");
        assert!(joined.contains("-cpu max"), "{joined}");
    }

    #[test]
    fn uefi_without_firmware_is_an_error() {
        let mut c = ctx();
        c.uefi_code = None;
        c.uefi_vars = None;
        assert!(qemu_args(&spec("test-vm"), &c).is_err());
    }

    #[test]
    fn secure_boot_uses_the_secboot_pair_smm_and_secure_pflash() {
        let mut s = spec("sb-vm");
        s.secure_boot = true;
        let mut c = ctx();
        c.vm_dir = PathBuf::from("/var/mnt/tank/vms/sb-vm");
        c.uefi_vars = Some(PathBuf::from("/var/mnt/tank/vms/sb-vm/vars.fd"));
        c.secure_boot_vars = Some(PathBuf::from("/var/mnt/tank/vms/sb-vm/vars.secboot.fd"));
        let argv = qemu_args(&s, &c).unwrap();
        let joined = argv.join(" ");
        assert!(joined.contains("-machine q35,accel=kvm,smm=on"), "{joined}");
        assert!(
            joined.contains("-global driver=cfi.pflash01,property=secure,value=on"),
            "{joined}"
        );
        assert!(joined.contains("-global ICH9-LPC.disable_s3=1"), "{joined}");
        assert!(
            joined.contains(
                "if=pflash,unit=0,format=raw,readonly=on,file=/usr/share/ovmf/OVMF_CODE_4M.secboot.fd"
            ),
            "{joined}"
        );
        assert!(
            joined.contains(
                "if=pflash,unit=1,format=raw,file=/var/mnt/tank/vms/sb-vm/vars.secboot.fd"
            ),
            "{joined}"
        );
        // The non-secure code/vars pair must never leak into a Secure Boot
        // command line.
        assert!(!joined.contains("OVMF_CODE_4M.fd"), "{joined}");
        assert!(!joined.contains("vars.fd "), "{joined}");
        assert!(!joined.contains("OVMF_VARS_4M.ms.fd"), "{joined}");
    }

    #[test]
    fn secure_boot_without_secboot_firmware_is_an_error() {
        let mut s = spec("sb-vm");
        s.secure_boot = true;
        let mut c = ctx();
        c.secure_boot_code = None;
        let err = qemu_args(&s, &c).unwrap_err();
        assert!(err.contains("Secure Boot firmware"), "{err}");
        c.secure_boot_code = ctx().secure_boot_code;
        c.secure_boot_vars = None;
        let err = qemu_args(&s, &c).unwrap_err();
        assert!(err.contains("Secure Boot vars"), "{err}");
    }

    #[test]
    fn secure_boot_with_bios_is_an_error() {
        let mut s = spec("sb-vm");
        s.secure_boot = true;
        s.firmware = Firmware::Bios;
        let err = qemu_args(&s, &ctx()).unwrap_err();
        assert!(err.contains("secureBoot requires UEFI"), "{err}");
    }

    #[test]
    fn firmware_availability_requires_a_secure_pair() {
        let firmware = FirmwareImages {
            code: Some(PathBuf::from("/code.fd")),
            vars: Some(PathBuf::from("/vars.fd")),
            ..FirmwareImages::default()
        };
        assert!(!firmware.secure_boot_available());
        let firmware = FirmwareImages {
            secure_boot_code: Some(PathBuf::from("/secboot-code.fd")),
            secure_boot_vars: Some(PathBuf::from("/ms-vars.fd")),
            ..FirmwareImages::default()
        };
        assert!(firmware.secure_boot_available());
    }

    #[test]
    fn macs_are_stable_and_locally_administered() {
        assert_eq!(stable_mac("test-vm"), stable_mac("test-vm"));
        assert!(stable_mac("test-vm").starts_with("52:54:00:"));
        assert_ne!(stable_mac("test-vm"), stable_mac("other-vm"));
    }

    #[test]
    fn argv_structure_is_flag_value_pairs() {
        // The builder is the last line of defence (SEC-1): every generated
        // value that interpolates model data lands in a value position, so a
        // hostile value could only ever be misparsed if it started with '-'
        // AND sat in a flag position. Pin the structure: odd indices are
        // always literal QEMU options, even indices are the values.
        let argv = qemu_args(&spec("abc"), &ctx()).unwrap();
        assert_eq!(argv.len() % 2, 0, "argv must be flag/value pairs");
        for (i, arg) in argv.iter().enumerate() {
            if i % 2 == 0 {
                assert!(arg.starts_with('-'), "position {i} must be a flag: {arg}");
            }
        }
        // The only model string that reaches a value position is the
        // validated DNS-1123 name, always namespaced by the supervisor.
        assert_eq!(argv[1], "naslos-vm-abc");
    }
}
