#!/usr/bin/env bash
#
# build-install-pack.sh - build `naslos-install-pack-<version>.tar.gz`, the
# versioned artifact the desktop installer (Naslos-Installer) pins, downloads,
# checksum-verifies and embeds (FR-INSTALL; docs/installer-contract.md).
#
# Contents:
#   metadata.json                     versions + ISO URL + per-member sha256
#   charts/naslos/                    umbrella chart, incl. charts/*.tgz subcharts
#   charts/naslos/values-installer.yaml
#   machine-config/naslos-installer.yaml.tmpl
#   cilium/cilium.yaml
#   manifests/local-path-v0.0.26.yaml
#   schematic/naslos.yaml
#
# The chart's subchart `.tgz`s must already be vendored under
# charts/naslos/charts/ (they are committed); this script does not run
# `helm dependency update`, so the build needs no network.
#
# When IMAGE_DIGESTS_FILE/IMAGE_DIGESTS is set (the release path, FR-INSTALL-13),
# the chart copy's values-installer.yaml is rewritten to pin every chart image by
# digest and metadata.json gains an additive `images` map. Without it the pack
# keeps the tag placeholders (the local/dev shape).
#
# Usage: scripts/build-install-pack.sh [output-dir]
# Environment:
#   PACK_VERSION       override the pack/naslos version (default: Chart.yaml version)
#   OUTPUT_DIR         output directory (default: dist)
#   IMAGE_DIGESTS_FILE JSON {"api":"repo@sha256:...", ...} or `component=ref` lines
#   IMAGE_DIGESTS      same content inline (`api=repo@sha256:...,agent=...`)
#                      Supplying either is fail-closed: all 7 chart images required.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$root"

OUTPUT_DIR="${1:-${OUTPUT_DIR:-dist}}"
CHART_DIR="charts/naslos"
CHART_YAML="$CHART_DIR/Chart.yaml"
TEMPLATE="bootstrap/installer/naslos-installer.yaml.tmpl"
CILIUM="bootstrap/cilium/cilium.yaml"
LOCAL_PATH="bootstrap/local-path/local-path-storage.yaml"
SCHEMATIC="bootstrap/schematic/naslos.yaml"

TALOS_VERSION="v1.14.1"
SCHEMATIC_ID="4dd8e3a8b6203d3c14f049da8db4d3bb0d6d3e70c5e89dfcc1e709e81914f63c"
ISO_URL="https://factory.talos.dev/image/${SCHEMATIC_ID}/${TALOS_VERSION}/metal-amd64.iso"

for f in "$CHART_YAML" "$TEMPLATE" "$CILIUM" "$LOCAL_PATH" "$SCHEMATIC"; do
    [ -f "$f" ] || { echo "build-install-pack: missing $f" >&2; exit 1; }
done

# Strip either YAML quote style from both fields.
chart_version=$(awk -v q="'" '/^version:/ {gsub(/"/, "", $2); gsub(q, "", $2); print $2; exit}' "$CHART_YAML")
chart_app_version=$(awk -v q="'" '/^appVersion:/ {gsub(/"/, "", $2); gsub(q, "", $2); print $2; exit}' "$CHART_YAML")
VERSION="${PACK_VERSION:-$chart_version}"

# Optional digest map (FR-INSTALL-13). Supplying one is fail-closed: the pack
# must pin all 7 chart images or the build fails (pin-installer-values.py).
IMAGE_DIGESTS_FILE="${IMAGE_DIGESTS_FILE:-}"
IMAGE_DIGESTS="${IMAGE_DIGESTS:-}"

# Subcharts must be vendored (no network in the pack build).
if ! ls "$CHART_DIR"/charts/*.tgz >/dev/null 2>&1; then
    echo "build-install-pack: no vendored subcharts under $CHART_DIR/charts/*.tgz" >&2
    echo "run 'helm dependency build $CHART_DIR' once and commit the .tgz files" >&2
    exit 1
fi

# Keep the installer template's Cilium block in step with cilium.yaml.
scripts/render-installer-template.sh --check >/dev/null

# Gate: the template's machine.install image must match the pinned
# schematic/Talos, or the pack's advertised ISO URL and the image the node
# installs would silently disagree.
expected_installer="factory.talos.dev/installer/${SCHEMATIC_ID}:${TALOS_VERSION}"
published_installer=$(grep -m1 -E '^[[:space:]]*image:[[:space:]]*factory\.talos\.dev/installer/' "$TEMPLATE" | awk '{print $2}')
if [ "$published_installer" != "$expected_installer" ]; then
    echo "build-install-pack: template installer image '$published_installer' != expected '$expected_installer'" >&2
    exit 1
fi

mkdir -p "$OUTPUT_DIR"
build_dir=$(mktemp -d)
trap 'rm -rf "$build_dir"' EXIT
pack_root="$build_dir/naslos-install-pack-$VERSION"
mkdir -p "$pack_root/charts" "$pack_root/machine-config" "$pack_root/cilium" \
         "$pack_root/manifests" "$pack_root/schematic"

# Chart (templates + values + Chart.yaml/lock + vendored subcharts), excluding
# any local scratch.
cp -R "$CHART_DIR" "$pack_root/charts/naslos"
rm -f "$pack_root/charts/naslos/.helmignore" 2>/dev/null || true
cp "$TEMPLATE" "$pack_root/machine-config/naslos-installer.yaml.tmpl"
cp "$CILIUM" "$pack_root/cilium/cilium.yaml"
cp "$LOCAL_PATH" "$pack_root/manifests/local-path-v0.0.26.yaml"
cp "$SCHEMATIC" "$pack_root/schematic/naslos.yaml"

# FR-INSTALL-13: pin the copied installer profile's images by digest when a map
# is supplied, and remember the normalized map for metadata.json.
images_json=""
if [ -n "$IMAGE_DIGESTS_FILE" ] || [ -n "$IMAGE_DIGESTS" ]; then
    pin_args=(--require-all --emit-json "$build_dir/images.json")
    [ -n "$IMAGE_DIGESTS_FILE" ] && pin_args+=(--digests-file "$IMAGE_DIGESTS_FILE")
    [ -n "$IMAGE_DIGESTS" ] && pin_args+=(--digests "$IMAGE_DIGESTS")
    scripts/pin-installer-values.py "$pack_root/charts/naslos/values-installer.yaml" "${pin_args[@]}"
    images_json="$build_dir/images.json"
fi

# metadata.json: versions, ISO URL, and a sha256 for every other member.
python3 - "$pack_root" "$VERSION" "$chart_version" "$chart_app_version" \
    "$TALOS_VERSION" "$SCHEMATIC_ID" "$ISO_URL" "$images_json" <<'PY'
import hashlib, json, os, sys, datetime

(pack_root, version, chart_version, chart_app_version, talos_version,
 schematic_id, iso_url, images_json) = sys.argv[1:9]

checksums = {}
for dirpath, _dirs, files in os.walk(pack_root):
    for name in files:
        path = os.path.join(dirpath, name)
        rel = os.path.relpath(path, pack_root)
        if rel == "metadata.json":
            continue
        with open(path, "rb") as f:
            checksums[rel] = hashlib.sha256(f.read()).hexdigest()

metadata = {
    "name": "naslos-install-pack",
    "formatVersion": 1,
    "naslosVersion": version,
    "chartVersion": chart_version,
    "chartAppVersion": chart_app_version,
    "talosVersion": talos_version,
    "schematicId": schematic_id,
    "isoUrls": {"metal-amd64": iso_url},
    "generatedAt": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "checksums": dict(sorted(checksums.items())),
}
if images_json:
    with open(images_json) as f:
        metadata["images"] = json.load(f)
with open(os.path.join(pack_root, "metadata.json"), "w") as f:
    json.dump(metadata, f, indent=2, sort_keys=True)
    f.write("\n")
PY

tarball="$OUTPUT_DIR/naslos-install-pack-$VERSION.tar.gz"
tar -C "$build_dir" -czf "$tarball" "naslos-install-pack-$VERSION"
( cd "$OUTPUT_DIR" && sha256sum "naslos-install-pack-$VERSION.tar.gz" > "naslos-install-pack-$VERSION.tar.gz.sha256" )

echo "built $tarball"
echo "  talosVersion=$TALOS_VERSION schematicId=$SCHEMATIC_ID"
if [ -n "$images_json" ]; then
    echo "  images pinned by digest ($(python3 -c 'import json,sys; print(len(json.load(open(sys.argv[1]))))' "$images_json"))"
else
    echo "  images: tag placeholders (no IMAGE_DIGESTS_FILE/IMAGE_DIGESTS supplied)"
fi
echo "  $(cd "$OUTPUT_DIR" && cat "naslos-install-pack-$VERSION.tar.gz.sha256")"
