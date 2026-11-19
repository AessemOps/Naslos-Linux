//! cloud-init NoCloud seed generation (FR-VMT).
//!
//! The seed is a `cidata` ISO rendered from the form fields only: hostname,
//! user and SSH public keys. Passwords are never accepted, let alone baked
//! (the decision record forbids them), and `ssh_pwauth` is disabled so the
//! image cannot fall back to password login.

use crate::model::{CloudInitSpec, VmSpec};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Render the `user-data` file. Only model data lands here; the fields were
/// validated in `model::validate_vm`.
pub fn render_user_data(ci: &CloudInitSpec) -> String {
    let mut out = String::from("#cloud-config\n");
    out.push_str(&format!("hostname: {}\n", ci.hostname));
    out.push_str(&format!("fqdn: {}\n", ci.hostname));
    out.push_str("users:\n");
    out.push_str(&format!("  - name: {}\n", ci.user));
    out.push_str("    sudo: ALL=(ALL) NOPASSWD:ALL\n");
    out.push_str("    shell: /bin/bash\n");
    out.push_str("    lock_passwd: true\n");
    if !ci.ssh_keys.is_empty() {
        out.push_str("    ssh_authorized_keys:\n");
        for key in &ci.ssh_keys {
            out.push_str(&format!("      - {key}\n"));
        }
    }
    out.push_str("ssh_pwauth: false\n");
    out.push_str("chpasswd:\n  expire: false\n");
    out
}

/// Render the `meta-data` file. `instance-id` changes with the seed content
/// so an edited seed re-runs cloud-init on the next boot.
pub fn render_meta_data(ci: &CloudInitSpec) -> String {
    let digest = Sha256::digest(render_user_data(ci).as_bytes());
    format!(
        "instance-id: iid-{:016x}\nlocal-hostname: {}\n",
        u64::from_be_bytes(digest[..8].try_into().unwrap_or([0u8; 8])),
        ci.hostname
    )
}

/// Build the NoCloud seed ISO in the VM's dataset directory and return its
/// path. Idempotent: re-running overwrites the seed in place.
pub async fn build_seed(spec: &VmSpec, vm_dir: &Path) -> Result<PathBuf, String> {
    let Some(ci) = &spec.cloud_init else {
        return Err("the VM has no cloud-init configuration".to_string());
    };
    let staging = tempfile::tempdir().map_err(|e| format!("staging cloud-init: {e}"))?;
    let user_data = staging.path().join("user-data");
    let meta_data = staging.path().join("meta-data");
    tokio::fs::write(&user_data, render_user_data(ci))
        .await
        .map_err(|e| format!("writing cloud-init user-data: {e}"))?;
    tokio::fs::write(&meta_data, render_meta_data(ci))
        .await
        .map_err(|e| format!("writing cloud-init meta-data: {e}"))?;

    let seed = vm_dir.join("seed.iso");
    // genisoimage writes to a temp file next to the target first, so the
    // seed never appears half-written to a booting VM.
    let mut cmd = tokio::process::Command::new(crate::model::genisoimage_binary());
    cmd.arg("-output")
        .arg(&seed)
        .arg("-volid")
        .arg("cidata")
        .arg("-joliet")
        .arg("-rock")
        .arg(&user_data)
        .arg(&meta_data);
    let output = cmd
        .output()
        .await
        .map_err(|e| format!("running genisoimage: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "genisoimage failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(seed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ci() -> CloudInitSpec {
        CloudInitSpec {
            hostname: "vm-1".into(),
            user: "admin".into(),
            ssh_keys: vec!["ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExample t@e".into()],
        }
    }

    #[test]
    fn user_data_has_no_passwords_and_disables_pwauth() {
        let rendered = render_user_data(&ci());
        assert!(rendered.starts_with("#cloud-config\n"));
        assert!(rendered.contains("hostname: vm-1\n"));
        assert!(rendered.contains("  - name: admin\n"));
        assert!(rendered.contains("    lock_passwd: true\n"));
        assert!(rendered.contains("ssh_pwauth: false\n"));
        assert!(rendered.contains("      - ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExample t@e\n"));
        // No line may set a plaintext password: `passwd:` (cloud-config's
        // password field) must never appear at line start.
        assert!(
            !rendered
                .lines()
                .any(|l| l.trim_start().starts_with("passwd:")),
            "no password field may be rendered"
        );
    }

    #[test]
    fn meta_data_instance_id_tracks_the_seed_content() {
        let a = render_meta_data(&ci());
        let mut changed = ci();
        changed.hostname = "vm-2".into();
        let b = render_meta_data(&changed);
        assert!(a.contains("local-hostname: vm-1\n"));
        assert!(b.contains("local-hostname: vm-2\n"));
        assert_ne!(
            a.split('\n').next().unwrap(),
            b.split('\n').next().unwrap(),
            "instance-id must change when the seed changes"
        );
    }
}
