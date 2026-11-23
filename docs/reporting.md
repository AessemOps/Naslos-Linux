# Historical reporting (FR-RPT)

**Status:** implemented subset. A bounded, embedded, append-only store records
CPU, memory, network, ZFS pool and disk SMART history on the node; the API and
the dashboard expose 24 h / 7 d / 30 d views. Remaining gaps are listed at the
end. Normative requirements are FR-RPT-01…04 (implemented) and FR-RPT-05…07
(`[OPEN]`) in [spec.md](spec.md#372-historical-reporting-fr-rpt).

Naslos shows live metrics from the 5-second collector ([monitoring.md](monitoring.md)).
This document describes the local time-series store that keeps a short history:
**24 h / 7 d / 30 d**, embedded in the API, with **no bundled Prometheus**, no
external time-series service and no new database dependency.

## 1. Goals and non-goals

### Goals

- Answer "what happened in the last day/week/month" for CPU, memory, network,
  ZFS and disk health without an operator installing anything.
- Fixed, predictable disk usage and query cost. A broken or deleted history
  file must never affect live metrics, alerts, shares or the dashboard.
- Keep the data on the node. No remote write, no cloud account, no telemetry.
- Single-node only, matching the product's current scope.

### Non-goals

- Long-term retention (more than 30 days) or capacity planning over months.
- Per-app, per-container, per-process or per-dataset cardinality.
- Prometheus/Grafana compatibility, PromQL, remote write, or an exporter.
- Alert evaluation over history (alerts stay transition-based, §3.5/FR-ALT).
- Annotations, incident timelines or log storage.
- Backup/restore of history: it is disposable telemetry, not configuration.

## 2. Why an append-only file instead of SQLite

The design originally assumed SQLite. The implementation uses a fixed-width
append-only file instead because the access pattern is tiny and completely
known: three resolutions, an allowlist of series, append-only writes, bounded
time-window scans. That keeps the API image free of a new database dependency
(no `rusqlite`, no bundled C SQLite) and makes corruption behaviour explicit.

| Option | Verdict |
| --- | --- |
| Fixed-width append-only records + atomic rewrite for pruning | **Implemented.** One file, CRC per record, bounded size, no dependency, easy to test. |
| SQLite on the reporting volume | Not used. It remains a reasonable fallback if query needs grow; adopting it is a direct dependency change (`CREDITS.md`, image review). |
| RRD files | Rejected: a file per series and fixed round-robin semantics add complexity for little gain at this scale. |
| Prometheus / VictoriaMetrics / InfluxDB | Rejected: a second stateful service and its own storage/retention/upgrade surface. |

## 3. Series allowlist

Cardinality is bounded by a compile-time allowlist, not by whatever the node
happens to run: **256 series**, **8 series per query**, and a **64 MiB**
default file cap (chart value `api.reporting.maxBytes`).

| Family | Series (per key) | Cap |
| --- | --- | --- |
| CPU | `cpu.usage_percent`, `cpu.load1`, `cpu.load5`, `cpu.load15`, `cpu.temperature_c` | 5 |
| Memory | `memory.total_bytes`, `memory.used_bytes`, `memory.available_bytes`, `memory.usage_percent`, `memory.swap_used_bytes` | 5 |
| Network | `network.<if>.rx_bytes_per_s`, `network.<if>.tx_bytes_per_s` | 16 interfaces × 2 |
| ZFS pools | `zfs.<pool>.used_bytes`, `.alloc_bytes`, `.free_bytes` | 8 pools × 3 |
| Disks | `disk.<device>.temperature_c`, `disk.<device>.wear_percent` | 64 disks × 2 |

Rules:

- Interface names are filtered to likely physical NICs (loopback, `veth`,
  `docker`, `cni`, `flannel`, `cali`, Kubernetes/overlay/tunnel/bridge prefixes
  are excluded), sorted and capped at 16. The filter is deliberately
  conservative: an unusual virtual name may still be recorded, but the cap
  bounds the damage.
- Pool and disk names are sanitized into one series component (`[A-Za-z0-9._-]`,
  other characters collapse to `_`, 40-character limit); after the 256-series
  cap, new keys are skipped with a single warning and never merged.
- Network counters are stored as per-second rates derived at write time, not as
  cumulative counters. A negative delta (link flap or reboot) is skipped, so a
  reset is a gap rather than a spike.
- Non-numeric state (pool health, SMART PASSED/FAILED) stays in the alerting
  path. History stores numbers only.

## 4. Storage model

One file, `history.bin`, plus a small `series.json` registry, on a dedicated
reporting volume (chart: `api.reporting.*`, default
`/var/lib/naslos/reporting`). The file is `0600` in a `0750` directory like the
other state files. It is mounted read-write only in the API pod.

```
history.bin
  header: "NASLOSRPT1\n" (11 bytes)
  record: 56 bytes, little-endian
    CRC32 over bytes 4..56       4
    series id                   4
    resolution (60/300/3600)    4
    sample count                4
    bucket start (epoch s)      8
    min, max, avg, last         4 × 8
```

- A record is appended per series per resolution bucket. Writes are
  `write_all` appends; no read-modify-write, no page cache database.
- `series.json` maps the human key to a stable numeric id plus `kind`/`unit`.
  It is written atomically (temp file + fsync + rename) only when a new series
  is first seen.
- A corrupt header moves the file aside to `history.bin.corrupt-<epoch>` and
  starts a new store. A corrupt or truncated tail is truncated to the last valid
  record. A CRC failure is treated as the end of valid history.
- Every scan streams one record at a time. Queries filter by resolution, series
  and window before accumulating, retention keeps only the records it retains,
  and the startup rollup rebuild releases the initial scan before reading the
  file again, so a full history does not double peak memory during a rollup.
- The registry is independent: if it is corrupt it moves aside and old records
  are orphaned telemetry (pruned normally) rather than a startup failure. On
  open, the next id is raised above every id present in `history.bin`, so a
  lost registry (or a crash before the registry save) cannot make new series
  reuse an old id and silently serve old samples under a new key.

## 5. Sampling, rollups and retention

| Resolution | Window | Points per series | Source |
| --- | --- | --- | --- |
| `60` s (raw) | 24 h | 1 440 | one sample per recorder tick |
| `300` s | 7 d | 2 016 | rollup of raw |
| `3600` s | 30 d | 720 | rollup of 5-minute rows |

- The recorder ticks every **60 s** (`REPORTING_INTERVAL_SECONDS`), reads the
  latest `SystemMetrics` snapshot and performs one bounded agent call for disk
  SMART values. It never adds a second Talos poll and never delays the 5-second
  collector.
- Rollups are derived, never accumulated blindly:
  - a closed 5-minute bucket is recomputed from its raw rows;
  - a closed 1-hour bucket is recomputed from its 5-minute rows.
  A missed tick degrades a rollup point instead of corrupting the next level.
  At startup, missing rollups still covered by raw/5-minute retention are
  rebuilt before serving queries.
- A gap is a gap: the API omits missing buckets and never invents or
  interpolates samples. The dashboard `Sparkline` positions adjacent returned
  points by index, so a long outage looks like a compressed line rather than a
  time-proportional hole; the reported values themselves are still the stored
  buckets.
- Pruning runs hourly (and once at startup):
  - raw older than 24 h;
  - 5-minute older than 7 d;
  - 1-hour older than 30 d.
  A prune rewrites the retained records to a temporary file, fsyncs, opens the
  replacement handle and renames it atomically, so a crash cannot leave a
  half-pruned store and a failed reopen cannot leave later appends on an
  unlinked inode; test `a_failed_rewrite_keeps_the_store_appendable`.
- If the file reaches `api.reporting.maxBytes`, the oldest buckets are dropped
  first and a warning is logged once. That is the final backstop; normal
  retention keeps expected usage in the tens of MiB.

## 6. Write-path failure isolation

- Recording is best-effort and off the request path. The recorder is its own
  tokio task; a slow disk means a missing point, not a slow dashboard.
- A write failure is logged once and retried on the next tick. The store stays
  readable; the API reports `available:false` with a short error when it cannot
  serve history.
- A missing directory, read-only volume, disk full or unopenable file degrades
  to "no history". Live metrics, alerts, shares and the dashboard are
  untouched. There is no unbounded in-memory backlog: at most the latest
  snapshot is retained while the store is unavailable.
- The reporting volume is mounted only in the API pod. It is not exposed to
  apps or the agent; a full or corrupt reporting volume cannot fill the config
  volume.

## 7. Query and API contract

Read-only and **admin-gated** (the API's owner router). No write endpoint
exists; the recorder owns writes.

```
GET /api/reports
GET /api/reports/series?range=24h|7d|30d&metric=cpu.usage_percent,memory.used_bytes
```

- `/api/reports` returns the fixed ranges (`range`, `resolution`,
  `window_seconds`, `max_points`), the series registry present in the store, the
  caps, and `enabled`/`available`/`error`.
- `range` is an enum, never an arbitrary duration. The resolution is implied:
  `24h → 60`, `7d → 300`, `30d → 3600`.
- `metric` accepts a comma-separated list. `series` is accepted as the design
  doc's older spelling. Omitted means the default set (CPU usage, memory usage,
  memory used, then the first available families), capped at **8** series.
- Unknown or disallowed keys are never turned into SQL or file paths; they are
  returned in `ignored`. More than 8 requested keys puts the remainder in
  `ignored`.
- The window is `(from, to]`: `from` is the start of the requested range, so a
  complete 24 h / 7 d / 30 d view contains exactly 1 440 / 2 016 / 720 buckets
  and keeps its stored interval. Points are capped per range; if stored rows
  ever exceed the cap, one factor is chosen for the whole response so every
  series shares the reported `sample_interval_seconds`, and adjacent buckets
  are merged server-side (`min`, weighted `avg`, latest `last`, summed
  `count`).
- Response shape:

  ```json
  {
    "range": "24h",
    "resolution": 60,
    "sample_interval_seconds": 60,
    "from": "2026-11-21T00:00:00Z",
    "to": "2026-11-21T23:59:00Z",
    "available": true,
    "error": null,
    "ignored": [],
    "series": [
      {
        "key": "cpu.usage_percent",
        "kind": "cpu",
        "unit": "percent",
        "points": [[1732233600, 1.7, 12.4, 4.1, 3.2, 60]]
      }
    ]
  }
  ```

  Each point is `[bucket, min, max, avg, last, count]`. Units are `percent`,
  `bytes`, `bytes_per_s`, `celsius` and `load`.

The dashboard History card (`ui/src/lib/components/Dashboard.svelte`) shows a
range selector (24 h / 7 d / 30 d), a metric picker grouped by family, an
inline `Sparkline`, and explicit empty/error states. The section is hidden for
non-admin users (the API returns 403) and reloads every 60 s.

## 8. Chart and configuration

| Value | Default | Meaning |
| --- | --- | --- |
| `api.reporting.enabled` | `true` | Create the PVC/recorder; `false` serves live metrics only |
| `api.reporting.mountPath` | `/var/lib/naslos/reporting` | Mount point in the API pod |
| `api.reporting.file` | `<mountPath>/history.bin` | Store path (`series.json` sits beside it) |
| `api.reporting.maxBytes` | `67108864` (64 MiB) | Hard file-size cap |
| `api.reporting.intervalSeconds` | `60` | Recorder cadence |
| `api.reporting.size` | `256Mi` | Dedicated reporting PVC size |

Optional env overrides: `REPORTING_ENABLED`, `REPORTING_FILE`,
`REPORTING_MAX_BYTES`, `REPORTING_INTERVAL_SECONDS`.

## 9. Testing

Rust unit tests (`rust/api/src/reporting.rs`, no node required):

- bucketing boundaries (59/60/61 s, 299/300/301 s, hour edges);
- weighted `min/max/avg/last/count` merging;
- raw → 5-minute → 1-hour derivation, including a missed tick (gap);
- retention per resolution and the 30-day floor;
- the 256-series cap and stable ids across a registry reload;
- network counter resets become gaps, not spikes;
- a corrupt header is moved aside; a corrupt/truncated tail is repaired and can
  be written again; an unopenable path degrades to unavailable;
- point downsampling and the 8-series query cap.

API contract tests (`rust/api/src/server/reports.rs`): range enum validation,
missing range, default selection, ignored keys, the 8-series cap, response
shape, disabled-store behaviour, and the admin gate through the shared route
inventory. `cargo test -p naslos-api`, `npm run check`, `npm run build` and
`helm lint` are the local gates.

## 10. Remaining `[OPEN]`

- FR-RPT-05: `fragmentation_percent` is in the design's optional ZFS set but is
  not exposed by the agent pool list; do not synthesize it.
- FR-RPT-06: live drill comparing one hour of history with `/api/metrics` on
  the production node and confirming the size cap across a restart.
- FR-RPT-07: a dedicated Reporting page (beyond the dashboard card) and
  Playwright coverage for seeded, empty and corrupt states.
- Per-dataset history remains a non-goal until users ask for it; if added, it
  must stay a fixed allowlist and never become per-app cardinality.
