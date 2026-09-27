#!/usr/bin/env python3
"""Pin the chart images in a `values-installer.yaml` copy to immutable digests.

Part of the release path (spec `FR-INSTALL-13`): `scripts/build-install-pack.sh`
calls this on the chart copy it puts in the pack, so the pack's installer profile
renders `repository@sha256:...` for every chart image (NAS-022). The in-tree
template keeps its tag placeholders so a local `make install-pack` and
`scripts/audit.sh` are unchanged.

Usage:
  pin-installer-values.py <values.yaml> --digests-file <file> [--require-all] [--emit-json <out>]
  pin-installer-values.py <values.yaml> --digests "api=ref,agent=ref,..." [--require-all]

The digests input (file or `--digests`) is either JSON
`{"api": "repo@sha256:...", ...}` or `component=repo@sha256:...` lines/pairs.
Components map to the chart's `.image.digest`:
`api`, `agent`, `ui`, `samba` (`.shares.smb`), `nfs` (`.shares.nfs`) and
`terminal`; `openldap` is a single full reference (`.openldap.image`). Each
`<comp>.image.tag` is kept for humans - the chart's digest wins over the tag.

`--require-all` fails closed: every one of the 7 chart images must be present.
"""
import argparse
import json
import os
import re
import sys

try:
    import yaml
except ImportError:  # pragma: no cover - environment guard
    sys.exit("pin-installer-values: PyYAML is required (python3 -m pip install pyyaml)")

# Component -> value path to the `.image` map (a digest is set inside it).
DIGEST_IMAGES = {
    "api": ("api", "image"),
    "agent": ("agent", "image"),
    "ui": ("ui", "image"),
    "samba": ("shares", "smb", "image"),
    "nfs": ("shares", "nfs", "image"),
    "terminal": ("terminal", "image"),
}
# Component -> scalar full reference (not a repository/tag/digest map).
OPENLDAP = "openldap"

REF_RE = re.compile(r"^(?P<repo>.+)@(?P<digest>sha256:[0-9a-f]{64})$")


def parse_digests_file(path):
    with open(path) as f:
        text = f.read()
    stripped = text.lstrip()
    if stripped.startswith("{"):
        data = json.loads(text)
        if not isinstance(data, dict):
            raise ValueError(f"{path}: expected a JSON object")
        return {str(k): str(v) for k, v in data.items()}
    digests = {}
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if "=" not in line:
            raise ValueError(f"{path}: expected 'component=ref', got {line!r}")
        comp, ref = line.split("=", 1)
        digests[comp.strip()] = ref.strip()
    return digests


def parse_inline(value):
    digests = {}
    for pair in value.replace("\n", ",").split(","):
        pair = pair.strip()
        if not pair:
            continue
        if "=" not in pair:
            raise ValueError(f"--digests: expected 'component=ref', got {pair!r}")
        comp, ref = pair.split("=", 1)
        digests[comp.strip()] = ref.strip()
    return digests


def nested_set(doc, path, value):
    cur = doc
    for key in path[:-1]:
        nxt = cur.get(key)
        if nxt is None:
            nxt = {}
            cur[key] = nxt
        if not isinstance(nxt, dict):
            raise ValueError(f"cannot descend into {'.'.join(path)}: {key} is not a map")
        cur = nxt
    cur[path[-1]] = value


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("values", help="values-installer.yaml to rewrite in place")
    ap.add_argument("--digests-file", default="")
    ap.add_argument("--digests", default="")
    ap.add_argument("--require-all", action="store_true")
    ap.add_argument("--emit-json", default="", help="write the normalized {component: ref} map here")
    args = ap.parse_args()

    if not args.digests_file and not args.digests:
        ap.error("supply --digests-file and/or --digests")

    digests = {}
    if args.digests_file:
        digests.update(parse_digests_file(args.digests_file))
    if args.digests:
        digests.update(parse_inline(args.digests))

    known = set(DIGEST_IMAGES) | {OPENLDAP}
    unknown = sorted(set(digests) - known)
    if unknown:
        ap.error(f"unknown component(s): {', '.join(unknown)} (expected: {', '.join(sorted(known))})")

    required = sorted(DIGEST_IMAGES) + [OPENLDAP]
    if args.require_all:
        missing = [c for c in required if c not in digests]
        if missing:
            sys.exit(
                "pin-installer-values: missing digest(s) for "
                + ", ".join(missing)
                + " (--require-all pins every chart image)"
            )

    with open(args.values) as f:
        doc = yaml.safe_load(f)
    if not isinstance(doc, dict):
        sys.exit(f"pin-installer-values: {args.values} is not a YAML map")

    normalized = {}
    for comp, ref in digests.items():
        m = REF_RE.match(ref)
        if not m:
            sys.exit(f"pin-installer-values: {comp}: not a digest ref: {ref!r} (want repo@sha256:<64 hex>)")
        if comp == OPENLDAP:
            nested_set(doc, ("openldap", "image"), ref)
        else:
            base = DIGEST_IMAGES[comp]
            nested_set(doc, base + ("repository",), m.group("repo"))
            nested_set(doc, base + ("digest",), m.group("digest"))
        normalized[comp] = ref

    with open(args.values, "w") as f:
        yaml.safe_dump(doc, f, sort_keys=False, default_flow_style=False, allow_unicode=True)

    if args.emit_json:
        with open(args.emit_json, "w") as f:
            json.dump(dict(sorted(normalized.items())), f, indent=2, sort_keys=True)
            f.write("\n")

    for comp in required:
        if comp in normalized:
            print(f"  pinned {comp}: {normalized[comp]}")


if __name__ == "__main__":
    main()
