# Shares (SMB / NFS / Time Machine)

Shares expose ZFS datasets over SMB (and, with limitations, NFS).
Share definitions live in the API; the file-serving workloads are separate
containers that run on the node with `hostNetwork` and host mounts.

## Architecture

```
 Naslos UI ──▶ API (source of truth)
                 │  shares.json + smbusers.json  (PVC: /var/lib/naslos)
                 │  renders smb.conf / exports / smbusers
                 ▼
        naslos-agent (privileged DaemonSet, /host mounted)
                 │  atomic writes
                 ▼
   /var/lib/naslos/shares/{smb.conf,exports,smbusers}   (Talos host)
                 │  hostPath, mounted read-write
                 ▼
   naslos-samba (DaemonSet, hostNetwork)  ── smbd :445
     • reloads smbd when smb.conf changes (after `testparm` accepts it)
     • imports accounts with `pdbedit -i smbpasswd:<smbusers>` on change
     • serves /var/mnt (the ZFS datasets) with `force user = root`
```

Why the agent writes the files: the API is a non-root Deployment with no
cluster RBAC. The agent is the only component that can write to the Talos host
filesystem (`/host`), so it is the sole writer of the rendered configuration.

Why SMB uses `hostNetwork`: SMB is not HTTP and cannot be reverse-proxied by
Traefik. The pod binds the node's 445/139 directly.

## API surface

| Method | Path | Description |
| --- | --- | --- |
| GET / POST | `/api/shares` | List / create |
| GET / PUT / DELETE | `/api/shares/{name}` | Detail / update / delete |
| GET | `/api/shares/paths` | Shareable dataset paths + ZFS base |
| GET | `/api/shares/status` | Rendered revision + what the node has applied |
| POST | `/api/shares/apply` | Re-render and push the configuration to the node |
| GET | `/api/shares/config/samba` | Generated `smb.conf` (text/plain) |
| GET | `/api/shares/config/nfs` | Generated `/etc/exports` (text/plain) |

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
| `path` | Filesystem path — must be **inside** `/var/mnt` (`SHARES_ZFS_BASE`) |
| `protocol` | `smb` \| `nfs` |
| `description` | Comment (may be empty) |
| `readOnly` | Export read-only |
| `browseable` | Visible in share list (defaults to true when omitted) |
| `allowedHosts` | Host allow list (empty = all) |
| `validUsers` | SMB users/groups allowed (empty = all authenticated users) |
| `timeMachine` | SMB Time Machine (macOS) share |
| `enabled` | Export active/inactive (defaults to true when omitted) |

Validation (`Manager.Create`):

- `name` required and free of the characters above;
- `path` required, canonicalised with `filepath.Clean`, and rejected unless it
  is *strictly inside* the ZFS base — this blocks traversal such as
  `/var/mnt/../etc`, which a plain prefix check would allow;
- protocol must be `smb` or `nfs` (AFP is not served — see below);
- duplicate names rejected.

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
smb://192.168.1.96/test                  [Copy]
```

- The host is taken from the address the operator is browsing the UI on
  (`window.location.hostname`). SMB is served by a `hostNetwork` pod on that
  same node, so the UI host *is* the SMB server; the port is deliberately not
  included because SMB uses 445, not the UI's NodePort.
- Only protocols that are actually served get an address. NFS shares show none,
  because Talos has no kernel NFS server yet (see the limitation below) — a
  copyable address that cannot be dialled would be worse than none.
- Disabled shares show "Disabled — not reachable until enabled" instead.
- The create/edit form offers only `smb` and `nfs`: AFP is rejected by the API,
  so offering it would produce a 400.

## Config generation

### Samba (`GenerateSambaConfig`)

- `[global]` workgroup `NASLOS`, `security = user`, fruit VFS enabled
  (`fruit:time machine = yes`, `fruit:model = MacSamba`, …) for macOS interop.
- One `[share]` stanza per enabled `smb`/`afp` share:
  `path`, `comment`, `read only`, `browseable`, `hosts allow/deny`,
  `valid users`, and `fruit:time machine` when `timeMachine` is set.
- `force user = root`, `force group = root`, masks `0664`/`0775`.

### NFS (`GenerateNFSExports`)

- One line per enabled NFS share: `<path> <host>(opts) …`
- Options: `rw|ro,sync,no_subtree_check`; no `allowedHosts` → `*(…)` (everyone).

## AFP / Time Machine

AFP is not served (netatalk is effectively dead). Time Machine uses SMB with the
`fruit` VFS, which is what the generated configuration enables.

> **Talos NFS limitation:** the kernel has no NFS *server* (`nfsd`) — only the
> client (`/proc/filesystems` lists `nfs`/`nfs4`, and `/proc/fs/nfsd` does not
> exist). Serving NFS therefore requires a userspace server (NFS-Ganesha) in a
> container with `hostNetwork`; kernel `nfsd` is not an option on Talos.

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
    image: { repository: 192.168.1.2:30095/naslos-samba, tag: "0.1.0" }
  nfs:
    enabled: true
    image: { repository: 192.168.1.2:30095/naslos-nfs,   tag: "0.1.0" }
  afp:
    enabled: false      # deprecated in favor of SMB Time Machine
```

`api.datasetsHostPath` is a hostPath, so it pins the API pod to one node — fine
for the single-node layout, but it must be cleared on multi-node clusters
(otherwise only that node's datasets are listed).

## Images

| Image | Role | Make target |
| --- | --- | --- |
| `naslos-samba` | SMB / Time Machine serving (smbd, smbclient, pdbedit, samba-vfs-modules, **libnss-extrausers**) | `make samba-image` |
| `naslos-nfs` | NFS serving (userspace, planned) | `make nfs-image` |

## Verification

```bash
# Configuration reached the node
curl -s "$API/api/shares/status"
talosctl -n "$VM" read /var/lib/naslos/shares/smb.conf
talosctl -n "$VM" read /var/lib/naslos/shares/smbusers

# An LDAP user's password works over SMB (no node account is created)
kubectl -n naslos exec ds/naslos-samba -- sh -c \
  "export SMB_CONF_PATH=/etc/naslos/shares/smb.conf; \
   getent passwd '<uid>'; \
   smbclient //127.0.0.1/<share> -U '<uid>%<password>' -c 'ls; put /etc/hostname probe.txt'"

# The account mirror resolves, or the entrypoint says which user does not
kubectl -n naslos logs ds/naslos-samba | grep -i 'resolve through NSS'
```
