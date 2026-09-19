# Monitoring

Naslos ships two monitoring layers:

1. A **live metrics API** (`GET /api/metrics`, `GET /api/dashboard`) served by
   `naslos-api` from the `metrics` package.
2. **Prometheus + Alertmanager** (Helm dependencies) with a 30-day retention.
   Grafana was removed on 2026-09-19 (AUDIT-H4); the metrics API plus Prometheus
   are the current story.

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
snapshot; the API serves it under a mutex.

## Prometheus & Alertmanager

Wired through `charts/naslos/values.yaml`:

```yaml
prometheus:
  enabled: true
  prometheus:
    prometheusSpec:
      retention: 30d
      resources:
        requests: { cpu: 200m, memory: 512Mi }

# Grafana is NOT deployed: it was removed on 2026-09-19 (AUDIT-H4) because it
# shipped a committed default admin password and was unused (ClusterIP, no
# IngressRoute). Re-enabling it requires a credential from a Secret.
# See docs/AUDIT-2026-09-19-REPORT.md.
```

- Prometheus scrapes Kubernetes metrics; retention is 30 days.
- Alertmanager routes alerts; it is a Helm dependency of the umbrella chart.
- Grafana is **not deployed** (removed 2026-09-19). Re-enabling it needs a
  credential from a Secret, never the old committed default.

## Relating to the dashboard UI

The SvelteKit home screen (`ui/src/lib/components/Dashboard.svelte`) renders
`GET /api/dashboard` data; Prometheus is the deeper, long-term view (query it
directly or through the API; there is no Grafana dashboard to link to).

See [api.md](api.md) for the exact routes and [deployment.md](deployment.md)
for Helm values that control these components.