# AI Handoff — Naslos

**This file is the project's current state, kept to about a page.** The detailed
per-session narrative (1250 lines, 2026-09-17 snapshot) is archived at
`docs/archive/ai-handoff-log-2026-09.md`. The audit and fix report — the only
current audit document — is `docs/AUDIT-2026-09-19-REPORT.md`; the detailed
working papers (audit findings, fix plan, code-review list, the superseded
2026-09-14 audit) are archived in `docs/archive/`. Normative requirements are in
`docs/spec.md`.

## What this is

A single-node NAS appliance on Talos Linux (Kubernetes) with a web UI.

## Current instance (2026-10-04) — clean install from the desktop installer, v0.1.2

`192.168.1.117` was reset to maintenance and provisioned end to end by the
**Naslos-Installer desktop app** (a clean install from the user's PC), embedding
the `v0.1.2` install pack. Live state:

- Node `talos-nu3-7j0`, Talos v1.14.1 / k8s v1.37.0, **control-plane Ready**.
- Helm release `naslos` (chart `naslos-0.1.2`), all images pinned by digest
  (`ghcr.io/aessemops/naslos-api@sha256:aabdf383…`).
- The install's **recovery bundle is extracted into `bootstrap/vm/`**:
  `talosconfig`, `controlplane.yaml`, `talos-secrets.json`, `kubeconfig` — all
  gitignored (master credentials, never commit). `kubeconfig` is the live
  cluster-admin access: `export KUBECONFIG=bootstrap/vm/kubeconfig`.
- The administrator is `admin` with a TOTP device (secret in the emitted
  `totp` payload / recovery ZIP README); the login URL is
  `https://naslos.local/authelia`.

Kept to continue work on the appliance from the workspace. The earlier
`0.1.0-r*` live-drill sections below are historical.

## RAM plan — Phase 0 done (PR #54), Phase 1 done (PR #55), Phase 2 spike done

Plan: `.kilo/plans/1791069821704-go-to-rust-migration.md` (see "Execution log").
Goal: free RAM for custom apps. `TARGET_FREE` = **>= 2.5 GiB available under
load** on this 5.76 GiB node.

- **Phase 1 (merged):** the privileged agent is now Rust (`rust/agent`,
  `naslos-agent`). Live measurement: ~4 MB/pod saved.
- **Phase 2 spike (branch `refactor/rust-api-spike`):** a minimal Rust API
  (`rust/api-spike`) with health/ready/dashboard/metrics + auth + the metrics
  collector measured at **4.6 MiB RSS / ~5 MiB working set** vs the Go API's
  **18.8 MiB RSS / 98.1 MiB working set** live — the gate **PASSES** (~93 MiB
  reclaimed), so the full API port is justified. Not yet deployed.
- **Phase 2 S1 (branch `refactor/rust-api`, in progress):** `rust/api`
  (`naslos-api`) foundation — config/logsafe/auth/metrics/talos(CLI)/server with
  the full route surface and correct auth gates; health/ready/auth-me/dashboard/
  metrics are live, other owner routes return 501. Talos CLI adapter verified
  live. `api/Dockerfile.rust` builds (10.6 MB, ~2.4 MiB RSS). The Go `api/`
  remains the deployed image until parity.
- **Phase 2 S2 (same branch):** agent HTTP client + ZFS/disks/datasets handlers
  (`/api/volumes/zfs*`, `/api/datasets`, `/api/disks*`) with ported validation
  and agent-error mapping (503 forwarded, transport → 502), plus the Talos disk
  discovery + topology advisor and a read-only shares store. 31 crate tests
  including a mock-agent server. Shares render/apply, LDAP, apps/helm, routing,
  domains/ddns and buddy remain.

## RAM plan Phase 0 detail (2026-10-04) — Prometheus removed; ARC blocked

Branch `chore/ram-baseline-prometheus-removal` (PR open). Plan:
`.kilo/plans/1791069821704-go-to-rust-migration.md` (see "Execution log").
Goal: free RAM for custom apps. `TARGET_FREE` = **>= 2.5 GiB available under
load** on this 5.76 GiB node.

- **Baseline harness**: `scripts/memory-baseline.sh` (kubelet summary API +
  `talosctl` arcstats; no Prometheus/metrics-server). Idle baseline: available
  3.4 GiB, pod working set 1.81 GiB, ZFS ARC `c_max` default 4.76 GiB.
- **Prometheus/Alertmanager subchart removed** (Chart.yaml, Chart.lock, values,
  schema, docs, CREDITS). Live `helm upgrade` on the v0.1.2 instance removed the
  stack (working set 1.81 -> 1.59 GiB) and an orphaned alertmanager PVC was
  deleted; `/api/health`, `/api/metrics`, `/api/dashboard` all 200.
- **ARC cap is BLOCKED, not deployed.** On Talos v1.14.1 + `siderolabs/zfs` +
  SDBoot, `machine.kernel.modules[].parameters` is ignored (the extension loads
  the module), `machine.install.extraKernelArgs` is unsupported, and a
  `machine.files` modprobe drop-in was ignored (and coincided with a CRI boot
  failure). The runtime sysfs write works. Deferred; planned fix is an agent
  DaemonSet initContainer writing `/host/sys/module/zfs/parameters/zfs_arc_max`
  at boot. Evidence in `docs/deployment.md` -> "ZFS ARC".
- **Right-sizing**: OpenLDAP request 256Mi -> 768Mi (measured idle 673 MiB);
  other workloads left as-is (requests are hints, not reclamation).
- **Live now (2026-10-04)**: helm revision **5** (`naslos-0.1.2`), no
  Prometheus/Alertmanager, OpenLDAP request 768Mi/limit 1Gi. The one-time
  alertmanager PVC cleanup is wired into `make install-vm`. Prometheus removal
  is the only deployed RAM win so far; the ARC cap is still the missing piece.
- **Gate**: not met until ARC is capped — `c_max` can still grow to ~4.76 GiB
  under load. Phase 1/2 remain justified.

## App catalog refactor (2026-09-26) — implemented, live-drilled

Merged in PR #28 (branch `feature/charts-repo-and-app-install-refactor`).
Plan + full status: `.kilo/plans/1789943277180-charts-repo-and-app-install-refactor.md`.

The hard-coded Go catalog is replaced by **git-based chart repositories**:

- `api/internal/chartsrepo` — source store (official + user), pure-Go `go-git`
  clone/pull per `(source, channel)` into `/var/lib/naslos/charts`, TTL refresh
  with stale fallback, credentials from Secrets (public/token/SSH), traversal +
  size guards.
- `api/internal/catalog` — loads `apps/<name>/naslos-app.yaml` from the clones;
  user sources override the official repo on name collision; channel aggregation.
- `api/internal/apps` — persisted install records (`apps.json`), install/
  upgrade/exposure/uninstall from the **local clone** (helm `LoadDir`), orphan
  backfill (Helm workload labels, read-only), route-target discovery when a
  manifest declares no `services[]`.
- `api/internal/routing` — one Traefik `IngressRoute` + middlewares per app,
  rendered/applied by the API; `security-headers` omits `stsIncludeSubdomains`.
- `api/internal/certs` — base domains + cert-manager ACME DNS-01 `Issuer` and
  wildcard `Certificate` CRs; SSL page gated on the CRDs.
- Chart: `naslos-apps` (PSA `baseline`), opt-in **`naslos-apps-priv`** (PSA
  `privileged`) for `privileged: true` charts (gluetun `NET_ADMIN`); namespaced
  API Roles; the **`naslos-datasets`** RWX PV/PVC (baseline forbids `hostPath`);
  `networkPolicy.gitEgress` (443/22/9418); `apps.officialSource.channels`
  (`{Prod: main}`); `make crds` installs the cert-manager CRDs.
- UI: Sources tab, config→exposure→review→confirm install, exposure editor with
  route-target picker, Domains & SSL page.

**NaslosCharts** (public, `AessemOps/NaslosCharts`): README app-authoring spec +
10 charts — Jellyfin, Radarr, Sonarr, Seerr, FlareSolverr, Prowlarr(+gluetun),
qBittorrent(+gluetun), Audiobookshelf, Calibre-Web, SearXNG (last two VPN charts
declare `privileged: true`). Chart repo is currently a single `main` branch.

**Live on `192.168.1.117` (helm revision 21, `naslos-api 0.1.0-r22`, `naslos-ui
0.1.0-r13`)**: catalog refresh pulled all 10 apps from GitHub; Jellyfin installed
from the catalog and serves **200 via Traefik**; a `privileged: true` test chart
installed into `naslos-apps-priv` with its route; Playwright `apps.spec.ts` +
`domains.spec.ts` 11/11. Bugs the drill found and fixed are listed in the plan
(Release.Name service template, forwardAuth namespace, exposure PUT clobbering
the target, Helm-list backfill needing secret access, `make crds` empty
cert-manager step, HSTS subdomains, baseline hostPath → datasets PVC).

**Not done**: start/stop (`FR-APP-04`) is `[OPEN]`; gluetun charts need the
user's VPN provider/key. (PR #28 is merged; the current deployed tags are in
"Deployed right now" below.)

## Dynamic DNS + declarative DNS providers (2026-09-26) — implemented, merged, live-drilled

Merged in PR #29 (branch `feature/ddns-dynamic-dns`). Plan:
`.kilo/plans/1790427669127-dynamic-dns-providers.md`. The provider configs and
the detection model are based on
[qdm12/ddns-updater](https://github.com/qdm12/ddns-updater) (MIT) — see
`CREDITS.md` → "Ported & adapted code".

- `api/internal/providers` — declarative registry: embedded YAML defaults plus
  an optional override directory (`DDNS_PROVIDERS_DIR`). Cert providers:
  `ovh`, `cloudflare`, `rfc2136`, `passthrough`. DDNS-only providers: `generic`
  (custom HTTP), `duckdns`, `dynu`, `noip`, `freedns`, `namecheap`, `desec`,
  `spdyn`, `selfhostde`, `dynv6`, `digitalocean`, `godaddy`, `porkbun`. Field
  model: `secret`, `secretKey`, `scope: cert|ddns`, `showIf: {key,value}`. An
  informational `apiRights` list (DNS-01 permissions) is returned by
  `GET /api/providers` and shown in the Domains form's info bubble. A bad
  override is skipped, logged and surfaced by `GET /api/providers`.
- `api/internal/certs` — `Validate`/`solverFor` render through the registry;
  OVH certificates go through the **`cert-manager-webhook-ovh` webhook** (OVH is
  not a cert-manager built-in DNS-01 solver); `cloudflare` / `rfc2136` /
  `passthrough` solver output is pinned byte-identical by tests. `Domain` has
  `providerConfig`; the Domains form fetches the registry and hides
  `scope: ddns` fields.
- `api/internal/ddns` — entry store (atomic JSON, 0600) and drivers `ovh`,
  `cloudflare`, `digitalocean`, `godaddy`, `porkbun`, `http`. Detection follows
  ddns-updater: the reconciler DNS-resolves each record and updates only when
  the public IP is not among the answers (a lookup failure updates), with a
  per-record cooldown (`DDNS_UPDATE_COOLDOWN_SECONDS`); proxied Cloudflare
  compares the stored `lastIP`. IP detection cycles
  `DDNS_IP_SOURCES`/`DDNS_IPV6_SOURCES` (HTTP URLs plus `dns:opendns` /
  `dns:google`). OVH has `mode: dynamic` (DynHost username/password, default)
  and `mode: api` (ZoneDNS signed API). `POST /api/ddns/{id}/run` forces a run
  (bypasses the DNS pre-check and cooldown).
- Credentials are written to a Secret in `naslos-apps` (`naslos-ddns-<id>`,
  `naslos-domain-<domain>-creds`); the API never returns a value. No Secret
  access is added in the release namespace.
- Routes: `GET /api/providers`, `GET/POST /api/ddns`,
  `GET/PUT/DELETE /api/ddns/{id}`, `POST /api/ddns/{id}/run` (owner/admin).
- Chart: `ddns` values (`ipSources`/`ipv6Sources`/`updateCooldownSeconds`), API
  env + optional `ddns.providersConfigMap` mount, `networkPolicy.ddnsEgress`
  (HTTP/HTTPS + DNS 53, **release namespace only**); VM profile enables it. UI:
  `/dns` page + sidebar entry. Spec §3.9 `FR-DNS`; `docs/dynamic-dns.md`;
  `CREDITS.md`.
- Live-drilled: a real OVH DynHost entry (`dyn.florentinrichard.fr`) updated to
  the public IP; Playwright `ddns.spec.ts` + `domains.spec.ts` 9/9 (the OVH
  form shows DynHost fields, hides ZoneDNS fields until `mode: api`).

**Not done**: `ipv6_suffix` is intentionally not implemented (ddns-updater's
IPv6 interface-identifier rewrite is not meaningful for the API pod); further
providers are added as YAML, not code.

| Piece | What it is |
| --- | --- |
| `api/` (Go) | UI-facing HTTP API + the buddy sender, scheduler and job runner |
| `agent/` (Go) | Privileged, host-networked DaemonSet: the only thing that runs `zpool`/`zfs`/`wipefs`; also serves the streaming backup endpoints |
| `rust/agent/` (Rust) | Port of `agent/` — same HTTP contract; the privileged DaemonSet image is now built from `agent/Dockerfile.rust` |
| `ui/` | Svelte 5 + TS + Tailwind, built statically and served by unprivileged nginx |
| `charts/naslos` | Helm chart: api, ui, agent, samba, nfs, terminal, openldap, Traefik + Authelia (and optional cert-manager) subcharts |
| `openldap/ samba/ nfs/ terminal/` | Per-service images and config templates |
| `api/cmd/buddyctl`, `api/cmd/buddy-receiver` | Standalone backup client and Docker receiver (no Kubernetes, no ZFS) |

Features: ZFS pools/datasets, SMB + NFSv4 shares with LDAP identities, dashboard and
metrics, web terminal, app catalog, notifications, and zero-knowledge peer backups
("Buddy").

## Where things stand (2026-09-19)

- **2026-09-26 update — app catalog refactor.** Branch
  `feature/charts-repo-and-app-install-refactor` (PR #28, open) replaces the
  hard-coded catalog with git chart repositories, adds the app lifecycle,
  routing/certs, the privileged apps namespace and the datasets PV/PVC (see the
  section above). All gates green; deployed to `192.168.1.117` at revision 21.
  The bullets below describe the pre-refactor `master`/audit state that the
  branch is based on.
- **`master` = `85ae874`**; PRs #9–#23 merged (buddy, security fixes, the
  authenticated-only removal of the dev endpoint, namespace/Authelia/admin
  gating). The audit branch `audit/full-2026-09-19` carries the remediation
  through `b985e03` (50 commits, 63 ahead of `master`) and **is not merged yet**.
- **The instance runs on `192.168.1.117`** — see the section below. The old
  `192.168.1.96` VM is powered off.
- **The full audit and its fixes are in `docs/AUDIT-2026-09-19-REPORT.md`.**
  All four Highs are fixed (LDAP service credential rotated/de-committed, backup
  chunks on their own dataset, Go vulnerabilities 18 → 4 with no fix available
  for the rest, Grafana removed), 10 of 14 Mediums are closed, and the NAS-010
  default-credential work is closed, *including* the live LDAP admin rotation.
  **M4 and M6 are effectively done:** flannel and kube-proxy were replaced by
  **Cilium v1.20.2**, the chart's default-deny/per-workload policies plus the
  `naslos-allow-host` `CiliumNetworkPolicy` are **enforced live**, and the four
  hostNetwork/privileged workloads (agent, samba, nfs, terminal) were split into
  the **`naslos-privileged`** namespace. The remaining work is §8 of the report:
  the M4 residual (the hostNetwork agent `:9090` is still reachable from pods —
  pod-level policy cannot cover host-network pods; the Cilium host-firewall
  template is off by default and its node-selector semantics are unresolved),
  Authelia's `jwt_secret` still in the `authelia-config` ConfigMap (**M3
  residual** — the LDAP bind password moved to a Secret mount at revision 41),
  the recorded `trivy config` items (openldap root/`readOnlyRootFilesystem`,
  Dockerfile last-`USER root`), and the upstream trixie base-CVE backlog. Batch 6
  is **done**: the `trivy` image scan (api/ui/agent 0 HIGH at scan time; the four
  trixie images' 219 findings are all no-Debian-fix base libraries), the `trivy
  config` scan (fixed DS-0031 baked `LDAP_ADMIN_PASSWORD`, removed unused
  `pods/exec` Role KSV-0053), `semgrep` (4 false positives, verified by hand),
  the AV-5…AV-12 active tests (AV-8/11/12 live), and the buddy crypto deep-dive
  (no findings). The two AV-8 findings are **fixed**: the agent reports exact
  `UsedBytes` and the send refuses a contentless stream (a small-fraction guard
  with a 1 MiB floor), and `DestroyDataset` recovers from a stale-mount
  `dataset is busy` via `zfs unmount -f` then `mountpoint=none,canmount=off`
  (a leaked-kernel-reference variant is named with its pool export/import
  remedy; `test/audit-av8` remains a stray stub). The audit sweep gates on
  govulncheck and gosec again. Everything else — the dependency bumps, nginx
  non-root, `values.schema.json`/`.Release.Namespace`, `go test -race`, the
  log/conversion hardening and the dead-code cleanup — is done and deployed.
- **The detailed audit, fix plan and code-review list are archived** in
  `docs/archive/`; the report supersedes them.

## Current instance: 192.168.1.117 (2026-09-19)

Fresh install from scratch (the old .96 VM is off): Talos from the Talos ISO
(Naslos schematic), etcd bootstrapped, local-path provisioner, OpenLDAP + bootstrap. The instance is
now on the **only posture** (`make install-vm`): Traefik on hostPort 80/443,
Authelia forwardAuth with the portal at `https://naslos.local/authelia`, 2FA for
`naslos_admins` on every path, `/api` routed straight to the API. The dev posture
was **removed entirely** — no UI NodePort, no `auth.disabled`, no
`AUTH_DISABLED`/`AGENT_AUTH_DISABLED`; the API and agent refuse to start without
their credential. The first admin (`admin` in `naslos_admins`) predates the
removal. Recovery is `helm rollback` / `git revert` + redeploy, not a bypass. The
Playwright suite authenticates through Authelia (see "Running the E2E suite" in
`docs/deployment.md`).

- **Fresh PKI.** `scripts/deploy-vm.sh` *reuses* `bootstrap/vm/talosconfig` when it
  exists, so a brand-new VM would have been installed with the **old cluster's CA
  and cluster secret** (the same discovery identity as .96). The PKI was regenerated
  for this node, the new config applied using the old credentials as client, then
  bootstrapped. When re-imaging, move `bootstrap/vm/talosconfig*` aside first.
- **Deploy fixes found by doing it** (PR #20): the authenticated-apply fallback now
  matches `tls: certificate required` / `unknown authority` (against an installed
  node the insecure apply is refused, and the script used to give up); and the Helm
  install now overrides **every** image tag (samba, nfs, terminal and the API's
  LDAP-wait `openldap.image` were left on `:0.1.0`, which no longer exists →
  `ErrImagePull` on a fresh tag).
- **Talos upgraded v1.14.0 → v1.14.1** with the same pinned schematic
  (`--drain=false`: single node, everything reboots anyway). Result: kernel
  **6.18.51-talos**, containerd **2.3.5**, `zfs 2.4.4-v1.14.1`, `ext-zfs-service`
  up on the new boot, `talosctl health` green, all workloads back. During the
  reboot the transient `not-ready` taint held the Deployments Pending (DaemonSets
  tolerate it) until the node was Ready — no intervention needed.
- **Verified after**: `/api/ready` 200, `/api/users` 200 (LDAP bind),
  `/api/volumes/zfs` 200 (agent + ZFS), dashboard with `ens3 → 192.168.1.117`,
  Playwright **16 passed / 5 skipped** (same baseline; the skips are the bare-install
  gaps, CR-23). The local `talosctl` client is v1.14.0 against a v1.14.1 server —
  fine within the minor, upgrade when convenient.
- **Storage is in place**: pool **`test`** = stripe of `/dev/vdb`+`/dev/vdc`
  (79 G usable), dataset `test/drill` (the Playwright suite uses the first child
  dataset, so a child dataset should exist for it). Created through the API
  (`POST /api/volumes/zfs`), so the agent's `zpool create`/`zfs set` path is the one
  exercised. **Re-import verified**: with the pool and datasets present the node was
  rebooted, `ext-zfs-service` imported the pool automatically, `zpool status`
  came back ONLINE with both vdevs and the datasets reported `mounted: true` by the
  agent — this closes the gap the Talos upgrade test left open. The suite then ran
  **28 passed / 4 skipped** (up from 16/5): the extra passes are the shares/pools
  specs that were skipping for lack of storage.
- **Buddy is enabled and drilled** (chart revision 3, `buddy.name=naslos-vm`,
  receive dataset `test/naslos-buddy`): the ownership init container chowned the
  freshly created dataset by itself (`was 0:0`), the identity was created
  (`naslos-vm`, fp `SHA256:R0SKyVd…`), and the instance was authorized on its own
  receiver as `self-loopback` (`allowedSources: [naslos-vm/]`). Send → verify →
  restore all passed: chain `ad1aa6ecd3d59a99`, 1 chunk / 44 376 B, verify digest
  `e29d09f2…`, and the restore landed `test/drill-buddy-restored` identical to the
  source (96K/96K, 1.00x) carrying its `buddy-20260919T002521Z-3aa5` snapshot.
- **Suite: `35 passed` against the proxy posture** (setup + 34 tests), including
  the interactive terminal tests that used to skip on the NodePort. See below.
- **The authenticated posture is live (helm revision 16).** Traefik hostPort 80/443,
  `https://naslos.local` (mDNS via the Samba container's Avahi record), Authelia
  forwardAuth, portal at `/authelia`, a chart-generated `naslos-tls` cert, `/api`
  routed straight to the API, NodePort off. The Traefik dashboard is at
  `/traefik/dashboard/` (admin-only, linked from the sidebar for
  `naslos_admins`; `api.basePath=/traefik`, `api.insecure` off). Verified
  unauthenticated:
  `/api/health` 200, `/` and `/api/users` 302 to the portal, `/api/buddy/v1/`
  bypass reaches the API, a pod calling the API without the proxy secret gets
  401, and `:30080` refuses. `two_factor` is applied per **subject**
  (`group:naslos_admins`) on every path, so the portal starts TOTP/WebAuthn
  enrolment at the first login; a path-list version let the SPA open on a
  one-factor session and the admin API calls bounced, which the UI showed as
  "Failed to connect to API". Still to do by a human: log in once as `admin` at
  `https://naslos.local` and finish enrolment. Gotchas found doing the cutover,
  now encoded in the chart: the Authelia Service is `<release>-authelia`
  (port 80), a bare `domain: "*"` never matches in Authelia (use the real host),
  the forwardAuth `authelia_url` needs a trailing slash, subject-based
  two_factor beats a path list for an all-admin appliance, and the Makefile now
  passes a config checksum so editing `authelia-config.yaml` rolls the Authelia
  pod. Rollback is in `docs/deployment.md`. Cosmetic residual: the peer JSON still
  serializes `lastSeenAt` as `0001-01-01T00:00:00Z` (the schedule equivalent was fixed;
  the peer struct was not).
- **The Playwright suite now runs against the production posture, 35 passed on
  two consecutive runs** (helm revision 15; re-verified `35 passed` at revision
  16 with api `0.1.0-r4`, agent `0.1.0-r3`, ui `0.1.0-r5`). It
  logs into Authelia once in a setup project (`ui/tests/auth.setup.ts`, password
  + TOTP generated from the secret in the gitignored `ui/.env.playwright.local`)
  and reuses the session via `storageState`. See `docs/deployment.md` "Running
  the E2E suite" for the env file and `NASLOS_RECEIVER_URL`. Running it found two
  real production bugs, both fixed:
  - **Agent streaming token (NAS-002 gap).** `api/internal/agent/backup.go` used
    a package-level `&http.Client{}` for the streaming send/receive paths, so
    those requests carried no `Authorization` header and every backup failed
    with the agent's 401 once auth was on. The client now has a dedicated
    `stream` client built from the same token transport
    (`api/internal/agent/client_test.go` pins it). It stayed hidden while the
    agent ran with the (now removed) `AGENT_AUTH_DISABLED=true` opt-out, which is
    why it only surfaced once auth was enforced.
  - **Password changes were a silent no-op.** The user-edit form put `password`
    in the `PUT /api/users/{uid}` body, which ignores it; the dialog still closed
    as if saved. It now calls `POST /api/users/{uid}/password`. `docs/spec.md`
    had that route as PUT, which the handler rejects.
  The suite also needs `NASLOS_RECEIVER_URL` for its two self-send tests: a
  page-driven send targets the browser origin, and the API pod cannot resolve
  `naslos.local` (mDNS is not in cluster DNS).

## How to run it

```bash
# Gates (all pass; scripts/audit.sh runs the sweep including go test -race)
sh scripts/audit.sh
cd api   && go build ./... && go vet ./... && go test -race ./...
cd agent && go build ./... && go vet ./... && go test -race ./...
cd ui    && npm run check && npx playwright test      # Playwright needs the VM
helm lint charts/naslos -f charts/naslos/values.yaml

# Deploy: always with FRESH tag suffixes (a retag can serve stale code)
make api-image IMAGE_TAG=0.1.0-r29 && docker push 192.168.1.2:30095/naslos-api:0.1.0-r29
make ui-image  IMAGE_TAG=0.1.0-r21 && docker push 192.168.1.2:30095/naslos-ui:0.1.0-r21
# install-vm renders -f values.yaml -f values-vm.yaml (NOT --reuse-values), so
# bump the tag in values-vm.yaml or pass --set; new keys apply from the files.
# It also installs cert-manager + the OVH webhook first (see its prerequisites).
make install-vm HELM_FLAGS="--set api.image.tag=0.1.0-r29"
curl -sk -o /dev/null -w '%{http_code}\n' https://naslos.local/api/health

# App-catalog API is owner-gated AND restricted to the proxy/pod CIDR, so query
# it from a trusted pod, not from the workstation:
SECRET=$(kubectl -n naslos get secret naslos-proxy -o jsonpath='{.data.secret}' | base64 -d)
kubectl -n naslos-privileged exec deploy/naslos-terminal -- sh -c \
  "curl -s -H 'Remote-User: admin' -H 'Remote-Groups: naslos_admins' \
   -H \"X-Naslos-Proxy-Secret: $SECRET\" \
   http://naslos-api.naslos.svc.cluster.local:8080/api/catalog"
```

VM facts: node `192.168.1.117`, UI at `https://naslos.local` (the only listener;
no NodePort), private registry `192.168.1.2:30095`, namespaces `naslos`
(authenticated services), `naslos-privileged` (agent/samba/nfs/terminal),
**`naslos-apps`** (installed apps, PSA baseline) and **`naslos-apps-priv`**
(privileged apps, PSA privileged), `TALOSCONFIG=bootstrap/vm/talosconfig`.
Pods have outbound git access (`networkPolicy.gitEgress`) and the datasets root
is exposed as the `naslos-datasets` RWX PVC/PV.
Pool `test` (stripe of `/dev/vdb`+`/dev/vdc`, 79 G) with datasets `test/drill` and
`test/naslos-buddy` (the buddy receive dataset); **Buddy is enabled** with
`buddy.name=naslos-vm` and `peersFile`/`schedulesFile` on the state PVC.

## Operational gotchas (hard-won — read before drilling)

1. **Always bump image tag suffixes.** The registry reuses tags and the chart pulls
   with `IfNotPresent`, so a retag alone can keep running old code (NAS-022).
   Digests are supported (`api.image.digest`, `make image-digests`); a stored digest
   wins over a new tag until you clear it (`--set api.image.digest=`).
2. **`helm upgrade --reuse-values` ignores `-f` files.** Anything new must be passed
   with `--set` (e.g. `ingress.enabled`), or the release keeps old values and
   templates that dereference new keys can fail to render.
3. **There is one posture and it is authenticated.** Traefik on hostPort 80/443
   with Authelia forwardAuth; no NodePort and no auth bypass exist. `make
   install-vm` installs it, `helm rollback`/`git revert` undoes it, and there is
   no unauthenticated listener to fall back to (NAS-008 resolved).
4. **The buddy identity is the KEK.** `/var/lib/naslos/buddy-identity.json` holds the
   private key and the key-encryption key: losing it makes every stored backup
   unreadable, and `helm uninstall` would delete it (CR-02). Back it up separately.
5. **Platform mount-propagation trap.** A dataset created from inside a pod is mounted
   in that pod's namespace only; in the host namespace its mountpoint is a plain
   directory on the parent. `zfs send` of such a dataset captures nothing — the API
   refuses it (`requireMountedDataset`), and the agent's `UsedBytes`-based guard
   also refuses a published stream that is a small fraction of the dataset's used
   space. The fix is a node reboot (or `zfs mount <dataset>` on the host). Shares
   have the same trap for newly created datasets.
6. **The receive dataset's ownership is fixed by the
   `fix-receive-dataset-ownership` init container** (uid 65532). Manual fallback:
   `chown 65532:65532 /var/mnt/<pool>/naslos-buddy`.
7. **Store-root dot files matter**: `.enroll-used` records a spent enrollment token
   (delete it to re-arm the token), `.nonces` is the persisted replay cache. Both are
   dot-prefixed so quota accounting ignores them; wiping `k*` directories clears the
   stored backups.
8. **Quotas are per key and count *sealed* bytes**, and `MinPeerQuota` is one sealed
   chunk (`MaxSealedChunkSize`), not one plain chunk.
9. **The Playwright suite runs against the live VM and can skip** when the identity,
   a dataset or a peer scope is missing: treat a skip as a setup gap, not a pass
   (CR-23). It authenticates through Authelia (credentials in the gitignored
   `ui/.env.playwright.local`) and runs against `https://naslos.local`.
10. **A dataset once mounted in a pod namespace can answer `dataset is busy`.**
    `zfs receive` runs through the privileged agent, so the destination stays
    mounted in the **agent's** mount namespace (the terminal pod's namespace does
    not see that mount). `DestroyDataset` now recovers on its own: on a busy
    failure it forces `zfs unmount -f`, then clears
    `mountpoint=none,canmount=off` and retries. Only a residual leaked-kernel
    reference (no mounts, no holds, no snapshots) still needs a pool
    export/import, which the agent deliberately does not do; the error names that
    state. `kubectl -n naslos-privileged rollout restart ds/naslos-agent` remains
    a manual fallback.
11. **PSA is enforced per namespace.** `naslos` and `naslos-privileged` are
    `privileged`; `naslos-apps` is **`baseline`** and `naslos-apps-priv` is
    `privileged`. Baseline **forbids `hostPath` volumes** (verified live:
    "violates PodSecurity baseline: hostPath volumes"), so managed app charts
    mount the platform's `naslos-datasets` RWX PVC; `hostPath` and `NET_ADMIN`
    only work in `naslos-apps-priv` (a chart opts in with `privileged: true`).
12. **The node has Internet; pods do not, by default.** `naslos-workload-egress`
    is default-deny, so the API cannot clone a chart repo until
    `networkPolicy.gitEgress: true` (on in `values-vm.yaml`; ports 443/22/9418).
    Image pulls are kubelet traffic and unaffected by pod policy.
13. **Channels are a source property, and authoritative.** A source that declares
    channels uses exactly those (no defaults merge). NaslosCharts has only `main`,
    so `values-vm.yaml` sets `apps.officialSource.channels: {Prod: main}`;
    a channel pointing at a missing branch reports a per-source error.
14. **`make install-vm` renders the `-f` files; it is not `--reuse-values`.** New
    keys in `values*.yaml` apply, but the official source is seeded only when it
    is absent from `sources.json` — to change its URL/channels/auth, delete it
    (`DELETE /api/sources/naslos`) and restart the API so it re-seeds from env.

## Conventions

- **Spec rule**: a change that alters a MUST in `docs/spec.md` updates the spec *and*
  its test in the same change; unimplemented MUSTs are marked `[OPEN]` (VER-4).
- **Credits rule**: `CREDITS.md` is always kept current. Any change that
  ports/adapts code or config from another project (add a provenance header to the
  ported file), or that changes a direct dependency (`api/go.mod`,
  `ui/package.json`, `charts/naslos/Chart.yaml`, or a Dockerfile base image),
  updates `CREDITS.md` **in the same change**, and bumps its `Last reviewed` line.
- **Found a defect while drilling?** Fix it in the same branch with a test, and record
  it where the next session will read it — that is how every fix in this repo landed.
- **Fixes must land in a fresh install**: put them in the declarative path (the
  `Makefile`, `charts/naslos/` + `values-vm.yaml`, or `scripts/deploy-vm.sh`) and in
  the images built at install time — never only in the live cluster. Verify with
  `make -n install-vm` / `helm template`; document any operator-only prerequisite.
- **Tests before hand-off**: `go build/vet/test` for both Go modules, `svelte-check`,
  `helm lint`, the Playwright suite for UI/API changes, and a live drill for anything
  touching the node (shares, LDAP, ZFS, backups).
- **Plans** live in `.kilo/plans/`; the per-topic docs in `docs/` are the reference
  material (`buddy-backup.md`, `shares.md`, `storage-zfs.md`, `api.md`,
  `deployment.md`, `operations.md`, `architecture.md`). Third-party attribution
  lives in [`CREDITS.md`](CREDITS.md).
- **Agent instructions** live in [`AGENTS.md`](AGENTS.md) (build/test/lint
  commands, the branch/PR workflow, the credits and spec rules). Keep it in sync
  with this file when a command or convention changes.
- **Record the deployed tags** below whenever they change, and keep this file short:
  new session narratives belong in the archive, not here.

## Audit (2026-09-19)

The full audit of `master` @ `85ae874` — code quality, security, secret use — and
everything fixed since is in **`docs/AUDIT-2026-09-19-REPORT.md`** (the detailed
findings, fix plan and code-review list are in `docs/archive/`). Headline: 0
Critical, 4 High, 14 Medium, 10 Low; **all four Highs are fixed**, 10 Mediums
are closed (M4/M6 included: Cilium enforces the policies and the privileged
workloads are split into `naslos-privileged`), M4/M3 are reduced to documented
Low residuals, M10 is excluded and **M12 (no CI) is closed**: `ci.yml` runs the
gate sweep on every PR/push and `release.yml` publishes images + the
digest-pinned pack on a `vX.Y.Z` tag, and the
NAS-010 default-credential work is closed, including the live LDAP service and
admin rotations. The auth model verifies sound (anonymous 302, pod without the
secret 401, agent without the token 401, no NodePort, no bypass). The report's §8
lists what remains: the M4 hostNetwork agent `:9090` residual, Authelia's
`jwt_secret` in the ConfigMap (the LDAP bind password moved to a Secret mount at
revision 41), the recorded `trivy config` items, and the upstream trixie
base-CVE backlog. No secret values are in the report; the repository is private
(unauthenticated GitHub API returns 404), so the committed credentials that were
found were insider-exposure, not internet-exposure.

## Base-domain choice + runtime SSO promotion (2026-09-27) — branch, live-drilled

Branch `fix/app-base-domain-choice` (PR #35). Plan:
`.kilo/plans/1790459894361-app-base-domain-choice.md`.

- **Exposure base-domain choice (FR-APP-10).** `GET /api/domains` returns
  `selectableDomains`/`ssoDomains`; the exposure GET returns the record's own
  `baseDomain` (was hard-coded to the primary, so saving silently reverted a
  chosen domain) plus `primaryDomain`, `selectableDomains`, `ssoDomains` and a
  correct `authAllowed`; install/`PUT exposure` reject an unconfigured
  `baseDomain`. UI: the exposure form's static suffix is a domain `<select>`.
- **Runtime SSO promotion (FR-APP-15).** A registered non-primary domain can be
  promoted from the Domains page: `POST /api/domains/{domain}/sso {enabled}`.
  `certs.Domain.sso` is persisted; the effective SSO list is primary + store
  flags + the chart `SSO_DOMAINS` floor. The API renders the session cookies and
  access-control rules into the `naslos-authelia-sso` ConfigMap (mounted at
  `/config-sso`, injected via Authelia's `fileContent`) and restarts Authelia by
  deleting `naslos-authelia-0` (no workload-write RBAC). It also renders a portal
  IngressRoute per promoted domain in `naslos-apps` (ExternalName alias to the
  Authelia Service, the domain's wildcard TLS Secret) because Authelia's session
  cookie is per domain — without it the login redirect to
  `https://<promoted-domain>/authelia/` 404s. Traefik needs
  `providers.kubernetesCRD.allowExternalNameServices` (set).
- **Authelia is now a StatefulSet** (was a DaemonSet) so the restart target is
  the deterministic `naslos-authelia-0`; `make install-vm` deletes the legacy
  DaemonSet first so the two never share the session PVC.
- **Live-drilled on revision 38** (`naslos-api 0.1.0-r32`, `naslos-ui
  0.1.0-r23`): promoted `florentinrichard.fr`, Authelia restarted, its portal
  served 200 on the promoted domain, an app installed there with auth got the
  `302 → https://florentinrichard.fr/authelia/?rd=…` challenge with a
  `domain=florentinrichard.fr` session cookie; demotion with the app installed
  was refused 409; a `helm upgrade` and an API restart kept the promoted state
  (no Authelia restart). Then uninstalled the app, demoted, and confirmed the
  fragments and portal route were removed. Playwright `apps.spec.ts` +
  `domains.spec.ts` 14/14 (the install test needed a longer timeout: a cold
  catalog install is a synchronous image pull).

## App install progress jobs (2026-09-27) — branch, deployed + live-drilled

Branch `feature/app-install-progress`. Plan:
`.kilo/plans/1790503400805-app-install-progress-jobs.md`.

- **Async lifecycle (FR-APP-18).** `POST /api/apps`, `PUT /api/apps/{name}` and
  `DELETE /api/apps/{name}` now validate, enqueue and answer
  `202 {jobId, state:"running"}`; the operation runs under a server-owned
  `context.Background()`, so a closed modal or a navigated-away page can no
  longer cancel a cold install (which used to pass `r.Context()` through). New
  `GET /api/apps/jobs` and `GET /api/apps/jobs/{id}` report
  `state`/`stage`/`message`/`error`; a second job for an app already running one
  is `409`. Jobs are in-memory (`api/internal/server/app_jobs.go`, mirroring the
  buddy job runner): a restart abandons a running job and `reconcileApps`
  re-converges records/routes.
- `api/internal/apps` gains `InstallWithProgress`/`UpgradeWithProgress`/
  `UninstallWithProgress` (stages `preparing`/`installing`/`finalizing`);
  `Install`/`Upgrade`/`Uninstall` are thin wrappers. `api/internal/helm` is
  unchanged (`Wait=true`, 5m timeout).
- UI: shared store `ui/src/lib/stores/appJobs.ts` (one 1 s poller while any job
  runs), global `AppJobsDrawer.svelte` + `Toast.svelte` mounted in
  `+layout.svelte`; install/configure/uninstall components enqueue, track and
  close immediately.
- Docs: `docs/spec.md` FR-APP-18, `docs/api.md`, `docs/app-catalog.md`.
- **Deployed at revision 39** (`naslos-api 0.1.0-r33`, `naslos-ui 0.1.0-r24`)
  and live-drilled: an install through the UI closed the modal immediately, the
  drawer showed `preparing`/`installing`/`finalizing`, the success toast fired
  and the app landed `running` in the Installed tab; the async uninstall job
  reached `succeeded` and removed the release, route and record. Playwright
  `apps.spec.ts` 8/8 (the install test now polls the 202 job).

## Desktop installer — install pack + chart refactor (2026-09-27) — branch, not deployed

Branch `feature/install-pack` (PR #39). Plan:
`.kilo/plans/1790466900440-desktop-installer-app.md`; contract:
`docs/installer-contract.md`; spec `FR-INSTALL` (§3.10).

- **OpenLDAP is now part of the umbrella chart.** The old
  `openldap/manifests/*` (StatefulSet, bootstrap Job, backup CronJob) and
  `openldap/generate-secrets.sh` are gone; `charts/naslos/templates/openldap-*`
  render them from `openldap.*` values. `deploy-vm.sh` no longer applies them.
  The bootstrap Job is a `post-install,post-upgrade` hook (a Job's pod template
  is immutable) and the backup CronJob is a `CronJob`.
- **Chart-generated LDAP Secrets** (`templates/openldap-secrets.yaml`):
  lookup-guarded, so a first `helm install` needs no out-of-band step and an
  upgrade re-emits the existing bytes (never re-keys OpenLDAP). The Authelia
  config no longer `fail`s when the Secret is missing.
- `make install`/`install-vm` add `--take-ownership` (Helm 4) to adopt the
  pre-existing namespaces and the old kubectl-created OpenLDAP Secrets.
- **`charts/naslos/values-installer.yaml`**: public image base
  (`ghcr.io/aessemops/naslos-*`), no private registry; the installer overrides
  domain/discovery/CIDRs at install time.
- **`make install-pack`** (`scripts/build-install-pack.sh`) builds
  `dist/naslos-install-pack-<version>.tar.gz` + `.sha256`: chart (+vendored
  subcharts), `values-installer.yaml`, the parameterised machine-config template
  (`bootstrap/installer/naslos-installer.yaml.tmpl`,
  `scripts/render-installer-template.sh`), Cilium, the pinned local-path
  manifest, the schematic, and a `metadata.json` with per-member sha256 + the
  ISO URL. `.github/workflows/release.yml` attaches it to a `vX.Y.Z` release.
- **Not deployed**: the live cluster is unchanged (revision 39). A live drill is
  needed before this is called done: `helm upgrade` must adopt the existing
  OpenLDAP Secrets/StatefulSet without re-keying, then the normal gates.
- **Next**: build `Naslos-Installer` against this pack (engine, Tauri shell,
  recovery ZIP, resolver). Spike first: confirm a CLI-generated TOTP device
  (`authelia storage user totp generate`) is accepted at the portal.
- **Installer repo progress** (2026-09-27): `AessemOps/Naslos-Installer` is
  bootstrapped on `main` (`f9fb079`) with the Go engine foundation; open
  PR [#1](https://github.com/AessemOps/Naslos-Installer/pull/1) adds Talos
  PKI/config generation and the lifecycle client. See that repo's `AI_HANDOFF.md`
  for state, spike results and the exact next steps. The TOTP CLI command was
  validated live (generate/delete with a throwaway user; admin untouched).
- **CI + GHCR releases (FR-INSTALL-13).** `.github/workflows/ci.yml` runs the
  full `scripts/audit.sh` sweep (security tools installed), gitleaks, a no-push
  Docker build of all 8 images and a pack build on every PR/push. On a strict
  `vX.Y.Z` tag, `.github/workflows/release.yml` guards the version, pushes all 8
  images to `ghcr.io/aessemops/naslos-*` (linux/amd64), builds the pack with
  every chart image **pinned by digest** (`scripts/pin-installer-values.py` +
  `metadata.json.images`), attaches it and dispatches the installer. A local
  `make install-pack` keeps the tag placeholders.
  **Released: `v0.1.0`**: all 8 images are `ghcr.io/aessemops/naslos-*:0.1.0`
  (public, anonymous pull verified) and `naslos-install-pack-0.1.0.tar.gz` is
  attached with all 7 chart images digest-pinned. The cross-repo chain is live:
  the `INSTALLER_DISPATCH_TOKEN` secret is configured and the installer pipeline
  is landed on `AessemOps/Naslos-Installer` `main` (PR #2), so attaching a pack
  dispatches a rebuild — `naslos-v0.1.0` is released with the engine for
  linux/darwin (amd64+arm64) and windows/amd64 built against the pack. Releases
  are **immutable**, so a re-run keeps the published pack and only re-dispatches
  (the workflow creates a release only when missing and never replaces assets).
  The old residual (tag-pinned placeholders / NAS-022) is
  closed for released packs. Turning the gate on also surfaced and fixed two
  pre-existing issues: stale reachable advisories (go-git / x-crypto / go-billy
  bumped to the fixed versions) and a random share order in
  `GenerateSambaConfig` that flaked the shares test.

## Deployed right now (2026-09-27)

On `192.168.1.117`, chart `naslos-0.1.0`, **helm revision 39**:
`naslos-api` **`0.1.0-r33`** (async app lifecycle jobs with progress, FR-APP-18;
exposure base-domain choice, runtime SSO promotion + per-domain Authelia portals,
declarative DNS providers + Dynamic DNS, OVH webhook solver), `naslos-ui`
**`0.1.0-r24`** (job drawer + completion toasts; domain-select exposure form,
Domains-page SSO toggle, cert badge, sign-out), `naslos-agent`
**`0.1.0-r8`**, `naslos-samba`/`naslos-nfs`/`naslos-terminal` **`0.1.0-r3`**,
OpenLDAP per `values.yaml`. **Authelia is a StatefulSet** (`naslos-authelia-0`)
and the `naslos-authelia-sso` fragments ConfigMap exists (API-owned,
`keep`). Talos **v1.14.1** (kernel 6.18.51-talos), Cilium v1.20.2, ZFS pool
`test` (stripe, 79 G) + `test/drill`, `test/naslos-buddy`.

Namespaces: `naslos` (authenticated services), `naslos-privileged`
(agent/samba/nfs/terminal), `naslos-apps` (installed apps, PSA baseline,
`naslos-datasets` RWX PVC), `naslos-apps-priv` (privileged apps), and
**`cert-manager`** (cert-manager v1.18.2 + `cert-manager-webhook-ovh` 0.9.17,
installed by `make install-vm`; deliberately outside the naslos default-deny
policies). Posture is the
**only one**: Traefik v3.7.13 (chart 41.6.0) on hostPort 80/443, Authelia
4.39.24 (chart 0.11.22) at `https://naslos.local/authelia`, no NodePort and no
auth bypass, `admin` in `naslos_admins`.

Domains: `florentinrichard.fr` holds a live production Let's Encrypt wildcard
certificate (`*.florentinrichard.fr` + apex, Secret
`naslos-florentinrichard-fr-tls`) issued through the OVH webhook.

App catalog: official source `https://github.com/AessemOps/NaslosCharts.git`
(public, channels `{Prod: main}`) — catalog lists **10 apps** (Jellyfin, Radarr,
Sonarr, Seerr, FlareSolverr, Prowlarr, qBittorrent, Audiobookshelf, Calibre-Web,
SearXNG). **No app is installed** (the SSO drill's Jellyfin was uninstalled).
Merged into `master`: app catalog (PR #28), Dynamic DNS + providers (PR #29),
third-party credits (PR #30), ddns-updater credits + credits rule (PR #32), the
app-base-domain choice + runtime SSO promotion (PR #35, merged), and the
docs/README currency rules (PRs #36/#37). Merged: `feature/app-install-progress`
(PR #38, async app lifecycle jobs, FR-APP-18). Open: `feature/install-pack`
(PR #39, install pack + chart-owned OpenLDAP, not deployed). Merged:
`feature/domain-provider-api-rights` (PR #33) — declarative per-provider
`apiRights` surfaced by `GET /api/providers` and shown as an info bubble next to
the Domains form's DNS-01 provider selector, plus the fix that makes OVH
certificates actually issue: cert-manager and the `cert-manager-webhook-ovh`
webhook are installed as separate releases in the `cert-manager` namespace
(`make cert-manager`, `make cert-manager-webhook-ovh`), `ovh.yaml` renders the
`webhook` solver, and every OVH credential (application key/secret/consumer key)
is a Secret field read by the webhook. The same PR also adds a sidebar sign-out
control (POST `/authelia/api/logout`) and a self-refreshing certificate status
badge. **Live-drilled on revisions 29–32**: the OVH
Issuer is created and `READY=True`, and `florentinrichard.fr` issued a
production Let's Encrypt certificate (SANs `*.florentinrichard.fr` +
`florentinrichard.fr`, Secret `naslos-florentinrichard-fr-tls`) once the OVH API
token covered the zone. Residual: the token still lacks **DELETE** on
`/domain/zone/florentinrichard.fr/*`, so the webhook's `CleanUp` gets a 403 and
leaves the `_acme-challenge` TXT record in the zone (issuance is unaffected);
add DELETE for the four methods on the zone wildcard so cleanup succeeds. The
pre-refactor revision 55 narrative is archived at
`docs/archive/ai-handoff-log-2026-09.md`.
