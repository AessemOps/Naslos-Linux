//! Hardware preflight (FR-VMT): what the node can run.

use serde::Serialize;

#[derive(Debug, Clone, Default, Serialize)]
pub struct Capabilities {
    pub kvm: bool,
    #[serde(rename = "cpuModel")]
    pub cpu_model: String,
    pub nested: bool,
    pub tun: bool,
    pub macvtap: bool,
    pub nics: Vec<String>,
}

/// Detect the node's virtualization capabilities.
///
/// `macvtap` is probed by creating and deleting one throwaway device: the
/// module is only loaded (and /sys/module/macvtap only appears) after first
/// use, so a passive check would always answer false on a fresh boot.
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
    let tun = std::path::Path::new("/dev/net/tun").exists();
    let macvtap = probe_macvtap();
    Capabilities {
        kvm,
        cpu_model,
        nested,
        tun,
        macvtap,
        nics: physical_nics(),
    }
}

fn probe_macvtap() -> bool {
    // std::process (not tokio): detect() is sync and runs on the blocking
    // pool; the probe is two ~ms `ip` calls once per capabilities request.
    let probe = "mtap-probe00";
    let ok = std::process::Command::new(crate::model::ip_binary())
        .args([
            "link", "add", probe, "link", "lo", "type", "macvtap", "mode", "bridge",
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if ok {
        let _ = std::process::Command::new(crate::model::ip_binary())
            .args(["link", "del", probe])
            .output();
    }
    ok
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
    }
}
