# NasOS API

Two HTTP APIs exist:

| API | Base | Runs as | Port |
| --- | --- | --- | --- |
| `nasos-api` | `/api/...` | Deployment in `nasos` namespace | 8080 (ClusterIP) |
| `nasos-agent` | `/api/v1/...` | DaemonSet on every node (hostNetwork) | 9090 |

The web UI is served as static files by `nasos-api` (`/var/nasos/ui`) and also by
the separate `nasos-ui` SvelteKit deployment; the IngressRoute routes the UI to
`nasos-ui` and lets the SPA reach the API on the same origin.

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

## nasos-api routes

### Health & auth

| Method | Path | Description |
| --- | --- | --- |
| GET | `/api/health` | Liveness probe; always `{"status":"ok"}` and bypassed by Authelia |
| GET | `/api/auth/me` | Current user from `Remote-*` headers (username, groups, email, displayName) |

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
| GET | `/api/volumes/zfs` | Stub describing pool listing “via agent” |
| POST | `/api/volumes/zfs` | Request `{name, topology, disks, options}` pool creation |

The actual ZFS work is delegated to `nasos-agent` on each node:
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
| GET | `/` | Serves `/var/nasos/ui` via `http.FileServer` as a fallback |

## nasos-agent routes (host-level)

| Method | Path | Description |
| --- | --- | --- |
| GET | `/health` | Agent health |
| GET | `/api/v1/pools` | `zpool list` |
| POST | `/api/v1/pools` | Create pool `{name, topology, disks, options}` |
| GET | `/api/v1/pools/{name}` | `zpool status` |
| DELETE | `/api/v1/pools/{name}` | Destroy pool |
| GET/POST | `/api/v1/datasets/{pool}` | List / create dataset |
| GET/POST | `/api/v1/snapshots/{dataset}` | List / create snapshot |

## Environment variables (`nasos-api`)

| Variable | Default | Description |
| --- | --- | --- |
| `LDAP_HOST` | `nasos-openldap` | LDAP server hostname |
| `LDAP_PORT` | `636` | LDAPS port |
| `LDAP_BASE_DN` | `dc=nasos,dc=local` | Base DN |
| `LDAP_BIND_DN` | `cn=nasos-service,ou=services,dc=nasos,dc=local` | Service bind DN |
| `LDAP_BIND_PASS` | — | Service account password |
| `LDAP_USE_TLS` | `true` | Use LDAPS |
| `LDAP_CA_CERT` | — | Path to CA cert for LDAPS verification |
| `TRAEFIK_CIDR` | `10.0.0.0/8` | Comma-separated CIDRs trusted for auth headers |
| `NASOS_NAMESPACE` | — | Namespace injected by the Helm chart |
| `KUBECONFIG` | — | Path to kubeconfig (default: in-cluster) |

The `nasos-agent` uses `NODE_NAME` (from `spec.nodeName`) and listens on `:9090`.

## API flow notes

- The disk wizard calls `POST /api/disks/recommend`, then `POST /api/volumes/zfs`
  (the agent creates the pool; `zfs-service` re-imports on boot).
- The app catalog merges `DefaultValues` with request `values` (request wins)
  before `helm install`; apps are installed into the `nasos` namespace.
- Password changes hit `/api/users/{uid}/password`, which updates LDAP via
  Password Modify (RFC 3062) and returns the NT hash for the Samba sync half
  described in [identity-sso.md](identity-sso.md#shared-password-flow).