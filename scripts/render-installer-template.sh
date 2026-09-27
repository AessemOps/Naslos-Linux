#!/usr/bin/env bash
#
# render-installer-template.sh - generate the Cilium inline block inside the
# install-pack machine-config template
# (bootstrap/installer/naslos-installer.yaml.tmpl) from the checked-in
# bootstrap/cilium/cilium.yaml (AUDIT-M4).
#
# The installer template is a Talos machine-config patch, like
# bootstrap/vm/naslos-vm.yaml, but it is parameterised for an arbitrary node
# ({{NODE_SUBNET}}, {{INSTALL_DISK}}) and carries no private-registry mirror. It
# is packed into `naslos-install-pack-<version>.tar.gz` as
# `machine-config/naslos-installer.yaml.tmpl`.
#
# Idempotent and deterministic: the generated block is delimited by BEGIN/END
# markers and re-inserted immediately after the CILIUM_INLINE_MANIFEST_ANCHOR
# comment on every run, so the output does not depend on whether the block was
# already present.
#
# Usage: scripts/render-installer-template.sh [--check]
#   --check   fail if the template does not already match cilium.yaml
#             (used by the audit sweep / CI); never writes.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
manifest="$root/bootstrap/cilium/cilium.yaml"
template="$root/bootstrap/installer/naslos-installer.yaml.tmpl"
check_only=0
[ "${1:-}" = "--check" ] && check_only=1

[ -f "$manifest" ] || { echo "render-installer-template: missing $manifest" >&2; exit 1; }
[ -f "$template" ] || { echo "render-installer-template: missing $template" >&2; exit 1; }

out=$(mktemp)
trap 'rm -f "$out"' EXIT

python3 - "$manifest" "$template" "$out" <<'PY'
import sys

manifest_path, template_path, out_path = sys.argv[1], sys.argv[2], sys.argv[3]

BEGIN = "# --- BEGIN GENERATED CILIUM INLINE MANIFEST (scripts/render-installer-template.sh) ---"
END = "# --- END GENERATED CILIUM INLINE MANIFEST ---"
ANCHOR = "# CILIUM_INLINE_MANIFEST_ANCHOR"

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

# 1. Drop any existing generated block, then 2. re-insert it after the anchor.
out_lines, skip, inserted = [], False, False
for line in open(template_path).read().split("\n"):
    if line.strip() == BEGIN:
        skip = True
        continue
    if skip:
        if line.strip() == END:
            skip = False
        continue
    out_lines.append(line)
    if not inserted and line.strip() == ANCHOR:
        out_lines.append(block)
        inserted = True
if not inserted:
    out_lines.append(block)

open(out_path, "w").write("\n".join(out_lines).rstrip("\n") + "\n")
PY

# The template must keep its engine placeholders after a render.
for ph in '{{NODE_SUBNET}}' '{{INSTALL_DISK}}'; do
    grep -qF "$ph" "$out" || { echo "render-installer-template: template lost placeholder $ph" >&2; exit 1; }
done

if [ "$check_only" -eq 1 ]; then
    if ! cmp -s "$out" "$template"; then
        echo "render-installer-template: --check: bootstrap/installer/naslos-installer.yaml.tmpl is out of date with bootstrap/cilium/cilium.yaml" >&2
        echo "run: scripts/render-installer-template.sh" >&2
        exit 1
    fi
    echo "installer template cilium block is up to date"
    exit 0
fi

cp "$out" "$template"
echo "spliced $(grep -c '' "$manifest")-line Cilium manifest into $template"
