//! Size parsing/formatting and device-wait helpers (port of `zfs/utils.go`).

use std::time::{Duration, Instant};

/// Parse a human-readable size string to bytes (`100G`, `1.5T`, `512`).
pub fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(0);
    }

    let split = s
        .find(|c: char| c.is_ascii_alphabetic())
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let value: f64 = num
        .trim()
        .parse()
        .map_err(|e| format!("invalid size {s:?}: {e}"))?;

    let multiplier: f64 = match unit.to_ascii_uppercase().as_str() {
        "B" | "" => 1.0,
        "K" | "KB" | "KIB" => 1024.0,
        "M" | "MB" | "MIB" => 1024.0 * 1024.0,
        "G" | "GB" | "GIB" => 1024.0 * 1024.0 * 1024.0,
        "T" | "TB" | "TIB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return Err(format!("unknown unit {unit:?}")),
    };
    Ok((value * multiplier) as u64)
}

/// Format bytes the way `FormatSize` did.
pub fn format_size(bytes: u64) -> String {
    const TIB: u64 = 1024 * 1024 * 1024 * 1024;
    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;
    const KIB: u64 = 1024;
    if bytes >= TIB {
        format!("{:.2} TiB", bytes as f64 / TIB as f64)
    } else if bytes >= GIB {
        format!("{:.2} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.2} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.2} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

/// Wait for a device node to appear (after wipefs).
pub fn wait_for_device(device: &str, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if std::path::Path::new(device).exists() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err(format!("timeout waiting for device {device}"))
}
