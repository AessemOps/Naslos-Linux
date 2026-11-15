# Disk Health: resolve pool membership for partition vdevs

Status: implemented · Repo: `Naslos-Linux` (agent + API)

## Problem

The Disk Health page shows `—` in **Pool / vdev** and **ZFS** for `/dev/sdb`
and `/dev/sdc` even though both belong to the `Data` pool (the main vdev is
`/dev/sdc2` and `/dev/sdb1` is the cache vdev).

Two independent causes, both in the membership path:

1. `ZfsClient::disk_health` parses plain `zpool status`. For a pool imported by
   path/GUID the config rows name the stored id (`44b52a63-…`) or the partition
   (`sdc2`), and `canonical_zfs_device` only accepted whole disks, so the member
   was dropped instead of attributed to `/dev/sdc` / `/dev/sdb`.
2. `parse_pool_health` skipped group rows (`cache`, `logs`, `spares`,
   `special`, `dedup`) because they carry no READ/WRITE/CKSUM columns, so the
   cache member was labelled with its own device name instead of `cache`.

## Fix

- `rust/agent/src/zfs/operations.rs`
  - `disk_health` reads `zpool status -L` (`pool_status_kernel`), so config rows
    use kernel device names (`sdc2`) even for a GUID/path-imported pool.
  - `canonical_zfs_device` accepts partition names; `collect_disk_health` maps
    a partition member to its whole disk with `whole_disk_path`.
  - `parse_pool_health` keeps non-leaf group rows in the config tree, so a
    cache/log/special member is attributed to that group as its vdev.
- `rust/api/src/server/disks.rs`
  - After merging the agent report, `GET /api/disks/health` fills `inPool` from
    `list_pools` (whole-disk mapping included) when the agent did not report a
    `zfs` entry, so a pool name is still visible with an older agent or an
    unparsed vdev.

## Tests

- Agent: `disk_health_flattens_vdevs_to_whole_disks` now asserts the `-L`
  argument; new `disk_health_maps_partition_members_to_whole_disks` covers a
  GUID-imported `Data` pool with `sdc2` + cache `sdb1`.
- API: `disk_health_fills_the_pool_when_the_agent_omits_membership`.
- Gates: `cargo fmt`, `cargo test -p naslos-agent`, `cargo test -p naslos-api`,
  `cargo clippy -p naslos-agent -p naslos-api --all-targets -- -D warnings`.

## Deploy note

Ships with the next Naslos release; no chart/UI change is required. After the
release, the page must show `Data` (linked) plus vdev `sdc2` for `/dev/sdc` and
vdev `cache` for `/dev/sdb`.
