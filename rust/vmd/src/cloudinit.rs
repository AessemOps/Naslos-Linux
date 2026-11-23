//! cloud-init NoCloud seed generation (FR-VMT).
//!
//! The seed is a `cidata` ISO rendered from the form fields only: hostname,
//! user and SSH public keys. Passwords are never accepted, let alone baked
//! (the decision record forbids them), and `ssh_pwauth` is disabled so the
//! image cannot fall back to password login.

use crate::model::{CloudInitSpec, VmSpec};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Render one YAML scalar as a single-quoted string.
///
/// Single quotes need no backslash processing (only an embedded `'` is
/// doubled), and the validator rejects every control character, so a value
/// can never span a line or inject another directive. Quoting is done here
/// rather than trusting the validator: a `#`, `:` or leading `-` in a key
/// comment must stay data.
fn yaml_scalar(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Render the `user-data` file. Only model data lands here; the fields were
/// validated in `model::validate_vm`.
pub fn render_user_data(ci: &CloudInitSpec) -> String {
    let mut out = String::from("#cloud-config\n");
    out.push_str(&format!("hostname: {}\n", yaml_scalar(&ci.hostname)));
    out.push_str(&format!("fqdn: {}\n", yaml_scalar(&ci.hostname)));
    out.push_str("users:\n");
    out.push_str(&format!("  - name: {}\n", yaml_scalar(&ci.user)));
    out.push_str("    sudo: ALL=(ALL) NOPASSWD:ALL\n");
    out.push_str("    shell: /bin/bash\n");
    out.push_str("    lock_passwd: true\n");
    if !ci.ssh_keys.is_empty() {
        out.push_str("    ssh_authorized_keys:\n");
        for key in &ci.ssh_keys {
            out.push_str(&format!("      - {}\n", yaml_scalar(key)));
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
        yaml_scalar(&ci.hostname)
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

    // Build the ISO in the private staging dir first, then copy it into the
    // dataset through a `.part` file and rename. The seed is regenerated on
    // every start, but the target must never be a symlink (a share account
    // could otherwise redirect the write and truncate an arbitrary file).
    let staged_iso = staging.path().join("seed.iso");
    let mut cmd = tokio::process::Command::new(crate::model::genisoimage_binary());
    cmd.arg("-output")
        .arg(&staged_iso)
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
    let bytes = tokio::fs::read(&staged_iso)
        .await
        .map_err(|e| format!("reading the staged seed: {e}"))?;

    let seed = vm_dir.join("seed.iso");
    refuse_symlink(&seed, "the cloud-init seed").await?;
    let part = vm_dir.join("seed.iso.part");
    match tokio::fs::symlink_metadata(&part).await {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(format!(
                "refusing to follow the symlink at {}",
                part.display()
            ))
        }
        Ok(_) => {
            tokio::fs::remove_file(&part)
                .await
                .map_err(|e| format!("removing the stale {}: {e}", part.display()))?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("checking {}: {e}", part.display())),
    }
    {
        use tokio::io::AsyncWriteExt;
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&part)
            .await
            .map_err(|e| format!("creating {}: {e}", part.display()))?;
        file.write_all(&bytes)
            .await
            .map_err(|e| format!("writing {}: {e}", part.display()))?;
        file.sync_all()
            .await
            .map_err(|e| format!("syncing {}: {e}", part.display()))?;
    }
    tokio::fs::rename(&part, &seed)
        .await
        .map_err(|e| format!("installing {}: {e}", seed.display()))?;
    Ok(seed)
}

/// Refuse a target that already exists as a symlink (or is not a regular
/// file). A missing target is fine: it is created below.
async fn refuse_symlink(path: &Path, what: &str) -> Result<(), String> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(meta) if meta.file_type().is_symlink() => Err(format!(
            "refusing to use the symlink at {}: {what} must be a regular file",
            path.display()
        )),
        Ok(meta) if !meta.is_file() => Err(format!(
            "{} exists but is not a regular file",
            path.display()
        )),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("checking {}: {e}", path.display())),
    }
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
        assert!(rendered.contains("hostname: 'vm-1'\n"));
        assert!(rendered.contains("  - name: 'admin'\n"));
        assert!(rendered.contains("    lock_passwd: true\n"));
        assert!(rendered.contains("ssh_pwauth: false\n"));
        assert!(rendered.contains("      - 'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExample t@e'\n"));
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
    fn yaml_scalars_are_single_quoted_and_escaped() {
        let mut ci = ci();
        ci.ssh_keys = vec!["ssh-ed25519 AAAAkey it's #not: a comment".into()];
        let rendered = render_user_data(&ci);
        assert!(
            rendered.contains("- 'ssh-ed25519 AAAAkey it''s #not: a comment'\n"),
            "{rendered}"
        );
        // A quoted scalar can never start a new line (validation rejects
        // control characters before this point).
        assert_eq!(rendered.matches("ssh_authorized_keys").count(), 1);
    }

    #[test]
    fn meta_data_instance_id_tracks_the_seed_content() {
        let a = render_meta_data(&ci());
        let mut changed = ci();
        changed.hostname = "vm-2".into();
        let b = render_meta_data(&changed);
        assert!(a.contains("local-hostname: 'vm-1'\n"));
        assert!(b.contains("local-hostname: 'vm-2'\n"));
        assert_ne!(
            a.split('\n').next().unwrap(),
            b.split('\n').next().unwrap(),
            "instance-id must change when the seed changes"
        );
    }
}
