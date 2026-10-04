#!/usr/bin/env bash
# Memory baseline for the Naslos single-node appliance.
#
# Part of Phase 0 of the Go->Rust RAM plan (see
# .kilo/plans/1791069821704-go-to-rust-migration.md). It is deliberately
# dependency-free: it reads the kubelet summary API (always present, unlike a
# metrics-server) and talosctl for the ZFS ARC, never Prometheus.
#
# Usage:
#   scripts/memory-baseline.sh                       # 3 samples, 60s apart (idle)
#   SAMPLES=1 scripts/memory-baseline.sh
#   SAMPLES=3 INTERVAL=10 scripts/memory-baseline.sh
#   NODE=talos-nu3-7j0 scripts/memory-baseline.sh
#
# Run it once idle and once under load (a share/backup drill); the Phase 0 gate
# is available >= TARGET_FREE_MIB under load.
#
# Env:
#   KUBECONFIG       standard; defaults to the ambient config
#   TALOSCONFIG      standard; honoured by talosctl for the ARC reading
#   TALOS_ENDPOINT   node IP for talosctl -e/-n (defaults to the node InternalIP)
#   NODE             node name (defaults to the first Ready node)
#   SAMPLES          number of samples (default 3)
#   INTERVAL         seconds between samples (default 60)
#   TARGET_FREE_MIB  available-memory floor for custom apps (default 2560 = 2.5 GiB)
set -euo pipefail

SAMPLES="${SAMPLES:-3}"
INTERVAL="${INTERVAL:-60}"
TARGET_FREE_MIB="${TARGET_FREE_MIB:-2560}"
KUBECTL="${KUBECTL:-kubectl}"
TALOSCTL="${TALOSCTL:-talosctl}"

command -v "$KUBECTL" >/dev/null || { echo "ERROR: $KUBECTL not found" >&2; exit 1; }

if [ -z "${NODE:-}" ]; then
  NODE="$("$KUBECTL" get nodes -o jsonpath='{.items[*].metadata.name}' | awk '{print $1}')"
fi
[ -n "$NODE" ] || { echo "ERROR: no Ready node found" >&2; exit 1; }

if [ -z "${TALOS_ENDPOINT:-}" ]; then
  TALOS_ENDPOINT="$("$KUBECTL" get node "$NODE" \
    -o jsonpath='{.status.addresses[?(@.type=="InternalIP")].address}' | awk '{print $1}')"
fi

read -r CAP_MEM ALLOC_MEM <<EOF
$("$KUBECTL" get node "$NODE" -o json | python3 -c '
import sys, json
s = json.load(sys.stdin)["status"]
print(s["capacity"].get("memory", "?"), s["allocatable"].get("memory", "?"))
')
EOF

# Read one ARC stat key (bytes). talosctl first; fall back to the terminal
# pod's host chroot when TALOSCONFIG is not usable from here.
arc_key() {
  local key="$1" raw
  raw="$("$TALOSCTL" -e "$TALOS_ENDPOINT" -n "$TALOS_ENDPOINT" \
    read /proc/spl/kstat/zfs/arcstats 2>/dev/null || true)"
  if [ -z "$raw" ]; then
    # Talos has no `cat` in its host rootfs, so `chroot /host cat` fails
    # (verified). Read through the terminal pod's host-root bind mount with the
    # pod image's own cat instead.
    raw="$("$KUBECTL" -n naslos-privileged exec deploy/naslos-terminal -- \
      cat /host/proc/spl/kstat/zfs/arcstats 2>/dev/null || true)"
  fi
  printf '%s\n' "$raw" | awk -v k="$key" '$1==k {print $3; exit}'
}

sample() {
  local ts cmax c size
  ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "=============================================================="
  echo "sample  $ts   node=$NODE   endpoint=$TALOS_ENDPOINT"
  echo "capacity=$CAP_MEM   allocatable=$ALLOC_MEM"
  "$KUBECTL" get --raw "/api/v1/nodes/$NODE/proxy/stats/summary" | python3 -c '
import sys, json
d = json.load(sys.stdin)
n = d["node"]["memory"]
def mib(b): return b / 1048576.0
print("node memory: available={:.0f} MiB  usage={:.0f} MiB  workingSet={:.0f} MiB  rss={:.0f} MiB".format(
    mib(n["availableBytes"]), mib(n["usageBytes"]), mib(n["workingSetBytes"]), mib(n["rssBytes"])))
rows = []
for p in d["pods"]:
    ws = sum(c.get("memory", {}).get("workingSetBytes", 0) for c in p.get("containers", []))
    rss = sum(c.get("memory", {}).get("rssBytes", 0) for c in p.get("containers", []))
    rows.append((ws, rss, p["podRef"]["namespace"], p["podRef"]["name"]))
rows.sort(reverse=True)
for ws, rss, ns, name in rows:
    print("  {:8.1f} MiB ws  {:8.1f} MiB rss  {}/{}".format(mib(ws), mib(rss), ns, name))
print("  TOTAL {:.1f} MiB working set over {} pods".format(mib(sum(r[0] for r in rows)), len(rows)))
'
  cmax="$(arc_key c_max)"; c="$(arc_key c)"; size="$(arc_key size)"
  if [ -z "$cmax$c$size" ]; then
    echo "WARN: could not read ZFS arcstats (talosctl and terminal-pod fallback both failed); values below are unknown, not zero" >&2
  fi
  echo "zfs arc: c_max=$(( ${cmax:-0} / 1048576 )) MiB  c=$(( ${c:-0} / 1048576 )) MiB  size=$(( ${size:-0} / 1048576 )) MiB"
}

echo "memory-baseline: samples=$SAMPLES interval=${INTERVAL}s TARGET_FREE=${TARGET_FREE_MIB} MiB"
for i in $(seq 1 "$SAMPLES"); do
  sample
  [ "$i" -lt "$SAMPLES" ] && sleep "$INTERVAL"
done

echo "--------------------------------------------------------------"
echo "Phase 0 gate: node available memory must be >= TARGET_FREE=${TARGET_FREE_MIB} MiB under load."
echo "Override TARGET_FREE_MIB; record the observed numbers in the PR / AI_Handoff.md."
