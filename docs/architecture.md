# Naslos Architecture

This document is the system-level view of Naslos: how the pieces fit together,
where they run, how they talk, and the important data flows. For deeper dives
see the [document index](README.md).

## Design Principles

1. **Stock Talos only** — No base modification. All Naslos functionality is
   delivered as machine-config documents, Helm charts, and containers. Talos
   upgrades stay clean via `talosctl upgrade`.

2. **ZFS as the filesystem** — Uses the official `siderolabs/zfs` extension from
   the Image Factory. Pools auto-import at boot via `zfs-service`
   (`zpool import -fal`).

3. **Privileged agent for ZFS** — Since ZFS pools live outside Talos's volume
   system (which only supports ext4/xfs/btrfs), a privileged DaemonSet executes
   `zpool`/`zfs` commands via `chroot /host`.

4. **Web terminal for shell access** — Talos has no shell by design. Naslos
   provides a zsh web terminal in a privileged container with host mounts.

5. **One password for web + shares** — A shared-password single sign-on:
   Authelia (via Traefik forwardAuth) authenticates the web UI against
   OpenLDAP, and the same password is mirrored to Samba's passdb as an NT hash
   so SMB logins work without a second password.

## Layer 1 — System Context

```
                ┌──────────────────────────────────────────────┐
                │                   Users                        │
                │  browser · mobile · CLI · SMB client · NFS    │
                │  client · Time Machine (macOS)                │
                └──────┬───────────────────────────┬────────────┘
                       │ HTTPS :443                │ SMB :445 / NFS :2049
                       ▼                           ▼
  ┌───────────────────────────────────────────┐   ┌───────────────┐
  │             TRUST ZONE (K8s cluster)      │   │ OUTSIDE zone  │
  │  ┌─────────────┐   ┌───────────────────┐  │   │ Samba / NFS   │
  │  │  Traefik    │──▶│  Authelia SSO     │  │   │ export        │
  │  │  ingress    │   │  :9091 forwardAuth│  │   │ services      │
  │  └─────┬───────┘   └──────┬────────────┘  │   └──────┬────────┘
  │        │                 │ LDAPS :636      │          │
  │        ▼                 ▼                │          │
  │  ┌─────────────┐   ┌──────────────┐       │          ▼
  │  │ Naslos UI   │   │  OpenLDAP    │       │   ┌─────────────┐
  │  │  :80        │   │  :636 store  │       │   │ ZFS pool    │
  │  │             │   │              │       │   │ /var/mnt/   │
  │  └─────┬───────┘   └──────────────┘       │   └─────────────┘
  └────────┼───────────────────────────────────┘
           │ HTTPS /api
           ▼
   ┌─────────────────────────────────────────────────────────┐
   │            naslos-api  :8080   (the brain)                │
   │  auth · catalog · helm · shares · identity · metrics ·  │
   │  talos · websocket                                       │
   └─────────────────────────────────────────────────────────┘
```

### Outbound connections of naslos-api

| Downstream | Protocol / port | Used for |
| --- | --- | --- |
| Talos API (each node) | Talos machinery API | disk discovery, `UserVolumeConfig` documents |
| Kubernetes control plane | K8s API (in-cluster) | pod info, logs, exec |
| `naslos-agent` :9090 (each node) | HTTP | `zpool`/`zfs` via `chroot /host` |
| OpenLDAP :636 | LDAPS | users, groups, password modify |
| Samba workload | agent-pushed `smbusers` mirror (+ `pdbedit -i smbpasswd:`) | NT-hash sync |
| Helm SDK | in-process | app install / upgrade / uninstall |
| Prometheus | HTTP | metrics (Grafana removed 2026-09-19) |
| ntfy (external `ntfy.sh` or self-hosted) | HTTPS | push notifications |
| Git chart hosts | HTTPS 443 / SSH 22 / git 9418 | clone configured chart repositories (`networkPolicy.gitEgress`) |
| Public-IP sources + DNS provider APIs | HTTPS 443 (HTTP 80 for `http` providers) | Dynamic DNS detection and record updates (`networkPolicy.ddnsEgress`, §3.9) |

## Layer 2 — Deployment Topology (namespaces `naslos` + `naslos-privileged`)

```
        ┌──────────────────────────────────────────────────────────┐
        │                  naslos namespace                        │
        │  (pod-security enforce: privileged)                      │
        │                                                          │
        │  ┌───────────────────┐   ┌───────────────────┐           │
        │  │ traefik           │   │ authelia          │           │
        │  │  (Helm dep)       │   │  ConfigMap:       │           │
        │  │ IngressRoutes:    │   │  authelia-config  │           │
        │  │  naslos-ui :443   │   │  (LDAP backend;   │           │
        │  │  naslos-redirect  │   │   password from   │           │
        │  │ Middlewares:      │   │   a Secret)       │           │
        │  │  forwardauth-auth │   └────────┬──────────┘           │
        │  │  authelia         │        ┌───▼───────────┐          │
        │  │  security-headers │        │  OpenLDAP    │          │
        │  └────────┬──────────┘        │  StatefulSet │          │
        │           │                   │  PVCs:       │          │
        │  ┌────────▼─────────┐         │  local-path  │          │
        │  │ naslos-ui        │         └──────────────┘          │
        │  │  Deployment      │   ┌────────────────────────┐       │
        │  │  svc ClusterIP   │   │ naslos-api Deployment  │       │
        │  │  :80             │   │  svc ClusterIP :8080   │       │
        │  └──────────────────┘   │  SA: naslos-api        │       │
        │  ┌────────────┐  ┌──────┴──────┐  ┌─────────────┐       │
        │  │ prometheus │  │ alertmgr    │  │ (no ntfy    │       │
        │  │ ret. 30d   │  │  routing    │  │  workload;  │       │
        │  └────────────┘  └─────────────┘  │  external)  │       │
        │                                    └─────────────┘       │
        └──────────────────────────┬───────────────────────────────┘
                                   │ exec RBAC + agent-token Secret
        ┌──────────────────────────▼───────────────────────────────┐
        │              naslos-privileged namespace                  │
        │  (pod-security enforce: privileged)                       │
        │  ┌────────────────────────────────────────────────────┐  │
        │  │ naslos-agent DaemonSet (per node, hostNetwork)     │  │
        │  │  privileged · hostMounts: /host /dev /run /var     │  │
        │  │  cmd: chroot /host zpool|zfs|wipefs                 │  │
        │  └────────────────────────────────────────────────────┘  │
        │  ┌────────────┐  ┌────────────┐  ┌──────────────────┐    │
        │  │ naslos-    │  │ naslos-nfs │  │ naslos-terminal  │    │
        │  │ samba      │  │ (Ganesha)  │  │  (privileged)    │    │
        │  │ hostNetwork│  │ hostNetwork│  └──────────────────┘    │
        │  └────────────┘  └────────────┘                          │
        └──────────────────────────────────────────────────────────┘
```

### Workloads

- **naslos-ui** — SvelteKit static site. In dev, Vite proxies `/api` to
  `:8080` (`ui/vite.config.ts`). In the cluster it is a plain Deployment whose
  work is done by Traefik + Authelia in front.
- **naslos-api** — stateless Deployment; holds all "brain" logic, the auth
  middleware, and serves UI static files as fallback. Talks to LDAP, Talos,
  Kubernetes, Helm (in-process) and the agent. It mounts hostPath volumes for
  the shares view and the buddy receive dataset, which is why its namespace
  stays `privileged`.
- **naslos-agent** — a **DaemonSet** (one pod per node) in `naslos-privileged`
  with `hostNetwork: true` and a `privileged` security context (no `hostPID`).
  It bind-mounts the host root at `/host` and runs every ZFS command through
  `chroot /host` — the only way to reach pools outside Talos's volume system.
- **OpenLDAP** — StatefulSet with two `local-path` PVCs (no ZFS LocalPV
  provisioner is installed): data under `/var/lib/ldap`, config under
  `/etc/ldap/slapd.d`. TLS certs come from the `naslos-openldap-tls` secret.
- **Samba / NFS** — chart DaemonSets
  (`charts/naslos/templates/samba-daemonset.yaml`, `nfs-daemonset.yaml`) in
  `naslos-privileged`; their images are built from `samba/image` and
  `nfs/image`. They consume generated configs from
  `/api/shares/config/samba` (Samba) and `/api/shares/config/nfs` (the
  NFS-Ganesha config) and export ZFS dataset paths under `/var/mnt`.
- **naslos-terminal** — a privileged Deployment in `naslos-privileged`; the UI
  discovers which namespace holds it rather than assuming one.

## Layer 3 — Component Model (naslos-api internals)

```
                    ┌──────────────────────────────────────────────┐
                    │                naslos-api :8080               │
                    │                                              │
                    │   HTTP Router (ServeMux, /api/...)           │
                    │   ┌──────────────────────────────────────┐   │
                    │   │ handlers: health · catalog · apps ·  │   │
                    │   │ disks · volumes · shares · users ·   │   │
                    │   │ groups · metrics · notifications ·   │   │
                    │   │ ws/logs · ws/exec · auth/me          │   │
                    │   └──────────────┬───────────────────────┘   │
                    │                  │                            │
                    │   ┌──────────────▼──────────────────────────┐ │
                    │   │          auth.Middleware                │ │
                    │   │  RequireAuth / RequireAdmin            │ │
                    │   │  (Remote-* from TRAEFIK_CIDR + the      │ │
                    │   │   X-Naslos-Proxy-Secret injected by      │ │
                    │   │   Traefik)                               │ │
                    │   └─────────────────────────────────────────┘ │
                    │                                              │
                    │   ┌─────────┐ ┌─────────┐ ┌──────────┐ ┌────┐ │
                    │   │ catalog │ │  helm   │ │  shares  │ │... │ │
                    │   └─────────┘ └─────────┘ └──────────┘ └────┘ │
                    └──────────────────────────────────────────────┘
```

**Component responsibilities**

| Package | Owning directory | Responsibility |
| --- | --- | --- |
| `auth` | `api/internal/auth` | Middleware: accepts `Remote-User/-Groups/-Email/-Name` only from `TRAEFIK_CIDR` **and** with the `X-Naslos-Proxy-Secret` injected by Traefik; `RequireAdmin` checks `naslos_admins` |
| `talos` | `api/internal/talos` | Talos machinery client, disk discovery, `VolumeAdvisor`, `UserVolumeConfig` documents |
| `catalog` | `api/internal/catalog` | Built-in app store + JSON Schema per app; loads external JSON catalog overrides |
| `helm` | `api/internal/helm` | Helm SDK wrapper: install/upgrade/uninstall/list/get/rollback, chart repo cache |
| `shares` | `api/internal/shares` | Share CRUD + generated `smb.conf` and NFS-Ganesha config |
| `identity` | `api/internal/identity` | OpenLDAP client: persons/groups, SetPassword; NT-hash computed for the agent-pushed `smbusers` mirror |
| `metrics` | `api/internal/metrics` | SystemMetrics snapshot + dashboard projection |
| `notifications` | `api/internal/notifications` | ntfy settings + push delivery |
| `providers` | `api/internal/providers` | Declarative DNS providers: embedded YAML + override dir, cert-manager solver rendering |
| `ddns` | `api/internal/ddns` | Dynamic-DNS entry store, public-IP detection, `ovh`/`cloudflare`/`http` drivers, credential Secrets |
| `server` | `api/internal/server` | HTTP routing, WebSocket log/exec, Samba sync glue |

## Layer 4 — Port & Trust Map

```
            ┌───────────┐   ┌──────────────┐   ┌────────────┐
 Browser───▶│ Traefik   │──▶│   Authelia   │──▶│  OpenLDAP  │
            │ :80→443   │   │   :9091      │──▶│   :636     │
            └─────┬─────┘   └──────────────┘   └────────────┘
                  │           │ trust header boundary:
                  │           │ Remote-* headers + X-Naslos-
                  │           │ Proxy-Secret, and only from
                  │           │ TRAEFIK_CIDR (default 10/8)
                  ▼
            ┌──────────────┐    ┌─────────────────────────────┐
            │  naslos-ui  │    │   naslos-api :8080          │
            │  :80         │───▶│   └→ agent :9090 (privileged)│
            └──────────────┘    └─→ LDAP :636 · K8s · Talos · Samba
```

**Trust boundaries (most critical first)**

1. **Traefik ⇄ API header trust.** The API accepts `Remote-User/Remote-Groups`
   only from the configured Traefik pod CIDR **and** only when the request
   carries the `X-Naslos-Proxy-Secret` that Traefik injects. An attacker who can
   reach the API directly (bypassing Traefik/Authelia) can neither spoof the
   headers nor replay them. Default `TRAEFIK_CIDR` is `10.244.0.0/16` (the
   cluster pod CIDR Traefik actually sources from).
2. **Agent privilege.** `naslos-agent` runs privileged with host mounts. It
   exposes a local HTTP port (`:9090`) and must not be reachable from outside.
   Cilium NetworkPolicies are enforced, but because the agent is `hostNetwork`
   pod-level policy does not cover it; see the AUDIT-M4 residual.
3. **Authelia ⇄ LDAP.** All directory access is LDAPS (`:636`) with the CA from
   the `naslos-openldap-tls` secret; Authelia verifies the server name. The bind
   password comes from the `naslos-openldap` Secret, not the ConfigMap.
4. **Samba NT-hash sync.** The agent pushes a `smbusers` mirror which the Samba
   container imports with `pdbedit -i smbpasswd:`; the Samba container must
   constrain `root` usage and should enforce host allow lists on exports.

## Layer 5 — Key Data Flows

### 5.1 First boot & ZFS auto-import

```
 boot ──▶ zfs-service (extension) ──▶ zpool import -fal
   │                                    │  imports every pool found
   │                                    ▼
   └─▶ pools mounted under /var/<name>  ──▶ naslos-agent verifies ZFS,
                                             naslos-api lists pools
```

### 5.2 Disk wizard (create a pool)

```
 UI ──▶ GET  /api/disks          → Talos API      → discovered disks
 UI ──▶ POST /api/disks/recommend → VolumeAdvisor → topology (mirror/raidz1/2/3)
 UI ──▶ POST /api/volumes/zfs    → naslos-api      → naslos-agent
                                                  → chroot /host wipefs *
                                                  → chroot /host zpool create -f
                                                      -o ashift=12 -O compression=zstd …
                                                  → zfs set context=none … (SELinux)
```

### 5.3 SSO login (web)

```
 Browser ──▶ Traefik ──▶ Authelia /api/authz/forward-auth
    Authelia ──▶ OpenLDAP bind (validates userPassword)
    Authelia ──▶ success → Remote-User, Remote-Groups, Remote-Email, Remote-Name
 Browser ──▶ UI (session cookie)
 UI ──▶ /api/... ──▶ API middleware verifies source IP ∈ TRAEFIK_CIDR
```

### 5.4 Password change (web + SMB in one)

```
 UI ──▶ POST /api/users/{uid}/password  (plaintext)
   API ──▶ LDAP Password Modify (RFC 3062)  → OpenLDAP hashes + stores
   API ──▶ compute NT hash (MD4 of UTF-16LE), write the smbusers mirror
   agent ──▶ renders smbusers to the node; samba imports it (`pdbedit -i smbpasswd:`)
   OK ──▶ both stores match; web login + SMB login use the same password
```

### 5.5 Install an app from the catalog

```
 UI ──▶ GET  /api/catalog/{name}       → JSON Schema → rendered form
 UI ──▶ POST /api/apps {name, values}  → merge DefaultValues + values
   API ──▶ helm Install (namespace naslos) → locates chart, waits, deploys
   API ──▶ lists release under GET /api/apps
```

### 5.6 Terminal / logs (WebSocket)

```
 Browser(xterm) ──▶ /api/ws/exec?pod=…  upgrade to ws
   API ──▶ in-cluster K8s config
   API ──▶ remotecommand SPDY executor  → pod streams stdout/stderr back
 Browser ──▶ /api/ws/logs?pod=…         → follow pod logs (tail 100)
```

## ZFS Pool Lifecycle

1. **Discovery** — `talosctl get discoveredvolumes` lists available disks.
2. **Recommendation** — `VolumeAdvisor` suggests topology
   (single / mirror / raidz1 / raidz2 / raidz3) and filters system disks.
3. **Creation** — agent runs `zpool create -f -o ashift=12 -O compression=zstd …`
   with best-practice options, then disables SELinux contexts.
4. **Boot persistence** — the `zfs-service` extension runs
   `zpool import -fal` at every boot.
5. **Shutdown** — `zfs-service` runs `zfs unmount -au` + `zpool export -a`.

Pools built by the wizard use the option set in `zfs.DefaultOptions` plus
`aclmode=restricted` (SMB compatibility) and SELinux context disabling.

## Talos-Specific ZFS Gotchas

- **SELinux**: imported pools need
  `zfs set context=none fscontext=none defcontext=none rootcontext=none <pool>`.
- **Foreign pools**: Samba/NFS share properties from other NAS OSes must be
  cleared (agent wipes disks with `wipefs --all` before creating a pool).
- **Mount path**: pools must mount under `/var/<name>` (Talos's persistent
  writable path); default is `/var/mnt/<pool>`.
- **Noexec**: `/var` defaults to noexec in recent Talos, but ZFS pool mounts
  are separate mounts, so binaries placed on pools still execute.

## Security Model

- The API authenticates to the Talos API with the mounted `talosconfig` Secret
  (`TALOSCONFIG`); it is currently full admin credentials, not an `os:reader`
  Talos ServiceAccount.
- Agent runs privileged but only for ZFS operations.
- Web terminal is ephemeral — no persistent state.
- All operations are auditable via Kubernetes events.
- Auth headers are trusted only from Traefik's pod CIDR (see Layer 4).

## Upgrades

Talos upgrades are unaffected because:

- The ZFS extension is in the Image Factory schematic.
- Naslos components are Kubernetes workloads (survive node reboots).
- ZFS pools are not in Talos's volume system.
- Machine-config patches are applied out-of-band with `talosctl apply-config` /
  `scripts/deploy-vm.sh`; the API does not reapply them.