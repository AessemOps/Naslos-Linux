# NasOS

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

Talos is an immutable, API-driven, minimal Linux OS purpose-built for Kubernetes. NasOS layers on top of it as machine-config documents, Helm charts, and a management UI — nothing is forked.

## Features

- **ZFS** — official `siderolabs/zfs` extension; pools auto-import at boot via `zfs-service`
- **Disk setup wizard** — topology advisor (mirror / RAIDZ1 / RAIDZ2), best-practice tuning (`ashift=12`, `compression=zstd`, `xattr=sa`, `acltype=posixacl`, `atime=off`)
- **App catalog** — deploy and configure apps via Helm with schema-driven forms
- **Logs & terminal** — stream logs and open a zsh shell to any pod from the UI
- **Start/stop apps** — scale replicas 0↔N, preserving PVCs
- **Shares** — SMB, NFS, and Time Machine (AFP) over ZFS datasets
- **ntfy notifications** — push alerts on Talos / Kubernetes / ZFS events
- **Monitoring dashboard** — Prometheus + Grafana home screen

## Architecture

```
api/         Go — Talos API + K8s API + ZFS orchestration (the brain)
agent/       Go DaemonSet, privileged — executes zpool/zfs via chroot /host
ui/          SvelteKit + Tailwind — dashboard, wizards, terminal
catalog/     Helm chart repo — the app store
charts/      nasos umbrella chart (api, ui, agent, ntfy, shares, monitoring)
shares/      Samba + NFS-Ganesha + Avahi images/config
bootstrap/   schematic + ISO generator, first-boot wizard
```

## Requirements

- [talosctl](https://www.talos.dev/latest/talosctl/installation/) v1.14+
- [kubectl](https://kubernetes.io/docs/tasks/tools/)
- [helm](https://helm.sh/docs/intro/install/)
- [docker](https://docs.docker.com/get-docker/) or [podman](https://podman.io/)
- [go](https://go.dev/) 1.22+ (for building api/agent)
- [node](https://nodejs.org/) 20+ (for building ui)

## Quick start

```bash
# Build everything
make all

# Run a local dev cluster
make dev-cluster

# Access the UI
kubectl port-forward -n nasos svc/nasos-ui 8080:80
```

## License

Apache 2.0
