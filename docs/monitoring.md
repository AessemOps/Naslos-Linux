# Monitoring

NasOS ships two monitoring layers:

1. A **live metrics API** (`GET /api/metrics`, `GET /api/dashboard`) served by
   `nasos-api` from the `metrics` package.
2. **Prometheus + Grafana** (Helm dependencies) with a 30-day retention and a
   preloaded `NasOS Overview` dashboard.

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

## Prometheus & Grafana

Wired through `charts/nasos/values.yaml`:

```yaml
prometheus:
  enabled: true
  prometheus:
    prometheusSpec:
      retention: 30d
      resources:
        requests: { cpu: 200m, memory: 512Mi }

grafana:
  enabled: true
  adminPassword: "nasos-admin"     # CHANGE ME
  dashboardProviders:
    dashboardproviders.yaml:
      providers:
        - name: nasos
          orgId: 1
          folder: NasOS
          type: file
          options:
            path: /var/lib/grafana/dashboards/nasos
  dashboards:
    nasos:
      nasos-overview:
        json: '{ "title": "NasOS Overview", "uid": "nasos-home" }'
```

- Prometheus scrapes Kubernetes metrics; retention is 30 days.
- Grafana reads a `NasOS Overview` dashboard from the provided JSON (extend
  `dashboards.nasos` with real panels).
- Both are Helm dependencies of the umbrella chart, enabled by default.

## Relating to the dashboard UI

The SvelteKit home screen (`ui/src/lib/components/Dashboard.svelte`) renders
`GET /api/dashboard` data; the Grafana dashboard is the deeper, long-term view
and can be linked from the UI.

See [api.md](api.md) for the exact routes and [deployment.md](deployment.md)
for Helm values that control these components.