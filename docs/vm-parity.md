# VM parity: remaining gaps (`FR-VMT`)

This page tracks the VM capabilities that are designed but **not implemented**.
Secure Boot (FR-VMT-12) and the rest of the shipped VM feature set are covered
in [virtual-machines.md](virtual-machines.md); the normative requirements are in
[spec.md](spec.md) §3.13. Nothing on this page is wired into the chart, the API,
the UI or the supervisor today: each item lists the requirement shape, the
blocking dependencies and the review needed before implementation.

| Gap | Spec | Status | Why it is separate |
| --- | --- | --- | --- |
| LAN bridge | FR-VMT-04 | `[OPEN]` | Re-addresses the node's primary interface in the Talos machine config; own release |
| Disk import/export | FR-VMT-14 | `[OPEN]` | Untrusted image parsing, conversion pipeline, format allow-list |
| PCIe/USB/GPU passthrough | FR-VMT-15 | `[OPEN]` | Talos device plugins, new privileged host devices, mandatory security review |
| TPM 2.0 | FR-VMT-13 | `[OPEN]` | The vmd image does not ship `swtpm`; needs an image + supervisor process dependency |

## LAN bridge (FR-VMT-04)

Today the only networking mode is NAT (QEMU slirp) with TCP port forwards.
A guest that needs its own DHCP address on the LAN requires a host bridge.

**Requirement shape**

- `net.mode: "lan"` plus `net.nic` naming one of the physical NICs reported by
  `GET /api/virt/capabilities`; port forwards are rejected in LAN mode. The
  record fields already exist for forward compatibility.
- The bridge is created by a Talos `BridgeConfig` machine-config document (the
  sanctioned KubeVirt-on-Talos pattern), not by vmd. A separate release must
  add it to the declarative install path and to `KubeNodeConfig.nodeIP.validSubnets`
  when the primary address moves.
- vmd attaches the QEMU tap to that bridge and keeps the VM's stable
  name-derived MAC; MTU and IPv6 follow the bridge.
- The change must preserve API/UI reachability through the bridge, and needs a
  documented revert path (the day-2 network try/revert pattern in
  `docs/deployment.md` is the precedent).

**Do not revive macvtap.** The removed draft's probe created a throwaway
macvtap linked to `lo` (a false negative on kernels that support macvtap), and
the device was created without the guest's stable MAC, so macvtap's ingress
filter dropped unicast traffic to the guest.

**Release constraint:** the node's primary interface is re-addressed, so this
cannot ship as a quiet VM patch; it needs its own release, an upgrade note and
a live drill on a node with a second physical NIC.

## Disk import/export (FR-VMT-14)

Cloud images can already be imported into the per-pool library and seeded into
a new disk. Importing an existing VM disk and exporting a Naslos disk are not
implemented.

**Requirement shape**

- **Format allow-list**, not an open `qemu-img` passthrough: `qcow2`, `raw`,
  `vmdk`, `vdi` and `vhdx` in, `qcow2` and `raw` out. Every source is probed
  with `qemu-img info` and refused unless its detected format is on the list;
  the format string is never caller-controlled.
- **Import pipeline:** upload or URL-import the source into the library with
  the existing chunk/SSRF paths, convert to a temporary file on the target VM
  dataset (`qemu-img convert -O qcow2`), verify the virtual size against the
  declared disk size and dataset quota, fsync and atomically rename to
  `disk.qcow2`. Refuse while the VM is running; clean up the temporary file on
  failure or cancel.
- **Export pipeline:** require the VM stopped (or base a future live export on
  a ZFS snapshot), convert to the requested allow-listed format in a temporary
  file, checksum it and stream it to the owner-gated download route. Never
  write to an operator-supplied path; keep the temporary file inside the VM
  dataset or a bounded scratch area and delete it after the transfer.
- **Security:** `qemu-img` is a C image parser and is the attack surface. Run
  it with resource limits, keep sources read-only, enforce a maximum virtual
  and on-disk size, and treat a malformed image as a failed job (no partial
  disk becomes bootable). Add unit tests for the format allow-list, size/quota
  checks and temp-file cleanup, plus an integration test on a scratch dataset.

## PCIe / USB / GPU passthrough (FR-VMT-15)

No host device can be attached to a VM today. This is the largest VM gap and
the one with the highest security cost.

**Requirement shape**

- A Talos device-plugin/extension path for `vfio-pci` (IOMMU on, device bound
  to `vfio-pci`, ACS/IOMMU-group reporting) plus a capability report that lists
  only passthrough-safe devices. USB passthrough (`usb-host`) needs an
  explicit host device allow-list from `/dev/bus/usb`.
- A per-VM device record (vendor/device or PCI address, never an arbitrary
  host path) rendered by vmd as `-device vfio-pci,host=…` or
  `-device usb-host,…`; devices are exclusive, reset on VM stop, and refused
  when already claimed.
- The supervisor pod is already privileged, but passthrough adds new host
  device nodes and a DMA-capable path into the guest. **Security review is
  mandatory** before any code: DMA/IOMMU escape risk, malicious device
  firmware, host-driver detach/reset, secrets in guest memory, snapshot and
  rollback semantics (device state cannot be snapshotted), and audit of every
  attach/detach.
- Chart/install changes (device plugin, node labels, privileged mounts) and a
  live drill on hardware with a spare GPU/USB controller are prerequisites;
  feature detection must degrade to "unavailable", never to a broken start.
- This cannot be implemented in the API/UI alone: vmd and the Talos node
  configuration change together.

## TPM 2.0 (FR-VMT-13)

The record schema, UI and supervisor have no TPM today, and the vmd image does
not install `swtpm`. A TPM flag without the emulator would produce a guest that
silently lacks the device (and would fail Windows 11 checks), so the field is
deliberately absent until the dependency work lands.

**Exact dependency and image work**

1. Install `swtpm` and `swtpm-tools` (Debian bookworm packages) in
   `vmd/Dockerfile`, and add both packages plus `libtpms` to `CREDITS.md`.
2. Add `tpm: bool` to DM-7 (default `false`, UEFI-only, like `secureBoot`),
   validation in `naslos_vmd::model::validate_vm`, the desired-config render
   and the UI toggle.
3. Create a per-VM state directory (`<dataset>/tpm/`) so the TPM state is
   preserved across restarts and included in ZFS snapshots; refuse to start if
   the directory cannot be created.
4. Supervise one `swtpm socket --tpm2 --tpmstate dir=<dataset>/tpm
   --ctrl type=unixio,path=<run_dir>/<name>.swtpm-sock` process per VM alongside
   QEMU, stop it with the VM, and remove the socket on stop (the state stays).
5. Render `-chardev socket,id=chrtpm,path=… -tpmdev
   emulator,id=tpm0,chardev=chrtpm -device tpm-tis,tpmdev=tpm0`; pin the argv
   in the golden tests and add a supervisor test that the emulator is started
   before QEMU and reaped with it.
6. Report `tpm` in capabilities and refuse a start when the image lacks
   `swtpm`; add a UI hint that Windows 11 needs Secure Boot **and** TPM.

**Security notes:** the swtpm state is guest secret material; keep it on the
VM dataset (already owner-only and quota-bounded), never copy it into the ISO
library, and include it in the snapshot/restore semantics. swtpm runs as the
same unprivileged-in-pod user as vmd, needs no new host mount, and must not be
reachable outside the per-VM socket.

## Out of scope here

- Secure Boot custom key enrollment (the shipped path uses the Microsoft keys
  pre-enrolled by the distro vars template).
- VM HA/failover, live migration and vGPU partitioning.
- A second hypervisor (KubeVirt/libvirt/Incus) or a host shell.
