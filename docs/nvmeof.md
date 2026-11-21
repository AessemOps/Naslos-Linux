# NVMe-oF TCP (spike)

**Status:** design / go-no-go spike, 2026-11-21. **No target is implemented.**
There is no API route, chart object, supervisor image, kernel change, or test in
this tree. The stable requirement IDs are reserved in
[spec.md](spec.md#312-block-storage--nvme-of-tcp-fr-nvme) and every one is
`[OPEN]`.

This document answers one question: should Naslos expose ZFS zvols as an
NVMe-oF TCP target, and if so, on which implementation? It is based on
documentary evidence and source inspection only. No production-node command was
run and no benchmark was executed; the validation plan in §13 is the proof that
is still required.

## 1. Decision

| Question | Answer |
| --- | --- |
| Kernel `nvmet` available on Talos? | **Yes.** The Talos v1.14 kernel configuration builds `nvmet` and `nvmet-tcp` as modules and the kernel package installs all modules. No custom extension is needed. |
| Is a target shippable on the current Talos v1.14 line? | **No-go.** Stock Linux 6.18 `nvmet-tcp` advertises MDTS 0 ("no limit") while rejecting data above the 4 MiB `NVMET_TCP_MAXH2CDATA` limit. The upstream fix is not in the pinned kernel. Large I/O can fail with I/O errors, which is a data-loss risk on some initiator filesystems. |
| Which implementation should a future version use? | **Kernel `nvmet`**, configured through configfs by a small privileged supervisor. It is the smallest data path, needs no hugepages, and its data plane survives a supervisor crash. |
| SPDK? | **No-go as the default.** It is a second userspace data plane on top of kernel-backed zvols, needs hugepages and pinned cores, and a target crash drops every connection. It also has its own CVE cadence. Revisit only if the kernel path stays blocked and the product accepts that cost. |
| Custom Talos system extension? | **No-go.** The modules already ship; a custom module would require a custom signed kernel and add an upgrade-maintenance burden for no functional gain. |
| ESXi / Windows initiators? | **Not supported by the kernel target.** VMware ESXi needs fused commands that the Linux target does not implement (TrueNAS documents the same incompatibility). Windows' native NVMe-oF initiator is a preview, not a production path. Linux is the only sane v1 initiator. |
| HA / multipath? | **No HA.** Native NVMe multipath can use two target ports, but on a single node both paths hit the same kernel, pool, and zvol. It is path redundancy, not node redundancy. |

**Recommendation:** treat NVMe-oF TCP as **no-go for a release in the current
Talos v1.14 line**. Keep the kernel-`nvmet` design as the chosen direction and
re-open it when a pinned Talos kernel carries the bounded-MDTS fix (or an
equivalent target-side limit exists) and the prerequisites in §2 are met. If
ESXi support is a hard product requirement, the answer is **no-go** for the
kernel path regardless: that needs a separate userspace-target investigation.

The rest of this document records the evidence, the rejected alternatives, the
operational design, and the tests that must pass before the decision can change.

## 2. Prerequisites

NVMe-oF is block storage, so it depends on work that is not in this tree:

1. **Zvol lifecycle (P1-8).** The agent must create/delete/resize zvols with a
   safe property subset. NVMe-oF namespaces are zvols, not datasets and not
   raw disks. P1-8 is a separate backlog item and is not implemented here.
2. **A pinned Talos kernel without the MDTS defect.** The kernel must either
   report a bounded MDTS that matches its transport limit or expose a supported
   target-side way to set one. Linux 6.18 does neither (see §3.4). A Talos
   upgrade that carries the upstream fix is the clean unblock.
3. **A stable storage address.** A listener binds a concrete IPv4 address.
   DHCP is acceptable only with a reservation or a controlled rebind path; an
   address change must stop the listener and disconnect initiators first.
4. **A storage network policy.** NVMe/TCP data is cleartext. Host-NQN
   allowlisting and optional DH-HMAC-CHAP are authentication, not encryption.
   A dedicated VLAN or a switch ACL is the practical control.
5. **A scratch Talos node.** Every claim below that depends on module loading,
   configfs in a pod, `/dev/zvol` visibility, CHAP, or performance must be
   proven on a disposable Talos node before a production drill.

## 3. Talos platform evidence

### 3.1 The kernel has the target

The Talos v1.14 packaging branch (`siderolabs/pkgs`, `release-1.14`) builds
Linux 6.18.63 with these relevant options in `kernel/build/config-amd64`:

```
CONFIG_NVME_CORE=y
CONFIG_NVME_FABRICS=y
CONFIG_NVME_TCP=y              # host side
CONFIG_NVME_MULTIPATH=y
CONFIG_NVME_AUTH=m
CONFIG_NVME_TARGET=m           # nvmet.ko
CONFIG_NVME_TARGET_TCP=m       # nvmet-tcp.ko
CONFIG_NVME_TARGET_AUTH=y      # DH-HMAC-CHAP on the target
CONFIG_CONFIGFS_FS=y
# CONFIG_NVME_TARGET_TCP_TLS is not set
# CONFIG_NVME_HOST_AUTH is not set
# CONFIG_ISCSI_TARGET is not set
```

The same options are present in the arm64 configuration. The kernel package
(`kernel/kernel/pkg.yaml`) runs `make modules_install`, so every `=m` module,
including `nvmet.ko` and `nvmet-tcp.ko`, is installed under
`/usr/lib/modules/<kernel>`. This is why the plan's original assumption
("Talos likely lacks `nvmet`") is wrong for the v1.14 line.

For contrast, `CONFIG_ISCSI_TARGET` is not set. Talos has no kernel iSCSI
target, which is why the iSCSI backlog item plans a userspace target. NVMe-oF
does **not** share that constraint.

The target options are not loaded automatically. Talos only loads modules named
in the machine configuration; the repository's own ZFS module block exists for
exactly that reason. Talos 1.14 documents `KernelModuleConfig` as the
replacement for the deprecated `.machine.kernel.modules` field, and notes that
modules added to the configuration are loaded but never unloaded; removing a
module or changing its parameters needs a reboot.

### 3.2 No system extension is required

The official Sidero extension catalog has no NVMe-oF target extension, and none
is needed. A custom extension would not be a normal userspace add-on: Talos signs
kernel modules with a key generated during its own kernel build, and a custom
module must be built against a custom signed kernel. That violates the
project's "no custom kernels" goal and would make every Talos upgrade a
rebuild. The stock, signed modules are strictly better.

An extension that only ships userspace tools (for example `nvmetcli`) would not
need a custom kernel, but the design below does not need `nvmetcli` either: the
supervisor writes configfs directly, avoiding a Python runtime and a shell-out
surface.

### 3.3 What the target can expose

The kernel target exposes NVMe subsystems, namespaces, ports, and host
allowlists through configfs:

```
/sys/kernel/config/nvmet/
  subsystems/<nqn>/
    attr_allow_any_host
    attr_model, attr_serial, attr_version, attr_qid_max
    allowed_hosts/<host-nqn>        (symlink)
    namespaces/<nsid>/
      device_path
      enable
      buffered_io, revalidate_size, ...
  ports/<port-id>/
    addr_trtype (tcp), addr_adrfam (ipv4), addr_traddr, addr_trsvcid
    subsystems/<nqn>                (symlink)
  hosts/<host-nqn>/
    dhchap_key
    dhchap_ctrl_key
    dhchap_hash
    dhchap_dhgroup
```

`device_path` is a block device path, which is why a zvol is the natural
backend. `attr_allow_any_host` must be 0 when explicit hosts are linked; the
kernel refuses the combination. `device_path` may only be changed while the
namespace is disabled. `dhchap_*` attributes exist because
`CONFIG_NVME_TARGET_AUTH=y` is built in.

The kernel does not expose a clean per-controller list through configfs, and
`CONFIG_NVME_TARGET_DEBUGFS` is not set. A future UI must not invent connected
hosts it cannot read; it can show configured state, listener state, and
namespace I/O errors, and mark live controller identity as unavailable unless a
supported interface is found.

### 3.4 The blocking defect: advertised MDTS vs the TCP limit

In Linux 6.18, `nvmet` sets the Identify Namespace MDTS field to 0 unless the
transport implements `get_mdts`:

```c
/* drivers/nvme/target/admin-cmd.c */
if (ctrl->ops->get_mdts)
        id->mdts = ctrl->ops->get_mdts(ctrl);
else
        id->mdts = 0;
```

`nvmet-tcp` does not implement `get_mdts`. Its transport caps incoming data at:

```c
/* drivers/nvme/target/tcp.c */
#define NVMET_TCP_MAXH2CDATA 0x400000 /* 4 MiB */
...
icresp->maxdata = cpu_to_le32(NVMET_TCP_MAXH2CDATA);
```

On the initiator, `nvme-tcp` applies the advertised MDTS, not the transport
`maxdata`, to the block queue's maximum request size:

```c
/* drivers/nvme/host/core.c */
if (id->mdts)
        max_hw_sectors = nvme_mps_to_sectors(ctrl, id->mdts);
else
        max_hw_sectors = UINT_MAX;
ctrl->max_hw_sectors =
        min_not_zero(ctrl->max_hw_sectors, max_hw_sectors);
```

The result is an advertised "no limit" and an enforced 4 MiB limit. A Linux
initiator that issues a larger request gets a target-side protocol error. This
is not theoretical: TrueNAS 25.10.8 shipped a fix after large NVMe/TCP
transfers failed and could silently lose data on filesystems such as ext4, and
the upstream "report a bounded MDTS" patch was still under review at the time of
this spike. Talos v1.14's kernel patch list contains no such patch, so the
pinned kernel inherits the defect.

For a NAS, silent I/O errors are a release blocker, not a tuning note. A future
implementation must either wait for a kernel with the fix, or prove a
target-side/initiator-side cap that makes large I/O impossible. An
initiator-side `max_sectors_kb` workaround is not a product guarantee: the NAS
cannot enforce it on every client, and a single client that ignores it can hit
the defect. If the product accepts "Linux only, admin must apply a 1 MiB queue
cap", that must be explicit in the UI and docs; this spike does not recommend
it as the default.

## 4. Options considered

### 4.1 Kernel `nvmet` in a privileged supervisor (chosen design)

```
Linux initiator (nvme-cli, nvme-tcp)
        |  TCP 4420, cleartext data + in-band DH-HMAC-CHAP
        v
Talos node -----------------------------------------------------
  kernel nvmet-tcp (data plane; configfs is the control plane)
        |  block I/O
        v
  /dev/zvol/<pool>/<zvol>  <---- naslos-agent (only ZFS writer)
        ^
  naslos-nvmetd (privileged, hostNetwork, configfs reconciler)
        ^  bearer token + TLS
  naslos-api (validation, records on the state PVC, UI)
```

Why a separate supervisor instead of the API:

- The API is not privileged and does not run in the host network namespace.
  Configfs needs `CAP_SYS_ADMIN` (and a privileged or near-privileged pod) and
  the listener must bind the node's address.
- The supervisor has no ZFS write path. The agent remains the only writer of
  `zpool`/`zfs`, consistent with the existing shares and VM designs.
- The supervisor has no Kubernetes API client, like the agent and vmd.
- The data plane is in the kernel. If the supervisor crashes or is upgraded,
  existing controllers keep running; the supervisor only loses its ability to
  change the configuration until it restarts and adopts the existing tree.
  This is a material advantage over a userspace target.

Why configfs directly, not `nvmetcli`:

- `nvmetcli` is a Python tool and would add a runtime and a shell-out surface.
  The supervisor can create directories, write attributes, and create symlinks
  with ordinary file APIs.
- Direct writes still need strict validation: every path component is built
  from a validated NQN, a numeric namespace ID, and a numeric port ID. No
  user-supplied path is ever interpolated into a filesystem path.

### 4.2 SPDK userspace target (rejected as the default)

SPDK's NVMe-oF target is a complete userspace implementation with TCP, RDMA,
DH-HMAC-CHAP, and experimental TLS. It can run in a privileged container with
host networking. It is still the wrong default here:

- **ZFS is still in the kernel.** SPDK would use `bdev_aio`/`bdev_uring` on a
  zvol, so every I/O still enters the kernel ZFS/block layers. SPDK adds a
  userspace hop and memory copies without owning the disks, which is the case
  where SPDK's poll-mode design buys the least.
- **Hugepages and CPU.** SPDK needs hugepage memory and (by default) dedicated
  polling cores. A NAS appliance that already gives memory to ZFS ARC should
  not reserve another pool and burn idle CPU for a target that is idle most of
  the day. SPDK has an interrupt mode for TCP, but that is an additional tuning
  surface, not a reason to duplicate the data path.
- **Crash behavior.** The SPDK process is the data plane. A crash or OOM kill
  drops every controller and in-flight I/O. The kernel target survives a
  management-process crash.
- **Maintenance.** SPDK is a large C/DPDK dependency with its own release and
  CVE cadence. The kernel target moves with the Talos kernel the project
  already tracks.
- **The zvol path has been fragile.** SPDK 24.09 changed `bdev_aio` behavior
  and broke zvol I/O (`io_submit` returned `-EOPNOTSUPP`); the issue was fixed
  in 2026 by restoring the old default. That is a reminder that the zvol path
  is not SPDK's primary use case.

SPDK can be configured with an explicit maximum I/O size, which may avoid the
kernel's advertised-vs-enforced MDTS mismatch. That is a reason to keep it as a
fallback if the kernel path stays blocked, not a reason to adopt it now. If it
is ever revisited, it needs its own spike covering hugepage reservation on
Talos, CPU
pin delegation, zvol bdev stability, image size, CVE response, and crash
recovery.

### 4.3 Custom Talos system extension (rejected)

A system extension could ship `nvmet`/`nvmet-tcp` if the kernel did not, but the
kernel already has them. A module extension must be built and signed with a
custom kernel, which is a fork in all but name and makes every Talos upgrade a
rebuild. The correct place to fix the MDTS defect is the Talos kernel, not a
Naslos extension.

### 4.4 Summary

| Option | ZFS integration | Data plane survives mgmt crash | Hugepages / pinned cores | Auth | Upgrade cost | Verdict |
| --- | --- | --- | --- | --- | --- | --- |
| Kernel `nvmet` + supervisor | zvol block device, no extra layer | Yes | No / No | DH-HMAC-CHAP; no TLS | Talos kernel + chart | **Chosen design; blocked by MDTS today** |
| SPDK | zvol through kernel AIO/uring | No | Yes / usually | DH-HMAC-CHAP; experimental TLS | SPDK image + hugepages | Fallback only |
| Custom extension | N/A | N/A | No | N/A | Custom signed kernel | Reject |
| iSCSI userspace target | zvol through kernel | N/A | Maybe | iSCSI CHAP | Separate stack | Different protocol; P1-8 |

## 5. ZFS backing model

### 5.1 One namespace per subsystem (v1)

The kernel supports multiple namespaces per subsystem. Naslos v1 should expose
exactly one zvol namespace per subsystem:

- Access control, capacity, snapshots, and deletion stay one-to-one and easy to
  explain.
- A mistaken host mapping cannot expose a second volume through the same
  subsystem.
- TrueNAS's own NVMe-oF UI uses one namespace per subsystem, so this matches
  the parity target.

Multi-namespace subsystems are a future extension, not a v1 requirement. The
API and supervisor must still validate a namespace ID rather than assume `1`
forever, so the wire shape does not paint us into a corner.

### 5.2 Zvol properties

A namespace zvol is created through the agent with a conservative property
set:

| Property | Value | Why |
| --- | --- | --- |
| `volmode` | `dev` | The block device must exist at `/dev/zvol/<pool>/<zvol>`. |
| `volblocksize` | validated (4K/8K/16K/32K/64K/128K) | Cannot be changed later and drives read-modify-write behavior. 16K is a reasonable default for mixed VM workloads; 64K/128K suits sequential. |
| `refreservation` | `none` (thin) by default | A NAS should not reserve the full zvol size up front. Thick is an explicit opt-in for latency-sensitive workloads. |
| `compression` | `inherit` or a validated algorithm | Compression is one of the reasons to back block storage with ZFS. |
| `sync` | `standard` | Do not silently disable flushes for a block export. A SLOG is the performance answer for sync-heavy guests, not `sync=disabled`. |
| `logbias` | `latency` or `throughput` | Explicit per-export choice; default `latency`. |
| `readonly` | `off` | A read-only namespace is a future option; v1 exports are read-write. |

Thin provisioning has a failure mode: the initiator sees the full zvol size,
but the pool can run out of space. Writes then fail with I/O errors inside the
guest. The API must show pool free space and per-zvol `used`/`referenced`, and
the alerting path must warn well before full. This is not optional polish; it is
the difference between a predictable capacity error and a guest filesystem
going read-only.

### 5.3 Device path safety

`device_path` is the most dangerous input in the design. A privileged process
that can point configfs at any block device can export the node's system disk
to the network. Therefore:

- The UI/API never accepts a device path. The operator selects a pool and a
  zvol name; the path is derived.
- The agent confirms the object is a ZFS volume (`type=volume`), has
  `volmode=dev`, is not mounted, and belongs to an allowed pool.
- The supervisor resolves `/dev/zvol/<pool>/<zvol>` and checks that the result
  is a block device. It also checks the stable symlink still points at the
  expected zvol; a missing or changed target is a hard error, not a retry with
  a fallback path.
- The supervisor's own HTTP API accepts the full desired configuration from
  the API and re-validates it before touching configfs. It is not a generic
  "create any subsystem" API.

### 5.4 Lifecycle and snapshots

- **Snapshot:** `zfs snapshot <pool>/<zvol>@<name>` while exported is
  crash-consistent, like a VM disk snapshot. The initiator must be quiesced for
  an application-consistent point.
- **Restore:** refuse while the namespace is enabled or a controller is
  connected. `zfs rollback -r` deletes newer snapshots; the UI must say so.
- **Clone:** a clone is a new zvol and can be exported as its own subsystem.
  This is the supported way to expose a point-in-time copy; do not try to
  export a snapshot object directly.
- **Resize:** `zfs set volsize` is not safely live for every initiator. v1
  should require the namespace to be disabled and the initiator disconnected,
  then re-enable and rescan. A future version can test `revalidate_size` and
  initiator rescan behavior before allowing live growth.
- **Delete:** refused while a namespace references the zvol, while the
  namespace is enabled, or while a controller is connected. The agent's
  existing dataset guards are not enough because the agent does not know about
  NVMe state; the API must check the export record and the live supervisor
  state before calling the agent.

### 5.5 Mounted check

Exporting a zvol that is mounted on the node corrupts data. The API/agent must
refuse if the zvol or any partition on it is mounted, and the UI must explain
why. A zvol used by a VM or a container is also off limits; the API should
check the VM records and any known mount consumers before allowing export.
This is the block-storage equivalent of the share-path guard.

## 6. Networking and exposure

### 6.1 Listener

- Transport: TCP, port **4420** (the IANA-assigned NVMe-oF port). A
  non-default port is allowed only if the operator has a reason and the
  initiator config uses it.
- Address family: IPv4 in v1. `0.0.0.0` is not acceptable as the advertised
  address; bind a concrete storage address so discovery returns something a
  client can connect to.
- The pod is `hostNetwork: true`. The kernel owns the listening socket, not the
  pod process. This is why a supervisor restart does not drop connections.
- No Traefik/Ingress route. NVMe-oF is not HTTP, and the API must never proxy
  block I/O.

### 6.2 Firewall and NetworkPolicy

Kubernetes `NetworkPolicy` does not apply to host-network pods: the pod carries
the node's network identity. The chart's existing agent/vmd policies are
intent documents, not enforcement. NVMe-oF needs one of:

- a switch/VLAN ACL that permits 4420 only from the initiator subnet, or
- a Cilium host-firewall policy keyed to the node label, or
- an nftables rule managed through the Talos machine config.

The Cilium host firewall is deliberately off by default in this repository
because a bad rule locked the node out during a live attempt. NVMe-oF must not
turn it on implicitly. A feature that opens a storage port needs an explicit
operator step and a console-access recovery plan. The docs must state that the
port is reachable from the LAN unless the operator adds the ACL.

### 6.3 Address changes

A DHCP lease change or a network reconfiguration invalidates the listener.
The network-change flow (FR-NET) and the NVMe-oF supervisor must coordinate:

1. Refuse a network change while an export is enabled and a controller is
   connected, or require an explicit "disconnect storage first" confirmation.
2. On an approved change, stop accepting new connections, let the API mark the
   export degraded, and rebind only after the new address is applied.
3. Never leave a listener on an address the node no longer owns.

A reservation/static address for storage is the recommended deployment. This
also makes the initiator configuration stable.

### 6.4 MTU and TCP

Jumbo frames help large sequential I/O but must match end to end. A mismatch
usually shows up as stalled or tiny-window connections, not a clean error.
The validation plan must test MTU 1500 and 9000 separately, and the docs must
tell the operator to verify the whole path (initiator NIC, switch, node NIC).

## 7. Access control and authentication

### 7.1 Host NQN allowlist

Every subsystem has an explicit allowed-host list; `attr_allow_any_host` is 0.
Discovery is filtered by the host NQN, so a client that is not allowed should
not see the subsystem. The host NQN is self-asserted and visible on the wire;
it prevents mistakes, not a determined attacker. That is why CHAP and network
isolation both matter.

Host NQN validation: the standard `nqn.<year>-<month>.<reverse-domain>:<name>`
shape, at most 223 bytes, no path separators. The API stores the string; the
supervisor re-validates before creating the `hosts/<nqn>` directory and symlink.

### 7.2 DH-HMAC-CHAP

`CONFIG_NVME_TARGET_AUTH=y` and the kernel crypto configuration include
HMAC-SHA-256/512 and the RFC 7919 FFDHE groups. A Linux initiator with a modern
`nvme-cli` can connect with:

```bash
HOSTNQN=$(nvme show-hostnqn)
HOSTKEY=$(nvme gen-dhchap-key -m 1 -n "$HOSTNQN")
CTRLKEY=$(nvme gen-dhchap-key -m 1 -n "$SUBSYSTEM_NQN")
```

The exact CLI spelling changed across `nvme-cli` 2.x/3.x (`gen-dhchap-key`
versus `keys gen-kxchap-secret`, `--dhchap-secret` versus `--kxchap-secret`).
The Naslos docs must pin a supported `nvme-cli` version and show both forms
where necessary.

Target-side, each host object gets `dhchap_key` (what the host must prove) and
optionally `dhchap_ctrl_key` (what the target proves for bidirectional auth),
plus a hash and DH group. The key format is `DHHC-1:<hash-id>:<base64>:`. The
`dhchap_key` and `dhchap_ctrl_key` are secrets.

Secret handling:

- Stored in a Kubernetes Secret, mounted as a file, mode 0400, read by the
  supervisor; never placed in chart values, environment variables, logs, API
  responses, or error strings.
- The API reports only "CHAP configured" and never returns the key or a
  reversible fingerprint.
- Rotation writes a new Secret and updates the target host object; existing
  connections continue until they reconnect, so the API must state that
  revocation is effective on the next connection (or force a disconnect).
- Configfs attributes are root-readable on the host. A root compromise of the
  node is already game over; the threat model is the network and the
  supervisor, not a hostile root on the same node.

### 7.3 No TLS

`CONFIG_NVME_TARGET_TCP_TLS` is not set in the Talos kernel. The kernel target
cannot do TLS 1.3. SPDK's TLS is experimental and is not a reason to adopt a
second data plane. Therefore:

- Data is cleartext on the wire.
- CHAP authenticates but does not encrypt.
- A dedicated storage VLAN or switch ACL is mandatory for any deployment that
  is not a fully trusted lab.
- The UI/docs must say this plainly; "CHAP enabled" must not be presented as
  confidentiality.

If confidentiality on an untrusted network becomes a requirement, the kernel
path needs a Talos kernel with `NVME_TARGET_TCP_TLS` or a different design.
That is an open question, not a v1 promise.

### 7.4 Supported initiators

| Initiator | Status | Notes |
| --- | --- | --- |
| Linux (`nvme-tcp`, `nvme-cli`) | **Supported target** | The reference path; native multipath; CHAP supported. |
| Proxmox VE (Linux) | Supported via Linux | Same kernel initiator; useful for VM datastores. |
| VMware ESXi | **Not supported by the kernel target** | ESXi requires fused commands; the Linux `nvmet` target does not implement them. TrueNAS documents the same incompatibility for its kernel target. |
| Windows Server | **Not supported in production** | A native NVMe-oF initiator was preview-only in Windows Server Insider builds at spike time; third-party initiators exist but are outside a supported NAS claim. |
| macOS | Not supported | No native NVMe-oF initiator. |

This is a Linux-first scope with ESXi explicitly out, matching the documented
limitation of TrueNAS's kernel-based target. A marketing claim of "NVMe-oF"
without the initiator matrix would be misleading.

## 8. Initiator configuration (Linux)

### 8.1 Discover and connect

```bash
# Host identity: stable across reboots.
cat /etc/nvme/hostnqn             # or: nvme show-hostnqn
modprobe nvme-tcp                  # usually autoloaded by nvme-cli

# Discover the target's allowed subsystems.
nvme discover -t tcp -a <target-address> -s 4420

# Connect one path.
nvme connect -t tcp -a <target-address> -s 4420 \
  -n <subsystem-nqn> --hostnqn "$(cat /etc/nvme/hostnqn)" \
  --ctrl-loss-tmo=1800

# Verify.
nvme list
nvme list-subsys
```

With CHAP, add `--dhchap-secret=<host-key>` and optionally
`--dhchap-ctrl-secret=<controller-key>` (or the `kxchap` equivalents on newer
`nvme-cli`). Passing secrets on the command line exposes them in shell history
and the process list; for production, use the nvme-cli JSON configuration with
tight file permissions or the distribution's keyring integration.

### 8.2 Persistence

Connections made with `nvme connect` do not survive a reboot. On a Linux
initiator, put the discovery portal in `/etc/nvme/discovery.conf` (or the newer
`/etc/nvme/nvme-fabrics.conf`) and enable the distribution's
`nvmf-autoconnect` service. Verify the exact unit name on the initiator
distribution; it is not part of Naslos.

### 8.3 Timeouts and queue depth

- `--ctrl-loss-tmo` controls how long the initiator keeps retrying after a
  path loss. The default is 600 s. `-1` means retry forever, which can hang
  processes indefinitely; a bounded value plus application retry is safer for
  a single-node target. Document the chosen value.
- Keep the default queue count unless benchmarks show a single queue is the
  bottleneck. More queues help small random I/O; they also multiply CPU and
  memory use.
- Never format or mount a namespace that the target says is still in use.
  Formatting the initiator's block device destroys the zvol. The UI/docs must
  warn about this in the same way a pool-destroy dialog does.

### 8.4 Multipath

Native NVMe multipath is the supported Linux mechanism; device-mapper
multipath is not used for NVMe/TCP. It groups controllers that share a
subsystem NQN. It only helps if there are genuinely independent paths:

- two target NICs on different subnets/VLANs, or
- two physical links through different switches.

Two IP addresses on the same NIC, or two paths through the same switch, are
not redundancy. On a single-node Naslos, both paths terminate in the same
kernel, pool, and zvol, so multipath cannot survive a node reboot, kernel
panic, or pool loss. Do not market it as HA. For a single-node appliance, a
second path is optional and should be documented as "path failover only."

## 9. Performance

No benchmark was run in this spike. The following is the measurement plan and
the published context, not a Naslos result.

### 9.1 Where time goes

1. **ZFS zvol:** `volblocksize`, compression, sync writes, SLOG, ARC, and pool
   topology dominate. A 4K random workload on a RAIDZ HDD pool will be slow no
   matter what the fabric does; an all-NVMe mirror will be fast.
2. **Block layer:** queue depth, scheduler, and the zvol's logical block size.
3. **nvmet-tcp:** per-queue TCP processing and data copies. Multiple queue
   pairs spread work across CPUs; a single queue can saturate one core.
4. **Network:** 1/10/25/40 GbE line rate, MTU, and switch buffering.
5. **Initiator:** filesystem, page cache, and application I/O size.

Published field reports for NVMe/TCP on a healthy 25 GbE link with a modern CPU
are in the range of 1.5–2.5 GB/s per connection and a few hundred thousand 4K
random-read IOPS per target core, with the target core or the backend usually
the limit. Treat those as order-of-magnitude guidance only; the zvol and pool
will decide the real number. The reports are not a controlled benchmark for
this hardware.

### 9.2 Benchmark matrix

Use `fio` with `--direct=1` on both the local zvol (for a baseline) and the
remote namespace:

| Axis | Values |
| --- | --- |
| Access | sequential read, sequential write, 4K random read, 4K random write, 70/30 mixed |
| Block size | 4K, 64K, 1M, and an 8M/16M/32M large-I/O probe |
| Queue depth | 1, 32 |
| Jobs | 1, 4 |
| MTU | 1500, 9000 |
| Target | 1 and 4 queue pairs where the initiator allows it |
| Duration | 60 s after a 10 s ramp; repeat 3 times |

Record: throughput, IOPS, p50/p95/p99 latency, target CPU per core, softirq
time, `zpool iostat -v 1`, ARC hit rate, and any `nvmet_tcp` errors in dmesg.
The large-I/O probe is mandatory because of the MDTS defect: a run that errors
above 4 MiB is a blocker, not a performance result.

### 9.3 Proposed acceptance thresholds

These are starting thresholds, to be confirmed with product:

- Sequential throughput: at least 80% of the local zvol baseline on the same
  initiator and link, or line rate, whichever is lower.
- 4K random IOPS: at least 70% of the local zvol baseline at QD32.
- p99 latency: no more than 2x the local zvol baseline for the same workload.
- No I/O errors, no controller resets, no unexpected reconnects in a 30-minute
  soak.
- Large I/O (8/16/32 MiB): zero errors. If the kernel advertises MDTS 0, this
  test fails by design and the feature stays blocked.
- Idle target CPU: effectively zero (the kernel target is interrupt-driven;
  SPDK's poll mode is the contrast).

## 10. Failure modes and recovery

| Failure | Symptom / impact | Recovery | Prevention |
| --- | --- | --- | --- |
| Supervisor crash / pod restart | Existing controllers keep I/O; config changes stop until restart. | Restart the pod; it adopts the existing configfs tree and reconciles. | `Recreate` strategy; idempotent reconcile; no data-plane ownership in the pod. |
| Node reboot | All controllers drop; configfs is empty. | Talos loads modules, agent imports pools, supervisor recreates exports, initiators reconnect within `ctrl-loss-tmo`. | Bounded reconnect timeout; boot ordering; readiness gate. |
| Network partition / cable pull | I/O stalls, then errors after the timeout; multipath may hide one path. | Restore the link; initiator reconnects. | Two independent paths if the operator needs path failover; monitoring. |
| Target disable while connected | Removing the port link disconnects controllers; in-flight I/O errors. | Treat as disruptive; require explicit confirmation; initiator reconnects when re-enabled. | Two-phase disable: stop new connections, wait for controllers, then unlink. |
| Pool full / thin zvol full | Namespace writes fail; guest filesystem may go read-only. | Free space or grow the pool; re-enable writes; check guest filesystem. | Capacity alerts, overprovision limits, `refreservation` for critical volumes. |
| Zvol delete / resize / rollback while exported | Data loss or I/O errors. | Refuse the operation; disconnect and disable first. | API guard checks export record + live supervisor state; agent check for mounts. |
| CHAP rotation | New connections fail until the initiator has the new key; existing connections may continue. | Update the initiator config, disconnect/reconnect. | Document rotation; offer a force-disconnect action. |
| Host removed from allowlist | Existing controllers may keep running until disconnect; new connects fail. | Disconnect the host; re-add if needed. | UI says revocation is not immediate; force disconnect. |
| Address change / DHCP lease change | Listener points at an address the node no longer owns; connects fail. | Stop, rebind on the new address, reconnect initiators. | Static/reserved storage address; network-change guard. |
| Talos upgrade | Modules reload; configfs is empty; supervisor image may also roll. | Supervisor recreates exports; initiators reconnect. | Test on a scratch node; do not upgrade during a backup window. |
| Module unload | `rmmod` fails while ports/namespaces exist; a forced unload can panic or drop I/O. | Clear configfs, disconnect, then reboot if needed. | Never unload in place; disable the feature and reboot. |
| Configfs partial apply (crash mid-reconcile) | Some objects exist, some do not; a naive retry fails with `EEXIST`/`EBUSY`. | Reconcile from observed state: adopt, fix, or remove in dependency order. | Idempotent diff; startup validation; a single writer. |
| Large I/O >4 MiB (MDTS defect) | Target rejects; initiator logs I/O errors; possible silent data loss on ext4. | None at the target; initiator must retry or cap I/O. | **Blocker.** Wait for the kernel fix, or enforce/prove a cap. |
| ESXi connect | Path initialization fails (fused commands unsupported). | Use a Linux initiator or a different target implementation. | Do not claim ESXi support. |
| Port conflict (4420 in use) | Listener creation fails. | Find the conflicting process/socket; choose a free port only if documented. | Preflight bind check; fail closed. |
| NQN collision / changed NQN | Initiator cannot connect or sees a different subsystem. | Restore the original NQN; reconnect. | Persist a generated NQN; warn that changing it breaks initiators. |

## 11. Upgrade, rollback, and install path

### 11.1 Declarative module load

The future install path is:

- `bootstrap/vm/naslos-vm.yaml` and
  `bootstrap/installer/naslos-installer.yaml.tmpl`: add one
  `KernelModuleConfig` document for `nvmet` and one for `nvmet-tcp` (Talos
  1.14's non-deprecated form). Keep the existing ZFS module load; if the
  template is regenerated, migrate ZFS to `KernelModuleConfig` at the same
  time so there is one style.
- No schematic change: the modules are in the stock kernel package.
- No runtime `modprobe` from a pod and no `/lib/modules` host mount.
- A clean install must reproduce the capability; a hand-applied
  `talosctl patch` is not acceptable.

### 11.2 Chart and image

A future implementation adds:

- `nvmeof.enabled: false` in `charts/naslos/values.yaml`, with the image,
  token/TLS settings, target port, and resource requests.
- `naslos-nvmetd` Deployment in `naslos-privileged`, `hostNetwork: true`,
  `strategy: Recreate`, no Kubernetes API token, `/dev` HostToContainer,
  configfs mounted read-write inside the pod, `/run` emptyDir for runtime
  state.
- Chart-generated token and TLS Secrets, following the agent/vmd pattern; the
  API pins the CA.
- A headless Service for the API-to-supervisor channel (not for initiators).
- A NetworkPolicy intent object, plus a clear comment that host-network pods
  are exempt and a node-level firewall is the real control.
- A new image in `.github/image-matrix.json`, a values tag, the release guard,
  and `CREDITS.md` if any new dependency is added.
- API state on the existing state PVC (`nvmeof.json`); CHAP secrets only in
  Kubernetes Secrets.

### 11.3 Upgrade and rollback

- **Naslos chart upgrade:** the supervisor is a control-plane pod. A rolling
  restart is unnecessary; `Recreate` avoids two reconcilers. The kernel target
  keeps serving during the pod restart.
- **Talos upgrade:** modules are part of the new kernel package; configfs is
  recreated by the supervisor; initiators reconnect. Test the exact upgrade
  path on a scratch node.
- **Feature disable:** stop accepting new connections, require the operator to
  disconnect initiators, unlink the ports/subsystems, and leave the zvols
  intact. Re-enabling recreates the same NQNs and namespaces.
- **Rollback:** chart rollback restores the previous supervisor image; the
  config schema must remain backward compatible. Data lives in zvols, not in
  the supervisor. A rollback must not delete zvols.
- **Uninstall:** remove the chart objects, stop the target, disconnect
  initiators, then optionally remove the module config. Modules cannot be
  unloaded while in use; a reboot is the reliable way to unload.

### 11.4 Proof before merge

When the blocker clears, the implementation PR must show:

```bash
make -n install-vm
helm lint charts/naslos -f charts/naslos/values.yaml -f charts/naslos/values-vm.yaml
helm lint charts/naslos -f charts/naslos/values.yaml -f charts/naslos/values-installer.yaml
helm template naslos charts/naslos -f charts/naslos/values.yaml -f charts/naslos/values-vm.yaml
sh scripts/audit.sh
```

plus the scratch-node drill from §13 and a clean-node reinstall. No one-off
`kubectl`, `helm --set`, or manual configfs edit is an acceptable install path.

## 12. Security review obligations

Any future privileged container or system extension must pass a review with
these items written down. This is the gate, not a checklist to wave through.

### 12.1 Command and path construction

- No `sh -c`, no shell interpolation, no `Command::new` with a user string.
  Configfs is written with file APIs, and every path component is validated.
- NQN: grammar, length, no `/` or `..`; host NQN and subsystem NQN are
  separate validations.
- Namespace ID: a bounded integer; port ID: a bounded integer. v1 uses one
  namespace, but the validator is not hard-coded to accept arbitrary strings.
- Device path: derived from a pool + zvol record, resolved, checked as a block
  device, and checked against the allowed zvol prefix. A user-supplied device
  path is never accepted.
- Golden tests for the exact configfs operations and a property test for
  invalid names, mirroring the vmd argv golden tests.

### 12.2 Egress

- The supervisor needs no outbound network access. It talks to the API over
  the node network and writes configfs. A default-deny egress policy is the
  intent; because the pod is host-network, a node-level control is the real
  enforcement.
- No URL import, no registry access, no telemetry from this pod.

### 12.3 Host mounts and privileges

- `/dev` HostToContainer (zvol device nodes appear after the agent creates
  them). No `/host` root mount, no host PID, no `/var/mnt` unless a concrete
  need is proven.
- Configfs mounted read-write inside the pod's mount namespace (for example
  `/run/nvmet-config`), with a read-only root filesystem and an emptyDir for
  the mount point. If the kernel's configfs cannot be mounted privately, stop
  and re-evaluate; do not silently fall back to a broad `/host` mount. A
  hostPath of `/sys/kernel/config` is acceptable only if the host already
  mounts configfs and the review accepts the narrower isolation.
- Prefer the minimal capability set (`SYS_ADMIN` plus root) over full
  `privileged: true` if a scratch-node test proves configfs and `/dev/zvol`
  access work. Either way the pod belongs in `naslos-privileged`, not the
  release namespace.
- The supervisor has no Kubernetes API token and no RBAC.

### 12.4 Secrets

- CHAP keys are Secret files, not environment variables, not values.
- The API never returns a key; logs and errors redact it. A test should grep
  the serialized API response and supervisor logs for the key.
- Rotation is explicit; revocation requires a reconnect.
- The Secret is scoped to the workload namespace; the API does not get to
  read other namespaces' Secrets.

### 12.5 Blast radius

- Configfs is global kernel state. A compromised supervisor can export any
  block device unless the code prevents it. The device-path allowlist and the
  absence of a generic "export this path" API are the controls; code review
  must treat a path-validation bypass as critical.
- The kernel target inherits the kernel's NVMe/TCP CVE surface. Patch cadence
  is tied to Talos releases. Track NVMe-oF CVEs and do not ship a kernel with a
  known exploitable issue.
- The host firewall is a double-edged control: it can lock the operator out.
  The feature must be opt-in, with a documented console recovery and a test of
  the rule in audit mode before enforcement.
- Rate/connection limits: the kernel accepts connections up to its queue
  limits; a hostile client can consume resources. The host allowlist and
  network ACL are the first controls. A future version can add connection
  accounting and alerts.

### 12.6 Review sign-off

The security review must be a written artifact in the implementation PR,
covering: threat model, command construction, egress, mounts, secrets,
device-path allowlist, configfs blast radius, kernel CVE status, firewall
recovery, and the test evidence. "It is just a storage target" is not a review.

## 13. Validation plan

The `T-NVME-*` IDs are referenced inline by the `FR-NVME` requirements in
[spec.md](spec.md#312-block-storage--nvme-of-tcp-fr-nvme). They are planned;
none exists in this tree.

| ID | Test | Pass criteria |
| --- | --- | --- |
| `T-NVME-01` | Stock-module and configfs capability probe on a scratch Talos v1.14 node: add `KernelModuleConfig` for `nvmet` and `nvmet-tcp`, reboot, confirm `/sys/module/nvmet` and `/sys/module/nvmet_tcp`, mount configfs, create a loop/null_blk namespace. | Modules load from the stock image; configfs tree appears; no custom kernel/extension. Record the exact kernel and module versions. |
| `T-NVME-02` | Zvol create/export/connect/I/O: 1 GiB zvol with `volmode=dev`, `volblocksize=16K`, `refreservation=none`; enable namespace; connect from a Linux initiator; `nvme id-ns`, write/read, `zpool iostat`; attempt export of a mounted zvol and delete of an exported zvol. | I/O succeeds; the mounted-export and exported-delete attempts are refused with a clear error; no data loss. |
| `T-NVME-03` | Access control: wrong host NQN, `allow_any_host=0`, wrong CHAP secret, correct CHAP, bidirectional CHAP, discovery filtering, secret redaction. | Wrong NQN/secret refused; correct secret connects; the API/logs never contain the key; discovery only lists allowed subsystems. |
| `T-NVME-04` | Supervisor crash and idempotence: run I/O, `kill -9` the supervisor, confirm controllers keep I/O; restart and confirm a no-op reconcile; delete/recreate a namespace and confirm no stale configfs objects. | Data plane survives; restart adopts the tree; no duplicate/partial objects. |
| `T-NVME-05` | Exposure: listener on the configured address only; `ss` shows 4420; host-firewall/ACL test from an allowed and a denied source; network-change rebind. | Only the intended address/subnet reaches 4420; no wildcard advertisement; a network change stops/rebinds the listener. |
| `T-NVME-06` | Failure drills: cable pull, node reboot, pool-full (small test zvol), zvol resize/rollback attempts while connected, CHAP rotation, host revocation. | Each failure has the documented symptom; no silent corruption; refusals are explicit; recovery steps work. |
| `T-NVME-07` | Performance matrix (§9.2), including 8/16/32 MiB I/O and MTU 1500/9000. | Thresholds in §9.3; **zero large-I/O errors**. If MDTS is 0 and large I/O fails, the feature is blocked. |
| `T-NVME-08` | Upgrade/rollback: Talos upgrade with the module config; chart upgrade/rollback; disable/re-enable the feature; verify zvol data and initiator reconnect. | Capability returns after reboot; data intact; rollback does not delete zvols. |
| `T-NVME-09` | Install-path render: `make -n install-vm`, `helm lint` on both profiles, `helm template`, machine-config overlay contains the module documents, clean-node install. | No manual `kubectl`/`helm --set`; clean install reproduces the target and the capability report. |
| `T-NVME-10` | Security review (§12) with the threat model, path-validation tests, secret-redaction test, mount/egress inspection, and host-firewall recovery drill. | Written sign-off; no unresolved critical/high finding. |

## 14. Open questions

1. **MDTS fix timing.** Will Talos backport the bounded-MDTS patch to the v1.14
   kernel, or only ship it in a later line? Is there any supported way to set
   MDTS on 6.18 (configfs `param_mdts` landed later)? If not, the feature is
   blocked on an OS upgrade.
2. **Configfs in a pod.** Does a privileged pod on the pinned Talos image mount
   configfs privately and see the global nvmet tree? If not, is a read-write
   hostPath acceptable, and what does that do to the isolation claim?
3. **`/dev/zvol` visibility.** Does HostToContainer `/dev` propagate a zvol
   created after the supervisor pod starts? The agent's mount propagation
   already handles datasets; zvol device nodes are a different path and must be
   proven.
4. **CHAP reality check.** Does the exact Talos kernel/`nvme-cli` combination
   negotiate the chosen hash/DH group without fallback? Which `nvme-cli`
   version is the documented minimum?
5. **Multi-namespace.** Is one namespace per subsystem enough for v1, or do
   VM/database users need several namespaces behind one NQN? Multi-namespace
   expands the access-control and deletion-guard surface.
6. **Live resize.** Can a zvol grow under an enabled namespace and be picked
   up by `revalidate_size` plus an initiator rescan, or must v1 require a
   disconnect?
7. **Static address.** Is a reservation/static storage address a product
   requirement, or can the rebind path be made reliable with DHCP? A listener
   on a stale address is a silent outage.
8. **ESXi and Windows demand.** If ESXi is a must-have, the kernel target is
   out (fused commands). Is there enough demand to justify a separate SPDK
   spike, with its hugepage/CPU/crash trade-offs?
9. **SPDK fallback.** If the kernel MDTS issue is not fixed within the target
   release window, is a narrow SPDK target acceptable despite the cost? This
   spike says no by default; a product decision could override it.
10. **Security posture.** Is a dedicated storage VLAN available on the target
    hardware, or is the LAN flat? Without a VLAN/ACL, CHAP is the only control
    and the data is cleartext.
11. **Host firewall.** Should NVMe-oF ship with an opt-in Cilium host-firewall
    rule, given the documented node lockout? The safer default is a switch ACL
    and a documented warning.
12. **Snapshots and clones.** Is a crash-consistent snapshot enough, or do
    users expect an application-consistent quiesce hook? That is a guest-side
    integration, not a target feature.
13. **Secret custody.** Who generates the CHAP keys: the API (random, stored
    in a Secret) or the operator (paste)? Rotation UX and recovery need a
    decision.
14. **NQN stability.** Use a persisted random export UUID in the NQN, or derive
    it from pool/zvol names? Renaming a zvol must not silently change the NQN.
15. **Observability.** The kernel does not expose a clean connected-host list
    in this configuration. Is "configured state + listener state + I/O errors"
    enough for the UI, or is a small kernel/debugfs dependency required?

## 15. References

All URLs were read on 2026-11-21.

- Talos v1.14 kernel configuration:
  <https://github.com/siderolabs/pkgs/blob/release-1.14/kernel/build/config-amd64>
- Talos kernel package (`modules_install`):
  <https://github.com/siderolabs/pkgs/blob/release-1.14/kernel/kernel/pkg.yaml>
- Talos configuration document map (`KernelModuleConfig`):
  <https://docs.siderolabs.com/talos/v1.14/reference/configuration/document-map>
- Talos machine-config editing (module load/unload semantics):
  <https://docs.siderolabs.com/talos/v1.14/configure-your-talos-cluster/system-configuration/editing-machine-configuration>
- Talos kernel-module signing:
  <https://docs.siderolabs.com/talos/v1.14/build-and-extend-talos/custom-images-and-development/kernel-module>
- Linux 6.18 `nvmet` Identify MDTS:
  <https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/plain/drivers/nvme/target/admin-cmd.c?h=v6.18>
- Linux 6.18 `nvmet-tcp` 4 MiB limit and missing `get_mdts`:
  <https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/plain/drivers/nvme/target/tcp.c?h=v6.18>
- Linux 6.18 host MDTS-to-queue mapping:
  <https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/plain/drivers/nvme/host/core.c?h=v6.18>
- nvmet configfs ABI:
  <https://www.kernel.org/doc/html/latest/admin-guide/abi-stable.html>
- nvme-cli `connect` options:
  <https://nvme-cli.readthedocs.io/en/latest/nvme-connect.html>
- SPDK NVMe-oF target (CHAP, TLS, JSON-RPC):
  <https://spdk.io/doc/nvmf.html>
- SPDK containers (hugepages, `--privileged`, host networking):
  <https://spdk.io/doc/containers.html>
- SPDK block devices (`bdev_aio`, `bdev_uring`):
  <https://spdk.io/doc/bdev.html>
- SPDK zvol `bdev_aio` regression and fix:
  <https://github.com/spdk/spdk/issues/3684>
- TrueNAS 25.10 NVMe-oF (ESXi incompatibility):
  <https://www.truenas.com/docs/scale/25.10/scaletutorials/shares/nvme-of/>
- TrueNAS 25.10.8 release notes (large NVMe/TCP transfer fix):
  <https://www.truenas.com/docs/scale/25.10/gettingstarted/versionnotes/>
- TrueNAS Proxmox plugin MDTS field report:
  <https://github.com/truenas/truenas-proxmox-plugin/issues/96>
- SUSE NVMe-oF (timeouts, discovery, multipath):
  <https://documentation.suse.com/sles/15-SP6/html/SLES-all/cha-nvmeof.html>
- NVMe over TCP field guide (order-of-magnitude expectations, not a
  controlled benchmark):
  <https://www.bigiron.cc/guides/nvme-over-tcp-deep-dive-when-its-the-right-block-protocol>
- Microsoft Windows NVMe-oF initiator preview (2026):
  <https://techcommunity.microsoft.com/blog/windowsservernewsandbestpractices/introducing-the-windows-nvme-of-initiator-preview-in-windows-server-insiders-bui/4501344>
- IBM/Ceph ESXi NVMe/TCP initiator (7.0U3+):
  <https://www.ibm.com/docs/en/storage-ceph/9.9.2?topic=initiator-configuring-nvme-vmware-esxi>
