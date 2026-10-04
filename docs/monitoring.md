# Monitoring

Naslos ships one monitoring layer: a **live metrics API**
(`GET /api/metrics`, `GET /api/dashboard`) served by `naslos-api` from the
`metrics` package. There is **no Prometheus, Alertmanager or Grafana** in the
chart.

The Prometheus + Alertmanager subchart was removed on 2026-10-04 (Phase 0 of the
Go→Rust RAM plan): together they cost ~265 MiB of resident memory on the
single-node appliance, and nothing consumed their data. Grafana had already been
removed on 2026-09-19 (AUDIT-H4). See `docs/AUDIT-2026-09-19-REPORT.md`.

## Metrics model

`metrics.Manager` keeps a single `SystemMetrics` snapshot:

```text
SystemMetrics
├── cpu     { usagePercent, cores, loadAvg1/5/15 }
├── memory  { total, used, free, available, usagePercent, swapTotal, swapUsed }
├── disk    { total, used, free, usagePercent }
├── network { bytesSent, bytesRecv, packetsSent, packetsRecv,
│             interfaces[ { name, ipAddress, macAddress, bytesSent, bytesRecv } ] }
├── zfs     { pools[ { name, size, alloc, free, usagePercent, health } ] }
├── system  { hostname, uptime, os, kernel, talosVersion, lastBoot }
└── updatedAt
```

- `GET /api/metrics` returns the whole object.
- `GET /api/dashboard` returns the home-screen projection (CPU/memory/disk
  usage, ZFS pool list, hostname/uptime/OS, `updatedAt`).

The manager is intentionally a pass-through: a collector process fills the
snapshot; the API serves it under a mutex. The payload is JSON, so it is not a
Prometheus scrape target.

## Node and workload measurement

Because there is no metrics-server, operator measurement uses the dependency-free
kubelet summary API and `talosctl` — this is what the RAM baseline
(`scripts/memory-baseline.sh`) does:

```bash
kubectl get --raw "/api/v1/nodes/<node>/proxy/stats/summary"   # per-pod working set
talosctl read /proc/spl/kstat/zfs/arcstats                     # ARC c_max / size
```

`kubectl top` is only available if a metrics-server is installed; the summary API
is always present and is the source of truth for the baseline.

## Relating to the dashboard UI

The SvelteKit home screen (`ui/src/lib/components/Dashboard.svelte`) renders
`GET /api/dashboard`. There is no long-term store and no external dashboard to
link to; the live snapshot is the whole story. If long-term retention is ever
wanted, it has to be added back deliberately (and sized against the node's RAM
budget) — it is not a silent dependency.

See [api.md](api.md) for the exact routes and [deployment.md](deployment.md)
for Helm values that control these components.
