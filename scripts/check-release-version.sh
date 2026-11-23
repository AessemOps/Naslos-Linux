#!/usr/bin/env bash
#
# check-release-version.sh - release guard (spec FR-INSTALL-13).
#
# Assert the `vX.Y.Z` tag version about to be released matches every version the
# pack advertises, so a tag cannot ship a stale chart/pack:
#   - charts/naslos/Chart.yaml  `version`
#   - charts/naslos/Chart.yaml  `appVersion` (quotes stripped)
#   - ui/package.json           `version`
#   - charts/naslos/values.yaml and values-installer.yaml image tags
#
# Usage: scripts/check-release-version.sh <X.Y.Z>
set -euo pipefail

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
version="${1:?usage: check-release-version.sh <X.Y.Z>}"

chart="$root/charts/naslos/Chart.yaml"
# Strip either YAML quote style from both fields.
chart_version=$(awk -v q="'" '/^version:/ {gsub(/"/, "", $2); gsub(q, "", $2); print $2; exit}' "$chart")
chart_app_version=$(awk -v q="'" '/^appVersion:/ {gsub(/"/, "", $2); gsub(q, "", $2); print $2; exit}' "$chart")
ui_version=$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["version"])' "$root/ui/package.json")

fails=0
for spec in "Chart.yaml version=$chart_version" "Chart.yaml appVersion=$chart_app_version" "ui/package.json version=$ui_version"; do
    name=${spec%%=*}
    value=${spec#*=}
    if [ "$value" != "$version" ]; then
        printf 'release guard: %s is %s, expected %s\n' "$name" "${value:-<empty>}" "$version" >&2
        fails=$((fails + 1))
    fi
done

# values.yaml is the chart default and values-installer.yaml ships inside the
# pack. Only the installer overlay gets a digest rewrite at pack build time, so
# both files' in-tree tags must already match the chart version; otherwise a
# direct `helm install` (or an unpinned pack) runs older images against a newer
# chart.
for values_file in values.yaml values-installer.yaml values-vm.yaml; do
    if ! python3 - "$root/charts/naslos/$values_file" "$chart_app_version" "$values_file" <<'PY'
import re
import sys

import yaml

path, expected, label = sys.argv[1:4]
doc = yaml.safe_load(open(path, encoding="utf-8"))
errors = []


def check_ref(ref, where):
    ref = ref.strip()
    if "@sha256:" in ref:
        digest = ref.rsplit("@sha256:", 1)[1]
        if not re.fullmatch(r"[0-9a-f]{64}", digest):
            errors.append(f"{where}: invalid digest in {ref}")
        return
    name = ref.rsplit("/", 1)[-1]
    if ":" not in name:
        errors.append(f"{where}: image {ref} has no tag or digest")
        return
    tag = name.rsplit(":", 1)[1]
    if "naslos-" in ref and tag != expected:
        errors.append(f"{where}: naslos image tag {tag} != appVersion {expected} ({ref})")


def walk(node, where):
    if isinstance(node, dict):
        repository = node.get("repository")
        if isinstance(repository, str) and repository:
            tag = node.get("tag")
            digest = node.get("digest")
            if digest:
                digest_hex = str(digest).removeprefix("sha256:")
                if not re.fullmatch(r"[0-9a-f]{64}", digest_hex):
                    errors.append(f"{where}.digest: invalid sha256 {digest}")
            elif tag is None or str(tag) == "":
                errors.append(f"{where}: repository {repository} has no tag or digest")
            elif "naslos-" in repository and str(tag) != expected:
                errors.append(
                    f"{where}.tag: {tag} != appVersion {expected} ({repository})"
                )
        for key, value in node.items():
            child = f"{where}.{key}" if where else key
            if key in ("image", "provisionerImage", "helperImage") and isinstance(value, str):
                check_ref(value, child)
            else:
                walk(value, child)
    elif isinstance(node, list):
        for index, value in enumerate(node):
            walk(value, f"{where}[{index}]")


walk(doc, "")
if errors:
    for error in errors:
        print(f"release guard: {label} {error}", file=sys.stderr)
    sys.exit(1)
PY
    then
        fails=$((fails + 1))
    fi
done

# values-vm.yaml is the node overlay used by `make install-vm`; its advertised
# release facts drive the web upgrader, so a stale value makes the node report
# an older release than the chart it runs.
vm="$root/charts/naslos/values-vm.yaml"
for field in naslosVersion chartVersion; do
    value=$(awk -v key="$field" '
        /^release:/ { inside = 1; next }
        inside && $1 == key ":" { gsub(/"/, "", $2); print $2; exit }
    ' "$vm")
    if [ "$value" != "$version" ]; then
        printf 'release guard: values-vm.yaml release.%s is %s, expected %s\n' "$field" "${value:-<empty>}" "$version" >&2
        fails=$((fails + 1))
    fi
done

if [ "$fails" -ne 0 ]; then
    exit 1
fi
printf 'release guard: %s matches Chart.yaml version/appVersion, ui/package.json, image tags and values-vm release facts\n' "$version"
