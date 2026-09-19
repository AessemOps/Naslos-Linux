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

- **`master` = `4feb048`**; PRs #9–#19 merged (buddy, security fixes, fan-out,
  hardening batches, product gaps, a11y/interface IPs, digests, receive-dataset
  init container, walk bounds, the code review and the handoff rewrite).
- **The instance now runs on the new VM: `192.168.1.117`** — see the section below.
  The old `192.168.1.96` VM is powered off.
- **Open work is in `docs/CODE-REVIEW.md`**: three blocking findings — CR-01 (the
  chart's Authelia policy bypasses the whole domain), CR-02 (`helm uninstall`
  deletes the namespace and the state volume holding the buddy identity), CR-03
  (`RequireAdmin`/`IsAdmin` are never wired) — then the important list (dependency
  bumps, `-race`, token exposure, UI silent failures, observability, docs).
  **CR-01/CR-02/CR-03 are fixed** on `fix/chart-auth-and-admin` (PR #20), not yet
  merged.
- **Security audit status**: 22 NAS findings, each fixed, accepted or open per
  `docs/CODE-REVIEW.md` §6.

## Current instance: 192.168.1.117 (2026-09-19)

Fresh install from scratch (the old .96 VM is off): Talos from the Naslos ISO,
etcd bootstrapped, local-path provisioner, OpenLDAP + bootstrap, chart revision
**2**, every image at **`0.1.0-r1`**. Posture is still the **dev one**
(`auth.disabled=true`, UI on the NodePort) — the proxy path needs the pieces listed
under "Not done" below.

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
- **Not done yet**: no ZFS pool (two spare 40 GB disks visible) so datasets/shares
  and the pool re-import path are untested; Buddy is disabled (no receive dataset);
  and the real posture still needs Authelia portal routing, the `naslos-tls` secret,
  Traefik exposed on the LAN, an LDAP operator in `naslos_admins` with TOTP, and a
  peer-API bypass.

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
curl -s -o /dev/null -w '%{http_code}\n' http://192.168.1.96:30080/api/ready
```

VM facts: node `192.168.1.117`, UI on NodePort `:30080`, private registry
`192.168.1.2:30095`, namespace `naslos`, `TALOSCONFIG=bootstrap/vm/talosconfig`.
Buddy is **disabled** on this VM and there is **no ZFS pool yet**: create one
(two spare 40 GB disks) before datasets/shares/buddy; the receive dataset would be
`<pool>/naslos-buddy` (`buddy.receivePath` `/var/lib/naslos/buddy`).

## Operational gotchas (hard-won — read before drilling)

1. **Always bump image tag suffixes.** The registry reuses tags and the chart pulls
   with `IfNotPresent`, so a retag alone can keep running old code (NAS-022).
   Digests are supported (`api.image.digest`, `make image-digests`); a stored digest
   wins over a new tag until you clear it (`--set api.image.digest=`).
2. **`helm upgrade --reuse-values` ignores `-f` files.** Anything new must be passed
   with `--set` (e.g. `auth.disabled`, `ingress.enabled`), or the release keeps old
   values and templates that dereference new keys can fail to render.
3. **The VM runs `auth.disabled=true`** because it has no reachable Traefik: every
   owner route is open on the NodePort (NAS-008). Never expose it. A real deployment
   runs with the default `auth.disabled=false` behind the proxy secret.
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

On `192.168.1.117`: `naslos-api`, `naslos-ui`, `naslos-agent`, `naslos-samba`,
`naslos-nfs`, `naslos-terminal` and the OpenLDAP manifests all at **`0.1.0-r1`**,
chart `naslos-0.1.0`, helm revision **2**, Talos **v1.14.1** (kernel 6.18.51-talos).
The old VM's tags (`api 0.1.0-b21`, `agent 0.1.0-b6`, `ui 0.1.0-b9`, revision 82)
are retired with it.
