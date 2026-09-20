#!/usr/bin/env bash
#
# render-cilium.sh - generate the Cilium TALOS inline manifest and splice it
# into bootstrap/vm/naslos-vm.yaml, where `make bootstrap-vm` picks it up as
# the machine-config patch (AUDIT-M4).
#
# Why this exists: Talos applies a KubeInlineManifestConfig on bootstrap and
# after every control-plane boot, which is how Cilium persists. The manifest
# itself is a checked-in artifact (bootstrap/cilium/cilium.yaml) because the
# chart/values pin the exact Cilium release; this script only wraps it in the
# Talos document and substitutes it into the patch file.
#
# Idempotent: re-running replaces the previously generated block in place, so
# the patch file stays reviewable in git. The block is delimited by BEGIN/END
# markers; the first run additionally consumes the CILIUM_INLINE_MANIFEST_
# PLACEHOLDER comment.
#
# Usage: scripts/render-cilium.sh [--check]
#   --check   fail if the patch file does not already match cilium.yaml
#             (used by the audit sweep / CI); never writes.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
manifest="$root/bootstrap/cilium/cilium.yaml"
patch="$root/bootstrap/vm/naslos-vm.yaml"
check_only=0
[ "${1:-}" = "--check" ] && check_only=1

[ -f "$manifest" ] || { echo "render-cilium: missing $manifest" >&2; exit 1; }
[ -f "$patch" ] || { echo "render-cilium: missing $patch" >&2; exit 1; }

out=$(mktemp)
trap 'rm -f "$out"' EXIT

python3 - "$manifest" "$patch" "$out" <<'PY'
import sys

manifest_path, patch_path, out_path = sys.argv[1], sys.argv[2], sys.argv[3]

BEGIN = "# --- BEGIN GENERATED CILIUM INLINE MANIFEST (scripts/render-cilium.sh) ---"
END = "# --- END GENERATED CILIUM INLINE MANIFEST ---"
PLACEHOLDER = "# CILIUM_INLINE_MANIFEST_PLACEHOLDER"

cilium = open(manifest_path).read().rstrip("\n")
indented = "\n".join(("      " + line) if line.strip() else "" for line in cilium.split("\n"))

block = "\n".join([
    BEGIN,
    "apiVersion: v1alpha1",
    "kind: KubeInlineManifestConfig",
    "name: cilium",
    "manifest: |",
    indented,
    END,
])

text = open(patch_path).read()

# 1. Remove any existing generated block (idempotent re-run).
lines = text.split("\n")
kept, skip = [], False
for line in lines:
    if line.strip() == BEGIN:
        skip = True
        continue
    if skip:
        if line.strip() == END:
            skip = False
        continue
    kept.append(line)
text = "\n".join(kept)

# 2. Splice the fresh block. Prefer the placeholder anchor; if it is already
#    consumed (re-run), re-attach after the ResolverConfig document.
if PLACEHOLDER in text:
    text = text.replace(PLACEHOLDER, block)
elif "{BEGIN}" in text or block not in text:
    # No anchor and no block above: append at the end of the patch file.
    text = text.rstrip("\n") + "\n\n" + block + "\n"

text = text.rstrip("\n") + "\n"
open(out_path, "w").write(text)
PY

if [ "$check_only" -eq 1 ]; then
    if ! cmp -s "$out" "$patch"; then
        echo "render-cilium: --check: bootstrap/vm/naslos-vm.yaml is out of date with bootstrap/cilium/cilium.yaml" >&2
        echo "run: scripts/render-cilium.sh" >&2
        exit 1
    fi
    echo "cilium inline manifest is up to date"
    exit 0
fi

cp "$out" "$patch"
echo "spliced $(grep -c '' "$manifest")-line Cilium manifest into $patch"
