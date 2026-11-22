# Storage & ZFS

ZFS is Naslos's primary filesystem. Pools are created and managed by the
privileged **naslos-agent** (a DaemonSet) because they live outside Talos's
volume system. Talos's own `UserVolumeConfig` only supports ext4/xfs/btrfs and
has no apply path in Naslos today, so it is not exposed (the `/api/volumes`
stub was removed in PF-M8).

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

## Disk health

`GET /api/disks/health` merges the Talos disk inventory with per-disk SMART
health from the privileged agent and the vdev state/error counters from
`zpool status`. SMART is read-only and standby-safe; a virtio/USB/RAID disk
without SMART is reported as unavailable while its ZFS state stays visible.
See [operations.md](operations.md#disk-health) for thresholds and replacement
guidance.

## Datasets & snapshots

The agent exposes dataset and snapshot operations:

- `GET/POST /api/v1/datasets/{pool}`, `DELETE /api/v1/datasets/{pool}/{name…}`
- `GET/POST/DELETE /api/v1/snapshots/{dataset}` (`?paths=true` returns full
  `dataset@snap` paths; `DELETE ?name=` destroys one snapshot)

Datasets are how shares get structured sizes/quota boundaries and are the unit
of snapshots (`zfs snapshot pool/ds@name`). Shares previously used AFP/Time
Machine; the modern path is SMB + `fruit` VFS (see [shares.md](shares.md)).
Snapshots that should appear as SMB previous versions must be named
`naslos-YYYY-MM-DD_HH-MM-SS` (UTC); scheduled snapshots use that format, and
see [shares.md](shares.md#shadow-copies-previous-versions).

### Scheduled snapshots and scrubs (FR-STO-10/11)

`Pools → <pool>` owns the maintenance schedules:

- **Snapshot schedule** (per dataset): `GET|POST|DELETE /api/storage/schedules`
  with `kind: "snapshot"`. Cadence is hourly/daily/weekly in UTC, with an
  optional `recursive` flag (`zfs snapshot -r`) and `keep` retention (0 = keep
  all, max 1000). At most one enabled schedule may cover a dataset: exact
  duplicates and recursive ancestor/descendant overlaps are refused, while a
  disabled duplicate is allowed. A 0.6.0 store with overlapping enabled
  schedules is migrated on load: the first in id order stays enabled and the
  rest are disabled with an explanatory `lastError`. Every run creates
  `naslos-YYYY-MM-DD_HH-MM-SS` (UTC, the `vfs_shadow_copy2` format) and records
  the owning schedule in each snapshot's `user:naslos:schedule` property; if
  that property write fails, the run removes the snapshots it just created
  instead of leaking an untagged name. Retention deletes only snapshots whose
  property matches that schedule's id, keeping the newest N per dataset.
  User-created snapshots are never touched. For one release after 0.6.0,
  property-less legacy `naslos-auto-<schedule>-…` names are still recognised as
  owned so old schedules do not orphan their snapshots.
- **Scrub schedule** (per pool): `kind: "scrub"`, daily or weekly only, plus
  **Scrub now** (`POST /api/volumes/zfs/{pool}/scrub`). The manual route answers
  `409` when `zpool status` reports a scrub or resilver already running; a
  scheduled run records `skipped` (or leaves its own in-flight result in place)
  instead of starting a second scrub. The agent runs only
  `zpool scrub <validated pool>`, and the schedule's result is read from the
  finished `scan` line (`ok` only with zero errors).

Schedules and their last/next-run state persist on the state volume
(`/var/lib/naslos/storage-schedules.json`, `api.storageSchedulesFile`), and a
run missed while the API was down fires once at startup. A run that cannot
reach the agent keeps its due time and is retried on the next tick, so an
upgrade does not consume a daily or weekly snapshot.

### Local replication (FR-REP)

`POST /api/replication/send` copies a source dataset to a destination dataset
on the **same node** as an async job (`202 {jobId}`), with
`GET /api/replication/jobs[/{id}]` for progress and
`DELETE /api/replication/jobs/{id}` to cancel. The API creates a
`naslos-rep-<pair token>-<UTC>-<random>` snapshot when none is supplied and pipes
the agent's `zfs send` stream into `zfs receive`; `recursive` uses `zfs send -r` (descendants, no property propagation).

- Both datasets must already exist (the destination is never created
  implicitly), and identical or nested paths are refused.
- Incremental vs full is chosen from the newest **snapshot GUID** the source and
  destination share, not from snapshot names.
- A full send replaces the destination dataset's contents, so the destination is
  a replica target: it is refused if the destination has a snapshot that does
  not carry this pair's `naslos-rep-…` prefix, or any child dataset. Incremental
  receives never use `-F`, so a user snapshot on the destination is never
  deleted.
- `pruneKeep` retains the newest N destination snapshots carrying this pair's
  prefix; only those names are eligible for deletion. Source snapshots created
  by replication are not pruned here.
- Encrypted/raw replication, remote replication, rsync and cloud sync are
  `[OPEN]` (see the spec's `FR-REP` group).

Plumbing added for this path: `zfs send` accepts `recursive=true` (`-r`) and
`/api/v1/zfs/snapshots/{dataset}` accepts `recursive=false`; every dataset and
snapshot name still goes through the existing validators before a command line.

### Creating a dataset (UI)

`Pools → <pool>` lists the pool's datasets and creates new ones. The API layer
(`POST /api/datasets`, `DELETE /api/datasets?name=…`) validates before anything
reaches ZFS:

| Rule | Why |
| --- | --- |
| Each name component is ZFS-safe, no traversal, no `@` | A name is interpolated into `zfs create`; `../` or a snapshot marker must not get through |
| Nested names allowed (`photos/2026`), created with `-p` | One call, and the intermediate levels are what the operator meant |
| Options limited to `compression`, `quota`, `recordsize`, `atime`, `copies`, `readonly` with validated values | `zfs create -o` accepts arbitrary properties; `mountpoint=/` would put the dataset somewhere unexpected |
| Encryption is raw-key only for now | `encryption` + `keyformat=raw` creates an encrypted dataset whose 32-byte key lives in a Secret in `naslos-keys`; an administrator must unlock it after every boot. Passphrase keys, encrypted send/receive and in-place conversion are `[OPEN]` (FR-ENC, [dataset-encryption.md](dataset-encryption.md)) |
| The pool must exist | Otherwise the failure surfaces after the name was accepted |
| Destroy needs `recursive=true` for a non-empty dataset, never applies to a pool's root dataset, and is refused (409) while a share serves the dataset's path | A dataset picker must not delete data or quietly break a share |

The UI's dataset form only asks for what matters (name, compression, quota) and
the pool page shows used/free per dataset.

### Block volumes (zvol)

A zvol is a ZFS block device (`/dev/zvol/<pool>/<name>`), not a mounted
filesystem. The API exposes it without changing the dataset surface:

| Route | Operation |
| --- | --- |
| `GET /api/zvols?pool=<pool>` | List zvols (omit `pool` for every pool) |
| `POST /api/zvols` | Create `{pool, name, size, options}` → `zfs create -p -V <size>` |
| `PUT /api/zvols?name=<pool>/<zvol>` | Resize `{size}` → `zfs set volsize=<size>` |
| `DELETE /api/zvols?name=<pool>/<zvol>&confirm=<pool>/<zvol>` | Destroy the zvol |

Rules the API and the agent both enforce (FR-ZVOL):

| Rule | Why |
| --- | --- |
| Name is validated exactly like a new dataset (strict per-component charset, no traversal, no `@`) | The name becomes an argv element of `zfs create`/`zfs destroy` |
| `size` is a validated ZFS size (`10G`, `512M`, `1.5T`); resize is growth-only | Shrinking a zvol can corrupt the filesystem inside it |
| Only `volblocksize` (512–16M, power of two) and `compression` are accepted | No arbitrary `zfs create -o` properties |
| Dataset-only options (`quota`, `recordsize`, `atime`, `copies`, `readonly`, `mountpoint`) are refused on the zvol path, and zvol-only options on the dataset path | A request cannot hop between the two APIs |
| Delete needs `confirm` equal to the full name and first checks `type=volume` | A same-named dataset must never be destroyed by mistake |
| Delete/resize is refused while the zvol is mounted, held by a sysfs holder, or open by a visible host/agent process; a ZFS `volume is busy` is mapped to the same refusal | A consumer has no chance to rescan or flush |
| A busy destroy is never retried with a forced unmount (unlike the dataset stale-mount ladder) | A busy zvol has a live consumer; taking it away is data loss |

There is no zvol UI yet and no iSCSI export: both are `[OPEN]` (FR-ZVOL-04).
The iSCSI spike, including the in-use consumer-registration requirement, is in
[iscsi.md](iscsi.md).

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
- `GET/POST/PUT/DELETE /api/v1/zvols` (list `?pool=`, create, resize, destroy; FR-ZVOL)
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

## Destructive-operation guards

Creating or extending a pool erases the selected disks, so three independent
checks stand between a request and `wipefs --all`:

1. **API: disk ownership and signatures.** The disk list comes from Talos
   (`get disks` / `get discoveredvolumes`), the system disk is excluded, and a
   disk that belongs to any pool is refused even with `force`. Signatures are
   attributed to their parent disk: a disk holding a **`zfs_member`** label is
   refused outright ("import or destroy that pool first"), and any other
   filesystem requires the disk to appear in the request's `wipeConfirmed`
   list. Ownership and signature lookups **fail closed**: if either cannot be
   read, the request is rejected rather than assumed safe.
2. **Agent: canonical device identity.** Request paths are canonicalized
   (`/dev//sdb`, `/dev/disk/by-id/...` and `/dev/mapper/...` all resolve to the
   real node) and must be whole disks. Pool membership is keyed by the same
   canonical name, so an alias cannot hide a member.
3. **Agent: strict membership and exported pools.** Destructive paths use
   `pools_strict()`, which fails if any `zpool status` cannot be read (a
   suspended pool is exactly that case), and refuse any disk that appears in
   `zpool import` output — the label would be destroyed before the import flow
   could recover it. A process-wide mutex serializes check → wipe → create, so
   two concurrent requests cannot both claim one disk.

The UI mirrors the rules: disks holding a ZFS label are not selectable, a disk
with existing data shows a signature badge, and the create button stays
disabled until the operator ticks "Erase this disk and everything on it".

## Pool destroy and import-all

`DELETE /api/volumes/zfs/{pool}` requires `confirm=<pool>`. `zpool destroy` runs
**without** `-f` unless the request also sends `force=true`, so a pool whose
datasets are mounted or busy fails instead of being torn down. The agent's
`DELETE /api/v1/pools/{pool}` applies the same rule; there is no unconfirmed
destroy path.

`POST /api/volumes/zfs/import` requires either a pool name or an explicit
`{"all": true}`. A malformed or empty body is rejected, because the previous
fallback imported every importable pool with `-f`.

Concurrent `zfs send`/`receive` streams are capped (`agent.maxStreams`,
`AGENT_MAX_STREAMS`, default 8) and a restore fails after
`agent.streamIdleTimeoutSecs` (default 300 s) without body data. A local
replication job holds two slots (its send and its receive), and a Buddy transfer
uses the same pool, so size the cap for the number of concurrent transfers plus
headroom.
