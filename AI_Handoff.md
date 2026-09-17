# AI Handoff — Naslos

## Current branch: `master`

The integration branch is `master`. The Buddy Backup work (and the security audit +
remediation plan) landed from `feature/buddy-backup` via **PR #9**, merge commit
`1f1edcf`; `master..feature/buddy-backup` is now 0. Earlier features are all in
master too (storage, shares, users/identity, dashboard/metrics, terminal, app catalog,
SSO, zfs-host-support, web-terminal). The project roadmap lives in
`.kilo/plans/1789412781292-buddy-ui-scheduler-plan.md` (consolidate → security gate →
buddy completion → product gaps).

**Post-merge verification (2026-09-17, from `master` `1f1edcf`)** — deployed api
`0.1.0-b9`, agent `0.1.0-b3`, ui `0.1.0-b5` (helm revision 60, `--reuse-values`):

- Local: `api` and `agent` `go build`/`vet`/`test` clean; `svelte-check` 0 errors /
  45 warnings.
- Playwright against the VM: **29 passed / 2 skipped / 0 failed** (the 2 skips need an
  interactive terminal session).
- Buddy smoke on the merged tree: `POST /api/buddy/send` → `succeeded`
  (chain `d8c080128f5e85ca`), `verify` returned the digest; a `daily` schedule due
  ~2 min later fired (`lastRun 17:32:41Z`, `lastResult: ok`, `nextRun` +1 day) and its
  job carried `scheduleId` + `pruneKeep: 2` and ran `incremental: true` off the base
  GUID. Identity and peers survived the reboot and the redeploy (PVC); the receive
  dataset ownership was still `65532:65532`, so no `chown` step was needed.

## Buddy Backup (in `master`) — zero-knowledge peer backups

**What it is.** One Naslos instance can push backups to another instance — or to a
standalone container — over a key-authenticated API. The receiver stores
**ciphertext it cannot read**: the sender encrypts with a per-chain AES-256-GCM key
wrapped by a key encryption key (KEK) only the owner holds. Full documentation in
`docs/buddy-backup.md`; requirements in `docs/spec.md` §3.8 (FR-BUD-01…10, SEC-6…8).

**Protocol** (identical for both receiver flavours):

- **Auth** — Ed25519 in OpenSSH form; the signature covers
  `BUDDY1\nMETHOD\nfull-URI\nsha256(body)\ntimestamp\nnonce`. 5-minute clock
  window, nonce replay cache, and a nonce is burned only **after** the signature
  verifies so junk traffic cannot exhaust a peer's nonces. First key in via a
  single-use enrollment token; everything else needs an already-authorized key.
- **Envelope** — 1 MiB chunks: `NBC1 | plainLen(4) | nonce(12) | ct+tag`, nonce =
  `streamPrefix||index`, AAD binds source+chain+index+plainLen. Per-chain DEK,
  wrapped by the KEK and carried in the signed manifest, which also stores a
  SHA-256 per plaintext chunk (only the owner can check that — the receiver cannot).
- **Endpoints** — `/api/buddy/v1/{enroll,status,backups,chunks/{source},manifest/{source},prune/{source}}`.
  A push is many small requests plus a signed manifest, which is what makes resume
  cheap and idempotent.

**Delivered**

- `api/internal/buddy/` — keys/identity, request signing + replay protection,
  chunked envelope, receiver store, peer registry, receiver HTTP surface, sender
  client.
- `api/cmd/buddyctl` — identity/enroll/push/status/backups/restore/prune
  (`make buddyctl`, dir/file/stdin, `--resume`).
- `api/cmd/buddy-receiver` + `api/Dockerfile.receiver` +
  `make buddy-receiver-image` — the standalone two-volume receiver (no ZFS, no
  Kubernetes, no database; two volumes and one binary).
- Instance receive side: the routes above plus `/api/buddy/status` and
  `/api/buddy/peers` for the owner, chart values (`buddy.*`), a `naslos-buddy`
  enrollment Secret, the dataset mount, and a UI-nginx `/api/buddy/` location with
  `client_max_body_size 8m` — the default 1 MB would reject every 1 MiB chunk with
  a 413 before it reached the API.
- 11 Go tests in `api/internal/buddy/buddy_test.go` driving a real receiver over
  `httptest`: round trip, tamper (flip/truncate/reorder/cross-chain), resume (and
  refusal to resume with changed data), scope, quota, unsigned/unknown/stale/replay,
  single-use enrollment, prune.

**Verified live on the VM** (peer → UI NodePort → nginx → API, chart `buddy`
enabled, dataset `test/naslos-buddy`):

- enrollment token accepted once (second attempt → 403); push 3 MiB → 4 chunks;
  `status`/`backups` report free space, stored bytes, last backup; restore + `diff -r`
  + `md5sum` byte-identical.
- 128 MiB push killed with `timeout -s KILL 1`, then `--resume` → 15 chunks
  verified-and-skipped, 114 uploaded, full verify-only restore clean.
- one byte flipped in a stored chunk → restore fails `cipher: message authentication
  failed` (the earlier "passed" run was my test racing the tamper, not a defect).
- unsigned → 401, unknown key → 401, out-of-scope source → 403, prune drops the old
  chain and the remaining chain still restores.
- the store contains only `chunk-*.enc` (1 MiB + 36 B, mode 0600, uid 65532) and the
  signed manifest; a plaintext needle is absent from it — the zero-knowledge
  property, checked on the deployed instance.

**The standalone container receiver is verified too**: `docker build -f
api/Dockerfile.receiver` → run with two volumes → enroll, push, `status` (free
space and last backup from the container's own volume), restore and `diff -r`
byte-identical. The volumes hold the same envelope-only tree and a `peers.json`
with public keys alone, and the binary passes its own `-health` check as the
image's HEALTHCHECK. Its volumes must be owned by **65532** (`-v` mounts keep host
ownership): `sudo chown -R 65532:65532 /srv/buddy-data /srv/buddy-config`.

**Deployment gotcha found live**: the API image is distroless and runs as uid
**65532**, so the receive dataset has to be handed to it —
`chown 65532:65532 /var/mnt/<pool>/naslos-buddy`. `fsGroup` does **not** apply to
`hostPath` volumes, and without the chown the first push fails with
`mkdir /var/lib/naslos/buddy/<key>: permission denied`. Documented in
`docs/buddy-backup.md` (§5.1 step 1b and the troubleshooting table).

**Second gotcha, platform-level and still open on the VM**: the agent and
terminal containers mount `/host` with `mountPropagation: HostToContainer`
(one-way), so a `zfs create` run inside a pod mounts the dataset only in *that
pod's* namespace. Pods that bind-mount the path - including the API - therefore
resolve it to the **parent** dataset, which is what the VM shows now:
`zfs list` reports `test/naslos-buddy` `USED 96K` while `df`/`du` on
`/var/mnt/test/naslos-buddy` report the pool root `test` with 133 MB. Backups are
stored and restorable either way, but the dedicated dataset's isolation (and its
quota) is not in effect until the dataset is mounted in the host namespace - a
node reboot does it (the ZFS extension runs `zfs mount -a` at boot), after which
the API deployment must be restarted so its hostPath bind picks the dataset up.
The check is one command from a fresh pod (`df -h` on the mount path must name
`<pool>/naslos-buddy`); it is written up as step 1c and in the troubleshooting
table of `docs/buddy-backup.md`.

**Instance-side sender and restore (new in this change, drilled live)**

- The agent grew streaming seams — `GET /api/v1/zfs/send/{dataset}?to=&from=&raw=&estimate=`,
  `POST /api/v1/zfs/receive/{dataset}?force=`, `GET /api/v1/zfs/snapshots/{dataset}`
  (GUIDs) and `POST /api/v1/snapshots/{dataset}` — and the control-plane client's
  180 s timeout is deliberately bypassed for them (bounded by the caller's context
  instead; `hostExec` still buffers, the streaming paths use `cmd.StdoutPipe`).
- The API drives them: `POST /api/buddy/send`, `POST /api/buddy/restore` (plus a
  `verify` mode that decrypts and hashes without touching ZFS) and
  `GET|POST /api/buddy/identity`. An interrupted send resumes from a `0600` state
  file written before the first chunk; snapshots are named `buddy-<UTC>-<4 hex>`.
- The base for an incremental is found by **GUID**, not name: the manifest records
  the GUID the receiver was given and the node is asked which snapshot carries it,
  so a rename still counts and a destroyed snapshot is known to be gone (the send
  then falls back to a full one rather than guessing).

**The drill, bit-for-bit** (`test/drill-src`, 53 MB full + a 57 MB change):

- full send: `POST /api/buddy/send` → chain `ff6e4b183f22f62e`, 53,390,016 bytes;
  the instance's `verify` digest equals the node's `zfs send -w | sha256sum`.
- incremental: chain `fe3a45a0c1dc502b`, `zfs send -w -i <base> <new>` on the node
  hashes `908ba240b9d93144a53bb18c2446eb1dc178506703636426fbf15320607802fd` over
  56,801,992 bytes — the instance's `verify` reports the same digest and count,
  exactly.
- the resume path: the first attempt was refused by the shortfall guard *after*
  every chunk was stored, so the chain had data but no manifest. Re-sending the same
  request **resumed** it (`resumed: true`, `uploaded: 0`, `skipped: 55`) and published
  the manifest — a stream that did not arrive whole never became the latest backup,
  and the retry continued the same chain, snapshot and data key.
- restore after a real `zfs destroy -rf test/drill-src`: the sequence
  (`ff6e4b18…` → `fe3a45a0…`) restored into `test/drill-restored` in 1.57 s —
  106 chunks, 110,192,008 bytes (= 53,390,016 + 56,801,992, exact), final snapshot
  `buddy-…308d`, and the dataset matches the destroyed source
  (`USED`/`REFER`/`LREFER` = 106M/106M/114M) while carrying both buddy snapshots.
- `verify` still succeeded with the source dataset gone (the backup, not the source,
  is what it reads).

**Fourth live finding — the dry-run estimate is not a floor.** `zfs send -nP` is
exact-to-within-framing for *full* sends (measured +232 B on 44 KB, +9,712 B on
53 MB) but can sit *above* an *incremental* stream: a 57 MB incremental came in
119,688 bytes below its estimate (−0.21%). The original 0.1% allowance therefore
rejected a complete send. It is now `max(1 MiB, 1%)`, documented as an early warning
rather than the integrity boundary: anything smaller is caught by ZFS at
`zfs receive` (the stream carries its own end record and per-record checksums) and a
retry resumes the chain.

**Third live finding — a `/` in a fingerprint split the key directory.** A peer
fingerprint used verbatim as a path component (`SHA256:ab/cd…`) created two nested
directories and the push failed with `permission denied`; key directories are now
hashed names, and snapshots take a random suffix because two sends inside one second
collided on the same name.

**Deployed**: api `0.1.0-b9`, agent `0.1.0-b3`, ui `0.1.0-b5` (helm revision 60 —
superseding the api `0.1.0-b7`/ui `0.1.0-b1` line below), terminal `0.1.0-t2`,
samba `0.1.0-s15`, nfs `0.1.0-n2`.

**Delivered since: UI + scheduler (FR-BUD-15/16)**
- `POST /api/buddy/send` is async (`202 {jobId}` + `GET/DELETE
  /api/buddy/jobs/{id}`, progress + cancel; 409 while the same
  (receiver, source) or dataset runs); the resume-state semantics are unchanged.
- Schedules (`GET/POST/DELETE /api/buddy/schedules`, `hourly|daily|weekly` +
  run-at UTC, `BUDDY_SCHEDULES`, minute tick, catch-up on startup, `pruneKeep`
  on success, ntfy `backup_success` + `backup_failure` gated on enabled events).
- `/backups` UI page: identity card, schedules + Back up now with live progress,
  ad-hoc send, per-source verify, confirmation-gated restore form, receiver
  status; `ui/tests/backups.spec.ts`.

**Not done yet, in plan order**: multi-buddy fan-out; and the peer-exposure
decision (a dedicated listener vs. Traefik + Authelia) still has to be taken. Also
still open: the receive dataset's host-namespace mount on Talos (see the platform
gotcha above), which the restore drill does not depend on because `zfs receive`
creates the mount itself.

**UI + scheduler verified live on the VM (2026-09-14/15), api `0.1.0-b8` + ui
`0.1.0-b4`, helm revision 59.** `go build`/`vet`/`test` clean, `svelte-check` 0
errors, full Playwright suite **29 passed / 2 skipped / 0 failed** (terminal exec
tests skip without a session). Drills, all through the UI NodePort with
`Remote-User: admin`:

- async jobs: `POST /api/buddy/send` → `202 {jobId}`, poll → `succeeded` with the
  full result payload (chain, chunks, uploaded/skipped, plainBytes, snapshot,
  GUIDs) and progress ticks; `GET /api/buddy/jobs` lists it; unknown job → 404;
  second send for the same (receiver, source) → 409 naming the running job.
- cancel + resume (FR-BUD-16): an in-cluster drill cancelled a 106 MiB send at
  chunk 16 (`DELETE` → `cancelling` → `cancelled`), and the retry resumed the
  **same snapshot**: `resumed: true`, 16 skipped, 90 uploaded, 106 chunks,
  succeeded. Same receiver+source is required (a different receiver is a
  different chain, by design).
- schedules (FR-BUD-15): a `daily` entry fired at its run-at, recorded
  `lastResult: ok` and advanced `nextRun`; the job carries `scheduleId`; a
  schedule pointed at a dead receiver recorded `lastResult: failed` +
  `lastError`; a `hourly` entry stayed due while a conflicting manual send ran.
  Catch-up is unit-tested (`TestBuddySchedulerCatchUpFiresOnceAndDisabledNeverFires`),
  not repeated live (it needs a past `nextRun` patched into the PVC, and the API
  image is distroless).
- notifier: ntfy `backup_success` (priority 2) and `backup_failure` (priority 4)
  both arrived at a capture listener with the expected titles/bodies; with only
  `backup_failure` enabled a success produced **no** post while failures still did.
- retention: the second send of a source was `incremental: true` off the recorded
  GUID (1 chunk), `pruneKeep` reported `prunedChains: 1` and the receiver kept one
  chain.
- restore: verify returned a chain digest and a restore landed with the source's
  exact `USED/REFER/RATIO` and its `buddy-…` snapshot (receive validates the
  stream itself; node-side `sha256sum` comparison was skipped because the sandbox
  forbids shell pipes).
- persistence: schedules (with `lastRun`/`lastResult`/`lastError`) and the identity
  survive an API restart; the job list is in-memory and resets.
- the receiver store holds only `chunk-*.enc` (0600, 1 MiB + 36 B) and the signed
  `manifest.json`/`current.json` — no plaintext.
- **the host-namespace mount gotcha is fixed on the VM by the node reboot**:
  `df` on `/var/mnt/test/naslos-buddy` from a fresh pod now names
  `test/naslos-buddy`, not the pool root. The `chown 65532:65532` step is still
  needed (the dataset was `root:root` again and the first push failed with
  `mkdir …/buddy/<key>: permission denied` until it was applied).

**Cross-flavour interop verified live (VM instance ↔ standalone container).**
The standalone `naslos-buddy-receiver:0.1.0-b9` (`api/Dockerfile.receiver`) ran on
the workstation (`192.168.1.135:8484`, two volumes at `~/buddy-standalone`,
enrollment closed, the VM's key pre-authorized in `peers.json`), reachable from
the VM's API pod; both flavours spoke the same protocol unchanged.

- **VM sender → container receiver:** `POST /api/buddy/send` → `succeeded`,
  chain `79c6006f5bc8fd5c`; `verify` returned the digest; restore onto the VM
  landed `test/docker-restored` identical to `test/Backup` (96K/96K, ratio
  1.00x) with the `buddy-…28a9` snapshot. A second send was `incremental: true`
  off the recorded base GUID (1 chunk, 624 B, chain `3f2dcb81e031f323`). The
  container's store held the same envelope layout as the VM's (and the same
  hashed key dir `k519a911…`, deterministic from the sender fingerprint).
- **Container restart** (`docker restart`): peer registry and store survived;
  the VM's `verify` then walked the full sequence (44,368 + 624 = 44,992 B).
- **Standalone client (`buddyctl`) → container:** a fresh `ws-sender` key pushed
  a directory (chain `f1b8c05c90ff8d2b`), restored byte-identical (`diff -r`
  clean), and a root `grep` for a plaintext needle in the receiver's store found
  **nothing** — ciphertext only. Restoring with a copy of the identity whose KEK
  was replaced failed with `cannot unwrap the data key: this backup was not made
  with this key` — the zero-knowledge property on real cross-flavour data.
- **Standalone client → VM receiver (reverse direction):** `buddyctl enroll` with
  the chart's token succeeded (the API restart re-armed the "single-use" token —
  audit NAS-011), push chain `4a0c114e0203f61b`, `buddyctl status` showed the
  per-key view (free space, stored-for-me, sources, last backup) and the restore
  was byte-identical. The test peer was then revoked and the VM store emptied.
- **Minor finding:** the peer-facing `/status` returns every peer *name*
  (`Status.PeerNames`, `api/internal/buddy/http.go:186-190`), so any authorized
  key learns which other peers exist (names only, no keys). Low sensitivity, but
  it is a deliberate choice worth making.

**Defects found live and fixed in this change**

- `ui/tests/backups.spec.ts` could not pass against a default deployment: the
  Playwright request context sent no `Remote-User`, so every owner-facing buddy
  call was 401 under `buddy.requireAuth=true`. It now sets the header (what
  Traefik's `forwardAuth` injects; the NodePort variant is NAS-008), uses an
  unambiguous heading locator (`exact: true` — "Backups" also matched "Stored
  backups"), picks a child dataset (the pool root is refused by the agent:
  `dataset must be <pool>/<name>`), and uses the instance's own name as the source
  prefix so the self-send is inside the peer's scope.
- `/backups` rendered the receiver's **Stored** column from `b.bytes`, but the API
  sends `storedBytes`, so it always showed `0 B` — fixed.
- The schedule table had no **Source** column (two schedules on one dataset were
  indistinguishable) — added, which is also what the spec's test expects.
- The dataset pickers offered the pool root, which the agent always refuses —
  they now list child datasets only.
- Added two page-level tests: "Back up now" driving a real job with Verify
  reading the chain back, and the restore form refusing an unconfirmed/incorrect
  destination before any request.

**Findings to follow up (not fixed here)**

1. **Cancel is not prompt while a chunk request is in flight.** `DELETE` sets the
   job context, but `buddy.Client`'s HTTP calls are not context-bound
   (`PushOptions`/`RestoreOptions` carry no context); the job only turns
   `cancelled` when the next read of the agent stream fails. Live: with a receiver
   that stalled 30 s, `DELETE` returned `cancelling` immediately but the job stayed
   `running` for the full 30 s. Thread a context through the client
   (`http.NewRequestWithContext`) so cancel aborts the in-flight request.
2. **A backup of a dataset that is not mounted in the host namespace succeeds
   while storing nothing.** A `dd` of 512 MiB / 2 GiB into datasets created from
   inside a pod landed on the **parent** dataset (the child datasets read 96 K,
   `test` grew to 2.76 G), and the sends reported `succeeded` with one 44 KB chunk
   — an empty backup over a green result. This is the documented
   `mountPropagation: HostToContainer` trap, but on the *send* path it is silent:
   the agent/API should verify the dataset is mounted in its namespace before
   snapshotting, or the send should warn.
3. **`pruneKeep` can leave only unrestorable incrementals.** With `pruneKeep: 1`
   on a chain whose newest backup is incremental, the base chain is pruned; the
   send and the schedule both report success, and only `verify`/restore later
   refuses: `chain … needs the chain that produced GUID …, which is not stored
   here`. Retention needs to count the sequence depth, or the UI/docs must warn.
4. **Schedule dataset validation is a loose pool-prefix match** (audit NAS-018,
   now confirmed live): `{"dataset":"test/nope"}` was accepted with 200. It fails
   later at send time, but the schedule looks valid.
5. **Minor:** a job can report a `snapshot` name that was never created (the
   field is set before `zfs create`/snapshot succeeds); a never-run schedule
   serialises `lastRun` as `"0001-01-01T00:00:00Z"` (`time.Time` ignores
   `omitempty`); a `zfs receive` destination stays mounted in the **agent's**
   namespace, so host-side `zfs destroy` reports `dataset is busy` until the
   agent pod restarts (`test/drill-api-restored`, empty 96 K, was left behind).
6. Deploy notes: the chart still does not pass `BUDDY_SCHEDULES` or
   `BUDDY_SCHEDULER_INTERVAL_MS` (defaults are used), `buddy.*` is not in
   `values-vm.yaml` so an upgrade must repeat the `--set` flags (or use
   `--reuse-values`), and notification settings are still in-memory
   (`notifications.NewManager("")`), so they reset on restart.

## Shares & LDAP account sync (in `master`)

### What was broken
Shares were **metadata only**. `server.New` called `shares.NewManager("")`, so
every share was lost on restart, and nothing in the cluster served SMB — the
generated `smb.conf` was returned as text over the API and consumed by nobody.
`syncSMBPassword`/`removeSMBUser` were **stubs that only printed**, so LDAP
users could never authenticate over SMB.

### What now works (verified end-to-end on the VM)
- Share definitions persist (`SHARES_CONFIG`, PVC) and are validated
  (canonicalised paths — the old prefix check allowed `/var/mnt/../etc`).
- The privileged agent writes rendered config to `/var/lib/naslos/shares`; the
  `naslos-samba` DaemonSet serves SMB on the node's :445 and reloads itself,
  only after `testparm` accepts the new file.
- **LDAP → Samba sync is fully automated**: creating a user through the API is
  enough. NT hashes go to the passdb (imported with `pdbedit -i smbpasswd:`)
  and the POSIX identity goes to NSS `extrausers` files, so no account is ever
  created on the node. Verified: create → login, change password, disable
  (`NT_STATUS_ACCOUNT_DISABLED`), enable, delete — all without manual steps.
- API: `/api/shares/paths`, `/api/shares/status`, `/api/shares/apply`.
- The shares UI shows each share's `smb://<host>/<name>` address (host taken from
  the browsing URL) with a copy button, and no longer offers AFP (the API
  rejects it).
- **Share paths can be folders inside a dataset, and the UI can create them.**
  The Path field is a folder picker (dataset → subfolders → `Create folder`), so
  one dataset can hold several shares (`/var/mnt/test/media`, …) without ever
  leaving the pool: folder creation goes through the agent and is refused for
  anything not on a dataset, for a missing parent, and for traversal. Shares can
  be repointed at another folder when edited (validated like a create), and a
  share can no longer be created on a folder that does not exist. Verified live:
  created `/var/mnt/test/media` from the API, shared it over NFS, mounted it from
  a client, wrote a file — and it landed in that folder on the pool. 18/18
  Playwright tests pass.
- **Datasets can be created, listed and destroyed from the pool page.** `Pools →
  <pool>` now has a Datasets list (name, mountpoint, used/free) with **New
  Dataset** (name, compression, quota) and per-row **Delete**. Verified live:
  created `test/media` with zstd + a 500G quota and `zfs list` on the node showed
  exactly those properties; the dataset then appeared in `/api/shares/paths`, so
  it is immediately shareable.
- **A pool can be grown with Add Drive** (`zpool add`), with the disk picker
  marking disks that are already in a pool as unselectable. No spare disk exists
  on the VM (both `/dev/vdb` and `/dev/vdc` belong to `test`), so the *success*
  path was verified only by unit tests asserting the exact `zpool add` command
  line — the live checks cover the refusals, which is what protects the pool:
  a member disk is refused even with `force: true`, a non-`/dev` path, an unknown
  disk, an unknown topology, and too few disks for a RAIDZ level are all 400s,
  and the pool's members are unchanged afterwards.
- **Destruction is guarded**: destroying a dataset a share serves is a 409 with
  the share named, a pool's root dataset is refused, and a non-empty dataset
  needs `recursive=true` (the UI asks in those terms).
- **The web terminal is real.** It used to be a facade: `handleExecWS` streamed
  output with `Stdin: nil` (keystrokes were read and thrown away), never sent a
  window size, defaulted the container to `main`, and nginx had no WebSocket
  upgrade headers at all - so nothing could work. Now: pickers for
  namespace/pod/container fed by new `/api/namespaces` and `/api/pods` endpoints
  (the shell container is marked and preselected), a privileged `naslos-terminal`
  container the chart deploys (`terminal.enabled`, FR-LOG-02), a real stdin pipe,
  resize messages, shells limited to bash/sh/ash/zsh, least-privilege RBAC (Role
  in the namespace + namespaces read), and a preflight GET that turns "pod not
  found" into a message the UI can show.
- **Verified in a browser**: Playwright attaches, waits for the prompt, runs
  `echo`, `id -u` (⇒ 0) and `zpool list` (⇒ the real pool), then disconnects.
  That exercises nginx's upgrade path, the API's exec, the RBAC and the host
  tooling in one go.
- Two bugs that test caught: nginx forwards `Host` **without** the NodePort while
  the browser's `Origin` keeps it, so a port-sensitive origin check refused the
  terminal's own UI; and `chroot /host /host/usr/local/sbin/zpool` is wrong -
  paths after a chroot are relative to the *new* root.
- **The terminal is gated on an authenticated session (FR-LOG-08).** The API
  requires the identity header the authenticating proxy injects (`Remote-User`,
  via Traefik `forwardAuth`, whose `authResponseHeaders` replaces any value a
  client sent) and fails closed with 401; the UI's nginx **refuses** the terminal
  paths outright (403), so the node-port listener - where nothing proves who is
  asking - cannot even forward them, and the chart routes them from Traefik
  straight to the API. `terminal.requireAuth: false` is the explicit opt-out.
- **NFS is served** by NFS-Ganesha in userspace (`naslos-nfs` DaemonSet,
  hostNetwork, NFSv4/TCP :2049) — Talos has no kernel `nfsd`, so the API renders
  Ganesha's config instead of `/etc/exports`. Verified live: a *separate client
  pod* mounted `192.168.1.96:/nfsproof`, listed, wrote, read back, created a
  directory, `df` showed the 38 G dataset, and unmounted cleanly. Ownership on
  the pool is the caller's real uid (uid 1000 → `1000:1000`, root → `0:0`), and
  permissions are enforced (uid 1000 was refused on a `755` root-owned dir).
  Changing a share reloads exports with `SIGHUP` — mounts are not interrupted.
- **Network discovery is live**: Avahi publishes `_smb._tcp` and `wsdd` provides
  WSD, so the server appears when browsing the network (verified: `avahi-browse`
  lists `naslos` at 192.168.1.96:445 next to the real `truenas`). Discovery is
  pinned to the default-route interface, otherwise avahi also advertises pod
  network (10.x) addresses.
- **Share access by group works**: a share can be restricted to LDAP groups
  (`validGroups` → `valid users = @group`), with the group's real membership
  mirrored into the node's NSS files. Verified live: member OK, non-member
  refused, and membership changes take effect without touching the share.
  Every identity change that affects access (membership, group delete, user
  create/delete) now re-pushes the mirror.
- **Password change → SMB**: measured 2.6–3.0 s until a *new* SMB login accepts
  the new password at the default `shares.confCheckInterval: 3`, and 0.8–1.1 s at
  `1`. The API call itself is ~25 ms; the delay is the serving container's poll.
  Existing sessions keep their old credentials until they reconnect.

### Two traps that cost most of the debugging time
1. **`SMB_CONF_PATH`**: `smbd` is started with `-s …/smb.conf` but
   `pdbedit`/`smbpasswd` default to `/etc/samba/smb.conf`, so they wrote a
   *different* passdb than the running smbd. Symptom: accounts "create" fine
   while every login returns `NT_STATUS_ACCESS_DENIED` (falling back to guest).
   The image now exports `SMB_CONF_PATH`.
2. **`!` in test passwords under zsh**: `NaslosTest123!` inside double quotes
   triggers history expansion and silently mangles the password. Use `%%` or
   single quotes when testing SMB from the shell.

### Known gaps (documented in docs/shares.md)
- **NFS is NFSv4-only** (no NFSv3: it would need `rpcbind`/`statd`, which Talos
  does not ship) and uses AUTH_SYS, so ownership is numeric uid/gid — `sec=krb5`
  is not configured. Clients resolve owner *names* through their own
  `rpc.idmapd`; a client without idmapping shows root-owned entries as `nobody`
  (display only — the file's uid on the pool is correct).
- An NFS share's `allowedHosts` maps to Ganesha `CLIENT` blocks and `readOnly`
  to `Access_Type = RO`; `No_Root_Squash` mirrors SMB's `force user = root`.
- `naslos-openldap-backup` CronJob is still in CrashLoopBackOff (pre-existing).
- `naslos-api`'s `AgentSharesStatus` struct still names the old agent status
  fields (`sambaRunning`, `nfsRunning`, `sambaTestOutput`, …), so
  `/api/shares/status` shows empty values for them. The agent reports
  `smbShareCount`/`nfsExportCount` instead; the API type should be updated.

### Environment notes
- Deploy: `helm upgrade naslos charts/naslos -n naslos -f charts/naslos/values-vm.yaml
  --set <component>.image.tag=<tag>` with `TALOSCONFIG=bootstrap/vm/talosconfig`.
  Bump tag suffixes per deploy (registry reuses `0.1.0` with `IfNotPresent`).
- Images: `make samba-image` / `make nfs-image` (new). Deployed tags at time of
  writing: api `0.1.0-s4`, agent `0.1.0-s5`, samba `0.1.0-s4`, ui `0.1.0-s1`.
- Playwright: 8 tests, all passing (`cd ui && npx playwright test`).
- Go tests: `api/internal/identity` (incl. NT-hash vectors cross-checked with
  OpenSSL) and `api/internal/shares` (smbpasswd rendering/persistence).

---

### Node reboot (observed, fixed)
The VM rebooted during this work (node uptime reset). Every pod restarted with
`Unknown`/exit 255, which is expected — but the UI showed **2 restarts** because
nginx resolves `naslos-api` at startup and refuses to boot before the Service is
in DNS:

```
[emerg] host not found in upstream "naslos-api" in /etc/nginx/conf.d/default.conf
```

Fixed by waiting for the name in an init container (`ui.waitForApi`, mirroring
the API's `wait-for-ldap`). Note: switching nginx to a `resolver` + variable
does **not** work here — nginx's own resolver ignores the pod search domains, so
the short name fails (returns 502); the literal name is resolved by the system
resolver, which applies them.

After the reboot everything else converged on its own: the API re-applied the
share configuration on startup (`applied: true`, revision set), and discovery,
the account mirror and group access were all healthy.

### Files "disappearing" from shares (reported, root-caused, mitigated)
Reported: files created on an SMB share vanished after a VM reboot.

What the node actually showed: only **one** pool exists (`test`, mirror of
`/dev/vdb`+`/dev/vdc`) mounted at `/var/mnt/test`; `/var/mnt/tank` and
`/var/mnt/Pog` are **plain directories on Talos's EPHEMERAL partition**, not
datasets. `zpool import` and `zpool import -D` both report nothing importable,
so whatever pool used to back those paths is gone.

Root cause: nothing required a share path to be a dataset. `AvailablePaths`
listed every child of `/var/mnt`, so the path picker *offered* `/var/mnt/tank`
and `/var/mnt/Pog`, and the API accepted them. Data written there is not in any
pool (no checksums/snapshots/redundancy), is wiped by a Talos upgrade, and is
shadowed - appearing to vanish - the moment a dataset is mounted over it.

Mitigated (all deployed):
- `GET /api/shares/paths` now returns **dataset mountpoints** from the agent.
- `POST /api/shares` **refuses** a path that is not on a dataset, naming the
  datasets that would work (`shares.PathOnDataset`, unit-tested).
- Mounts use `mountPropagation: HostToContainer`, so a dataset mounted after a
  pod starts is visible instead of the pod silently serving the underlying dir.
- `naslos-samba` logs each share path with its backing mount and warns for any
  path that is not on a mounted filesystem.
- `AvailablePaths` deleted so the trap cannot be reintroduced.

Verified: a share on `/var/mnt/test` refused nothing, an SMB write through it
appeared in the host's dataset view and survived a pod restart; a share on
`/var/mnt/tank` was refused with a 400 explaining why.

Note: my own earlier test files in `/var/mnt/tank` (hello.txt,
uploaded-from-smb.txt, auto-synced.txt, container-write-test) are still there on
EPHEMERAL - left untouched, and they are a live example of the trap.

## Earlier work: dashboard & metrics (`feature/dashboard-and-metrics`)
## Known issue (separate, unresolved)
`naslos-openldap-backup` CronJob is in CrashLoopBackOff on the VM as of
2026-09-13 — LDAP backups are failing. Not related to the LDAP-availability
fix below; needs its own investigation.

## LDAP availability after restart (fixed)
**Symptom:** after a restart, Users/Groups showed "Identity/LDAP is not
available…" although OpenLDAP was healthy, until the API pod was restarted.

**Cause:** `identity.NewClient` dialed LDAP eagerly at process start.
`server.New` turned any failure into a permanent `identity = nil` (its comment
"will retry on first use" was false — nothing retried). Losing the startup
race against OpenLDAP therefore disabled identity for the pod's whole life.
There were also no probes in the chart, so Kubernetes kept serving the
degraded pod.

**Fix:**
- `api/internal/identity/client.go`: `NewClient` no longer dials; added
  mutex-guarded lazy `EnsureConnection()` with a 2 s retry cooldown, plus a
  `do()` helper that drops a dead connection and retries the operation once.
  Removed the broken `reconnect()`/`extractHost()` dead code.
- `api/internal/server/server.go`: `identityUnavailable()` now attempts a
  reconnect and returns 503 **with the underlying cause**; added `/api/ready`
  (always 200; reports `ldap: up|down`).
- `charts/naslos/templates/api-deployment.yaml`: best-effort `wait-for-ldap`
  initContainer (~10 s, then starts anyway — deliberately NOT blocking, so an
  LDAP outage cannot take the dashboard down) + liveness on `/api/health` and
  readiness on `/api/ready`.
- Chart changes need `helm upgrade … --force-conflicts` (earlier
  `kubectl set image` owns the image field, so plain upgrade conflicts).

**Verified by live drill on the VM:** LDAP scaled to 0 → API restarted (503
naming `connection refused`, dashboard/metrics still 200) → LDAP scaled back →
`/api/groups` 200 **on the same API pod with 0 restarts**. Plus Go tests
`api/internal/identity/client_test.go`.

## Specification
`docs/spec.md` is now the **normative system specification** (RFC-2119 style):
stable requirement IDs (`FR-STO/IDN/SHR/APP/MET/LOG/NTF`, `SEC-*`, `NFR-*`,
`API-*`, `DM-*`, `VER-*`), full API contracts, data models, and the
acceptance-criteria ↔ Playwright-test mapping. Where the topical docs
disagree with the spec, the spec wins. Known gaps are marked **[OPEN]**
(e.g. FR-MET-10: per-interface IPs / multi-node aggregation). When a code
change alters a MUST in the spec, update the spec and its test in the same PR.

## Dashboard & metrics (in `master`)

### Live metrics on the main dashboard (latest work)
**Symptom:** `/api/metrics` and `/api/dashboard` always returned zeroed values
(cpu 0, memory 0, disk 0, empty system info); the dashboard showed placeholders.

**Root causes (all fixed):**
1. No collector existed — the `metrics.Manager` snapshot was never updated and
   `talos.GetSystemMetrics()` was never called by any code path.
2. `getDiskMetrics` used Talos `DiskUsage`, which only reports total size —
   no used/free. Talos `/` is also a read-only squashfs (256 KB).
3. Memory values were raw `/proc/meminfo` **KB**, displayed as bytes by the UI.
4. Network packet counters were incremented per interface instead of summed;
   the `/proc/net/dev` header line was parsed as an interface named "face".
5. `Kernel` struct tag in `api/internal/metrics/metrics.go` was malformed
   (`` `kernel"` ``) so it never serialized.

**Refresh cadence (5s):** the API collector defaults to
`METRICS_INTERVAL_SECONDS=5`, and the dashboard component polls
`/api/dashboard` every 5s (`REFRESH_INTERVAL_MS` in `Dashboard.svelte`,
with an in-flight guard and interval cleanup in `onDestroy`).

**Fixes:**
- `api/internal/server/metrics_collector.go` (new): background collector
  started from `Server.Start()`; collects immediately then every
  `METRICS_INTERVAL_SECONDS` (default 15s), publishing to the manager.
- `api/internal/talos/client.go`: disk now uses the `Mounts` RPC preferring
  `/var` (the EPHEMERAL partition) with `/` fallback; memory KB→bytes
  conversion; correct proc/net/dev parsing (colon-suffix filter, packet
  columns); kernel version from `proc/sys/kernel/osrelease`.
- `api/internal/server/metrics.go`: `/api/metrics` now overlays live agent ZFS
  pools (same as `/api/dashboard`) via a shared `agentPools()` helper.
- `ui/src/lib/components/Dashboard.svelte`: error banner, locale-formatted
  `updatedAt` (hides the Go zero time), System Info grid now 4 columns.

**Deployed:** `naslos-api:0.1.0-12`, `naslos-ui:0.1.0-7` (5s refresh cadence)

### Branding (sidebar logo)
- `ui/static/logo.png`: 128×128 downscale (13KB) of the 1254×1254 root
  `logo.png` (made with PIL; do not ship the 926KB original to the UI).
- `ui/src/lib/components/Sidebar.svelte`: 40px logo beside the "Naslos" title
  in the top-left header block.
- Gotcha: nginx's SPA fallback answers `200 text/html` for missing assets, so
  asset tests must assert `content-type` (e.g. `image/png`), not just `ok()`.
- Test: `ui/tests/logo.spec.ts` (asset is a real PNG, rendered ≤64px next to
  the title). Full suite: 8 passing tests.
(roll via `kubectl -n naslos set image deployment/naslos-api api=…` — note the
container names are `api` and `ui`, not the deployment names).

**Tests:** `ui/tests/dashboard.spec.ts` (3 tests) asserts API values are live
(hostname/cores/memory/updatedAt), that pools render, and that the dashboard
re-renders on the 5s poll (mocked API with incrementing values); the full
suite is 7 passing Playwright tests against `http://192.168.1.96:30080`.

---

## Earlier work: users & groups (branch `feature/users`)

### Context
The Users and Groups management pages in the Naslos UI would freeze on load,
preventing creation of new users and groups. Investigation found multiple
root causes across the frontend, API, and LDAP layer.

## Issues Found & Fixed

### 1. UI Freeze on Empty API Responses
**Symptom**: Pages stuck on "Loading…" when LDAP had no users or groups.
**Root cause**: `/api/users` returned `null` (instead of `[]`) when `ListPeople`
returned `nil`. Svelte then crashed trying to call `.length` on `null`.
**Fix**:
- `api/internal/identity/persons.go`: `ListPeople` now initializes with
  `[]Person{}` and returns an empty slice, not `nil`.
- `api/internal/identity/groups.go`: `ListGroups` returns `[]Group{}`.
- `filterPlaceholderMembers` returns `[]string{}`, not `nil`.
- `ui/src/routes/users/+page.svelte`: Added `Array.isArray(data) ? data : []`
  guard and always sets `loading = false` on error.
- `ui/src/routes/groups/+page.svelte`: Same guard pattern applied.

### 2. Groups Page Column Misalignment
**Symptom**: The Users table columns shifted/misaligned, especially when group
badges wrapped across multiple lines.
**Root cause**: Default table layout resized columns based on content width.
**Fix**: `ui/src/routes/users/+page.svelte` now uses:
- `table-layout: fixed` with explicit column widths (`w-48`, `w-56`, `w-28`)
- `align-top` on all `<td>` cells so multi-line content doesn't push sibling
  columns down
- `truncate` + `title` attributes on long text (UID, email, display name)
- `whitespace-nowrap` on group badges

### 3. Group Short Names
**Symptom**: Group membership badges showed full DNs like
`cn=naslos_users,ou=groups,dc=naslos,dc=local`.
**Fix**: Added `shortNames()` helper in `api/internal/identity/persons.go`
that extracts `cn=` or `uid=` values from DNs. Applied in both `GetPerson`
and `ListPeople`. Also applied `shortName()` in the UI's
`ui/src/routes/groups/+page.svelte` and `ui/src/lib/components/GroupForm.svelte`.

### 4. Empty Group Description Rejected by OpenLDAP
**Symptom**: Creating a group with an empty Description field returned
"LDAP Result Code 21 — Invalid Attribute Syntax".
**Root cause**: `CreateGroup` always set the `description` LDAP attribute,
even to an empty string. OpenLDAP's `description` syntax rejects empty values.
**Fix**: `api/internal/identity/groups.go` now only adds the `description`
attribute when it's non-empty:
```go
if description != "" {
    addReq.Attribute("description", []string{description})
}
```

### 5. Enable/Disable User
**Symptom**: The enabled/disabled status wasn't working correctly for some
users (e.g., `florentin`).
**Root cause**: `isPersonEnabled` didn't handle all `shadowExpire` values
correctly.
**Fix**: `isPersonEnabled` in `api/internal/identity/persons.go` now treats:
- `""` (empty) or `-1` → enabled (never expires)
- `"0"` → expired/disabled
- Any other value → not expired (enabled)

### 6. Placeholder Members in groupOfNames
**Root cause**: The `groupOfNames` LDAP objectClass requires at least one
`member` attribute. Creating a group with no members fails.
**Fix**: `CreateGroup` adds a placeholder member (`cn=empty-members,ou=groups,...`)
during creation. `filterPlaceholderMembers` removes it from all API responses.

## Tests Added
Playwright E2E tests in `ui/tests/`:
- `tests/users.spec.ts` — Users page loads and can open the New User form.
- `tests/groups.spec.ts` — Groups page loads, can open New Group form, and
  can create a group with an empty description.
- `tests/e2e.spec.ts` — Full lifecycle: create user → create group → edit
  group to add member → verify short names displayed (not full DNs) → delete
  group → delete user.

All tests run against the deployed VM at `192.168.1.96:30080`.

## Files Modified
- `api/internal/identity/persons.go` — `ListPeople`, `GetPerson`, `shortNames`,
  `isPersonEnabled`
- `api/internal/identity/groups.go` — `ListGroups`, `CreateGroup`,
  `filterPlaceholderMembers`
- `ui/src/routes/users/+page.svelte` — table alignment, error handling
- `ui/src/routes/groups/+page.svelte` — short names, table layout
- `ui/src/lib/components/GroupForm.svelte` — short names in member selection
- `ui/tests/e2e.spec.ts`, `ui/tests/users.spec.ts`, `ui/tests/groups.spec.ts`
- `docs/api.md` — Users & Groups API route documentation
- `docs/identity-sso.md` — UI management section

## Deployment
Current live images on the VM: `naslos-api:0.1.0-12` and `naslos-ui:0.1.0-6`.
Registry: `192.168.1.2:30095`. Because the registry reuses tags and nodes pull
with `IfNotPresent`, always retag to a fresh suffix (e.g. `0.1.0-13`) and
`kubectl -n naslos set image` before rolling out.
**Pitfall:** never run `make *-image` and `docker tag/push` concurrently — the
tag can capture the stale `0.1.0` image before the build finishes (this shipped
old content under a new tag once). Chain them with `&&` instead, and verify the
pushed digest matches the local one (`docker images --digests`).

## Git Context
- Branch: `feature/dashboard-and-metrics` (current work)
- Commits on `feature/users` (already landed):
  - `ab482b4` — adds test for empty group description
  - `1869b9c` — fix: allow empty description when creating groups
  - `40608b2` — align users UI columns and ensure consistent table layout
  - `e22fc05` — fix(users UI): align table columns and show short group names
  - `7bc4c87` — fix(users/groups): prevent UI freeze on empty API responses

## Next Steps / To-Dos
- `/api/metrics/network.interfaces` and `/api/dashboard` do not yet expose
  per-interface IP addresses (the collector parses them but the dashboard
  does not render the interface list).
- Metrics are per-node only; no aggregation across the cluster's nodes yet.
- The `naslos-agent` image tag is stale (`0.1.0`); consider bumping all
  images consistently for the next release.
- The `description` attribute on groups is still returned as `""` when
  unset; consider omitting it from JSON responses for consistency.
- Add validation feedback in the GroupForm UI when description is optional.
