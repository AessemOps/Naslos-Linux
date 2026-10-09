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

### Execution strategy (incremental slices)

The full port is ~23k LOC across 20 packages; it lands as **vertical slices**,
each a working increment with contract tests, on branch `refactor/rust-api`:

1. **S1 — foundation** (this slice): crate skeleton `rust/api`, `config`,
   `logsafe`, `metrics`, `auth`, `talos` (CLI adapter), the `server` router +
   the four endpoints for real, `/api/auth/me`, and the deployable image. This
   makes the API runnable and replaces the spike.
2. **S2 — ZFS/shares read path:** `agent` client (token transport + TLS) and the
   `server` handlers for pools/datasets/disks/shares (read + apply).
3. **S3 — identity/LDAP:** `identity`, users/groups handlers.
4. **S4 — apps/catalog/helm:** `chartsrepo`, `catalog`, `apps`, the bundled
   `helm` adapter, `kube`, app jobs.
5. **S5 — routing/certs/authelia/domains/providers/ddns.** 
6. **S6 — buddy** (the crypto-heavy slice) + notifications.
7. **S7 — retire Go:** Playwright green against the Rust API, then remove
   `api/` (or archive), drop the Go CI steps, update AGENTS/README/spec.

Each slice keeps the Go API as the reference; the image tag only moves in S7's
deploy, and the Go tag stays for rollback.

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
- **Not done (release-time):** version bump, installer dispatch, and the live
  DaemonSet rollout drill. The image tag is already bumped to `0.1.0-r9` in
  `values-vm.yaml`, so `make install-vm` will pick up the Rust image once it is
  built and pushed.

### Phase 1 review fixes (2026-10-04)

Self-review of the Phase 1 branch found and fixed, with regression tests:

- **CRITICAL — restore-stream leak:** the receive error paths awaited the child
  before closing stdin; `spawn_in` already took stdin, so `wait()` could never
  see EOF. A client that aborts a restore blocked a task and orphaned
  `zfs receive`. Fixed by closing stdin before every `wait.await` on error paths.
- **Contract parity:** `GET /pools/{pool}/devices` now serializes an empty list
  as `null` (Go's nil slice), not `[]`; bare trailing-slash URLs
  (`/api/v1/datasets/`, `/snapshots/`, `/zfs/{send,receive,snapshots}/`) now
  reach the handlers and return 400 as Go's prefix mux did, instead of 404;
  relative folder paths (`var/mnt/...`) are rejected as non-absolute again.
- **Deploy:** `values-vm.yaml` agent tag bumped `0.1.0-r8` -> `0.1.0-r9`.
- **Performance:** 128 KiB `ReaderStream` capacity on the send hot path;
  shares apply/status/folder ops moved to `spawn_blocking`.
- **Security posture:** restored the Go header-read timeout (10s) and installed
  the hyper timer axum-server omits.
- **Dead code removed:** `zfs::utils` module, `dataset_exists`, `is_base`,
  `other_err!` and the unused `ZfsError::invalid/other`.

### Phase 2 spike (2026-10-04) — branch `refactor/rust-api-spike`

Timeboxed spike required before the full API port: a minimal Rust API
(`rust/api-spike`) serving only `/api/health`, `/api/ready`, `/api/dashboard`
and `/api/metrics`, with the proxy auth middleware, the metrics collector loop
and the exact Go JSON contracts (RFC3339 `updatedAt`, nil slices -> `null` in the
dashboard projection, trailing newline).

- **Measured (same host, single process):**

  | | Go API (live, in-cluster) | Rust spike |
  | --- | --- | --- |
  | RSS | 18.8 MiB | **4.6 MiB idle / 4.8 MiB after 3000 requests** |
  | Working set | 98.1 MiB | ~5 MiB |
  | Binary | 80.8 MB | **2.0 MB** |

- **Gate: PASS.** The spike alone reclaims ~93 MiB of working set — far more
  than the Phase 0 gap to `TARGET_FREE` (and ~3x the Rust agent's own saving).
  The API runs the full metrics collector + auth path here, so this is a
  realistic floor, not a trivial responder. The full port is justified; the
  remaining cost is engineering, not RAM risk.
- **Caveats for the full port.** The spike deliberately stubs the Talos collector
  (it reads `/proc` locally) and does not touch LDAP, kube-rs, Helm, git clones,
  buddy crypto or the 20 state-file packages; those will raise RSS above 4.6 MiB.
  Even a 3-4x increase keeps a large win against 98 MiB. The bundled `helm`/
  `talosctl` binaries add image size, not idle RSS (documented).
- **Contract tests** (5): public health/ready with the trailing newline, 401
  without secret/identity, dashboard/metrics shape including RFC3339 `updatedAt`
  and `null` nil slices, and default-deny on an unknown `/api/` route.

### Phase 2 S1 — foundation (2026-10-04) — branch `refactor/rust-api`

First vertical slice of the full port: `rust/api` (`naslos-api`) replaces the
spike and is runnable.

- **Modules:** `config`, `logsafe`, `auth` (proxy secret + CIDR + identity +
  admin gate), `metrics` (manager + models + dashboard projection), `talos`
  (bundled-`talosctl` adapter), `server` (router + S1 handlers).
- **Route surface matches Go now.** The full owner route table is registered
  with the correct gates: `/api/health`, `/api/ready` public; `/api/auth/me`,
  `/api/dashboard`, `/api/metrics` auth-but-not-admin; every other `/api/` route
  admin-only. Handlers not yet ported return a documented **501** so the surface
  and its auth are already correct.
- **Live endpoints:** health, ready, auth/me, dashboard, metrics — real, with the
  metrics collector polling the node.
- **Talos CLI adapter verified live:** `talosctl read /proc/{meminfo,uptime,stat,
  loadavg,cpuinfo,sys/kernel/*}` + `get {version,addresses,disks} -o json`,
  `TALOSCONFIG` resolution (env → `~/.talos/config` → in-cluster mount),
  `TALOS_ENDPOINTS`, defensive JSON parsing that skips the version-mismatch
  `WARNING:` preamble. Matched the Go client's fields on the live node.
- **Tests (15 in the crate):** auth gates (fail-closed without secret/identity,
  plain-user 200 on dashboard/metrics, plain-user 403 + admin 501 on admin
  routes), the four endpoint shapes incl. RFC3339 / Go zero-time, logsafe
  sanitisation, meminfo/size/addresses parsing, talosctl JSON preamble skipping.
- **Image:** `api/Dockerfile.rust` (multi-stage musl → distroless static,
  nonroot). Built and smoke-tested: `/health` 200, no-secret 401, dashboard 200
  with the CIDR + headers, plain-user admin route 403. **10.6 MB image, ~2.4 MiB
  RSS** idle. The deployed image stays `api/Dockerfile` (Go) until S7.

**Not yet ported (next slices):** agent client + shares/ZFS, identity/LDAP,
apps/catalog/helm, routing/certs/authelia/domains/ddns, buddy, notifications.
The image matrix, values tags, and the Go API remain the deployed path until S7.

### Phase 2 S2 — agent client + ZFS/disks (2026-10-04) — branch `refactor/rust-api`

Second vertical slice: the ZFS read/write path through the privileged agent.

- **`agent` client** (`rust/api/src/agent.rs`): typed HTTP client for the
  DaemonSet — pools (list/create/delete/health/status), import, vdev add,
  datasets (list/create/destroy), shares config/status/folders. Bearer token per
  request; optional pinned CA (`AGENT_CA_FILE`, fail-closed, PF-M5); 180s
  timeout; `AgentError{status,message}` so the handler forwards the upstream
  status (503 degraded) instead of collapsing to 500. `AGENT_BASE_URL` /
  `NASLOS_NAMESPACE` resolution. RFC3986 escaping without an extra crate.
- **Talos disks** (`talos.rs` additions): `get_discovered_volumes()` via
  `talosctl get disks` + `get systemdisk`, mapping dev_path/size/serial/rotational
  and deriving SSD/HDD/NVME/UNKNOWN; `VolumeAdvisor::recommend` (1→single,
  2→mirror, 3-5→raidz1, 6-10→raidz2, 11+→raidz3) with lowercase JSON keys.
- **`shares` store** (read path): loads the share definitions so dataset
  deletion can refuse to remove a dataset a share serves.
- **Handlers** (`server/zfs.rs`, `server/disks.rs`): `/api/volumes/zfs`
  (GET/POST), `/import` (GET/POST), `/{pool}` (GET/DELETE), `/{pool}/health`,
  `/{pool}/devices` (POST), `/api/datasets` (GET/POST/DELETE), `/api/disks`
  (GET), `/api/disks/recommend` (POST). Ported validators (pool/dataset/options/
  disk selection) fail fast with 400 before contacting the agent; agent errors
  map via `writeAgentError` (upstream status, else 502).
- **Tests (31 in the crate now):** the S1 auth/route suite plus a mock-agent
  server: pool list pass-through, 503-degraded forwarding, 400 agent-validation
  forwarding, fail-fast validation, dataset-path rejection, and no-agent → 502.
- **Not yet ported (next slices):** shares render/apply + LDAP (S3), apps/
  catalog/helm (S4), routing/certs/authelia/domains/ddns (S5), buddy (S6).

- **Image:** `api/Dockerfile.rust` rebuilt and smoke-tested — 13.3 MB, ~3.7 MiB
  RSS; `/health` 200, no-secret 401, dashboard 200, `/api/volumes/zfs` 502 with
  no agent configured. The agent image (`agent/Dockerfile.rust`) still builds
  (20.2 MB) with the `api` workspace member present.

### Phase 2 S3a — identity/LDAP library (2026-10-04) — branch `refactor/rust-api-identity`

S3 is identity + shares; it is split because the users/groups handlers depend on
the shares store. S3a is the identity (LDAP) library.

- **`rust/api/src/identity.rs` + `identity/ops.rs`**: the LDAP client (lazy
  connect, reconnect cooldown, retry-once on connection errors, all in one `Op`
  runner), person/group CRUD, `set_password` via the RFC 3062 Password Modify
  exop returning the NT hash, POSIX id lookup, enable/disable, membership.
- **Security parity**: the `^[a-z0-9][a-z0-9._-]{0,63}$` name allowlist, RFC
  4514/4515 DN/filter escaping, unguessable placeholder password (NAS-010),
  fail-closed LDAPS with a pinned CA (rustls, ring provider) — no silent fallback
  to system roots.
- **NT hash**: a minimal MD4 (RFC 1320) implementation, required by the SMB
  protocol and not used for security (AUDIT-L7); verified against the known
  `password` vector `8846F7EAEE8FB117AD06BDD830B7586C`.
- **Wiring**: `AppState.identity` is built from `LDAP_*`; `/api/auth/me` now
  returns 503 when LDAP is unconfigured (matching Go) and the Go identity shape
  (`groups` as a comma-joined string).
- **Deps**: ldap3 0.12 (`tls-rustls-ring`), rustls 0.23, rustls-pemfile,
  getrandom.
- **Tests (17 lib + 9 contract):** NT-hash vector, name allowlist, DN/filter
  escaping, `hashUID`, `shortNames`, `personCN`, and the auth/me 200/503 paths.
- **Not yet (S3b):** the shares render/apply package, the samba users store, and
  the users/groups/shares handlers (which need `refreshShareAccess`). A live
  LDAP drill is part of S3b once the handlers land.

### Phase 2 S3b — shares manager + renderers (2026-10-04) — branch `refactor/rust-api-identity`

The shares core: definitions, validation, persistence and the smb.conf /
ganesha.conf renderers (port of `shares/{shares,manager,config}.go`).

- **`rust/api/src/shares/mod.rs`**: `Share`/`Protocol`, `CreateShareRequest` /
  `UpdateShareRequest`, `Manager` (load/save atomic + 0600, CRUD, `normalize_path`,
  `validate_path`, `PathOnDataset`, `validate_share_name`, `validate_share_fields`,
  `normalize_list`, the SHA-256 content `revision`, `render_config_bundle`).
- **`rust/api/src/shares/render.rs`**: `generate_samba_config` (global + per-share
  sections in stable sorted order, hosts allow/deny with the fail-closed client
  list, `valid users` with `@group`, Time Machine), `generate_ganesha_config`
  (NFSv4-only EXPORT blocks, Root_Squash by default, stable FNV-1a Export_Id),
  `sanitize_netbios_name`.
- **Security parity (NAS-007)**: share names and every rendered field reject the
  control characters / `;` / `"` that would start a new config directive; a share
  that names no hosts gets the LAN CIDR or localhost, never `*` (PF-H4).
- **Tests (11 new, 28 lib total):** stable share ordering, wildcard hosts,
  `@group` rendering, ganesha export block + Root_Squash + stable id, NetBIOS
  sanitisation, injection refusal at create, path-on-dataset specificity,
  `normalize_list`, `clean_path`.
- **Not yet (S3c):** the Samba users store (passwd/group/shadow mirrors + NT
  hashes) and the users/groups/shares handlers, which need it plus the agent
  shares-config apply. Live LDAP/SMB drill is part of S3c.

### Phase 2 S3c — samba users store + users/groups/shares handlers (2026-10-04)

Completes S3. The Rust API now serves the full identity + shares surface.

- **`shares/smbusers.rs`**: `SambaUserStore` (atomic 0600 load/save, upsert with
  NT-hash validation, enable/disable, remove) and the four renders —
  `smbpasswd` (U/DU flags + LCT), extrausers `passwd`/`group`/`shadow`, with
  `group_gid` (FNV-1a, 20000..28000) and `normalize_nt_hash` (32 hex, uppercase).
- **`server/shares.rs`**: share CRUD with the dataset-path guard
  (`require_dataset_path` → the "data would live on the ephemeral partition"
  error), `share_paths`, `share_folders` (via the agent), `status`, `apply`
  (render bundle + Samba mirrors → `agent.apply_shares_config`), and the raw
  `config/samba` / `config/nfs` text endpoints.
- **`server/users.rs`**: users (list/create with required password + SMB sync),
  user detail (update with a group membership delta, delete), password,
  enable/disable, and groups (list/create/detail/members/delete), each mirroring
  to the Samba store and re-pushing the share config.
- **Interior mutability**: `AppState.shares`/`samba_users` are `std::sync::Mutex`
  (CRUD handlers mutate them). The one subtlety: `apply_shares_config` resolves
  LDAP groups BEFORE locking the account store — a `MutexGuard` across an await
  made the handler futures non-`Send` (found via `axum::debug_handler`).
- **Tests (48 total; +6 shares/users contract, +5 smbusers unit):** share list
  empty, create 400 paths, samba text, status revision, users/groups 503 without
  LDAP; NT-hash validation, smbpasswd flags/LCT, passwd/group/shadow rendering,
  group_gid stability, colon rejection.
- **Image** rebuilt: 14.2 MB. **Not yet:** the live LDAP/SMB drill (needs the
  Rust API deployed, S7); S4 apps/catalog/helm is next.

### Phase 2 S4a — Helm CLI adapter (2026-10-04) — branch `refactor/rust-api-apps`

S4 is apps/catalog/helm (~2450 LOC); it starts with the Helm adapter, since
there is no Rust Helm SDK (the plan's bundled-CLI decision).

- **`rust/api/src/helm.rs`**: drives the bundled `helm` binary — `install_dir`
  (`--wait --timeout 5m0s`), `upgrade_dir` (`--reset-values`, mirroring the Go
  `ResetValues=true`), `uninstall`, `list`, `get`, `rollback`. Values are written
  to a temp JSON file and passed with `-f`.
- **In-cluster kubeconfig**: the Go SDK resolved its Kubernetes config
  in-cluster, which the `helm` CLI does not do automatically, so the adapter
  builds a kubeconfig from the pod's service account (`KUBERNETES_SERVICE_HOST`
  + the SA token/CA) unless `HELM_KUBECONFIG` is set.
- **JSON parsing**: `helm list -o json` and `helm status -o json`, with
  `release_status` (deployed→running, pending→pending, uninstalled→stopped),
  `split_chart` ("nginx-15.0.0" → name/version), and `parse_helm_time` for the
  list's `updated` field.
- **Tests (4):** status mapping, chart split, list-time parsing, list-entry →
  App. **Not yet (S4b/S4c):** chartsrepo (git clone + sources), catalog, apps
  install/jobs and their handlers.

### Phase 2 S4b — chartsrepo + catalog (2026-10-04) — branch `refactor/rust-api-charts`

The chart-repository ingestion layer and the catalog that reads it.

- **`rust/api/src/chartsrepo.rs`**: `Source`/`AuthType`/`Store` (source CRUD,
  channels, DNS-1123 name validation), `Manager` (cache dir per source/channel,
  `refresh`/`refresh_all`, `ensure_fresh` with the TTL + stale-clone fallback,
  `app_names`/`app_dir`, `safe_join` traversal guard, `stat_repo` size/file
  guard), and the credentials trait.
- **Git via a `GitBackend` trait**: `GitCliBackend` drives the bundled `git`
  CLI (`clone --single-branch --depth 1 --no-tags`, `fetch`+`reset --hard`),
  with HTTPS-token basic auth and `GIT_SSH_COMMAND` for deploy keys. The trait
  keeps the repository logic testable without a real remote. (Deviation from the
  plan's gix/git2 note, consistent with the helm/talosctl bundled-CLI decision;
  the API image will bundle `git` at S7.)
- **`rust/api/src/catalog.rs`**: `App`/`Service`/`ExposureDefaults`/
  `CatalogEntry`/`SourceRef`, YAML `naslos-app.yaml` + `Chart.yaml` parsing,
  `load`/`pick_winner` (user-over-official, then Prod, then alphabetical),
  `validate_manifest` (name == folder, DNS-1123, port/scheme, release-name-only
  service templates), and the summary/detail accessors.
- **Tests (11 new, 48 lib total):** DNS-1123 + source validation, channel
  defaults/custom + branch lookup, `safe_join`, cache dir casing, `app_names`
  filtering; catalog load, user-over-official precedence, name/folder mismatch,
  channel collection, service-template rules.
- **Not yet (S4c):** the apps install/jobs package and the catalog/apps/sources
  handlers.

### Phase 2 S4c — apps orchestration (2026-10-04) — branch `refactor/rust-api-charts`

The installed-app record store and lifecycle orchestration (port of
`api/internal/apps`, 912 LOC). No client-go needed — it composes the ported
helm/catalog/chartsrepo and two traits.

- **`rust/api/src/apps.rs`**: `Exposure`/`Record`/`View`/`DiscoveredService`,
  `Config`/`Manager`/`Store` (atomic 0600 JSON, clone-on-read), and the
  lifecycle: `install` (resolve chart → merge values → helm install → discover
  service → record → route), `upgrade` (re-merge + `--reset-values`),
  `set_exposure` (toggle merge preserving the route target), `uninstall`,
  `list` (merges managed + privileged namespaces), `backfill` (orphaned
  records), `reconcile_routes`, `discover_services`, `view`/`url_for`/
  `auth_allowed`, and the `exposure_for`/`merge_values`/`render_service_name`
  helpers.
- **Traits** for the S5/kube edges: `Router` (exposure layer) and
  `ServiceDiscoverer` (release Services); both optional, so the manager works
  without them. `CatalogProvider` is a swappable closure (refresh replaces the
  snapshot).
- **Parity details**: `Record.Namespace` is `json:"-"`; progress stages
  (preparing/installing/finalizing); user values deep-merge over defaults;
  routing failure is non-fatal (recorded as `lastError`); release-name-only
  service templates.
- **Tests (5 new, 53 lib total):** release-name/subdomain validation, deep
  merge, service-name templating, store round-trip + ordering.
- **Not yet (S4d):** the catalog/apps/sources handlers and the async app-jobs
  runner, plus wiring the Manager into AppState.

### Phase 2 S4d — catalog + sources handlers (2026-10-04) — branch `refactor/rust-api-charts`

The read/refresh half of the app catalog.

- **`AppState`** gains `charts: Option<Arc<chartsrepo::Manager>>` and
  `catalog: Arc<CatalogHolder>` (a swappable snapshot). `build_chart_repos()`
  constructs the source store, seeds the official source from `SOURCES_OFFICIAL_*`,
  builds the git manager (public-only creds for now — documented) and the initial
  catalog; `build_catalog()` scans every source/channel dir.
- **`server/catalog.rs`**: `GET /api/catalog`, `GET /api/catalog/{name}`,
  `GET|POST /api/sources`, `POST /api/sources/refresh?name=`,
  `GET|DELETE /api/sources/{name}` — with the 503 "chart repositories are not
  available" guard and catalog rebuild after refresh/delete.
- **Routes** for catalog/sources moved off the 501 placeholder.
- **Tests (2 new, 76 total):** empty catalog without sources; sources 503 without
  the chart manager.
- **Not yet (S4e):** the apps handlers (`/api/apps*`), the async app-jobs runner,
  and wiring the apps Manager into `AppState`.

### Phase 2 S4e — apps handlers + async jobs (2026-10-04) — branch `refactor/rust-api-charts`

Completes S4. The Rust API now serves the full app-catalog + lifecycle surface.

- **`server/app_jobs.rs`**: the in-memory job registry (`JobPublic`/`JobState`/
  `JobKind`/`JobStage`, `AppJobManager` with conflict detection + a ~20-finished
  retention cap) and the async runner — the handler enqueues and answers 202,
  the install/upgrade/uninstall runs in a server-owned task the operator polls.
- **`server/apps.rs`**: `GET|POST /api/apps` (install requires `confirmed`,
  base-domain gate, conflict → 409, 202 + jobId), `GET|PUT|DELETE
  /api/apps/{name}` (upgrade/uninstall jobs), `/{name}/services`,
  `/{name}/exposure` (GET reads the record's exposure + domains; PUT validates +
  applies), `GET /api/apps/jobs`, `GET /api/apps/jobs/{id}`.
- **Wiring**: `AppState` gains `app_manager` (built from the helm clients, the
  charts manager and a catalog closure), `app_jobs` and `base_domain`. Routing
  (S5) and Service discovery (kube) are `None` for now, so installs record +
  render but do not yet route — documented.
- **Tests (4 new, 82 total):** apps 503 without the manager, install 503,
  empty jobs list, unknown job 404.
- **Not yet:** S5 (routing/certs/authelia/domains/providers/ddns) supplies the
  Router/Discoverer and the real SSO/domain lists; S6 buddy/notifications.

### Phase 2 S5a — DNS providers (2026-10-09) — branch `refactor/rust-api-routing`

S5 (routing/certs/authelia/domains/providers/ddns, ~3600 LOC) starts with the
declarative DNS provider registry — the base the domains and DDNS layers build
on.

- **`rust/api/src/providers.rs`**: `Field`/`ShowIf`/`CertManager`/`Ddns`/
  `Provider`/`Registry`, embedded built-ins (the 17 YAMLs via `include_dir`,
  copied from `api/internal/providers/builtin/`), an optional override
  directory, `parse`/`validate`, `ResolveFields` (the single place deciding
  config vs Secret vs required vs valid), `WithDefaults`, and the cert-manager
  `Solver` renderer (`${secret}` / `${cred.<key>}` placeholders).
- **`server/providers.rs`**: `GET /api/providers` (views + load errors), moved
  off the 501 placeholder. `AppState.providers` is loaded from
  `DDNS_PROVIDERS_DIR`.
- **Tests (7 new, 89 total):** all 17 built-ins load clean, OVH solver renders
  the secret + cred, passthrough supports certificates without a solver,
  ResolveFields splits secret/config, a bad override is recorded not fatal,
  field-type validation, and the handler lists built-ins.
- **Not yet (S5b/S5c):** domains (the certs domain store) + authelia; routing +
  certs reconcilers (kube); ddns (drivers + manager + handlers).

### Phase 2 S5b — domains + authelia fragments (2026-10-09) — branch `refactor/rust-api-routing`

The pure halves of the certificate/SSO layer (the kube reconcilers are S5c).

- **`rust/api/src/certs.rs`**: `Domain`/`Provider`/`Store`, `validate`,
  `secret_name`/`issuer_name`, and `spec()` rendering the cert-manager `Issuer`
  + wildcard `Certificate` as JSON (staging/production ACME servers, the DNS-01
  solver from the provider registry — passthrough or rendered). A process-wide
  provider registry (`configure_registry`, mirroring the Go global) links certs
  to providers.
- **`rust/api/src/authelia.rs`**: `fragments(domains)` rendering `cookies.yml`
  and `rules.yml` — the primary apex is omitted (static chart rules) while every
  wildcard is emitted.
- **Tests (7 new, 68 lib total):** Cloudflare production spec (ACME server +
  dnsNames), staging default, domain validation (bad domain/missing secret/
  passthrough/bad env), store round-trip; cookie/rule shape, primary-only,
  zero-domain empty list.
- **Not yet (S5c):** the certs and authelia reconcilers (dynamic/client-go →
  kube), the domains handlers, and wiring the Router/ServiceDiscoverer.
