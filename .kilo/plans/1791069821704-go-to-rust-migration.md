# Naslos Go → Rust migration + RAM reclamation for custom apps

## Goal

Reduce the RAM/CPU the Naslos platform itself consumes so user-installed apps
(`naslos-apps`) get the largest possible share of the node. Two workstreams:

1. **Reclaim RAM with measured, high-yield levers first** (Phase 0).
2. **Refactor the Go components to Rust** — `agent/` first, then `api/` only if
   the measurement justifies it — while keeping every external contract
   identical.

The stated goal is *free memory for custom apps*, not "use Rust" for its own
sake. Phase 0 exists to prove where the memory actually goes and to capture the
cheapest wins before spending effort on a rewrite that may yield little.

## Non-goals

- No UI (`ui/`) rewrite; Svelte/TS stays.
- No rewrite of Samba, NFS, OpenLDAP, Traefik, Authelia images.
- `api/cmd/buddyctl` and `api/cmd/buddy-receiver` are **out of scope initially**
  (standalone client / off-node container, zero idle RAM on the appliance).
  Revisit only after the API port lands.
- No change to the HTTP wire contract, auth model, or on-disk state formats.

## Locked decisions (from planning)

1. **Sequencing:** measure first → port `agent/` → port `api/` **only if** the
   Phase 0/1 gate passes.
2. **Phase 0 includes the non-Rust RAM levers**, explicitly including
   **removal of the Prometheus subchart**, ZFS ARC capping, and resource
   right-sizing. The port is judged against the residual need.
3. **Helm and Talos in the Rust API:** bundle the official static `helm` and
   `talosctl` binaries and shell out. Both are admin-time control-plane
   operations (chart install already waits 5 min; metrics/disks are periodic
   reads), never hot paths. No reimplementation of Helm release storage or
   Talos COSI/gRPC.

## Findings that shape the plan

- `agent/` is ~8 source files and **zero external Go dependencies**
  (`agent/go.mod` is 3 lines). It shells to `chroot /host zpool|zfs|wipefs` and
  serves HTTP+JSON. Rust port is near-mechanical.
- `api/` is 20 internal packages and 100+ files with heavyweight deps:
  `helm.sh/helm/v3`, `k8s.io/client-go`, `siderolabs/talos/pkg/machinery`
  (generated protobuf + COSI), `go-git`, `go-ldap`, `gorilla/websocket`,
  `x/crypto` (see `api/go.mod`).
- **Talos usage is narrow:** read-only system metrics (`api/internal/talos/client.go`),
  discovered disks + `UserVolumeConfig` builder (`api/internal/talos/volumes.go`).
  `GetDiscoveredVolumes` is used by `server/zfs.go`, `server/disks.go`,
  `server/health_notify.go`; metrics by `server/metrics_collector.go`.
- **Helm usage is a thin wrapper** (`api/internal/helm/operations.go`):
  `InstallDir`/`UpgradeDir`/`Uninstall`/`List`/`Get`/`Rollback`, namespace-scoped,
  `Wait=true`, `UpgradeDir` uses `ResetValues=true`.
- **Prometheus is a subchart only**: `charts/naslos/Chart.yaml` dependency
  `prometheus 25.0.0` (`condition: prometheus.enabled`), values block in
  `charts/naslos/values.yaml` (512Mi request, 30d retention), schema entry in
  `charts/naslos/values.schema.json`. **No UI or API code depends on it** (the
  dashboard reads `/api/metrics`, which is the API's own Go aggregator). Removal
  is self-contained.
- **`zfs.arcMax` is documented but unwired**: it appears only in
  `charts/naslos/values.yaml` and `docs/deployment.md`; no template consumes it.
  The ZFS module is loaded by the machine config
  (`bootstrap/vm/naslos-vm.yaml` → `machine.kernel.modules[name=zfs]`).
- Live chart requests: api 128Mi, agent 64Mi, ui 64Mi, samba/nfs/terminal 64Mi,
  OpenLDAP 256Mi→1Gi, Prometheus 512Mi. A Rust port plausibly saves tens of MiB;
  Prometheus + ZFS ARC are worth hundreds of MiB–GiB. This is why Phase 0 comes
  first.

## Cross-cutting invariants (all phases)

- **Wire contract frozen:** same paths, methods, status codes, JSON field names
  and error shape (`{"error": "..."}`), same env var names, same ports
  (8080 API, 9090 agent), same TLS/bearer/proxy-secret auth. The Playwright
  suite and the Go integration tests are the conformance oracle.
- **State formats frozen:** `shares.json`, `smbusers.json`, `notifications.json`,
  `apps.json`, `sources.json`, `domains.json`, `ddns.json`, `buddy-peers.json`,
  `buddy-schedules.json`, `buddy-identity.json`, the nonce cache, and the chart
  cache layout must round-trip byte-compatibly with the Go versions.
- **Spec rule** (`AGENTS.md`): any MUST in `docs/spec.md` affected by the port is
  updated with its test in the same change.
- **Credits rule:** every new Rust dependency and the bundled `helm`/`talosctl`
  binaries are recorded in `CREDITS.md` (with `Last reviewed`) in the same change.
- **Docs rule:** `AI_Handoff.md` (tags/revision), `docs/architecture.md`,
  `docs/api.md`, `docs/deployment.md`, `docs/operations.md`,
  `docs/installer-contract.md` updated in the same change.
- **Fresh-install rule:** every change lands in `charts/naslos/`,
  `bootstrap/**`, `scripts/deploy-vm.sh` or the `Makefile`, never only live.
- **Never push to `master`.** One branch per phase, PR against `master`,
  Conventional Commits.
- **Cross-repo:** changing the image set/names is an interface change to
  `docs/installer-contract.md`; the `Naslos-Installer` repo must be updated in
  the same release. Confirm before finalising any release tag.

## Branches

Branch creation is an implementation step (the planning agent cannot run git).
Suggested: Phase 0 `chore/ram-baseline-prometheus-removal`, Phase 1
`refactor/rust-agent`, Phase 2 `refactor/rust-api`. Each is a separate PR; do not
mix the port with the memory reclamation.

---

## Phase 0 — Baseline + reclaim RAM (no Rust)

**Exit gate:** a measured baseline exists and the cheap wins are deployed. A
documented decision (go/no-go) for Phase 2 is recorded.

1. **Establish the measurement harness** (commit a small script under
   `scripts/`, e.g. `scripts/memory-baseline.sh`):
   - Per-pod working set from the kubelet summary API
     (`kubectl get --raw /api/v1/nodes/<node>/proxy/stats/summary`) and
     `kubectl top pod -A --containers`. This is dependency-free, so it survives
     Prometheus removal.
   - Node total/available memory from the same summary API.
   - ZFS ARC: `talosctl read /proc/spl/kstat/zfs/arcstats` (or from the terminal
     pod: `chroot /host cat /proc/spl/kstat/zfs/arcstats`) — capture `c_max`,
     `c` (size), `size`.
   - Sample 3× over a few minutes at idle and once under a share/backup load.
2. **Record node RAM and set `TARGET_FREE`** (required input): how much RAM must
   remain available for custom apps under load. Write it into the plan/PR
   description; it is the Phase 2 gate constant.
3. **Remove Prometheus:**
   - Delete the `prometheus` dependency from `charts/naslos/Chart.yaml` and
     refresh `Chart.lock` (`helm dependency update`).
   - Delete the `prometheus:` block from `charts/naslos/values.yaml` and the
     schema entry in `charts/naslos/values.schema.json`.
   - Keep `/api/metrics` and the dashboard untouched (independent).
   - Update `AI_Handoff.md`, `docs/architecture.md`, `docs/operations.md`,
     `docs/deployment.md`, `CREDITS.md` (dependency change).
   - Verify: `helm lint` both overlays, `helm template`, `make -n install-vm`, a
     live `helm upgrade` that removes the release objects (delete the
     `naslos-prometheus-*` Deployment/STS/ServiceAccount and its PVC explicitly
     if Helm leaves them), then confirm the dashboard `/api/metrics` still 200.
4. **Cap ZFS ARC** (wire the currently-dead `zfs.arcMax`):
   - Apply `zfs_arc_max` at boot via the machine config
     (`bootstrap/vm/naslos-vm.yaml` and the installer template
     `bootstrap/installer/naslos-installer.yaml.tmpl`):
     `machine.kernel.modules: [{name: zfs, parameters: ["zfs_arc_max=<bytes>"]}]`.
   - Either implement `zfs.arcMax` as the rendered source of that parameter, or
     remove the misleading value and document the machine-config knob. Prefer
     implementing it in the installer template (fresh-install rule) and setting
     the VM value in `bootstrap/vm/naslos-vm.yaml`.
   - Target: leave `TARGET_FREE` plus headroom for the control plane; e.g.
     `arcMax = total_ram - reserve`. Confirm `c_max` changed after reboot.
5. **Right-size the remaining workloads** (measure, then set request = observed
   working set + ~25%, keep limits for bursts):
   - OpenLDAP: lower `resources.requests.memory`; **keep the 1Gi limit** (the
     comment in `values.yaml` records first-boot RSA-4096 + slapadd OOM at
     512Mi), or add a startup-only higher limit only if the platform supports it.
   - API/agent/UI/samba/nfs/terminal: adjust after measurement; do not over-tighten
     (OOMKill risks backups).
6. **Record the go/no-go gate** in `AI_Handoff.md`: after Phase 0, is
   `available ≥ TARGET_FREE` under load? If yes, Phase 2 is optional; if no,
   continue to Phase 1 and re-evaluate.

Early non-Rust lever if still short: the API's `metrics_collector` and health
notifier poll intervals, and whether `prometheus` should be replaced by a tiny
scrape-free exporter. Do not silently drop operator observability — document the
replacement.

---

## Phase 1 — Port `agent/` to Rust

**Scope:** the entire agent (`agent/internal/{zfs,shares,server}`, `agent/cmd`).

1. **Workspace:** create `rust/` Cargo workspace (members `agent` later `api`,
   `buddyctl`, `buddy-receiver`). Static musl builds
   (`x86_64-unknown-linux-musl`). Recommended stack: `tokio`, `axum`, `rustls`,
   `serde`/`serde_json`, `clap`, `tracing`.
2. **Endpoints to reproduce exactly** (from `agent/internal/server/server.go`):
   - Public: `GET /health`.
   - Bearer-authenticated (constant-time compare; body caps 1 MiB JSON, 1 TiB on
     `/api/v1/zfs/receive/`): `GET|POST /api/v1/pools`,
     `GET|POST /api/v1/pools/import`, `GET|DELETE /api/v1/pools/{pool}`,
     `GET|POST /api/v1/pools/{pool}/devices`, `GET /api/v1/datasets`,
     `GET|POST|DELETE /api/v1/datasets/{pool}[/{name…}]` (recursive query param),
     `GET|POST /api/v1/snapshots/{dataset}`, `GET|PUT /api/v1/shares/config`,
     `GET /api/v1/shares/status`, `GET|POST /api/v1/shares/folders`,
     `…/zfs/send/{…}`, `…/zfs/receive/{…}`, `…/zfs/snapshots/{…}`.
   - Error mapping: validation errors → 400, backend failures → 500, degraded
     (no ZFS / no `/host`) → 503. Preserve `writeClientError` semantics.
3. **Command layer:** port `zfs/{operations,pool,dataset,devices,backup,validation,utils}.go`
   and `shares/{shares,folders}.go`. Keep the same `chroot /host <bin> …`
   invocations and argument construction so behavior is identical. Streaming
   `zfs send`/`receive` must stay pipes (needed for self-backup), not captured output.
4. **TLS/auth:** serve HTTPS when both `AGENT_TLS_CERT`/`AGENT_TLS_KEY` set;
   refuse to start on a half-configured pair; `AGENT_TOKEN` required.
5. **Image:** keep **Alpine** (busybox `chroot`), run as root, `EXPOSE 9090`
   (`agent/Dockerfile` comment explains why not distroless).
6. **Tests:**
   - Port the Go unit tests (`zfs/*_test.go`, `shares/*_test.go`,
     `server/*_test.go`), including validation edge cases.
   - **Golden wire tests:** generate request/response fixtures from the Go agent
     (kept temporarily) and assert the Rust agent returns byte-identical JSON
     and status codes for the same inputs.
   - Live drill on the VM: create/destroy pool + dataset, snapshot, add vdev,
     share config apply, a buddy `zfs send`/`receive` round-trip.
7. **Deploy:** bump `agent.image` tag in `charts/naslos/values-vm.yaml`;
   `helm lint`/`template`, `make -n install-vm`, `helm upgrade`, verify
   `rollout status daemonset/naslos-agent`. Keep the Go image tag for one-step
   rollback.
8. **Gate to Phase 2:** measure the Rust agent working set vs the Go agent
   (same host, same load) and record the delta. Extrapolate a conservative API
   saving; require it to close a meaningful fraction (≥50%) of the Phase 0 gap
   to `TARGET_FREE`. If not, stop here.

---

## Phase 2 — Port `api/` to Rust (conditional)

**Precondition:** Phase 1 gate passed. Do a **spike first** (timeboxed): a
minimal Rust API serving only `/api/health`, `/api/ready`, and `/api/dashboard`
+ `/api/metrics` via `talosctl`, measured for RSS. Only if the spike confirms the
saving go to the full port.

### Module mapping

| Go (api/internal) | Rust approach |
| --- | --- |
| `server` (routes/handlers, 756-line `server.go`) | `axum` router; same paths/methods/status/JSON |
| `auth` (proxy secret + CIDR + identity headers, admin gate) | `tower` middleware; same env (`PROXY_SHARED_SECRET`, `TRAEFIK_CIDR`) |
| `identity` (LDAP users/groups, NT hash, samba passdb) | `ldap3`; keep NT-hash algorithm identical |
| `shares` (manager, smb/nfs render, samba users) | direct port; `serde` field names match Go json tags exactly |
| `agent` client (token transport, TLS pinning) | `reqwest` + `rustls`; dedicated streaming client with the same auth |
| `buddy` (Ed25519/X25519, sealed chunks, nonces, store, peers) | RustCrypto (`ed25519-dalek`, `x25519-dalek`, `chacha20poly1305`, `hkdf`, `sha2`) |
| `catalog` / `chartsrepo` (git clone/pull, TTL cache) | `gix` (pure Rust) or `git2`; same cache dir/layout |
| `apps` (install records, lifecycle, orphan backfill) | port; Helm via the bundled `helm` CLI |
| `helm` | bundled `helm` binary, `-o json`, same `Wait`/5m/`ResetValues` semantics |
| `routing` / `certs` (Traefik IngressRoutes, cert-manager CRs) | `kube-rs` |
| `authelia` (ConfigMap fragments + delete pod `naslos-authelia-0`) | `kube-rs` |
| `ddns` / `providers` (persisted registry, drivers, IP detection) | `serde_yaml` + `reqwest` + a DNS resolver crate |
| `notifications` (ntfy) | `reqwest` |
| `metrics` (dashboard aggregation) | port |
| `talos` | bundled `talosctl -o json` for version/read/memory/mounts/disks; parse defensively |
| `config`, `logsafe` | port; keep log redaction rules |

### Tasks

1. Create `rust/api` crate; port package-by-package behind contract tests, keeping
   the Go API running as the reference until parity.
2. **Helm CLI adapter:** pin the `helm` version, call `helm install|upgrade|uninstall|list|get|rollback`
   against the cloned chart dir, parse `-o json`. Fail-closed on non-zero exit;
   preserve the current human-readable status mapping (`ReleaseStatus`).
3. **Talos CLI adapter:** bundle `talosctl`; keep `TALOSCONFIG` resolution
   semantics (env first, then in-cluster mount). Golden-test its JSON against
   the Go client's output for the dashboard shape.
4. **State compatibility:** copy real state files into test fixtures and assert
   read→write round-trips are byte-identical (or field-identical where Go
   re-marshals). Buddy interop: Rust must decrypt Go-sealed chunks and vice
   versa using existing fixtures.
5. **Image:** distroless static for the API (`api/Dockerfile`), `HELM`/`TALOSCTL`
   binaries COPYed in and checksum-pinned. Run non-root as today.
6. **Deploy + rollback:** bump `api.image` tag; keep the Go tag for rollback.
7. **Retire Go:** only after Playwright is green against the Rust API, remove
   `api/` (or archive it), drop `go.mod`/CI Go steps for the API, and update
   `AGENTS.md`/README/CREDITS/spec.

### Optional Phase 3

Port `buddyctl` / `buddy-receiver` to Rust, then make the API image simpler.
Not required for the RAM goal.

---

## Validation matrix

| Change | Required evidence |
| --- | --- |
| Prometheus removal | `helm lint`/`template`, `make -n install-vm`, live upgrade, `/api/metrics` + dashboard 200 |
| ZFS ARC cap | `c_max` in `arcstats` after reboot; `c` bounded under load |
| Resource right-sizing | No OOMKills over a drill; working set within request |
| Agent port | Go↔Rust golden wire tests, ported unit tests, live ZFS/share/backup drill, `rollout status ds/naslos-agent` |
| API port | Go↔Rust contract tests, Playwright suite green, live drill (shares, LDAP, ZFS, buddy send/restore, apps install, ddns, domains), dashboard `/api/metrics` |
| Buddy interop | Rust decodes Go chunks and vice versa from existing fixtures |

## Risks & mitigations

- **Rust saving smaller than hoped** → Phase 0 gate + Phase 2 spike; do not start
  the full API port without a measured spike.
- **`helm`/`talosctl` output drift** → pin versions, use `-o json`, contract tests,
  and fail closed.
- **State/сrypto incompatibility** → golden fixtures and cross-implementation
  buddy tests before switching traffic.
- **Auth/RBAC drift** → Playwright + Go black-box tests as the oracle.
- **Two-language maintenance during transition** → port per endpoint behind
  contract tests; delete Go only after parity.
- **Image growth from bundled Go CLIs** → disk/image size only (no idle RSS);
  pin checksums, record in CREDITS.
- **`helm upgrade` leaves removed Prometheus objects** → explicitly delete the
  release's Deployment/SA/PVC if Helm does not.

## Open questions / required inputs

1. **`TARGET_FREE`**: node total RAM and the minimum RAM to leave available for
   custom apps under load. Required to make the Phase 2 gate objective.
2. Confirm `zfs.arcMax` should become a real rendered machine-config knob (vs.
   removing the value and documenting the machine-config parameter).
3. Confirm `buddyctl` / `buddy-receiver` stay out of scope.
4. Confirm the bundled `helm` + `talosctl` binaries are acceptable in the API
   image (supply-chain policy).

## Out of scope

- UI, Samba/NFS/OpenLDAP/Traefik/Authelia images, OS/Talos changes beyond the
  ZFS ARC module parameter, and any release/version bump (a release needs the
  `Chart.yaml` + `ui/package.json` version bump and installer dispatch per
  `AGENTS.md`, tracked separately).

---

## Execution log

### Phase 0 (2026-10-04) — branch `chore/ram-baseline-prometheus-removal`

- **Required input.** Node MemTotal = 5.76 GiB (6,041,596 kB). `TARGET_FREE`
  set to **>= 2.5 GiB available under load**, per the user's "as much as
  possible for apps without compromising ZFS/OS".
- **Measurement harness.** `scripts/memory-baseline.sh` (kubelet summary API +
  `talosctl` arcstats; no Prometheus, no metrics-server).
- **Baseline (idle).** Node available 3.4 GiB; pod working set 1.81 GiB
  (Prometheus stack ~265 MiB, OpenLDAP 673 MiB, kube-apiserver 391 MiB,
  cilium 213 MiB). ZFS ARC `c_max` default 4.76 GiB, `c` ~184 MiB.
- **Prometheus removed** (Chart.yaml dep + Chart.lock + vendored tgz, values
  block, schema entry, docs, CREDITS). Live `helm upgrade` removed the stack
  (working set 1.81 -> 1.59 GiB); the orphaned alertmanager PVC was deleted.
  `/api/health`, `/api/metrics`, `/api/dashboard` all 200.
- **ARC cap (2 GiB) BLOCKED.** No machine-config route works on this
  Talos v1.14.1 + `siderolabs/zfs` extension + SDBoot node:
  `machine.kernel.modules[].parameters` is ignored (extension loads the module),
  `machine.install.extraKernelArgs` is unsupported on SDBoot, and a
  `machine.files` `/etc/modprobe.d` drop-in was ignored (and coincided with a
  CRI boot failure). The runtime sysfs write works. Deferred by user decision;
  the planned fix is an agent DaemonSet initContainer writing
  `/host/sys/module/zfs/parameters/zfs_arc_max` at boot. Documented in
  `docs/deployment.md` -> "ZFS ARC".
- **Right-sizing.** OpenLDAP request raised 256Mi -> 768Mi (measured idle
  673 MiB; the old request was below real usage). Other workloads unchanged:
  requests are scheduling hints, not memory reclamation, and lowering them only
  worsens eviction on a single node.
- **Go/no-go gate.** With ARC uncapped, `c_max` (~4.76 GiB) will reclaim down
  under pressure but can squeeze available below TARGET_FREE under a big
  share/backup load. So the gate is **not met** until the ARC cap is restored
  (initContainer follow-up); Phase 1/2 remain justified. The Prometheus removal
  is the only deployed RAM win so far.


### Phase 1 (2026-10-04) — branch `refactor/rust-agent`

Port of `agent/` (Go, 4554 LOC, zero external deps) to a Rust workspace at
`rust/` (`naslos-agent` crate). Same HTTP contract, same route set, same
validation, same degraded-mode 503s.

- **Layout.** `rust/Cargo.toml` (workspace, release: `opt-level="z"`, LTO,
  `panic="abort"`, strip) + `rust/agent/`. Modules mirror the Go packages:
  `zfs/{types,validation,runner,utils,devices,dataset,operations,backup}`,
  `shares/{mod,config,folders}`, `server/{mod,auth,error,handlers}`.
- **Runner seam.** A `Runner` trait replaces Go's `var runHost`; `RealRunner`
  spawns `chroot /host <bin>` with piped stdio for the streaming send/receive
  and a combined-output capture for buffered calls. Tests inject a
  `FakeRunner` that records argv and replays canned responses — the destructive
  argument assertions port one-for-one.
- **HTTP parity.** `axum` + `axum-server` (rustls). The API router is mounted
  behind `middleware::from_fn_with_state(require_auth)`, so a new route is
  authenticated by default (the Go longest-prefix default-deny property).
  `subtle::ConstantTimeEq` for the bearer token. JSON responses append the
  trailing newline Go's `json.Encoder` wrote. All struct JSON tags are copied
  exactly (lowercase keys, `usedBytes`, `smbShareCount`, `ioStats`, …).
- **Body caps.** 1 MiB JSON everywhere; the receive handler counts streamed
  bytes and stops at 1 TiB (the Go `MaxBytesReader` equivalent). `WriteTimeout`
  is not set (axum has none by default), so long `zfs send` streams survive.
- **Deploy wiring.** `agent/Dockerfile.rust` (multi-stage rust:alpine → musl
  static → alpine:3.24 for busybox `chroot`), `.github/image-matrix.json`
  repointed, `make agent-image` uses it, `make agent` builds via cargo,
  `scripts/audit.sh` runs `cargo fmt/clippy/test` for the agent, CI installs
  the Rust toolchain + rust-cache. The Go `agent/` tree is deleted.
- **Tests.** 43 Rust tests ported from the Go suite: zfs validation (every
  refusal asserts no command ran), the AV-8 stale-mount destroy ladder, dataset
  option sorting, `humanBytes` boundaries, folder confinement incl. the PF-L5
  symlink escape, secret-file modes (CR-15), backup command lines, plus HTTP
  contract tests (public `/health` with trailing newline, 401 on the API,
  a new route 401s, exact degraded-mode 503 messages, 400-vs-500 classification,
  lowercase pool JSON).
- **Phase 1 gate (measured).** Degraded mode, same host: RSS idle 7.1 MB (Go)
  → 5.3 MB (Rust); RSS after 500 requests 10.2 MB → 5.7 MB; server CPU per 2000
  requests 390 ms → 260 ms; static binary 7.7 MB → 3.9 MB. The savings are real
  but small in absolute terms (~4 MB/pod); the decisive RAM is the ARC cap and
  the API port, not the agent. Proceeding past Phase 1 is justified mainly by the
  API port and by the lower CPU/FD footprint.
- **Not done (release-time):** image tag bump in `values-vm.yaml`, version bump,
  installer dispatch, and the live DaemonSet rollout drill. `make install-vm`
  will pick up the Rust image once a fresh tag is built and pushed.
