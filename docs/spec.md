# Naslos System Specification

**Version:** 0.1.0 · **Status:** Normative · **Supersedes:** none (complements the topical docs in this directory)

This document is the complete normative specification of Naslos. It defines
*what the system MUST do* — requirements, contracts, data models, and
acceptance criteria. The other documents in `docs/` describe *how it currently
does it*; where they disagree with this spec, this spec wins. Where the
implementation lags a requirement, the requirement is marked **[OPEN]** and
tracked in `AI_Handoff.md` (current state) and
`docs/AUDIT-2026-09-19-REPORT.md` (findings and remaining work; the historical
code-review list is in `docs/archive/`).

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
- Serving NFS from the kernel: Talos ships no kernel NFS server, so NFS is
  served in userspace by NFS-Ganesha, NFSv4 only (FR-SHR-02). NFSv3 and its
  `rpcbind`/`statd` dependencies are out of scope, as is Kerberos
  (`sec=krb5`) — NFS uses AUTH_SYS.

### 1.4 Target platform

| Constraint | Requirement |
| --- | --- |
| OS | Stock Talos Linux with the official `siderolabs/zfs` Image Factory extension |
| Cluster | Kubernetes ≥ 1.30 across the `naslos` and `naslos-privileged` namespaces |
| Architecture | amd64 |
| Deployment | Helm umbrella chart `charts/naslos` |

---

## 2. System architecture (normative)

### 2.1 Components

| Component | Kind | Role |
| --- | --- | --- |
| `naslos-api` | Deployment | The brain: Talos API + K8s API + ZFS orchestration + HTTP API for the UI |
| `naslos-agent` | privileged DaemonSet (hostNetwork) | Executes `zpool`/`zfs` via `chroot /host`, and is the only writer of the rendered share configuration on the host (`/var/lib/naslos/shares`) |
| `naslos-ui` | Deployment (nginx, static SvelteKit) | Dashboard, wizards, terminal, admin pages |
| `naslos-samba` | DaemonSet (hostNetwork) | Serves SMB on the node's :445, reloads on config change, imports the account mirror, advertises via mDNS/WSD |
| `naslos-openldap` | StatefulSet | Identity store (users, groups, password hashes) |
| `naslos-traefik` | Deployment | Ingress (IngressRoutes), TLS termination, forwardAuth to Authelia |
| Authelia | StatefulSet | Web SSO / 2FA against OpenLDAP. The API restarts `naslos-authelia-0` (pod delete) when the runtime SSO fragments change (FR-APP-15) |
| ntfy | (external server, no bundled chart) | Push notifications for system events (`ntfy.sh` or self-hosted) |
| `zfs-service` | Talos system service | Auto-imports pools at boot (`zpool import -fal`) |

### 2.2 Trust boundaries and security requirements

- **SEC-1** — `naslos-agent` MUST run privileged (host mounts + `chroot /host`)
  solely to execute ZFS commands; it MUST NOT expose any endpoint that runs
  arbitrary shell input. All command arguments MUST be generated server-side.
- **SEC-2** — LDAP client connections MUST use LDAPS (TLS) with a CA cert
  (`LDAP_USE_TLS=true` default).
- **SEC-3** — `naslos-api` MUST only honor Traefik-injected authentication
  headers (`Remote-User` etc.) from requests originating in `TRAEFIK_CIDR`
  (default `10.244.0.0/16`). A request without a peer address MUST be refused
  (fail closed).
- **SEC-15** — Unsafe HTTP methods MUST be refused when the browser labels the
  request cross-site (`Sec-Fetch-Site`), when `Origin` does not match the
  request host, or when a body-bearing write does not declare
  `application/json`; non-browser clients that send neither header remain
  supported.
- **SEC-16** — A WebSocket upgrade MUST be refused unless a present `Origin`
  matches the request host (ports aside) or a configured Naslos domain.
  Non-browser clients that send no `Origin` remain supported.
- **SEC-17** — The MCP endpoint MUST authenticate clients with operator-created
  bearer tokens: the stored form is a SHA-256 hash compared in constant time,
  the plaintext is shown exactly once at creation, every token carries one
  scope (read < write < full), and `tools/call` MUST re-check both the scope
  and (for full-scope destructive tools) an explicit `confirm` argument equal
  to the resolved target before dispatch. The endpoint MUST refuse every
  request while MCP is disabled (404), apply the SEC-10 proxy trust checks
  with no opt-out, refuse a foreign `Origin` (SEC-16 rules), and block a
  source address after ten failed attempts inside five minutes. *(contract
  tests: `the_full_chain_rejects_every_shortcut`, `disabled_answers_404`,
  `ten_failures_block_the_source_ip`, `a_read_token_cannot_call_a_write_tool`,
  `a_full_tool_without_confirm_is_refused_before_dispatch`,
  `forged_identity_cannot_bypass_the_public_auth_layers`)*
- **SEC-4** — The user's LDAP password MUST be mirrored to Samba's passdb as
  an NT hash at password-change time; plaintext passwords MUST NOT be stored
  anywhere.
- **SEC-5** — Service secrets (`LDAP_BIND_PASS_FILE`, ntfy topic tokens) MUST be
  injected by the Helm chart, never baked into images.
- **SEC-6** — Backup peer requests MUST authenticate with the peer's own Ed25519
  key and MUST NOT be served on the strength of the interactive-proxy identity
  header (a peer cannot complete an interactive login). The receive API MUST be
  discoverable only where the API already is: no additional listener, port or
  ingress is introduced for peers.
- **SEC-7** — The endpoints that authorize or revoke backup peers MUST require
  evidence of an authenticated session through the owner gate (SEC-10), not the
  mere presence of an identity header.
- **SEC-8** — A receiver MUST validate every path component derived from a peer's
  input (source names) and MUST confine the received data to the dataset it was
  given: the receive dataset is the only read-write host path the API Deployment
  may hold.
- **SEC-9** — The agent's streaming backup endpoints MUST NOT be reachable from
  outside the cluster (the agent Service stays ClusterIP/headless) and MUST be
  limited to server-generated command lines: a caller may name a dataset and a
  snapshot, nothing more.
- **SEC-10** — Every owner-facing API route MUST require proof that the request
  came through the authenticating proxy: a shared secret, generated by the chart
  into a Secret, injected by a Traefik middleware, and compared in constant time.
  An identity header without it MUST NOT authorize anything, the API MUST refuse
  to start when the secret is unset, and there MUST be no opt-out that serves
  owner routes without it.
- **SEC-11** — The privileged, host-networked agent MUST require a bearer token
  on every endpoint except its health probe, MUST compare it in constant time,
  MUST refuse to start without one, and MUST NOT expose any opt-out that turns
  the check off.
- **SEC-12** — A one-time enrollment token MUST be single use across restarts (the
  record MUST survive a process restart), and an existing peer's name MUST NOT be
  re-keyed without an explicit revoke: otherwise a leaked token could substitute a
  peer's key while the owner's peer list looked unchanged.
- **SEC-13** — Owner-facing job identifiers MUST be unguessable (at least 128 bits):
  they are the handle for inspecting and cancelling a running transfer.
- **SEC-14** — A receiver's replay cache MUST survive a process restart (a request
  captured just before a restart MUST NOT be replayable inside the clock-skew
  window), MUST be bounded in memory per key, and MUST reject a nonce that is not
  the expected shape before storing it.

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
  listable from the UI via the agent. Dataset names MUST be validated
  (ZFS-safe per component, no traversal, no snapshot `@`), and creation MUST
  only set a safe property subset (`compression`, `quota`, `recordsize`,
  `atime`, `copies`, `readonly`) so a request cannot place a dataset outside
  the pool (e.g. `mountpoint=/`).
- **FR-STO-08** — A pool MUST be growable from the UI by attaching disks
  (`zpool add`). The API MUST refuse a disk that is not a whole disk the node
  reported as usable (so the system disk and a mistyped path are rejected) and
  MUST refuse a disk that already belongs to a pool **even with `force`**, since
  that would overwrite the owning pool's label. The UI MUST state, before the
  operation, that a redundancy-less vdev lowers the pool's fault tolerance and
  that the change is not reversible.
- **FR-STO-09** — Dataset destruction MUST be explicit: a non-empty dataset
  MUST require the recursive flag (the UI MUST say what that destroys), a pool's
  root dataset MUST NOT be destroyable through the dataset API, and a dataset
  that a share is serving (or that backs a share's path) MUST be refused with a
  conflict instead of removing the share's data.

### 3.1.1 Disk health (`FR-DHK`)

- **FR-DHK-01** — The agent MUST report per-whole-disk SMART health (health
  status, temperature, power-on hours, wear, reallocated/pending/uncorrectable
  sectors and media/error-log counts where the device exposes them) read-only,
  and MUST NOT wake a standby disk (`smartctl -n standby`).
- **FR-DHK-02** — Unsupported or unavailable SMART MUST be reported as
  `available:false` with a reason, not as an endpoint error; the UI MUST show
  the disk as unavailable/standby while keeping its ZFS data visible.
- **FR-DHK-03** — `GET /api/disks/health` MUST merge the Talos disk inventory,
  ZFS pool/vdev state and error counters, and the agent's SMART data. The
  existing `GET /api/disks` wizard contract MUST stay unchanged.
- **FR-DHK-04** — The UI MUST provide a per-disk health page with a summary,
  a severity-sorted table (device, model/serial, pool/vdev, ZFS state, SMART
  health, temperature, hours, wear, error counters) and a refresh control.
- **FR-DHK-05** — The health watcher MUST notify on SMART `FAILED`, temperature
  above `DISK_HEALTH_TEMPERATURE_C` (default 55 °C), wear at/above
  `DISK_HEALTH_WEAR_PERCENT` (default 80 %) and ZFS vdev
  `FAULTED|OFFLINE|UNAVAIL|REMOVED`/`DEGRADED` transitions, using the existing
  `disk_failure`/`zfs_health` events. Repeated polls MUST NOT repeat an alert;
  a recovered disk/condition MUST send at most one recovery notice.
- **FR-DHK-06** — The agent image MUST bundle a pinned, checksum-verified
  `smartctl`, and the DaemonSet MUST mount the host `/sys` read-only for device
  detection. The agent endpoint MUST require the agent bearer token.
- **FR-DHK-07** — Disk health MUST be read-only: no SMART self-tests, no
  enabling SMART, no device writes, and no user-controlled command arguments.

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
- **FR-IDN-13** — SMB authentication MUST work for directory users with no
  account created on the node. Since Samba attaches a session to a POSIX uid,
  the API MUST mirror each user's POSIX identity (`uidNumber`, `gidNumber`) and
  each group's membership into the NSS files the serving container resolves
  (`passwd`/`group`/`shadow` via `extrausers`), and MUST mirror the NT hash into
  Samba's passdb. The serving container MUST NOT need LDAP credentials, TLS
  material or network access to the directory.
- **FR-IDN-14** — A password change MUST apply to **new** SMB sessions, and the
  UI MUST state (a) that it is not instant and (b) that already-connected
  sessions keep the old credentials until the client reconnects. The convergence
  window MUST be bounded and tunable (`shares.confCheckInterval`).
- **FR-IDN-15** — Account state MUST track LDAP: disabling a user MUST deny SMB
  logins (`NT_STATUS_ACCOUNT_DISABLED`) while retaining the stored hash, so
  re-enabling needs no new password; deleting a user MUST remove both the
  passdb account and its NSS entry.
- **FR-IDN-16** — Change detection for the node's mirror MUST NOT rely on
  whole-second timestamps: two writes in the same second MUST both take effect.

### 3.3 Shares (`FR-SHR`)

- **FR-SHR-01** — Shares MUST be backed by ZFS datasets: a share path MUST be a
  folder on a mounted ZFS dataset — either the dataset's mountpoint or a
  subfolder of it — and the API MUST refuse a path that is not (the base
  directory's children are not necessarily datasets — Talos keeps `/var` on
  EPHEMERAL, so a plain directory there would hold data outside every pool,
  without checksums, snapshots or redundancy, and lose it on upgrade). A share
  path MUST exist as a folder; the API MUST provide folder creation and listing
  confined to the datasets so an operator can point a share at a new folder
  inside the pool without leaving the UI, and that creation MUST be refused
  outside the datasets and MUST NOT be able to delete non-empty folders. Share
  paths MUST be editable, and a path change MUST be validated like a create.
  `GET /api/shares/paths` MUST offer only dataset mountpoints, and the serving
  containers MUST log whether each share path is on a mounted filesystem.
  Mount points exposed to containers MUST use `mountPropagation: HostToContainer`
  so a dataset mounted after a pod starts is visible rather than the pod serving
  the underlying directory.
- **FR-SHR-02** — SMB (including Time Machine via the `fruit` VFS) MUST be
  served. AFP MUST NOT be offered — it is not served, and the API MUST reject
  it rather than accept a share nothing exports. NFS MUST also be served, in
  userspace: the Talos kernel has no NFS server (`nfsd`), so NFS is served by
  NFS-Ganesha (NFSv4 over TCP on the node's :2049) from an API-rendered config,
  and it MUST NOT require `rpcbind`, a node account, or a kernel module. Every
  NFS share MUST be presented with a reachable address in the UI.
- **FR-SHR-03** — Effective Samba/NFS configuration MUST be retrievable from
  the API (`/api/shares/config/samba`, `/api/shares/config/nfs`).
- **FR-SHR-04** — Share definitions MUST be durable: they MUST survive an API
  pod restart and a chart upgrade. (An in-memory-only manager was the original
  defect.)
- **FR-SHR-05** — Share paths MUST be canonicalised before validation, and a
  path that escapes the ZFS base after canonicalisation (e.g.
  `/var/mnt/../etc`) MUST be rejected. A plain prefix check is not sufficient.
- **FR-SHR-06** — Rendered configuration MUST reach the node without the API
  needing cluster RBAC: the privileged agent MUST be the only writer of the
  host configuration, writing atomically, and MUST NOT rewrite unchanged files.
  A new configuration MUST NOT be applied unless it validates (`testparm`), so
  a bad render leaves the previous working configuration serving.
- **FR-SHR-07** — Share access MUST be restrictable by **user and by LDAP
  group**. Group entries MUST be rendered as `@group`, and the group's real
  membership MUST be resolvable on the node (see FR-IDN-13). A name MUST NOT be
  ambiguous between a user and a group.
- **FR-SHR-08** — The server MUST advertise itself so it is discoverable by
  browsing clients: mDNS (`_smb._tcp`) for Linux/macOS and WSD for Windows. The
  advertisement MUST be restricted to the LAN interface — advertising
  pod-network addresses MUST NOT happen — and MUST be best-effort: a discovery
  failure MUST NOT stop SMB from serving.
- **FR-SHR-09** — The UI MUST display, for each share, the address a client
  should use (`smb://<host>/<name>`), and MUST NOT display an address for a
  protocol that is not served.
- **FR-SHR-10** — The Samba `netbios name` MUST equal the advertised discovery
  name, so browsing clients and direct connections see one identity.
- **FR-SHR-11** — Any change that affects access MUST re-push the node's
  mirror: share create/update/delete, but also group membership changes, group
  delete, user create and user delete. A change that only mutates LDAP and
  never re-pushes would leave access stale.
- **FR-SHR-12** — Share list fields (`allowedHosts`, `validUsers`,
  `validGroups`) MUST be served as arrays, never `null`, including for
  definitions read back from disk (FR-IDN-08's rule applies to shares too).

### 3.3.1 Access control (`FR-ACL`)

Dataset and share permissions are being introduced in slices; the decision
record is [`docs/acl.md`](acl.md). FR-STO-01's pool default (`acltype=posixacl`)
remains pinned: a move to NFSv4 ACLs is a separate, explicit decision, never a
silent default change.

- **FR-ACL-01** — ACL permission changes MUST be expressed as a fixed,
  allow-listed preset that maps to POSIX owner/group/other permissions and the
  corresponding base `u::`/`g::`/`o::`/`m::` entries. The system MUST NOT accept
  free-form ACL text, arbitrary ACL entries, raw `setfacl` arguments or a
  preset name that is not on the allow-list. An allow-listed preset MUST NOT
  grant write access to the other class. *(acl tests:
  `shares::acl::tests::preset_allow_list_maps_to_posix_mode_and_entries`,
  `shares::acl::tests::preset_names_round_trip_and_unknown_names_are_refused`,
  `shares::acl::tests::no_preset_grants_world_write`)*
- **FR-ACL-02** — Applying a preset MUST be refused when it would leave the
  Naslos service account without its required access to the dataset or share
  path: read and traverse for a read-only requirement, read/write/traverse for
  a read-write requirement. The check MUST evaluate POSIX owner, group
  (including supplementary groups) and other classes, and every allow-listed
  preset MUST keep the dataset owner's full access. *(acl tests:
  `shares::acl::tests::effective_permissions_follow_owner_group_other`,
  `shares::acl::tests::preset_that_locks_out_the_service_account_is_refused`,
  `shares::acl::tests::read_only_requirement_accepts_read_presets`,
  `shares::acl::tests::every_preset_keeps_owner_full_access`)*
- **FR-ACL-03** — The UI MUST provide a permission editor for a dataset or share
  folder that shows the owner, group, current mode/base ACL and selected preset,
  and lets an operator choose only an allow-listed preset. **[OPEN]**
- **FR-ACL-04** — Directory inheritance MUST be explicit: default ACL entries
  MUST be visible and editable, a new child MUST inherit them, and a recursive
  apply MUST require explicit confirmation of its scope. **[OPEN]**
- **FR-ACL-05** — An effective-access preview MUST state which layer it
  evaluates (dataset ACL, share access list, protocol identity) and MUST NOT
  claim a live probe when it is a calculation. **[OPEN]**
- **FR-ACL-06** — The system MUST provide a Samba ACL mapping that does not
  silently create two access models: if Windows ACLs are stored opaquely, the
  UI MUST say that SMB and NFS see different permissions, or the backend MUST
  keep the dataset ACL authoritative for both protocols. **[OPEN]**
- **FR-ACL-07** — NFSv4 datasets (`acltype=nfsv4`) MAY be offered as an
  explicit per-dataset option only after Samba's ACL backend, NFS-Ganesha's
  NFSv4 ACL support, `aclmode`/`aclinherit` behavior and Windows identity
  mapping are proven; the pool default MUST remain FR-STO-01's `posixacl`.
  **[OPEN]**
- **FR-ACL-08** — A dataset ACL-type migration MUST be explicit and have a
  planned rollback: the system MUST NOT flip `acltype` in place without a
  confirmation that states the conversion limits, MUST expose the ACL type per
  dataset so a mixed tree is visible, and MUST describe how a rollback restores
  the previous ACLs. **[OPEN]**
- **FR-ACL-09** — Live ACL writes MUST be validated in the API and applied by
  the privileged agent, never by the unprivileged API or by a browser-supplied
  command. A write MUST record an audit entry, offer a dry-run, and have a
  documented recovery path for a service-account lockout. **[OPEN]**

### 3.4 App catalog & install (`FR-APP`)

Apps come from **git-based chart repositories** (an official repository plus
admin-added ones) rather than a compiled-in list; each app is a chart directory
with a sibling `naslos-app.yaml` install-config. See
[app-catalog.md](app-catalog.md).

- **FR-APP-01** — Apps MUST be defined as entries discovered from configured
  chart repositories, each with display metadata, a Helm chart and a
  JSON-Schema config form. *(catalog_test.go)*
- **FR-APP-02** — App configuration forms MUST be generated from the app's
  JSON Schema.
- **FR-APP-03** — Installing/upgrading an app MUST be a Helm operation **from the
  cloned local chart path** in the `naslos-apps` namespace. **[OPEN]** — the
  Rust port drives the bundled `git` and `helm` CLIs (verified checksums in
  `rust/api/Dockerfile`), so the “MUST NOT require a binary” clause is not met;
  the spec needs either an amendment or an embedded library.
- **FR-APP-04** — Start/stop MUST scale replicas 0↔N and MUST preserve PVCs.
  **[OPEN]** — no route exists yet; uninstall/install is the supported path.
- **FR-APP-05** — A source MUST declare public, HTTPS-token or SSH-deploy-key
  authentication; credentials MUST come from Kubernetes Secrets and MUST NOT be
  returned by the API. *(chartsrepo tests)*
- **FR-APP-06** — Repository ingestion MUST resist path traversal and symlink
  escapes, and MUST refuse pathological repositories (size/file-count guard).
  *(chartsrepo tests)*
- **FR-APP-07** — Sources MUST map channels to git branches; an unknown channel
  MUST be rejected rather than silently falling back. Each app exposes the
  channels it is available in, and a **user source overrides the official source
  on name collision**. *(catalog tests)*
- **FR-APP-08** — `naslos-app.yaml` MUST validate: the name MUST equal the folder
  name and be a DNS-1123 label, service ports MUST be in range, and service-name
  templating MUST be limited to the release name. *(catalog tests)*
- **FR-APP-09** — Catalog refresh MUST use a cached clone with a TTL and MUST
  fall back to the stale clone when the remote is unreachable. *(chartsrepo
  tests)*
- **FR-APP-10** — Exposure MUST be orthogonal toggles: `baseDomain`, `subdomain`,
  `tls`, `auth`, `localOnly`, `internet`. The `baseDomain` MUST be selectable
  among the configured domains (primary first, default the primary) and an
  unconfigured one MUST be rejected. Turning `tls` off MUST NOT inherit
  subdomain HSTS; an empty `subdomain` MUST remove the route (cluster-internal
  only); `auth` MUST be offered only when the selected base domain is in the
  effective SSO list. `internet` MUST default to on. Turning it off MUST be
  accepted only for an app in a dedicated (isolated) baseline namespace, where
  the app MUST be denied public egress while keeping cluster DNS and
  app-to-app traffic; the API MUST reject the opt-out for privileged or shared
  namespaces, whose policy always allows public egress.
  *(routing tests, apps tests, server tests)*
- **FR-APP-11** — The API MUST own one Traefik `IngressRoute` per app plus its
  middlewares and the per-app Internet-egress `NetworkPolicy` in the app
  namespace, and MUST reconcile them on startup and delete them on uninstall.
  When an app installs into a dedicated (isolated) baseline namespace with
  `internet` on, the API MUST apply the egress policy **before** the Helm
  install wait, remove it if the install fails, and sweep API-created
  policies left by an interrupted install before the reserved namespace is
  reused (and at startup). *(routing tests, apps tests; startup call in
  `main.rs` after SEC-2026-H9)*
- **FR-APP-16** — When a manifest declares no route target, the API MUST
  discover the release's Services from its Helm labels, route to one, and expose
  the candidates for the operator to pick. *(discovery_test.go)*
- **FR-APP-17** — An app whose manifest sets `privileged: true` (a VPN sidecar
  needing `NET_ADMIN`, which PSA `baseline` forbids) MUST install into the
  separate privileged apps namespace, with its own Role and routing; all other
  apps MUST stay under `baseline`. Such an install MUST additionally carry
  `privilegedConfirmed: true`, and the UI MUST show a separate privileged
  warning; a missing confirmation is a 400. *(chart render + routing/apps tests,
  `privileged_installs_need_a_second_confirmation`)*
- **FR-APP-20** — With per-app isolation enabled, an installed app MUST run in
  its own namespace (a fixed pool is prepared by the chart) with PSA, quota and
  a default-deny NetworkPolicy. The chart MUST provision more slots than are
  usable by default, and the owner MAY raise or lower the usable limit from the
  UI up to the provisioned count without a platform upgrade (persisted across
  restarts and upgrades, never below the installed app count); exceeding the
  provisioned count requires a platform upgrade. An app MAY select any number of
  registered
  datasets at install and on reconfigure, and each MUST be exposed through a
  PV/PVC whose `hostPath` is that dataset only (privileged apps receive the host
  path directly). The API MUST inject the authoritative `datasets` list — one
  `{name, mountPath: "/datasets/<name>", claim|hostPath}` entry per dataset —
  strip client-supplied `datasets`/`datasetsClaim`/`datasetsClaims` values, and
  never hand a chart the `/var/mnt` root. Records that carry datasets MUST be
  migrated once to that layout, and a record without a per-app namespace MUST
  NOT gain or change datasets without a reinstall. *(chart render gate +
  `apps::dataset_layout_tests` + `apps::value_schema_tests` pool-limit tests)*
- **FR-APP-21** — An app install MAY select a ZFS pool for the chart's own
  persistence PVCs. When `pool` is set the API MUST reject a pool that does not
  exist, provision a pool-scoped local-path provisioner and StorageClass
  (`naslos-pool-<slug>-<hash>`, path `/var/mnt/<pool>/apps`), and set every
  chart `persistence.*.storageClass` to that class. The default (no pool)
  remains `local-path`. *(apps tests, live drill)*
- **FR-APP-22** — The Installed apps view MUST show catalog identity, the
  installed and latest chart versions, an update signal and live per-app usage.
  `GET /api/apps` MUST add `displayName`, `icon`, `iconUrl`,
  `installedVersion` (live Helm version when readable, else the record),
  `latestVersion`, `latestSource`, `latestChannel`, `updateAvailable`,
  `imageUpdateAvailable`, `imageCurrent` and `imageLatest` (the last two are
  short running/served digests when an image update is offered);
  `GET /api/apps/updates` MUST return the
  `{count, apps:[{name, installedVersion, latestVersion, imageUpdate}]}`
  summary without Helm calls (an image-only update uses the cached drift
  snapshot from FR-APP-27, never a registry call); `GET /api/apps/usage` MUST
  aggregate the kubelet `stats/summary` (CPU `usageNanoCores`, memory
  `workingSetBytes`, cumulative `network.rxBytes`/`txBytes`) per app, answer
  **200** even when unavailable (`available:false` + `error`), and report
  unknown numeric values as `null`. Chart update availability MUST be false
  for an empty latest version, an orphaned record or a missing catalog entry,
  and MUST NOT report a downgrade as an update. The upgrade action MUST be
  `PUT /api/apps/{name}` with `{}`. Reading `nodes`/`nodes/proxy` MUST be a
  separate read-only ClusterRole (`get`/`list` on nodes, `get` on
  nodes/proxy) with no write or exec. *(apps update tests, app_usage tests,
  chart render gate)*
- **FR-APP-23** — The catalog MUST be refreshed in the background at
  `CHARTS_TTL` (default 15m) and the snapshot rebuilt each cycle; a failed
  refresh MUST keep the previous snapshot and fall back to the stale clone. The
  one-time dataset-layout migration MUST stay chained to the first refresh.
  *(chartsrepo tests, `main.rs` refresher)*
- **FR-APP-24** — The app detail drawer MUST show the app's live logs: a
  read-only log stream for a pod/container of the app, with pod and container
  pickers, a follow toggle and a bounded buffer. `GET /api/apps/{name}/pods`
  MUST list only pods in that record's namespace (filtered by
  `app.kubernetes.io/instance` when the namespace is shared) with their phase,
  start time and containers; streaming MUST reuse the owner-gated
  `/api/ws/logs` socket. The API's `pods/log` access MUST be `get`-only and, for
  isolated apps, granted by the per-namespace isolation ClusterRole.
  *(chart render gate + audit assertion, live drill)*
- **FR-APP-25** — The app detail drawer MUST let the owner edit the app's
  container CPU/memory requests and limits for the chart's `resources` values,
  in both the flat (`resources.requests|limits`) and per-component
  (`resources.<component>.requests|limits`) layouts the catalog charts use.
  Saving MUST enqueue the normal upgrade job (`PUT /api/apps/{name}` with
  `{values:{resources}}`), preserving resource keys the form does not edit. The
  API MUST reject an invalid CPU/memory quantity (anything other than a
  non-negative decimal with an optional decimal or binary suffix) on install and
  upgrade before Helm runs, and MUST reject aggregate totals above the app
  namespace's `naslos-app-quota`. `GET /api/apps/{name}/quota` MUST report that
  quota (`{available, namespace, hard}`) so the drawer shows the effective cap.
  The quota itself MUST be configurable (`apps.isolation.quota`).
  *(apps::resource_quantity_tests, UI check)*
- **FR-APP-26** — A chart MAY declare `capabilities: [env]`. For such a chart
  the owner MUST be able to set custom environment variables per installed app.
  Values MUST live only in a platform-owned Secret `naslos-env-<release>` in the
  app's namespace (labels `app.kubernetes.io/managed-by: naslos`,
  `naslos.io/app: <name>`), MUST NOT be written to the app record or Helm
  values, and MUST NOT be returned by `GET /api/apps`; `GET /api/apps/{name}`
  MUST return the names, with `null` values for `secret` variables. The API
  MUST validate names (`^[A-Za-z_][A-Za-z0-9_.-]*$`, ≤128 bytes, unique, no
  `NASLOS_` prefix) and size (≤100 variables, ≤32 KiB per value, <512 KiB in
  total), and MUST reject a non-empty list when the chart lacks the capability
  (400). It MUST write the Secret before Helm, delete it when the install fails
  or the app is uninstalled, and change the `naslosEnvChecksum` Helm value on
  every value change so the pod template annotation rolls the workload. Names
  the chart itself sets on the containers that mount the Secret MUST be rejected
  after rendering it (`env var X is set by the chart`). An upgrade MUST treat an
  omitted `env` as unchanged and a `secret` variable with `value: null` as
  keep-stored; it MUST snapshot and restore the Secret across a failed Helm
  operation, and a chart that drops the capability MUST keep the Secret and
  record a warning instead of failing. *(app_env tests, catalog tests, apps
  record tests, Playwright)*
- **FR-APP-27** — The Installed view MUST offer an update when the registry
  serves a different digest for an installed app's image tag (a floating tag
  such as `latest`), not only when the catalog chart version changes. The API
  MUST compare the running pod's `imageID` digest with the tag's top-level
  registry digest *or* its `linux/amd64` manifest digest (runtimes report
  either), cache lookups for `IMAGE_CHECK_TTL` (default 1h), normalize
  `docker.io`/`index.docker.io` to `registry-1.docker.io`, and never report an
  update for a digest-pinned image, an unreadable registry, or a catalog chart
  older than the installed one. The background catalog cycle and the manual
  Refresh MUST rebuild the per-app drift snapshot; list and update-summary
  requests MUST NOT make registry calls, and the manual Refresh MUST NOT block
  its HTTP response on the registry scan. Every successful upgrade with a known
  drift MUST force a re-pull of only the drifted containers
  (`imagePullPolicy: Always` plus a `naslos.io/image-refresh` pod annotation),
  wait for the bounded rollout, and record a failed pull/rollout in the app's
  `lastError` while keeping the drift for retry. The chart MUST allow API-only
  egress to HTTPS registries (`networkPolicy.imageEgress`, default on).
  *(images tests, apps update tests, chart render gate, live drill)*
- **FR-APP-19** — Chart installs MUST run as a namespaced identity that cannot
  create Traefik `IngressRoute`/`Middleware` or cert-manager objects; the API
  MUST own the app's route and certificate. *(chart render gate:
  `naslos-helm-apps` Role excludes `traefik.io`/`cert-manager.io`)*
- **FR-APP-18** — Install, upgrade and uninstall MUST run as **background jobs**
  with an observable stage (`preparing` → `installing` → `finalizing`).
  `POST /api/apps`, `PUT /api/apps/{name}` and `DELETE /api/apps/{name}` MUST
  validate, enqueue and answer `202 {jobId, state:"running"}` instead of blocking
  on Helm; the job MUST run under a server-owned context so a disconnected
  client cannot cancel it, its state MUST be observable via
  `GET /api/apps/jobs/{id}` (and `GET /api/apps/jobs`), and a second job for an
  app that already has one running MUST be rejected with `409`. *(server
  app_jobs tests, apps tests)*
- **FR-APP-12** — Installing a third-party chart MUST require an explicit
  confirmation in the request (`confirmed: true`), plus
  `privilegedConfirmed: true` when the catalog entry is privileged (FR-APP-17).
  *(server tests)*
- **FR-APP-13** — Base domains and their cert-manager ACME DNS-01 certificates
  MUST be manageable, with the provider and its solver driven by the declarative
  registry (§3.9): built-in `cloudflare`, `rfc2136` and raw `passthrough` behavior
  MUST be preserved and OVH MUST be supported, with staging/production
  environments; the wildcard Certificate MUST cover the domain and
  `*.<domain>`. *(certs tests)*
- **FR-APP-14** — A release found in the cluster without a catalog match MUST be
  surfaced as **orphaned** rather than silently uninstallable. *(apps tests)*
- **FR-APP-15** — Authelia MUST protect every base domain in the effective SSO
  list — the primary domain plus domains promoted at runtime from the Domains
  page, seeded by the chart-declared `sso.domains` — including app subdomains.
  A promotion MUST take effect live: the API MUST render the SSO fragments,
  restart Authelia, and serve the portal on the promoted domain (Authelia's
  session cookie is per domain, so without a portal there the login redirect
  404s); the primary MUST NOT be demotable, and a domain MUST NOT be demoted
  while an installed app requires auth on it. *(chart render: authelia-config +
  naslos-authelia-sso; authelia tests; routing tests; server tests)*


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
    "cpu":     { "usagePercent": 1.7, "cores": 6, "loadAvg1": 0.1, "loadAvg5": 0.2,
                 "loadAvg15": 0.1, "model": "AMD Ryzen 5 3600 6-Core Processor",
                 "temperatureC": 45.0,
                 "perCore": [ { "core": 0, "usagePercent": 1.2, "frequencyMhz": 3600 } ] },
    "memory":  { "total": 10441696256, "used": 3108004864, "free": 3217534976,
                 "available": 7153684480, "usagePercent": 29.7, "swapTotal": 0, "swapUsed": 0 },
    "disk":    { "total": 43580284928, "used": 5165384704, "free": 38414900224, "usagePercent": 11.8 },
    "network": { "bytesSent": 462901472, "bytesRecv": 1348276586,
                 "packetsSent": 755373, "packetsRecv": 797224,
                 "interfaces": [ { "name": "eth0", "ipAddress": "" } ] },
    "zfs":     { "pools": [ { "name": "tank", "size": 0, "alloc": 0, "free": 0,
                              "usagePercent": 0.0, "health": "ONLINE" } ] },
    "system":  { "hostname": "talos-…", "uptime": 18654, "os": "Talos Linux",
                 "kernel": "6.18.51-talos", "talosVersion": "v1.14.1" },
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
- **FR-MET-10** — The dashboard MUST show each network interface with its address:
  the routable IPv4 where there is one (otherwise the routable IPv6), never the
  loopback or a link-local address, and the interface name alone when no address
  can be determined. Interface names and counters come from `/proc/net/dev`;
  addresses come from the node's Talos `AddressStatus` resources, and a failure to
  read them MUST NOT hide the rest of the metrics. Multi-node metric aggregation
  remains **[OPEN]** (this is a single-node product).
- **FR-MET-11** — CPU detail MUST include the model name, logical core count,
  1/5/15-minute load averages, and per-core usage derived from two
  `/proc/stat` samples (a bounded delta, not the since-boot average). Per-core
  frequency MUST be included when `/proc/cpuinfo` exposes it. The collector
  MUST also publish the CPU temperature from a CPU-like `hwmon`
  (`coretemp`/`k10temp`/`zenpower`/`cpu`) or, failing that, a thermal zone; an
  absent sensor MUST be `temperatureC: null` and MUST NOT fail the metrics.
- **FR-MET-12** — The dashboard MUST show the CPU model, load averages,
  temperature (or an explicit "not exposed on this node" message), and one
  usage bar per logical core. The per-core count MUST match `cpu.cores`.

### 3.6 Logs & terminal (`FR-LOG`)

- **FR-LOG-01** — Log streaming and shell access MUST use WebSockets
  (`/api/ws/logs`, `/api/ws/exec`).
- **FR-LOG-02** — The web terminal MUST run a shell in a privileged container
  with the host mounted (Talos has no shell by design), deployed by the chart
  and switchable off with `terminal.enabled`. The container MUST NOT need ZFS
  packages of its own: it reaches the host's tooling through `chroot /host`, so
  it runs the binaries matching the kernel module.
- **FR-LOG-03** — The terminal MUST NOT require typing a pod name: the API MUST
  list namespaces and pods (with their containers) and mark the shell container,
  and the UI MUST preselect it.
- **FR-LOG-04** — The exec endpoint MUST accept only shell names from a fixed
  set (never an arbitrary command), MUST resolve the container rather than guess
  (an ambiguous pod is an error naming the containers), MUST reject a pod that is
  not running, and MUST validate namespace/pod names against DNS-1123 so a caller
  cannot shape an API path.
- **FR-LOG-05** — The interactive session MUST carry window resizes, so
  full-screen tools are usable: the browser MUST send the terminal size on open
  and on every resize, and the server MUST apply it as the session's TTY size.
- **FR-LOG-06** — A failed session MUST be diagnosable: the endpoint MUST answer
  a plain GET (no upgrade) with the resolved target or the reason it cannot
  attach, because a browser cannot read the status of a failed WebSocket
  handshake.
- **FR-LOG-07** — The API's terminal permissions MUST be least-privilege: pods,
  pods/log and pods/exec scoped to the Naslos namespace by a Role, and only
  namespace listing granted cluster-wide.
- **FR-LOG-08** — The terminal MUST be served only to requests that prove an
  authenticated session, and MUST fail closed without it: the API MUST require
  the proxy-issued shared secret (SEC-10) *and* the identity header its
  authenticating proxy injects (Authelia's `Remote-User` via Traefik
  `forwardAuth`, which replaces any client-supplied value). The chart MUST route
  those paths from the proxy straight to the API, and MUST NOT expose an
  unauthenticated listener that could reach them.

### 3.7 Notifications (`FR-NTF`)

- **FR-NTF-01** — System events (Talos / Kubernetes / ZFS) MUST push to ntfy
  topics with a severity level.
- **FR-NTF-02** — The API MUST expose `POST /api/notifications/test` to send
  a test notification.

### 3.8 Buddy Backup (`FR-BUD`)

Buddy Backup lets one instance push encrypted backups to another instance, or to
a standalone container receiver, without the receiver being able to read them.
See `docs/buddy-backup.md`.

- **FR-BUD-01** — Every instance MUST be able to act as both a receiver and a
  sender; the receive API MUST live at `/api/buddy/v1/*` and the standalone
  `naslos-buddy-receiver` container MUST serve the identical routes, so a sender
  needs no special case per receiver flavour (SQLite-free, ZFS-free).
- **FR-BUD-02** — Backups MUST be encrypted **client-side** with AES-256-GCM
  before they leave the sender, using a fresh per-chain 256-bit data key that is
  wrapped with the owner's key encryption key (KEK) and stored in the signed
  manifest. The receiver MUST NOT hold, request or be able to derive the KEK: it
  stores ciphertext and wrapped keys only.
- **FR-BUD-03** — Requests MUST be authenticated with an Ed25519 keypair owned by
  the sender (OpenSSH form): the signature MUST cover the method, the full request
  URI, the SHA-256 of the body, a timestamp and a nonce. The receiver MUST reject
  a timestamp outside 5 minutes of its clock and MUST reject a repeated nonce.
  Enrollment of a first key MUST be possible with a single-use token.
- **FR-BUD-04** — The payload MUST be sent as independently sealed, fixed-size
  chunks whose nonce is derived from a per-chain prefix and the chunk index, and
  whose AEAD additional data MUST bind the source, the chain, the index and the
  plaintext length, so that reordering, swapping, truncation or cross-source
  substitution is detectable.
- **FR-BUD-05** — A push MUST be resumable: interrupting it MUST NOT require
  re-sending what the receiver already holds, and resuming MUST verify the stored
  chunks against the sender's own bytes and MUST fail rather than mix two versions
  of the data into one chain.
- **FR-BUD-06** — A chain MUST NOT be restorable until its manifest is published,
  and the receiver MUST refuse a manifest whose listed chunks are not all present,
  so "the latest backup" can never point at an incomplete chain. The manifest MUST
  be signed by the sender's key and MUST record a SHA-256 per plaintext chunk so
  the owner can verify integrity independently of the receiver.
- **FR-BUD-07** — The receiver MUST report, per key: free space, bytes stored,
  optional quota, the sources it holds, their current chains, and the time of the
  last backup; the sender MUST be able to display these.
- **FR-BUD-08** — A receiver MUST enforce per-key scope (allowed source prefixes,
  no path traversal, no absolute sources) and an optional per-key quota, and MUST
  refuse a chunk that would exceed it.
- **FR-BUD-09** — The owner MUST be able to prune a source to the newest N chains
  and to revoke a key. Revoking MUST stop new pushes immediately and MUST NOT be
  presented as a deletion of existing backups. Pruning MUST NOT orphan a chain the
  newest backup descends from: keeping a few more chains than requested is correct,
  leaving an incremental without its base is not (the send and the schedule would
  report success while the backup could no longer be restored).
- **FR-BUD-10** — Unknown keys, unsigned requests, stale timestamps, replayed
  requests, out-of-scope sources, tampered chunks and tampered manifests MUST each
  fail with an explicit error; a restore MUST never write unverified data.
- **FR-BUD-11** — An instance MUST be able to back up its own datasets without an
  external tool: the API MUST drive the node's `zfs send` through the agent, stream
  it (never buffering a whole dataset in memory) into the encrypted push, and MUST
  send incrementally whenever the receiver already holds the base snapshot,
  identified by its ZFS GUID rather than by name. It MUST refuse to send a dataset
  that is not mounted in the node's own mount namespace, because such a send
  captures an empty filesystem while reporting success (the pod mount-propagation
  trap).
- **FR-BUD-12** — The agent MUST expose `zfs send`/`zfs receive` as streams, and MUST
  validate every dataset and snapshot name it is given before it reaches a command
  line (`SEC-1`). Streaming calls MUST NOT be bound by the control-plane client's
  180 s timeout.
- **FR-BUD-13** — An interrupted instance-side send MUST be resumable, and safely so:
  a manifest MUST NOT be published for a stream that did not arrive whole, the resume
  MUST repeat the same stream (same snapshot, same chain, same data key) so the
  receiver can skip what it already holds, and a chain whose recorded snapshot no
  longer exists MUST be abandoned rather than guessed at.
- **FR-BUD-14** — A restore MUST rebuild the sequence a backup needs - the last full
  send followed by each incremental in order - MUST refuse a sequence whose base
  chain is missing rather than applying half a backup, and MUST offer verification
  that decrypts and hashes the stored stream without touching ZFS.
- **FR-BUD-15** — Scheduled backups: an instance MUST be able to back its datasets
  up on an interval cadence (hourly / daily / weekly with a run-at time in UTC),
  persisted across restarts. A run missed while the API was down MUST fire once on
  startup (catch-up). A run MUST be able to target several buddies at once: each
  destination is an independent chain with its own resume state, a failure at one
  MUST NOT prevent or fail the others, and the entry MUST record the outcome per
  destination (overall `ok` only when every destination succeeded). A scheduled run
  MUST apply the entry's retention (`pruneKeep`) on success and MUST notify via
  ntfy on success (`backup_success`) and on failure (`backup_failure`), gated on the
  notification settings' enabled events.
- **FR-BUD-16** — Owner UI: the `/backups` page MUST show the instance identity,
  the schedules, live progress of a manual send, the receiver status and a
  verify view. A manual send MUST run as an async job (`202` + pollable
  progress + cancellation) and MUST refuse a second send for the same
  (receiver, source) or dataset while one runs. Cancelling MUST abort an
  in-flight transfer promptly rather than waiting for the current chunk request
  to return. A restore from the UI MUST require an explicit confirmation naming
  the destination dataset.
- **FR-BUD-17** — A schedule MUST only reference a dataset that exists on the node
  when it is created (an exact match, not a pool-prefix guess), so a typo fails at
  creation rather than at the first run.

### 3.9 Dynamic DNS & DNS providers (`FR-DNS`)

DNS providers are **declarative**: a provider is described by a YAML file
(metadata, credential fields, an optional cert-manager DNS-01 solver and an
optional DDNS driver). The registry is the embedded defaults plus an optional
mounted override directory, shared by domains/certificates and Dynamic DNS. The
API keeps A/AAAA records pointed at the appliance's current public IP. See
[dynamic-dns.md](dynamic-dns.md).

- **FR-DNS-01** — Providers MUST be declared in YAML (built-in defaults plus an
  optional override directory); a provider that uses a built-in cert-manager
  solver or a built-in DDNS driver MUST require no code change. An invalid or
  unreadable override MUST be skipped with a logged error and surfaced by
  `GET /api/providers`, never fatal. *(providers tests)*
- **FR-DNS-02** — DDNS entry CRUD MUST NOT return credential values: the API
  MUST return only the Secret name and which credential fields are set. Secret
  fields submitted through the API MUST be written to a Kubernetes Secret and
  MUST NOT be persisted in the entry store. *(ddns tests, `TestDdnsNeverReturnsCredentialValues`)*
- **FR-DNS-03** — A background reconciler MUST detect the public IP on an
  interval (`DDNS_INTERVAL_SECONDS`, default 300) from an ordered list of
  sources (HTTP URLs and DNS fetchers `dns:opendns`/`dns:google`), DNS-resolve
  each enabled record, and call the provider only when the public IP is not
  already among its resolved addresses; a resolution failure MUST update rather
  than silently skip. It MUST apply a per-record update cooldown
  (`DDNS_UPDATE_COOLDOWN_SECONDS`, default 300), and record
  `lastIP`/`lastStatus`/`lastError`/`lastRunAt`/`nextRunAt`/`lastUpdateAt`. A
  detection or update failure MUST be recorded, not fatal. *(ddns tests,
  audit.sh egress assertion)*
- **FR-DNS-04** — DDNS and domain credentials MUST come from Kubernetes Secrets
  in the apps namespace, where the API already has namespaced Secret CRUD. The
  API MUST NOT be granted Secret access in the release namespace, so
  proxy/LDAP/Authelia secrets stay unreadable. *(audit.sh platform-read Role
  assertion)*
- **FR-DNS-05** — cert-manager DNS-01 solvers MUST be rendered from the
  registry (with `${secret}` / `${cred.<key>}` substitution), OVH MUST be
  supported (via the OVH cert-manager webhook, since OVH is not a cert-manager
  built-in solver), and the existing `cloudflare`, `rfc2136` and `passthrough`
  output MUST be preserved. OVH MUST support both its DynHost service
  (`mode: dynamic`, username/password, the default) and the signed ZoneDNS API
  (`mode: api`). *(certs tests: `TestSpecOVH`, `TestSpecCloudflareSolverShape`,
  `TestSpecRFC2136SolverShape`, `TestSpecPassthroughIsUnchanged`;
  `TestOVHDriverDynHostMode`)*
- **FR-DNS-06** — A force-run endpoint (`POST /api/ddns/{id}/run`) MUST exist
  and MUST update the record even when the detected IP is unchanged, bypassing
  the DNS pre-check and the cooldown.
  *(ddns tests: `TestManagerUpdatesWhenDNSDiffersAndRespectsCooldown`)*
- **FR-DNS-07** — The generic `http` driver MUST render operator-supplied
  URL/body templates, apply the configured authentication, and MUST refuse a
  target that is not a public address (loopback, private, link-local or cluster
  service ranges), so an admin-supplied URL cannot be used as an SSRF pivot.
  *(ddns tests: `TestHTTPDriverRejectsNonPublicTargets`)*
- **FR-DNS-08** — The provider set MUST include, beyond the cert-manager
  providers, the DDNS providers ported from ddns-updater (`duckdns`, `dynu`,
  `noip`, `freedns`, `namecheap`, `desec`, `spdyn`, `selfhost.de`, `dynv6`,
  `digitalocean`, `godaddy`, `porkbun`) plus the generic custom provider, each
  as YAML where a built-in driver suffices. Every built-in DDNS provider MUST
  resolve to a known driver. *(ddns test: `TestBuiltinProvidersHaveKnownDrivers`)*

- **FR-DNS-09** — App DNS records MUST be opt-in per domain:
  `appRecordsDdnsEntry` names a non-wildcard DDNS entry, and only domains whose
  provider is `ovh` or `cloudflare` may set it (otherwise 400). Every exposed
  app (non-empty `subdomain`, including `localOnly` apps) on such a domain MUST
  get a CNAME `<subdomain>.<baseDomain>` whose target is the DDNS entry's
  hostname. The API MUST write it with the domain's cert-manager credentials
  (OVH ZoneDNS keys with TTL 0 and an absolute target plus a zone refresh;
  Cloudflare `proxied:false` with TTL auto), resolve the registered zone by
  walking `baseDomain` upward, and reject a target equal to the app hostname.
  Only records the platform created (or adopted because an existing CNAME
  already had the exact target) MUST be updated or deleted; any other record at
  the name MUST be left untouched and reported as a DNS error on the app. DNS
  failures MUST NOT fail install/exposure/uninstall. The record MUST be
  reconciled after install and exposure changes, on startup, when the linked
  DDNS entry or the domain changes, on manual
  `POST /api/apps/{name}/dns/reconcile`, and retried on the catalog-refresh
  cadence while in error; deleting a DDNS entry referenced by a domain MUST be
  a 409. Uninstall, exposure removal and domain removal MUST delete owned
  records; a chart/domain update removes the old record before creating the new
  one. *(dnsrecords tests, apps tests, server tests)*

### 3.10 Installer & first-run provisioning (`FR-INSTALL`)

The desktop installer (`Naslos-Installer`, a separate repo) provisions a
freshly-booted Talos node end to end. Naslos-Linux supplies the declarative
artifact — the **install pack** — and the stable interfaces the installer
consumes; the contract is `docs/installer-contract.md`. Engine-side steps are
**[OPEN]** until the installer repo ships them (the pack and chart halves are
implemented here).

- **FR-INSTALL-01** — `make install-pack` MUST build
  `dist/naslos-install-pack-<version>.tar.gz` containing the umbrella chart
  (with vendored subcharts), `values-installer.yaml`, the machine-config
  template, the Cilium manifest, the pinned local-path manifest and the
  schematic, plus a `metadata.json` that pins `talosVersion`, `schematicId`,
  the amd64 ISO URL and a sha256 for every member. *(audit
  `check_install_pack`)*
- **FR-INSTALL-02** — The installer MUST refuse a pack whose `talosVersion` or
  `schematicId` does not match what it was built against. **[OPEN]**
- **FR-INSTALL-03** — `charts/naslos/values-installer.yaml` MUST parameterise
  the image repository base (published `ghcr.io/...`) and MUST NOT reference the
  private VM registry. `helm lint`/`helm template` with it MUST succeed.
- **FR-INSTALL-04** — The machine-config template
  (`bootstrap/installer/naslos-installer.yaml.tmpl`) MUST parameterise the
  node's IPv4 /24 and install disk, MUST NOT reference a private registry, and
  MUST keep the ZFS module, Cilium inline manifest, kube-proxy replacement and
  host-DNS settings. Its Cilium block MUST stay in step with
  `bootstrap/cilium/cilium.yaml`. *(audit `check_install_pack` /
  `render-installer-template.sh --check`)*
- **FR-INSTALL-05** — The chart MUST generate the `naslos-openldap` and
  `naslos-openldap-tls` Secrets when they do not exist and MUST preserve an
  existing Secret's bytes across upgrades (lookup-guarded, no rotation), so a
  bare `helm install` needs no out-of-band step and an upgrade does not re-key
  OpenLDAP. *(audit `check_authelia_ldap_secret`; `helm template` first-install
  render)*
- **FR-INSTALL-06** — The installer MUST create the first administrator through
  the existing owner-gated `POST /api/users`, from inside the `naslos-terminal`
  pod (reading the `naslos-proxy` Secret and sending `Remote-User` /
  `Remote-Groups` / `X-Naslos-Proxy-Secret`); it MUST NOT add a new auth
  surface. **[OPEN]**
- **FR-INSTALL-07** — After creating the admin, the installer MUST set up a
  second factor and show the `otpauth://` URI and its base32 secret. A device
  created with `authelia storage user totp generate` MUST be accepted at the
  portal without web enrolment. **[OPEN — spike: validate CLI-generated TOTP
  login on the live VM]**
- **FR-INSTALL-08** — The installer MUST write a recovery ZIP with
  `talosconfig`, `controlplane.yaml`, the Talos secrets bundle, `kubeconfig`,
  the schematic, the ISO URL + checksum and a README. It MUST NOT contain the
  admin password, and MUST warn that `/var/lib/naslos/buddy-identity.json` is
  the backup KEK and must be kept offline. **[OPEN]**
- **FR-INSTALL-09** — The installer MUST persist its state and MUST refuse to
  regenerate Talos PKI against an already-installed node (mirroring the
  `bootstrap-vm` / `WIPE_STATE` guard). **[OPEN]**
- **FR-INSTALL-10** — Adding the chosen name to the local resolver MUST be
  opt-in, MUST use OS privilege elevation, MUST rewrite only its own marked
  entry, and MUST always show the exact hosts line/DNS record as a fallback.
  **[OPEN]**
- **FR-INSTALL-11** — The engine MUST define all command arguments itself
  (Talos is immutable and has no shell) and MUST NOT expose arbitrary shell
  input to the node (SEC-1). **[OPEN]**
- **FR-INSTALL-12** — The engine MUST stream step progress as newline-delimited
  JSON on stdout, ending in a `done` or `error` event; the shell MUST render a
  progress bar and MUST be able to cancel by killing the child. **[OPEN]**
- **FR-INSTALL-13** — a released pack (`vX.Y.Z`) MUST pin each of the 8 chart
  images (`api`, `agent`, `ui`, `samba`, `nfs`, `terminal`, `openldap`, `vmd`)
  by digest
  to the public registry (`ghcr.io/aessemops/naslos-*`), and the tag workflow
  MUST publish those images before it builds the pack, so a pack never
  references an unpublished image. The digest MUST win over the tag (VER-2), and
  the pack build MUST fail closed when a chart image has no digest. *(audit
  `check_image_digest_render` / `check_pack_digest_pinning`; workflow
  `.github/workflows/release.yml`)*

### 3.11 System upgrades (`FR-UPD`)

A node installed from an installer pack can be upgraded from the web UI. A
`make install-vm` deployment is explicitly not web-upgradable.

- **FR-UPD-01** — The pack's `values-installer.yaml` MUST carry
  `release.{profile,naslosVersion,chartVersion,talosVersion,schematicId,kubernetesVersion}`
  and `metadata.json` MUST carry `kubernetesVersion`. The chart MUST render the
  facts as API env vars. Without `release.profile == "installer"` the API MUST
  report `upgradable: false` and MUST refuse every operation (`412`).
- **FR-UPD-02** — The API MUST check the newest strict `vX.Y.Z` GitHub release
  (or a configured mirror), MUST download the pack and its `.sha256`, MUST
  verify the tarball sha256 and every member checksum, and MUST cache nothing
  that fails. The source is `system.updates.source`.
- **FR-UPD-03** — The API MUST offer the newest release whose `talosVersion` and
  `kubernetesVersion` are each at most one minor ahead of the running versions,
  MUST name the blocking reason otherwise, and MUST block a Naslos upgrade while
  the node's Talos/Kubernetes differ from the current release's pins.
- **FR-UPD-04** — Operations MUST run in order **Naslos → Talos → Kubernetes**,
  one at a time, each pinned by the installed release. The API MUST return
  `409` while an operation, app job or Buddy job is running, and `412` when a
  precondition fails (profile, order, stepping, pool health). When the node
  already runs the newest release but Talos or Kubernetes still lag that
  release's pins, the API MUST still offer the installed release as the
  candidate so the remaining ordered steps can be completed.
- **FR-UPD-05** — Each operation MUST be an in-cluster Job running
  `naslos-upgrade` with ServiceAccount `naslos-upgrader`; the Naslos operation
  MUST run the image pinned by the target pack, while the Talos and Kubernetes
  operations use the current image. The Job MUST take an etcd snapshot before
  changing the node.
- **FR-UPD-06** — A Naslos upgrade MUST rebuild only the installer's override
  keys from the current release, MUST `helm upgrade --reset-values` from the
  new pack, MUST verify the running images against the pack's digests, and MUST
  roll back to the previous Helm revision on failure. A Talos upgrade MUST
  detect the A/B rollback after reboot. A Kubernetes upgrade MUST dry-run first
  and report that it has no automatic rollback.
- **FR-UPD-07** — A `ValidatingAdmissionPolicy` MUST reject any pod using the
  `naslos-upgrader` ServiceAccount unless every container runs a
  `ghcr.io/aessemops/naslos-api@sha256:` image with the `naslos-upgrade`
  entrypoint.
- **FR-UPD-08** — The status/lock MUST live in the
  `naslos-system-operation` ConfigMap (`phase`, `steps`, timestamps, result)
  and MUST survive API restarts and node reboots. The API's RBAC MUST be
  limited to the operation Jobs, their pods/logs and that ConfigMap.
- **FR-UPD-09** — The API MUST expose admin-only `GET /api/system/version`,
  `GET|POST /api/system/updates[/check]`,
  `POST /api/system/operations` and `GET /api/system/operations/current`.
- **FR-UPD-10** — The UI MUST show the version table, the available release and
  notes link, the three ordered actions with disabled reasons, typed-version
  confirmation dialogs (the Talos one warning about downtime), a progress panel
  that keeps polling through the reboot, and an update badge.

### 3.12 Host network configuration (`FR-NET`)

Day-2 host networking is edited through the API and the UI, not by hand-editing
the machine config. Talos remains the single source of truth; the API persists
only the in-flight change and the previous config snapshot.

- **FR-NET-01** — The API MUST expose the node's inventory on
  `GET /api/network`: hostname (value and source), resolver mode (inherited or
  explicit), every physical (`ether`) link with its live addresses, MTU, speed,
  duplex, ring/WOL features, and the effective per-link addressing, routes and
  MTU from the machine config. Loopback, VLAN, bond and bridge links are not
  editable.
- **FR-NET-02** — A change MUST be validated server-side before any patch is
  written: the link must be an existing physical link; a static change needs
  1–4 unicast, non-loopback, non-link-local, non-multicast, non-unspecified CIDR
  addresses with a valid prefix; a gateway must be unicast, inside one of the
  configured subnets and distinct from every configured address; MTU must be
  576–9216 and below 1280 only with explicit confirmation; DNS lists are 1–3
  nameservers and at most 6 DNS-1123 search domains. The API MUST return `409`
  while another change is pending or a system upgrade operation is running.
- **FR-NET-03** — A change MUST be applied with `talosctl patch machineconfig
  --mode=try` after a certSANs union patch, verified at the target address,
  confirmed with a genuinely different certSANs update, and reverted to the
  saved full config if it does not verify inside the guard window. A pending
  record MUST survive an API restart: on boot the watchdog either continues or
  reverts it, and MUST never leave a dead pending record that blocks later
  changes. An operator revert MUST win over an in-flight confirmation.
- **FR-NET-04** — The node hostname MUST be read-only in this surface, with the
  install-time explanation that the stable auto hostname stays authoritative and
  that renaming would break local-path volume node affinity.
- **FR-NET-05** — The install path MUST stay DHCP with an auto hostname. Static
  addressing, custom DNS and MTU are runtime day-2 operations, not installer
  fields.
- **FR-NET-06** — A static address outside the current kubelet subnets MUST be
  added to `KubeNodeConfig.nodeIP.validSubnets` in the same change. A legacy
  `machine.network.interfaces` entry for the edited link MUST be removed in
  the same patch. `ResolverConfig.hostDNS` MUST be preserved by every resolver
  patch. After an MTU change is confirmed the API MUST restart the CNI so it
  re-detects the interface MTU.

### 3.13 Virtual machines (`FR-VMT`)

Naslos 0.4 ships a first-party hypervisor: a QEMU/KVM supervisor pod
(`naslos-vmd`) reconciles VMs on the node while the API stays the brain —
the exact shares pattern: the API validates, persists records
(`vms.json` on the state PVC) and pushes the whole desired state; vmd
diffs it against the running QEMU processes and starts/stops/recreates as
needed. The agent remains the only ZFS writer: VM datasets
(`<pool>/vms/<name>`) are created/destroyed through the existing agent
dataset APIs, so `make install-vm` on a clean node reproduces everything.
KubeVirt was rejected for this (Kubernetes 1.37 vs KubeVirt support skew,
bare-metal K8s required) and libvirt/Incus for daemon weight; the supervisor
is ~3k lines of audited Rust (see `.kilo/plans/…-qemu-hypervisor.md`).

- **FR-VMT-01** — The node's virtualization capabilities MUST be reported on
  `GET /api/virt/capabilities`: KVM availability, CPU model, nested
  virtualization and the physical NIC list. The UI MUST surface a
  missing-KVM banner; a pre-0.4 node (no supervisor) must render the page with
  a clear message, never a crash.
- **FR-VMT-02** — VM creation MUST be validated server-side before any host
  change: DNS-1123 name, pool existence, vCPU/memory/disk ranges, unique
  host ports across all VMs, firmware/disk-format/bus enums, the NAT-only
  network mode, library references (`isos/<file>`), and cloud-init fields
  (hostname, POSIX user, ≤20 SSH public keys, no passwords). vmd MUST
  re-validate the same rules before building any QEMU command line (SEC-1: no
  unvalidated string ever reaches `exec`).
- **FR-VMT-03** — A VM's dataset MUST be a child of its pool
  (`<pool>/vms/<name>`) with `quota` equal to the declared disk size, created
  through the agent; the first start creates the disk image blank (`qcow2`
  sparse by default, `raw` optional) or seeds it from a library cloud image
  and resizes it to the declared size. Deleting a VM MUST refuse while it is
  running and MUST destroy the dataset only after the record is removed,
  restoring the record if the destroy fails.
- **FR-VMT-04** — Networking v1 is NAT (slirp) with per-VM editable TCP port
  forwards exposed on the node; that is the only mode. A LAN mode that gives
  the guest its own DHCP address is **[OPEN]**: the planned path is a Talos
  `BridgeConfig` host bridge (the sanctioned KubeVirt-on-Talos pattern),
  which re-addresses the node's primary interface and is therefore a separate
  release; no Talos machine-config change is made today. A future implementer
  MUST NOT revive the removed macvtap path unchanged: its capability probe
  created a throwaway macvtap linked to `lo` (a false negative on kernels
  that do have macvtap), and the device was created without the guest's
  stable MAC, so macvtap's ingress filter dropped unicast to the guest.
- **FR-VMT-05** — Boot media: an install ISO attaches as a CD-ROM from the
  per-pool image library (`<pool>/vms/isos`). UEFI (OVMF, with a per-VM
  writable vars copy) is the default firmware; BIOS (SeaBIOS) remains a
  select option for legacy guests. The `sata` bus option maps to the q35
  built-in AHCI controller (Windows guests without virtio drivers).
- **FR-VMT-06** — cloud-init (NoCloud): the seed MUST carry only hostname,
  user and SSH public keys; password authentication MUST be disabled in the
  seed (`ssh_pwauth: false`, `lock_passwd: true`). Seeds are regenerated on
  every start; the instance-id changes with the seed content so an edited
  seed re-runs on the next boot.
- **FR-VMT-07** — The image library MUST support (a) browser upload in
  chunks, relays to the supervisor without landing the file in API memory,
  (b) server-side URL import restricted to public http(s) targets — the
  FR-DNS-07 SSRF guard (redirects re-validated hop by hop, connection pinned
  to the vetted address), optional sha256 verification — and (c) delete.
  The library lives at `<pool>/vms/isos`, flat file names only.
- **FR-VMT-08** — The console MUST be reachable from the browser:
  `GET /api/ws/vm/{name}/vnc` (plain GET) is a preflight returning the VNC
  port, the per-boot VNC password and `ready to attach`; the same route with
  a websocket upgrade bridges RFB bytes to QEMU's VNC listener (noVNC
  client, `@novnc/novnc`). The bridge MUST enforce the `/api/ws/exec`
  posture: same-origin check, finite session caps (max session, idle
  timeout, concurrent session semaphore). The credentials MUST appear only
  in the preflight response, never in list/detail payloads.
- **FR-VMT-09** — Snapshots MUST be ordinary ZFS snapshots of the VM dataset
  (created/listed via the agent, crash-consistent while the VM runs, backed
  up by Buddy like any dataset). Restore MUST refuse a running VM and MUST
  roll the dataset back (`zfs rollback -r`, deleting newer snapshots).
- **FR-VMT-10** — `state` MUST be derived live from the supervisor, never
  persisted. The API MUST push the desired config at its own startup with
  `desired=running` for autostart-marked VMs (pod restart = VM restart for
  autostart VMs), and the supervisor MUST wait (bounded) for a fresh VM's
  dataset mountpoint to appear before starting (the mount-propagation trap).
  Failed starts and unexpected exits MUST raise ntfy events (`vm_failed`,
  `vm_exited_unexpected`, notifications the owner enables explicitly).
- **FR-VMT-11** — The console channel and the supervisor API MUST be
  authenticated and TLS-protected exactly like the agent (NAS-002/PF-M5):
  chart-generated token Secret, chart-generated CA + server certificate,
  bearer-token check on every route but the health probe, no plaintext
  opt-out short of an explicit `VMD_ALLOW_PLAINTEXT=1` for development. The
  QEMU VNC listener MUST be password-protected (the password is set over
  QMP per boot and handed to the browser only through the owner-gated
  preflight).

### 3.14 MCP server (`FR-MCP`)

Naslos 0.4.4 ships a first-party MCP (Model Context Protocol) endpoint: a
Streamable HTTP server **inside the existing `naslos-api` pod** — no new
image, Service, deployment or NetworkPolicy — that exposes the API's
owner routes to AI assistants and other programmatic clients through
scoped bearer tokens. Traefik routes `Host(apex) && PathPrefix('/mcp')`
to the API without the Authelia forwardAuth (a programmatic client cannot
complete an interactive 2FA login); the Authelia bypass lives next to the
buddy bypass, and the API's own guard keeps SEC-10 in force. See
`docs/mcp.md` for the operator guide.

- **FR-MCP-01** — The endpoint MUST be off by default: state persists in
  `mcp.json` on the state PVC (`MCP_CONFIG`), the admin toggle is
  `PUT /api/mcp {enabled}`, and a disabled install answers `/mcp` with 404
  so the surface is not advertised. `mcp.available` in the chart only gates
  routing and the bypass (a chart-level kill switch for locked-down
  installs).
- **FR-MCP-02** — Tokens: `POST /api/mcp/tokens` (owner route, SEC-10 +
  admin) creates a token `{name, scope, expiresInDays?}`; the response
  shows the plaintext exactly once and every other view (Settings UI,
  `GET /api/mcp`) omits both the plaintext and the hash. Tokens verify by
  SHA-256 hash with constant-time comparison, may expire, and revoke via
  `DELETE /api/mcp/tokens/{id}`. New tokens default to the `read` scope:
  a `full` token is an administrator without 2FA. *(SEC-17)*
- **FR-MCP-03** — Scopes: every tool declares `read` < `write` < `full`.
  `tools/list` MUST only list the tools the calling token's scope allows,
  and `tools/call` MUST re-check the scope (a client can call an unlisted
  tool). Refusals return tool errors the caller's LLM can read, never a
  protocol-level failure.
- **FR-MCP-04** — Destructive (full-scope) tools MUST require a `confirm`
  argument equal to the resolved target (the same resolved value used in
  the path or query, never a second lookup), checked before any dispatch.
  The `initialize` instructions MUST state the rule.
- **FR-MCP-05** — Tool dispatch MUST call the real handlers through the
  same router surface `build_router` mounts (split into `owner_routes()`
  and `user_routes()`), with a synthetic `mcp:<token>` admin identity
  injected per call — the internal router is never reachable from the
  HTTP listener. Tools get API behavior for free: validation, JSON error
  bodies, and 503 for absent subsystems; the registry stays a declarative
  table plus one dispatcher. *(contract tests:
  `every_registry_tool_matches_a_real_route`,
  `every_route_answers_401_without_the_proxy_secret`,
  `every_owner_route_is_403_for_non_admins`,
  `internal_router_answers_with_a_synthetic_identity_and_hides_public_routes`)*
- **FR-MCP-06** — The server MUST be stateless (Streamable HTTP,
  `json_response` preferred, no session store): one API replica today, but
  a stateful session would silently break the day a second replica
  exists. Resources and prompts are out of scope for v1; `tools` is the
  only advertised capability.
- **FR-MCP-07** — Every `tools/call` MUST be audited: a structured log
  line (`mcp_tool_call`, token name, tool, scope, status, duration) plus
  an in-memory per-token ring buffer (newest 50 across tokens) surfaced at
  `GET /api/mcp`. Arguments are NEVER logged or recorded — tool bodies can
  carry passwords, secrets and tokens.

---

## 4. API contracts (normative)

### 4.1 Conventions

- **API-01** — All `naslos-api` routes live under `/api/`; agent routes under
  `/api/v1/`.
- **API-02** — Errors MUST use `{"error": "<message>"}` with a proper HTTP
  status (400 invalid input, 404 unknown, 405 wrong method, 503 dependency
  unavailable). Unknown `/api/*` paths remain behind the auth gate. *(contract
  tests: `unknown_routes_return_json_404`, `a_wrong_method_returns_json_405`)*
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
| `/api/users/{uid}` | GET, PUT, DELETE | Detail / update fields and group membership / delete |
| `/api/users/{uid}/password` | POST | Change password (updates Samba hash too) |
| `/api/users/{uid}/enable` `/disable` | POST | `shadowExpire` toggling |
| `/api/groups` | GET, POST | List (array) / create (description optional) |
| `/api/groups/{cn}` | GET, PUT, DELETE | Detail / update members / delete |
| `/api/catalog` | GET | Available apps |
| `/api/catalog/{app}` | GET | App detail + JSON-Schema form |
| `/api/apps` | GET | Installed apps |
| `/api/apps/{app}` | GET, PUT, DELETE | Detail / configure / uninstall |
| `/api/providers` | GET | Declarative DNS providers + override load errors (§3.9) |
| `/api/ddns` | GET, POST | Dynamic-DNS entries (credentials never returned) / create (§3.9) |
| `/api/ddns/{id}` | GET, PUT, DELETE | Entry detail / update / delete (deletes its credential Secret) |
| `/api/ddns/{id}/run` | POST | Force one reconcile even when the IP is unchanged |
| `/api/system/version` | GET | Running Naslos/chart/Talos/Kubernetes, release pins, profile, `upgradable` (§3.11) |
| `/api/system/updates` | GET | Cached update check, candidate, stepping decision |
| `/api/system/updates/check` | POST | Run an update check now |
| `/api/system/operations` | POST | Start an ordered upgrade operation (`202`; `409` lock; `412` precondition) |
| `/api/system/operations/current` | GET | Operation status + Job log tail |
| `/api/disks` | GET | Node disks |
| `/api/disks/health` | GET | Per-disk SMART + ZFS health (FR-DHK) |
| `/api/disks/recommend` | POST | Topology advisor |
| `/api/volumes/zfs` | GET | Pools (live via agent) |
| `/api/volumes/zfs/import` | POST | Import existing pool |
| `/api/volumes/zfs/{pool}` | GET, DELETE | Pool health / destroy |
| `/api/shares` | GET, POST | List / create |
| `/api/shares/{name}` | PUT, DELETE | Update / delete |
| `/api/shares/paths` | GET | Shareable dataset paths + ZFS base |
| `/api/shares/status` | GET | Rendered revision + what the node has applied |
| `/api/virt/capabilities` | GET | Node virtualization preflight (FR-VMT-01) |
| `/api/virt/vms` | GET, POST | VM records merged with live state / create (dataset + record + push) |
| `/api/virt/vms/{name}` | GET, PUT, DELETE | Detail / update / delete (refuses a running VM) |
| `/api/virt/vms/{name}/start` `stop` `force-stop` `restart` | POST | Lifecycle actions (stop = graceful ACPI; restart = force-stop + start) |
| `/api/virt/vms/{name}/snapshots` | GET, POST | List / create a ZFS snapshot of the VM dataset |
| `/api/virt/vms/{name}/snapshots/{snap}/restore` | POST | `zfs rollback -r`; refuses a running VM |
| `/api/virt/images?pool=` | GET | Per-pool library listing (imports + last errors included) |
| `/api/virt/images/upload?pool=&name=&offset=&final=` | POST | One upload chunk (`application/octet-stream`, the only non-JSON body) |
| `/api/virt/images/import` | POST | Server-side URL import (202; SSRF-guarded in the supervisor) |
| `/api/virt/images/{name}?pool=` | DELETE | Delete a library file |
| `/api/ws/vm/{name}/vnc` | GET | Console preflight (plain) / RFB bridge (websocket upgrade) |
| `/api/shares/apply` | POST | Re-render and push the configuration to the node |
| `/api/shares/config/samba` `/nfs` | GET | Generated configs |
| `/api/notifications` | GET | Notification settings/state |
| `/api/notifications/test` | POST | Send test push |
| `/api/network` | GET | Host network inventory: hostname, resolvers and physical links (§3.12) |
| `/api/network/changes` | POST | Start a guarded network change (`202`; `409` while pending or during an upgrade) |
| `/api/network/changes/{id}` | GET | Change phase, target address, deadline and error |
| `/api/network/changes/{id}/revert` | POST | Apply the saved previous config now (`202`) |
| `/api/buddy/v1/status` `/backups` | GET | Receiver report to a peer: free space, stored bytes, quota, sources, last backup (§3.8) |
| `/api/buddy/v1/chains/{source}` | GET | Every stored chain of a source, newest first (a restore replays them in order) |
| `/api/buddy/v1/chunks/{source}` | GET, PUT | List / fetch / upload one sealed chunk (`?chain=&index=`) |
| `/api/buddy/v1/manifest/{source}` | GET, PUT | Fetch / publish the signed manifest of a chain |
| `/api/buddy/v1/prune/{source}` | POST | Keep the newest N chains |
| `/api/buddy/v1/enroll` | POST | Single-use token bootstrap: authorize a new peer key (the only unsigned buddy route) |
| `/api/buddy/status` | GET | Owner view: free space, peers, stored backups |
| `/api/buddy/peers` | GET, POST, DELETE | List / authorize / revoke peer keys (authenticated session required) |
| `/api/buddy/identity` | GET, POST | This instance's key material: report the public key / create it (`replace` required to overwrite) |
| `/api/buddy/send` | POST | Back a local dataset up to a buddy: `202 {"jobId"}` immediately, then snapshot, incremental by GUID, stream `zfs send` into the push under a server-owned context |
| `/api/buddy/jobs` | GET | List async send jobs (running + recent finished) |
| `/api/buddy/jobs/{id}` | GET, DELETE | Job detail incl. progress / cancel a running send (resume state is kept) |
| `/api/buddy/schedules` | GET, POST, DELETE | Scheduled backups: list / create-or-update / delete (`?id=`) |
| `/api/buddy/restore` | POST | Restore a buddy's backup into a local dataset (whole chain sequence), or `verify` to hash it without touching ZFS |
| `/api/metrics` | GET | Live `SystemMetrics` (§3.5) |
| `/api/dashboard` | GET | Dashboard view model + live pools |
| `/api/mcp` | GET, PUT | MCP endpoint state (enabled, endpointUrl, tokens without hashes, recent calls) / toggle (§3.14) |
| `/api/mcp/tokens` | POST | Create a scoped bearer token; the plaintext is shown once (§3.14, SEC-17) |
| `/api/mcp/tokens/{id}` | DELETE | Revoke a token |
| `/mcp` | POST, GET | The MCP endpoint itself: Streamable HTTP, bearer-token guarded, no Authelia (§3.14) |
| `/api/ws/logs` `/api/ws/exec` | WS | Streams (§3.6) |
| `/*` | GET | UI static assets (legacy `/var/naslos/ui` path; the served UI is the `naslos-ui` deployment) |

### 4.3 `naslos-agent` routes (host-level)

| Route | Methods | Notes |
| --- | --- | --- |
| `/health` | GET | Liveness |
| `/api/v1/pools` | GET | List pools (via `zpool list`) |
| `/api/v1/pools/import` | POST | `zpool import` |
| `/api/v1/pools/{name}` | GET, DELETE | Status/iostat / destroy |
| `/api/v1/disks/health` | GET | Per-disk SMART + ZFS health (FR-DHK) |
| `/api/v1/datasets/{pool}` | GET, POST | List / create datasets |
| `/api/v1/snapshots/{pool}` | GET, POST | List / create snapshots |
| `/api/v1/shares/config` | PUT | Write the API-rendered share config to the host (atomic) |
| `/api/v1/shares/status` | GET | Applied revision + rendered share counts |

---

## 5. Data models (normative JSON)

- **DM-1 Person**: `{ "uid", "displayName", "email", "uidNumber", "enabled": bool, "groups": ["short-name", …] }`
- **DM-2 Group**: `{ "cn", "description?", "members": ["uid", …] }` — `members` MUST be an array (possibly empty), `description` omitted when empty.
- **DM-3 Pool**: `{ "name", "size", "alloc", "free", "usagePercent", "health" }` — sizes in bytes, parsed from human `zpool list` output.
- **DM-4 Share**: `{ "name", "path", "protocol": "smb"|"nfs", "description", "readOnly": bool, "browseable": bool, "allowedHosts": [], "validUsers": [], "validGroups": [], "timeMachine": bool, "createdAt", "enabled": bool }` — `path` MUST be inside the ZFS base; the three list fields MUST be arrays (never `null`, including when read back from disk); `validUsers` entries render bare and `validGroups` entries render as `@group` in `smb.conf` (FR-SHR-07/12).
- **DM-5 SystemMetrics**: see FR-MET-03. CPU also carries `model`, `temperatureC` (nullable), and `perCore[]` (`core`, `usagePercent`, optional `frequencyMhz`). `updatedAt` is RFC 3339; before the first successful collection it is the Go zero time and consumers MUST treat it as "no data yet".
- **DM-6 DiskHealth**: `{ "smartctlAvailable": bool, "error": string, "temperatureWarningC": number, "wearWarningPercent": number, "disks": [{ "device", "size", "isSystemDisk", "model", "serial", "busPath", "type", "signatures": [], "inPool"?, "smart": { "available", "enabled", "health": "PASSED"|"FAILED"|"UNKNOWN", "temperatureC"?, "powerOnHours"?, "wearPercent"?, "reallocatedSectors"?, "pendingSectors"?, "uncorrectableSectors"?, "mediaErrors"?, "errorCount"?, "standby", "message" }, "zfs"?: { "pool", "vdev", "state", "readErrors", "writeErrors", "cksumErrors" } }] }` — `smart.available:false` is normal for virtio/USB/RAID devices and MUST NOT fail the endpoint (FR-DHK-02/03).
- **DM-7 Vm**: `{ "name", "pool", "cpu", "memMiB", "autostart": bool, "firmware": "uefi"|"bios", "desired": "running"|"stopped", "disk": { "sizeGB", "format": "qcow2"|"raw", "bus": "virtio"|"sata", "sourceImage"?: "isos/<file>" }, "net": { "mode": "nat"|"lan", "forwards": [{ "host", "dst" }], "nic"? }, "cloudInit"?: { "hostname", "user", "sshKeys": [] }, "iso"?: "isos/<file>" | null }`. `name` is a DNS-1123 label; live fields (`state`: "running"|"stopped"|"unknown", `pid`, `cpuPercent`, `memUsedMiB`, `unexpectedExit`, `lastError`) are merged in responses and MUST NOT be persisted; the console credentials (`vncPort`, `vncPassword`) appear only in the `/api/ws/vm/{name}/vnc` preflight, never in list or detail payloads (FR-VMT).

---

## 6. Non-functional requirements

- **NFR-1 Performance** — Dashboard data MUST be at most ~5 s stale
  (FR-MET-02/08). UI auto-refresh MUST NOT stack requests.
- **NFR-2 Talos compatibility** — Talos upgrades MUST remain clean; Naslos
  MUST NOT modify the Talos base image.
- **NFR-3 Security** — see §2.2 (SEC-1…SEC-14).
- **NFR-4 Resilience** — The API MUST return usable (non-crashing) responses
  when a dependency is down: LDAP failures yield `503` with the underlying
  cause (FR-IDN-11) while non-identity features keep working (FR-IDN-12); the
  UI MUST show errors, never silently freeze. A component MUST also survive a
  **node reboot**: startup MUST NOT depend on another component already being
  resolvable, or it will crash-loop in the race (nginx resolving the API host
  name at boot was exactly that failure, so the UI waits for the name first).
- **NFR-5 Footprint** — UI assets MUST be minimized (e.g. the logo ships as a
  128×128 PNG, not the 1254×1254 original).

---

## 7. Acceptance criteria & verification

> **Status note (2026-10-24).** Rows below that name `Test…` functions refer to
> the deleted Go suite; the Rust equivalents live in `rust/api/tests/` and the
> crate unit tests, and the mapping (including the rows whose test did not
> survive the port) is in `docs/AUDIT-2026-10-24.md`. The Playwright rows are
> still authoritative.

The authoritative executable acceptance suite is the Playwright suite in
`ui/tests/` against a live deployment:

| Test | Verifies |
| --- | --- |
| `dashboard.spec.ts` — live node metrics | FR-MET-03/04/05, FR-IDN-08 (live values, non-zero memory/cores, no zero-time) |
| `dashboard.spec.ts` — zfs pools | FR-STO-06, FR-MET-07 |
| `disk-health.spec.ts` — per-disk inventory | FR-DHK-02/03/04 (API/UI shape, SMART-unavailable path) |
| `dashboard.spec.ts` — auto-refresh | FR-MET-08 (5 s re-render without reload) |
| `dashboard.spec.ts` — CPU detail | FR-MET-11/12 (model, load averages, one bar per core, unavailable-temperature note) |
| `TestAddressesByLink`, `TestAddressesByLinkPrefersIPv4OverIPv6` | FR-MET-10 (routable IPv4 over IPv6, no loopback/link-local, no address for an unnamed link) |
| Live dashboard shows each interface with its IP (`eth0` → the node's address) | FR-MET-10 |
| `users.spec.ts` | FR-IDN-02, FR-IDN-08, **FR-IDN-14** (the edit dialog states the share timing and the stale-session caveat) |
| `groups.spec.ts` (both) | FR-IDN-06, FR-IDN-08 |
| `e2e.spec.ts` — full lifecycle | FR-IDN-01…05, FR-STO-07 (group add-member), delete paths |
| `shares.spec.ts` — smb:// address | FR-SHR-09 |
| `shares.spec.ts` — group restriction + AFP absent | FR-SHR-07, FR-SHR-02 (no AFP offered) |
| `logo.spec.ts` | NFR-5, asset served as real PNG (SPA-fallback guard) |
| `network.spec.ts` — inventory and client validation | FR-NET-01/02 (hostname, resolver and link shape; invalid CIDR/gateway blocked in the browser with no POST) |
| `network.spec.ts` — hub navigation | The Settings → Network card reaches `/settings/network` |
| `mcp.spec.ts` — MCP settings | FR-MCP-01/02/03/07 (disabled-by-default view, token create/reveal-once/revoke flow, scope choice, recent-calls table; leaves the endpoint disabled and no tokens behind) |

Share serving, the account mirror and group access involve the node, so they
are additionally verified against the live VM (not by Playwright):

| Check | Verifies |
| --- | --- |
| `pdbedit -L`/`getent passwd` inside `naslos-samba` | FR-IDN-13 (a directory user with no node account resolves) |
| SMB login + read/write with a directory user's password | FR-IDN-13, FR-SHR-06 |
| Disable → `NT_STATUS_ACCOUNT_DISABLED`; enable → works again | FR-IDN-15 |
| Password change → new session accepted, timed | FR-IDN-14 (2.6–3.0 s at interval 3; 0.8–1.1 s at 1) |
| 8 changes ~0.4 s apart → final password wins | FR-IDN-16 |
| Group-restricted share: member OK / non-member refused | FR-SHR-07 |
| Remove member → revoked; add outsider → granted | FR-SHR-07, FR-SHR-11 |
| `POST /api/shares` on a non-dataset path → 400; `/api/shares/paths` lists datasets only | FR-SHR-01 |
| SMB write lands in the pool (`talosctl ls <dataset>`) and survives a pod restart | FR-SHR-01, FR-SHR-04 |
| `naslos-samba` logs each share path with its backing mount | FR-SHR-01 |
| `avahi-browse -rt _smb._tcp` lists the server at the LAN address | FR-SHR-08 |
| `netbios name` in `smb.conf` equals the advertised discovery name | FR-SHR-10 |

Rust tests cover the parts that need no node: `rust/api/src/identity.rs`
(NT-hash known vectors against OpenSSL-computed values);
`rust/api/src/shares.rs` (smbpasswd and
extrausers rendering, stable group GIDs, access-list groups, NetBIOS name
sanitising); `rust/api/src/shares/acl.rs` (fixed POSIX ACL presets, mode/entry
mapping and the service-account lockout guard, FR-ACL-01/02);
`rust/api/src/server/` (the user-edit membership diff, FR-IDN-04;
the write-only ntfy token, `TestNotificationsNeverReturnTheAuthToken`); and
`rust/agent/src/shares/` (the 0600 passdb/shadow mirrors, FR-IDN-13).

Buddy Backup is verified by the Rust suites in `rust/api/src/buddy/` and
`rust/api/tests/`, which run the real receiver over HTTP against the real
client (the case names below are the original Go suite's):

| Test | Verifies |
| --- | --- |
| `TestIdentityRoundTrip` | FR-BUD-03 (OpenSSH-form key, 0600 on disk, signatures verify with the public key only) |
| `TestEnvelopeRejectsTampering` | FR-BUD-04 (bit flip, truncation, wrong index, wrong source, wrong chain all fail) |
| `TestDEKWrapNeedsTheOwnersKEK` | FR-BUD-02 (a stranger's KEK cannot unwrap a chain; the wrapped key is chain-bound) |
| `TestManifestSignature` | FR-BUD-06 (edited metadata invalidates the signature; another key cannot sign for it) |
| `TestPushRestoreRoundTrip` | FR-BUD-02/06/07 (byte-identical restore, `unknown`/free space/last backup, and the receiver's files contain no plaintext) |
| `TestPushResumeUploadsOnlyMissingChunks` | FR-BUD-05 (killed push resumes: 2 skipped, 1 uploaded; a changed source is refused) |
| `TestReceiverRefusesTamperedChunk` | FR-BUD-10 (corrupted ciphertext fails authentication instead of restoring) |
| `TestScopeAndQuota` | FR-BUD-08 (out-of-scope source 403, quota 413) |
| `TestRequestAuthentication` | FR-BUD-03 (unsigned 401, unknown key 401, stale timestamp 401, replayed nonce 401) |
| `TestEnrollAuthorizesTheFirstKey` | FR-BUD-03 (single-use token; closed receivers refuse enrollment) |
| `TestPruneKeepsNewestChains` | FR-BUD-09 (prune removes the old chains, keeps the current one restorable) |
| `TestPruneNeverOrphansAnIncremental` | FR-BUD-09 (keep=1 on an incremental keeps its base; the sequence still restores) |
| `TestPushAbortsWhenTheContextIsCancelled` | FR-BUD-16 (cancelling aborts an in-flight chunk instead of waiting for the receiver) |
| `TestBuddyScheduleFanOutToSeveralBuddies` | FR-BUD-15 (two destinations get a job each, a dead one fails only itself, per-destination results recorded) |
| `TestBuddyScheduleReceiversValidation` | FR-BUD-15 (at least one receiver, every URL valid, duplicates collapsed) |
| `TestBuddySendRefusesADatasetTheHostCannotSee` / `TestBuddySendAllowsADatasetWithNoMountpoint` | FR-BUD-11 (a dataset mounted only inside a pod is refused before any snapshot; mountpoint none stays sendable) |

Dynamic DNS and the provider registry are verified by
`rust/api/src/providers.rs`, `rust/api/src/ddns.rs` and
`rust/api/src/certs.rs`:

| Test | Verifies |
| --- | --- |
| `TestLoadBuiltins`, `TestOverrideReplacesBuiltinAndSkipsInvalid` | FR-DNS-01 (YAML providers, override merge, invalid file skipped and recorded) |
| `TestOVHSolverShape`, `TestSolverMissingConfigIsAnError` | FR-DNS-01/05 (solver rendering and substitution) |
| `TestSpecOVH`, `TestSpecCloudflareSolverShape`, `TestSpecRFC2136SolverShape`, `TestSpecPassthroughIsUnchanged` | FR-APP-13, FR-DNS-05 (OVH supported; existing solvers byte-identical) |
| `apps::value_schema_tests::pool_storage_class_is_safe_and_case_aware`, `persistence_storage_class_covers_top_level_and_nested` | FR-APP-21 (pool StorageClass naming and persistence overrides) |
| `apps::tests::update_available_compares_versions`, `apps::tests::enrich_view_uses_catalog_and_live_version`, `apps::tests::enrich_view_reports_image_drift_separately_from_a_chart_update` | FR-APP-22 (update signal, catalog identity, orphan/empty rules) |
| `images::tests::*`, `apps::tests::image_pull_patch_targets_only_the_drifted_container` | FR-APP-27 (reference parsing, index/platform digest matching, realm validation, digest cache, targeted re-pull patch) |
| `app_usage::tests::*` | FR-APP-22 (summary parsing, rate baselines, pod→app attribution, aggregation) |
| `shares::acl::tests::*` | FR-ACL-01/02 (fixed preset allow-list, POSIX mode/entry mapping, no world-write, owner/group/other resolution, service-account lockout refusal) |
| `apps::dataset_layout_tests::*` | FR-APP-20 (selection normalisation, one claim/hostPath entry per dataset, client value stripping, chart support scan) |
| `apps::resource_quantity_tests::*` | FR-APP-25 (flat and per-component resources, quantity grammar, non-quantity types, unrelated keys) |
| `TestManagerUpdateSkipsUnchangedAndHidesSecrets`, `TestDdnsNeverReturnsCredentialValues` | FR-DNS-02/03/06 (skip-when-unchanged, force-run, no credential values) |
| `TestManagerRecordsCredentialErrors`, `TestDetectIPValidatesResponse` | FR-DNS-03 (errors recorded, IP response validated) |
| `TestOVHDriverUpdate`, `TestCloudflareDriverCreateAndUpdate`, `TestHTTPDriverRendersAndAuthenticates` | FR-DNS-01 (drivers: URL/method/body/signature/auth) |
| `TestHTTPDriverRejectsNonPublicTargets` | FR-DNS-07 (SSRF guard) |
| `helm lint` + `scripts/audit.sh` DDNS assertions | FR-DNS-03/04 (env wired, egress port-scoped, no release-namespace Secret access) |
| `naslos_vmd::model::tests::*` | FR-VMT-02 (name/range/enum/cloud-init validation, library traversal guards) |
| `naslos_vmd::args::tests::*` (golden argv) | FR-VMT-02/05 (SEC-1: the exact QEMU command line is a pinned, reviewable diff) |
| `naslos_vmd::qmp::tests::*`, `naslos_vmd::supervisor::tests::*` | FR-VMT-10 (QMP round-trip incl. event skipping, reconcile invariants, force-stop guards) |
| `naslos_vmd::images::tests::*` | FR-VMT-07 (SSRF guard covers mapped/ULA addresses, scheme allowlist, upload name guards, redirect resolution) |
| `naslos_vmd::cloudinit::tests::*` | FR-VMT-06 (no password fields, pwauth disabled, instance-id tracks the seed) |
| `naslos_api::virt::tests::*` | FR-VMT-10 (store round-trip + revalidation, boot render marks autostart VMs, views survive a dead supervisor, credentials stay out of payloads) |
| `contract.rs` virt tests (fail-closed shapes, octet-stream scoped to the upload route only, console origin/auth) | FR-VMT-08/11 |
| `ui/tests/vms.spec.ts` (capability-skip) | FR-VMT-01 (banner on KVM-less nodes, list contract, preflight 409 on a stopped VM) |

Historical verification (2026-09-14, retired .96 VM, through its UI NodePort —
that listener was removed on 2026-09-19, and the same checks now run through the
ingress): peer → nginx → API, with the chart's `buddy` values enabled:

| Check | Verifies |
| --- | --- |
| `zfs create test/naslos-buddy`, `chown 65532:65532` (API runs as distroless nonroot), helm upgrade | SEC-8, the dataset is the only read-write host path |
| `df -h` on the receive mount path from a *fresh* pod | SEC-8, positive and negative: it found that a dataset created from inside a pod is mounted only in that pod's namespace, so the API's hostPath bound the parent dataset (`test`), not `test/naslos-buddy` — detection is now a documented deployment step |
| `buddyctl enroll --token …` then a second enrollment with the same token → 403 | FR-BUD-03 (single-use bootstrap) |
| `buddyctl push --dir` 3 MiB → 4 chunks; `buddyctl status`/`backups` show free space, stored bytes and last backup | FR-BUD-07, the nginx `client_max_body_size` path |
| `find` + `grep` in the store: only `chunk-*.enc` (1 MiB + 36 B, mode 0600) and the signed manifest; the plaintext needle is absent | FR-BUD-02 (zero-knowledge receiver) |
| `buddyctl restore --dir` + `diff -r` + `md5sum` | FR-BUD-06 (byte-identical restore) |
| 128 MiB push killed with `timeout -s KILL 1`, then `--resume` → 15 already present, 114 uploaded, full verify-only restore clean | FR-BUD-05 |
| one byte flipped in a stored chunk → restore fails with `cipher: message authentication failed` | FR-BUD-10 |
| `curl` without headers → 401; a second identity's key → 401 `unknown key`; push outside the key's scope → 403 | FR-BUD-03/08/10 |
| `buddyctl prune --keep 1` → storage drops by the pruned chain; remaining chain still restores | FR-BUD-09 |
| `GET /api/buddy/peers` without the proxy identity → 401, with `Remote-User` → 200 and the peer list | SEC-7 |
| `TestOwnerRoutesRequireAuth` walks every owner path with no headers, with a forged `Remote-User`, and with a wrong secret → 401 each | SEC-10 |
| `TestPublicRoutesStayPublic` (`/api/health`, `/api/ready`, and the peer API answering its own JSON 401) | SEC-10 (explicit allow-list) |
| `TestRequireAuthFailsClosedWithoutAConfiguredSecret` (no secret → nothing is authorized) | SEC-10 |
| `TestRequireAuth` on the agent: `/health` public, every `/api/v1/*` behind `Authorization: Bearer`, wrong/missing/partial token → 401 | SEC-11 |
| `TestNoncesSurviveARestart`, `TestNonceFileIsCompacted`, `TestPersistNoncesWithoutAPathStaysInMemory` | SEC-14 |
| `TestSequenceFollowsTheGuidIndex`, `TestSequenceRefusesWhenTheBaseIsGone`, `TestChainsListingIsCapped`, `TestRestoreSequenceFallsBackToTheListing` | FR-BUD-14, NAS-021 (bounded walks) |
| Agent refusal tests (`TestCreatePoolRefusals…`, `TestCreatePoolRefusesBadDisks`, `TestPoolSinksRefuseInvalidNames`, `TestSnapshotRefusesInvalidArguments`) assert no command ran at all | SEC-9/SEC-11 |
| `TestOwnerRoutesRequireAuth` (every owner path, with and without a forged `Remote-User`) → 401; `TestOwnerRoutesPassWithTheProxySecret` → reaches the handler; no opt-out exists | SEC-10 |
| `POST /api/buddy/send` → `202 {jobId}`; `GET /api/buddy/jobs/{id}` reaches `succeeded` with the sync-era result fields | FR-BUD-16 (async jobs) |
| second send for the same (receiver, source) or dataset while running → 409; `DELETE` mid-send → `cancelled`, resume state kept, retry `resumed: true` | FR-BUD-16 |
| schedule due → job runs, `lastResult: ok`, `pruneKeep` keeps the restorable sequence, one `backup_success` ntfy post | FR-BUD-15 |
| schedule against a dead receiver → job `failed`, `lastResult: failed`, one `backup_failure` ntfy post | FR-BUD-15 |
| `ui/tests/backups.spec.ts` — identity card, schedule create/delete, receiver free space, manual send to `succeeded` | FR-BUD-16 |

Rust verification: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`
and `cargo test` in `rust/` (agent and API). `npm run check` in `ui/` with 0 errors.

LDAP resilience (FR-IDN-11/12) is covered by
`rust/api/src/identity.rs` (lazy client and retry-after-failure cases) plus this
live drill:

1. `kubectl -n naslos scale sts naslos-openldap --replicas=0`
2. `kubectl -n naslos rollout restart deploy/naslos-api` (the restart race)
3. `/api/groups` → `503` naming the cause; `/api/ready` → `ldap:down` but
   `200`; `/` and `/api/metrics` → `200`
4. `kubectl -n naslos scale sts naslos-openldap --replicas=1`
5. `/api/groups` → `200` **with the same API pod, 0 restarts**

Installer provisioning (FR-INSTALL) is verified here by the install-pack build
and checksum gate (`scripts/audit.sh` → `check_install_pack`), `helm lint` of
`values-installer.yaml`, the machine-config template's
`render-installer-template.sh --check` tripwire against `cilium.yaml`, and the
released-pack digest pinning (FR-INSTALL-13: `check_image_digest_render` proves
the chart renders `repository@sha256:…`, `check_pack_digest_pinning` proves the
pack build pins all 8 chart images, records them in `metadata.json.images`, and fails
closed on a missing image). The engine-side steps are verified end to end from
`Naslos-Installer`: boot the ISO
from the pack's URL, run `naslos-install` headless, then check node Ready, pods
Running, admin login at `https://<domain>/authelia` with an installer-displayed
TOTP code, SMB login with the same password, and a recovery ZIP that restores a
working `talosctl`/`kubectl` context. **[OPEN until Naslos-Installer ships]**

Conformance rule: any PR that changes a MUST in this spec MUST update the
corresponding test in the same PR.

---

## 8. Versioning & conformance

- **VER-1** — The spec version matches the product version in `charts/naslos`
  values (`0.1.0` at time of writing).
- **VER-2** — Deployed images carry build suffixes (`naslos-api:0.1.0-r10`,
  `naslos-ui:0.1.0-r11`) because the registry reuses tags with
  `imagePullPolicy: IfNotPresent`; each deploy MUST retag to a fresh suffix
  and `kubectl set image` (container names are `api` and `ui`). Where a digest is
  configured (`<component>.image.digest`, `make image-digests` prints them) the
  chart renders `repository@sha256:…` instead, which is immutable: a pinned
  release MUST NOT also rely on a tag, and a component with no digest keeps the
  retag rule above.
- **VER-3** — API additions SHOULD be backward compatible; breaking changes
  to §4/§5 contracts require a minor version bump and an update to
  `docs/api.md`.
- **VER-4** — Known gaps are tracked as **[OPEN]** requirements here and in
  `AI_Handoff.md`/`docs/AUDIT-2026-09-19-REPORT.md`; a release MUST NOT claim
  conformance to an [OPEN] requirement.

---