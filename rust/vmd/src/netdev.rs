//! LAN (macvtap) networking: one macvtap device per running VM, bridged onto
//! the physical NIC so the guest gets its own DHCP address on the LAN.
//!
//! vmd is the only creator of `mtap-*` devices on the node, so an orphaned
//! device from a crashed pod is reclaimed by the boot cleanup
//! (`remove_all_macvtap`). The macvtap host-isolation caveat (the NAS host
//! itself cannot talk to macvtap guests) is documented in the UI and
//! docs/virtual-machines.md; it is a macvtap property, not a Naslos bug.

use sha2::{Digest, Sha256};
use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::Path;

/// Device name for a VM: `mtap-` plus 8 hex chars of the VM name hash,
/// deterministic and inside IFNAMSIZ (15).
pub fn device_name(vm: &str) -> String {
    let hash = Sha256::digest(vm.as_bytes());
    format!(
        "mtap-{:02x}{:02x}{:02x}{:02x}",
        hash[0], hash[1], hash[2], hash[3]
    )
}

async fn run(argv: &[&str]) -> Result<(), String> {
    let output = tokio::process::Command::new(crate::model::ip_binary())
        .args(argv)
        .output()
        .await
        .map_err(|e| format!("running {}: {e}", crate::model::ip_binary()))?;
    if !output.status.success() {
        return Err(format!(
            "ip {} failed: {}",
            argv.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

/// Create (idempotently) the macvtap device for a VM on `nic` and return the
/// device name and its ifindex.
pub async fn ensure_macvtap(nic: &str, vm: &str) -> Result<(String, u32), String> {
    if !Path::new(&format!("/sys/class/net/{nic}")).exists() {
        return Err(format!("interface {nic:?} does not exist on the node"));
    }
    let dev = device_name(vm);
    if !Path::new(&format!("/sys/class/net/{dev}")).exists() {
        run(&[
            "link", "add", &dev, "link", nic, "type", "macvtap", "mode", "bridge",
        ])
        .await?;
    }
    run(&["link", "set", &dev, "up"]).await?;
    let ifindex = std::fs::read_to_string(format!("/sys/class/net/{dev}/ifindex"))
        .map_err(|e| format!("reading ifindex of {dev}: {e}"))?
        .trim()
        .parse::<u32>()
        .map_err(|e| format!("parsing ifindex of {dev}: {e}"))?;
    Ok((dev, ifindex))
}

/// Open the macvtap character device; QEMU inherits the fd via
/// `-netdev tap,fd=N`.
pub fn open_tap_fd(ifindex: u32) -> Result<File, String> {
    File::open(format!("/dev/tap{ifindex}"))
        .map_err(|e| format!("opening /dev/tap{ifindex}: {e} (is macvtap enabled in the kernel?)"))
}

/// The opened fd number, for `-netdev tap,fd=`.
pub fn tap_fd_number(file: &File) -> i32 {
    file.as_raw_fd()
}

/// Remove a VM's macvtap device.
pub async fn remove_macvtap(vm: &str) -> Result<(), String> {
    let dev = device_name(vm);
    if Path::new(&format!("/sys/class/net/{dev}")).exists() {
        run(&["link", "del", &dev]).await?;
    }
    Ok(())
}

/// Boot cleanup: every `mtap-*` device on the node belongs to a previous vmd
/// life (vmd is the only creator), so delete them all before reconciling.
pub async fn remove_all_macvtap() {
    let entries = match std::fs::read_dir("/sys/class/net") {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("mtap-") {
            if let Err(e) = run(&["link", "del", &name]).await {
                tracing::warn!("removing stale macvtap device {name}: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_names_are_stable_and_short() {
        let dev = device_name("my-vm");
        assert!(dev.starts_with("mtap-"));
        assert!(dev.len() <= 15, "must fit IFNAMSIZ: {dev}");
        assert_eq!(dev, device_name("my-vm"));
        assert_ne!(dev, device_name("other"));
        // Long VM names still produce short devices.
        assert!(device_name(&"x".repeat(63)).len() <= 15);
    }
}
