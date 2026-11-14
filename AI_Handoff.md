# AI Handoff — Naslos

**This is the current deployed state plus the operational gotchas, kept
short.** Historical session narratives are in [`docs/archive/`](docs/archive/);
the pre-cleanup handoff snapshot is
[`docs/archive/ai-handoff-log-2026-11-08.md`](docs/archive/ai-handoff-log-2026-11-08.md).
Normative requirements are in [`docs/spec.md`](docs/spec.md); build/test/deploy
rules and conventions are in [`AGENTS.md`](AGENTS.md).

## What this is

A single-node NAS appliance on Talos Linux (Kubernetes) with a web UI.

## Production instance (2026-11-14)

- **Hardware:** the production NAS is the bare-metal node `talos-zgr-9yg` at
  `192.168.1.38` (Talos v1.14.5, Kubernetes v1.37.0, control-plane Ready). The
  old `192.168.1.117` VM is powered off. The recovery bundle imported into
  `bootstrap/vm/` points at `.38`; the `vm` names (`bootstrap/vm/*`,
  `values-vm.yaml`, `install-vm`, `deploy-vm.sh`) are historical paths for the
  single node, not a VM.
- **Deployed release:** Helm `naslos` chart `0.3.23`, appVersion `0.3.23`,
  revision 32, deployed 2026-11-14 11:18 UTC from `master` with
  `make install-vm`. API/agent/OpenLDAP/Samba/NFS/terminal are digest-pinned
  from `v0.3.23`: API `fa9c8956…`, agent `4f680bfc…`, OpenLDAP `e57e3fbf…`,
  Samba `b785ee38…`,
  NFS `4d8806ea…`, terminal `a000ca0c…` (public GHCR). The UI is the
  post-0.3.23 `sha-6ebab2db…` build (`bc8f5013…`) with the corrected System
  message. `/api/system/version` reports Naslos/chart 0.3.23, profile
  `make-install`, Talos v1.14.5 and k8s v1.37.0; the chart falls back to its own
  version so a plain make-install node is not blank. `values-vm.yaml` now
  declares `release.profile: installer` with the running facts, so the web
  upgrader is enabled: SA `naslos-upgrader` + cluster-admin binding, the
  `naslos-api-system` Role/RoleBinding, and the upgrader admission policy are
  present, and a GitHub check returns `enabled:true, current:0.3.23, no newer
  release`; the operations endpoint correctly answers 412 until a check offers
  a release. The private registry `192.168.1.2:30095` is retired — do not
  build/push local images.
- **Latest release:** `v0.3.23` (2026-11-14) and installer `naslos-v0.3.23`
  are published; the node runs those exact digests plus the post-release UI
  message fix. Full Playwright suite green on revision 30 (61 passed, 2
  skipped); the system spec is green on revision 33 with web upgrades enabled.
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
- **Update check:** current deployed `0.3.21`; **`v0.3.22` is offered** by the system updater.

### Open items

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
- **Playwright** has not been rerun since the bare-metal switch.

## How to operate

```bash
# Gates (run inside the repo; there is no make test/lint)
cd rust && cargo fmt --all -- --check
cd rust && cargo clippy -p naslos-api --all-targets -- -D warnings
cd rust && cargo test -p naslos-api
cd rust && cargo test -p naslos-agent
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
5. **Platform mount-propagation trap.** A dataset created from inside a pod is
   mounted in that pod's namespace only; in the host namespace its mountpoint is
   a plain directory on the parent. `zfs send` of such a dataset captures
   nothing — the API refuses it (`requireMountedDataset`), and the agent's
   `UsedBytes`-based guard also refuses a published stream that is a small
   fraction of the dataset's used space. The fix is a node reboot (or `zfs mount
   <dataset>` on the host). Shares have the same trap for newly created
   datasets.
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
15. **Live hotfixes survive Helm upgrades.** The 0.3.9 upgrade initially kept
    the 0.3.7 `hotfix-bin` API command (`/hotfix-bin/hotfix/naslos-api`) and the
    UI `naslos-ui-nginx` ConfigMap/PVC root (`/var/lib/naslos/ui-html`), so the
    node reported 0.3.9 while running the old API/UI and the new app feature
    was inactive. They were removed on 2026-11-08: the API now runs
    `/app/naslos-api` from the image and the UI serves `/usr/share/nginx/html`.
    If an upgrade behaves like the old version, check the Deployment `command`
    and the UI nginx root first. Never ship a fix as a live `kubectl patch`;
    put it in the image/chart.

## History

Release history and earlier sessions are kept in
[`docs/archive/ai-handoff-log-2026-11-08.md`](docs/archive/ai-handoff-log-2026-11-08.md)
and the GitHub releases. Per-topic reference documentation lives in `docs/`
(`spec.md`, `api.md`, `deployment.md`, `operations.md`, `app-catalog.md`,
`dynamic-dns.md`, `buddy-backup.md`, …).
