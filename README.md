# Naslos

A user-friendly NAS distribution built on **stock Talos Linux** — no base modification, so Talos upgrades stay clean via `talosctl upgrade`.

> ⚠️ **AI Use Disclaimer**
> This project was developed with the assistance of AI tools (AI-assisted coding). While every effort has been made to ensure correctness, security, and best practices, this software is provided **as-is** without warranty of any kind.
>
> **Please be aware:**
> - AI-generated code may contain bugs, security vulnerabilities, or suboptimal patterns that are not immediately obvious.
> - You should **thoroughly review, test, and audit** all code before deploying it in any production environment or connecting it to untrusted networks.
> - The authors and contributors **accept no liability** for data loss, security breaches, system damage, or any other consequences arising from the use of this software.
> - Critical infrastructure (including NAS/storage systems holding valuable data) demands **independent verification** — do not rely solely on AI-generated implementations.
>
> **Use at your own risk.** Always maintain backups of your data.

## Why Talos

Talos is an immutable, API-driven, minimal Linux OS purpose-built for Kubernetes. Naslos layers on top of it as machine-config documents, Helm charts, and a management UI — nothing is forked.

## Features

- **ZFS** — official `siderolabs/zfs` extension; pools auto-import at boot via `zfs-service`
- **Accessible UI** — Svelte 5 + Tailwind design-token system with a persisted
  light/dark theme (no-FOUC bootstrap, `prefers-color-scheme` default), a
  grouped collapsible sidebar, SVG icon set (Lucide), shared modal/loading/
  empty/error/status components, and an axe-core WCAG 2.x scan of every route
  in both themes in the e2e suite
- **Disk setup wizard** — topology advisor (mirror / RAIDZ1 / RAIDZ2), best-practice tuning (`ashift=12`, `compression=zstd`, `xattr=sa`, `acltype=posixacl`, `atime=off`)
- **Pool health page** — live device tree, IO stats, scan state, and error summary from `zpool status` / `zpool iostat`
- **Scheduled storage maintenance** — periodic snapshot tasks per dataset (hourly/daily/weekly, keep-N retention, optional recursive) and per-pool scrub schedules, with a manual "scrub now", last/next-run state and results on the pool page
- **Disk health** — per-disk SMART (read-only, standby-safe) plus ZFS vdev state/error counters, with temperature/wear threshold alerts
- **CPU detail** — model, per-core load/frequency, load averages and CPU temperature (when the hardware exposes it) on the dashboard
- **Import existing pools** — discover and import pools already on disk via `zpool import`
- **App catalog** — install apps from git-based chart repositories (official +
  user-added), with schema-driven config and per-app exposure (subdomain, TLS,
  Authelia auth, local-only) rendered as Traefik routes; install/upgrade/
  uninstall run as background jobs with live progress; the Installed tab shows
  compact rows with live CPU/RAM/network usage and update badges (one-click
  per-app or **Update all** bulk upgrades with a version confirmation) plus a
  per-app detail drawer; the update badge also covers newly released upstream
  images for floating tags, and applying it forces a re-pull; the catalog
  refreshes in the background at the cache TTL, and domains and wildcard ACME
  certificates are managed from the UI. Per-app custom environment variables
  are stored in a Kubernetes Secret and mounted only into the app's main
  container(s), and a base domain can opt into automatic per-app CNAMEs pointing
  at a DDNS hostname (see [docs/app-catalog.md](docs/app-catalog.md) and
  [docs/dynamic-dns.md](docs/dynamic-dns.md)).
- **Logs & terminal** — stream logs and open an interactive shell (`bash` by default; `sh`/`ash`/`zsh` where installed) in any pod from the UI
- **MCP server** — let AI assistants manage the NAS through the Model Context
  Protocol: a Streamable HTTP endpoint at `/mcp` inside the API, off by
  default, with scoped bearer tokens (read / write / full), a `confirm` guard
  on destructive tools, audited calls and a Settings page
  (see [docs/mcp.md](docs/mcp.md))
- **System config export** — download a versioned, secret-free JSON snapshot
  of shares, snapshot/scrub and buddy schedules, domains, DDNS entries (without
  credentials), app records and registered app datasets; import/restore is
  planned. This is application config, not the installer recovery ZIP
  (see [docs/api.md](docs/api.md#system-config-export-fr-cfg-see-specmd-315))
- **Virtual machines** — a first-party QEMU/KVM hypervisor on the node
  (`rust/vmd` supervisor): VM datasets under `<pool>/vms/<name>` with quota =
  declared disk, UEFI/BIOS firmware, NAT with editable port forwards (LAN
  attachment via a host bridge is planned), browser console (noVNC over an
  owner-gated websocket bridge), cloud-init NoCloud seeds (keys only, no
  passwords), an ISO/cloud-image library with chunked browser upload and
  SSRF-guarded URL import, and ZFS snapshots/rollback per VM
  (see [docs/virtual-machines.md](docs/virtual-machines.md))
- **Shares** — SMB, NFS, and Time Machine (SMB with the fruit VFS; AFP is not served)
- **ntfy notifications** — push alerts on backup and ZFS/agent health transitions
- **Network settings** — day-2 host link addressing, MTU and DNS changes from the UI, applied with a machine-side `--mode=try` guard, an API watchdog and a saved-config revert (see [docs/deployment.md](docs/deployment.md#day-2-network-changes-fr-net))
- **Monitoring dashboard** — live metrics API (`/api/metrics`) + home dashboard; no bundled metrics stack
- **Desktop installer** — a one-app installer (separate
  [Naslos-Installer](https://github.com/AessemOps/Naslos-Installer) repo) that
  provisions the Naslos node from a machine booted from the Talos ISO using this
  repo's versioned **install pack**
  (`make install-pack`), with a progress bar, first-admin + 2FA handoff and a
  recovery ZIP; the stable interfaces are in
  [docs/installer-contract.md](docs/installer-contract.md)
- **Web upgrades** — installer-provisioned nodes update Naslos, then Talos,
  then Kubernetes from Settings → System: verified release packs, one operation
  at a time, etcd snapshots, Helm rollback, A/B rollback detection and a
  progress panel that survives the reboot (see
  [docs/installer-contract.md §7](docs/installer-contract.md#7-web-upgrades-fr-upd))
- **Published images + release CI** — every PR runs the full gate sweep,
  supply-chain scan, secret scan and a Docker build of all images with an
  image CVE gate and SBOM; a semantic `vX.Y.Z` tag pushes
  all 9 images to `ghcr.io/aessemops/naslos-*` and publishes the install pack
  with every chart image pinned by digest (`FR-INSTALL-13`). See
  [docs/deployment.md](docs/deployment.md)

## Architecture

```
rust/agent/  Rust DaemonSet, privileged — executes zpool/zfs via chroot /host
rust/api/    Rust API — Talos/K8s/ZFS orchestration + buddyctl/buddy-receiver
rust/vmd/    Rust QEMU/KVM supervisor (naslos-vmd) — privileged, QMP-controlled
ui/          SvelteKit + Tailwind — dashboard, wizards, terminal, VM console
charts/      naslos umbrella chart (api, ui, agent, vmd, openldap, samba, nfs, terminal, Traefik/Authelia)
samba/       Samba image + config
nfs/         NFS-Ganesha image + config
terminal/    web-terminal image
vmd/         naslos-vmd image (QEMU supervisor runtime)
openldap/    OpenLDAP SSO image (identity store; the workloads live in charts/naslos)
bootstrap/   schematic + ISO generator, Cilium manifest, VM + installer machine-config patches
scripts/     deploy-vm.sh, render-cilium.sh, build-install-pack.sh, audit.sh
.github/     CI + release workflows (gate sweep, image push, digest-pinned pack)
docs/        architecture, spec, API, storage, identity, catalog, installer contract, ops docs
```

## Documentation

The full documentation set lives in [`docs/`](docs/README.md):

| Doc | Topic |
| --- | --- |
| [architecture](docs/architecture.md) | Design principles + system/deployment/component diagrams, data flows |
| [api](docs/api.md) | HTTP API reference (naslos-api + naslos-agent) |
| [bootstrap](docs/bootstrap.md) | Image Factory schematic & ZFS extension |
| [storage-zfs](docs/storage-zfs.md) | ZFS pools, datasets, snapshots, storage classes |
| [dataset-encryption](docs/dataset-encryption.md) | ZFS native encryption design: key custody, Talos boot/unlock, encrypted send, migration (FR-ENC) |
| [identity-sso](docs/identity-sso.md) | Authelia + OpenLDAP + Samba single sign-on |
| [rbac-audit](docs/rbac-audit.md) | Read-only/operator/admin roles + append-only audit log (design, `[OPEN]`) |
| [shares](docs/shares.md) | SMB / NFS / Time-Machine shares |
| [app-catalog](docs/app-catalog.md) | Catalog, schema-driven forms, Helm lifecycle |
| [monitoring](docs/monitoring.md) | Live metrics API + dashboard (no Prometheus) |
| [notifications](docs/notifications.md) | ntfy alerts |
| [deployment](docs/deployment.md) | Prerequisites, Make targets, Helm values |
| [installer-contract](docs/installer-contract.md) | Install pack + interfaces for the desktop installer (FR-INSTALL) |
| [operations](docs/operations.md) | Backups, restore, troubleshooting |
| [development](docs/development.md) | Layout, builds, extending the catalog |
| [audit-report](docs/AUDIT-2026-10-24.md) | Current post-Rust audit: findings, fixes, open/accepted risks (pre-Rust report kept in `docs/AUDIT-2026-09-19-REPORT.md`) |

## Requirements

- [talosctl](https://www.talos.dev/latest/talosctl/installation/) v1.14+
- [kubectl](https://kubernetes.io/docs/tasks/tools/)
- [helm](https://helm.sh/docs/intro/install/)
- [docker](https://docs.docker.com/get-docker/) or [podman](https://podman.io/)
- [rust](https://rustup.rs/) 1.88+ (for building the agent and API)
- [node](https://nodejs.org/) 20+ (for building ui)

## Quick start

```bash
# Build everything
make all

# Run a local dev cluster
make dev-cluster

# Access the UI
kubectl port-forward -n naslos svc/naslos-ui 8080:80
```

## License

GNU Affero General Public License v3.0 — see [LICENSE](LICENSE).
Third-party open-source components and their licenses are listed in
[CREDITS.md](CREDITS.md).
