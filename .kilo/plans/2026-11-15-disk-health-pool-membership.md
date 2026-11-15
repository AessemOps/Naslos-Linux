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

## Review follow-ups (2026-11-15)

The first PR (#177) merged before the review fixes landed. A follow-up branch
applies them:

- **One disk, several members.** Two ZFS members on one physical disk (a data
  partition plus a log/cache partition) used to overwrite each other in the
  per-whole-disk map. The merge now keeps the worse state, adds the error
  counters, and keeps both vdev labels (`sdb1, logs`).
- **Group rows stay internal.** `parse_pool_health` (the pool page) keeps its
  previous shape; only `disk_health` parses with `keep_groups`, so `cache` is a
  vdev label there but not a top-level `PoolHealth.config` entry.
- **No duplicate enumeration.** The API only calls `list_pools` when the agent
  report succeeded and a non-system disk is still without membership.
- **One status wrapper.** `pool_status`/`pool_status_kernel` share
  `pool_status_with(name, -L)`, and `pool_health` uses `pool_status`, so no
  test-only wrapper remains.

Tests: `disk_health_merges_members_on_one_disk` and
`pool_health_config_omits_group_rows` (agent); the existing API fallback test
still covers the gated `list_pools` path.
