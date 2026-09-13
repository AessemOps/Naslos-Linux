# Naslos System Specification

**Version:** 0.1.0 · **Status:** Normative · **Supersedes:** none (complements the topical docs in this directory)

This document is the complete normative specification of Naslos. It defines
*what the system MUST do* — requirements, contracts, data models, and
acceptance criteria. The other documents in `docs/` describe *how it currently
does it*; where they disagree with this spec, this spec wins. Where the
implementation lags a requirement, the requirement is marked **[OPEN]** and
tracked in `AI_Handoff.md`.

The key words **MUST**, **MUST NOT**, **SHOULD**, **SHOULD NOT**, and **MAY**
are to be interpreted as described in RFC 2119.

---

## 1. Overview

### 1.1 Product definition

Naslos is a user-friendly NAS distribution built on **stock Talos Linux**.
It layers ZFS storage management, an app catalog, SMB/NFS/Time-Machine
shares, LDAP-backed single sign-on, monitoring, and notifications on top of an
unmodified Talos installation, administered through a web UI.

### 1.2 Goals

1. Talos upgrades MUST remain clean (`talosctl upgrade`) — no base-image
   modification, ever.
2. A single password MUST grant access to the web UI, SMB, NFS, and Time
   Machine.
3. ZFS pool lifecycle (create, import, destroy, monitor) MUST be doable from
   the web UI without shell access.
4. Applications from the catalog MUST be deployable and configurable from the
   web UI without writing YAML.
5. Node health (CPU, memory, disk, network, uptime) MUST be visible on the
   dashboard with at-most-seconds staleness.

### 1.3 Non-goals

- Multi-tenancy beyond LDAP users/groups.
- Talos base-image forks, custom kernels, or non-ZFS storage backends.
- Managing clusters of arbitrary size: the current release targets a
  **single-node** cluster (one control-plane node); multi-node aggregation of
  metrics is **[OPEN]**.
- Replacing Kubernetes tooling: Naslos sits on top of k8s, not beside it.

### 1.4 Target platform

| Constraint | Requirement |
| --- | --- |
| OS | Stock Talos Linux with the official `siderolabs/zfs` Image Factory extension |
| Cluster | Kubernetes ≥ 1.30 in the `naslos` namespace |
| Architecture | amd64 |
| Deployment | Helm umbrella chart `charts/naslos` |

---

## 2. System architecture (normative)

### 2.1 Components

| Component | Kind | Role |
| --- | --- | --- |
| `naslos-api` | Deployment | The brain: Talos API + K8s API + ZFS orchestration + HTTP API for the UI |
| `naslos-agent` | privileged DaemonSet (hostNetwork) | Executes `zpool`/`zfs` via `chroot /host` on each node |
| `naslos-ui` | Deployment (nginx, static SvelteKit) | Dashboard, wizards, terminal, admin pages |
| `naslos-openldap` | Deployment | Identity store (users, groups, password hashes) |
| `naslos-traefik` | DaemonSet | Ingress (IngressRoutes), TLS termination, forwardAuth to Authelia |
| Authelia | Deployment | Web SSO / 2FA against OpenLDAP |
| ntfy | Deployment | Push notifications for system events |
| Prometheus + Grafana | Deployments | Long-term metrics and dashboards |
| `zfs-service` | Talos system service | Auto-imports pools at boot (`zpool import -fal`) |

### 2.2 Trust boundaries and security requirements

- **SEC-1** — `naslos-agent` MUST run privileged (host mounts + `chroot /host`)
  solely to execute ZFS commands; it MUST NOT expose any endpoint that runs
  arbitrary shell input. All command arguments MUST be generated server-side.
- **SEC-2** — LDAP client connections MUST use LDAPS (TLS) with a CA cert
  (`LDAP_USE_TLS=true` default).
- **SEC-3** — `naslos-api` MUST only honor Traefik-injected authentication
  headers (`Remote-User` etc.) from requests originating in `TRAEFIK_CIDR`
  (default `10.0.0.0/8`).
- **SEC-4** — The user's LDAP password MUST be mirrored to Samba's passdb as
  an NT hash at password-change time; plaintext passwords MUST NOT be stored
  anywhere.
- **SEC-5** — Service secrets (`LDAP_BIND_PASS`, ntfy topic tokens) MUST be
  injected by the Helm chart, never baked into images.

### 2.3 Request flow (normative)

```
Browser → Traefik (TLS, IngressRoute) → forwardAuth (Authelia → OpenLDAP)
       → naslos-ui (static) ── XHR /api/* ──► naslos-api ──► Talos API / K8s API
                                              │
                                              └─► naslos-agent (zpool/zfs on node)
```

---

## 3. Functional requirements

Requirement IDs are stable: never renumber, only deprecate.

### 3.1 Storage — ZFS (`FR-STO`)

- **FR-STO-01** — The system MUST create ZFS pools with best-practice
  defaults: `ashift=12`, `compression=zstd`, `xattr=sa`,
  `acltype=posixacl`, `atime=off`.
- **FR-STO-02** — The disk wizard MUST advise a topology (mirror / RAIDZ1 /
  RAIDZ2) from the number and size of selected disks.
- **FR-STO-03** — Pools MUST be importable from the UI when they exist on
  disk but are not yet imported (`zpool import` discovery + import).
- **FR-STO-04** — Pools MUST auto-import at boot via `zfs-service`
  (`zpool import -fal`).
- **FR-STO-05** — Pool deletion MUST require explicit user confirmation and
  MUST NOT be undoable (the UI MUST state this).
- **FR-STO-06** — Pool health view MUST expose: device tree, IO stats, scan
  state, and error summary (from `zpool status` / `zpool iostat`).
- **FR-STO-07** — Datasets and snapshots on a pool MUST be creatable and
  listable from the UI via the agent.

### 3.2 Identity & SSO (`FR-IDN`)

- **FR-IDN-01** — Users and groups MUST be stored in OpenLDAP under
  `dc=naslos,dc=local` (people: `ou=people`; groups: `ou=groups`).
- **FR-IDN-02** — Users MUST be creatable with uid, display name, email, and
  password. The uid MUST be normalized to lowercase and rejected if invalid.
- **FR-IDN-03** — Every user MUST have a stable integer `uidNumber`,
  derived deterministically when not supplied.
- **FR-IDN-04** — Group membership MUST be displayed in the Users table by
  **short name** (`jdoe`, `testgroup`), never as a full DN.
- **FR-IDN-05** — A user's `enabled` state MUST map from `shadowExpire`:
  absent or `-1` ⇒ enabled; `0` (or any past date) ⇒ disabled. Enable/disable
  MUST be togglable from the UI.
- **FR-IDN-06** — Groups MUST be creatable with an **optional** description;
  an empty description MUST be omitted from the LDAP entry, not stored as an
  empty value (LDAP rejects empty attribute syntax).
- **FR-IDN-07** — `groupOfNames` requires ≥1 member; internal placeholder
  members (e.g. `cn=placeholder`) MUST be filtered out of API responses.
- **FR-IDN-08** — All list endpoints (`/api/users`, `/api/groups`) MUST
  return a JSON **array** (`[]` when empty), never `null`. This is a hard
  contract: the UI MUST NOT freeze on empty directories.
- **FR-IDN-09** — Password changes MUST update both LDAP `userPassword` and
  the Samba NT hash (same password everywhere).
- **FR-IDN-10** — Web login MUST flow through Authelia forwardAuth; API
  routes behind Traefik MUST reject unauthenticated requests.
- **FR-IDN-11** — LDAP connectivity MUST NOT be established at process start.
  The client MUST connect lazily and re-establish the connection automatically
  after failure, so that:
  - LDAP being unavailable when the API starts (e.g. both pods restarting
    together) MUST NOT disable identity permanently;
  - an LDAP restart at runtime MUST be recovered from without restarting the
    API pod;
  - while LDAP is down, identity routes MUST return `503` whose body names the
    underlying cause, and MUST recover once LDAP returns.
- **FR-IDN-12** — LDAP outages MUST NOT make non-identity features unavailable:
  the dashboard, pools, disks, apps, shares and metrics MUST keep working, and
  the API pod MUST NOT be considered unready because of LDAP.

### 3.3 Shares (`FR-SHR`)

- **FR-SHR-01** — Shares MUST be backed by ZFS datasets.
- **FR-SHR-02** — The system MUST support SMB, NFS, and Time Machine (AFP)
  share types, generating Samba `smb.conf`, NFS-Ganesha, and Avahi configs
  from the share model.
- **FR-SHR-03** — Effective Samba/NFS configuration MUST be retrievable from
  the API (`/api/shares/config/samba`, `/api/shares/config/nfs`).

### 3.4 App catalog (`FR-APP`)

- **FR-APP-01** — Apps MUST be defined as catalog entries (name, description,
  Helm chart, JSON-Schema form).
- **FR-APP-02** — App configuration forms MUST be generated from the app's
  JSON Schema.
- **FR-APP-03** — Installing/upgrading an app MUST be a Helm operation in
  the `naslos` namespace.
- **FR-APP-04** — Start/stop MUST scale replicas 0↔N and MUST preserve PVCs.

### 3.5 Dashboard & metrics (`FR-MET`)

- **FR-MET-01** — A background collector MUST poll the Talos API, publish a
  `SystemMetrics` snapshot to the metrics manager, and collect once
  immediately at startup.
- **FR-MET-02** — Collection interval MUST default to **5 seconds** and be
  overridable via `METRICS_INTERVAL_SECONDS`.
- **FR-MET-03** — `GET /api/metrics` MUST return the live snapshot with this
  shape (all byte quantities in **bytes**, not KB):

  ```json
  {
    "cpu":     { "usagePercent": 1.7, "cores": 6, "loadAvg1": 0.1, "loadAvg5": 0.2, "loadAvg15": 0.1 },
    "memory":  { "total": 10441696256, "used": 3108004864, "free": 3217534976,
                 "available": 7153684480, "usagePercent": 29.7, "swapTotal": 0, "swapUsed": 0 },
    "disk":    { "total": 43580284928, "used": 5165384704, "free": 38414900224, "usagePercent": 11.8 },
    "network": { "bytesSent": 462901472, "bytesRecv": 1348276586,
                 "packetsSent": 755373, "packetsRecv": 797224,
                 "interfaces": [ { "name": "eth0", "ipAddress": "" } ] },
    "zfs":     { "pools": [ { "name": "tank", "size": 0, "alloc": 0, "free": 0,
                              "usagePercent": 0.0, "health": "ONLINE" } ] },
    "system":  { "hostname": "talos-…", "uptime": 18654, "os": "Talos Linux",
                 "kernel": "6.12.8-talos", "talosVersion": "v1.10.x" },
    "updatedAt": "2026-09-12T01:30:00Z"
  }
  ```
- **FR-MET-04** — Disk metrics MUST reflect the writable root filesystem. On
  Talos, `/` is a read-only squashfs; the collector MUST prefer the `/var`
  mount (EPHEMERAL partition) and fall back to `/` only if `/var` has zero
  size.
- **FR-MET-05** — Memory MUST be converted from Talos `MemInfo` (KB units,
  mirroring `/proc/meminfo`) to bytes.
- **FR-MET-06** — Network totals MUST be summed from `/proc/net/dev` using
  the real byte/packet columns; the header line MUST NOT be parsed as an
  interface.
- **FR-MET-07** — `GET /api/dashboard` MUST return the dashboard view model
  and MUST overlay **live** ZFS pool data from the agent (agent is the source
  of truth for pool state) even between collection ticks.
- **FR-MET-08** — The dashboard UI MUST auto-refresh every **5 seconds**
  without a page reload, skip overlapping in-flight requests, and stop its
  timer on navigation (`onDestroy`).
- **FR-MET-09** — The UI MUST display a visible error message when the
  dashboard API is unreachable, and MUST NOT render the Go zero-time
  (`0001-01-01…`) as a timestamp.
- **FR-MET-10** — Per-interface IP addresses in the UI and multi-node metric
  aggregation are **[OPEN]** (interface names are collected; IPs are not yet).

### 3.6 Logs & terminal (`FR-LOG`)

- **FR-LOG-01** — Log streaming and shell access MUST use WebSockets
  (`/api/ws/logs`, `/api/ws/exec`).
- **FR-LOG-02** — The web terminal MUST run zsh in a privileged container
  with host mounts (Talos has no shell by design).

### 3.7 Notifications (`FR-NTF`)

- **FR-NTF-01** — System events (Talos / Kubernetes / ZFS) MUST push to ntfy
  topics with a severity level.
- **FR-NTF-02** — The API MUST expose `POST /api/notifications/test` to send
  a test notification.

---

## 4. API contracts (normative)

### 4.1 Conventions

- **API-01** — All `naslos-api` routes live under `/api/`; agent routes under
  `/api/v1/`.
- **API-02** — Errors MUST use `{"error": "<message>"}` with a proper HTTP
  status (400 invalid input, 404 unknown, 405 wrong method, 503 dependency
  unavailable).
- **API-03** — Wrong HTTP method MUST yield `405`.
- **API-04** — List responses MUST be JSON arrays, never `null` (see
  FR-IDN-08).

### 4.2 `naslos-api` routes

| Route | Methods | Notes |
| --- | --- | --- |
| `/api/health` | GET | Liveness (process only, never LDAP-dependent) |
| `/api/ready` | GET | Readiness; always 200, reports `{"status","ldap":"up"\|"down"}` |
| `/api/auth/me` | GET | Authenticated user from trusted headers |
| `/api/users` | GET, POST | List (array) / create |
| `/api/users/{uid}` | GET, DELETE | Detail / delete |
| `/api/users/{uid}/password` | PUT | Change password (updates Samba hash too) |
| `/api/users/{uid}/enable` `/disable` | POST | `shadowExpire` toggling |
| `/api/groups` | GET, POST | List (array) / create (description optional) |
| `/api/groups/{cn}` | GET, PUT, DELETE | Detail / update members / delete |
| `/api/catalog` | GET | Available apps |
| `/api/catalog/{app}` | GET | App detail + JSON-Schema form |
| `/api/apps` | GET | Installed apps |
| `/api/apps/{app}` | GET, PUT, DELETE | Detail / configure / uninstall |
| `/api/disks` | GET | Node disks |
| `/api/disks/recommend` | POST | Topology advisor |
| `/api/volumes` | GET | Volumes overview |
| `/api/volumes/zfs` | GET | Pools (live via agent) |
| `/api/volumes/zfs/import` | POST | Import existing pool |
| `/api/volumes/zfs/{pool}` | GET, DELETE | Pool health / destroy |
| `/api/shares` | GET, POST | List / create |
| `/api/shares/{name}` | PUT, DELETE | Update / delete |
| `/api/shares/config/samba` `/nfs` | GET | Generated configs |
| `/api/notifications` | GET | Notification settings/state |
| `/api/notifications/test` | POST | Send test push |
| `/api/metrics` | GET | Live `SystemMetrics` (§3.5) |
| `/api/dashboard` | GET | Dashboard view model + live pools |
| `/api/ws/logs` `/api/ws/exec` | WS | Streams (§3.6) |
| `/*` | GET | UI static assets (legacy `/var/naslos/ui` path; the served UI is the `naslos-ui` deployment) |

### 4.3 `naslos-agent` routes (host-level)

| Route | Methods | Notes |
| --- | --- | --- |
| `/health` | GET | Liveness |
| `/api/v1/pools` | GET | List pools (via `zpool list`) |
| `/api/v1/pools/import` | POST | `zpool import` |
| `/api/v1/pools/{name}` | GET, DELETE | Status/iostat / destroy |
| `/api/v1/datasets/{pool}` | GET, POST | List / create datasets |
| `/api/v1/snapshots/{pool}` | GET, POST | List / create snapshots |

---

## 5. Data models (normative JSON)

- **DM-1 Person**: `{ "uid", "displayName", "email", "uidNumber", "enabled": bool, "groups": ["short-name", …] }`
- **DM-2 Group**: `{ "cn", "description?", "members": ["uid", …] }` — `members` MUST be an array (possibly empty), `description` omitted when empty.
- **DM-3 Pool**: `{ "name", "size", "alloc", "free", "usagePercent", "health" }` — sizes in bytes, parsed from human `zpool list` output.
- **DM-4 Share**: `{ "name", "type": "smb"|"nfs"|"timemachine", "dataset", "…type-specific options" }`
- **DM-5 SystemMetrics**: see FR-MET-03. `updatedAt` is RFC 3339; before the first successful collection it is the Go zero time and consumers MUST treat it as "no data yet".

---

## 6. Non-functional requirements

- **NFR-1 Performance** — Dashboard data MUST be at most ~5 s stale
  (FR-MET-02/08). UI auto-refresh MUST NOT stack requests.
- **NFR-2 Talos compatibility** — Talos upgrades MUST remain clean; Naslos
  MUST NOT modify the Talos base image.
- **NFR-3 Security** — see §2.2 (SEC-1…SEC-5).
- **NFR-4 Resilience** — The API MUST return usable (non-crashing) responses
  when a dependency is down: LDAP failures yield `503` with the underlying
  cause (FR-IDN-11) while non-identity features keep working (FR-IDN-12); the
  UI MUST show errors, never silently freeze.
- **NFR-5 Footprint** — UI assets MUST be minimized (e.g. the logo ships as a
  128×128 PNG, not the 1254×1254 original).

---

## 7. Acceptance criteria & verification

The authoritative executable acceptance suite is the Playwright suite in
`ui/tests/` against a live deployment:

| Test | Verifies |
| --- | --- |
| `dashboard.spec.ts` — live node metrics | FR-MET-03/04/05, FR-IDN-08 (live values, non-zero memory/cores, no zero-time) |
| `dashboard.spec.ts` — zfs pools | FR-STO-06, FR-MET-07 |
| `dashboard.spec.ts` — auto-refresh | FR-MET-08 (5 s re-render without reload) |
| `users.spec.ts` | FR-IDN-02, FR-IDN-08 |
| `groups.spec.ts` (both) | FR-IDN-06, FR-IDN-08 |
| `e2e.spec.ts` — full lifecycle | FR-IDN-01…05, FR-STO-07 (group add-member), delete paths |
| `logo.spec.ts` | NFR-5, asset served as real PNG (SPA-fallback guard) |

Go verification: `go build ./...` in `api/` and `agent/`; `go vet` clean;
`gofmt` clean on touched files; `npm run check` in `ui/` with 0 errors.

LDAP resilience (FR-IDN-11/12) is covered by
`api/internal/identity/client_test.go` (`TestNewClientIsLazyAndDoesNotFailWhenLDAPIsDown`,
`TestEnsureConnectionRetriesAfterFailure`) plus this live drill:

1. `kubectl -n naslos scale sts naslos-openldap --replicas=0`
2. `kubectl -n naslos rollout restart deploy/naslos-api` (the restart race)
3. `/api/groups` → `503` naming the cause; `/api/ready` → `ldap:down` but
   `200`; `/` and `/api/metrics` → `200`
4. `kubectl -n naslos scale sts naslos-openldap --replicas=1`
5. `/api/groups` → `200` **with the same API pod, 0 restarts**

Conformance rule: any PR that changes a MUST in this spec MUST update the
corresponding test in the same PR.

---

## 8. Versioning & conformance

- **VER-1** — The spec version matches the product version in `charts/naslos`
  values (`0.1.0` at time of writing).
- **VER-2** — Deployed images carry build suffixes (`naslos-api:0.1.0-12`,
  `naslos-ui:0.1.0-7`) because the registry reuses tags with
  `imagePullPolicy: IfNotPresent`; each deploy MUST retag to a fresh suffix
  and `kubectl set image` (container names are `api` and `ui`).
- **VER-3** — API additions SHOULD be backward compatible; breaking changes
  to §4/§5 contracts require a minor version bump and an update to
  `docs/api.md`.
- **VER-4** — Known gaps are tracked as **[OPEN]** requirements here and in
  `AI_Handoff.md`; a release MUST NOT claim conformance to an [OPEN]
  requirement.

---