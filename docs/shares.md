# Shares (SMB / NFS / Time Machine)

Shares expose ZFS datasets over SMB (and, with limitations, NFS).
Share definitions live in the API; the file-serving workloads are separate
containers that run on the node with `hostNetwork` and host mounts.

## Architecture

```
 Naslos UI ──▶ API (source of truth)
                 │  shares.json + smbusers.json  (PVC: /var/lib/naslos)
                 │  renders smb.conf / ganesha.conf / smbusers
                 ▼
        naslos-agent (privileged DaemonSet, /host mounted)
                 │  atomic writes
                 ▼
   /var/lib/naslos/shares/{smb.conf,ganesha.conf,smbusers}   (Talos host)
                 │  hostPath, mounted read-write
                 ▼
   naslos-samba (DaemonSet, hostNetwork)  ── smbd :445
     • reloads smbd when smb.conf changes (after `testparm` accepts it)
     • imports accounts with `pdbedit -i smbpasswd:<smbusers>` on change
     • serves /var/mnt (the ZFS datasets) with `force user = root`
   naslos-nfs (DaemonSet, hostNetwork)  ── ganesha.nfsd :2049 (NFSv4/TCP)
     • reloads exports on SIGHUP when ganesha.conf changes (mounts stay alive)
     • serves /var/mnt with `Squash = No_Root_Squash`, AUTH_SYS numeric ids
```

Why the agent writes the files: the API is a non-root Deployment with no
cluster RBAC. The agent is the only component that can write to the Talos host
filesystem (`/host`), so it is the sole writer of the rendered configuration.

Why SMB and NFS use `hostNetwork`: neither is HTTP, so neither can be
reverse-proxied by Traefik. The pods bind the node's 445 (SMB) and 2049 (NFS)
directly.

## API surface

| Method | Path | Description |
| --- | --- | --- |
| GET / POST | `/api/shares` | List / create |
| GET / PUT / DELETE | `/api/shares/{name}` | Detail / update / delete |
| GET | `/api/shares/paths` | Shareable dataset paths + ZFS base |
| GET / POST / DELETE | `/api/shares/folders` | List / create / remove folders inside the datasets (share paths) |
| GET | `/api/shares/status` | Rendered revision + what the node has applied |
| POST | `/api/shares/apply` | Re-render and push the configuration to the node |
| GET | `/api/shares/config/samba` | Generated `smb.conf` (text/plain) |
| GET | `/api/shares/config/nfs` | Generated NFS-Ganesha config (text/plain) |

Agent endpoints (`naslos-agent`, in-cluster only):

| Method | Path | Description |
| --- | --- | --- |
| PUT | `/api/v1/shares/config` | Write rendered config to the host |
| GET | `/api/v1/shares/status` | Report applied revision + share counts |

Every create/update/delete re-renders and pushes automatically, and the API
re-applies on startup so the node converges after a reboot or upgrade. A push
failure is logged and surfaced but never rolls back the share definition:
the API stays the source of truth and the next apply converges the node.

## Share model

| Field | Meaning |
| --- | --- |
| `name` | Unique share name (no `/ \ [ ] " ' : * ? < > = + ; ,`, ≤80 chars) |
| `path` | Filesystem path — a folder **on a ZFS dataset**: the dataset mountpoint itself or any subfolder of one. Must be inside `/var/mnt` (`SHARES_ZFS_BASE`) and must exist (create it from the UI's folder picker) |
| `protocol` | `smb` \| `nfs` |
| `description` | Comment (may be empty) |
| `readOnly` | Export read-only |
| `browseable` | Visible in share list (defaults to true when omitted) |
| `allowedHosts` | Host allow list (empty = all) |
| `validUsers` | SMB users/groups allowed (empty = all authenticated users) |
| `validGroups` | LDAP groups allowed, rendered as `@group` (empty = all) |
| `timeMachine` | SMB Time Machine (macOS) share |
| `enabled` | Export active/inactive (defaults to true when omitted) |

Validation (`Manager.Create`):

- `name` required and free of the characters above;
- `path` required, canonicalised with `filepath.Clean`, and rejected unless it
  is *strictly inside* the ZFS base — this blocks traversal such as
  `/var/mnt/../etc`, which a plain prefix check would allow;
- `path` must be on a **dataset** (or in a subfolder of one) and must exist as a
  folder: a path on the ephemeral partition is refused (see "Share paths must be
  on a ZFS dataset"), and so is a folder that is not there yet;
- protocol must be `smb` or `nfs` (AFP is not served — see below);
- duplicate names rejected.

An existing share can be repointed at another folder (`PUT /api/shares/{name}`
with `path`); the new path is validated exactly like a create.

## Folders in the pool (share paths)

Share paths are not limited to dataset roots: any folder inside a dataset is a
valid share path, so one dataset can hold several shares (`/var/mnt/test/media`,
`/var/mnt/test/backups`, …) and each stays visible/redundant/snapshotted with the
dataset.

The share form is a folder picker rather than a flat list: pick the dataset, walk
into its folders, or create a new one — creating it and saving the share links
the share to that new folder in the pool. Folders are created on the host by the
privileged agent (the API mounts the datasets **read-only**, it only needs to
list and stat them):

| Method | Path | Description |
| --- | --- | --- |
| GET | `/api/shares/folders?path=/var/mnt/test` | List the subfolders of a folder (`base`, `path`, `folders`) |
| POST | `/api/shares/folders` | Create `{"path": "<parent>", "name": "<folder>"}` → `{"path": "<parent>/<folder>"}` |
| DELETE | `/api/shares/folders?path=…` | Remove an **empty** folder |

Guarantees, checked on both sides:

- **Confined to the datasets.** The API rejects a path that is not on a dataset
  (and normalises `..` first); the agent rejects anything outside `/var/mnt`.
  Without that, the endpoint would be a remote file-creation primitive on the
  node.
- **One level at a time.** The parent must exist, so a typo cannot silently
  create a chain of directories and then share it.
- **Nothing is destroyed.** Delete refuses a non-empty folder (and the dataset
  root): a folder picker must not be able to remove share data — there is no
  recycle bin.
- **Reserved names.** Names containing separators or control characters are
  refused, as is `.zfs` (ZFS's snapshot directory, which is also hidden from
  listings).

## Account synchronisation (SMB ⇄ LDAP)

Naslos keeps one identity (OpenLDAP) and mirrors it into Samba's passdb so the
same user name and password work for the web UI and for SMB.

LDAP stores salted hashes from which an NT hash cannot be derived, so the NT
hash is captured when the password is set through the API
(`identity.SetPassword` → `computeNTHash`, MD4 of the UTF-16LE password) and
recorded in `smbusers.json` (`shares.SambaUserStore`). The store is rendered as
an `smbpasswd`-format file which the agent writes to the node and the
naslos-samba container imports:

```
smbtest:19330:XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX:6574CC…:LCT-6AA6D03E:
   │      │                 │                      │
   │      │                 │                      └─ NT hash of the password
   │      │                 └─ LM hash disabled (placeholder, never the password)
   │      └─ POSIX uid, read from the LDAP entry (uidNumber)
   └─ LDAP uid / Samba user name
```

Trigger points: user create (with a password), password change, delete, and
enable/disable (a disabled account keeps its hash but is flagged `[DU]`, so
re-enabling requires no new password).

### POSIX identity (why NSS is involved)

Samba attaches an SMB session to a **UNIX uid**, so an account that exists in
the passdb but cannot be resolved through NSS still cannot log in — it is
mapped to guest and the client sees `NT_STATUS_ACCESS_DENIED`, which is easy to
misread as a bad password. (An unresolvable uid is stored as `4294967295`.)

The API therefore mirrors each user's POSIX identity too: it reads `uidNumber`
and `gidNumber` from the LDAP entry and renders the `extrausers` files

```
/var/lib/naslos/shares/extrausers/passwd   →  mounted at /var/lib/extrausers
/var/lib/naslos/shares/extrausers/group
/var/lib/naslos/shares/extrausers/shadow
```

which the `naslos-samba` image resolves with

```
passwd: files extrausers
group:  files extrausers
shadow: files extrausers
```

This keeps the whole path automated — creating a user through the API is
enough; no account is ever created on the node, and the serving container needs
no LDAP credentials, CA or network path to OpenLDAP (it only needs the files).
The entrypoint additionally runs a **resolution check** on every sync and warns
loudly about any passdb account that does not resolve through NSS, because that
specific mismatch is otherwise silent.

> Alternatives considered: `nslcd`/`libnss-ldapd` or `passdb backend =
> ldapsam` against OpenLDAP directly. Both work, but they require directory
> credentials and TLS material inside the serving container and add a runtime
> dependency on LDAP for mere name resolution. The file-based mirror reuses the
> transport that already delivers the share configuration.

### Two pitfalls that silently break SMB logins

1. **`SMB_CONF_PATH` must be set for every Samba tool.** `smbd` is started with
   `-s /etc/naslos/shares/smb.conf`, but `pdbedit`/`smbpasswd` default to
   `/etc/samba/smb.conf` — so without `SMB_CONF_PATH` they read and write a
   *different* passdb (`/var/lib/samba/private/passdb.tdb`) than the running
   smbd. Accounts then look "created" to the tooling while every login falls
   back to guest and returns `NT_STATUS_ACCESS_DENIED`. The image exports
   `SMB_CONF_PATH` in its entrypoint for this reason.
2. **A resolvable POSIX account is required** — handled by the extrausers
   mirror above. Verify with `getent passwd <uid>` inside the samba pod.

### Verified behaviour

| Action (via API) | SMB result |
| --- | --- |
| Create user with a password | logs in immediately, no node account needed |
| Change password | new password works, old one stops working |
| Disable user | `NT_STATUS_ACCOUNT_DISABLED` (passdb flag `[DU]`) |
| Enable user | logs in again with the same password (hash retained) |
| Delete user | account removed from passdb and NSS files |
| Create / edit / delete share | smbd reloads and serves the change |

## UI (Shares page)

Each share card shows the address a client should use, with a copy button:

```
test            [SMB/CIFS]                      Edit  Delete
/var/mnt/test
smb://naslos.local/test                  [Copy]
```

- The host is taken from the address the operator is browsing the UI on
  (`window.location.hostname`). SMB and NFS are both served by `hostNetwork`
  pods on that same node, so the UI host *is* the file server; the port is
  deliberately not included (SMB uses 445, NFS 2049 — not the UI's HTTPS port).
- SMB shares show `smb://<host>/<name>`; NFS shares show `nfs://<host>/<name>`,
  which is the NFSv4 pseudo path, i.e. exactly the source that
  `mount -t nfs4` takes (`mount -t nfs4 <host>:/<name> /mnt`).
- Disabled shares show "Disabled — not reachable until enabled" instead.
- The **Path** control is a folder picker, not a flat list: it offers dataset
  mountpoints (the durable choices), walks into their subfolders, and can create
  a new folder to link the share to (`+ Create folder`). The dataset is what the
  browsing is confined to, so "Up" stops there and no path outside the pool can
  be selected. When editing a share the picker opens at the share's current
  folder, so it can be repointed at another one.
- The create/edit form offers only `smb` and `nfs`: AFP is rejected by the API,
  so offering it would produce a 400.

## Share paths must be on a ZFS dataset

A share path MUST be a directory on a ZFS dataset. This is enforced, not just
documented, because the alternative fails **silently and destructively**.

Talos keeps `/var` (and therefore `/var/mnt`) on its **EPHEMERAL partition**.
A directory such as `/var/mnt/tank` is a real, writable directory even when
`tank` is *not* a dataset — so a share pointed at it works perfectly until the
data is needed:

- the data is **not in any pool**: no checksums, no snapshots, no redundancy,
  invisible to `zpool`/`zfs`, absent from pool health and scrubs;
- it is **wiped by a Talos upgrade** (EPHEMERAL is not preserved);
- if a dataset is later mounted over that directory, the files are **shadowed**
  and appear to vanish from the share;
- if the pool that used to be mounted there is destroyed, the data goes with it.

Enforcement, at every layer:

| Layer | Behaviour |
| --- | --- |
| `GET /api/shares/paths` | Offers **only dataset mountpoints** (from the agent's `zfs list`), never arbitrary directories under the base |
| `POST /api/shares` | Rejects a path that is not on a dataset (HTTP 400) naming the datasets that would work, and rejects a dataset path that does not exist |
| `GET/POST/DELETE /api/shares/folders` | Folders can be created *inside* a dataset only (see "Folders in the pool") — so a share can be pointed at a subfolder without ever leaving the pool |
| `naslos-samba` / `naslos-nfs` startup | Log each share/export path with the mount backing it, and warn loudly for any path that is not on a mounted filesystem |
| Schedules | `mountPropagation: HostToContainer` on the `/var/mnt` mounts, so a dataset mounted *after* a pod starts (pool import on boot) is visible instead of the pod serving the underlying directory |

```
$ curl -X POST /api/shares -d '{"name":"trap","path":"/var/mnt/tank",...}'
{"error":"/var/mnt/tank is not on a ZFS dataset, so its data would live on the
node's ephemeral partition (no snapshots, no redundancy, lost on upgrade).
Create a dataset under one of /var/mnt/test first"}
```

### Recovering files that "disappeared"

1. Check whether the path was actually a dataset: `zfs list -o name,mountpoint`.
   If the path is not listed, the files were on EPHEMERAL.
2. If EPHEMERAL is intact but a dataset now covers the path, the files are
   **shadowed, not gone** — they are visible from the host with the dataset
   temporarily unmounted (`zfs unmount <dataset>`).
3. If the pool itself is gone, `zpool import` / `zpool import -D` are the only
   recovery paths; a destroyed pool with no importable label cannot be recovered.

## Share access by user and group

A share is open to any authenticated account until an access list is set:

- `validUsers` — user names, rendered bare in `valid users`.
- `validGroups` — LDAP group names, rendered as `@group`.

They are separate fields on purpose: a single free-text list would be ambiguous
about whether `naslos_users` means a user or a group. Typing `@group` directly
into `validUsers` still works (it is passed through), and a stray `@` elsewhere
in a user name is stripped, since Samba would otherwise read the remainder as a
group name and silently widen or break access.

### How Samba evaluates a group on the node

Samba does not ask LDAP: it resolves `@group` through **NSS** and checks the
session user against the group's members. Two things therefore have to be true
on the node, and both are automated:

1. `valid users = @group` appears in the rendered `smb.conf` (from
   `validGroups`).
2. The group exists in the mirrored `extrausers` group file with its real
   members, e.g. `sharetest:x:27676:alice,bob`.

LDAP groups are `groupOfNames` with **no gidNumber**, but NSS needs one, so a
stable gid is derived from the group name (`shares.GroupGID`, in the
20000–27999 range so it cannot collide with the 10000-range user gids). The gid
must be stable across renders or the group would change identity between syncs.

Because the node evaluates a *mirror* rather than LDAP itself, every change that
affects access re-pushes it: group membership add/remove, group delete, user
create (which may add group memberships), user delete, and password change.

### Verified end-to-end

```
smb.conf:  valid users = @sharetest
node NSS:  sharetest:x:27676:grpmember
```

| Check | Result |
| --- | --- |
| Group member logs in | OK |
| Non-member refused | FAIL (`NT_STATUS_ACCESS_DENIED`) |
| Member removed from group → revoked | access FAIL |
| Outsider added to group → granted | access OK |

The API also accepts either uids or DNs in a group's `members` on PUT: GET
returns member DNs, so a client that PUTs back exactly what it read must not
corrupt the group (a DN used as a uid once produced
`uid=uid=alice,ou=people,...`).

## Network discovery (browsing `smb://`)

The server advertises itself so it appears in a client's network browse view —
Dolphin's `smb://` place list, Finder's sidebar, Explorer's Network — instead
of being reachable only by typing its address:

- **mDNS / DNS-SD via Avahi** publishes `_smb._tcp` on port 445 (plus
  `_device-info._tcp` with `model=MacSamba`, so macOS shows a server icon
  rather than an unknown device). This is what Linux and macOS file managers
  browse.
- **WSD via `wsdd`** covers Windows Explorer, which dropped SMBv1 browsing.
- `netbios name` in the rendered `smb.conf` is set to the same name
  (`shares.discovery.name`, default `naslos`, sanitised to NetBIOS rules by the
  API), so browsing clients and direct connections see one identity.

```
$ avahi-browse -rt _smb._tcp
+ enp1s0 IPv4 naslos     Microsoft Windows Network local
   hostname = [naslos.local]
   address  = [<node-ip>]
   port     = [445]
```

### Advertising on the right interface

The node also has `cilium_host`, `lxc*` and a veth pair per pod. Left alone,
Avahi enumerates all of them and advertises the server at pod-network (10.x)
addresses too, which clients cannot reach. The entrypoint therefore pins
discovery to the **default-route interface**, auto-detected by parsing
`/proc/net/route` (so no iproute2 is needed in the image), and passes the same
interface to `wsdd -i`. Override it with `shares.discovery.interface`.

### Verifying

```bash
# What the network advertises (run from the samba pod, or any Avahi host)
kubectl -n naslos-privileged exec ds/naslos-samba -- avahi-browse -rt _smb._tcp
kubectl -n naslos-privileged exec ds/naslos-samba -- avahi-resolve -n naslos.local

# The daemons and their sockets
kubectl -n naslos-privileged exec ds/naslos-samba -- ps -eo pid,args | grep -E 'avahi|wsdd'
```

Discovery is **best-effort**: if Avahi or wsdd cannot start (no multicast, port
conflict), the failure is logged and SMB keeps serving — a client can always
connect by address (see the share card URL).

### How fast a password change applies

Measured on the VM against a fresh SMB login (a *new* connection, not an
existing one):

| `shares.confCheckInterval` | API call itself | Until a new SMB login accepts it |
| --- | --- | --- |
| 3 (default) | 24–29 ms | **2.6 – 3.0 s** |
| 1 | 27–30 ms | **0.8 – 1.1 s** |

The API call is synchronous: the LDAP password, the recorded NT hash and the
push to the node all complete before it returns, so the API call itself costs
tens of milliseconds. The delay is entirely the serving container's poll
interval, which is why it is tunable via `shares.confCheckInterval`
(`CONF_CHECK_INTERVAL`). Measured figures include ~0.4 s of probe overhead.

Three nuances matter more than the number:

- **Only new connections are affected.** Samba authenticates at session setup,
  so an already-mounted share keeps working with the old password until the
  client reconnects. A share that "still works" right after a change is a stale
  session, not a failed sync.
- **Client credential caches** (macOS Keychain, Windows Credential Manager,
  GNOME Keyring) can keep replaying the old password. That is client-side.
- **Web/SSO is immediate** — Authelia validates against LDAP directly, with no
  polling step.

Change detection uses a **content hash**, not mtime: mtime has one-second
granularity, so two writes inside the same second would compare equal and the
later change could be skipped. Verified by firing 8 password changes ~0.4 s
apart (several within the same second) and confirming the final password is the
one that works, while intermediate and initial passwords are rejected.

## Config generation

### Samba (`GenerateSambaConfig`)

- `[global]` workgroup `NASLOS`, `security = user`, fruit VFS enabled
  (`fruit:time machine = yes`, `fruit:model = MacSamba`, …) for macOS interop.
- One `[share]` stanza per enabled `smb` share (AFP is not served):
  `path`, `comment`, `read only`, `browseable`, `hosts allow/deny`,
  `valid users`, and `fruit:time machine` when `timeMachine` is set.
- `force user = root`, `force group = root`, masks `0664`/`0775`.

### NFS (`GenerateGaneshaConfig`)

NFS is served by **NFS-Ganesha in userspace**, not by kernel `nfsd`: Talos ships
only the NFS *client* (`/proc/filesystems` lists `nfs`/`nfs4`; `/proc/fs/nfsd`
does not exist), so there is no `nfsd` to read `/etc/exports` and no `rpcbind`
to register with. The API therefore renders Ganesha's own config format, and
`/api/shares/config/nfs` returns that (not `/etc/exports`).

For every enabled NFS share the renderer emits one `EXPORT` block:

```
NFS_CORE_PARAM { Protocols = 4; Enable_NLM = false; Enable_RQUOTA = false;
                 NFS_Port = 2049; mount_path_pseudo = true; }
NFSv4         { Grace_Period = 10; Lease_Lifetime = 90; }
NFS_KRB5      { Active_krb5 = false; }        # no krb5 configured → no keytab probing

EXPORT {
    Export_Id       = <stable hash of the share name>;   # unique, non-zero
    Path            = /var/mnt/test;                     # the real path served
    Pseudo          = /<share name>;                     # what clients mount
    Access_Type     = RW | RO;                           # from readOnly
    Squash          = No_Root_Squash;
    SecType         = sys;                               # AUTH_SYS, numeric uid/gid
    Protocols       = 4;
    Transports      = TCP;
    FSAL            { Name = VFS; }
    CLIENT          { Clients = <allowedHosts…> | *; Access_Type = RW|RO; }
}
```

Decisions worth knowing:

- **NFSv4 only.** NFSv3 needs `rpcbind` (111) and `statd`, neither of which
  exists on Talos. v4 also has locking built in, so `Enable_NLM = false` is not
  a compromise.
- **`Pseudo` is separate from `Path`**, so clients mount `<host>:/<share name>`
  and the share can be repointed at another dataset without clients changing
  anything. `Export_Id` is derived from the share name (not a running counter),
  so adding or removing one share does not renumber the others.
- **No host restriction = `Clients = *`** (everyone), matching the SMB default;
  `allowedHosts` entries become one `CLIENT` block each, so a share can be
  limited to a subnet or a single host.
- **`No_Root_Squash` mirrors SMB's `force user = root`.** The datasets are
  root-owned, so squashing root would make them unwritable for an admin client.
  Change it here if root-squash semantics are wanted.

## NFS serving container (`naslos-nfs`)

| Aspect | Behaviour |
| --- | --- |
| Image | `nfs/image/Dockerfile` — Debian + `nfs-ganesha` + `nfs-ganesha-vfs` |
| Config | `/var/lib/naslos/shares/ganesha.conf`, written by the agent, read-only mounted |
| Reload | Polls the config every `shares.confCheckInterval` seconds and sends `SIGHUP`; Ganesha re-reads its export table **without dropping mounts**. A config that fails to parse leaves the running exports in place |
| Port | `2049/TCP` on the node (`hostNetwork`); readiness/liveness are TCP probes |
| Datasets | `/var/mnt` with `mountPropagation: HostToContainer` (same reason as Samba: a pool imported after pod start must become visible) |
| Startup log | Reports each export path *with the filesystem backing it*, and warns when a path is not on a mounted filesystem |

Two container-specific requirements, both of which fail in confusing ways if
missed:

1. **`/etc/mtab`** — the VFS FSAL enumerates mounted filesystems through it, and
   Debian containers do not ship it (`systemd` normally creates it). Without it
   *every* export fails to load with
   `vfs_create_export: resolve_posix_filesystem(<path>) returned No such file or directory`.
   The image symlinks `/etc/mtab → /proc/mounts`.
2. **`CAP_DAC_READ_SEARCH`** — the VFS FSAL serves files by NFS file handle via
   `open_by_handle_at(2)`, which needs this capability. Without it the mount
   *succeeds* but every `ls`/open fails with `Operation not permitted`
   (`NFS4ERR_PERM`); the server log shows
   `vfs_open_by_handle :FSAL :Failed with Operation not permitted`.

### Using it from a client

```bash
# Linux
sudo mount -t nfs4 <node-ip>:/test /mnt/nas
# macOS
mount_nfs -o vers=4 <node-ip>:/test /Volumes/nas
```

Client notes:

- **Ownership display depends on the client's idmap.** Ganesha reports owners as
  *names* when it can resolve them locally (uid 0 → `root`), and clients resolve
  them back through `rpc.idmapd`/`nfsidmap` — which every normal distribution
  starts. A client with idmapping disabled and no idmap daemon (e.g. a bare
  container) shows such entries as `nobody`/`4294967294`. This is display-only:
  verified on the pool, files written through NFS carry the caller's real uid
  (a client running as uid 1000 created `1000:1000`, root created `0:0`).
- **Permissions are enforced, not widened** (verified live): uid 1000 could not
  write into a `755` root-owned directory (`Permission denied`), while a
  world-writable directory accepted the write.
- LDAP users are known to the *server* through the NSS mirror, so a client using
  matching uids keeps its identity on the share; `No_Root_Squash` means the
  client's numeric uid is what the filesystem records.

## AFP / Time Machine

AFP is not served (netatalk is effectively dead). Time Machine uses SMB with the
`fruit` VFS, which is what the generated configuration enables.

> **Talos NFS limitation:** the kernel has no NFS *server* (`nfsd`) — only the
> client (`/proc/filesystems` lists `nfs`/`nfs4`, and `/proc/fs/nfsd` does not
> exist). Kernel `nfsd` is therefore never an option on Talos; NFS is served in
> userspace by NFS-Ganesha (see "NFS serving container" above).

## Chart flags (`charts/naslos/values.yaml`)

```yaml
api:
  sharesConfig:                # PVC holding shares.json + smbusers.json
    enabled: true
    size: 64Mi
  datasetsHostPath: /var/mnt   # read-only, so the API can list/validate paths

shares:
  enabled: true
  smb:
    enabled: true
    workgroup: "NASLOS"
    timeMachine: true
    image: { repository: 192.168.1.2:30095/naslos-samba, tag: "0.1.0-r3" }
  # Network advertisement (mDNS + WSD) so the server shows up when browsing.
  discovery:
    enabled: true
    name: "naslos"     # also the Samba NetBIOS name
    interface: ""      # empty = auto-detect the default-route interface
  # Seconds between config/account re-checks; bounds how long a password change
  # takes to apply to new SMB connections (3 = up to ~3 s, 1 = up to ~1 s).
  confCheckInterval: 3
  nfs:
    enabled: true
    image: { repository: 192.168.1.2:30095/naslos-nfs,   tag: "0.1.0-r3" }
  afp:
    enabled: false      # deprecated in favor of SMB Time Machine
```

`api.datasetsHostPath` is a hostPath, so it pins the API pod to one node — fine
for the single-node layout, but it must be cleared on multi-node clusters
(otherwise only that node's datasets are listed).

## Images

| Image | Role | Make target |
| --- | --- | --- |
| `naslos-samba` | SMB / Time Machine serving (smbd, smbclient, pdbedit, samba-vfs-modules, libnss-extrausers, **avahi-daemon + wsdd** for discovery) | `make samba-image` |
| `naslos-nfs` | NFS serving (NFS-Ganesha, NFSv4/TCP on :2049) | `make nfs-image` |

## Verification

```bash
# Configuration reached the node
curl -s "$API/api/shares/status"
talosctl -n "$VM" read /var/lib/naslos/shares/smb.conf
talosctl -n "$VM" read /var/lib/naslos/shares/smbusers

# An LDAP user's password works over SMB (no node account is created)
kubectl -n naslos-privileged exec ds/naslos-samba -- sh -c \
  "export SMB_CONF_PATH=/etc/naslos/shares/smb.conf; \
   getent passwd '<uid>'; \
   smbclient //127.0.0.1/<share> -U '<uid>%<password>' -c 'ls; put /etc/hostname probe.txt'"

# The account mirror resolves, or the entrypoint says which user does not
kubectl -n naslos-privileged logs ds/naslos-samba | grep -i 'resolve through NSS'

# NFS: the rendered config reached the node and Ganesha loaded every export
talosctl -n "$VM" read /var/lib/naslos/shares/ganesha.conf
kubectl -n naslos-privileged logs ds/naslos-nfs | grep -E 'ganesha.nfsd running|export path'
kubectl -n naslos-privileged exec ds/naslos-nfs -- \
  grep -icE 'Could not create export|NFS4ERR_PERM' /var/log/ganesha/ganesha.log   # 0

# NFS: a real client mounts, reads, writes and unmounts (run on the LAN, not in
# the cluster - a pod has no CAP_SYS_ADMIN and cannot mount)
sudo mkdir -p /mnt/nas
sudo mount -t nfs4 "$VM":/<share> /mnt/nas && ls -la /mnt/nas \
  && echo probe > /mnt/nas/probe.txt && cat /mnt/nas/probe.txt \
  && cd / && sudo umount /mnt/nas

# NFS: a share on a FOLDER inside a dataset (created from the UI's picker)
curl -s -X POST "$API/api/shares/folders" -H 'Content-Type: application/json' \
  -d '{"path":"/var/mnt/test","name":"media"}'      # -> {"path":"/var/mnt/test/media"}
curl -s -X POST "$API/api/shares" -H 'Content-Type: application/json' \
  -d '{"name":"media","path":"/var/mnt/test/media","protocol":"nfs"}'
# ...then mount it from another machine and write:
sudo mount -t nfs4 "$VM":/media /mnt/nas && echo ok > /mnt/nas/probe.txt && ls -la /mnt/nas

# The folder endpoints are confined to the datasets: both of these must be 400
curl -s -X POST "$API/api/shares/folders" -d '{"path":"/etc","name":"evil"}'
curl -s -X POST "$API/api/shares/folders" -d '{"path":"/var/mnt/test/../../etc","name":"evil"}'

# NFS: a share change is reloaded in place (mounts are not interrupted)
kubectl -n naslos-privileged logs ds/naslos-nfs | grep 'reloading exports'
```
