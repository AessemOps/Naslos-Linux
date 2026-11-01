#!/usr/bin/env python3
"""Append the pack's `release` facts to its values-installer.yaml.

The web upgrader (docs/installer-contract.md §7) must know which Naslos, Talos,
schematic and Kubernetes versions the installed pack pins. The pack build
injects them here so the chart renders them as API environment variables and
every install/upgrade path carries them without engine-side state.

The in-tree values-installer.yaml deliberately has no `release` block: a local
`helm install` is then not web-upgradable (D2), while a released pack is.

Usage: scripts/inject-release-facts.py --values <file> --profile installer \
    --naslos-version X --chart-version X --talos-version vX --schematic-id HEX \
    --kubernetes-version vX
"""
import argparse
import json
import sys

FIELDS = (
    ("profile", "profile"),
    ("naslosVersion", "naslos_version"),
    ("chartVersion", "chart_version"),
    ("talosVersion", "talos_version"),
    ("schematicId", "schematic_id"),
    ("kubernetesVersion", "kubernetes_version"),
)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--values", required=True)
    for _, dest in FIELDS:
        ap.add_argument("--" + dest.replace("_", "-"), dest=dest, required=True)
    args = ap.parse_args()

    path = args.values
    with open(path, encoding="utf-8") as fh:
        text = fh.read()

    for line in text.splitlines():
        if line.startswith("release:"):
            print(
                f"inject-release-facts: {path} already has a release block",
                file=sys.stderr,
            )
            return 1

    lines = ["", "# --- release facts (injected by scripts/build-install-pack.sh) ---", "release:"]
    for yaml_key, dest in FIELDS:
        value = getattr(args, dest)
        if not value or any(ch in value for ch in "\n\r"):
            print(f"inject-release-facts: invalid {dest}", file=sys.stderr)
            return 1
        lines.append(f"  {yaml_key}: {json.dumps(value)}")

    with open(path, "w", encoding="utf-8") as fh:
        fh.write(text.rstrip("\n") + "\n" + "\n".join(lines) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
