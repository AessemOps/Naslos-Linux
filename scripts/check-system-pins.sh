#!/usr/bin/env bash
#
# check-system-pins.sh - keep the Talos/Kubernetes tool pins in step with the
# versions the pack advertises (spec FR-UPD, docs/installer-contract.md §7).
#
# The web upgrader runs `talosctl` and `kubectl` from the API image against the
# node. A client that is older than the node (Talos) or more than one minor off
# the Kubernetes control plane can fail mid-upgrade, so fail the release/audit
# gate instead.
#
# Checks:
#   - Dockerfile TALOSCTL_VERSION == build-install-pack.sh TALOS_VERSION
#   - Dockerfile KUBECTL_VERSION is the same minor as KUBERNETES_VERSION (±1)
set -euo pipefail

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
build="$root/scripts/build-install-pack.sh"
dockerfile="$root/rust/api/Dockerfile"

talos_version=$(sed -n 's/^TALOS_VERSION="\([^"]*\)".*/\1/p' "$build")
kubernetes_version=$(sed -n 's/^KUBERNETES_VERSION="\([^"]*\)".*/\1/p' "$build")
talosctl_version=$(sed -n 's/^ARG TALOSCTL_VERSION=\([^ ]*\).*/\1/p' "$dockerfile")
kubectl_version=$(sed -n 's/^ARG KUBECTL_VERSION=\([^ ]*\).*/\1/p' "$dockerfile")

fail=0
for pair in "TALOS_VERSION=$talos_version" "KUBERNETES_VERSION=$kubernetes_version" \
            "TALOSCTL_VERSION=$talosctl_version" "KUBECTL_VERSION=$kubectl_version"; do
    if [ -z "${pair#*=}" ]; then
        printf 'system pins: %s is empty\n' "${pair%%=*}" >&2
        fail=1
    fi
done

if [ "$talosctl_version" != "$talos_version" ]; then
    printf 'system pins: talosctl %s != pack talosVersion %s\n' \
        "$talosctl_version" "$talos_version" >&2
    fail=1
fi

minor() { printf '%s' "$1" | sed -E -n 's/^v?([0-9]+)\.([0-9]+).*/\1 \2/p'; }

read -r kmaj kmin <<<"$(minor "$kubernetes_version")"
read -r cmaj cmin <<<"$(minor "$kubectl_version")"
if [ -z "$kmaj" ] || [ -z "$cmaj" ]; then
    printf 'system pins: cannot parse Kubernetes versions (%s, %s)\n' \
        "$kubernetes_version" "$kubectl_version" >&2
    fail=1
else
    k=$((kmaj * 1000 + kmin))
    c=$((cmaj * 1000 + cmin))
    diff=$((k - c))
    [ "$diff" -lt 0 ] && diff=$((-diff))
    if [ "$diff" -gt 1 ]; then
        printf 'system pins: kubectl %s is more than one minor from Kubernetes %s\n' \
            "$kubectl_version" "$kubernetes_version" >&2
        fail=1
    fi
fi

[ "$fail" -eq 0 ] || exit 1
printf 'system pins: talosctl %s, kubectl %s (Kubernetes %s)\n' \
    "$talosctl_version" "$kubectl_version" "$kubernetes_version"
