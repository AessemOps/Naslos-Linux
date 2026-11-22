# NFSv3 feasibility spike

**Status:** recommendation — **no-go for the current release**. NFS stays
**NFSv4-only** (`FR-SHR-02`). This document records what serving NFSv3 on Talos
would require, what NFS-Ganesha already provides in userspace, and the
conditions under which the decision should be revisited. The normative
`[OPEN]` conditions are `FR-NFS3-01`…`FR-NFS3-05` in
[spec.md](spec.md#33-shares-fr-shr).

## Why NFSv3 is not just a flag

NFSv4 is a single TCP port (2049) with integrated locking and a pseudo
filesystem. NFSv3 is a family of RPC services:

| Service | Program | Default port | Needed for |
| --- | --- | --- | --- |
| portmapper / rpcbind | 100000 | **111** TCP+UDP | clients discovering the other services |
| NFS | 100003 | 2049 TCP (and optionally UDP) | file I/O |
| MOUNT | 100005 | 20048 (fixed in our config) | obtaining the root file handle |
| NLM | 100021 | 32803 (fixed) | POSIX/`fcntl`/`flock` locks |
| NSM (statd) | 100024 | 662 (fixed) | lock crash recovery (`SM_NOTIFY`) |
| RQUOTA | 100011 | 875 | quotas (not offered) |

The Talos **host** has no kernel `nfsd`, no `rpcbind`, and no `rpc.statd`.
That is not automatically a blocker: all of these services can run in
userspace inside the Ganesha container, and the container already uses
`hostNetwork` with root and the capabilities needed to bind privileged ports.
The real question is whether the resulting service is safe, reliable and
compatible enough to justify the extra surface.

## What NFS-Ganesha provides

Ganesha can serve NFSv3; it is not a kernel-NFS wrapper. Relevant core options
(`NFS_CORE_PARAM`):

- `Protocols = 3, 4;` — opens v3 (MOUNT/NLM) in addition to v4. The list is a
  hard limit; an export can restrict it further but cannot expand it.
- `MNT_Port`, `NLM_Port`, `NSM_Port`, `NFS_Port` — pin every RPC listener to a
  fixed port instead of an ephemeral one.
- `Enable_NLM = true;` — Ganesha's own NLM server (no kernel `lockd` on the
  server). This is required for normal POSIX locking from v3 clients.
- `Internal_statd = true;` — Ganesha provides the NSM protocol itself on
  `NSM_Port` instead of delegating to an external `rpc.statd`. This option
  exists only when the package is built with internal-statd support; the
  Debian package in the NFS image has to be verified for that.
- `Enable_UDP = false;` — TCP-only v3, avoiding UDP listeners.
- `mount_path_pseudo = true;` — already set by Naslos. With it, a v3 client
  mounts the same pseudo path (`<host>:/<share>`) as a v4 client, so the share
  naming model would not change.

What Ganesha does **not** provide is a portmapper. It registers its RPC
programs with an external `rpcbind`; an NFSv3 client normally asks that
portmapper which port MOUNT and NLM are on. Without `rpcbind`, clients fail at
mount time unless every client is configured with explicit `port=`,
`mountport=` and `nolock` options. That defeats the compatibility reason for
adding v3 in the first place, so a viable design must ship `rpcbind` in the
container (or accept a deliberately restricted, fixed-port client set).

### The feasible shape (if revisited)

1. Keep the v4-only DaemonSet as the default; add a separate opt-in v3 profile
   (values flag) rather than changing the existing one.
2. Run `rpcbind` in the same container, together with Ganesha's internal NLM
   and internal NSM. Pin 111, 2049, 20048, 32803 and 662; do not expose RQUOTA.
3. Mount a persistent NSM/state directory (or prove that Ganesha's internal
   statd survives a container restart and sends `SM_NOTIFY`).
4. Keep `mount_path_pseudo = true` and one `EXPORT` table for both versions.
5. Restrict the listening address, the port exposure and the export client
   lists to the LAN; never enable v3 on a public interface.

## Client compatibility

| Client | v3 support | Notes |
| --- | --- | --- |
| Linux | Yes (`-o vers=3`) | Modern kernels default to v4.2; v3 needs portmapper (or explicit ports) and `lockd`/`statd` on the client. `nolock` works but loses cross-client POSIX locking. |
| macOS | Yes | `mount_nfs -o vers=3`; v4 is the better default. |
| Windows NFS client | Yes (v3; v4.1 on newer builds) | Older Windows/Server NFS clients are a common reason to want v3. |
| VMware ESXi | Yes | ESXi datastores are v3-first; Ganesha's `Short_File_Handle = true` is required to fit VMware's 56-byte handle limit. |
| Appliances / legacy UNIX | Often v3-only | The compatibility argument for v3, but also the least controllable clients. |

NFSv3 has no pseudo filesystem: a client mounts each export separately. That
is compatible with Naslos's per-share model, but it means a v3 client cannot
browse the server's export tree.

## Security analysis

- **AUTH_SYS only.** Naslos has no Kerberos keytab and the spec keeps
  `sec=krb5` out of scope. A v3 client asserts a numeric uid/gid; any host on
  an allowed network can claim to be any user, including root. The same is true
  of the current v4/AUTH_SYS exports, but v3 adds more legacy clients and more
  services to the trust boundary.
- **Root squash.** Naslos defaults to `Root_Squash` (`noRootSquash` is an
  explicit per-share opt-out). NFSv3 makes `noRootSquash` even more dangerous
  because an unauthenticated client can claim uid 0; v3 must not be offered on
  a share that opts out without a prominent warning.
- **Portmapper exposure.** `rpcbind` on 111 is an unauthenticated service
  discovery endpoint with a history of reflection/amplification abuse. It must
  be firewalled to the LAN, not exposed to the internet.
- **NLM/NSM exposure.** NLM and NSM are unauthenticated RPC services; the
  `SM_NOTIFY` path has historically been spoofable. `NSM_Use_Caller_Name =
  false` (the default) avoids reverse-DNS trust, but does not authenticate.
- **hostNetwork and NetworkPolicy.** The file-serving pods bind host ports.
  Kubernetes `NetworkPolicy` is not a reliable boundary for hostNetwork
  traffic, so LAN-only exposure has to be enforced by the network/firewall,
  not by the chart alone.
- **No transport security.** Neither v3 nor the current v4/AUTH_SYS setup
  encrypts or integrity-protects data. v3 does not make that worse, but it
  expands the set of clients that can reach the service.

## Reliability and operations

- NFSv3 data operations are stateless, but locking is not. A server restart
  must notify clients through NSM; otherwise clients can wait for a lock that
  will never be released, or reclaim against a stale state.
- Ganesha reloads its export table on `SIGHUP`, but changes to `Protocols`,
  `mount_path_pseudo`, ports and NLM/NSM settings need a restart. A v3 profile
  therefore needs a restart procedure and a compatibility check for mounted v3
  clients.
- Extra daemons and ports mean extra liveness/readiness probes, log surfaces
  and image packages (`rpcbind`, and `nfs-common` if external `rpc.statd` is
  used instead of `Internal_statd`).
- The current NFS image is deliberately minimal (Ganesha + VFS FSAL). Adding
  portmapper/statd grows the attack surface and the CVE surface of the image.

## Recommendation

**No-go for the current release.** The current product decision (open
question 5 in the parity plan) is to keep NFSv4-only until a concrete
user-facing v3 compatibility requirement is demonstrated. The spike shows that
v3 *can* be built in userspace, but only by adding a portmapper, NLM/NSM state
handling, extra privileged ports and a second compatibility/security model —
for a protocol that is strictly weaker than the NFSv4 path already shipped.

Revisit when **all** of the following hold:

1. A real client requirement exists (for example an ESXi datastore or a
   v3-only appliance), not just feature parity with another NAS.
2. The NFS image is verified to expose a working portmapper plus internal
   statd (or a managed external `rpc.statd`), with every port fixed and
   documented.
3. A live drill proves mount, read/write, POSIX locking, client reboot and
   server restart/lock recovery with at least Linux and the requesting client.
4. The v3 profile is opt-in, LAN-scoped, root-squashed by default, and cannot
   weaken the v4 auth or client allow-list.
5. The extra ports and daemons are covered by the image scan, the audit gate
   and the operations documentation.

Until then, NFSv3 remains disabled and unadvertised; the v4 export table and
its tests are unchanged.
