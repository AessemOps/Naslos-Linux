# Naslos API

Two HTTP APIs exist:

| API | Base | Runs as | Port |
| --- | --- | --- | --- |
| `naslos-api` | `/api/...` | Deployment in `naslos` namespace | 8080 (ClusterIP) |
| `naslos-agent` | `/api/v1/...` | DaemonSet on every node (hostNetwork, `naslos-privileged`) | 9090 |

The web UI is served as static files by `naslos-api` (`/var/naslos/ui`) and also by
the separate `naslos-ui` SvelteKit deployment; the IngressRoute routes the UI to
`naslos-ui` and lets the SPA reach the API on the same origin.

## Common behaviors

- All routes respond with JSON (`Content-Type: application/json`) unless noted.
- Error payload shape:
  ```json
  { "error": "message" }
  ```
- Unsupported methods return `405 Method Not Allowed`.
- Authentication **is** enforced: the API is only reachable through Traefik's
  `proxy-identity` middleware, and `api/internal/auth` independently requires the
  shared proxy secret plus a `Remote-User` header from the Traefik pod CIDR
  (see [identity-sso.md](identity-sso.md#header-trust)). The API refuses to start
  without the secret, so a request that bypasses Traefik cannot authenticate.

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
| GET | `/api/catalog` | List catalog entries (summary incl. `source`, `channel`, `channels`) |
| GET | `/api/catalog/{name}` | Full entry incl. JSON Schema, `services`, `exposure`, `chartPath` |
| GET | `/api/apps` | Installed-app records with live status and URL |
| POST | `/api/apps` | Enqueue an install job: `{name, values, exposure?, baseDomain?, confirmed:true}` (confirmed is required) → `202 {jobId, state}` |
| GET | `/api/apps/{name}` | Record + release status |
| PUT | `/api/apps/{name}` | Enqueue an upgrade job: `{values}` → `202 {jobId, state}` |
| DELETE | `/api/apps/{name}` | Enqueue an uninstall job (removes the release, its route and record) → `202 {jobId, state}` |
| GET | `/api/apps/jobs` | Lifecycle jobs: running plus the last ~20 finished |
| GET | `/api/apps/jobs/{id}` | One job's `state`, `stage`, `message` and `error` (the poll target) |
| GET | `/api/apps/{name}/exposure` | Exposure settings, the app's own `baseDomain` (fallback primary), `primaryDomain`, `selectableDomains`, `ssoDomains` and `authAllowed` |
| PUT | `/api/apps/{name}/exposure` | Update `{exposure, baseDomain?}`, re-render the route; an unconfigured `baseDomain` is rejected (400) |
| GET | `/api/apps/{name}/services` | Services the release rendered (route-target discovery/picker) |

Install/upgrade/uninstall are **asynchronous** (FR-APP-18): the handler
validates and enqueues, then answers `202 {jobId, state:"running"}`; the UI
polls `GET /api/apps/jobs/{id}`. A job reports `kind` (`install`/`upgrade`/
`uninstall`), `app`, `state` (`running`/`succeeded`/`failed`), `stage`
(`preparing`/`installing`/`finalizing`) and `message`, and a failed job carries
`error`. A second job for an app that already has one running is `409`. Jobs
are in-memory (a restart abandons a running job; the app record and
`reconcileApps` are the durable outcome). `/api/apps/jobs` is matched before
`/api/apps/{name}`, so `jobs` is never an app name.

### Chart repositories (sources)

| Method | Path | Description |
| --- | --- | --- |
| GET | `/api/sources` | List configured sources |
| POST | `/api/sources` | Add a source `{name, url, auth, credentialsSecret?, channels?}` |
| GET | `/api/sources/{name}` | Read one source |
| DELETE | `/api/sources/{name}` | Remove a source and its cached clones |
| POST | `/api/sources/refresh` | Refresh all sources, or one with `?name=` |

### Domains & certificates

| Method | Path | Description |
| --- | --- | --- |
| GET | `/api/domains` | List domains + cert-manager availability, plus `baseDomain`, `selectableDomains` and `ssoDomains` |
| POST | `/api/domains` | Add a domain `{baseDomain, dnsProvider, acmeEmail, environment, credentialsSecret?, fields?}` (`fields` = provider fields; secret fields → a Secret) |
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
| DELETE | `/api/ddns/{id}` | Remove the entry and its credential Secret |
| POST | `/api/ddns/{id}/run` | Force one reconcile even when the IP is unchanged |


### Disks, volumes & ZFS

| Method | Path | Description |
| --- | --- | --- |
| GET | `/api/disks` | Discovered disks from Talos (`talosctl get discoveredvolumes`) |
| POST | `/api/disks/recommend` | `{disks:[...]}` → `VolumeAdvisor` topology recommendation |
| GET | `/api/volumes/zfs` | List ZFS pools via `naslos-agent` (`agent.ListPools`) |
| POST | `/api/volumes/zfs` | Create pool `{name, topology, disks, options}` via `naslos-agent` (validated: ZFS-safe name, non-empty disks, known topology) |
| GET | `/api/volumes/zfs/{name}` | Structured health data (device tree, IO stats, scan state, errors) |
| DELETE | `/api/volumes/zfs/{name}` | Destroy a pool via `naslos-agent` |
| GET | `/api/volumes/zfs/{name}/health` | Structured health data (alias) |
| GET | `/api/volumes/zfs/import` | List pools available for import (on disk but not imported) |
| POST | `/api/volumes/zfs/import` | Import: `{"name":"tank"}` for one pool, `{}` for all |
| POST | `/api/volumes/zfs/{name}/devices` | Attach disks: `{disks, topology, force}` → `zpool add`. Refuses a disk already in any pool (even with `force`), a non-`/dev` path, an unknown disk, and too few disks for the topology (FR-STO-08) |
| GET | `/api/datasets` | Every dataset with used/avail/refer/mountpoint |
| POST | `/api/datasets` | Create: `{pool, name, options}` → `zfs create -p` (validated name + safe option subset, FR-STO-07) |
| DELETE | `/api/datasets?name=<pool>/<name>&recursive=true` | Destroy a dataset. Empty → plain destroy; non-empty needs `recursive=true`; a pool root dataset is refused; a dataset backing a share's path is refused with 409 (FR-STO-09) |

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

The actual ZFS work is delegated to `naslos-agent` on each node:
`chroot /host zpool …` / `chroot /host zfs …`.

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
| POST | `/api/notifications/test` | Send a test notification |
| GET | `/api/metrics` | Full `SystemMetrics` object |
| GET | `/api/dashboard` | Dashboard-formatted subset (home screen) |

### Logs & terminal (WebSocket)

| Method | Path | Description |
| --- | --- | --- |
| GET | `/api/namespaces` | Namespace names, for the terminal's picker |
| GET | `/api/pods?namespace=naslos-privileged` | Pods with their containers, phase, readiness, and a `terminal` flag marking the shell container (the terminal UI probes the candidate namespaces) |
| GET | `/api/ws/logs?namespace=&pod=&container=&tail=` | Stream a pod's live logs |
| GET | `/api/ws/exec?namespace=&pod=&container=&shell=` | Interactive exec (xterm). Defaults: namespace = the Naslos namespace, shell = `sh`, container = the pod's only container |

`/api/ws/logs` reads the pod log stream (tail 200 lines, follow).
`/api/ws/exec` uses the Kubernetes `remotecommand` SPDY executor over a websocket.

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

### Static assets

| Method | Path | Description |
| --- | --- | --- |
| GET | `/` | Serves `/var/naslos/ui` via `http.FileServer` as a fallback |

## naslos-agent routes (host-level)

| Method | Path | Description |
| --- | --- | --- |
| GET | `/health` | Agent health |
| GET | `/api/v1/pools` | `zpool list` |
| POST | `/api/v1/pools` | Create pool `{name, topology, disks, options}` |
| GET | `/api/v1/pools/{name}` | Structured health data (device tree, IO stats, scan, errors) |
| DELETE | `/api/v1/pools/{name}` | Destroy pool |
| GET | `/api/v1/pools/import` | List pools available for import (`zpool import` dry-run) |
| POST | `/api/v1/pools/import` | Import pool(s): `{"name":"tank"}` or `{}` for all |
| GET/POST | `/api/v1/datasets/{pool}` | List / create dataset |
| GET/POST | `/api/v1/snapshots/{dataset}` | List / create snapshot |
| GET | `/api/v1/zfs/send/{dataset}?to=&from=&raw=&estimate=` | Stream a `zfs send` (or report its size with `estimate=true`) |
| POST | `/api/v1/zfs/receive/{dataset}?force=` | Stream a restore into `zfs receive` |
| GET | `/api/v1/zfs/snapshots/{dataset}` | Snapshots with their GUIDs (how an incremental base is found) |

## Environment variables (`naslos-api`)

| Variable | Default | Description |
| --- | --- | --- |
| `LDAP_HOST` | `naslos-openldap` | LDAP server hostname |
| `LDAP_PORT` | `636` | LDAPS port |
| `LDAP_BASE_DN` | `dc=naslos,dc=local` | Base DN |
| `LDAP_BIND_DN` | `cn=naslos-service,ou=services,dc=naslos,dc=local` | Service bind DN |
| `LDAP_BIND_PASS` | — | Service account password |
| `LDAP_USE_TLS` | `true` | Use LDAPS |
| `LDAP_CA_CERT` | — | Path to CA cert for LDAPS verification |
| `TRAEFIK_CIDR` | `10.244.0.0/16` | Comma-separated CIDRs trusted for auth headers (the cluster pod CIDR Traefik sources from); the `X-Naslos-Proxy-Secret` is the actual proof of the proxy hop |
| `NASLOS_NAMESPACE` | — | Namespace injected by the Helm chart |
| `KUBECONFIG` | — | Path to kubeconfig (default: in-cluster) |
| `BUDDY_NAME` | `naslos` | Name this instance reports to backup peers |
| `BUDDY_RECEIVE_PATH` | `/var/lib/naslos/buddy` | Dataset (mounted read-write) that stores received sealed chunks. The VM profile sets `buddy.enabled=true` + `receiveHostPath: /var/mnt/test/naslos-buddy`, so chunks land on their own dataset (AUDIT-H2) |
| `BUDDY_PEERS` | `/var/lib/naslos/buddy-peers.json` | Authorized-keys registry for backup peers |
| `BUDDY_ENROLL_TOKEN` | — | One-time token that lets a peer authorize its own key (from the `naslos-buddy` Secret) |
| `PROXY_SHARED_SECRET` | — | **Required.** Shared secret Traefik's `proxy-identity` middleware injects; the API refuses to start without it (from the `naslos-proxy` Secret, key `secret`). There is no opt-out |
| `AGENT_TOKEN` | — | **Required.** Bearer token sent to the agent on every request but `/health` (from the `naslos-agent` Secret, key `token`) |
| `BUDDY_IDENTITY` | `/var/lib/naslos/buddy-identity.json` | This instance's key material (private key + KEK); created on demand |
| `BUDDY_SCHEDULES` | `/var/lib/naslos/buddy-schedules.json` | Scheduled backups (interval cadence, catch-up on startup) |
| `APPS_NAMESPACE` | `naslos-apps` | Namespace user-installed apps run in |
| `APPS_PRIVILEGED_NAMESPACE` | `naslos-apps-priv` | Namespace for apps that declare `privileged: true` |
| `APPS_CONFIG` | `/var/lib/naslos/apps.json` | Installed-app records |
| `SOURCES_CONFIG` | `/var/lib/naslos/sources.json` | Configured chart repositories |
| `DOMAINS_CONFIG` | `/var/lib/naslos/domains.json` | Base domains |
| `DDNS_ENABLED` | `true` | Wire the DDNS reconcile loop |
| `DDNS_CONFIG` | `/var/lib/naslos/ddns.json` | Dynamic-DNS entry store |
| `DDNS_PROVIDERS_DIR` | — | Override directory for provider `*.yaml` files |
| `DDNS_IP_SOURCE` / `DDNS_IPV6_SOURCE` | `https://api.ipify.org` / `https://api6.ipify.org` | Public-IP detection URLs |
| `DDNS_IP_SOURCES` / `DDNS_IPV6_SOURCES` | — | Comma-separated source lists (HTTP URLs or `dns:opendns`/`dns:google`) |
| `DDNS_INTERVAL_SECONDS` | `300` | How often the reconciler checks the public IP |
| `DDNS_UPDATE_COOLDOWN_SECONDS` | `300` | Minimum time between successful updates of one record |
| `CHARTS_CACHE_DIR` | `/var/lib/naslos/charts` | Git clone cache (one tree per source/channel) |
| `CHARTS_TTL` | `15m` | Cache freshness before a refresh |
| `SOURCES_OFFICIAL_URL` | — | Official chart repository to seed (empty disables) |
| `SOURCES_OFFICIAL_NAME` / `_DISPLAY` / `_AUTH` / `_SECRET` | `naslos` / `NaslosCharts` / `public` / — | Official source fields |
| `NASLOS_DOMAIN` | `naslos.local` | Primary domain app subdomains hang off (always an SSO domain) |
| `SSO_DOMAINS` | primary domain | Comma-separated domains Authelia protects, seeded by the chart. It is a floor: the effective list is this plus the primary plus domains promoted at runtime, and a chart entry cannot be demoted from the UI |
| `APPS_TLS_SECRET` | `naslos-apps-tls` | Fallback TLS Secret when the app's base domain has no domain record |
| `AUTHELIA_SERVICE` / `AUTHELIA_PORT` | `naslos-authelia` / `80` | Authelia forwardAuth target (FQDN, cross-namespace) |
| `AUTHELIA_SSO_CONFIGMAP` / `AUTHELIA_POD` / `AUTHELIA_NAMESPACE` | `naslos-authelia-sso` / `naslos-authelia-0` / release ns | Where the API writes the SSO fragments and the Authelia pod it deletes on a promotion (the StatefulSet recreates it) |
| `EXPOSURE_LOCAL_ONLY_CIDR` | — | LAN CIDR an `localOnly` app is restricted to |
| `PLATFORM_RELEASE` | release name | Release to exclude from the installed-app list/backfill |

The `naslos-agent` uses `NODE_NAME` (from `spec.nodeName`) and listens on `:9090`.

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