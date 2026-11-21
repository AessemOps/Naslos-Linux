# AI Handoff — Naslos

**This is the current deployed state plus the operational gotchas, kept
short.** Historical session narratives are in [`docs/archive/`](docs/archive/);
the pre-cleanup handoff snapshot is
[`docs/archive/ai-handoff-log-2026-11-08.md`](docs/archive/ai-handoff-log-2026-11-08.md).
Normative requirements are in [`docs/spec.md`](docs/spec.md); build/test/deploy
rules and conventions are in [`AGENTS.md`](AGENTS.md).

## What this is

A single-node NAS appliance on Talos Linux (Kubernetes) with a web UI.

## Shipped — UI/UX overhaul (v0.4.1, 2026-11-20, was `feature/ui-ux-overhaul` PR #181)

The audited overhaul from
[`.kilo/plans/2026-11-20-ui-ux-review-and-overhaul.md`](.kilo/plans/2026-11-20-ui-ux-review-and-overhaul.md)
shipped in **v0.4.1**: a CSS-variable token layer with a
persisted light/dark theme (pre-paint bootstrap in `app.html`, toggle in the
sidebar footer and an Appearance card on `/settings`), ~34 hand-adapted Lucide
icons replacing every emoji, a grouped collapsible sidebar (longest-prefix
active match, `naslos:sidebar` persistence), shared `Modal`/`LoadingState`/
`EmptyState`/`ErrorState`/`StatusBadge` components (the 7 raw modals converted
or given `use:modal`), a `naslos.link` ink token because the brand primary
fails AA as dark-theme text, and an axe a11y spec (`@axe-core/playwright`
4.14.0, dev-only) scanning all 18 routes in both themes. Drilled green on the
`sha-90d24d3` build (revision 41, 65 passed), then **released as v0.4.1**
(2026-11-20): the version bump, tag and pack landed on master, all nine
images are digest-pinned from the release, and the node runs the release
(revision 44). Suite on the release: **67 passed, 9 skipped, 0 failed** —
the network API-contract tests now RUN (the 0.4.1 API serves `/api/network`)
and the vms capability-skips remain while the supervisor is parked (see
below).

## Live and drilled — QEMU hypervisor (v0.4.2; platform fixes on `fix/agent-mount-propagation`)

The full FR-VMT feature shipped in v0.4.0/v0.4.1 and is **enabled and drilled
end-to-end on the node** since v0.4.2 fixed the vmd startup panic (an explicit
rustls ring provider — the crate graph compiles both providers and never
installed one). `values-vm.yaml` sets `virt.enabled: true`; the supervisor
runs as the ninth image. The operator drill from docs/virtual-machines.md is
automated in `ui/vmd-drill.mjs` (freshen the session with the `setup` project
first) and **passed in full on 2026-11-20**: capabilities (KVM + NICs),
SSRF-refused private import (TEST-NET-1), 204 MB cloud-image import,
create/start (console preflight answers `vncPort` + per-boot password — the
listing never carries console credentials by design), graceful ACPI stop,
snapshot + restore while stopped, restore refused 409 while running,
force-stop, recursive delete, image cleanup.

The drill exposed **four platform defects**, all fixed on the branch
`fix/agent-mount-propagation` (deployed as drill builds, pending v0.4.3):

1. **Mount propagation** — the agent's host-root bind was `HostToContainer`,
   so chroot-created dataset mounts stayed pod-side; the chart bind is now
   Bidirectional (gotcha #5).
2. **vmd reconcile cancellation** — axum dropped the handler future when the
   client disconnected, aborting synchronous reconciles mid-flight; `apply()`
   now installs the config synchronously and spawns the reconcile detached
   (`rust/vmd/src/supervisor.rs`, test
   `apply_returns_before_reconcile_completes`).
3. **Stale agent hotfix command** — the live DaemonSet still executed
   `/hotfix-bin/naslos-agent` from an ancient kubectl patch that survived
   every helm upgrade (gotcha #15's class, second occurrence); the chart now
   pins `command: ["/app/naslos-agent"]` so the field is managed and any
   future out-of-band patch self-reverts. The leftover volume/mount was
   patched away once; `/var/naslos-hotfix` on the host is inert.
4. **VM delete was not recursive** — a VM dataset owns its snapshots, so a
   flat destroy failed with "filesystem has children" after any snapshot;
   `vm_detail_delete` now destroys recursively (regression test in
   `rust/api/tests/zfs_agent.rs`).

The same session surfaced and fixed **three buddy defects** (see the same
branch): self-send transport (loopback rewrite, gotcha #17), the replay-window
budget (gotcha #18), and the UI drills' dataset choice. The branch also adds
upstream image release detection for installed apps (FR-APP-27: registry
tag→digest drift, `new image` badge, forced re-pull on update). The branch needs
a **v0.4.3 release** to ship all of it.

## Shipped — MCP server (drilled on the node; PR #187, `feature/mcp-server`)

The MCP endpoint (spec `FR-MCP-01…07`, `SEC-17`; guide `docs/mcp.md`) is
implemented and **drilled end-to-end on the node** (2026-11-21): a stateless
Streamable HTTP server at `/mcp` **inside the `naslos-api` pod** — no new
image, Service or NetworkPolicy. Off by default (404 while disabled);
Traefik `Host && PathPrefix(/mcp)` without forwardAuth + the Authelia bypass
(placed before the `naslos_admins` two_factor rule); the chart-level
`mcp.available` kill switch removes both. Bearer tokens (`nmcp_…`,
SHA-256-hashed, constant-time compare, one-time plaintext reveal, optional
expiry, revocation) carry scopes `read < write < full`; `full` = admin
without 2FA. ~128 tools (declarative registry, one file per domain in
`rust/api/src/mcp/tools/`) dispatch through `internal_router` — the real
owner/user handlers with a synthetic `mcp:<token>` identity; the internal
router is never listener-mounted (regression tests sweep every route's auth
and a forged identity cannot bypass the layers). Destructive tools require
`confirm == the resolved target` before dispatch. Every call is audited
(`mcp_tool_call` line + the Settings recent-calls feed; arguments never
logged). `build_router` now composes `owner_routes()`/`user_routes()`.
Live drill: guard chain (401/Bearer challenge, foreign Origin 403, disabled
404, 10-fail backoff), initialize (serverInfo `naslos 0.4.4` from the
release facts), read-token `tools/list` (62 tools, all read-only) and
`tools/call` (dashboard/pool live data, `pod_logs` bounded snapshot),
scope and confirm refusals, token create/reveal-once/revoke through the UI
(the new Settings → MCP page, `bot` icon), `mcp.json` persistence across
restarts (lastUsed debounced flush survived the deploys). Node left with
MCP **disabled** and no drill tokens; the Playwright `mcp.spec.ts`
cleanes up after itself. Suite after the drill: **78 passed, 5 skipped,
0 failed** — includes the a11y spec, which surfaced a pre-existing master
defect now fixed in this branch: the installed-apps update badge's
aria-label did not contain its visible text (`label-content-name-mismatch`
on /apps, first visible once the post-0.4.3 UI reached the node).
Observed while drilling (not caused by the deploy, nothing touches 6443):
the **workstation→apiserver LAN path flaked** with intermittent
"Couldn't get current server API group list" / raw `/apis` 404s while
in-cluster access stayed healthy and Talos services reported 337h uptime;
retrying or using the browser path worked around it. Worth watching.

## Production instance (2026-11-21)

- **Hardware:** the production NAS is the bare-metal node `talos-zgr-9yg` at
  `192.168.1.38` (Talos v1.14.5, Kubernetes v1.37.0, control-plane Ready). The
  old `192.168.1.117` VM is powered off. The recovery bundle imported into
  `bootstrap/vm/` points at `.38`; the `vm` names (`bootstrap/vm/*`,
  `values-vm.yaml`, `install-vm`, `deploy-vm.sh`) are historical paths for the
  single node, not a VM.
- **Deployed release:** Helm `naslos` chart **`naslos-0.5.1`**, revision 58
  (2026-11-21), deployed with `make install-vm`; the node runs the v0.5.1
  chart and release images, digest-pinned (public GHCR): API `e9a461fc…`,
  UI `418f1240…`, agent `de15ccba…`, OpenLDAP `2f6d7077…`, Samba
  `59088ce9…`, NFS `32cf601f…`, terminal `6c4fc507…`, vmd `fee36ced…`.
  `virt.enabled: true`. `/api/system/version` reports the `release.*`
  facts from `values-vm.yaml`: Naslos/chart **0.5.1**, profile
  `installer`, release Talos pin **v1.14.6** and k8s v1.37.0, while the
  running Talos is still **v1.14.5** (the Talos step has not been run).
  The web upgrader is enabled (SA `naslos-upgrader` + cluster-admin
  binding, the `naslos-api-system` Role/RoleBinding, and the upgrader
  admission policy). **Web-upgrader fix verified live:** `POST
  /api/system/updates/check` now returns `current`/`available` 0.5.1 with
  `talosVersion: v1.14.6`, `stepping.allowed: true`, so the UI shows the
  system upgrade and enables **Upgrade Talos**; before 0.5.1 it answered
  `no newer release` once Naslos was current. The same deploy corrected
  `values-vm.yaml`, which still advertised the release facts as 0.4.4
  (the release guard now covers `values-vm.yaml` tags and facts). The
  private registry `192.168.1.2:30095` is retired — do not build/push
  local images.
- **Latest release:** `v0.5.2` (2026-11-21) and installer `naslos-v0.5.2`
  are published; the pack `naslos-install-pack-0.5.2.tar.gz` is attached.
  It makes VM networking **NAT-only**: the macvtap LAN-attachment draft is
  removed (spec FR-VMT-04 keeps the Talos host-bridge path `[OPEN]`), and it
  resolves installed app status per namespace instead of a serial scan
  (#205, #206). `v0.5.1` (2026-11-21) fixed the web upgrader so a node that
  already runs the newest release
  can still complete the pending Talos/Kubernetes steps: the update check
  now offers the installed release while its system pins lag, and the UI
  labels that as a system upgrade. It also bumps the node overlay's
  release facts and folds `values-vm.yaml` into the release guard, so a
  stale overlay cannot make the upgrader misread the installed version.
  `v0.5.0` (2026-11-21) shipped the MCP
  server (PR #187), targeted Talos **v1.14.6** for fresh installs and web
  upgrades, and landed the Vite 8 / SvelteKit 3 / TypeScript 6 UI stack,
  the Tailwind 4 desktop wizard and the Rust `sha2` 0.11 / `getrandom` 0.4
  / `ed25519-dalek` 3 majors. Before that, `v0.4.4` (2026-11-21) shipped
  the Apps speed/catalog work (PR #185): Installed is the default
  tab and paints the last in-session list while revalidating, `GET /api/apps`
  resolves release status concurrently with one status-only Helm read per app
  instead of a serial two-call loop, and the catalog offers grid, list and
  compact layouts. Before that, `v0.4.3` (2026-11-20, PR #184) shipped
  upstream image-release detection for catalog apps plus the platform fixes
  merged since v0.4.2 (PR #183); `v0.4.2` (2026-11-20, PR #182: the vmd
  rustls provider fix, API uninstall tolerance, auth.setup TOTP window
  retry) shipped the same day. Earlier, `v0.4.1` (the UI/UX overhaul, PR
  #181) shipped the same day; its first release attempt failed the pack job
  because the release workflow raced CI (the green-CI gate saw `audit` still
  running); the packless tag was deleted and re-pushed once CI was green,
  per the documented rule. Full Playwright suite green on revision 50 with
  the drill builds: **71 passed, 5 skipped, 0 failed** — the skips are state
  preconditions (VM console tests with no VM present, link-dependent network
  mutations); the buddy self-send tests now run for real (the node's own
  identity is self-enrolled as a peer, scope `naslos/`). The vmd drill
  passed end-to-end (see the virt section above).
- **Deployed since 0.3.22:** per-app custom environment variables (FR-APP-26,
  #158; charts PR NaslosCharts#19/20) and automatic per-domain app CNAMEs
  pointing at a DDNS hostname (FR-DNS-09, #159/#161). Both live drills passed:
  env create/update/keep-secret/reserved-name/remove on `calibre-web`, and CNAME
  create/`409`/delete on OVH `shail.ovh` via a temporary `drill.shail.ovh`.
  Two OVH bugs were found and fixed during the drill: zone discovery must treat
  403 as an unowned zone and probe `/record?subDomain=` (the app key lacks the
  zone-root right). Also fixed the Helm 4 `--server-side=true` flags, the agent
  probe's invalid `insecureSkipTLSVerify`, and moved the Buddy store to
  `/var/mnt/ssd-storage/naslos-buddy`.
- **Release notes since 0.3.7:**
  - `0.5.2` (2026-11-21) — VM networking is NAT-only: the `macvtap` LAN mode,
  `nic` spec field and vmd netdev/tap hand-off are removed, and the wizard
  always renders port forwards. A Talos `BridgeConfig` host bridge remains the
  `[OPEN]` path for VMs that need a LAN address. Also speeds up the Apps page
  by resolving installed status per namespace (#205).
  - `0.5.1` (2026-11-21) — web-upgrader fix: the update check now offers the
    installed release while its Talos/Kubernetes pins are still pending, so a
    node that finished the Naslos step (0.5.0) can complete the pinned
    Talos v1.14.6 upgrade from the UI instead of reading "no newer release".
    The UI labels this case as a system upgrade, not a new release.
  - `0.5.0` (2026-11-21) — MCP server (spec FR-MCP/SEC-17) inside the API pod:
    a stateless Streamable HTTP endpoint at `/mcp` with scoped bearer tokens
    and a Settings → MCP page (PR #187). The release also moves the install
    target to Talos **v1.14.6** (talosctl pin and the Image Factory installer
    image; the running node stays on v1.14.5 until upgraded) and lands the
    dependency majors: Rust `sha2` 0.11, `getrandom` 0.4 and
    `ed25519-dalek` 3; UI `vite` 8 with `@sveltejs/vite-plugin-svelte` 7 and
    `@sveltejs/kit` 3 (`adapter-static` 4, TypeScript 6); the desktop wizard on
    Tailwind 4 and `vite` 8.
  - `0.4.4` (2026-11-21) — Apps speed and catalog views: the page opens on
    Installed, paints the last in-session list while revalidating, and
    `GET /api/apps` resolves release status concurrently with one
    status-only Helm read per app (8 at a time) instead of a serial
    two-call loop. The catalog adds grid, list, and compact layouts with
    the choice remembered in local storage, and the install form loads its
    domain data in the initial parallel request batch. PR #185.
  - `0.3.23` (2026-11-14) — per-app custom environment variables
    (`capabilities: [env]`, `naslos-env-<release>` Secret, checksum-driven
    rollout) and opt-in automatic app CNAMEs per domain (OVH ZoneDNS /
    Cloudflare, ownership/adoption/conflict rules, retries, 409 on referenced
    DDNS entries). Images now come only from public GHCR packages; the local
    registry is retired. Includes the OVH 403/apex-record zone-discovery fixes,
    the Helm 4 `--server-side=true` install fix, the agent probe cleanup, and
    the `/api/system/updates` disabled-shape fix. Live drills passed for both
    features; the full Playwright suite is green (61 passed, 2 skipped).
  - `0.3.22` (published, not yet applied) — audit remediation: Buddy-disabled
    installs answer 503 instead of aborting the API (#148); share paths reject
    symlinks (#149); notifications/DDNS pin the vetted address (#152); disabled
    accounts are excluded from Authelia's login (#154); share/user mutations
    report apply failures and roll back (#153); ZFS import requires a pool name
    (#151); Buddy KEK kept on uninstall, privileged quota from values, UI route
    pins the TLSOption (#150); `hickory-resolver` 0.26.3 (#147); audit report
    and remediation plan; handoff refreshed (#155). Catalog: Prowlarr's
    safer default is proposed in NaslosCharts #19 (still open, independent of
    the platform pack).
  - `0.3.21` (deployed) — per-path Authelia protection: an app with
    `exposure.auth: false` can keep selected prefixes behind Authelia via
    `protectedPaths` (a priority-100 `PathPrefix` route; the catch-all stays
    public). #145. Also enforces the per-app `ResourceQuota` on install and
    update (`validate_resource_quota`), so a chart whose requests/limits
    exceed `apps.isolation.quota` is rejected before Helm. #146.
  - `0.3.20` (deployed) — edit an installed app's resource requests/limits
    from the Installed drawer (`PUT /api/apps/{name}` values merge); the
    quota added in 0.3.21 validates it. #144.
  - `0.3.19` (deployed) — live app logs in the Installed drawer: the detail
    drawer streams a pod's logs (`GET /api/apps/{name}/pods` + the existing
    `/api/ws/logs`, tail 200 + follow) with pod/container pickers and a follow
    toggle; the API's per-namespace isolation ClusterRole gained `pods/log`
    `get` (audit-guarded). PR #142. Plan:
    `.kilo/plans/2026-11-10-app-drawer-live-logs.md`.
  - `0.3.18` (deployed) — owner-adjustable per-app namespace pool: the chart
    provisions 32 baseline slots but hands out 20 by default; Apps → Installed
    shows `used/limit` and raises or lowers the limit
    (`GET`/`PUT /api/apps/namespaces`, persisted in `APPS_POOL_CONFIG`). Also
    fixes the full-pool error text. PR #140. Plan:
    `.kilo/plans/2026-11-10-app-namespace-pool-limit.md`.
  - `0.3.17` (deployed) — fixes the 0.3.16 live-usage Forbidden
    error by granting read-only `pods` in the per-app isolation ClusterRole
    (bound by RoleBinding in each pool namespace only), guarded by an audit
    assertion. Same Installed rework as 0.3.16. PR #136.
  - `0.3.16` (deployed) — Apps → Installed rework: compact rows
    with live CPU/RAM/network from the kubelet `stats/summary`, catalog update
    badges with a one-click tracked upgrade, a right-side detail drawer with
    client-side sparklines, a background catalog refresh at `CHARTS_TTL`, and a
    read-only `naslos-api-node-metrics` ClusterRole/Binding. PR #134.
  - `0.3.15` (deployed) — a duplicate Install of an already-recorded app is
    rejected before namespace reservation with `app "<name>" is already
    installed; reconfigure or uninstall it instead`, instead of a misleading
    `no free per-app namespace` when the baseline pool is full. PR #133.
  - `0.3.14` (deployed) — app installs into a dedicated baseline namespace get
    their `<app>-internet` egress policy **before** the Helm wait (Bazarr and
    other apps whose first start calls the public network), with ownership
    labels, policy-only cleanup on failure, and an orphan sweep before
    namespace reuse and at startup. Public GHCR image references no longer trip
    gitleaks. PR #132. Plan:
    `.kilo/plans/2026-11-08-app-install-internet-egress.md`.
  - `0.3.13` (deployed) — SSO portal routes for promoted domains
    (FR-APP-15): `Manager::reconcile_routes` now calls
    `Router::reconcile_portals`, so `https://shail.ovh/authelia/` serves the
    Authelia portal instead of Traefik's 404 and Radarr/Sonarr (auth on) can
    redirect there. PR #131.
  - `0.3.12` (deployed) — numeric catalog values are coerced server-side
    (browser forms send PUID/PGID as strings), integer fields render as
    numbers, the install/configure forms offer a PGID group select
    (`naslos_users` 10000 / default 1000), and selecting a dataset defaults
    PUID/PGID to 10001/10000 so the app can read the dataset trees. PR #130.
  - `0.3.11` — multiple datasets per app (install and reconfigure),
    every attached dataset mounted at `/datasets/<name>` with a one-time
    `datasetsLayout: v2` migration, PVC quota 12, and the 12 dataset catalog
    charts bumped to `0.1.2`. The node had no dataset-carrying apps, so the
    migration had nothing to rewrite. Plan:
    `.kilo/plans/2026-11-08-multiple-app-datasets.md`.
  - `0.3.10` — public Traefik ingress
    (`networkPolicy.publicIngressCIDR`, needed for internet-facing app domains)
    and the SMB account-case fix.
  - `0.3.9` — per-app Internet access (default on) with a per-app
    `<app>-internet` NetworkPolicy; opt-out is enforced for isolated baseline
    apps. Plan: `.kilo/plans/2026-11-08-app-internet-access.md`.
  - `0.3.8` (superseded) — pool-backed app storage, `zd*` disk filtering, agent
    share-folder view, `naslos_users` supplemental group.
- **Storage:** `Data` (10.9T, `sdc` + `sdb` cache) is ONLINE with datasets under
  `/var/mnt/Data` (normalized from the old TrueNAS `/Data` tree); `ssd-storage`
  (238G, `nvme0n1`) hosts app data. Ownership is `admin:naslos_users`
  (10001:10000), directories setgid. The `naslos-pool-ssd-storage-*`
  provisioner/StorageClass remains from the storage-pool drill.
- **Apps/domains:** `searxng` (`naslos-app-1`, `search.shail.ovh`; 57-result
  live test, `searxng-internet`), `audiobookshelf` (`naslos-app-2`,
  `audio.shail.ovh`, `audiobooks`), `calibre-web` (`naslos-app-3`,
  `calibre.shail.ovh`, `ebooks`), `jellyfin` (`naslos-app-4`, `jelly.shail.ovh`,
  `animes`/`movies`/`series`/`ukrmovies`, wizard complete), `radarr`
  (`naslos-app-5`, `radarr.shail.ovh`, `movies`/`ukrmovies`), `sonarr`
  (`naslos-app-6`, `sonarr.shail.ovh`, `animes`/`series`), `flaresolverr`
  (`naslos-app-7`, `http://flaresolverr.naslos.local`, internal), `bazarr`
  (`naslos-app-8`, `https://bazarr.shail.ovh`,
  `animes`/`movies`/`series`/`ukrmovies`), `seerr` (`naslos-app-9`,
  `https://serve.shail.ovh`), `prowlarr` (`naslos-app-p2`,
  `https://prowlarr.shail.ovh`, privileged), and `qbittorrent`
  (`naslos-app-p1`, `https://qbit.shail.ovh`, privileged,
  `animes`/`downloads`/`movies`/`series`/`ukrmovies`). Every dataset app runs
  as `10001:10000` (PGID `naslos_users`); the LinuxServer apps needed a one-off
  `chown -R 10001:10000 /config` after the UID/GID switch. Datasets under
  `/var/mnt/Data` are recursively group-owned `naslos_users` (10000). The
  `shail.ovh` domain (OVH, production, `sso: true`) and
  `naslos-shail-ovh-tls` are present. The baseline pool has **32 provisioned
  slots with a usable limit of 20** (`GET /api/apps/namespaces`: 9 used, 11
  available); the privileged pool has 2 used of 4, `naslos-app-p3..p4` free.
- **Update check:** current deployed `0.3.23`; the GitHub check reports no
  newer release (the `v0.3.22` offer was resolved by deploying 0.3.23).

### Open items

- **App upgrades blocked by a Helm field-manager conflict (fixed on this
  branch, needs a release):** the image refresh's `kubectl patch` owned
  `imagePullPolicy`, so later app upgrades and their rollbacks failed
  (gotcha #19). App install/upgrade now pass `--force-conflicts`; ProjectSend,
  Sonarr and Prowlarr were repaired live on 2026-11-21 with the fixed flags
  (revisions 8, 9 and 12) and are deployed again. The node's API still
  predates the fix, so the next release must ship it before the next
  image-refresh cycle re-creates the conflict.

- **Audit 2026-11-11:** the full read-only audit is
  [`docs/AUDIT-2026-11-11.md`](docs/AUDIT-2026-11-11.md) with the ordered fix
  plan in [`.kilo/plans/2026-11-11-audit-remediation.md`](.kilo/plans/2026-11-11-audit-remediation.md).
  Open remediation PRs on the branch series: #148 (Buddy-disabled API crash),
  #149 (share symlinks), #150 (chart hardening), #151 (ZFS import body),
  #152 (SSRF pinning), #153 (apply failures), #154 (disabled accounts), plus
  NaslosCharts #19 (Prowlarr exposure default). The live node still runs
  0.3.21, so `AUD-2611-T6-1` (Buddy-disabled crash) remains live until the
  next release; do not probe `/api/buddy/v1/*` before then.

- **Baseline namespace pool:** 20 usable of 32 provisioned slots after the
  2026-11-10 release. The limit is owner-editable in **Apps → Installed**
  (`PUT /api/apps/namespaces`); a limit above the provisioned count still needs
  a chart change (`apps.isolation.namespaces`) and a platform upgrade.
  Privileged apps still have `naslos-app-p2..p4`.
- **Multi-dataset live drill:** six apps now mount datasets (Jellyfin and Sonarr
  have several), which exercises the per-dataset claims and
  `/datasets/<name>` mounts. Still to verify: remove one dataset from an
  installed app and confirm its claim and registry attachment are cleaned up,
  and attach a second dataset to an existing app.
- **SSO portal:** verified live after 0.3.13 — `https://shail.ovh/authelia/`
  returns the Authelia page and `radarr.shail.ovh`/`sonarr.shail.ovh` redirect
  there (`rd` back to the app). Log in and confirm the full forwardAuth round
  trip.
- **UI cache after upgrades:** `ui/nginx.conf` sends no `Cache-Control` for
  `index.html`, so an already-open SPA tab keeps the old bundle until a hard
  reload. Add `index.html: no-cache` plus immutable caching for
  `_app/immutable` in a follow-up release.
- **Audiobookshelf setup:** the old `/var/mnt/Data/ix-apps` tree is empty
  (0 files), so no old database/users were imported; the app starts fresh and
  the library path is `/datasets/audiobooks/AudioBooks`.
- **SMB account-case fix** is deployed (`v0.3.10`, PR #123); the live passdb
  was remediated manually on 2026-11-08.
- **External ingress:** the OVH router forwards 80/443 to `192.168.1.38`
  (verified by packet capture: public SYNs arrive). The durable fix
  (`networkPolicy.publicIngressCIDR: 0.0.0.0/0`) is deployed in `v0.3.10`;
  `https://search.shail.ovh/` is reachable externally.
- **Playwright** was rerun on 2026-11-20 against the v0.4.1 release
  (revision 44, all images released digests): **67 passed, 9 skipped,
  0 failed** — the full suite plus the `tests/a11y.spec.ts` axe scans
  (18 routes × both themes); the network API-contract tests now run against
  the 0.4.1 API, and the skips are the vms capability-skips (supervisor
  parked) plus the link-dependent network mutations. The same day's drill
  build (revision 41, `sha-90d24d3`) was 65 passed with the network contract
  test skipping against the then-0.3.23 API. A
  keyboard-only drill passed: sidebar collapse/expand via the toggle, modal
  focus trap + Escape + focus restoration, theme toggle `aria-pressed` round
  trip.
- **v0.4.1 release-drill defects (fixed on `fix/release-drill-defects`,
  PR open):** (1) the released vmd panicked at its first TLS handshake —
  both rustls crypto providers compiled, none installed; the node runs
  `virt.enabled: false` until the fix ships. (2) The API's uninstall treated
  a missing Helm release as fatal and stranded the app record — observed as
  `actual-budget` 409-blocking reinstall after a morning test cycle; the
  stale record was removed from `/var/lib/naslos/apps.json` inside the API
  pod (12 records remain) and the API restarted to reload it, which
  unblocked the apps spec. (3) `tests/auth.setup.ts` burned its first TOTP
  code when a run started in the same 30 s window as a previous login and
  Authelia's rejection notice auto-dismissed before it was read — two
  back-to-back suite runs failed at setup this way; space runs by one
  window until the setup hardening merges.

## How to operate

```bash
# Gates (run inside the repo; there is no make test/lint)
cd rust && cargo fmt --all -- --check
cd rust && cargo clippy -p naslos-api --all-targets -- -D warnings
cd rust && cargo test -p naslos-api
cd rust && cargo test -p naslos-agent
cd rust && cargo test -p naslos-vmd
cd ui   && npm run check && npm run build
helm lint charts/naslos -f charts/naslos/values.yaml -f charts/naslos/values-vm.yaml
helm lint charts/naslos -f charts/naslos/values.yaml -f charts/naslos/values-installer.yaml
sh scripts/audit.sh
```

- **Deploy:** `make install-vm` is the historical target name for the single
  node. It renders `-f values.yaml -f values-vm.yaml` (not `--reuse-values`),
  installs `crds`, `cert-manager` and the OVH webhook first, and targets the
  current kubeconfig (`bootstrap/vm/kubeconfig`, endpoint `192.168.1.38`).
  Always bump image tags; the registry reuses tags and the chart pulls with
  `IfNotPresent`.
- **Verify:** `kubectl -n naslos rollout status deploy/naslos-api` and
  `curl -sk https://naslos.local/api/health` (`naslos.local` resolves to
  `192.168.1.38`).
- **Cluster access:** `export KUBECONFIG=bootstrap/vm/kubeconfig` and
  `export TALOSCONFIG=bootstrap/vm/talosconfig` (the files point at `.38`).
- **API access:** the API is owner-gated and restricted to the proxy/pod CIDR.
  Query it from a trusted pod (e.g.
  `kubectl -n naslos-privileged exec deploy/naslos-terminal`) with
  `Remote-User`, `Remote-Groups` and `X-Naslos-Proxy-Secret`, not from the
  workstation.

## Operational gotchas (hard-won — read before drilling)

1. **Always bump image tag suffixes.** The registry reuses tags and the chart
   pulls with `IfNotPresent`, so a retag alone can keep running old code
   (NAS-022). Digests are supported (`api.image.digest`, `make image-digests`);
   a stored digest wins over a new tag until you clear it
   (`--set api.image.digest=`).
2. **`helm upgrade --reuse-values` ignores `-f` files.** Anything new must be
   passed with `--set` (e.g. `ingress.enabled`), or the release keeps old values
   and templates that dereference new keys can fail to render.
3. **There is one posture and it is authenticated.** Traefik on hostPort 80/443
   with Authelia forwardAuth; no NodePort and no auth bypass exist. `make
   install-vm` installs it; there is no unauthenticated listener to fall back to
   (NAS-008 resolved).
4. **The buddy identity is the KEK.** `/var/lib/naslos/buddy-identity.json` holds
   the private key and the key-encryption key: losing it makes every stored
   backup unreadable, and `helm uninstall` would delete it (CR-02). Back it up
   separately.
5. **Platform mount-propagation trap (root cause found and fixed
   2026-11-20).** The agent runs zfs via `chroot /host`, and its host-root
   bind was `HostToContainer` — so every dataset created through the agent
   mounted only in the agent pod's namespace: the host saw a plain directory
   on the parent, shares of new datasets served pool-root bytes instead of
   the dataset (violating FR-SHR-01), and vmd's `wait_for_dir` timed out
   after 60 s on every VM create. The chart now declares the agent's
   host-root bind **Bidirectional**, so chroot-created mounts propagate up
   to the host and on to every HostToContainer consumer. Datasets created
   while a pre-fix agent ran (anything created after the last host mount
   event, not just VM datasets) still need one convergence: run `zfs mount
   -a` on the host (a privileged nsenter one-shot) or reboot. `zfs send`
   guards (`requireMountedDataset`, UsedBytes) remain in place for any
   dataset still mounted pod-side.
6. **The receive dataset's ownership is fixed by the
   `fix-receive-dataset-ownership` init container** (uid 65532). Manual
   fallback: `chown 65532:65532 /var/mnt/<pool>/naslos-buddy`.
7. **Store-root dot files matter:** `.enroll-used` records a spent enrollment
   token (delete it to re-arm the token), `.nonces` is the persisted replay
   cache. Both are dot-prefixed so quota accounting ignores them; wiping `k*`
   directories clears the stored backups.
8. **Quotas are per key and count *sealed* bytes**, and `MinPeerQuota` is one
   sealed chunk (`MaxSealedChunkSize`), not one plain chunk.
9. **The Playwright suite runs against the live node and can skip** when the
   identity, a dataset or a peer scope is missing: treat a skip as a setup gap,
   not a pass (CR-23). It authenticates through Authelia (credentials in the
   gitignored `ui/.env.playwright.local`) and runs against
   `https://naslos.local`.
10. **A dataset once mounted in a pod namespace can answer `dataset is busy`.**
    `zfs receive` runs through the privileged agent, so the destination stays
    mounted in the **agent's** mount namespace (the terminal pod's namespace
    does not see that mount). `DestroyDataset` now recovers on its own: on a
    busy failure it forces `zfs unmount -f`, then clears
    `mountpoint=none,canmount=off` and retries. Only a residual leaked-kernel
    reference (no mounts, no holds, no snapshots) still needs a pool
    export/import, which the agent deliberately does not do; the error names
    that state. `kubectl -n naslos-privileged rollout restart ds/naslos-agent`
    remains a manual fallback.
11. **PSA is enforced per namespace.** `naslos` and `naslos-privileged` are
    `privileged`; the shared `naslos-apps` is **`baseline`** and
    `naslos-apps-priv` is `privileged`; the per-app pools `naslos-app-1..32` are
    `baseline` and `naslos-app-p1..4` are `privileged`. Baseline **forbids
    `hostPath` volumes**, so managed app charts mount the platform's
    `naslos-datasets` RWX PVC; `hostPath` and `NET_ADMIN` only work in the
    privileged namespaces (a chart opts in with `privileged: true`).
12. **The node has Internet; platform pods do not, by default.**
    `naslos-workload-egress` is default-deny, so the API cannot clone a chart
    repo until `networkPolicy.gitEgress: true` (on in the node overlay; ports
    443/22/9418). Image pulls are kubelet traffic and unaffected by pod policy.
    **Apps are the exception:** per-app exposure defaults `internet: true` and
    the API applies `<app>-internet` in the app namespace (0.3.9+). Turning
    "Internet access" off is enforced only for a baseline app in its own
    isolated namespace; privileged and legacy shared namespaces always allow
    Internet and reject the opt-out. **First installs need egress before the
    wait:** the isolation policy is default-deny and the app policy used to be
    reconciled only after a successful install, so an app that blocks on
    startup network calls (Bazarr fetches releases and announcements) could
    never become ready and Helm always rolled it back. The fix (0.3.14+,
    PR #132) applies the egress policy before the Helm wait, labels it
    (`naslos.local/managed-by=naslos-api`), deletes only that policy on
    failure, and sweeps labeled policies with no record before a reserved
    namespace is reused and at startup.
13. **Channels are a source property, and authoritative.** A source that
    declares channels uses exactly those (no defaults merge). NaslosCharts has
    only `main`, so the node overlay sets `apps.officialSource.channels: {Prod:
    main}`; a channel pointing at a missing branch reports a per-source error.
14. **`make install-vm` renders the `-f` files; it is not `--reuse-values`.**
    New keys in `values*.yaml` apply, but the official source is seeded only
    when it is absent from `sources.json` — to change its URL/channels/auth,
    delete it (`DELETE /api/sources/naslos`) and restart the API so it re-seeds
    from env.
15. **Live hotfixes survive Helm upgrades — second occurrence found
    2026-11-20.** Helm only reverts fields its manifests manage, so an
    out-of-band `kubectl patch` outlives every upgrade. First occurrence
    (0.3.9, removed 2026-11-08): the API kept the `/hotfix-bin/hotfix/naslos-api`
    command and the UI served the old nginx root. Second occurrence (found by
    the VM snapshot-restore 404s): the **agent** DaemonSet still executed
    `/hotfix-bin/naslos-agent` from a `hostPath: /var/naslos-hotfix` patch — a
    pre-virt binary — while the image was correct. The chart now pins
    `command: ["/app/naslos-agent"]` so the field is helm-managed and any future
    command patch self-reverts; the stale volume/mount were patched away once
    and `/var/naslos-hotfix` on the host is inert. If an upgrade behaves like
    the old version, check the Deployment/DaemonSet `command` and extra
    volumes first. Never ship a fix as a live `kubectl patch`; put it in the
    image/chart.
16. **Wait for Authelia after `make install-vm` before running Playwright.**
    Every helm upgrade restarts OpenLDAP, and Authelia's startup check treats
    an unreachable LDAP backend as fatal (`dial tcp …:636: no route to host`
    → exit 1), so `naslos-authelia-0` crash-loops for one backoff cycle until
    OpenLDAP answers. During that window the portal challenge is flaky and
    `tests/auth.setup.ts` fails fast with `expected the Authelia portal, landed
    on https://naslos.local/`. Settling takes ~1 minute; check
    `kubectl -n naslos get pod naslos-authelia-0` shows `Ready` with a stable
    restart count before trusting a suite result. Authelia has no
    wait-for-ldap init container (the API does); adding one is a chart
    improvement, not urgent.
17. **Cilium hostPorts are not reachable from pods on the same node** (found
    2026-11-20 via the buddy self-send). A pod dialing the node's own
    `https://<node>` (:443, Traefik's hostPort) times out at TCP connect —
    hostNetwork ports (agent :9090, vmd :9091) answer fine, so the difference
    is the hostPort plumbing, not the egress policy (even policy-free pods
    time out). The buddy sender therefore rewrites receiver URLs that resolve
    back to the node (host literal or DNS match against `NASLOS_NODE_IP`
    from the downward API; only the 443/80 entryports) to the API's own
    loopback listener, which serves the same `/api/buddy/v1/*` routes with
    the same Ed25519 auth — signatures cover the path, never the host. Any
    future in-cluster consumer of the node's public URL needs the same
    loopback/service treatment; never "fix" this by opening netpols (the
    drop is below the policy layer).
18. **The buddy replay window is a rate budget, not just replay protection**
    (found 2026-11-20 by a multi-GiB self-send). Every signed request holds a
    per-key nonce slot for 10 minutes, never evicted early; beyond the cap the
    receiver answers 401 "retry after the replay window", which the sender
    now honors with a fresh nonce and backoff (cap raised to 65536 ≈ LAN-rate
    chunk streams; sustained ceiling ≈ cap/600s requests per key). Two
    same-key sends run concurrently at their combined budget. The refusal
    wording is contract — the sender keys its retry on it.
19. **Helm owns app objects with server-side apply; a foreign field manager
    wedges every later upgrade** (found 2026-11-21). Helm v4 applies release
    objects server-side. The image-refresh pass (FR-APP-27) separately patches
    `imagePullPolicy` on the drifted container with `kubectl patch`, so the
    Deployment has a `kubectl-patch` field manager owning that field. The next
    `helm upgrade` — and its automatic rollback — fails with `Apply failed with
    1 conflict: conflict with "kubectl-patch" … imagePullPolicy`, leaving the
    release `failed` while the pods keep running the last deployed revision
    (ProjectSend, Sonarr, Prowlarr on 2026-11-21; the app job log only showed
    the minimised Helm conflict plus harmless `restricted:latest` PSA warnings
    from the baseline namespaces). Install/upgrade now pass `--force-conflicts`
    (Helm owns the app's objects) and use `--rollback-on-failure` instead of
    the deprecated `--atomic` (`rust/api/src/helm.rs`). The three releases were
    repaired live with the same flags (revisions 8, 9 and 12). Any other app
    that has been image-refreshed once will fail its next update until the fix
    ships.

## History

Release history and earlier sessions are kept in
[`docs/archive/ai-handoff-log-2026-11-08.md`](docs/archive/ai-handoff-log-2026-11-08.md)
and the GitHub releases. Per-topic reference documentation lives in `docs/`
(`spec.md`, `api.md`, `deployment.md`, `operations.md`, `app-catalog.md`,
`dynamic-dns.md`, `buddy-backup.md`, …).
