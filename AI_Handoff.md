# AI Handoff — Naslos

**This file is the project's current state, kept to about a page.** The detailed
per-session narrative (1250 lines, 2026-09-17 snapshot) is archived at
`docs/archive/ai-handoff-log-2026-09.md`. Open quality findings are in
`docs/CODE-REVIEW.md`. Normative requirements are in `docs/spec.md`.

## What this is

A single-node NAS appliance on Talos Linux (Kubernetes) with a web UI.

| Piece | What it is |
| --- | --- |
| `api/` (Go) | UI-facing HTTP API + the buddy sender, scheduler and job runner |
| `agent/` (Go) | Privileged, host-networked DaemonSet: the only thing that runs `zpool`/`zfs`/`wipefs`; also serves the streaming backup endpoints |
| `ui/` | Svelte 4 + TS + Tailwind, built statically and served by nginx |
| `charts/naslos` | Helm chart: api, ui, agent, samba, nfs, terminal, openldap, Traefik + Authelia subcharts |
| `openldap/ samba/ nfs/ terminal/` | Per-service images and config templates |
| `api/cmd/buddyctl`, `api/cmd/buddy-receiver` | Standalone backup client and Docker receiver (no Kubernetes, no ZFS) |

Features: ZFS pools/datasets, SMB + NFSv4 shares with LDAP identities, dashboard and
metrics, web terminal, app catalog, notifications, and zero-knowledge peer backups
("Buddy").

## Where things stand (2026-09-19)

- **`master` = `90d4351`**; PRs #9–#20 merged (buddy, security fixes, fan-out,
  hardening batches, product gaps, a11y/interface IPs, digests, receive-dataset
  init container, walk bounds, the code review, the handoff rewrite, and the
  namespace/Authelia/admin-gating fixes).
- **The instance now runs on the new VM: `192.168.1.117`** — see the section below.
  The old `192.168.1.96` VM is powered off.
- **Open work is in `docs/CODE-REVIEW.md`**: the three blocking findings — CR-01
  (the chart's Authelia policy bypassed the whole domain), CR-02 (`helm uninstall`
  deleted the namespace and the state volume holding the buddy identity) and CR-03
  (`RequireAdmin`/`IsAdmin` were never wired) — are **fixed and merged** (PR #20).
  The important list is still open: dependency bumps (18 reachable Go vulns via
  Helm, 11 npm advisories), `-race` (the suite fails under it), observability,
  docs. The **correctness/secret-leak batch is fixed on a local branch (not yet
  merged/pushed)**: CR-07 (the ntfy token is no longer returned; `hasAuthToken`
  replaces it), CR-15 (the NSS `shadow` mirror is `0600`), CR-18 (a user edit now
  applies group changes), CR-19/CR-22 (the UI checks `res.ok` and surfaces
  failures instead of parsing error bodies or looking successful).
- **Security audit status**: 22 NAS findings, each fixed, accepted or open per
  `docs/CODE-REVIEW.md` §6.

## Current instance: 192.168.1.117 (2026-09-19)

Fresh install from scratch (the old .96 VM is off): Talos from the Naslos ISO,
etcd bootstrapped, local-path provisioner, OpenLDAP + bootstrap. The instance is
now on the **production posture** (helm revision 9): Traefik on hostPort 80/443,
Authelia forwardAuth with the portal at `https://naslos.local/authelia`, 2FA on
every admin prefix, `/api` routed straight to the API, and the UI NodePort gone.
The first admin (`admin` in `naslos_admins`) was created before the flip. Use
`make install-vm` for the dev posture (NodePort, auth off) and
`make install-prod` for this one; the Playwright suite now runs against both
(see "Running the E2E suite" in `docs/deployment.md`).

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
- **Suite baseline now `30 passed / 2 skipped`** — the two Buddy tests run, and the
  only skips left are terminal exec over the NodePort (refused by nginx, by design).
- **Production posture is live (helm revision 9).** Traefik hostPort 80/443,
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
  two consecutive runs** (helm revision 15, api `0.1.0-r3`, ui `0.1.0-r4`). It
  logs into Authelia once in a setup project (`ui/tests/auth.setup.ts`, password
  + TOTP generated from the secret in the gitignored `ui/.env.playwright.local`)
  and reuses the session via `storageState`; on the dev NodePort it writes an
  empty state and behaves as before. See `docs/deployment.md` "Running the E2E
  suite" for the env file and `NASLOS_RECEIVER_URL`. Running it found two real
  production bugs, both fixed:
  - **Agent streaming token (NAS-002 gap).** `api/internal/agent/backup.go` used
    a package-level `&http.Client{}` for the streaming send/receive paths, so
    those requests carried no `Authorization` header and every backup failed
    with the agent's 401 once auth was on. The client now has a dedicated
    `stream` client built from the same token transport
    (`api/internal/agent/client_test.go` pins it). In the dev posture the agent
    opted out with `AGENT_AUTH_DISABLED=true`, which is why this only surfaced
    after the cutover.
  - **Password changes were a silent no-op.** The user-edit form put `password`
    in the `PUT /api/users/{uid}` body, which ignores it; the dialog still closed
    as if saved. It now calls `POST /api/users/{uid}/password`. `docs/spec.md`
    had that route as PUT, which the handler rejects.
  The suite also needs `NASLOS_RECEIVER_URL` for its two self-send tests: a
  page-driven send targets the browser origin, and the API pod cannot resolve
  `naslos.local` (mDNS is not in cluster DNS).

## How to run it

```bash
# Gates (all should pass; -race currently FAILS, see CR-06)
cd api   && go build ./... && go vet ./... && go test ./...
cd agent && go build ./... && go vet ./... && go test ./...
cd ui    && npm run check && npx playwright test      # Playwright needs the VM
helm lint charts/naslos -f charts/naslos/values.yaml

# Deploy: always with FRESH tag suffixes (a retag can serve stale code)
make api-image IMAGE_TAG=0.1.0-b22 && docker push 192.168.1.2:30095/naslos-api:0.1.0-b22
helm upgrade naslos charts/naslos -n naslos --reuse-values \
  --set api.image.tag=0.1.0-b22 --wait
curl -s -o /dev/null -w '%{http_code}\n' http://192.168.1.117:30080/api/ready
```

VM facts: node `192.168.1.117`, UI on NodePort `:30080`, private registry
`192.168.1.2:30095`, namespace `naslos`, `TALOSCONFIG=bootstrap/vm/talosconfig`.
Pool `test` (stripe of `/dev/vdb`+`/dev/vdc`, 79 G) with datasets `test/drill` and
`test/naslos-buddy` (the buddy receive dataset); **Buddy is enabled** with
`buddy.name=naslos-vm` and `peersFile`/`schedulesFile` on the state PVC.

## Operational gotchas (hard-won — read before drilling)

1. **Always bump image tag suffixes.** The registry reuses tags and the chart pulls
   with `IfNotPresent`, so a retag alone can keep running old code (NAS-022).
   Digests are supported (`api.image.digest`, `make image-digests`); a stored digest
   wins over a new tag until you clear it (`--set api.image.digest=`).
2. **`helm upgrade --reuse-values` ignores `-f` files.** Anything new must be passed
   with `--set` (e.g. `auth.disabled`, `ingress.enabled`), or the release keeps old
   values and templates that dereference new keys can fail to render.
3. **The VM runs the production posture** (Traefik hostPort 80/443 + Authelia,
   NodePort off). `make install-vm` (values-vm.yaml alone) reverts it to the dev
   posture where every owner route is open on the NodePort (NAS-008) — that is
   the rollback and the only posture the Playwright suite can run against, so
   never leave it up on an untrusted network.
4. **The buddy identity is the KEK.** `/var/lib/naslos/buddy-identity.json` holds the
   private key and the key-encryption key: losing it makes every stored backup
   unreadable, and `helm uninstall` would delete it (CR-02). Back it up separately.
5. **Platform mount-propagation trap.** A dataset created from inside a pod is mounted
   in that pod's namespace only; in the host namespace its mountpoint is a plain
   directory on the parent. `zfs send` of such a dataset captures nothing — the API
   now refuses it (`requireMountedDataset`) — and the fix is a node reboot (or
   `zfs mount <dataset>` on the host). Shares have the same trap for newly created
   datasets.
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
   (CR-23). Terminal exec tests always skip over the NodePort (nginx refuses those
   paths by design).
10. **A restored dataset cannot be destroyed until the agent restarts.** `zfs receive`
   runs through the privileged agent, so the destination stays mounted in the
   **agent's** mount namespace and `zfs destroy` answers `dataset is busy` (the
   terminal pod's namespace does not see that mount). Remedy:
   `kubectl -n naslos rollout restart ds/naslos-agent`, then destroy — verified on
   .117 with the drill's `test/drill-buddy-restored`.

## Conventions

- **Spec rule**: a change that alters a MUST in `docs/spec.md` updates the spec *and*
  its test in the same change; unimplemented MUSTs are marked `[OPEN]` (VER-4).
- **Found a defect while drilling?** Fix it in the same branch with a test, and record
  it where the next session will read it — that is how every fix in this repo landed.
- **Tests before hand-off**: `go build/vet/test` for both Go modules, `svelte-check`,
  `helm lint`, the Playwright suite for UI/API changes, and a live drill for anything
  touching the node (shares, LDAP, ZFS, backups).
- **Plans** live in `.kilo/plans/`; the per-topic docs in `docs/` are the reference
  material (`buddy-backup.md`, `shares.md`, `storage-zfs.md`, `api.md`,
  `deployment.md`, `operations.md`, `architecture.md`).
- **Record the deployed tags** below whenever they change, and keep this file short:
  new session narratives belong in the archive, not here.

## Deployed right now (2026-09-19)

On `192.168.1.117`: `naslos-api`, `naslos-ui` and `naslos-agent` are at
**`0.1.0-r2`** (the CR-07/15/18/19/22 batch); `naslos-samba`, `naslos-nfs`,
`naslos-terminal` and the OpenLDAP manifests stay at **`0.1.0-r1`**; chart
`naslos-0.1.0`, helm revision **9**, Talos **v1.14.1** (kernel 6.18.51-talos),
ZFS pool `test` (stripe, 79 G) + dataset `test/drill`. The posture is the
**production one** (Traefik hostPort 80/443, Authelia at
`https://naslos.local/authelia`, NodePort off) with `admin` in
`naslos_admins`; TOTP/WebAuthn enrollment is the one remaining human step. The
old VM's tags (`api 0.1.0-b21`, `agent 0.1.0-b6`, `ui 0.1.0-b9`, revision 82)
are retired with it.
