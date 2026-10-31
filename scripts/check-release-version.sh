#!/usr/bin/env bash
#
# check-release-version.sh - release guard (spec FR-INSTALL-13).
#
# Assert the `vX.Y.Z` tag version about to be released matches every version the
# pack advertises, so a tag cannot ship a stale chart/pack:
#   - charts/naslos/Chart.yaml  `version`
#   - charts/naslos/Chart.yaml  `appVersion` (quotes stripped)
#   - ui/package.json           `version`
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

# values-installer.yaml ships inside the pack. Only its digest rewrite happens
# at pack build time, so its in-tree tags must already match the chart version;
# otherwise a direct `helm install` (or an unpinned pack) runs older images
# against a newer chart.
installer_values="$root/charts/naslos/values-installer.yaml"
if ! python3 - "$installer_values" "$chart_app_version" <<'PY'
import re
import sys

text = open(sys.argv[1], encoding="utf-8").read()
expected = sys.argv[2]
tags = set(re.findall(r'^\s*tag:\s*"([^"]+)"', text, re.M))
tags |= set(re.findall(r'naslos-[a-z]+:([0-9]+\.[0-9]+\.[0-9]+)', text))
bad = sorted(tag for tag in tags if tag != expected)
if not tags:
    print("release guard: values-installer.yaml pins no image tags", file=sys.stderr)
    sys.exit(1)
if bad:
    print(
        "release guard: values-installer.yaml tags "
        + ", ".join(bad)
        + f" do not match appVersion {expected}",
        file=sys.stderr,
    )
    sys.exit(1)
PY
then
    fails=$((fails + 1))
fi

if [ "$fails" -ne 0 ]; then
    exit 1
fi
printf 'release guard: %s matches Chart.yaml version/appVersion and ui/package.json\n' "$version"
