# NasOS Architecture

## Design Principles

1. **Stock Talos only** — No base modification. All NasOS functionality is delivered as machine-config documents, Helm charts, and containers. Talos upgrades stay clean via `talosctl upgrade`.

2. **ZFS as the filesystem** — Uses the official `siderolabs/zfs` extension. Pools auto-import at boot via `zfs-service` (`zpool import -fal`).

3. **Privileged agent for ZFS** — Since ZFS pools live outside Talos's volume system (which only supports ext4/xfs/btrfs), a privileged DaemonSet executes `zpool`/`zfs` commands via `chroot /host`.

4. **Web terminal for shell access** — Talos has no shell by design. NasOS provides a zsh web terminal in a privileged container with host mounts.

## Component Diagram

```
┌─────────────────────────────────────────────────────────┐
│                        Browser                          │
│  ┌─────────┐ ┌─────────┐ ┌──────────┐ ┌─────────────┐ │
│  │Dashboard│ │  Disk   │ │   App    │ │  Terminal   │ │
│  │ (home)  │ │ Wizard  │ │ Catalog  │ │   (zsh)     │ │
│  └────┬────┘ └────┬────┘ └────┬─────┘ └──────┬──────┘ │
└───────┼───────────┼───────────┼───────────────┼────────┘
        │           │           │               │
┌───────▼───────────▼───────────▼───────────────▼────────┐
│                    nasos-api (Go)                       │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌───────────┐ │
│  │  Talos   │ │  K8s     │ │   ZFS    │ │   Helm    │ │
│  │  Client  │ │  Client  │ │  Advisor │ │   SDK     │ │
│  └──────────┘ └──────────┘ └──────────┘ └───────────┘ │
└───────┬──────────────────────────────────┬─────────────┘
        │                                  │
┌───────▼──────────────┐    ┌──────────────▼─────────────┐
│   Talos API          │    │   nasos-agent (DaemonSet)  │
│  (machine config,    │    │   chroot /host zpool/zfs   │
│   volumes, metrics)  │    │   privileged, host mounts  │
└──────────────────────┘    └─────────────────────────────┘
```

## ZFS Pool Lifecycle

1. **Discovery** — `talosctl get discoveredvolumes` lists available disks
2. **Recommendation** — VolumeAdvisor suggests topology (mirror/raidz1/raidz2/raidz3)
3. **Creation** — Agent runs `zpool create -f -o ashift=12 -O compression=zstd ...`
4. **Boot persistence** — `zfs-service` extension runs `zpool import -fal` at every boot
5. **Shutdown** — `zfs-service` runs `zfs unmount -au` + `zpool export -a`

## Talos-Specific ZFS Gotchas

- **SELinux**: Imported pools need `zfs set context=none fscontext=none defcontext=none rootcontext=none <pool>`
- **Foreign pools**: Samba/NFS share properties from other NAS OSes must be cleared
- **Mount path**: Pools must mount under `/var/<name>` (Talos's persistent writable path)
- **Noexec**: `/var` defaults to noexec in 1.14, but ZFS pool mounts are separate mounts

## Security Model

- API server runs with Talos ServiceAccount (os:reader role)
- Agent runs privileged but only for ZFS operations
- Web terminal is ephemeral — no persistent state
- All operations are auditable via Kubernetes events

## Upgrades

Talos upgrades are unaffected because:
- ZFS extension is in the Image Factory schematic
- NasOS components are Kubernetes workloads (survive node reboots)
- ZFS pools are not in Talos's volume system
- Machine config patches are reapplied by the API after upgrade
