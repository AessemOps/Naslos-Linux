# Shares (SMB / NFS / Time Machine)

Shares expose ZFS datasets over SMB, NFS, and (legacy) AFP/Time Machine.
Share definitions live in the API (`shares.Manager`); the Samba/NFS services
consume generated configs.

## Share model

| Field | Meaning |
| --- | --- |
| `name` | Unique share name |
| `path` | Filesystem path — **must start with `/var/mnt`** (`shares.zfsBase`) |
| `protocol` | `smb` \| `nfs` \| `afp` |
| `description` | Comment |
| `readOnly` | Export read-only |
| `browseable` | Visible in share list |
| `allowedHosts` | Host allow list (empty = all) |
| `validUsers` | SMB users/groups allowed (empty = all) |
| `timeMachine` | SMB Time Machine (macOS) share |
| `enabled` | Export active/inactive |

Validation rules in `Manager.Create`:

- name required, path required, protocol must be `smb|nfs|afp`;
- path must be under the ZFS base `/var/mnt`;
- duplicate names rejected.

## API surface

| Method | Path | Description |
| --- | --- | --- |
| GET / POST | `/api/shares` | List / create |
| GET / PUT / DELETE | `/api/shares/{name}` | Detail / update / delete |
| GET | `/api/shares/config/samba` | Generated `smb.conf` (text/plain) |
| GET | `/api/shares/config/nfs` | Generated `/etc/exports` (text/plain) |

`Manager.ValidatePath` requires the path to exist, be a directory, and live
under `/var/mnt`. `AvailablePaths` lists candidate ZFS dataset mount dirs.

## Config generation

### Samba (`GenerateSambaConfig`)

- `[global]` workgroup `NASOS`, `security = user`, fruit VFS enabled
  (`fruit:time machine = yes`, `fruit:model = MacSamba`, …) for macOS interop.
- One `[share]` stanza per enabled `smb`/`afp` share:
  `path`, `comment`, `read only`, `browseable`, `hosts allow/deny`,
  `valid users`, and `fruit:time machine` when `timeMachine` is set.
- `force user = root`, `force group = root`, masks `0664`/`0775`.

### NFS (`GenerateNFSExports`)

- One line per enabled NFS share: `<path> <host>(opts) …`
- Options: `rw|ro,sync,no_subtree_check`; no `allowedHosts` → `*(…)` (everyone).

## Share → storage wiring

```
 ZFS pool  /var/mnt/<pool>
     └── dataset  pool/share-a   →  mountpoint  /var/mnt/<pool>/share-a
                                       │
   shares.Manager  ──spans──▶  share { name: share-a, path: /var/mnt/.../share-a }
                                       │
   Samba service ── /api/shares/config/samba → /etc/samba/smb.conf
   NFS  service  ── /api/shares/config/nfs   → /etc/exports
```

Time Machine uses SMB + `fruit` VFS: no dedicated AFP daemon is required in
modern deployments (`values.yaml: shares.afp.enabled: false`, SMB
`timeMachine: true` preferred).

## Chart flags (`charts/nasos/values.yaml`)

```yaml
shares:
  enabled: true
  smb:
    enabled: true
    workgroup: "NASOS"
    timeMachine: true
  nfs:
    enabled: true
  afp:
    enabled: false      # deprecated in favor of SMB Time Machine
```