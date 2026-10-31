#!/usr/bin/env bash
#
# dispatch-installer.sh - manually rebuild the desktop installer for a Naslos
# release. Use when the release workflow's INSTALLER_DISPATCH_TOKEN is invalid
# (the pack is already attached to the GitHub release; only the dispatch is
# missing).
#
# Usage: scripts/dispatch-installer.sh vX.Y.Z
# Requires: gh authenticated with access to AessemOps/Naslos-Installer
#           (repository dispatch needs contents:write there).
set -euo pipefail

tag="${1:?usage: dispatch-installer.sh vX.Y.Z}"
version="${tag#v}"

gh api repos/AessemOps/Naslos-Installer/dispatches \
  -f event_type=naslos-release \
  -f "client_payload[tag]=${tag}" \
  -f "client_payload[version]=${version}"

echo "dispatched naslos-release for ${tag}; watch it with:"
echo "  gh run list -R AessemOps/Naslos-Installer --limit 3"
