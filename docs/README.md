# Naslos Documentation

Welcome to the **Naslos** documentation set. Naslos is a user-friendly NAS
distribution built on top of **stock Talos Linux** — no base modification.
Everything (ZFS, app catalog, shares, identity/SSO, monitoring, notifications)
is delivered as a Kubernetes workload, a Helm chart, or an Image Factory
schematic, so `talosctl upgrade` keeps working cleanly.

This index is the recommended entry point. Each document is self-contained;
read them in order for the full picture, or jump straight to a topic.

## Document index

| Document | Covers | Status |
| --- | --- | --- |
| [spec.md](spec.md) | **Normative system specification** — requirements (FR/SEC/NFR IDs), API & data contracts, acceptance criteria, conformance | Authoritative |
| [architecture.md](architecture.md) | Design principles, system-context / deployment / component diagrams, data flows, trust map | Rebuilt |
| [api.md](api.md) | HTTP API surface of `naslos-api` and `naslos-agent`, env vars, error format | New |
| [bootstrap.md](bootstrap.md) | Image Factory schematic, ZFS extension, boot-time pool import, upgrades | New |
| [storage-zfs.md](storage-zfs.md) | ZFS pools, datasets, snapshots, topology advisor, Talos gotchas | New |
| [identity-sso.md](identity-sso.md) | Single sign-on: Authelia, OpenLDAP, Samba hash sync, RBAC groups, 2FA | Rebuilt (replaces `authentication.md`) |
| [shares.md](shares.md) | SMB / NFS / Time-Machine share model, generated configs | New |
| [terminal.md](terminal.md) | Web terminal: exec scoping, RBAC, terminal namespace | New |
| [buddy-backup.md](buddy-backup.md) | Buddy Backup: owner-to-owner encrypted backup, enrollment, verify/restore | New |
| [app-catalog.md](app-catalog.md) | App catalog, JSON-Schema-driven forms, Helm lifecycle | New |
| [monitoring.md](monitoring.md) | Metrics API, Prometheus + Alertmanager (Grafana removed) | New |
| [notifications.md](notifications.md) | ntfy alerting: topics, severities, defaults | New |
| [deployment.md](deployment.md) | Prerequisites, Make targets, Helm values walkthrough | New |
| [operations.md](operations.md) | Day-2: LDAP backup/restore, troubleshooting | New |
| [development.md](development.md) | Repo layout, how to build, extend the catalog/shares | New |
| [AUDIT-2026-09-19-REPORT.md](AUDIT-2026-09-19-REPORT.md) | **The audit and fix report**: findings, every security and code fix with commit/revision, verification, remaining work | Current |

The detailed working documents behind the report — the audit findings
(`AUDIT-2026-09-19.md`), its fix plan (`AUDIT-2026-09-19-FIXPLAN.md`), the
code-review list (`CODE-REVIEW.md`) and the superseded 2026-09-14 audit
(`SECURITY-AUDIT.md`, `SECURITY-FIX-PLAN.md`) — are archived under
[`archive/`](archive/) and kept for traceability. Historical implementation
plans live in [`plans/`](plans/).

## Reading order

0. **[spec.md](spec.md)** — the normative requirements and contracts everything else implements.
2. **[api.md](api.md)** and **[development.md](development.md)** — the concrete contracts and where code lives.
3. **[bootstrap.md](bootstrap.md)** + **[storage-zfs.md](storage-zfs.md)** — how the OS image and storage are built.
4. **[identity-sso.md](identity-sso.md)** + **[shares.md](shares.md)** — how users log in everywhere.
5. **[app-catalog.md](app-catalog.md)**, **[monitoring.md](monitoring.md)**, **[notifications.md](notifications.md)** — feature docs.
6. **[deployment.md](deployment.md)** + **[operations.md](operations.md)** — run and operate it.

## Conventions

- All diagrams are ASCII and render in any Markdown viewer.
- “Talos” always refers to stock Talos Linux; “Naslos” is the integration layer
  on top (schematic, chart, agents).
- The authenticated services run in the `naslos` namespace; the
  hostNetwork/privileged workloads (agent, samba, nfs, terminal) run in
  `naslos-privileged` (AUDIT-M6). Documents name the namespace where it matters.
- Command examples assume `kubectl`/`talosctl`/`helm` are available and pointed
  at the right cluster.