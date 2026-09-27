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
- **Disk setup wizard** — topology advisor (mirror / RAIDZ1 / RAIDZ2), best-practice tuning (`ashift=12`, `compression=zstd`, `xattr=sa`, `acltype=posixacl`, `atime=off`)
- **Pool health page** — live device tree, IO stats, scan state, and error summary from `zpool status` / `zpool iostat`
- **Import existing pools** — discover and import pools already on disk via `zpool import`
- **App catalog** — install apps from git-based chart repositories (official +
  user-added), with schema-driven config and per-app exposure (subdomain, TLS,
  Authelia auth, local-only) rendered as Traefik routes; install/upgrade/
  uninstall run as background jobs with live progress; domains and wildcard
  ACME certificates managed from the UI (see [docs/app-catalog.md](docs/app-catalog.md)).
- **Logs & terminal** — stream logs and open a zsh shell to any pod from the UI
- **Shares** — SMB, NFS, and Time Machine (SMB with the fruit VFS; AFP is not served)
- **ntfy notifications** — push alerts on Talos / Kubernetes / ZFS / backup events
- **Monitoring dashboard** — Prometheus + Alertmanager (Grafana was removed)
- **Desktop installer** — a one-app installer (separate
  [Naslos-Installer](https://github.com/AessemOps/Naslos-Installer) repo) that
  provisions a fresh Talos node from this repo's versioned **install pack**
  (`make install-pack`), with a progress bar, first-admin + 2FA handoff and a
  recovery ZIP; the stable interfaces are in
  [docs/installer-contract.md](docs/installer-contract.md)

## Architecture

```
api/         Go — Talos API + K8s API + ZFS orchestration (the brain)
agent/       Go DaemonSet, privileged — executes zpool/zfs via chroot /host
ui/          SvelteKit + Tailwind — dashboard, wizards, terminal
charts/      naslos umbrella chart (api, ui, agent, openldap, monitoring, shares)
samba/       Samba image + config
nfs/         NFS-Ganesha image + config
terminal/    web-terminal image (+ zsh-terminal/)
openldap/    OpenLDAP SSO image (identity store; the workloads live in charts/naslos)
bootstrap/   schematic + ISO generator, Cilium manifest, VM + installer machine-config patches
scripts/     deploy-vm.sh, render-cilium.sh, build-install-pack.sh, audit.sh
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
| [identity-sso](docs/identity-sso.md) | Authelia + OpenLDAP + Samba single sign-on |
| [shares](docs/shares.md) | SMB / NFS / Time-Machine shares |
| [app-catalog](docs/app-catalog.md) | Catalog, schema-driven forms, Helm lifecycle |
| [monitoring](docs/monitoring.md) | Metrics API + Prometheus/Alertmanager (Grafana removed) |
| [notifications](docs/notifications.md) | ntfy alerts |
| [deployment](docs/deployment.md) | Prerequisites, Make targets, Helm values |
| [installer-contract](docs/installer-contract.md) | Install pack + interfaces for the desktop installer (FR-INSTALL) |
| [operations](docs/operations.md) | Backups, restore, troubleshooting |
| [development](docs/development.md) | Layout, builds, extending the catalog |
| [audit-report](docs/AUDIT-2026-09-19-REPORT.md) | Audit and fix report: findings, fixes, verification, remaining work (archived working papers in `docs/archive/`) |

## Requirements

- [talosctl](https://www.talos.dev/latest/talosctl/installation/) v1.14+
- [kubectl](https://kubernetes.io/docs/tasks/tools/)
- [helm](https://helm.sh/docs/intro/install/)
- [docker](https://docs.docker.com/get-docker/) or [podman](https://podman.io/)
- [go](https://go.dev/) 1.26.5+ (for building api/agent)
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
