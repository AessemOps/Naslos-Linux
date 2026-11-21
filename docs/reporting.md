# Historical reporting (design)

**Status:** `[OPEN]` — design only, not implemented. The normative requirements
are FR-RPT-01…04 in [spec.md](spec.md#372-historical-reporting-fr-rpt).

Naslos shows live metrics from the 5-second collector ([monitoring.md](monitoring.md)).
This document defines the bounded, local time-series store that will keep a
short history for the dashboard and a future Reporting page: **24 h / 7 d /
30 d**, embedded in the API, with **no bundled Prometheus** and no external
time-series service.

## 1. Goals and non-goals

### Goals

- Answer "what happened in the last day/week/month" for CPU, memory, network,
  ZFS and disk health without an operator installing anything.
- Fixed, predictable disk usage and query cost. A broken or deleted history
  database must never affect live metrics, alerts, shares or the dashboard.
- Keep the data on the node. No remote write, no cloud account, no telemetry.
- Single-node only, matching the product's current scope.

### Non-goals

- Long-term retention (more than 30 days) or capacity planning over months.
- Per-app, per-container, per-process or per-dataset cardinality.
- Prometheus/Grafana compatibility, PromQL, remote write, or an exporter.
- Alert evaluation over history (alerts stay transition-based, §3.5/FR-ALT).
- Annotations, incident timelines or log storage.
- Backup/restore of history: it is disposable telemetry, not configuration.

## 2. Why SQLite (instead of RRD)

| Option | Verdict |
| --- | --- |
| SQLite on the state volume | **Chosen.** Already a single-file embedded store, transactional, easy to test with `tempfile`, supports rollup tables and bounded retention in SQL. No new service. |
| RRD files | Works, but adds a file-per-series layout, fixed round-robin semantics that are awkward to query flexibly, and a new dependency for little gain at this scale. |
| Prometheus / VictoriaMetrics / InfluxDB | Rejected: a second stateful service, its own storage/retention/upgrade surface, and exactly the "bundle Prometheus" outcome the parity plan rules out. |

The API does **not** currently link SQLite. If the implementation uses
`rusqlite` (bundled SQLite), that is a direct dependency change and must update
`CREDITS.md` and `ui/package.json`/image review in the same change. The
alternative is a small file format of fixed-width buckets; the design below
assumes SQLite because it makes the bounds and queries explicit.

## 3. Series allowlist

Cardinality is bounded by a compile-time allowlist, not by whatever the node
happens to run. Proposed maximum: **256 series** and a **256 MiB** hard cap on
the store (see §5); expected usage at the cap is tens of MiB.

| Family | Series (per key) | Cap |
| --- | --- | --- |
| CPU | `cpu.usage_percent`, `cpu.load1`, `cpu.load5`, `cpu.load15`, `cpu.temperature_c` | 5 |
| Memory | `memory.total_bytes`, `memory.used_bytes`, `memory.available_bytes`, `memory.usage_percent`, `memory.swap_used_bytes` | 5 |
| Network | `network.<if>.rx_bytes_per_s`, `network.<if>.tx_bytes_per_s` for physical interfaces only | 16 interfaces × 2 |
| ZFS pools | `zfs.<pool>.used_bytes`, `.alloc_bytes`, `.free_bytes`, `.fragmentation_percent` | 8 pools × 4 |
| Disks | `disk.<device>.temperature_c`, `disk.<device>.wear_percent` | 64 disks × 2 |

Rules:

- Interface names are limited to the node's physical NICs (the same inventory
  `/api/network` reports); virtual, pod and loopback interfaces are excluded.
- Pool and disk names are sanitized into the series key and capped; once the cap
  is reached, new keys are skipped (with a single warning), never merged into a
  dynamic table.
- Network counters are stored as per-second rates derived by the recorder, not
  as cumulative counters, so rollups are meaningful and a counter reset (link
  flap or reboot) is handled at write time (a negative delta becomes a reset,
  not a spike).
- Non-numeric state (pool health, SMART PASSED/FAILED) stays in the alerting
  path. History stores numbers only; health transitions are visible through
  notifications, not graphed as magic numbers.

## 4. Storage model

One SQLite file on a dedicated volume, e.g. `/var/lib/naslos/reporting/history.db`
(chart value `api.reportingFile`, `[OPEN]`). The current 64 MiB state PVC is
**not** large enough to share; the implementation should either add a dedicated
`reporting` PVC (proposed 512 MiB–1 GiB) or raise the state PVC explicitly.

```sql
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA auto_vacuum = INCREMENTAL;
PRAGMA max_page_count = 65536;        -- hard cap: 65536 × 4 KiB = 256 MiB

CREATE TABLE IF NOT EXISTS series (
    id        INTEGER PRIMARY KEY,
    key       TEXT NOT NULL UNIQUE,
    kind      TEXT NOT NULL,          -- cpu | memory | network | zfs | disk
    unit      TEXT NOT NULL,          -- percent | bytes | bytes_per_s | celsius | load
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS samples (
    series_id  INTEGER NOT NULL,
    resolution INTEGER NOT NULL,      -- 60 | 300 | 3600 seconds
    bucket     INTEGER NOT NULL,      -- epoch seconds, floored to resolution
    min        REAL NOT NULL,
    max        REAL NOT NULL,
    avg        REAL NOT NULL,
    last       REAL NOT NULL,
    count      INTEGER NOT NULL,
    PRIMARY KEY (series_id, resolution, bucket)
) WITHOUT ROWID;
```

No table grows with app, user, dataset or container count. `samples` is keyed by
`(series, resolution, bucket)`, so an upsert is idempotent and a re-run after a
crash cannot duplicate points.

## 5. Sampling, rollups and retention

| Resolution | Window | Points per series | Source |
| --- | --- | --- | --- |
| `60` s (raw) | 24 h | 1 440 | one sample per collector tick |
| `300` s | 7 d | 2 016 | rollup of raw |
| `3600` s | 30 d | 720 | rollup of 5-minute buckets |

- The recorder ticks every **60 s** and reads the latest `SystemMetrics`
  snapshot (it does not add a second Talos poll). ZFS/disk series come from the
  agent poll the health watcher already performs, or a bounded independent read
  if the cache is stale.
- Each tick upserts the raw bucket. When a 5-minute bucket closes, it is
  recomputed from its raw rows (`min/max/avg/last/count`); when an hour closes,
  the 1-hour bucket is recomputed from its 5-minute rows. Rollups are derived,
  never accumulated blindly, so a missed tick degrades the point instead of
  corrupting the next level.
- A gap is a gap: the query returns the points that exist. The UI marks a gap
  rather than interpolating across a reboot or an outage.
- Pruning runs hourly (and once at startup):
  - raw older than 24 h;
  - 5-minute older than 7 d;
  - 1-hour older than 30 d.
  Deletes are bounded batches followed by `PRAGMA incremental_vacuum`, so a
  large prune cannot hold a long write lock during a dashboard refresh.
- Disk budget: ~4 176 rows per series across all resolutions. At the 256-series
  cap and realistic row/index overhead this is well under the 256 MiB hard cap;
  `max_page_count` is the final backstop. If the cap is reached, the oldest
  buckets are pruned first and a warning is logged once.

## 6. Write-path failure isolation

- Recording is best-effort and off the request path. The live collector hands
  the recorder a snapshot; it never waits for SQLite.
- A locked database (prune or checkpoint) skips that tick and logs at debug
  level; the next tick continues. No unbounded in-memory backlog: at most the
  latest snapshot is retried, so a long outage becomes a gap.
- A missing directory or unreadable/corrupt file is handled by moving the file
  aside (`.corrupt-<epoch>`) once, recreating the schema, and continuing. If
  recreation fails (read-only volume, disk full), history is reported
  unavailable and live metrics are untouched.
- The `reporting` volume is mounted only in the API pod, read-write, and is not
  exposed to apps or the agent. The API writes the file `0600` in a `0750`
  directory like the other state files.

## 7. Query and API contract (`[OPEN]`)

Read-only, authenticated like `/api/metrics` (any authenticated user). No write
endpoint exists; the recorder owns writes.

```
GET /api/reporting/history?range=24h|7d|30d&series=cpu.usage_percent,memory.used_bytes
```

- `range` is an enum, never an arbitrary duration.
- `series` is optional; omitted means the default dashboard set (CPU, memory,
  network totals, pool usage). The request is capped at **8 series** per call.
- The resolution is implied by the range: `24h → 60`, `7d → 300`, `30d → 3600`.
- The response is capped at the point counts in §5 (and at 720 points per
  series after server-side grouping if a future range needs it).
- Response shape:

  ```json
  {
    "range": "24h",
    "from": "2026-11-21T00:00:00Z",
    "to": "2026-11-21T23:59:00Z",
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

  Each point is `[bucket, min, max, avg, last, count]`. Unknown or disallowed
  series keys are ignored and reported in an `ignored` list; they are never
  turned into SQL identifiers.

The future UI is one Reporting page with a 24 h / 7 d / 30 d selector and small
multi-series charts (inline SVG/canvas, no charting dependency). It shows the
same allowlisted families and states the range and sample interval; it does not
offer arbitrary PromQL-style queries.

## 8. Testing plan (when FR-RPT lands)

Rust unit tests, all without a node:

- series allowlist and key sanitization; the 256-series cap;
- bucketing boundaries (59/60/61 s, 299/300/301 s, hour edges) and the
  raw → 5-minute → 1-hour rollup math, including gaps and counter resets;
- retention pruning per resolution and the 30-day floor;
- point caps and the 8-series query cap;
- a corrupt database is moved aside and recreated; a read-only directory
  degrades to "history unavailable" without touching live metrics.

API contract tests: range enum validation, disallowed series ignored, response
shape and bounds, auth gate. Playwright: the range selector renders, a seeded
database shows points, and an empty/corrupt database shows an explicit empty
state instead of an error page. A live drill compares one hour of history with
`/api/metrics` and confirms the database stays under its cap across a restart.

## 9. Rollout

1. Land this design and the `[OPEN]` FR-RPT requirements (this pass).
2. Add the recorder + schema behind `REPORTING_ENABLED` (default off), with the
   unit tests above; no UI.
3. Add `GET /api/reporting/history`, then the Reporting page; flip the default
   on once the volume sizing is in the chart.
4. Optional follow-up: per-dataset history (still a fixed allowlist) if users
   ask; never per-app cardinality.

Until step 3 ships, `/api/metrics` and the dashboard remain the only metric
surfaces and no history is written.
