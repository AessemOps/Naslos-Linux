# QEMU Hypervisor with VM Management UI — v0.4.0

Plan date: 2026-11-19 · Status: approved design, ready to implement

Add a first-party QEMU/KVM hypervisor to Naslos with a full management
interface: VM lifecycle, VNC console, NAT and LAN networking, ISO/cloud-image
provisioning, cloud-init, and per-VM ZFS snapshots — released as **0.4.0**.

> Workflow notes: this plan file gets committed on the feature branch (plans
> are committed per AGENTS.md). Never put production node identifiers (URLs,
> domains, IPs) in PRs, commits, or commit messages — say "the production
> node".

## 1. Decision record (user-approved)

| Decision | Choice |
| --- | --- |
| Approach | **Custom QEMU supervisor** — new `naslos-vmd` image/pod, QMP-controlled by the API. No cluster dependencies. |
| Rejected: KubeVirt | Proven on Talos, but the node's Kubernetes 1.37 is ahead of the supported matrix (KubeVirt 1.9 tops out at 1.36; 1.10 not released yet), and it would add an operator + ~30 CRDs for features (migration, scheduling) a single node cannot use. Revisit only for a future multi-node story. |
| Rejected: libvirt-in-pod | Adds a stateful daemon + dual state (libvirt XML vs `vms.json`) without removing any of our UI/console/networking work. |
| Rejected: Incus-in-pod | k8s pods cannot get `cgroupns=host`; Incus wants to own ZFS pools and bridges, conflicting with the platform's storage/network management. |
| Networking (v1) | **NAT (slirp) with editable port forwards** + **macvtap LAN attachment** (own DHCP address on the LAN). No Talos machine-config changes. macvtap caveat: the NAS host itself cannot talk to macvtap guests; document in UI. Full host bridge stays out of scope. |
| Storage | **Per-VM child ZFS dataset** `<pool>/vms/<name>`, quota = declared disk size, sparse qcow2 default (raw optional), UEFI (OVMF) default firmware, SeaBIOS option. ISO/image library under `<pool>/vms/isos`. |
| Provisioning | Browser ISO/disk-image upload (chunked), server-side URL import (sha256 optional), cloud-init NoCloud seed from a form (hostname, user, SSH keys — no passwords). |
| v1 cut | Lifecycle CRUD + start/stop/force-stop/restart + autostart, per-VM vCPU/RAM with live usage, noVNC console, port forwards, ISO library, cloud-init, hardware preflight, per-VM snapshot/rollback (restore requires VM stopped), ntfy on failed start / unexpected exit. Buddy needs zero work (VM datasets are ordinary datasets; crash-consistent while running). |
| Deferred (v2+) | USB/PCI/GPU passthrough, templates/cloning (`zfs clone`), serial console, Ignition, secure boot/TPM, host-bridge machine-config mode, multi-node/KubeVirt migration. |
| Release | **0.4.0** (minor bump), staged PRs on `feature/virt-*`, installer lockstep via the release dispatch. |

## 2. Architecture

```
UI (/vms, noVNC) ── /api/virt/* ──► naslos-api (vms.json source of truth,
        │                            renders desired state, validates everything)
        └─ WS /api/ws/vm/{n}/vnc          │
                                         ├─► naslos-agent (creates <pool>/vms/<n>
                                         │    datasets + quotas — only zfs writer)
                                         └─► naslos-vmd (privileged pod, hostNetwork,
                                              bearer token + TLS like the agent)
                                              • reconciles QEMU processes vs desired
                                              • QMP: shutdown, stats, screendump
                                              • -vnc :N,websocket=<port> (native
                                                RFB-over-WebSocket; no websockify)
                                              • macvtap devices on the physical NIC
                                              • genisoimage cloud-init seeds
                                              • ISO/disk upload sink, URL importer
```

Key invariants (all copied from existing, audited patterns):

- **API is the brain, vmd applies.** Same shape as shares: validate → render
  desired state → push `PUT /api/v1/vms/config` (revision-gated, atomic) →
  vmd diffs and starts/stops/recreates QEMU. A bad render never reaches the pod.
- **SEC-1 pattern.** Every QEMU argument is generated server-side from a
  validated model; VM names DNS-1123; no user-supplied strings ever reach a
  command line unvalidated. Golden-argument unit tests.
- **vmd auth = agent auth.** Mandatory bearer token (constant-time) + TLS,
  chart-generated Secret, no plaintext opt-out; headless Service on :9091.
- **Owner gate on every `/api/virt/*` route.** `require_admin` +
  proxy-secret, JSON errors, list responses are arrays, WS gets the same
  same-origin check, session caps and plain-GET preflight as `/api/ws/exec`.
- **Pod: single replica, `Recreate` strategy** (two vmd pods would fight over
  QEMU processes, host ports and macvtap devices). Pod restart = VM restart
  for autostart-marked VMs (vmd reconciles on boot; documented behavior).
- **vmd mounts**: `/dev` (HostToContainer, gives `/dev/kvm`, `/dev/net/tun`),
  `/var/mnt` **RW** HostToContainer (disk images), `/sys` read-only if needed
  for NIC inventory. `privileged: true` like agent/terminal. Namespace
  `naslos-privileged`; network policy: ingress allow from API on :9091 only.
  (hostNetwork traffic is exempt from pod egress policy, same as the agent —
  the URL importer gets an FR-DNS-07-style SSRF guard instead: public targets
  only, `http(s)://` only.)

### Data model (DM-7 Vm — to be added to spec §5)

```json
{ "name": "dns1123", "cpu": 2, "memMiB": 2048, "autostart": false,
  "firmware": "uefi|bios", "disk": { "sizeGB": 40, "format": "qcow2|raw", "bus": "virtio|sata" },
  "net": { "mode": "nat|lan", "forwards": [{ "host": 8081, "dst": 80 }] },
  "cloudInit": { "enabled": true, "hostname": "...", "user": "...", "sshKeys": ["..."] },
  "iso": "isos/<file>" | null, "pool": "<zfs pool>", "state": "running|stopped|..." }
```

`state` is always derived live from vmd, never persisted. VM records live in
`vms.json` on the `/var/lib/naslos` PVC (same manager pattern as `apps.json`).

### API surface (owner-gated)

| Route | Notes |
| --- | --- |
| `GET /api/virt/capabilities` | `{kvm, cpuModel, nested, tun, nics[]}` from vmd; UI banner when `kvm:false` |
| `GET/POST /api/virt/vms` · `GET/PUT/DELETE /api/virt/vms/{name}` | CRUD; PUT re-renders + pushes |
| `POST /api/virt/vms/{name}/{start,stop,force-stop,restart}` | Actions |
| `GET /api/virt/vms/{name}/snapshots` · `POST .../snapshots` · `POST .../snapshots/{snap}/restore` | Reuse agent dataset-snapshot APIs scoped to `<pool>/vms/<name>`; restore refuses a running VM |
| `GET /api/virt/images` · `POST /api/virt/images/upload` (chunked stream) · `POST /api/virt/images/import` (URL + optional sha256) · `DELETE /api/virt/images/{name}` | Library under `<pool>/vms/isos` |
| `GET /api/ws/vm/{name}/vnc` (+ plain-GET preflight) | WS proxy API→vmd→QEMU native websocket VNC |

Upload is the repo's first browser file upload: XHR with progress, chunked to
the API (axum streams body → reqwest → vmd writer, mirroring the agent
`zfs send/receive` streaming pattern); document Traefik/API body-size and
timeouts found during implementation.

## 3. Task breakdown (ordered)

1. **`rust/vmd` crate skeleton** (`naslos-vmd`): axum server, token+TLS layer
   (copy `rust/agent/src/server/`), typed handlers, QMP client, QEMU argument
   builder with golden-arg unit tests, reconciler (desired vs running diff).
2. **vmd image + chart**: `vmd/Dockerfile` (Debian slim + pinned
   `qemu-system-x86`, `qemu-utils`, `ovmf`, `seabios`, `genisoimage`), add to
   `.github/image-matrix.json` (9th image), `charts/naslos/templates/virt-deployment.yaml`
   (copy agent-daemonset: hostNetwork, privileged, `/dev` + `/var/mnt`
   HostToContainer, headless svc :9091, `Recreate`), `values.yaml` `virt:` block
   (`enabled` default true, image, resources, vncPortRange, session caps),
   ingress netpol allow, tag pins in `values-vm.yaml` + `values-installer.yaml`;
   extend `scripts/check-release-version.sh` if it does not auto-discover the
   new image keys.
3. **Storage flow**: VM create/delete calls the existing agent dataset APIs
   (`<pool>/vms/<name>`, quota = sizeGB; delete = dataset destroy with the
   existing guards). ISO dataset bootstrap. Nothing new in the agent.
4. **API module** (`rust/api/src/virt/` + `server/virt.rs`): `vms.json`
   manager, validation, render+push, action routes, capabilities passthrough,
   snapshot/restore routes with stopped-guard; route table in
   `server/mod.rs` behind `require_admin`; unit + route tests.
5. **Images library**: chunked upload endpoint (browser XHR → API → vmd
   streaming), URL import with SSRF/public-target validation + optional
   sha256, delete. Test with a large ISO.
6. **cloud-init**: vmd generates NoCloud `cidata` ISO (genisoimage) from the
   form (hostname/user/sshKeys only — never passwords), attaches on first boot,
   detach vs keep-attached decided by test.
7. **Console**: QEMU `-vnc :<n>,websocket=<port>`; vmd tracks per-VM ports;
   API WS proxy route with `/api/ws/exec`-style origin check, caps, preflight
   GET; vendored noVNC (MPL-2.0) in `ui/`; console panel with clipboard +
   fullscreen.
8. **UI section**: nav `Virtual Machines 🖥️` → `/vms` (list with state badges
   + live CPU/RAM, capabilities banner), create wizard (pool picker, disk,
   cpu/mem, firmware, net mode + forwards, ISO/URL, cloud-init), detail page
   (console, actions, snapshots, forwards, autostart toggle). Svelte 5 legacy
   syntax, `.card/.btn/.input` classes, `fetch('/api/...')` like every page.
9. **Events + boot behavior**: ntfy `vm_failed`/`vm_exited_unexpected` via the
   notifications manager; vmd boot reconcile starts autostart VMs after
   datasets are visible (bounded wait — mount-propagation trap applies, see
   AI_Handoff gotcha 5); per-process usage stats via QMP balloon + /proc.
10. **Spec, docs, release**: spec §3.13 `FR-VMT-*` + DM-7 + route rows + fix
    the stale image count in FR-INSTALL-13; `docs/virtual-machines.md`;
    README feature list; `CREDITS.md` (QEMU GPLv2, genisoimage GPLv3, OVMF
    BSD, SeaBIOS LGPLv3, noVNC MPL-2.0) + `Last reviewed` bump;
    `AI_Handoff.md` after the live drill. Version bumps for 0.4.0 in
    `charts/naslos/Chart.yaml` (version + appVersion), `ui/package.json` (+
    lock), all image tags in `values.yaml` + `values-installer.yaml`;
    `values-vm.yaml` digest pins updated when deploying to the node.

## 4. Validation gates

- `cargo fmt --all -- --check`, `cargo clippy -p naslos-api -p naslos-agent
  -p naslos-vmd --all-targets -- -D warnings`, `cargo test` for all three
  crates; `cd ui && npm run check && npm run build`.
- `helm lint` with both overlay pairs; `helm template` proof for the new
  template; `make -n install-vm` dry run; `make install-pack` +
  `sh scripts/audit.sh`; `scripts/check-release-version.sh` green at 0.4.0.
- Playwright spec against the live node with the established
  capability-skip pattern: if `/api/virt/capabilities` lacks KVM, skip as a
  setup gap (not a pass). Spec covers create-from-URL-import (tiny cloud
  image), boot, console open, stop, snapshot/rollback, delete.
- Live drill on the production node: deploy via
  `KUBECONFIG=$PWD/bootstrap/vm/kubeconfig make install-vm`; verify
  `rollout status` + a VM with a NAT port forward reachable from the LAN, a
  macvtap VM getting DHCP on the LAN, dataset cleanup on delete, and a Buddy
  schedule accepting a VM dataset. Record results in `AI_Handoff.md`.
- Release: `publish=false` dry run first, then the `v0.4.0` tag; installer
  `naslos-v0.4.0` follows via dispatch (version parity MUST hold).

## 5. Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| No VT-x/AMD-V on the node's CPU (or disabled in BIOS) | Capabilities preflight; UI shows an explanatory unavailable state; vmd idles harmlessly |
| Mount-propagation trap (dataset mounted in pod ns) | Datasets are created through the agent (host ns), vmd sees them via HostToContainer; vmd boot reconcile waits bounded for paths |
| Pod restart kills VMs (qemu children) | Documented behavior; autostart reconcile; `Recreate` strategy prevents split-brain pods |
| VNC websocket port collisions | vmd allocates from a chart-configured range; port shown in VM detail |
| Windows guests without virtio drivers | Disk bus selectable (virtio default, sata option) + docs note |
| Large ISO uploads through Traefik/API | Chunked upload protocol with progress + resume per chunk; size limits documented |
| GPLv2/GPLv3 redistribution (QEMU/genisoimage) | Unmodified distro binaries in a public image; CREDITS.md provenance + source links |
| Memory overcommit across VMs | UI warning vs node RAM (metrics endpoint exists); no hard cap in v1 |
| macvtap host-isolation quirk | Helper text in the UI + docs |

## 6. Open implementation choices (non-blocking)

- Alpine vs Debian slim base for the vmd image — pick by qemu packaging
  maturity + final image size during task 2.
- Exact QEMU version pin + how firmware blobs are shipped (distro package).
- cloud-init seed lifetime after first boot (detach vs keep) — decide by test.