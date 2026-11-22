# iSCSI target spike

**Status: spike only — no iSCSI implementation ships in this release.** The
zvol prerequisite is implemented (`FR-ZVOL-01…03`: create/list/resize/delete
through the API and the privileged agent). This document evaluates a userspace
iSCSI target on stock Talos and records the requirements that remain
`[OPEN]`.

## Why this is a spike

TrueNAS parity item P1-8 asks for zvols plus an iSCSI target. zvols landed
first because a target without a block-volume API has nothing to export. The
target itself is constrained by the platform:

- **Talos Linux is immutable and ships no kernel iSCSI target.** The LIO
  modules (`target_core_mod`, `iscsi_target_mod`) are not available in the
  base image, and Talos does not load arbitrary third-party modules. A kernel
  target would need a Talos system extension built for the exact running
  kernel, re-validated on every Talos upgrade.
- **Naslos already uses the userspace precedent.** NFS is served by
  NFS-Ganesha, not the kernel NFS server, for the same reason. A target that
  cannot run in userspace should be rejected with evidence, not forced into
  the Talos base.
- **A target is a data-path service.** It needs block-device access, a LAN
  listener, CHAP secrets and a clear mapping to zvols. It is not a chart that
  can be added without a security review.

## Options considered

| Option | How it would run | Verdict |
| --- | --- | --- |
| LIO / SCST / IET (kernel) | Talos extension shipping `target_core_mod`, `iscsi_target_mod` and userspace tools | **No-go.** Requires a kernel-matched extension, a new upgrade surface and module signing/review work. Revisit only if upstream Talos ships the target modules. |
| `tgt` (userspace SCSI target) | `tgtd` in a host-networked container; block-backed LUNs open `/dev/zvol/<pool>/<name>`; management through the local `tgtadm` socket | **Conditional go** for a time-boxed spike. No kernel target module; the daemon and its block devices are the only moving parts. |
| SPDK iSCSI target | Userspace, but needs DPDK/hugepages, CPU pinning and a privileged data plane | **No-go for now.** Too heavy for a single-node appliance and a large new failure surface. |
| `istgt` (userspace) | Similar to `tgt` | **Fallback only.** Less maintained than `tgt`; no reason to lead with it. |
| NVMe-oF TCP | Separate spike | Out of scope here; Talos likely lacks `nvmet`, so it faces the same kernel/userspace decision. |

## Recommendation

**No-go for shipping iSCSI in this release; conditional go for a bounded
`tgt` spike.** Run the spike only against the gates below. If any gate fails,
record the evidence, keep iSCSI `[OPEN]`, and do not add a kernel extension on
speculation. The product does not promise iSCSI until the spike passes.

## Spike shape (not implemented)

If the spike is approved:

- New image `naslos-iscsi` running `tgtd` in the foreground. The chart flag is
  `iscsi.enabled: false` by default; no Service (hostNetwork) and no Ingress
  route.
- Configuration is a rendered `targets.conf`: one target IQN, one or more
  zvol LUNs, and explicit initiator ACLs. The renderer validates the IQN, LUN
  range, zvol name (the same rules as `FR-ZVOL`) and CHAP material.
- CHAP users come from a Kubernetes Secret mounted as files. Secrets never
  appear in chart values, logs, API responses or the UI.
- The spike does not add API mutations. API-managed targets are a follow-up
  requirement (`FR-ISCSI-02`); a hand-rendered config is enough to prove the
  data path.
- A zvol must already exist; the target maps exactly one LUN per configured
  zvol and never creates or deletes zvols itself.

## Privilege and NetworkPolicy review

- **hostNetwork bypasses pod NetworkPolicies** for host-network traffic in most
  CNIs (the agent's `:9090` has the same caveat). Port 3260 would therefore be
  reachable from the LAN even with a default-deny policy. A NetworkPolicy must
  not be the only control: require an explicit Talos host-firewall rule or a
  dedicated storage VLAN, restrict allowed initiator CIDRs, and never expose
  the port through Traefik.
- **PodSecurity:** run in `naslos-privileged` (PSA `privileged`) because
  opening block devices needs device access. A device plugin could avoid
  `privileged` later, but that is extra platform work. The container should
  drop every capability it does not need, use a read-only root filesystem, run
  without hostPID, and mount only `/dev/zvol` (read-write) plus a tmpfs
  runtime directory — never the host root.
- **Blast radius:** the target can read and write every zvol it maps. Treat a
  compromised target as raw disk access to those volumes; keep the image
  minimal, pinned by digest and free of shell tooling that is not needed.
- **Transport security:** iSCSI is plaintext and unauthenticated by default.
  CHAP is mandatory, an isolated VLAN is strongly recommended, and TLS/IPsec
  is out of scope for the spike.
- **Audit and abuse:** log target create/delete and failed logins without CHAP
  secrets; bound concurrent connections and segment sizes so one initiator
  cannot exhaust the node.

## Data-safety integration

- A target must register the zvols it maps with the API so `FR-ZVOL-03` can
  refuse delete/resize while a LUN is active. Until that registration exists,
  the agent's in-use check is best-effort: it sees mounts, sysfs holders and
  open file descriptors visible in `/proc`, but not a daemon with a private
  handle.
- Deleting a target must stop the export and leave the zvol and its data
  intact. Target deletion is not zvol deletion.
- Online growth needs an initiator rescan; it must be documented, not implied.
- A config that names a dataset, a snapshot or an unvalidated path must be
  refused at render time, before `tgtd` starts.

## Go/no-go gates

All gates must pass for the `tgt` spike to become a feature:

1. `tgtd` starts on stock Talos from the chart flag with no kernel module and
   no Talos extension.
2. A Linux `open-iscsi` initiator discovers the target, logs in with correct
   CHAP, is refused with wrong or missing CHAP, and cannot exceed its initiator
   ACL.
3. Data integrity: writes read back correctly, ZFS reports no checksum errors,
   and two initiators cannot write the same LUN.
4. Reconnect after a `tgtd` pod restart; the target returns after a node reboot
   without manual steps and without data loss.
5. Delete or resize of a mapped zvol is refused; unmapping and then deleting
   succeeds.
6. Throughput and latency are within an agreed margin of the local zvol
   baseline (record the measured numbers; do not guess them).
7. Uninstall stops the target, closes port 3260 and leaves the zvol intact.

If a gate fails, reject the userspace target and keep iSCSI `[OPEN]`. NVMe-oF
is a separate spike with its own evidence.

## `[OPEN]` requirements

- **FR-ISCSI-01 [OPEN]** — The system MUST create, list and delete iSCSI
  targets with a fixed IQN, one or more zvol LUNs and explicit initiator ACLs;
  a configuration MUST be validated before it is applied.
- **FR-ISCSI-02 [OPEN]** — The API MUST manage targets declaratively, render
  the target configuration from validated values, and MUST NOT expose
  arbitrary `tgtadm` or shell passthrough.
- **FR-ISCSI-03 [OPEN]** — CHAP MUST be required per target or initiator,
  secrets MUST come from Kubernetes Secrets, MUST NOT appear in logs, values or
  API responses, and wrong credentials MUST be refused.
- **FR-ISCSI-04 [OPEN]** — A zvol mapped by a target MUST be refused for
  delete/resize (extending `FR-ZVOL-03`), and deleting a target MUST NOT delete
  its zvol.
- **FR-ISCSI-05 [OPEN]** — Port 3260 MUST be restricted to configured
  initiator CIDRs or a dedicated VLAN by a host firewall; a NetworkPolicy
  alone is insufficient on hostNetwork.
- **FR-ISCSI-06 [OPEN]** — The UI MUST show target state, LUN mappings and last
  login errors without secrets, and the e2e suite MUST cover the go/no-go
  drill.
- **FR-ISCSI-07 [OPEN]** — Uninstall MUST stop the target, close port 3260 and
  leave all zvol data intact.
