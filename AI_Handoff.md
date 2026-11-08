# AI Handoff — Naslos

**This is the current deployed state plus the operational gotchas, kept
short.** Historical session narratives are in [`docs/archive/`](docs/archive/);
the pre-cleanup handoff snapshot is
[`docs/archive/ai-handoff-log-2026-11-08.md`](docs/archive/ai-handoff-log-2026-11-08.md).
Normative requirements are in [`docs/spec.md`](docs/spec.md); build/test/deploy
rules and conventions are in [`AGENTS.md`](AGENTS.md).

## What this is

A single-node NAS appliance on Talos Linux (Kubernetes) with a web UI.

## Production instance (2026-11-08)

- **Hardware:** the production NAS is the bare-metal node `talos-zgr-9yg` at
  `192.168.1.38` (Talos v1.14.5, Kubernetes v1.37.0, control-plane Ready). The
  old `192.168.1.117` VM is powered off. The recovery bundle imported into
  `bootstrap/vm/` points at `.38`; the `vm` names (`bootstrap/vm/*`,
  `values-vm.yaml`, `install-vm`, `deploy-vm.sh`) are historical paths for the
  single node, not a VM.
- **Deployed release:** Helm `naslos` chart `0.3.12`, appVersion `0.3.12`,
  revision 8, deployed 2026-11-08 21:49 UTC (web-UI upgrade from 0.3.11). Images
  are digest-pinned from the 0.3.12 pack: API `9ffd8c3b…`, agent `c8da5d2d…`, UI
  `7f79d295…`, OpenLDAP `a8b955a1…`, Samba `4f5b8c1c…`, NFS `7fe27a7e…`,
  terminal `28040f19…`. `/api/system/version` reports chart/naslos `0.3.12`,
  profile `installer`, Talos v1.14.5, k8s v1.37.0.
- **Latest release:** `v0.3.12` (2026-11-08); installer `naslos-v0.3.12` is
  rebuilt by the release dispatch. The node is on `0.3.12`. `v0.3.11`,
  `v0.3.10` and `v0.3.9` were the previous releases, `v0.3.8` was superseded.
- **Release notes since 0.3.7:**
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
  `audio.shail.ovh`, dataset `audiobooks` mounted at
  `/datasets/audiobooks/AudioBooks`), `calibre-web` (`naslos-app-3`,
  `calibre.shail.ovh`, dataset `ebooks`), and `jellyfin` (`naslos-app-4`,
  `jelly.shail.ovh`, datasets `animes`/`movies`/`series`/`ukrmovies`, wizard
  complete, reads all four trees). `audiobookshelf`, `calibre-web` and
  `jellyfin` run as `10001:10000`; the two LinuxServer apps needed a one-off
  `chown -R 10001:10000 /config` after the UID/GID switch. The `shail.ovh`
  domain (OVH, production, `sso: true`) and `naslos-shail-ovh-tls` are present.
- **Update check:** current `0.3.12`; no newer release is available.

### Open items

- **Multi-dataset live drill:** four apps now mount datasets (Jellyfin has four),
  which exercises the per-dataset claims and `/datasets/<name>` mounts. Still
  to verify: remove one dataset from an installed app and confirm its claim and
  registry attachment are cleaned up, and attach a second dataset to an existing
  app.
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
    `naslos-apps-priv` is `privileged`; the per-app pools `naslos-app-1..8` are
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
    Internet and reject the opt-out.
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
