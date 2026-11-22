# Naslos API

Two HTTP APIs exist:

| API | Base | Runs as | Port |
| --- | --- | --- | --- |
| `naslos-api` | `/api/...` | Deployment in `naslos` namespace | 8080 (ClusterIP) |
| `naslos-agent` | `/api/v1/...` | DaemonSet on every node (hostNetwork, `naslos-privileged`) | 9090 |

The web UI is served by the separate `naslos-ui` SvelteKit deployment (static
files behind nginx); the IngressRoute routes the SPA to `naslos-ui` and lets
its relative `/api/*` calls reach the API on the same origin.

## Common behaviors

- All routes respond with JSON (`Content-Type: application/json`) unless noted.
- Error payload shape:
  ```json
  { "error": "message" }
  ```
- Unsupported methods return `405` and unknown paths `404`, both with the JSON
  error shape. Unknown `/api/*` paths stay behind the auth gate (401 without
  the proxy identity) so route existence is not leaked.
- `GET /api/ready` always answers `200` (LDAP being down must not take the
  dashboard down) and reports the measured state: `{"status":"ok","ldap":"up"|"down"}`.
- Authentication **is** enforced: the API is only reachable through Traefik's
  `proxy-identity` middleware, and `rust/api/src/auth.rs` independently requires the
  shared proxy secret plus a `Remote-User` header from the Traefik pod CIDR
  (see [identity-sso.md](identity-sso.md#header-trust)). The API refuses to start
  without the secret, so a request that bypasses Traefik cannot authenticate.

### Authorization roles (planned)

Today there are two levels: `naslos_users` (dashboard, metrics and
`/api/auth/me`) and `naslos_admins` (the rest of the authenticated `/api/*`
surface, via `require_admin`). Public `/api/health` and `/api/ready` and the
Ed25519-authenticated `/api/buddy/v1/*` peer routes are outside that
`require_admin` gate. The read-only/operator/admin role model
([rbac-audit.md](rbac-audit.md), spec `FR-RBAC`) is **[OPEN]** and will map
route families to a minimum role after the P0 route work lands. Until then the
route tables below keep the current admin-or-baseline behavior; no route is
delegated by this document.

## naslos-api routes

### Health & auth

| Method | Path | Description |
| --- | --- | --- |
| GET | `/api/health` | Liveness probe; always `{"status":"ok"}` and bypassed by Authelia |
| GET | `/api/auth/me` | Current user from `Remote-*` headers (username, groups, email, displayName) |

### Users

| Method | Path | Description |
| --- | --- | --- |
| GET | `/api/users` | List all users (uid, displayName, firstName, lastName, email, enabled, groups) |
| POST | `/api/users` | Create user: `{uid, displayName, firstName, lastName, email, password, groups[]}` |
| GET | `/api/users/{uid}` | User detail |
| PUT | `/api/users/{uid}` | Update user attributes (`displayName`, `firstName`, `lastName`, `email`) |
| DELETE | `/api/users/{uid}` | Delete user (also removes SMB password entry) |
| POST | `/api/users/{uid}/password` | Change password: `{password}` — syncs LDAP + Samba NT hash |
| POST | `/api/users/{uid}/enable` | Enable account (`shadowExpire = -1`) |
| POST | `/api/users/{uid}/disable` | Disable account (`shadowExpire = 1`) |

**Notes:**
- `uid` is case-insensitive and auto-normalized to lowercase.
- On creation, `uidNumber` is derived from a hash of the uid; all users get `gidNumber=10000`.
- `enabled` is derived from `shadowExpire`: `-1` or unset = enabled; `0` = expired/disabled; `1` = disabled.
  Authelia's LDAP `users_filter` excludes `shadowExpire=1`, so a disabled account cannot start a new
  SSO session (an existing session lasts until its normal expiry/inactivity limit).
- An empty `groups` array is returned on success (never `null`) to prevent UI freezes.

### Groups

| Method | Path | Description |
| --- | --- | --- |
| GET | `/api/groups` | List all groups (cn, description, members[]) |
| POST | `/api/groups` | Create group: `{cn, description}` — description is optional |
| GET | `/api/groups/{cn}` | Group detail |
| PUT | `/api/groups/{cn}` | Update group members: `{members: [uid, ...]}` |
| DELETE | `/api/groups/{cn}` | Delete group |

**Notes:**
- Groups use `groupOfNames` objectClass; a placeholder member (`cn=empty-members,ou=groups,...`) is added during creation to satisfy the schema's "at least one member" requirement, then filtered from API responses.
- The `description` attribute is optional and omitted from the LDAP add request when empty (OpenLDAP rejects empty string values).

### App catalog & installed apps

> Apps come from git-based chart repositories (official + admin-added) and
> install into `naslos-apps`. See [app-catalog.md](app-catalog.md).

| Method | Path | Description |
| --- | --- | --- |
| GET | `/api/catalog` | List catalog entries (summary incl. `source`, `channel`, `channels`, `iconUrl`) |
| GET | `/api/catalog/{name}` | Full entry incl. JSON Schema, `services`, `exposure`, `chartPath`, `iconUrl` |
| GET | `/api/apps` | Installed-app records with live status (one `helm list` per app namespace, resolved concurrently) and URL, plus `displayName`, `icon`, `iconUrl`, `installedVersion` (live Helm version when readable), `latestVersion`, `latestSource`, `latestChannel`, `updateAvailable`, `imageUpdateAvailable`, `imageCurrent` and `imageLatest` from the current catalog snapshot and cached image drift |
| POST | `/api/apps` | Enqueue an install job: `{name, values, exposure?, baseDomain?, confirmed:true, privilegedConfirmed?:true, pool?: "<zfs pool>", env?: [{name, value, secret?}]}`. `env` requires the chart's `env` capability (otherwise 400) and is stored in the `naslos-env-<release>` Secret; see [app-catalog.md](app-catalog.md#environment-variables). `exposure.protectedPaths` lists path prefixes that stay behind Authelia while the rest of the host is public (used with `auth:false`). `exposure.internet` defaults to `true`; `false` installs the app with DNS + app-to-app egress only and is rejected for privileged or shared namespaces. With `internet` on and a dedicated baseline namespace, the `<app>-internet` egress policy is applied before the Helm wait (and removed on failure, with a startup/reuse sweep for leftovers) so an app whose first start calls the public network (e.g. Bazarr) can become ready. `pool` puts every chart persistence PVC under `/var/mnt/<pool>/apps` through a pool-scoped StorageClass; the pool must exist. An Install for a name the platform already records is rejected with an `already installed; reconfigure or uninstall it` job error. `confirmed` is always required; an app whose catalog entry declares `privileged: true` additionally requires `privilegedConfirmed:true` (SEC-2026-D8). Values are merged with the catalog defaults and validated against its JSON schema inside the job, so a violation fails the job (the request is still `202`) (SEC-2026-D7). `datasets` is a list of registered dataset names (any number; duplicates are collapsed); with per-app isolation enabled each new app gets a reserved namespace, a PV/PVC per dataset for baseline apps, and every dataset is injected into the chart's `datasets` Helm value as `/datasets/<name>` (SEC-2026-D3) → `202 {jobId, state}`. The runner re-checks `privilegedConfirmed` against the catalog it resolves at job time. |
| GET | `/api/apps/{name}` | Record + release status, catalog fields (`displayName`, `icon`, versions), `isolated`, `dns` (the maintained CNAME state), and `env: [{name, value, secret}]` read from the env Secret (`value:null` for secret variables). The list endpoint does not read the Secret |
| PUT | `/api/apps/{name}` | Enqueue an upgrade job: `{values, datasets?, env?}` → `202 {jobId, state}`. An omitted `env` keeps the stored variables; `[]` removes them; a submitted list replaces them, where a `secret` variable with `value:null` keeps its stored value. The Secret is snapshotted and restored if Helm fails. An omitted `datasets` keeps the stored selection, `[]` detaches every dataset (an isolated app must keep at least one; the job fails otherwise) and a non-empty list reconciles adds/removes. A dataset change on a record outside a per-app isolation namespace fails the upgrade job with a reinstall message (the request itself is still `202`). New claims are created before Helm; removed claims are deleted and detached only after the upgrade succeeds. `{}` keeps stored values and datasets and is the update path used by the Installed UI. CPU/memory quantities under `resources` (flat or per component) are validated before Helm; an invalid quantity fails the job instead of timing out the wait. Aggregate totals are also checked against the app namespace's `naslos-app-quota` and fail with the quota name/limits. The drawer writes `{values:{resources}}` from its Resources editor |
| DELETE | `/api/apps/{name}` | Enqueue an uninstall job (removes the release, its route, the `naslos-env-<release>` Secret and the record) → `202 {jobId, state}` |
| GET | `/api/apps/jobs` | Lifecycle jobs: running plus the last ~20 finished |
| GET | `/api/apps/jobs/{id}` | One job's `state`, `stage`, `message` and `error` (the poll target) |
| GET | `/api/apps/updates` | Update summary `{count, apps:[{name, installedVersion, latestVersion, imageUpdate}]}` from records + catalog + the cached image-drift snapshot (no Helm or registry calls); drives the Installed tab badge and the Apps sidebar dot |
| GET | `/api/apps/namespaces` | Per-app namespace slot accounting: `{baseline:{provisioned, limit, used, available}, privileged:{...}}`. `provisioned` is the chart pool; `limit` is the usable count, raised/lowered from the Installed tab |
| PUT | `/api/apps/namespaces` | `{limit}` → `200` with the same shape. The limit may not exceed the provisioned pool (a larger pool needs a platform upgrade) or drop below the installed app count; it is persisted in `APPS_POOL_CONFIG` (`/var/lib/naslos/app-pool.json`) |
| GET | `/api/apps/usage` | Bulk CPU/RAM/network for every installed app, always `200`: `{available, sampledAt, error, apps:[{name, namespace, pods, cpuMillicores, memoryBytes, networkRxBytesPerSec, networkTxBytesPerSec}]}`. Numeric fields are `null` when unknown (no pods, or no network baseline yet). `available:false` + `error` when the kube client, RBAC or kubelet read fails, so the UI hides the metrics column instead of erroring. Network rates come from the summary's cumulative counters, so they are `null` until two samples exist. Server-cached ~4s with single-flight collection |
| GET | `/api/apps/{name}/exposure` | Exposure settings (including `protectedPaths`), the app's own `baseDomain` (fallback primary), `primaryDomain`, `selectableDomains`, `ssoDomains`, `authAllowed`, `lastError` and `dns` |
| PUT | `/api/apps/{name}/exposure` | Partial update `{exposure, baseDomain?}`, re-render the route and the per-app Internet `NetworkPolicy`; omitted exposure fields keep their stored value (SEC-2026-D11), and an unconfigured `baseDomain` is rejected (400). `exposure.protectedPaths` replaces the guarded prefix list; with `auth:false` the API emits a higher-priority `PathPrefix` route with Authelia and leaves the catch-all public. `exposure.internet` defaults to `true`; `false` removes the app's public-egress policy and is rejected with a 400 for privileged or shared namespaces |
| GET | `/api/apps/{name}/services` | Services the release rendered (route-target discovery/picker) |
| GET | `/api/apps/{name}/pods` | Pods and containers the drawer's log stream can attach to: `{namespace, pods:[{name, phase, startedAt, containers:[{name, ready, restartCount}]}]}`. Running pods first; a legacy app in the shared namespace is filtered by `app.kubernetes.io/instance` |
| POST | `/api/apps/{name}/dns/reconcile` | Converge the app's CNAME now and return the updated record. Errors are stored in `dns.status`/`dns.error` (the request is still `200`); a missing app is `404`. See [dynamic-dns.md](dynamic-dns.md#app-records-fr-dns-09) |
| GET | `/api/apps/{name}/quota` | Per-app namespace quota: `{available, namespace, hard:{requests.cpu, requests.memory, limits.cpu, limits.memory}}`; `{available:false}` for a legacy app without a dedicated quota. The drawer shows it and the upgrade path enforces it |

Install/upgrade/uninstall are **asynchronous** (FR-APP-18): the handler
validates and enqueues, then answers `202 {jobId, state:"running"}`; the UI
polls `GET /api/apps/jobs/{id}`. A job reports `kind` (`install`/`upgrade`/
`uninstall`), `app`, `state` (`running`/`succeeded`/`failed`), `stage`
(`preparing`/`installing`/`finalizing`) and `message`, and a failed job carries
`error`. A second job for an app that already has one running is `409`. Jobs
are in-memory (a restart abandons a running job; the app record and
`reconcileApps` are the durable outcome). `/api/apps/jobs`,
`/api/apps/updates` and `/api/apps/usage` are matched before
`/api/apps/{name}`, so `jobs`/`updates`/`usage` are never parsed as app names.

### Chart repositories (sources)

| Method | Path | Description |
| --- | --- | --- |
| GET | `/api/sources` | List configured sources |
| POST | `/api/sources` | Add a source `{name, url, auth, credentialsSecret?, channels?, commit?}`; `commit` is a full 40-char SHA and pins the clone |
| GET | `/api/sources/{name}` | Read one source |
| DELETE | `/api/sources/{name}` | Remove a source and its cached clones |
| POST | `/api/sources/refresh` | Refresh all sources, or one with `?name=` |

### Domains & certificates

| Method | Path | Description |
| --- | --- | --- |
| GET | `/api/domains` | List domains + cert-manager availability, plus `baseDomain`, `selectableDomains` and `ssoDomains`. Each domain carries `appRecordsDdnsEntry` and `appRecordsSupported` (provider is `ovh`/`cloudflare`) |
| POST | `/api/domains` | Add a domain `{baseDomain, dnsProvider, acmeEmail, environment, credentialsSecret?, fields?, appRecordsDdnsEntry?}` (`fields` = provider fields; secret fields → a Secret). An explicit `credentialsSecret` must be a DNS-1123 name starting with `naslos-` (SEC-2026-D12). `appRecordsDdnsEntry` opts the domain into app CNAMEs; it must name a non-wildcard DDNS entry and the provider must be `ovh` or `cloudflare` (400 otherwise) |
| GET | `/api/domains/{domain}` | Read one domain |
| PUT | `/api/domains/{domain}` | Update a domain (re-renders its Issuer/Certificate) |
| DELETE | `/api/domains/{domain}` | Remove a domain and its CRs |
| GET | `/api/domains/{domain}/certificate` | Certificate readiness/conditions |
| POST | `/api/domains/{domain}/sso` | Promote/demote a domain in the runtime SSO list: `{enabled}`. The primary is 400; demoting while an app requires auth on it, or a chart `SSO_DOMAINS` entry, is 409 |

> Start/stop is not implemented; `POST /api/apps/{name}/start|stop` has no route.

### DNS providers & Dynamic DNS

> Providers are declarative YAML, shared by domains and DDNS
> (see [dynamic-dns.md](dynamic-dns.md)). Credential values are never returned.

| Method | Path | Description |
| --- | --- | --- |
| GET | `/api/providers` | Provider definitions + `errors` from override files that failed to load. Each provider carries its `apiRights` (informational DNS-01 permissions) when defined |
| GET | `/api/ddns` | Entries + `enabled` + `intervalSeconds` |
| POST | `/api/ddns` | Create `{provider, zone, record, recordType, ttl, enabled, fields}` (secret fields → a Secret) |
| GET | `/api/ddns/{id}` | Entry detail (Secret name + which fields are set, never values) |
| PUT | `/api/ddns/{id}` | Update; an omitted secret field keeps the stored value |
| DELETE | `/api/ddns/{id}` | Remove the entry and its credential Secret. **409** while a domain uses it as its `appRecordsDdnsEntry` |
| POST | `/api/ddns/{id}/run` | Force one reconcile even when the IP is unchanged |


### Disks, volumes & ZFS

| Method | Path | Description |
| --- | --- | --- |
| GET | `/api/disks` | Discovered disks from Talos (`talosctl get discoveredvolumes`) |
| GET | `/api/disks/health` | Per-disk SMART + ZFS health: Talos inventory merged with the agent's `smartctl` and `zpool status` data. `smartctlAvailable:false` is a normal 200 (virtio/USB/RAID); see FR-DHK |
| POST | `/api/disks/recommend` | `{disks:[...]}` → `VolumeAdvisor` topology recommendation |
| GET | `/api/volumes/zfs` | List ZFS pools via `naslos-agent` (`agent.ListPools`) |
| POST | `/api/volumes/zfs` | Create pool `{name, topology, disks, options, wipeConfirmed}` via `naslos-agent` (validated: ZFS-safe name, non-empty disks, known topology; a disk with an existing filesystem needs `wipeConfirmed`, a `zfs_member` disk is refused) |
| GET | `/api/volumes/zfs/{name}` | Structured health data (device tree, IO stats, scan state, errors) |
| DELETE | `/api/volumes/zfs/{name}?confirm=<name>` | Destroy a pool via `naslos-agent`. `confirm` must repeat the pool name; `force=true` adds `zpool destroy -f` (absent by default, so a pool with mounted datasets is refused rather than force-destroyed) |
| GET | `/api/volumes/zfs/{name}/health` | Structured health data (alias) |
| GET | `/api/volumes/zfs/import` | List pools available for import (on disk but not imported) |
| POST | `/api/volumes/zfs/import` | Import: `{"name":"tank"}` for one pool, `{"all":true}` for every importable pool. A missing/malformed body is a 400, never an implicit import-all |
| POST | `/api/volumes/zfs/{name}/devices` | Attach disks: `{disks, topology, force, wipeConfirmed}` → `zpool add`. Refuses a disk already in any pool (even with `force`), a disk holding a ZFS label, a non-`/dev` path, an unknown disk, and too few disks for the topology (FR-STO-08). A disk with another filesystem signature needs `wipeConfirmed`. |
| POST | `/api/volumes/zfs/{name}/scrub` | Start a scrub now (`202`). `409` while `zpool status` reports a scrub or resilver in progress (FR-STO-11) |
| GET/POST/DELETE | `/api/storage/schedules` | Periodic snapshot and scrub schedules (`{id, kind, target, cadence, runAt, keep, recursive, enabled}`; `?id=` deletes). Snapshot runs create `naslos-auto-<schedule>-<UTC>` and prune only their own names; scrub runs are daily/weekly and report the scan result (FR-STO-10/11) |
| GET | `/api/datasets` | Every dataset with used/avail/refer/mountpoint plus `encryption` (`off` or the cipher), `keystatus` (`none`, `unavailable`/locked, `available`/unlocked) and `encryptionRoot` (FR-ENC-04) |
| GET | `/api/datasets/registry` | List operator-registered datasets apps may mount (`{name, hostPath, shared, warningAcknowledged, createdAt}`) |
| POST | `/api/datasets/registry` | Register a dataset `{name, hostPath, shared, warningAcknowledged?}`. The path must be a mounted ZFS dataset below `/var/mnt`, must not overlap the Buddy store, and a `shared` dataset requires `warningAcknowledged:true` (400 otherwise) |
| DELETE | `/api/datasets/registry/{name}` | Unregister a dataset; existing app claims are untouched |
| POST | `/api/datasets` | Create: `{pool, name, options}` → `zfs create -p` (validated name + safe option subset, FR-STO-07). With `encryption` + `keyformat=raw` the API generates a 32-byte key into a Secret in the `naslos-keys` namespace, then sends it to the agent over the authenticated channel; the response is `201 {status, name, encryption, keystatus:"available"}`. `keyformat=passphrase` and a request-supplied `keylocation` are refused until FR-ENC-05 ships. A retry whose key Secret already exists is `409` rather than overwriting it (FR-ENC-03/06; see [dataset-encryption.md](dataset-encryption.md)) |
| DELETE | `/api/datasets?name=<pool>/<name>&recursive=true&confirm=<name>` | Destroy a dataset. Empty → plain destroy; non-empty needs `recursive=true`; a pool root dataset is refused; a dataset backing a share's path is refused with 409. An encrypted dataset (or a subtree containing one) additionally requires `confirm=<name>`; after a successful destroy its key Secret(s) are deleted (crypto-shredding), and `keysDeleted` reports whether cleanup succeeded. A failed destroy keeps every key (FR-STO-09, FR-ENC-09) |
| GET | `/api/datasets/keystatus?name=<pool>/<name>` | Authoritative `zfs get` encryption state: `{name, encryption, keystatus, encryptionRoot, mounted}` (FR-ENC-04) |
| POST | `/api/datasets/unlock` | `{name}` → load the dataset's key from its Secret. Admin-only, idempotent when already unlocked; a missing key Secret is `409` (restore it before unlocking). No key bytes are accepted or returned (FR-ENC-03/04) |
| POST | `/api/datasets/lock` | `{name}` → unload the key and remove the transient keyfile. Refused while the dataset or a descendant is mounted; idempotent when already locked (FR-ENC-04) |
| GET | `/api/zvols?pool=<pool>` | List ZFS zvols (`volsize`, `volsizeBytes`, `volblocksize`, compression, used/avail/refer); omit `pool` for every pool |
| POST | `/api/zvols` | Create `{pool, name, size, options}` → `zfs create -V`. Validated name and size; only `volblocksize` (512–16M, power of two) and `compression` are accepted (FR-ZVOL-01/02) |
| PUT | `/api/zvols?name=<pool>/<zvol>` | Resize `{size}` → `zfs set volsize`; growth only, refused while the zvol is in use (FR-ZVOL-03) |
| DELETE | `/api/zvols?name=<pool>/<zvol>&confirm=<pool>/<zvol>` | Destroy a zvol. `confirm` must repeat the full name; a path that is not a `type=volume` and an in-use zvol are refused (FR-ZVOL-03) |

The actual ZFS work is delegated to `naslos-agent` on each node:
`chroot /host zpool …` / `chroot /host zfs …`. The API reaches the agent
through the headless `naslos-agent` Service in `naslos-privileged` (`:9090`, one
endpoint per node; see `charts/naslos/templates/agent-daemonset.yaml`),
overridable via the `AGENT_BASE_URL` env var.

Error contract for the ZFS endpoints:

| Situation | Status | Example |
| --- | --- | --- |
| Bad request (name/topology/disks validation) | `400` | `{"error":"invalid pool name ..."}` |
| Node has no ZFS (agent degraded mode) | `503` (forwarded from agent) | `{"error":"ZFS is not available on this node ..."}` |
| Agent unreachable | `502` | `{"error":"contacting agent at ...: ..."}` |
| Agent rejected the rendered share/user config | `502` | the change is reported as failed; a share create/update/delete is rolled back so the stored list matches what the node serves (AUD-2611-T10-1/T10-2) |

The actual ZFS work is delegated to `naslos-agent` on each node:
`chroot /host zpool …` / `chroot /host zfs …`.

### Local replication (FR-REP)

Local replication copies one ZFS dataset to another dataset on the same node.
It is asynchronous: the request validates and enqueues, then the transfer runs
as a server-owned task with the agent's `zfs send` stream piped straight into
`zfs receive` (never buffered in the API). Jobs are in memory; the received
snapshots are the durable outcome.

| Method | Path | Description |
| --- | --- | --- |
| POST | `/api/replication/send` | `{source, destination, snapshot?, recursive?, pruneKeep?}` → `202 {jobId, status:"started"}` |
| GET | `/api/replication/jobs` | `{jobs:[…]}` running plus the last ~20 finished |
| GET | `/api/replication/jobs/{id}` | One job: state, progress, result, error (`404` unknown) |
| DELETE | `/api/replication/jobs/{id}` | Cancel a running job (`200 {"status":"cancelling"}`; `409` if already finished) |

Rules:

- `source` and `destination` must be existing datasets (`<pool>/<name>`); an
  exact match against the agent's dataset list is required, and the destination
  is never created implicitly. Identical paths, or either path nested inside the
  other, are refused with `400`.
- Without `snapshot`, the job creates
  `naslos-rep-<pair token>-<UTC timestamp>-<random>` on the source (recursive
  when `recursive` is true). A supplied snapshot must exist on the source.
- Incremental sends are chosen by the newest **snapshot GUID** shared with the
  destination (a differently named copy still counts); without one the job
  sends a full stream. `recursive` maps to `zfs send -r` (descendants, without propagating properties such as the source mountpoint).
- A full stream replaces the destination dataset's contents (treat the
  destination as a replica target); it is refused when the destination holds a
  snapshot that does not carry this pair's `naslos-rep-…` prefix or a child
  dataset. Incremental receives never use `-F`, so replication cannot delete a
  user snapshot. Encrypted (`-w`) replication is not implemented yet.
- A recursive job still verifies every descendant when the root target already
  exists. Each missing child subtree is sent as its own full stream rather than
  rolling the destination back to an older base; a child carrying a same-named
  snapshot with a different GUID is refused.
- `pruneKeep` (0 = keep all, max 1000) keeps the newest N destination snapshots
  carrying the pair's prefix. Source snapshots created by replication are left
  in place.
- A second job writing the same destination (or a nested one) is `409` while it
  runs.

Job shape: `{id, source, destination, snapshot?, recursive, pruneKeep?, state:
"running"|"succeeded"|"failed"|"cancelled", startedAt, finishedAt?, progress:
{bytes, estimatedBytes}, incremental, estimatedBytes?, result?: {status:
"replicated"|"up-to-date", baseSnapshot?, baseGUID?, toGUID?, bytes,
estimatedBytes, prunedSnapshots, retentionError?, durationSeconds}, error?}`.

The agent's streaming routes gained one optional argument each: `zfs send`
accepts `recursive=true` (`-r`), and `/api/v1/zfs/snapshots/{dataset}` accepts
`recursive=false` to list only the dataset's own snapshots (the default stays
recursive for Buddy).

### Shares

| Method | Path | Description |
| --- | --- | --- |
| GET | `/api/shares` | List share definitions |
| POST | `/api/shares` | Create `{name, path, protocol(smb|nfs), …}` (AFP is rejected) |
| GET | `/api/shares/{name}` | Share detail |
| PUT | `/api/shares/{name}` | Update share fields |
| DELETE | `/api/shares/{name}` | Remove share |
| GET | `/api/shares/config/samba` | Generated `smb.conf` (text/plain) |
| GET | `/api/shares/config/nfs` | Generated NFS-Ganesha config (text/plain), not `/etc/exports` |

Share create/update accept the SMB-only booleans `shadowCopies` and `audit`
(both default false; either on an `nfs` share is a 400). The rendered
`/api/shares/config/samba` shows the resulting `vfs objects`, `shadow:*` and
`full_audit:*` directives. See [shares.md](shares.md#shadow-copies-previous-versions)
and [nfsv3.md](nfsv3.md) for NFSv3 (not served).

### Buddy Backup (see also [buddy-backup.md](buddy-backup.md))

| Method | Path | Description |
| --- | --- | --- |
| GET | `/api/buddy/status` | Owner view: free space, peers, stored backups (503 when not configured) |
| GET, POST, DELETE | `/api/buddy/peers` | List / authorize / revoke peer keys (authenticated session required) |
| GET, POST | `/api/buddy/identity` | This instance's public key / create it (`replace` to overwrite) |
| POST | `/api/buddy/send` | Back a dataset up: `202 {"jobId","status":"started"}` immediately; the send runs under a server-owned context |
| GET | `/api/buddy/jobs` | List send jobs (running + recent finished) |
| GET, DELETE | `/api/buddy/jobs/{id}` | Job detail incl. progress / cancel (resume state is kept) |
| GET, POST, DELETE | `/api/buddy/schedules` | Scheduled backups (`hourly\|daily\|weekly` + run-at UTC, `pruneKeep`); delete takes `?id=` |
| POST | `/api/buddy/restore` | Restore into a dataset, or `{"verify":true}` to hash without touching ZFS |

### Notifications & metrics

| Method | Path | Description |
| --- | --- | --- |
| GET | `/api/notifications` | ntfy settings |
| PUT | `/api/notifications` | Update ntfy settings |
| POST | `/api/notifications/test` | Send a test notification (target must resolve to a public address; the host is resolved once and pinned at send time) |
| GET/PUT/DELETE | `/api/notifications/webhook` | Generic webhook channel settings / update / reset (`400` for an invalid or non-public URL) |
| POST | `/api/notifications/webhook/test` | Send a test webhook (`400` disabled, `502` delivery failure; the response never carries the token) |
| GET | `/api/metrics` | Full `SystemMetrics` object |
| GET | `/api/dashboard` | Dashboard-formatted subset (home screen) |

`SystemMetrics.cpu` carries `usagePercent`, `cores`, `loadAvg1/5/15`, `model`,
`temperatureC` (nullable) and `perCore[]` (`core`, `usagePercent`, optional
`frequencyMhz`). `/api/dashboard` exposes the same CPU fields as
`cpu.{usage,cores,model,temperatureC,loadAvg1,loadAvg5,loadAvg15,perCore}`.
Temperature is `null` when the node exposes no CPU sensor; the rest of the
metrics are unaffected.

### Host network settings (`FR-NET`)

| Method | Path | Description |
| --- | --- | --- |
| GET | `/api/network` | Inventory: hostname + source, resolver mode (`inherit`/explicit) and host-DNS flags, the primary link, and every physical link with live addresses, routes, effective config, speed/duplex and ring/WOL features |
| POST | `/api/network/changes` | Start one guarded change: `{link, addressing:"dhcp"/"static", addresses[], gateway?, mtu?, confirmLowMtu?, nameservers:{mode,servers}, searchDomains:{mode,domains}}` -> `202 {id, phase}`. Omitted fields are unchanged |
| GET | `/api/network/changes/{id}` | `{id, phase, link, target, deadline, error}`; phases `applying -> verifying -> awaiting -> applied / reverted / failed` |
| POST | `/api/network/changes/{id}/revert` | Apply the saved previous full machine config now -> `202` |

Validation is server-side and errors use `{"error": "..."}`: `400` invalid
input, `409` while a change is pending (or a system upgrade operation is
running), `404` unknown change, `503` when the API has no Talos client, and
`502` when the machine config cannot be parsed. A patch the node
rejects does not fail the POST: the change ends in `failed` and the detail
route reports the node's error. A change applies a certSANs union patch, then
`talosctl patch machineconfig --mode=try`, verifies the target address inside
the 60-second guard window, confirms with a different certSANs update, and
keeps the record `awaiting` for five minutes. Reverting re-applies the saved
full config, not a reverse patch. When the target address changes, the browser
and the operator's DNS or `/etc/hosts` entry must move to it; see
[deployment.md](deployment.md#day-2-network-changes).

`GET /api/network` shape (abridged):

```json
{
  "hostname": { "value": "<hostname>", "source": "auto-stable" },
  "resolvers": { "inherit": true, "nameservers": [], "searchDomains": [],
                 "searchInherit": true,
                 "hostDns": { "enabled": true, "forwardKubeDnsToHost": false } },
  "primaryLink": "<ifname>",
  "links": [{ "name": "<ifname>", "kind": "ether", "up": true, "mac": "<mac>",
              "mtu": 1500, "speedMbps": 2500, "duplex": "full",
              "addresses": [{ "address": "<cidr>", "source": "static" }],
              "gateway": "<ip>",
              "config": { "addressing": "static", "addresses": ["<cidr>"],
                          "routes": [{ "destination": null, "gateway": "<ip>" }],
                          "mtu": 1500 },
              "features": { "wol": [], "ringRx": 256, "ringTx": 256 } }],
  "appliance": { "domain": "<domain>", "lanCidr": "<cidr>" },
  "pending": null
}
```

### System upgrades (`FR-UPD`, see [installer-contract.md §7](installer-contract.md#7-web-upgrades-fr-upd))

| Route | Method | Body / result |
| --- | --- | --- |
| `/api/system/version` | GET | `{naslosVersion, chartVersion, talosVersion, kubernetesVersion, releaseTalosVersion, releaseKubernetesVersion, schematicId, profile, upgradable, updatesEnabled}` |
| `/api/system/updates` | GET | Cached check: `{enabled, upgradable, current, checkedAt, source, available, stepping:{allowed,reason}, blocked, error}` |
| `/api/system/updates/check` | POST | Run a check now; same shape as `GET /updates` |
| `/api/system/reboot` | POST | `?confirm=reboot` → `202 {status:"rebooting"}`. Plain Talos reboot (works on installer and `make install-vm` nodes). `400` without the confirmation token; `409` while a system operation, app job or Buddy job is active; `503` when the API has no Talos client. The response can be lost when the node goes down; poll `GET /system/version`. |
| `/api/system/operations` | POST | `{op:"naslos"\|"talos"\|"kubernetes", version?}` → `202 {operationId, job}`. `409` while an operation/app job/Buddy job runs; `412` for the profile, order, stepping or pool-health precondition |
| `/api/system/operations/current` | GET | `{id, op, from, to, phase, steps[], message, startedAt, finishedAt, result, attempts, log}`; `phase:"idle"` when no operation exists |

`available` is the verified candidate (`version`, `notesUrl`, `talosVersion`,
`kubernetesVersion`, `apiImage`) or `null`. The API never returns the pack
sha256 in this response; the Job receives it as the administrator-approved
value. All routes are admin-only (owner router).

### System config export (`FR-CFG`, see [spec.md §3.15](spec.md#315-system-config-exportimport-fr-cfg))

| Method | Path | Description |
| --- | --- | --- |
| GET | `/api/system/config/export` | Versioned, secret-free JSON download of the declarative records. Admin-only owner route; `Content-Disposition: attachment` and `Cache-Control: no-store` |

```json
{
  "kind": "naslos-config-export",
  "schemaVersion": 1,
  "generatedAt": "2026-11-21T22:00:00Z",
  "naslosVersion": "0.6.0",
  "baseDomain": "example.test",
  "shares": [
    { "name": "media", "path": "/var/mnt/tank/media", "protocol": "smb",
      "description": "Media", "readOnly": false, "browseable": true,
      "allowedHosts": [], "validUsers": [], "validGroups": [],
      "timeMachine": false, "noRootSquash": false, "enabled": true }
  ],
  "storageSchedules": [
    { "id": "snap-1", "kind": "snapshot", "target": "tank/media",
      "cadence": "daily", "runAt": "02:30", "keep": 7,
      "recursive": true, "enabled": true }
  ],
  "buddySchedules": [
    { "id": "buddy-1", "dataset": "tank/media", "source": "tank/media",
      "receivers": ["peer-a"], "cadence": "weekly", "runAt": "Sun 03:00",
      "pruneKeep": 5, "enabled": true }
  ],
  "domains": [
    { "baseDomain": "example.test", "dnsProvider": "cloudflare",
      "providerConfig": { "proxied": "false" }, "acmeEmail": "ops@example.test",
      "environment": "production", "primary": true, "sso": true,
      "appRecordsDdnsEntry": null }
  ],
  "ddnsEntries": [
    { "id": "ddns-1", "provider": "duckdns", "zone": "example.test",
      "record": "home", "recordType": "A", "ttl": 0, "enabled": true,
      "providerConfig": {} }
  ],
  "apps": [
    { "name": "demo", "source": "naslos", "channel": "stable",
      "chartVersion": "0.1.0", "baseDomain": "", "exposure": { "subdomain": "demo",
        "tls": true, "auth": true, "localOnly": false, "protectedPaths": [],
        "internet": true, "service": "", "port": 0, "scheme": "" },
      "datasets": ["media"], "privileged": false,
      "env": [{ "name": "API_TOKEN", "secret": true }],
      "values": { "replicas": 2 }, "redactedKeys": ["adminPassword"],
      "valuesOmitted": false }
  ],
  "datasetRegistry": [
    { "name": "media", "hostPath": "/var/mnt/tank/media", "shared": false,
      "warningAcknowledged": false }
  ]
}
```

All sections are arrays (never `null`), including when the subsystem is
unavailable. Runtime state is not exported: `lastRun`/`lastResult`/`lastError`/
`nextRun`, per-receiver results and server-owned timestamps are omitted.

The export is **not** a Secret dump and the API never reads a Kubernetes Secret
or the Buddy identity/KEK file. It excludes passwords, API/ntfy tokens, DDNS and
domain credential values **and credential Secret names**, `credentialFields`,
rendered cert-manager `solver` objects, TLS private keys, Buddy identities/peer
keys/KEKs and all Secret data. App `values` have the catalog's secret-marked
keys removed at any depth (their names are listed in `redactedKeys`); if the
catalog cannot be read, `values` is omitted and `valuesOmitted` is `true`.
Free-form value/provider keys that look like credentials (`password`, `passwd`,
`passphrase`, `secret`, `token`, `credential`, `privateKey`, `tls.key`, `apiKey`,
`accessKey`, `consumerKey`, `applicationKey`, `kek`) are removed even when a
provider schema forgot to mark them. Domain and DDNS `providerConfig` is
projected to the keys the provider schema declares non-secret; unknown keys,
free-form value templates that can embed a credential (the generic DDNS
`updateUrl`/`body`) and configs for an unknown provider are omitted.

**Import/restore is `[OPEN]`.** An importer must reject a newer `schemaVersion`,
accept only allow-listed fields (reject unknown or secret-like keys), revalidate
IDs/paths/datasets/pools/providers against the live node, never reuse an
exported ID to overwrite or delete a record, and never inject credentials. It
must run as a dry-run plan followed by explicit confirmation.

This document is distinct from the installer **recovery ZIP**
([installer-contract.md §2](installer-contract.md#2-cluster-objects-the-installer-depends-on)):
the recovery ZIP is bootstrap material (`talosconfig`, machine config, Talos
secrets, `kubeconfig`, schematic/ISO facts) used to rebuild a lost control
plane, while this export is application-config JSON with no PKI, kubeconfig or
Secret material. Neither replaces the other; back up the recovery ZIP offline
and store this export beside your data backups.

### Logs & terminal (WebSocket)

| Method | Path | Description |
| --- | --- | --- |
| GET | `/api/namespaces` | Namespace names, for the terminal's picker |
| GET | `/api/pods?namespace=naslos-privileged` | Pods with their containers, phase, readiness, and a `terminal` flag marking the shell container (the terminal UI probes the candidate namespaces) |
| GET | `/api/ws/logs?namespace=&pod=&container=&tail=` | Stream a pod's live logs |
| GET | `/api/ws/exec?namespace=&pod=&container=&shell=` | Interactive exec (xterm). Defaults: namespace = the Naslos namespace, shell = `sh`, container = the pod's only container. Rejected (403) when `Origin` is foreign |

### Request trust

Owner and user routes require Traefik's `X-Naslos-Proxy-Secret` **and** a peer
address inside `TRAEFIK_CIDR`; a request without `ConnectInfo` is refused (fail
closed). Unsafe methods additionally require a same-origin browser context
(`Sec-Fetch-Site: same-origin` or a matching `Origin`) and, when a body is
present, `Content-Type: application/json`; otherwise the API returns 403/415.
Non-browser clients that send neither header are unaffected. The one exception
is `POST /api/virt/images/upload`, which streams `application/octet-stream`
chunks; the same-origin checks still apply and octet-stream is not a
CORS-safelisted content type, so cross-origin posts still force a preflight.

`/api/ws/logs` reads the pod log stream (tail 200 lines, follow); the Installed
drawer uses it with a pod from `/api/apps/{name}/pods`. The API pod needs the
`pods/log` subresource (`get`) in the app namespace, granted by the per-namespace
isolation ClusterRole.
`/api/ws/exec` uses the Kubernetes `remotecommand` SPDY executor over a websocket.

Both WebSocket endpoints refuse an upgrade whose `Origin` does not match the
request host (or a configured domain); a non-browser client that sends no
`Origin` is allowed. This restores the Go `CheckOrigin` (NAS-009).

**Frame protocol** (the two are split by frame type so a resize can never be
typed into the shell as garbage):

| Frame | Meaning |
| --- | --- |
| binary | raw keystrokes → stdin |
| text | JSON control message: `{"type":"resize","cols":N,"rows":N}` |

**Preflight.** A plain `GET /api/ws/exec` (no `Upgrade` header) validates the
target and answers with the resolved `namespace`/`pod`/`container`/`shell`, or
the reason it cannot attach (404 unknown pod, 400 unknown shell or ambiguous
container, 409 pod not running). The terminal calls this before opening the
socket, because a browser cannot read the HTTP status of a failed websocket
handshake. `shell` is limited to `bash`, `sh`, `ash`, `zsh` - the endpoint runs a
shell, never an arbitrary command.

### Virtual machines (`FR-VMT`, see [virtual-machines.md](virtual-machines.md))

| Method | Path | Description |
| --- | --- | --- |
| GET | `/api/virt/capabilities` | Node preflight: KVM, CPU model, nested, physical NICs, and whether the supervisor image ships the OVMF Secure Boot firmware pair (`secureBoot`). 503 when the supervisor is not deployed/reachable |
| GET | `/api/virt/vms` | VM records merged with live state (`running`/`stopped`/`unknown`; live `pid`, `cpuPercent`, `memUsedMiB`, `unexpectedExit`, `lastError`) |
| POST | `/api/virt/vms` | Create: validates (`secureBoot` is UEFI-only and defaults off), creates `<pool>/vms/<name>` (quota = disk size) + the pool's `vms/isos` library via the agent, stores the record, pushes the desired config; rolls the record back if the push fails |
| GET/PUT/DELETE | `/api/virt/vms/{name}` | Detail (merged view) / update (name immutable; the supervisor recreates the QEMU process when the rendered args change, including `secureBoot`) / delete (409 while running; destroys the dataset, restoring the record if the destroy fails) |
| POST | `/api/virt/vms/{name}/start` `stop` `force-stop` `restart` | Lifecycle. `stop` = graceful ACPI shutdown (vmd escalates to SIGTERM/SIGKILL); `restart` = force-stop + start |
| GET/POST | `/api/virt/vms/{name}/snapshots` | List / create a ZFS snapshot of the VM dataset (validated name) |
| POST | `/api/virt/vms/{name}/snapshots/{snap}/restore` | `zfs rollback -r`; 409 while running (newer snapshots are deleted, stated in the response) |
| GET | `/api/virt/images?pool=` | Per-pool library listing plus in-flight imports and their last errors |
| POST | `/api/virt/images/upload?pool=&name=&offset=&final=` | One upload chunk, `application/octet-stream` (the API's only non-JSON body, scoped to this route), streamed through to the supervisor |
| POST | `/api/virt/images/import` | Server-side URL import → `202`; the SSRF guard lives in the supervisor; errors surface in the listing |
| DELETE | `/api/virt/images/{name}?pool=` | Delete a library file |
| GET | `/api/ws/vm/{name}/vnc` | Console: plain GET = preflight (`vncPort`, per-boot `vncPassword`, `ready to attach`; 409 while stopped); websocket upgrade = RFB bridge to QEMU's VNC (same-origin check, session caps like `/api/ws/exec`) |

### Static assets

| Method | Path | Description |
| --- | --- | --- |
| GET | `/` | The `naslos-ui` deployment serves the SPA; `naslos-api` serves no static files |

### MCP

The MCP endpoint is a separate surface (not under `/api`): Streamable HTTP at
`/mcp`, guarded by scoped bearer tokens instead of Authelia. Its settings
routes are ordinary owner routes:

| Method | Path | Description |
| --- | --- | --- |
| GET | `/api/mcp` | Endpoint state: `enabled`, `endpointUrl`, `tokens` (no hashes), `recentCalls` (§3.14) |
| PUT | `/api/mcp` | `{enabled}` — toggle the endpoint |
| POST | `/api/mcp/tokens` | `{name, scope, expiresInDays?}` — create a token; the plaintext is in this response only |
| DELETE | `/api/mcp/tokens/{id}` | Revoke a token |

See [mcp.md](mcp.md) for the client-facing guide (scopes, the `confirm`
rule, client configuration).

## naslos-agent routes (host-level)

| Method | Path | Description |
| --- | --- | --- |
| GET | `/health` | Agent health |
| GET | `/api/v1/pools` | `zpool list` |
| POST | `/api/v1/pools` | Create pool `{name, topology, disks, options}` |
| GET | `/api/v1/pools/{name}` | Structured health data (device tree, IO stats, scan, errors) |
| GET | `/api/v1/disks/health` | Per-whole-disk SMART (read-only, `-n standby`) + ZFS vdev state/errors. Requires the agent bearer token; SMART unavailable is a 200 with `smartctlAvailable:false` |
| DELETE | `/api/v1/pools/{name}` | Destroy pool |
| GET | `/api/v1/pools/import` | List pools available for import (`zpool import` dry-run) |
| POST | `/api/v1/pools/import` | Import pool(s): `{"name":"tank"}` or `{}` for all |
| GET/POST | `/api/v1/datasets/{pool}` | List / create dataset. A create may carry `key` (32 raw bytes in the JSON body) with `encryption`+`keyformat=raw`; the agent writes a 0600 tmpfs keyfile, creates and verifies the exact dataset, and removes the file. The key is never logged, echoed or put in argv |
| GET/POST | `/api/v1/encryption/status/{dataset}`, `/api/v1/encryption/unlock`, `/api/v1/encryption/lock` | Read `encryption`/`keystatus`/`encryptionroot`/`mounted`; load a key (`{dataset, key}`); unload it (`{dataset}`). Refuses to lock a mounted dataset or descendant |
| GET/POST/PUT/DELETE | `/api/v1/zvols` | List (`?pool=`), create `{name, size, options}`, resize `{size}` (`?name=`) or destroy (`?name=&confirm=`) a zvol. Property surface is `volblocksize` + `compression` only (FR-ZVOL) |
| GET/POST | `/api/v1/snapshots/{dataset}` | List / create snapshot |
| GET/POST/DELETE | `/api/v1/snapshots/{dataset}` | List (`?paths=true` returns full `dataset@snap` paths), create (`{name, recursive}`), destroy one (`?name=`) |
| POST | `/api/v1/pools/{pool}/scrub` | Start `zpool scrub <pool>` (pool name validated; no other arguments) |
| GET | `/api/v1/zfs/send/{dataset}?to=&from=&raw=&estimate=` | Stream a `zfs send` (or report its size with `estimate=true`) |
| POST | `/api/v1/zfs/receive/{dataset}?force=` | Stream a restore into `zfs receive` |
| GET | `/api/v1/zfs/snapshots/{dataset}` | Snapshots as `{dataset, name, guid, created}` (the owning dataset matters for recursive listings; the GUID is how an incremental base is found) |

## Environment variables (`naslos-api`)

| Variable | Default | Description |
| --- | --- | --- |
| `LDAP_HOST` | `naslos-openldap` | LDAP server hostname |
| `LDAP_PORT` | `636` | LDAPS port |
| `LDAP_BASE_DN` | `dc=naslos,dc=local` | Base DN |
| `LDAP_BIND_DN` | `cn=naslos-service,ou=services,dc=naslos,dc=local` | Service bind DN |
| `LDAP_BIND_PASS_FILE` | — | File holding the service-account password (mounted from `naslos-openldap`/`service-password`). **Required** when identity is enabled: there is no inline `LDAP_BIND_PASS` fallback (it was inherited by forked `helm`/`kubectl`/`talosctl`/`git`). `LDAP_CA_CERT` is required when `LDAP_USE_TLS=true` |
| `LDAP_USE_TLS` | `true` | Use LDAPS |
| `LDAP_CA_CERT` | — | Path to CA cert for LDAPS verification |
| `EXTERNAL_DIRECTORY_ENABLED` | `false` | Validate an optional external LDAP source (FR-EXT phase 1). Disabled by default; it does not participate in authentication yet |
| `EXTERNAL_DIRECTORY_PROVIDER` | `ldap` | Provider type; `ad` is rejected until phase 2 |
| `EXTERNAL_DIRECTORY_HOST` | — | External LDAP hostname/IPv4 (no scheme/port/path) |
| `EXTERNAL_DIRECTORY_PORT` | `636` (TLS) / `389` | External LDAP port |
| `EXTERNAL_DIRECTORY_BASE_DN` | — | External base DN |
| `EXTERNAL_DIRECTORY_BIND_DN` | — | External service bind DN |
| `EXTERNAL_DIRECTORY_BIND_PASS_FILE` | — | File holding the external bind password; the chart mounts a Secret. There is no inline fallback |
| `EXTERNAL_DIRECTORY_USER_SEARCH_BASE` | base DN | User search base (must be inside the base DN) |
| `EXTERNAL_DIRECTORY_GROUP_SEARCH_BASE` | base DN | Group search base (must be inside the base DN) |
| `EXTERNAL_DIRECTORY_UID_ATTRIBUTE` | `uid` | Login-name attribute |
| `EXTERNAL_DIRECTORY_UID_NUMBER_ATTRIBUTE` | `uidNumber` | POSIX uid attribute |
| `EXTERNAL_DIRECTORY_GID_NUMBER_ATTRIBUTE` | `gidNumber` | POSIX gid attribute |
| `EXTERNAL_DIRECTORY_GROUP_MEMBER_ATTRIBUTE` | `member` | Group-member attribute |
| `EXTERNAL_DIRECTORY_USE_TLS` | `true` | LDAPS for the external source; plaintext is refused off loopback |
| `EXTERNAL_DIRECTORY_CA_CERT` | — | CA bundle path; required when external TLS is on |
| `TRAEFIK_CIDR` | `10.244.0.0/16` | Comma-separated CIDRs trusted for auth headers (the cluster pod CIDR Traefik sources from); the `X-Naslos-Proxy-Secret` is the actual proof of the proxy hop |
| `NASLOS_NAMESPACE` | — | Namespace injected by the Helm chart |
| `KUBECONFIG` | — | Path to kubeconfig (default: in-cluster) |
| `BUDDY_NAME` | `naslos` | Name this instance reports to backup peers |
| `BUDDY_RECEIVE_PATH` | unset | Dataset (mounted read-write) that stores received sealed chunks. **Unset disables the receiver** (the chart sets it when `buddy.enabled=true`). The VM profile sets `receiveHostPath: /var/mnt/test/naslos-buddy`, so chunks land on their own dataset (AUDIT-H2 / SEC-2026-C10) |
| `BUDDY_PEERS` | `/var/lib/naslos/buddy-peers.json` | Authorized-keys registry for backup peers |
| `BUDDY_ENROLL_TOKEN` | — | One-time token that lets a peer authorize its own key (from the `naslos-buddy` Secret) |
| `PROXY_SHARED_SECRET` | — | **Required.** Shared secret Traefik's `proxy-identity` middleware injects; the API refuses to start without it (from the `naslos-proxy` Secret, key `secret`). There is no opt-out |
| `AGENT_TOKEN` | — | **Required.** Bearer token sent to the agent on every request but `/health` (from the `naslos-agent` Secret, key `token`) |
| `BUDDY_IDENTITY` | `/var/lib/naslos/buddy-identity.json` | This instance's key material (private key + KEK); created on demand |
| `BUDDY_SCHEDULES` | `/var/lib/naslos/buddy-schedules.json` | Scheduled backups (interval cadence, catch-up on startup) |
| `APPS_NAMESPACE` | `naslos-apps` | Namespace user-installed apps run in |
| `DATASET_KEYS_NAMESPACE` | `naslos-keys` | Namespace for per-dataset encryption key Secrets (FR-ENC-03); the chart grants the API Secret CRUD only there |
| `APPS_PRIVILEGED_NAMESPACE` | `naslos-apps-priv` | Namespace for apps that declare `privileged: true` |
| `APPS_CONFIG` | `/var/lib/naslos/apps.json` | Installed-app records |
| `SOURCES_CONFIG` | `/var/lib/naslos/sources.json` | Configured chart repositories |
| `DOMAINS_CONFIG` | `/var/lib/naslos/domains.json` | Base domains |
| `DDNS_ENABLED` | `true` | Wire the DDNS reconcile loop |
| `DDNS_CONFIG` | `/var/lib/naslos/ddns.json` | Dynamic-DNS entry store |
| `NETWORK_STATE` | `/var/lib/naslos/network.json` | In-flight host-network change record and its previous-config snapshot (`FR-NET`) |
| `ALERT_WEBHOOK_CONFIG` | `/var/lib/naslos/webhook.json` | Generic webhook alert channel settings (`FR-ALT`, mode `0600`; may hold a bearer token) |
| `STORAGE_SCHEDULES` | `/var/lib/naslos/storage-schedules.json` | Snapshot and scrub schedules plus their last/next-run state (`FR-STO-10/11`) |
| `DDNS_PROVIDERS_DIR` | — | Override directory for provider `*.yaml` files |
| `DDNS_IP_SOURCE` / `DDNS_IPV6_SOURCE` | `https://api.ipify.org` / `https://api6.ipify.org` | Public-IP detection URLs |
| `DDNS_IP_SOURCES` / `DDNS_IPV6_SOURCES` | — | Comma-separated source lists (HTTP URLs or `dns:opendns`/`dns:google`) |
| `DDNS_INTERVAL_SECONDS` | `300` | How often the reconciler checks the public IP |
| `DDNS_UPDATE_COOLDOWN_SECONDS` | `300` | Minimum time between successful updates of one record |
| `CHARTS_CACHE_DIR` | `/var/lib/naslos/charts` | Git clone cache (one tree per source/channel) |
| `CHARTS_TTL` | `15m` | Cache freshness before a refresh |
| `IMAGE_CHECK_TTL` | `1h` | Cache lifetime for a registry tag→digest lookup (image update detection) |
| `SOURCES_OFFICIAL_URL` | — | Official chart repository to seed (empty disables) |
| `SOURCES_OFFICIAL_NAME` / `_DISPLAY` / `_AUTH` / `_SECRET` | `naslos` / `NaslosCharts` / `public` / — | Official source fields |
| `NASLOS_DOMAIN` | `naslos.local` | Primary domain app subdomains hang off (always an SSO domain) |
| `SSO_DOMAINS` | primary domain | Comma-separated domains Authelia protects, seeded by the chart. It is a floor: the effective list is this plus the primary plus domains promoted at runtime, and a chart entry cannot be demoted from the UI |
| `APPS_TLS_SECRET` | `naslos-apps-tls` | Fallback TLS Secret when the app's base domain has no domain record |
| `AUTHELIA_SERVICE` / `AUTHELIA_PORT` | `naslos-authelia` / `80` | Authelia forwardAuth target (FQDN, cross-namespace) |
| `AUTHELIA_SSO_CONFIGMAP` / `AUTHELIA_POD` / `AUTHELIA_NAMESPACE` | `naslos-authelia-sso` / `naslos-authelia-0` / release ns | Where the API writes the SSO fragments and the Authelia pod it deletes on a promotion (the StatefulSet recreates it) |
| `EXPOSURE_LOCAL_ONLY_CIDR` | — | LAN CIDR an `localOnly` app is restricted to |
| `PLATFORM_RELEASE` | release name | Release to exclude from the installed-app list/backfill |
| `NASLOS_RELEASE_PROFILE` / `_VERSION` / `_CHART_VERSION` / `_TALOS_VERSION` / `_SCHEMATIC_ID` / `_KUBERNETES_VERSION` | — | Pack release facts (only an installer-profile release sets them) |
| `NASLOS_API_IMAGE` | — | Running API image, used by the Talos/Kubernetes operation Jobs |
| `SYSTEM_UPDATES_ENABLED` | `true` | Run the daily release check |
| `SYSTEM_UPDATES_SOURCE` | GitHub releases API | Base URL of the release source (a mirror may serve `releases.json`) |
| `SYSTEM_UPDATES_INTERVAL_HOURS` | `24` | Check interval |
| `SYSTEM_UPDATES_CACHE` | `/var/lib/naslos/updates` | Verified pack/check cache |
| `SYSTEM_UPGRADER_SERVICE_ACCOUNT` / `_CONFIGMAP` | `naslos-upgrader` / `naslos-system-operation` | Job identity and status object |
| `SYSTEM_UPGRADER_TALOS_SECRET` / `_STATE_PVC` | `api.talosConfigSecret` / `naslos-shares-config` | Job mounts |
| `SYSTEM_UPGRADER_BACKOFF_LIMIT` / `_TTL_SECONDS` | `3` / `3600` | Job retry/retention |
| `NASLOS_WORKLOAD_NAMESPACE` | `naslos-privileged` | Where the agent/samba/nfs/terminal/vmd workloads live |
| `SMB_NETBIOS_NAME` | `shares.discovery.name` | NetBIOS name rendered into `smb.conf` (same identity as mDNS/WSD) |
| `NASLOS_LAN_CIDR` | `networkPolicy.nfsClientCIDR` | Default client restriction for a share with no `allowedHosts` |
| `SHARES_SMB_MULTICHANNEL` | `shares.smb.multichannel` (`true`) | Explicit `server multi channel support = yes|no` in `smb.conf` (FR-SHR-13) |
| `SHARES_CONFIG` | `/var/lib/naslos/shares.json` | Share definitions on the state PVC |
| `VMD_BASE_URL` | `https://naslos-vmd.<workload ns>.svc.cluster.local:9091` | The QEMU supervisor the API pushes desired state to. Absent client (no `VMD_TOKEN`) disables `/api/virt/*` with 503 |
| `VMD_TOKEN` | — | Bearer token the supervisor requires (from the `naslos-vmd` Secret, key `token`) |
| `VMD_CA_FILE` | — | The chart-generated vmd CA, pinned by the API client; **required** for an `https://` `VMD_BASE_URL` (fail closed, PF-M5) |
| `VMS_CONFIG` | `/var/lib/naslos/vms.json` | VM records on the state PVC (`api.vmsFile` value) |
| `VIRT_CONSOLE_MAX_SESSION_SECS` / `_IDLE_TIMEOUT_SECS` / `_MAX_SESSIONS` | `14400` / `600` / `4` | Console bridge session caps (`api.virtConsole` values) |

The `naslos-agent` uses `NODE_NAME` (from `spec.nodeName`) and listens on `:9090`.
The `naslos-vmd` supervisor listens on `:9091` (see
[virtual-machines.md](virtual-machines.md) for its own variables:
`VMD_TOKEN`, `VMD_TLS_CERT`/`VMD_TLS_KEY` or `VMD_ALLOW_PLAINTEXT=1`,
`VMD_VNC_FIRST_DISPLAY`/`VMD_VNC_DISPLAYS`, `VMD_RUN_DIR`, and the optional
firmware path overrides `VMD_UEFI_CODE`/`VMD_UEFI_VARS` (plain UEFI) and
`VMD_UEFI_SECBOOT_CODE`/`VMD_UEFI_SECBOOT_VARS` (Secure Boot); each pair must
be set together).

Disk-health controls:

| Variable | Default | Description |
| --- | --- | --- |
| `DISK_HEALTH_ENABLED` | `true` | Emit per-disk SMART/ZFS notifications (the page still works when false) |
| `DISK_HEALTH_TEMPERATURE_C` | `55` | Warning threshold for a disk temperature above this value |
| `DISK_HEALTH_WEAR_PERCENT` | `80` | Warning threshold for wear at/above this percentage |

Agent-side SMART controls:

| Variable | Default | Description |
| --- | --- | --- |
| `SMARTCTL_BIN` | `/app/smartctl` | Bundled static smartctl; empty disables SMART (ZFS half remains) |
| `SMARTCTL_TIMEOUT_SECONDS` | `10` | Per-disk smartctl deadline |
| `SMARTCTL_MAX_DISKS` | `64` | Cap on disks probed per scan |

## API flow notes

- The disk wizard calls `POST /api/disks/recommend`, then `POST /api/volumes/zfs`
  (the agent creates the pool; `zfs-service` re-imports on boot).
- To import an existing pool, the UI calls `GET /api/volumes/zfs/import` to discover
  pools on disk, then `POST /api/volumes/zfs/import` with `{"name":"tank"}` to import.
  Pools already imported do not appear in the list.
- The app catalog merges `DefaultValues` with request `values` (request wins)
  before `helm install`; apps install into the `naslos-apps` namespace (a chart
  that declares `privileged: true` installs into `naslos-apps-priv` instead).
  Installing is a background job (FR-APP-18) and the request answers `202`.
- Password changes hit `/api/users/{uid}/password`, which updates LDAP via
  Password Modify (RFC 3062) and returns the NT hash for the Samba sync half
  described in [identity-sso.md](identity-sso.md#shared-password-flow).