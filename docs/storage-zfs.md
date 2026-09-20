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
| Health monitoring | `naslos-agent` | Parses `zpool status` + `zpool iostat` into structured data |
| Pool import | `naslos-agent` | `zpool import` (dry-run) to list; `zpool import -f` to import |
| Boot import | `zfs-service` (Image Factory extension) | `zpool import -fal` at boot |
| Volumes for K8s | local-path provisioner (only) | App/PVC storage; ZFS LocalPV is not installed |

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

## Pool health

`GET /api/volumes/zfs/{name}` (and `/api/volumes/zfs/{name}/health`) returns
structured health data parsed from `zpool status` and `zpool iostat`:

```json
{
  "name": "tank",
  "state": "ONLINE",
  "scan": "scrub repaired 0B in 00:01:23 with 0 errors",
  "errors": "No known data errors",
  "config": [
    {
      "name": "tank", "state": "ONLINE",
      "read": "0", "write": "0", "cksum": "0",
      "devices": [
        { "name": "mirror-0", "state": "ONLINE", "devices": [
          { "name": "/dev/sda", "state": "ONLINE", "read": "0", "write": "0", "cksum": "0" },
          { "name": "/dev/sdb", "state": "ONLINE", "read": "0", "write": "0", "cksum": "0" }
        ]}
      ]
    }
  ],
  "ioStats": { "readOps": "12", "writeOps": "345", "readBW": "1.2K", "writeBW": "45.6K" }
}
```

The device tree preserves indentation: top-level vdevs (mirror, raidz, cache)
nest their child disks under `devices`. The UI renders this on the
**Pool Health** page (`/pools/{name}`) with color-coded state badges.

## Import existing pools

Pools that already exist on disk but are not currently imported can be discovered
and imported:

| Method | Path | Description |
| --- | --- | --- |
| GET | `/api/volumes/zfs/import` | List importable pools (name, state, topology, disks) |
| POST | `/api/volumes/zfs/import` | `{"name":"tank"}` imports one; `{}` imports all |

The agent runs `zpool import` (dry-run) to list candidates, then
`zpool import -f <name>` (or `zpool import -f` for all) to import. Already-imported
pools are excluded from the list.

The UI offers **Import Pool** on the `/pools` page: a modal shows each importable
pool's name, topology (Mirror/RAIDZ1/etc.), state, and member disks with a
per-pool Import action.

## Datasets & snapshots

The agent exposes dataset and snapshot operations:

- `GET/POST /api/v1/datasets/{pool}`, `DELETE /api/v1/datasets/{pool}/{name…}`
- `GET/POST /api/v1/snapshots/{dataset}`

Datasets are how shares get structured sizes/quota boundaries and are the unit
of snapshots (`zfs snapshot pool/ds@name`). Shares previously used AFP/Time
Machine; the modern path is SMB + `fruit` VFS (see [shares.md](shares.md)).

### Creating a dataset (UI)

`Pools → <pool>` lists the pool's datasets and creates new ones. The API layer
(`POST /api/datasets`, `DELETE /api/datasets?name=…`) validates before anything
reaches ZFS:

| Rule | Why |
| --- | --- |
| Each name component is ZFS-safe, no traversal, no `@` | A name is interpolated into `zfs create`; `../` or a snapshot marker must not get through |
| Nested names allowed (`photos/2026`), created with `-p` | One call, and the intermediate levels are what the operator meant |
| Options limited to `compression`, `quota`, `recordsize`, `atime`, `copies`, `readonly` with validated values | `zfs create -o` accepts arbitrary properties; `mountpoint=/` would put the dataset somewhere unexpected |
| The pool must exist | Otherwise the failure surfaces after the name was accepted |
| Destroy needs `recursive=true` for a non-empty dataset, never applies to a pool's root dataset, and is refused (409) while a share serves the dataset's path | A dataset picker must not delete data or quietly break a share |

The UI's dataset form only asks for what matters (name, compression, quota) and
the pool page shows used/free per dataset.

### Growing a pool (adding disks)

`POST /api/volumes/zfs/{pool}/devices` → `zpool add [-f] <pool> [<topology>]
<disk>…`, surfaced as **Add Drive** on the pool page. Three properties drive the
design:

- **`zpool add` does not rebalance.** Existing data stays on the existing vdevs,
  so this adds capacity, not throughput for data already written.
- **Losing any vdev loses the pool.** A stripe vdev therefore *lowers* the pool's
  fault tolerance, which the dialog says in plain words before the button.
- **It writes to the disks.** Hence: only disks the node reports as usable whole
  disks are accepted (the system disk cannot be selected), a disk that already
  belongs to a pool is refused *even with `force`*, and `force` itself is a
  separate opt-in checkbox with its own warning.

Both the API and the agent check these, so the guardrails hold even when the UI
is bypassed.

### AGENT-side dataset/device endpoints

- `GET/POST /api/v1/datasets/{pool}`, `DELETE /api/v1/datasets/{pool}/{name…}?recursive=true`
- `GET/POST /api/v1/pools/{pool}/devices` (list disks that are free / attach a vdev)

## Storage classes & app data

`charts/naslos/values.yaml`:

```yaml
storage:
  localPath:
    enabled: true
    path: /var/mnt/local-path-provisioner
```

- **local-path-provisioner** is the only provisioner deployed; every PVC
  (OpenLDAP, shares, buddy) binds through it.
- **ZFS LocalPV is not installed.** A `storage.zfsLocalPV` block used to sit here
  but no template consumed it, the provisioner was never deployed and its pool
  name did not exist on the node; the block was removed on 2026-09-19
  (AUDIT-M13). Install the provisioner and add a `naslos-zfs` StorageClass first
  if ZFS-backed PVCs are wanted.

## Lifecycle & gotchas

See [architecture.md](architecture.md) → "ZFS Pool Lifecycle" and
"Talos-Specific ZFS Gotchas" for boot import, shutdown export, SELinux,
mount-under-`/var`, and noexec notes.

## Configuration env

| Variable | Default | Meaning |
| --- | --- | --- |
| agent `NODE_NAME` | from `spec.nodeName` | Which node this agent runs on |
| `LDAP_*` | see api.md | LDAP connection (for OpenLDAP's ZFS-backed PVCs) |