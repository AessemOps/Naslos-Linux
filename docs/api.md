# Naslos API

Two HTTP APIs exist:

| API | Base | Runs as | Port |
| --- | --- | --- | --- |
| `naslos-api` | `/api/...` | Deployment in `naslos` namespace | 8080 (ClusterIP) |
| `naslos-agent` | `/api/v1/...` | DaemonSet on every node (hostNetwork) | 9090 |

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
- Authentication is **not enforced inside the API router today**: the trust
  boundary is enforced at the ingress (Authelia forwardAuth) and in the
  `api/internal/auth` middleware, which only trusts `Remote-*` headers from the
  Traefik pod CIDR (see [identity-sso.md](identity-sso.md#header-trust)).

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

| Method | Path | Description |
| --- | --- | --- |
| GET | `/api/catalog` | List catalog entries (name, displayName, description, category, icon, version, tags) |
| GET | `/api/catalog/{name}` | Full entry incl. JSON Schema + default values |
| GET | `/api/apps` | Installed Helm releases |
| POST | `/api/apps` | Install from catalog: `{name, values}` — merges catalog defaults, runs `helm install` |
| GET | `/api/apps/{name}` | Details incl. `values` and status |
| PUT | `/api/apps/{name}` | Upgrade/reconfigure: `{values}` |
| DELETE | `/api/apps/{name}` | Uninstall app |

> Start/stop is intentionally a stub today: `POST /api/apps/{name}/start|stop`
> returns a note telling operators to scale via `replicaCount` in values.

### Disks, volumes & ZFS

| Method | Path | Description |
| --- | --- | --- |
| GET | `/api/disks` | Discovered disks from Talos (`talosctl get discoveredvolumes`) |
| POST | `/api/disks/recommend` | `{disks:[...]}` → `VolumeAdvisor` topology recommendation |
| GET | `/api/volumes` | List Talos user volumes (stub: returns a status message) |
| POST | `/api/volumes` | Create ext4/xfs/btrfs `UserVolumeConfig` document (ZFS is handled by the agent) |
| GET | `/api/volumes/zfs` | List ZFS pools via `naslos-agent` (`agent.ListPools`) |
| POST | `/api/volumes/zfs` | Create pool `{name, topology, disks, options}` via `naslos-agent` (validated: ZFS-safe name, non-empty disks, known topology) |
| GET | `/api/volumes/zfs/{name}` | Structured health data (device tree, IO stats, scan state, errors) |
| DELETE | `/api/volumes/zfs/{name}` | Destroy a pool via `naslos-agent` |
| GET | `/api/volumes/zfs/{name}/health` | Structured health data (alias) |
| GET | `/api/volumes/zfs/import` | List pools available for import (on disk but not imported) |
| POST | `/api/volumes/zfs/import` | Import: `{"name":"tank"}` for one pool, `{}` for all |

The actual ZFS work is delegated to `naslos-agent` on each node:
`chroot /host zpool …` / `chroot /host zfs …`. The API reaches the agent
through the headless `naslos-agent` Service (`:9090`, one endpoint per node;
see `charts/naslos/templates/agent-daemonset.yaml`), overridable via the
`AGENT_BASE_URL` env var.

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
| POST | `/api/shares` | Create `{name, path, protocol(smb|nfs|afp), …}` |
| GET | `/api/shares/{name}` | Share detail |
| PUT | `/api/shares/{name}` | Update share fields |
| DELETE | `/api/shares/{name}` | Remove share |
| GET | `/api/shares/config/samba` | Generated `smb.conf` (text/plain) |
| GET | `/api/shares/config/nfs` | Generated `/etc/exports` (text/plain) |

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
| GET | `/api/ws/logs?namespace=&pod=&container=` | Stream a pod's live logs |
| GET | `/api/ws/exec?namespace=&pod=&container=&command=` | Interactive exec (xterm); defaults: pod required, ns `default`, container `main`, command `/bin/sh` |

`/api/ws/logs` reads the pod log stream (`tail: 100` lines, follow). `/api/ws/exec`
uses the Kubernetes `remotecommand` SPDY executor over a websocket.

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
| `TRAEFIK_CIDR` | `10.0.0.0/8` | Comma-separated CIDRs trusted for auth headers |
| `NASLOS_NAMESPACE` | — | Namespace injected by the Helm chart |
| `KUBECONFIG` | — | Path to kubeconfig (default: in-cluster) |

The `naslos-agent` uses `NODE_NAME` (from `spec.nodeName`) and listens on `:9090`.

## API flow notes

- The disk wizard calls `POST /api/disks/recommend`, then `POST /api/volumes/zfs`
  (the agent creates the pool; `zfs-service` re-imports on boot).
- To import an existing pool, the UI calls `GET /api/volumes/zfs/import` to discover
  pools on disk, then `POST /api/volumes/zfs/import` with `{"name":"tank"}` to import.
  Pools already imported do not appear in the list.
- The app catalog merges `DefaultValues` with request `values` (request wins)
  before `helm install`; apps are installed into the `naslos` namespace.
- Password changes hit `/api/users/{uid}/password`, which updates LDAP via
  Password Modify (RFC 3062) and returns the NT hash for the Samba sync half
  described in [identity-sso.md](identity-sso.md#shared-password-flow).