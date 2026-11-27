# Naslos Documentation

Naslos is a NAS distribution built on **stock Talos Linux**. The OS stays
immutable and API-managed: everything Naslos adds (ZFS, shares, identity, the app
catalog, monitoring, notifications, backup) ships as a Kubernetes workload, a
Helm chart, or an Image Factory schematic, so `talosctl upgrade` keeps working.

This index is the entry point. Each document is self-contained: read them in
order for the full picture, or jump to the topic you need.

## Start here

| If you want to... | Read |
| --- | --- |
| Know what Naslos must do | [spec.md](spec.md) |
| Understand how the pieces fit | [architecture.md](architecture.md) |
| Call the API | [api.md](api.md) |
| Build or run it | [development.md](development.md), [deployment.md](deployment.md) |
| Operate a live node | [operations.md](operations.md) |

## Document index

| Document | Covers | Status |
| --- | --- | --- |
| [spec.md](spec.md) | **Normative system specification**: requirements (FR/SEC/NFR IDs), API and data contracts, acceptance criteria, conformance | Authoritative |
| [architecture.md](architecture.md) | Design principles, system-context / deployment / component diagrams, data flows, trust map | Rebuilt |
| [api.md](api.md) | HTTP API surface of `naslos-api` and `naslos-agent`, env vars, error format | New |
| [bootstrap.md](bootstrap.md) | Image Factory schematic, ZFS extension, boot-time pool import, upgrades | New |
| [storage-zfs.md](storage-zfs.md) | ZFS pools, datasets, zvols, snapshots, topology advisor, Talos gotchas | New |
| [dataset-encryption.md](dataset-encryption.md) | ZFS native encryption: Secret-backed raw-key custody, dataset create, keystatus, explicit unlock/lock, encrypted send, Buddy threat-model comparison, migration (FR-ENC) | Raw-key path implemented; passphrase/send/migration `[OPEN]` |
| [iscsi.md](iscsi.md) | iSCSI target spike: Talos kernel constraint, userspace `tgt` vs a system extension, go/no-go gates, privilege and NetworkPolicy review | Spike (`[OPEN]`) |
| [nvmeof.md](nvmeof.md) | NVMe-oF TCP spike: Talos `nvmet` evidence, SPDK and extension alternatives, security, performance, failure modes, go/no-go | Spike |
| [identity-sso.md](identity-sso.md) | Single sign-on: Authelia, OpenLDAP, Samba hash sync, RBAC groups, 2FA | Rebuilt (replaces `authentication.md`) |
| [external-directory.md](external-directory.md) | Optional external LDAP source (FR-EXT): phase-1 configuration, TLS, search bases, uid/gid mapping, AD/Kerberos roadmap | New |
| [rbac-audit.md](rbac-audit.md) | Read-only/operator/admin role model, route classification, append-only hash-chained audit log (FR-RBAC/FR-AUD) | Phase 2 + audit v1 implemented; delegation/auth/UI phases open |
| [shares.md](shares.md) | SMB / NFS / Time-Machine share model, generated configs | New |
| [nfsv3.md](nfsv3.md) | Why NFSv3 is not served, and what would be required | Current |
| [acl.md](acl.md) | Dataset/share ACL decision spike: POSIX presets vs NFSv4 + Samba mapping, migration fallout, recommendation | New |
| [terminal.md](terminal.md) | Web terminal: exec scoping, RBAC, terminal namespace | New |
| [buddy-backup.md](buddy-backup.md) | Buddy Backup: owner-to-owner encrypted backup, enrollment, verify/restore | New |
| [app-catalog.md](app-catalog.md) | App catalog: git chart repositories, `naslos-app.yaml`, install lifecycle, exposure/routing, domains/certs, privileged namespace | Rebuilt |
| [datasets-isolation.md](datasets-isolation.md) | Isolating installed apps from the shared datasets root (D3/F1): current model, options, migration, residual risk | Current |
| [virtual-machines.md](virtual-machines.md) | QEMU/KVM supervisor: VM datasets, firmware, NAT port forwards, noVNC console, cloud-init, ISO library, snapshots | New |
| [dynamic-dns.md](dynamic-dns.md) | Dynamic DNS and declarative DNS providers: provider YAML, entries, credential Secrets, drivers, egress | New |
| [mcp.md](mcp.md) | MCP server: Streamable HTTP endpoint, scoped tokens, destructive-tool guard, audit | New |
| [monitoring.md](monitoring.md) | Live metrics API and dashboard; node/workload measurement without Prometheus | New |
| [reporting.md](reporting.md) | Bounded historical reporting (24 h / 7 d / 30 d, embedded append-only store, API and dashboard card, no Prometheus) | Implemented subset (`FR-RPT-05…07` `[OPEN]`) |
| [notifications.md](notifications.md) | ntfy + webhook + SMTP + syslog alert channels: routing, severities, defaults, target policy | New |
| [deployment.md](deployment.md) | Prerequisites, Make targets, Helm values walkthrough | New |
| [installer-contract.md](installer-contract.md) | Install pack format and stable interfaces for the desktop installer (FR-INSTALL) | New |
| [operations.md](operations.md) | Day-2: LDAP backup/restore, troubleshooting | New |
| [development.md](development.md) | Repo layout, how to build, extend the catalog and shares | New |
| [AUDIT-2026-11-23.md](AUDIT-2026-11-23.md) | **Current full code review** (v0.7.1): findings per subsystem, verified blockers, ordered remediation plan | Current |
| [AUDIT-2026-11-11.md](AUDIT-2026-11-11.md) | Full audit of v0.3.21 against the live node, audit only | Superseded |
| [AUDIT-2026-10-24.md](AUDIT-2026-10-24.md) | Post-Rust migration security and best-practice audit | Superseded |
| [AUDIT-2026-09-19-REPORT.md](AUDIT-2026-09-19-REPORT.md) | The pre-Rust audit and fix report | Superseded |

The detailed working documents behind the earlier reports are archived under
[`archive/`](archive/) and kept for traceability. Historical implementation plans
live in [`plans/`](plans/).

## Reading order

1. **[spec.md](spec.md)**: the normative requirements and contracts everything
   else implements.
2. **[api.md](api.md)** and **[development.md](development.md)**: the concrete
   contracts and where the code lives.
3. **[bootstrap.md](bootstrap.md)** and **[storage-zfs.md](storage-zfs.md)**:
   how the OS image and storage are built.
4. **[identity-sso.md](identity-sso.md)** and **[shares.md](shares.md)**: how
   users log in everywhere.
5. **[app-catalog.md](app-catalog.md)**, **[virtual-machines.md](virtual-machines.md)**,
   **[monitoring.md](monitoring.md)**, and **[notifications.md](notifications.md)**:
   feature docs.
6. **[deployment.md](deployment.md)** and **[operations.md](operations.md)**:
   run and operate it.

## Conventions

- All diagrams are ASCII and render in any Markdown viewer.
- "Talos" always refers to stock Talos Linux; "Naslos" is the integration layer
  on top (schematic, chart, agents).
- Authenticated services run in the `naslos` namespace; hostNetwork and
  privileged workloads (agent, samba, nfs, terminal) run in `naslos-privileged`
  (AUDIT-M6). Installed apps run in `naslos-apps` (PSA `baseline`); apps that
  declare `privileged: true` (VPN sidecars) run in `naslos-apps-priv` (PSA
  `privileged`). Documents name the namespace where it matters.
- Command examples assume `kubectl`, `talosctl`, and `helm` are available and
  pointed at the right cluster.
- `[OPEN]` marks work that is specified but not implemented yet.
