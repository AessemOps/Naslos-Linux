# NasOS Documentation

Welcome to the **NasOS** documentation set. NasOS is a user-friendly NAS
distribution built on top of **stock Talos Linux** — no base modification.
Everything (ZFS, app catalog, shares, identity/SSO, monitoring, notifications)
is delivered as a Kubernetes workload, a Helm chart, or an Image Factory
schematic, so `talosctl upgrade` keeps working cleanly.

This index is the recommended entry point. Each document is self-contained;
read them in order for the full picture, or jump straight to a topic.

## Document index

| Document | Covers | Status |
| --- | --- | --- |
| [architecture.md](architecture.md) | Design principles, system-context / deployment / component diagrams, data flows, trust map | Rebuilt |
| [api.md](api.md) | HTTP API surface of `nasos-api` and `nasos-agent`, env vars, error format | New |
| [bootstrap.md](bootstrap.md) | Image Factory schematic, ZFS extension, boot-time pool import, upgrades | New |
| [storage-zfs.md](storage-zfs.md) | ZFS pools, datasets, snapshots, topology advisor, Talos gotchas | New |
| [identity-sso.md](identity-sso.md) | Single sign-on: Authelia, OpenLDAP, Samba hash sync, RBAC groups, 2FA | Rebuilt (replaces `authentication.md`) |
| [shares.md](shares.md) | SMB / NFS / Time-Machine share model, generated configs | New |
| [app-catalog.md](app-catalog.md) | App catalog, JSON-Schema-driven forms, Helm lifecycle, start/stop | New |
| [monitoring.md](monitoring.md) | Metrics API, Prometheus + Grafana dashboard | New |
| [notifications.md](notifications.md) | ntfy alerting: topics, severities, defaults | New |
| [deployment.md](deployment.md) | Prerequisites, Make targets, Helm values walkthrough | New |
| [operations.md](operations.md) | Day-2: LDAP backup/restore, troubleshooting | New |
| [development.md](development.md) | Repo layout, how to build, extend the catalog/shares | New |

## Reading order

1. **[architecture.md](architecture.md)** — how the whole system fits together.
2. **[api.md](api.md)** and **[development.md](development.md)** — the concrete contracts and where code lives.
3. **[bootstrap.md](bootstrap.md)** + **[storage-zfs.md](storage-zfs.md)** — how the OS image and storage are built.
4. **[identity-sso.md](identity-sso.md)** + **[shares.md](shares.md)** — how users log in everywhere.
5. **[app-catalog.md](app-catalog.md)**, **[monitoring.md](monitoring.md)**, **[notifications.md](notifications.md)** — feature docs.
6. **[deployment.md](deployment.md)** + **[operations.md](operations.md)** — run and operate it.

## Conventions

- All diagrams are ASCII and render in any Markdown viewer.
- “Talos” always refers to stock Talos Linux; “NasOS” is the integration layer
  on top (schematic, chart, agents).
- All components run in the `nasos` Kubernetes namespace unless noted.
- Command examples assume `kubectl`/`talosctl`/`helm` are available and pointed
  at the right cluster.