#!/usr/bin/env bash
#
# collect-image-digests.sh - merge per-image digest refs into the JSON map
# `scripts/build-install-pack.sh` pins `values-installer.yaml` from
# (spec FR-INSTALL-13).
#
# Usage:
#   collect-image-digests.sh "api=ghcr.io/aessemops/naslos-api@sha256:..." ...
#   collect-image-digests.sh digest-api.txt digest-ui.txt ...
#
# A `digest-<component>.txt` file holds the full reference on one line (the
# shape each `release.yml` matrix cell uploads). Emits a sorted
# `{"<component>": "<repo>@sha256:..."}` JSON object on stdout.
set -euo pipefail

[ "$#" -gt 0 ] || { echo "usage: collect-image-digests.sh <component=ref|digest-<component>.txt> ..." >&2; exit 2; }

python3 - "$@" <<'PY'
import json, os, re, sys

out = {}
for arg in sys.argv[1:]:
    if "=" in arg and not os.path.exists(arg):
        comp, ref = arg.split("=", 1)
    else:
        base = os.path.basename(arg)
        m = re.match(r"^digest-(.+)\.txt$", base)
        comp = m.group(1) if m else os.path.splitext(base)[0]
        with open(arg) as f:
            ref = f.read().strip()
    comp, ref = comp.strip(), ref.strip()
    if not comp or not ref:
        sys.exit(f"collect-image-digests: empty component/ref for {arg!r}")
    if not re.match(r"^.+@sha256:[0-9a-f]{64}$", ref):
        sys.exit(f"collect-image-digests: {comp}: not a digest ref: {ref!r}")
    if comp in out and out[comp] != ref:
        sys.exit(f"collect-image-digests: conflicting refs for {comp}")
    out[comp] = ref

json.dump(dict(sorted(out.items())), sys.stdout, indent=2, sort_keys=True)
sys.stdout.write("\n")
PY
