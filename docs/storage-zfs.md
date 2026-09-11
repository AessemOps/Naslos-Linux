# Storage & ZFS

ZFS is Naslos's primary filesystem. Pools are created and managed by the
privileged **naslos-agent** (a DaemonSet) because they live outside Talos's
volume system. Talos's own `UserVolumeConfig` only supports ext4/xfs/btrfs and
is exposed separately via `/api/volumes`.

## Who does what

| Layer | Actor | Responsibility |
| --- | --- | --- |
| Disk discovery | `naslos-api` `talos.GetDiscoveredVolumes` | `talosctl get discoveredvolumes` |
| Topology advice | `talos.VolumeAdvisor` | Recommends `single/mirror/raidz1/raidz2/raidz3` |
| Pool/DS/snapshot ops | `naslos-agent` (`chroot /host zpool/zfs`) | Actual creation, status, destroy, import/export |
| Boot import | `zfs-service` (Image Factory extension) | `zpool import -fal` at boot |
| Volumes for K8s | `naslos-zfs` storage class + local-path provisioner | App PVCs |

## Pool creation (what actually runs)

```
zpool create -f
  -o ashift=12 -o mountpoint=/var/mnt/<pool> -o xattr=sa
  -o compression=zstd -o acltype=posixacl -o atime=off
  -o dnodesize=auto -o relatime=on -o recordsize=128K
  -o special_small_blocks=0 -o com.sun:auto-snapshot=false
  -O aclmode=restricted
  <pool> [mirror | raidz1 | raidz2 | raidz3] <disks...>
zfs set context=none fscontext=none defcontext=none rootcontext=none <pool>
```

Highlights:

- Every disk is **wiped first** (`wipefs --all`) to clear foreign signatures
  (e.g. `com.sun:auto-snapshot` or Samba/NFS props from another NAS OS).
- `ashift=12` → 4K sector alignment, `compression=zstd`, `xattr=sa`,
  `acltype=posixacl`, `atime=off` are the NAS best-practice set.
- `aclmode=restricted` is forced for SMB compatibility.
- SELinux contexts are disabled on the pool — a Talos requirement.

## Topology advisor

`VolumeAdvisor.Recommend(disks)` filters out system disks, then:

| Usable disks | Recommendation |
| --- | --- |
| 0 | error (“no disks provided”) |
| 1 | `single` — no redundancy |
| 2 | `mirror` — capacity of the smallest disk |
| 3–5 | `raidz1` — single parity |
| 6–10 | `raidz2` — double parity |
| 11+ | `raidz3` — triple parity |

## Datasets & snapshots

The agent exposes dataset and snapshot operations:

- `GET/POST /api/v1/datasets/{pool}`
- `GET/POST /api/v1/snapshots/{dataset}`

Datasets are how shares get structured sizes/quota boundaries and are the unit
of snapshots (`zfs snapshot pool/ds@name`). Shares previously used AFP/Time
Machine; the modern path is SMB + `fruit` VFS (see [shares.md](shares.md)).

## Storage classes & app data

`charts/naslos/values.yaml`:

```yaml
storage:
  localPath:
    enabled: true
    path: /var/mnt/local-path-provisioner
  zfsLocalPV:
    enabled: true
    poolName: "naslos-pool"
```

- **local-path-provisioner** gives apps cheap host-path storage.
- **ZFS LocalPV CSI / `naslos-zfs`** gives apps ZFS-backed PVCs (used by the
  OpenLDAP StatefulSet and by catalog apps that request `storageClass:
  naslos-zfs`).

## Lifecycle & gotchas

See [architecture.md](architecture.md) → "ZFS Pool Lifecycle" and
"Talos-Specific ZFS Gotchas" for boot import, shutdown export, SELinux,
mount-under-`/var`, and noexec notes.

## Configuration env

| Variable | Default | Meaning |
| --- | --- | --- |
| agent `NODE_NAME` | from `spec.nodeName` | Which node this agent runs on |
| `LDAP_*` | see api.md | LDAP connection (for OpenLDAP's ZFS-backed PVCs) |