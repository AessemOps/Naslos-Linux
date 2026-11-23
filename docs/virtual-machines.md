# Virtual machines (FR-VMT)

Naslos 0.4 ships a first-party QEMU/KVM hypervisor: the node itself runs VMs
with a browser console, ZFS-backed disks and cloud-init provisioning, the
same "the API is the brain" architecture the shares use. There is no
libvirt and no KubeVirt; a ~3k-line privileged supervisor (`naslos-vmd`)
owns the QEMU processes and nothing else.

```
browser ── noVNC (RFB over WS) ──> naslos-api ── WS tunnel (vmd TLS + token) ──> naslos-vmd ── 127.0.0.1 TCP ──> QEMU VNC
browser ── /api/virt/* ──> naslos-api ── token+TLS ──> naslos-vmd (QMP) ──> QEMU
naslos-api ── zfs create/destroy/rollback ──> naslos-agent ──> ZFS datasets
```

## Architecture

| Piece | Role |
| --- | --- |
| `naslos-vmd` (`rust/vmd`, image `ghcr.io/aessemops/naslos-vmd`) | Privileged hostNetwork pod, one per node (`Recreate`). Validates, builds every QEMU argument from the validated model (golden-tested), supervises children, speaks QMP (graceful stop, VNC passwords, balloon stats), generates cloud-init seeds, and serves the image library at `<pool>/vms/isos`. |
| `naslos-api` (`rust/api/src/virt.rs`, `server/virt.rs`) | The brain: validates every record, persists `vms.json` on the state PVC, renders the whole desired config and pushes it (`PUT /api/v1/vms/config` on vmd, revision-tagged: an identical
  revision is a no-op). State is always derived live from vmd and never
  persisted. |
| `naslos-agent` | Still the only ZFS writer: VM datasets are created/destroyed/snapshotted/rolled-back through the existing dataset APIs. |
| QEMU (`qemu-system-x86_64`, Debian package) | The hypervisor, unmodified distro binaries; q35 machine, virtio defaults, OVMF (UEFI) firmware by default with a per-VM writable vars copy, optional OVMF Secure Boot (`secureBoot`), SeaBIOS selectable for legacy guests. |

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
- `disk.format`, `disk.bus`, `disk.sourceImage` and `pool` are **immutable
  after creation**; the API answers `409` if an edit tries to change them.
  A `disk.sizeGB` change is also refused (`409`): growing a disk needs
  `qemu-img resize` plus a matching `zfs set quota`, and the supervisor API
  does not expose that yet — recreate the VM at the new size instead. The
  residual is tracked with R-047.
- UEFI vars are a per-VM copy (`<dataset>/vars.fd`) made on first start;
  Secure Boot uses a separate copy (`<dataset>/vars.secboot.fd`) so the two
  NVRAM stores never mix. The seed ISO is `<dataset>/seed.iso`, regenerated
  on every start.
- QEMU's stderr lands in `<dataset>/qemu.log` — the first place to look
  when a VM refuses to start.
- The image library is `<pool>/vms/isos` (flat file names). It is created
  lazily with the first VM or the first library upload/import.
- Snapshots are ordinary ZFS snapshots of the dataset (created through the
  agent, so Buddy backs them up like any other dataset). While a VM runs a
  snapshot is **crash-consistent** — the equivalent of yanking the power
  cord; prefer stopping the VM for a clean backup point.
- Restore is `zfs rollback -r` and rolls the dataset back to the target:
  **every snapshot newer than it is destroyed, and the guest loses all state
  written after that point** (the reset is irreversible). The call requires
  `?confirm=<name>`; a missing or mismatched name is `400`. It fails closed —
  `503` when vmd is absent, `502` when the live report is unreadable, `409`
  unless the VM is positively reported `stopped`. Deleting a VM carries the
  same `confirm=<name>` requirement and the same fail-closed checks, and both
  serialize with start/stop through a per-VM lifecycle lock so a delete can
  never interleave with a queued start.

## Firmware and Secure Boot

- **UEFI (OVMF) is the default**; BIOS (SeaBIOS) stays selectable for legacy
  guests. UEFI keeps a per-VM writable NVRAM store (`vars.fd`).
- **Secure Boot is opt-in per VM** (`secureBoot`, default `false`, UEFI
  only). When enabled, vmd boots the SMM-enabled OVMF Secure Boot code image
  (`OVMF_CODE_4M.secboot.fd` on Debian), a per-VM `vars.secboot.fd`
  initialized from the key-enrolled `OVMF_VARS_4M.ms.fd`, `smm=on`, the
  secure pflash global and the ICH9 S3-disable global from the edk2 reference
  command line. The guest therefore boots only signed boot loaders that chain
  to the Microsoft keys already in that vars template (Windows, or a Linux
  distro's signed shim). Custom key enrollment is not exposed in the UI or
  API.
- The supervisor reports whether its image has the Secure Boot pair as
  `secureBoot` on `GET /api/virt/capabilities`. The UI disables the checkbox
  when false, and a start request that asks for Secure Boot on an image
  without the pair fails with an actionable error instead of silently booting
  the plain UEFI firmware. Toggling the flag on a running VM recreates the
  QEMU process.
- Operators can override the detected paths with
  `VMD_UEFI_CODE`/`VMD_UEFI_VARS` (plain UEFI) and
  `VMD_UEFI_SECBOOT_CODE`/`VMD_UEFI_SECBOOT_VARS` (Secure Boot); both
  variables of a pair must be set.
- **TPM 2.0 is `[OPEN]`** (see `docs/vm-parity.md`). The vmd image does not
  ship `swtpm`, so there is deliberately no TPM field or toggle: a broken
  device path is worse than none. Adding it needs `swtpm`/`swtpm-tools` in
  the vmd image (plus `CREDITS.md` entries), a supervised per-VM
  `swtpm socket --tpm2` process with its state under the VM dataset, and the
  QEMU `tpm-tis` device wired over the swtpm socket. Windows 11 additionally
  needs both Secure Boot and that TPM path.

## Networking

- **NAT only (v1)**: QEMU slirp. The VM reaches the LAN/Internet through
  the node; inbound access is via editable TCP port forwards
  (`node:port → guest:port`) in the create/edit wizard. The ports listen
  on the node itself — same reachability as an SMB share.
- Every VM gets a stable MAC derived from its name, so the slirp-side
  addressing survives restarts.
- **LAN attachment is [OPEN]** (a guest with its own DHCP address on your
  LAN). The planned path is a Talos `BridgeConfig` host bridge — the
  sanctioned KubeVirt-on-Talos pattern — which moves the node's primary
  address onto the bridge, so it is a separate release; 0.5.2 makes no
  machine-config change. The earlier macvtap draft was removed rather than
  shipped: its probe could false-negative on kernels that do have macvtap,
  and the device was created without the guest's stable MAC, so its ingress
  filter would have dropped unicast traffic to the guest.

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
  process). The bridge enforces a strict origin policy (the browser
  `Origin` must be exactly the HTTPS authority of the request's own `Host`,
  port included; HTTP and SSO/base-domain expansion are refused), a per-VM
  attach rate limit, max-session/idle timeouts and a concurrent-session
  semaphore (`api.virtConsole` values: 4 h / 10 min / 4 sessions).
- QEMU's VNC listener binds **loopback only** (`127.0.0.1`), so it is never
  directly reachable from the LAN or the pod network. The API bridges through
  an authenticated tunnel on vmd's existing TLS listener: the API's
  `vnc_bridge` opens a WebSocket to `/api/v1/vms/{name}/vnc?port=<port>` on
  vmd with the bearer token, and vmd proxies the bytes to
  `127.0.0.1:<port>`. The route is behind the same token middleware as every
  other vmd API, refuses any port outside the configured VNC display range,
  and requires the path VM name to own that port (`5900 + display`). Both
  checks together mean a token cannot turn the tunnel into a proxy for
  another VM's console or an arbitrary loopback service.
- Each console session writes an audit begin/end record attributed to the
  owner; the route refuses to start (`503`) while the audit log is degraded,
  and a failed audit write during the session marks the log degraded.
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
  memory**, appended at an explicit offset by vmd. The route is the API's
  only non-JSON body; the content-type carve-out is scoped to
  `/api/virt/images/upload` (contract-tested) and octet-stream still
  forces a CORS preflight, so the CSRF posture is unchanged.
  - vmd stages the upload in `<name>.part` (created with `create_new` and
    `O_NOFOLLOW`) and renames it into the library only on the `final=true`
    chunk. A failed or over-cap upload never replaces the previous image,
    and a planted symlink is refused rather than followed. A failed publish
    removes its staged file, and the next upload (or URL import) start
    sweeps any orphaned `.part` files left by a crash.
  - `offset` must equal the current staged size, so chunks are strictly
    contiguous. The browser generates one owner token per upload (mandatory
    `owner` query parameter on every chunk) and echoes it; a chunk from a
    different owner is refused (`409`) instead of discarding the active
    upload's progress. The token is stored in `sessionStorage` per pool+name,
    so a panel reload resumes the same server session; a fresh browser starts
    clean only once the old session is reclaimable (an idle session expires
    after two hours, and each chunk refreshes it). A stale offset, a lost
    staging file and an expired session all tell the caller to restart from
    offset 0: the same owner may then restart its own session, and only an
    idle session may be superseded by another owner. A retried chunk rewinds
    the staged file to the request's start offset. A chunk can never attach to
    a URL import's staging file.
  - `DELETE /api/virt/images/upload?pool=&name=&owner=` releases a session:
    only its owner may delete the staged `.part` (409 for another client or a
    URL import; an absent session is an idempotent `200`). The panel calls it
    from its "Cancel upload" action and after an owner-mismatch conflict.
  - The 32 GiB cap applies to the whole upload (`offset + bytes`), not to
    one request; an over-cap upload is removed and returns `413`.
  - Replacing an existing library image requires `confirm=<name>`; the
    Images panel detects that the upload/import target is already listed and
    shows an explicit "Replace the existing image" checkbox before it sends
    the confirmation. A `409` reloads the listing first, so a stale library
    view still reveals the checkbox instead of looping on the same refusal. vmd refuses the replacement (or a delete) while a
    running/starting VM attaches the file as its ISO or `disk.sourceImage`
    (`409`), including a re-check immediately before a URL import publishes.
  - The API route forwards the confirmation verbatim: a re-upload that
    would replace an existing image must send `confirm=<name>` on
    `/api/virt/images/upload`, and a refused overwrite reaches the caller
    as `409 Conflict` (not a bad-gateway) with the supervisor's reason.
- URL import: the node downloads the image itself (`202 Accepted`; errors
  surface in the library listing). vmd is hostNetwork, whose traffic is
  exempt from pod egress policy, so the FR-DNS-07 SSRF guard is the
  control: http(s) only, every resolved address must be public
  (loopback/private/link-local/CGNAT/NAT64/mapped-v6 all refused),
  redirects re-validated hop by hop and pinned to the vetted address, 3
  hops max, optional sha256 verified (mismatch deletes the staged file).
  Importing over an existing library name likewise requires an explicit
  `"confirm": "<name>"` field in the `POST /api/virt/images/import` body;
  without it the import refuses to replace the existing image and records
  the refusal in the library listing.
  The download is staged in `<name>.part` like an upload, and replacing an
  existing image needs `"confirm": "<name>"` in the request body.
- Deletion is `DELETE /api/virt/images/{name}` (there is no mutating GET
  alias); it refuses while a running VM attaches the image.

## Autostart, failure events, stats

- On API startup the desired config is re-pushed; autostart-marked VMs are
  set `desired=running` (pod restart = VM restart for autostart VMs — flip
  autostart off before a maintenance restart if you want a VM to stay
  down). vmd waits bounded (60 s) for a fresh dataset mountpoint to appear
  before starting: the agent mounts the dataset via `chroot /host`, and its
  host-root bind is Bidirectional so the mount reaches the host and
  propagates into vmd's `/var/mnt` without a pod restart (a pre-Bidirectional
  agent left the mount pod-local and this wait was the 60s failure).
- Reconciles are serialized and generation-checked: two config pushes never
  run two reconcilers at once, a slow older push is a no-op, and a start
  reserves the VM name + VNC display under the state lock before the 60 s
  dataset wait. QEMU is spawned with `kill_on_drop(true)`, so a cancelled
  start or a supervisor shutdown cannot orphan a process.
- The watchdog reconciles every 30 s: a desired-running VM whose QEMU
  exited is restarted (and re-reported in the live view), and a reaped
  entry is treated as absent by start/stop instead of as alive.
- A VM that fails to start or exits unexpectedly raises an ntfy
  notification (`vm_failed` / `vm_exited_unexpected`) — enable them on the
  Notifications page. Transitions only: one message per failure.
- vCPU/RAM: CPU is the QEMU process average since start; guest memory
  comes from the virtio balloon (`query-balloon`), so it reads 0 until the
  guest's balloon driver is up.

## Security posture

- vmd is privileged by design (KVM, dataset writes). Its
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
  validates, vmd re-validates, then renders). Secure Boot is a boolean enum
  choice, never a caller-supplied firmware path, and the render pins the
  secboot/SMM/secure-pflash options.
- Every vmd-created path under `<pool>/vms` (`disk.qcow2`, `vars*.fd`,
  `qemu.log`, `seed.iso`, library images and the `.part` staging files) is
  checked with `symlink_metadata`/`O_NOFOLLOW` and a pre-existing symlink is
  refused. A share-writable dataset directory therefore cannot redirect a
  QEMU open, a log truncate, or an image write to a host file.
- The cloud-init seed quotes every YAML scalar (single quotes, `''` escape)
  and rejects every control character (including `\r`, NEL and U+2028/2029)
  in an SSH key, so a key comment cannot inject another cloud-config
  directive.
- Secure Boot is guest firmware trust, not additional host isolation: vmd is
  still privileged, the Secure Boot pair comes from the same distro `ovmf`
  package already in the image, and no new host mount or device is added.
  TPM (`swtpm`) is not installed and no TPM device exists until the `[OPEN]`
  work in `docs/vm-parity.md` lands.
- The QEMU VNC listener binds loopback (`127.0.0.1`) with a per-boot
  password; the password only travels to the browser via the owner-gated
  preflight, and a failure to generate it refuses the start. The RFB port is
  not reachable from the LAN at all. Console sessions are audited and
  refused while the audit log is degraded.

## Chart values

| Key | Default | Meaning |
| --- | --- | --- |
| `virt.enabled` | `true` | Deploy the supervisor + wire the API client. |
| `virt.image` | `ghcr.io/aessemops/naslos-vmd:0.4.0` | Ninth CI image (buddy-receiver is the eighth); one of the 8 digest-pinned chart images in released packs (FR-INSTALL-13). |
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
6. Secure Boot: `GET /api/virt/capabilities` reports `secureBoot: true`;
   create/edit a UEFI VM with `secureBoot` on, start a signed installer, and
   confirm the guest reports Secure Boot enabled (`mokutil --sb-state`, or
   `dmesg | grep -i secure` for a Linux guest). A `secureBoot` request on an
   image without the firmware pair must fail with an actionable error.
7. Stop (graceful ACPI), snapshot, restore with `?confirm=<name>` and a
   typed VM-name prompt (refuses while running and destroys newer snapshots),
   force stop, delete with `?confirm=<name>` and a typed-name prompt → dataset
   destroyed.
8. Kill the vmd pod → its QEMU children die with it (they live in the pod's
   PID namespace); the API watcher notices the supervisor's return and
   re-pushes the desired state, so desired-running VMs restart — an
   explicitly stopped VM stays stopped. A killed QEMU raises
   `vm_exited_unexpected`.

## VNC console exposure

The vmd pod is hostNetwork because VM NAT host forwards must bind the node's
ports, but VNC is not one of them: vmd binds each QEMU console to `127.0.0.1`
and streams it through the authenticated `/api/ws/vm/<name>/vnc` WebSocket.
The chart exposes only the headless 9091 Service, runs vmd in the privileged
workload namespace with `hostPID`/`hostIPC` off and no service-account token,
and does not publish a 5900-range port. The pod deliberately has no memory
limit: QEMU children count against its cgroup, so a limit below the sum of the
declared guest memory (plus overhead) would OOM-kill running VMs; size the node
for that sum instead. A future per-VM memory limit belongs with the VM spec
(WP-03), not a static pod limit.
