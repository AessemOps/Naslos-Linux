# Virtual machines (FR-VMT)

Naslos 0.4 ships a first-party QEMU/KVM hypervisor: the node itself runs VMs
with a browser console, ZFS-backed disks and cloud-init provisioning, the
same "the API is the brain" architecture the shares use. There is no
libvirt and no KubeVirt; a ~3k-line privileged supervisor (`naslos-vmd`)
owns the QEMU processes and nothing else.

```
browser ── noVNC (RFB over WS) ──> naslos-api ── TCP bridge ──> QEMU VNC (per-VM password)
browser ── /api/virt/* ──> naslos-api ── token+TLS ──> naslos-vmd (QMP) ──> QEMU
naslos-api ── zfs create/destroy/rollback ──> naslos-agent ──> ZFS datasets
```

## Architecture

| Piece | Role |
| --- | --- |
| `naslos-vmd` (`rust/vmd`, image `ghcr.io/aessemops/naslos-vmd`) | Privileged hostNetwork pod, one per node (`Recreate`). Validates, builds every QEMU argument from the validated model (golden-tested), supervises children, speaks QMP (graceful stop, VNC passwords, balloon stats), creates macvtap devices for LAN mode, generates cloud-init seeds, and serves the image library at `<pool>/vms/isos`. |
| `naslos-api` (`rust/api/src/virt.rs`, `server/virt.rs`) | The brain: validates every record, persists `vms.json` on the state PVC, renders the whole desired config and pushes it (`PUT /api/v1/vms/config` on vmd, revision-gated). State is always derived live from vmd and never persisted. |
| `naslos-agent` | Still the only ZFS writer: VM datasets are created/destroyed/snapshotted/rolled-back through the existing dataset APIs. |
| QEMU (`qemu-system-x86_64`, Debian package) | The hypervisor, unmodified distro binaries; q35 machine, virtio defaults, OVMF (UEFI) firmware by default with a per-VM writable vars copy, SeaBIOS selectable for legacy guests. |

Why not KubeVirt: the node runs Kubernetes 1.37, ahead of KubeVirt's
supported skew, and KubeVirt demands bare-metal K8s features that add
operational surface a single-node appliance does not need. Why not
libvirt/Incus: a second privileged daemon whose config surface we would
have to mirror, audit and troubleshoot, versus a small supervisor whose
entire command line is pinned by unit tests.

## Storage

- A VM's dataset is `<pool>/vms/<name>`, created by the API through the
  agent with `quota` equal to the declared disk size and
  `compression=off` (the image is already compressed/sparse).
- The disk image is `<dataset>/disk.qcow2` (or `disk.qcow2` in raw mode —
  the file name stays, the format flag changes), created **sparse** on first
  start, or seeded from a library cloud image (`disk.sourceImage`) with
  `qemu-img convert` + resize to the declared size.
- UEFI vars are a per-VM copy (`<dataset>/vars.fd`) made on first start;
  the seed ISO is `<dataset>/seed.iso`, regenerated on every start.
- QEMU's stderr lands in `<dataset>/qemu.log` — the first place to look
  when a VM refuses to start.
- The image library is `<pool>/vms/isos` (flat file names). It is created
  lazily with the first VM or the first library upload/import.
- Snapshots are ordinary ZFS snapshots of the dataset (created through the
  agent, so Buddy backs them up like any other dataset). While a VM runs a
  snapshot is **crash-consistent** — the equivalent of yanking the power
  cord; prefer stopping the VM for a clean backup point. Restore is
  `zfs rollback -r` and refuses a running VM; newer snapshots are deleted
  by the rollback, which the UI states before you confirm.

## Networking

- **NAT (default)**: QEMU slirp. The VM reaches the LAN/Internet through
  the node; inbound access is via editable TCP port forwards
  (`node:port → guest:port`) in the create/edit wizard. The ports listen
  on the node itself — same reachability as an SMB share.
- **LAN (macvtap)**: the VM attaches to a physical NIC via a macvtap bridge
  device (`mtap-<hash>`) and gets **its own DHCP address** on your LAN, as
  if it were a separate machine. Two caveats, both macvtap properties:
  the **NAS host itself cannot talk to the guest** (reached from any other
  device instead), and the v1 UI enforces no forwards in this mode.
- No Talos machine-config change is involved in either mode; a full host
  bridge (so the host can reach guests) is explicitly out of scope for v1.
- Every VM gets a stable MAC derived from its name, so DHCP assignments
  survive restarts.

## Provisioning (cloud-init)

When enabled, vmd generates a NoCloud seed ISO (`cidata`, genisoimage)
from the form fields only: hostname, user, SSH public keys. Passwords are
never accepted: `ssh_pwauth: false` and `lock_passwd: true` are baked in,
so the image cannot fall back to password login. The `instance-id` in the
seed changes with the seed content, so editing keys re-runs cloud-init on
the next boot.

To provision a cloud image (e.g. Ubuntu cloud): import the `.qcow2`/`.img`
into the library (URL import works well), create the VM with
**Seed the disk from a cloud image**, start it — the disk is seeded and
resized to the declared size, the seed carries your user/keys, and the VM
boots straight to SSH.

## Console

`GET /api/ws/vm/{name}/vnc` (owner-gated like every `/api` route):

- A plain GET is the **preflight**: `{vncPort, vncPassword, status}` or a
  readable error (`409` while the VM is stopped). A browser cannot read a
  failed websocket handshake, so this is how the page shows "start the VM
  first".
- A websocket upgrade bridges RFB bytes to QEMU's VNC listener on the
  node — the websockify pattern implemented inside the API (no extra
  process). The bridge enforces the `/api/ws/exec` posture: same-origin
  check, max-session/idle timeouts, and a concurrent-session semaphore
  (`api.virtConsole` values: 4 h / 10 min / 4 sessions).
- The client is noVNC (`@novnc/novnc` 1.5.0, MPL-2.0 — see CREDITS.md),
  which performs the standard VNC auth against QEMU using the per-boot
  password vmd set over QMP. QEMU's VNC listener is password-protected;
  the password rotates every boot and is only ever handed to the browser
  through the preflight above.
- 1.5.0 is pinned deliberately: noVNC ≥1.6 uses a top-level `await`
  (WebCodecs H.264 probe) that the UI's build target does not support, and
  QEMU's VNC never streams H.264 anyway.

## Uploads and URL imports

- Browser upload: chunked (8 MiB `application/octet-stream` requests, XHR
  progress), relayed by the API **without buffering the file in API
  memory**, appended at an explicit offset by vmd. A failed chunk resumes
  from the last confirmed offset. The upload route is the API's only
  non-JSON body; the content-type carve-out is scoped to
  `/api/virt/images/upload` (contract-tested) and octet-stream still
  forces a CORS preflight, so the CSRF posture is unchanged.
- URL import: the node downloads the image itself (`202 Accepted`; errors
  surface in the library listing). vmd is hostNetwork, whose traffic is
  exempt from pod egress policy, so the FR-DNS-07 SSRF guard is the
  control: http(s) only, every resolved address must be public
  (loopback/private/link-local/CGNAT/NAT64/mapped-v6 all refused),
  redirects re-validated hop by hop and pinned to the vetted address, 3
  hops max, optional sha256 verified (mismatch deletes the file).

## Autostart, failure events, stats

- On API startup the desired config is re-pushed; autostart-marked VMs are
  set `desired=running` (pod restart = VM restart for autostart VMs — flip
  autostart off before a maintenance restart if you want a VM to stay
  down). vmd waits bounded (60 s) for a fresh dataset mountpoint to appear
  (the mount-propagation trap) before starting.
- A VM that fails to start or exits unexpectedly raises an ntfy
  notification (`vm_failed` / `vm_exited_unexpected`) — enable them on the
  Notifications page. Transitions only: one message per failure.
- vCPU/RAM: CPU is the QEMU process average since start; guest memory
  comes from the virtio balloon (`query-balloon`), so it reads 0 until the
  guest's balloon driver is up.

## Security posture

- vmd is privileged by design (KVM, tun, macvtap, dataset writes). Its
  HTTP API is on the node's network, so it carries the exact agent
  posture: a chart-generated bearer token (`naslos-vmd` Secret) checked on
  every route but `/health`, and chart-generated TLS (PF-M5) —
  `naslos-vmd-tls` + a CA-only copy pinned by the API client. No plaintext
  opt-out exists short of an explicit `VMD_ALLOW_PLAINTEXT=1` for local
  development.
- The NetworkPolicy for vmd records intent (:9091 from the API only) but
  hostNetwork pods are exempt from pod-level policy — the token + TLS are
  the enforcement, exactly like the agent.
- SEC-1: no unvalidated string reaches a QEMU command line. Names are
  DNS-1123, library references are flat validated file names, and the
  argument builder is pinned by golden tests on both sides (the API
  validates, vmd re-validates, then renders).
- The QEMU VNC listener binds the node with a per-boot password; the
  password only travels to the browser via the owner-gated preflight. If
  you want the port unreachable from the LAN, firewall 5900-5916 at the
  router — the UI console keeps working because the API bridges it.

## Chart values

| Key | Default | Meaning |
| --- | --- | --- |
| `virt.enabled` | `true` | Deploy the supervisor + wire the API client. |
| `virt.image` | `ghcr.io/aessemops/naslos-vmd:0.4.0` | Ninth image, same digest-pin discipline (FR-INSTALL-13). |
| `virt.tls.enabled` | `true` | Chart-generated vmd TLS + API CA pin. |
| `virt.tokenSecret` | `""` | Bring-your-own Secret (needs a `token` key). |
| `virt.vnc.firstDisplay` / `displays` | `1` / `16` | VNC display range (ports 5901..5916); bounds concurrent consoles. |
| `api.virtConsole.*` | 4 h / 10 min / 4 | Console bridge session caps. |
| `api.vmsFile` | `/var/lib/naslos/vms.json` | Records on the state PVC, next to shares/apps. |

## Live drill (operator)

The full gate from the plan, for the record:

1. `make install-vm` (vmd image built + pushed first).
2. `GET /api/virt/capabilities` → `kvm: true`, NICs listed.
3. Import a tiny cloud image by URL (`POST /api/virt/images/import`) → it
   appears in `<pool>/vms/isos`; a private-URL import is refused.
4. Create a VM from it (cloud-init on, one SSH key), autostart off →
   dataset `<pool>/vms/<name>` exists with the declared quota.
5. Start → state `running`, console connects via the browser, `qemu.log`
   exists, port forward answers on the node (NAT mode).
6. Stop (graceful ACPI), snapshot, restore (refuses while running), force
   stop, delete → dataset destroyed.
7. Kill the vmd pod → its QEMU children die with it (they live in the pod's
   PID namespace); the API watcher notices the supervisor's return and
   re-pushes the desired state, so desired-running VMs restart — an
   explicitly stopped VM stays stopped. A killed QEMU raises
   `vm_exited_unexpected`.
