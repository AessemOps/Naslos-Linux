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
chart_version=$(awk '/^version:/ {print $2; exit}' "$chart")
chart_app_version=$(awk '/^appVersion:/ {gsub(/"/, "", $2); print $2; exit}' "$chart")
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

if [ "$fails" -ne 0 ]; then
    exit 1
fi
printf 'release guard: %s matches Chart.yaml version/appVersion and ui/package.json\n' "$version"
