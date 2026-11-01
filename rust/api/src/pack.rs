//! Install-pack parsing and verification (docs/installer-contract.md §1).
//!
//! The web upgrader downloads the target release's pack, verifies its sha256
//! (pinned in the approved operation) and then every member checksum before it
//! runs Helm or Talos against the node. This is the same fail-closed rule the
//! desktop installer applies, implemented here so the API and the `naslos`
//! operation share one parser.

use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

pub const PACK_NAME: &str = "naslos-install-pack";
pub const FORMAT_VERSION: u32 = 1;

/// metadata.json (additive fields tolerated for older/newer packs).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PackMetadata {
    pub name: String,
    #[serde(rename = "formatVersion")]
    pub format_version: u32,
    #[serde(rename = "naslosVersion")]
    pub naslos_version: String,
    #[serde(rename = "chartVersion")]
    pub chart_version: String,
    #[serde(rename = "chartAppVersion")]
    pub chart_app_version: String,
    #[serde(rename = "talosVersion")]
    pub talos_version: String,
    #[serde(rename = "schematicId")]
    pub schematic_id: String,
    #[serde(rename = "kubernetesVersion", default)]
    pub kubernetes_version: String,
    #[serde(rename = "isoUrls", default)]
    pub iso_urls: HashMap<String, String>,
    #[serde(default)]
    pub checksums: HashMap<String, String>,
    #[serde(default)]
    pub images: HashMap<String, String>,
}

impl PackMetadata {
    /// The installer-image the pack's Talos upgrade must boot.
    pub fn talos_installer_image(&self) -> String {
        format!(
            "factory.talos.dev/installer/{}:{}",
            self.schematic_id, self.talos_version
        )
    }

    /// The API image the Naslos operation runs on, if the pack pins it.
    pub fn api_image(&self) -> Option<&str> {
        self.images
            .get("api")
            .map(String::as_str)
            .filter(|s| s.contains("@sha256:"))
    }
}

struct Entry {
    path: String,
    is_dir: bool,
    data: Vec<u8>,
}

/// Parse and checksum-verify a `.tar.gz` pack held in memory.
pub fn verify_pack(bytes: &[u8]) -> Result<PackMetadata, String> {
    let entries = read_entries(bytes)?;
    let meta = metadata_from(&entries)?;
    verify_entries(&entries, &meta)?;
    Ok(meta)
}

/// Verify and extract a pack under `dest`. Existing files are overwritten.
pub fn extract_pack(bytes: &[u8], dest: &Path) -> Result<PackMetadata, String> {
    let entries = read_entries(bytes)?;
    let meta = metadata_from(&entries)?;
    verify_entries(&entries, &meta)?;

    for entry in &entries {
        let target = dest.join(&entry.path);
        if entry.is_dir {
            std::fs::create_dir_all(&target)
                .map_err(|e| format!("creating {}: {e}", target.display()))?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("creating {}: {e}", parent.display()))?;
        }
        std::fs::write(&target, &entry.data)
            .map_err(|e| format!("writing {}: {e}", target.display()))?;
    }
    Ok(meta)
}

fn metadata_from(entries: &[Entry]) -> Result<PackMetadata, String> {
    let raw = entries
        .iter()
        .find(|e| e.path == "metadata.json" && !e.is_dir)
        .ok_or_else(|| "install pack has no metadata.json".to_string())?;
    let meta: PackMetadata =
        serde_json::from_slice(&raw.data).map_err(|e| format!("parsing metadata.json: {e}"))?;
    if meta.name != PACK_NAME {
        return Err(format!("unexpected pack name {:?}", meta.name));
    }
    if meta.format_version != FORMAT_VERSION {
        return Err(format!(
            "unsupported pack formatVersion {}",
            meta.format_version
        ));
    }
    if meta.naslos_version.is_empty()
        || meta.talos_version.is_empty()
        || meta.schematic_id.is_empty()
        || meta.kubernetes_version.is_empty()
    {
        return Err(
            "pack metadata must pin naslosVersion, talosVersion, schematicId and kubernetesVersion"
                .into(),
        );
    }
    if meta.checksums.is_empty() {
        return Err("pack metadata lists no checksums".into());
    }
    Ok(meta)
}

fn verify_entries(entries: &[Entry], meta: &PackMetadata) -> Result<(), String> {
    let mut seen = HashSet::new();
    for entry in entries {
        if entry.is_dir || entry.path == "metadata.json" {
            continue;
        }
        let want = meta.checksums.get(&entry.path).ok_or_else(|| {
            format!(
                "pack member {:?} is not listed in metadata.checksums; refusing an unverified file",
                entry.path
            )
        })?;
        let got = hex_sha256(&entry.data);
        if !got.eq_ignore_ascii_case(want) {
            return Err(format!(
                "checksum mismatch for {:?}: got {got}, want {want}",
                entry.path
            ));
        }
        seen.insert(entry.path.as_str());
    }
    for rel in meta.checksums.keys() {
        if !seen.contains(rel.as_str()) {
            return Err(format!("checksum lists a missing member {rel:?}"));
        }
    }
    Ok(())
}

fn read_entries(bytes: &[u8]) -> Result<Vec<Entry>, String> {
    const MAX_PACK_BYTES: u64 = 512 * 1024 * 1024;
    let decoder = GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    let mut entries = Vec::new();
    let mut total = 0u64;

    for item in archive
        .entries()
        .map_err(|e| format!("reading pack: {e}"))?
    {
        let mut item = item.map_err(|e| format!("reading pack entry: {e}"))?;
        let header = item.header();
        let kind = header.entry_type();
        if kind.is_symlink() || kind.is_hard_link() {
            return Err(format!(
                "pack contains a link at {:?}",
                item.path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            ));
        }
        let raw_path = item
            .path()
            .map_err(|e| format!("reading pack path: {e}"))?
            .to_path_buf();
        let path = safe_relative(&raw_path)?;
        let is_dir = kind.is_dir();
        if !is_dir && !kind.is_file() {
            return Err(format!("pack contains a non-regular entry {path:?}"));
        }
        let mut data = Vec::new();
        if !is_dir {
            item.read_to_end(&mut data)
                .map_err(|e| format!("reading {path:?}: {e}"))?;
            total = total.saturating_add(data.len() as u64);
            if total > MAX_PACK_BYTES {
                return Err("pack is larger than the 512 MiB limit".into());
            }
        }
        entries.push(Entry { path, is_dir, data });
    }
    Ok(entries)
}

/// Reject absolute paths, `..`, and any non-normal component so extraction can
/// never escape the destination directory.
fn safe_relative(path: &Path) -> Result<String, String> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!("pack path escapes the pack: {:?}", path.display()))
            }
        }
    }
    let text = out.to_string_lossy().replace('\\', "/");
    if text.is_empty() {
        return Err("pack contains an empty path".into());
    }
    Ok(text)
}

pub fn hex_sha256(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;

    fn make_pack(files: &[(&str, &str)]) -> Vec<u8> {
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            for (name, data) in files {
                let mut header = tar::Header::new_gnu();
                header.set_size(data.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder
                    .append_data(&mut header, name, data.as_bytes())
                    .unwrap();
            }
            builder.finish().unwrap();
        }
        let mut gz = GzEncoder::new(Vec::new(), Compression::default());
        gz.write_all(&tar_bytes).unwrap();
        gz.finish().unwrap()
    }

    fn pack_with(files: &[(&str, &str)], checksums: &[(&str, &str)]) -> Vec<u8> {
        let meta = serde_json::json!({
            "name": PACK_NAME,
            "formatVersion": FORMAT_VERSION,
            "naslosVersion": "0.3.0",
            "chartVersion": "0.3.0",
            "chartAppVersion": "0.3.0",
            "talosVersion": "v1.14.1",
            "schematicId": "abc",
            "kubernetesVersion": "v1.37.0",
            "checksums": checksums.iter().cloned().collect::<HashMap<_,_>>(),
            "images": {"api": "ghcr.io/aessemops/naslos-api@sha256:deadbeef"},
        })
        .to_string();
        let mut all: Vec<(&str, &str)> = vec![("metadata.json", &meta)];
        all.extend_from_slice(files);
        make_pack(&all)
    }

    #[test]
    fn verifies_and_extracts_a_pack() {
        let checksum = hex_sha256(b"hello");
        let bytes = pack_with(&[("values.yaml", "hello")], &[("values.yaml", &checksum)]);
        let meta = verify_pack(&bytes).unwrap();
        assert_eq!(meta.kubernetes_version, "v1.37.0");
        assert_eq!(
            meta.api_image().unwrap(),
            "ghcr.io/aessemops/naslos-api@sha256:deadbeef"
        );
        let dir = tempfile::tempdir().unwrap();
        extract_pack(&bytes, dir.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("values.yaml")).unwrap(),
            "hello"
        );
    }

    #[test]
    fn refuses_an_unlisted_member() {
        let checksum = hex_sha256(b"hello");
        let bytes = pack_with(
            &[("values.yaml", "hello"), ("extra.yaml", "boom")],
            &[("values.yaml", &checksum)],
        );
        let err = verify_pack(&bytes).unwrap_err();
        assert!(err.contains("not listed"), "{err}");
    }

    #[test]
    fn refuses_a_bad_checksum() {
        let bytes = pack_with(&[("values.yaml", "hello")], &[("values.yaml", "00")]);
        assert!(verify_pack(&bytes)
            .unwrap_err()
            .contains("checksum mismatch"));
    }

    #[test]
    fn refuses_traversal_paths() {
        assert!(safe_relative(std::path::Path::new("../escape")).is_err());
        assert!(safe_relative(std::path::Path::new("/absolute")).is_err());
        assert_eq!(safe_relative(std::path::Path::new("a/./b")).unwrap(), "a/b");
    }

    #[test]
    fn missing_kubernetes_pin_is_rejected() {
        let meta = serde_json::json!({
            "name": PACK_NAME, "formatVersion": 1, "naslosVersion": "0.3.0",
            "chartVersion": "0.3.0", "chartAppVersion": "0.3.0",
            "talosVersion": "v1.14.1", "schematicId": "abc",
            "checksums": {"a": "b"},
        })
        .to_string();
        let bytes = make_pack(&[("metadata.json", &meta)]);
        assert!(verify_pack(&bytes)
            .unwrap_err()
            .contains("kubernetesVersion"));
    }
}
