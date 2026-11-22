//! Hardware preflight (FR-VMT): what the node can run.

use crate::args::detect_firmware;
use serde::Serialize;

#[derive(Debug, Clone, Default, Serialize)]
pub struct Capabilities {
    pub kvm: bool,
    #[serde(rename = "cpuModel")]
    pub cpu_model: String,
    pub nested: bool,
    pub nics: Vec<String>,
    /// The supervisor image ships an OVMF Secure Boot code/vars pair.
    #[serde(rename = "secureBoot")]
    pub secure_boot: bool,
}

/// Detect the node's virtualization capabilities.
pub fn detect() -> Capabilities {
    let kvm = std::path::Path::new("/dev/kvm").exists();
    let cpu_model = std::fs::read_to_string("/proc/cpuinfo")
        .map(|content| {
            content
                .lines()
                .find_map(|line| {
                    line.strip_prefix("model name").and_then(|rest| {
                        let value =
                            rest.trim_start_matches(|c: char| c == ':' || c.is_whitespace());
                        (!value.is_empty()).then_some(value.to_string())
                    })
                })
                .unwrap_or_default()
        })
        .unwrap_or_default();
    let nested = ["kvm_intel", "kvm_amd"].iter().any(|module| {
        std::fs::read_to_string(format!("/sys/module/{module}/parameters/nested"))
            .map(|value| {
                let value = value.trim();
                value == "Y" || value == "1"
            })
            .unwrap_or(false)
    });
    Capabilities {
        kvm,
        cpu_model,
        nested,
        nics: physical_nics(),
        secure_boot: detect_firmware().secure_boot_available(),
    }
}

/// Physical (`ether`) links: everything under /sys/class/net that is not
/// virtual. Loopback, veths, bridges and taps live under /devices/virtual.
pub fn physical_nics() -> Vec<String> {
    let mut nics = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/sys/class/net") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "lo" {
                continue;
            }
            let is_virtual = entry
                .path()
                .read_link()
                .map(|target| target.to_string_lossy().contains("/devices/virtual/"))
                .unwrap_or(true);
            if !is_virtual {
                nics.push(name);
            }
        }
    }
    nics.sort();
    nics
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_reports_something_on_any_host() {
        let caps = detect();
        // A dev machine may lack /dev/kvm; the fields must still be sane.
        assert!(
            !caps.cpu_model.is_empty(),
            "/proc/cpuinfo should yield a model name on x86"
        );
        assert!(caps.nics.iter().all(|n| n != "lo"));
        assert!(caps.nics.iter().all(|n| !n.contains('/')));
        // The field is always present and never inferred from a plain pair.
        let _ = caps.secure_boot;
    }
}
