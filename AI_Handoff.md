# AI Handoff — Naslos

## Ops: image digests (`feature/image-digests`)

Branched from master `42df19e` (PR #14 merged). Chart-only change; no image
rebuild was needed.

**NAS-022 — digest pinning.** Every `<component>.image` block gained a `digest`
field, and a new `naslos.image` helper renders `repository@sha256:…` when it is
set (accepting the value with or without the `sha256:` prefix) and
`repository:tag` otherwise. `make image-digests` prints each built image's digest
to paste in. `openldap.image` is already a full image reference, so it is pinned
directly. VER-2 in `docs/spec.md` and `docs/deployment.md` now document the rule,
including the trap that **a stored digest wins over a new tag on the next
`--reuse-values` upgrade**, so switching back needs an explicit
`--set api.image.digest=`.

Live: upgraded with `api`/`ui` pinned to their `sha256:` digests — both
Deployments rendered `repo@sha256:…`, `/api/ready` 200, Playwright **30 passed / 2
skipped / 0 failed** — then reverted to tags (revision 80) to keep the dev VM
simple for drills. `helm lint` clean and `helm upgrade --dry-run` against the live
release values renders.

**Still open (low priority)**

- NAS-021's bounded FS walks (pagination of long chain listings) — an availability
  note, not a vulnerability.
- NAS-014's optional nonce-cache persistence (only if a restart-window replay is
  judged to matter after the idempotency analysis above).

## Polish: interface addresses + accessibility (`feature/dashboard-network`)

Branched from master `65e212e` (PR #13 merged). Deployed: api `0.1.0-b20`,
ui `0.1.0-b9` (helm revision 78), agent `0.1.0-b6`.

**FR-MET-10 — per-interface addresses (was `[OPEN]`).** `/proc/net/dev` carries
per-interface counters but no addresses, so the Talos client now reads the node's
`AddressStatus` resources (COSI, the same data `talosctl get addresses` shows) and
fills `NetworkInterface.IPAddress`. The selection is deterministic: routable IPv4
over IPv6, never loopback, link-local or multicast, and a link with no usable
address simply has none. A failure to read them is not fatal — the dashboard still
shows names and totals. The dashboard view model never carried `network` at all,
so that was added too (it is what the UI reads). The UI shows one line per
*addressed* interface (a node has dozens of veth/pseudo interfaces with none,
which would drown the card) plus received/sent totals.
Live: `enp1s0 → 192.168.1.96`, `cni0 → 10.244.0.1`, `flannel.1 → 10.244.0.0`.
Tests: `TestAddressesByLink`, `TestAddressesByLinkPrefersIPv4OverIPv6`, and a
Playwright case in `dashboard.spec.ts`. Multi-node aggregation stays `[OPEN]`
(single-node product), and FR-MET-10 in `docs/spec.md` now documents the rule.

**Accessibility: 45 `svelte-check` warnings → 0.** Form labels are associated with
their controls (`for`/`id`) in `UserForm`, `ShareForm`, `GroupForm`, the
notifications page, the pool detail page and the terminal page; the checkbox
groups (Groups/Members/Allowed Groups) and the dataset picker are labelled with
`role="group" aria-labelledby` (a plain label cannot name a group of controls);
modal backdrops are `role="presentation"` (they dismiss on an outside click, which
is decorative — Escape and the close button remain the keyboard paths); and the
catalog cards are keyboard-reachable (`role="button"`, `tabindex="0"`,
Enter/Space). No test selectors depended on the old markup: Playwright **30 passed
/ 2 skipped / 0 failed** after the change.

Note: these two commits were briefly committed on `master` by mistake and moved to
this branch before pushing; `master` is untouched at `65e212e`.

**Still open**

- NAS-021's bounded FS walks (pagination of long chain listings) — an availability
  note, not a vulnerability.
- Image tags are still reused (`0.1.0` + `IfNotPresent`) and the chart has no
  digest pinning (NAS-022): always bump the suffix per deploy, or add
  `image.digest` support.
- NAS-014's optional nonce-cache persistence (only if a restart-window replay is
  judged to matter after the idempotency analysis in the previous section).

## Phase 2 hardening, part 2: quota, replay cache, envelope bounds
## (`feature/buddy-hardening`)

Branched from master `aaaf95f` (PRs #11/#12 merged). Deployed: api `0.1.0-b18`
(helm revision 74), agent `0.1.0-b6`, ui `0.1.0-b6`.

**NAS-013 — quota.** The check and the charge now happen under the store lock,
before the bytes are written (`Store.PutChunkWithin` + `reserveUsage`, released on
a failed write), so two concurrent uploads cannot both pass a stale check. A
chunk that is already stored with the same digest is a free no-op, so resuming a
chain that fills the quota works. Manifest bytes are counted (the cached usage is
invalidated after a manifest write) and `dirSize` now ignores the store's own
dot-prefixed temp artifacts — a stray temp file from a crashed write used to
inflate a key's usage until restart. `ValidateQuota` rejects a negative quota, one
smaller than a single *sealed* chunk (`MinPeerQuota = MaxSealedChunkSize`) and an
implausibly large one; 0 stays the documented "unlimited".
Live: after the key filled up, a further chunk → `413 quota exceeded: 1052672 of
1052672 …`; a quota of 1024 → `400 quota must be at least 1052672 bytes (one
chunk), or 0 for unlimited`; a 1 MiB chunk stored and the source listed.
Note: the usage cache is in-memory, so deleting files outside the API does not
invalidate it (pre-existing; a pod restart recomputes).

**NAS-014 — replay cache.** It is now per key (`map[keyID]map[nonce]expiry`) with
a `maxNoncesPerKey` bound, so one key can neither grow the cache without limit nor
evict another key's entries; expired nonces are dropped as that key is used. Nonces
are format-checked (base64 decoding to 16..64 bytes, ≤128 chars) before they enter
the cache, so a peer cannot store megabyte-long keys. Residual risk, documented:
a nonce evicted under the bound, or the cache itself across a restart, could allow
a replay inside the 5-minute skew window — every operation a replay could repeat is
idempotent (chunk writes are digest-checked, a manifest cannot roll the pointer
back), which is why the bound was preferred to persistence. Tests:
`TestNonceFormatIsValidated`, `TestReplayCacheIsBoundedPerKey`.

**NAS-021 — envelope and store bounds.** The 32-bit nonce counter is guarded
(`MaxChunkIndex`, refused in `SealChunk`/`OpenChunk`) so an index cannot wrap and
reuse a nonce; `UnwrapDEK`'s length floor now includes the GCM tag (12+32+16);
and `sourcesIn` walks recursively, so a three-level source is listed instead of
being hidden while the quota counted its bytes. Tests:
`TestChunkIndexOverflowIsRefused`, `TestSourcesInFindsNestedSources`.
Live: `quota-test/nested/deep` appeared in `/api/buddy/status`.

Playwright **29 passed / 2 skipped / 0 failed**; `api` suite green; the VM is back
to baseline (test peer revoked, store emptied, `.enroll-used` removed).

**Still open**

- NAS-014's persistence option (only if a restart-window replay is judged to matter
  after the idempotency analysis above).
- NAS-021's bounded FS walks (pagination of long chain listings) — an availability
  note, not a vulnerability.
- Low-priority polish: FR-MET-10 per-interface IPs `[OPEN]`; 45 `svelte-check`
  warnings; image tags reused (`0.1.0` + `IfNotPresent`) — always bump the suffix.

## Phase 2 hardening: injection + receiver integrity (`feature/polish-gaps`)

Deployed: api `0.1.0-b16` (helm revision 72), agent `0.1.0-b6`, ui `0.1.0-b6`.

**NAS-006 — LDAP filter/DN injection (High).** Every uid/cn is now validated
against an allowlist (`^[a-z0-9][a-z0-9._-]{0,63}$`, the appliance's own
convention) *and* escaped: filters through `ldap.EscapeFilter`, DN components
through `ldap.EscapeDN` (`api/internal/identity/escape.go`). All 16 interpolation
sites in `persons.go`, `persons_extra.go` and `groups.go` go through the
helpers, so a `*`, `(`, NUL, `,` or space can neither alter a search nor break out
of a DN. Group names are normalized (lowercased) so creation and lookup agree.
Tests: `TestValidateIdentityNameRejectsMetacharacters`,
`TestGroupNamesAreNormalizedAndValidated`, `TestSearchValuesAreEscaped`.
Live: `POST /api/users {"uid":"*)(uid=*"}` → 400, `POST /api/groups
{"cn":"evil,ou=admin"}` → 400.

**NAS-007 — smb.conf / Ganesha config injection (High).** Share text fields
(description, path, allowed hosts, valid users/groups) reject newline/CR/NUL/tab,
and the list fields also reject `;` and `"` (a Ganesha statement terminator and
string delimiter). Share *names* reject control characters too. `validateShareFields`
runs on create, on update (rolling the in-memory share back on failure) and on
**load** — the config file is hand-editable, so an already-stored share whose
fields would render into directives is skipped rather than served. `smbusers.go`
now enforces `^[0-9A-F]{32}$` for NT hashes, and rejects `:`/control characters in
the uid, gecos and home directory (those files are colon-separated records).
Tests: `api/internal/shares/injection_test.go`.
Live: a description with `\n` → 400 `description must not contain control
characters`; `validUsers: ["alice; rm -rf"]` → 400; a clean share still creates
(201) and deletes.

**NAS-012 — receiver integrity (Medium).** Three changes, all receiver-side, since
the receiver cannot decrypt and must not trust sender metadata:

- `validateManifestShape` checks the sender-controlled manifest before storing it:
  known payload kind, non-zero creation time, `chunkPlainSize` equal to the stored
  chunk size, base64 stream prefix of 8 bytes, a decodable wrapped data key, and
  chunks with strictly ascending indices from 0, sane plain/sealed sizes and
  64-hex plaintext digests.
- `Store.PutManifest` refuses to move `current` back to a chain that is already
  stored (a rollback: a compromised or stale key replaying an older manifest).
  Re-publishing the *current* chain stays allowed — that is what a resumed push
  does — and rotations are logged.
- `Store.Prune` bases survivorship on the receiver's own `current.json` pointer,
  not on the sender's `CreatedAt`, and never prunes a chain the current backup
  descends from. A rewritten timestamp can no longer make the live chain look
  oldest. Tests: `TestManifestShapeValidation`,
  `TestReceiverRefusesChainRollback`, `TestPruneSurvivesAForgedTimestamp`.
- Live regression on the normal path: two sends (second incremental) with
  `pruneKeep: 1` kept both chains (`prunedChains` absent) and `verify` walked the
  sequence.

Playwright **29 passed / 2 skipped / 0 failed**; `api` Go suite green; the VM is
back to its baseline (test share, directory, chains and `.enroll-used` removed).

**Still open**

- NAS-013 (quota check-then-act race, manifest-byte undercount, `QuotaBytes`
  validation), NAS-014's remaining half (persist the nonce cache across restarts,
  cap its growth; the 128-bit job ids landed in Phase 2), NAS-021's cosmetic items
  (stream-counter overflow refusal, 2-level source walk, `LimitReader` truncation
  error, bounded FS walks).
- FR-MET-10 per-interface IPs `[OPEN]`; 45 `svelte-check` warnings; image tags are
  still reused (`0.1.0` + `IfNotPresent`) — always bump the suffix per deploy.

## Phase 3 start: product gaps (`feature/polish-gaps`, implemented)

Branched from `feature/buddy-fanout` (still open), so the PR carries both until
fan-out merges. Deployed: api `0.1.0-b15` (helm revision 71), agent `0.1.0-b6`, ui
`0.1.0-b6`.

**Notification settings persist.** They were constructed with an empty path
(`notifications.NewManager("")`), which the manager treats as memory-only, so every
API restart silently reset the operator's topic, token, event list and severity.
The manager now reads `NOTIFICATIONS_CONFIG` (chart value `api.notificationsFile`,
default `/var/lib/naslos/notifications.json`, i.e. the same volume as the share
config). Verified live: set a distinctive config, `rollout restart`, and the
settings came back unchanged — then the defaults were restored and are also
persisted. Unit test: `TestSettingsSurviveARestart`, plus
`TestMemoryOnlyManagerStillWorks` to keep the empty-path mode intentional.

**Never-run schedules no longer report a year-1 timestamp.** `omitempty` does
nothing for `time.Time`, so a fresh schedule serialised
`"lastRun":"0001-01-01T00:00:00Z"`. `buddyScheduleEntry` now marshals those two
timestamps as pointers (omitted when zero, present once a run happens). Test:
`TestBuddyScheduleJSONOmitsUnsetTimestamps`.

**OpenLDAP backup CronJob repaired** (`openldap/manifests/backup-cronjob.yaml`).
It mounted the config PVC at `/var/lib/ldap/backups` — *inside* the data PVC's
read-only mount — which the runtime cannot create, so every run died with
`RunContainerError` and nothing was ever backed up. It now mounts both PVCs at
their real paths (`/var/lib/ldap`, `/etc/ldap/slapd.d`) plus the config volume
again at `/backups` as the destination, read-write on both (the mdb backend maps a
lock file next to the database, so a read-only mount fails even for a pure read),
uses `slapcat -F /etc/ldap/slapd.d` for both databases, and prunes to the newest 7
of each. Verified live with `kubectl create job --from=cronjob/…`: the job
completed and the volume holds `config_*.ldif` (12 entries) and `data_*.ldif`
(8 entries). The drill job and the stale failed job were deleted.

**Stale note corrected:** the `/api/shares/status` "empty values" gap from the
earlier roadmap no longer exists — the API's `SharesConfigStatus` matches the
agent's JSON and the live response reports `smbShareCount: 3, nfsExportCount: 1`.
Do not re-investigate it.

**Still open**

- Hardening: NAS-012 (manifest rollback protection), NAS-013 (quota
  race/undercount), NAS-021 (counter overflow, FS walks), NAS-006/007 (injections).
- FR-MET-10 per-interface IPs `[OPEN]`; 45 `svelte-check` warnings (a11y); image
  tags are still reused (`0.1.0` + `IfNotPresent`), so always retag per deploy.

## Phase 2 continued: fan-out + peer exposure (`feature/buddy-fanout`, implemented)

Branched from `master` at `8cd75b8` (both earlier PRs merged: #9 buddy, #10
Phase 2). Deployed for the validation: api `0.1.0-b14`, ui `0.1.0-b6`, agent
`0.1.0-b6` (helm revision 70), `auth.disabled=true` (the VM's dev posture).

**Peer-exposure decision — taken, and verified live.** No dedicated listener: the
peer API rides the same ingress/node port the UI uses. `/api/buddy/v1/*` stays
public (a peer authenticates with its own Ed25519 key and cannot complete an
interactive login) while every owner-facing buddy route goes through the shared
owner gate. With the gate armed, `/api/users` → 401 without the proxy secret while
`buddyctl enroll` + `push` through that same `:30080` listener succeeded; SEC-9
still holds (the agent's streaming endpoints stay ClusterIP-only and are not
proxied by the UI's nginx). Recorded in `docs/buddy-backup.md` §9.

**Fan-out (FR-BUD-15).** A schedule (or "Back up now") can name several buddies:

- The entry gained `receivers []string` (with `receiver` still mirroring the first,
  for compatibility) and `receiverResults map[receiver]ok|failed`. Validation
  normalizes every URL, requires at least one, and collapses duplicates.
- The runner enqueues one job per destination, each an independent chain with its
  own resume state, so a dead or busy buddy fails only itself. `lastResult` is `ok`
  only when every destination stored the run; `lastError` names the failing buddy
  and keeps the underlying reason even when another destination succeeds afterwards
  (the first version blanked it — caught by the strengthened test and re-verified
  live).
- The job-level exclusion was relaxed from (receiver, source, **dataset**) to
  (receiver, source): concurrent fan-out jobs share one dataset by design, and each
  snapshots under its own unique name, so the dataset exclusion was both
  unnecessary and blocking. The manual-send 409 for a duplicate (receiver, source)
  is unchanged.
- UI: the schedule form takes one buddy URL per line; the table shows "N buddies"
  with a per-destination ✓/✗ from the last run; "Back up now" fans out too and
  tracks the first destination's progress.

**Live validation (VM, 2026-09-17).** A `daily` schedule with
`receivers=[self, http://127.0.0.1:1]` fired: the self job `succeeded` with a stored
chain, the dead job `failed` with the connection-refused error, the entry reported
`lastResult: failed`, `receiverResults {self: ok, dead: failed}` and
`lastError: backup to http://127.0.0.1:1 failed: … connection refused`. Playwright
**29 passed / 2 skipped / 0 failed**; `api` Go suite green; `vite`/`svelte-check`
0 errors. The schedule, the test chains, the test peer and the `.enroll-used`
marker were removed afterwards, so the VM is back to its baseline.

**Still open**

- Hardening: NAS-012 (manifest rollback protection), NAS-013 (quota
  race/undercount), NAS-021 (counter overflow, FS walks), NAS-006/007 (injections).
- Phase 3 product gaps: notification settings are still in memory
  (`notifications.NewManager("")`); `/api/shares/status` still returns empty values
  (agent field-name mismatch); `naslos-openldap-backup` CronJob in CrashLoopBackOff;
  FR-MET-10 per-interface IPs `[OPEN]`; 45 `svelte-check` warnings; `lastRun`
  serialises `0001-01-01T00:00:00Z` for a never-run schedule.

## Phase 2: buddy completion (`feature/buddy-completion`, merged via PR #10)

Stacked on `feature/security-fixes` (Phase 1, still unmerged), because the
peer-exposure decision and the owner gate belong together. Commits: the
correctness batch and the cancel follow-up.

**Deployed for the validation:** api `0.1.0-b12`, agent `0.1.0-b6`, ui `0.1.0-b5`
(helm revision 66), `auth.disabled=true` (the VM's documented dev posture).

**What changed**

- **Prompt cancel (FR-BUD-16).** `PushOptions`/`RestoreOptions` carry a context, the
  client's requests use `http.NewRequestWithContext`, and the loops check it between
  chunks. The base-manifest lookup needed the same treatment: the first live check
  still waited out the receiver's stall there, so `ManifestContext` /
  `ChainsContext` / `RestoreSequenceContext` exist for the API's job paths.
- **Refuse an unmounted dataset (FR-BUD-11).** The agent reports each dataset's
  `mounted` state (host mount namespace, where `zfs send` runs); a send refuses a
  dataset whose mountpoint is a path but is not mounted, with the fix in the
  message. This is the mount-propagation trap: a dataset created from inside a pod
  lives in that pod's namespace, so a send captured an empty filesystem and still
  reported success (observed as a 44 KB "backup" of a 2 GiB dataset).
  Deliberately conservative: an unmounted dataset with a real mountpoint is refused
  even though it may hold its own data; mount it (or set mountpoint `none`) first.
- **Retention never orphans an incremental (FR-BUD-09).** `Store.Prune` walks the
  newest chain's `FromGUID` links and keeps every chain it descends from, even when
  `keep` is smaller. Keeping more chains than asked is correct; an incremental
  without its base reported success and only failed at verify/restore time.
- **Single-use enrollment across restarts (SEC-12).** The receiver records the spent
  token in the store (`.enroll-used`), so a restart cannot re-arm it as it did
  before. `PeerStore.Add` also refuses to replace an existing name with a different
  key (a leaked token could otherwise substitute a peer's key invisibly).
- **Job ids are 128-bit (SEC-13)**, up from 32 bits.
- **Schedules only accept a dataset that exists (FR-BUD-17)**: an exact match
  against the agent's list instead of a pool-prefix guess (NAS-018).
- Chart: `BUDDY_SCHEDULES` and `BUDDY_SCHEDULER_INTERVAL_MS` are passed explicitly
  (`buddy.schedulesFile`, `buddy.schedulerIntervalMs`) instead of relying on the
  compiled-in defaults.
- The dead pre-jobs synchronous send (`handleBuddySendSyncLegacy`, never routed) is
  deleted: it was a second, unguarded copy of the send path.

**Live validation (VM, 2026-09-17)**

- Unmounted refusal: a dataset created from inside the terminal pod reported
  `mounted:false` (its 64 MiB had gone to the parent), the send failed in 8 ms with
  `refusing to back up: dataset test/trapcheck is not mounted on the node …`, no
  snapshot was created and nothing was stored. The other test datasets all report
  `mounted:true`, so the rule does not over-refuse in practice.
- Retention: a full send (106 chunks) plus an incremental with `pruneKeep:1` kept
  **both** chains (pruned 0) and `verify` walked the sequence successfully —
  previously that drill left one chain and verify failed with the missing-base
  error.
- Cancel: with a receiver stalling 30 s per request, `DELETE` reached `cancelled` in
  ~4 s wall clock (the DELETE round trip), against the full 30 s before the fix.
- Enrollment: first enroll 201; a second attempt 403 in the same process and
  **still 403 after an API restart** (the marker is on the PVC). Re-keying
  `enroll-a` through `POST /api/buddy/peers` → 400 `already exists with a different
  key`.
- Job ids in the responses are 32 hex characters.
- Playwright: **29 passed / 2 skipped / 0 failed**. Go suites green for `api` and
  `agent`; `helm lint` clean.
- Cleanup afterwards: the test peer revoked, the receiver store emptied, the
  listener removed, the trap dataset destroyed, and the `.enroll-used` marker
  deleted so the VM's enrollment token works again for the next drill (that file is
  the only place the "token spent" state lives; delete it to re-arm the token).

**Still open in Phase 2**

- **Multi-buddy fan-out** (one source → several receivers): not started; the
  schedule entry still holds a single receiver.
- **Peer-exposure decision**: recommended (and now unblocked) — keep the existing
  Traefik + Authelia path with the `proxy-identity` secret; no dedicated listener.
  Needs a decision + a doc paragraph, not code.
- Hardening still open: NAS-012 (manifest rollback protection), NAS-013 (quota
  race/undercount), NAS-021 (counter overflow, FS walks).
- Polish: notification settings are still in memory (`notifications.NewManager("")`);
  a never-run schedule still serialises `lastRun` as `0001-01-01T00:00:00Z`.

## Phase 1: security fixes (`feature/security-fixes`, implemented, not merged)

The security batch from `docs/SECURITY-FIX-PLAN.md` is implemented and live-verified
on the VM; it is **not merged yet** (two commits on `feature/security-fixes`).
`master` is at `0f1068a` (the buddy merge + handoff).

**Deployed to the VM for the validation (helm revision 64 at the end):** api
`0.1.0-b10`, agent `0.1.0-b5`, ui `0.1.0-b5`, with `auth.disabled=true` restored
afterwards (the VM has no reachable Traefik, so the chart's `ingress.enabled=false`
means the proxy secret would never be injected).

**What the batch changes**

- **NAS-001** — every owner route now lives on an `owner` mux behind
  `auth.RequireAuth`, which requires the proxy-issued `X-Naslos-Proxy-Secret`
  (constant time) *and* the identity header, in addition to the trusted CIDR. Only
  `/api/health`, `/api/ready`, the buddy peer API (`/api/buddy/v1/*`, its own
  Ed25519 auth) and the static UI are public. The API refuses to start without
  `PROXY_SHARED_SECRET` unless `AUTH_DISABLED=true` (then it logs a loud warning).
- **NAS-004/005** — the per-handler switches are gone (`requireTerminalAuth`,
  `requireBuddyAdminAuth`, the `Remote-User` reads in `handleAuthMe`); `/api/ws/logs`
  is authenticated by construction.
- **NAS-002** — the agent requires `Authorization: Bearer <AGENT_TOKEN>` on
  everything but `/health` (constant time) and refuses to start without a token
  (`AGENT_AUTH_DISABLED=true` is the logged dev opt-out). The API injects the token
  in a transport, so the streaming send/receive requests are covered too.
- **NAS-003** — every destructive sink validates first (pool name, topology, disks
  via `normalizeDiskPath` + duplicate + pool-member checks, cache device, dataset
  options, dataset paths, snapshot names). Caller-fixable input is now a **400**
  (`zfs.ValidationError` → `writeClientError`), node failures stay 500. The API
  mirrors the pool-name and disk checks (fast 400) and no longer offers the pool
  root as a send source.
- Chart: `naslos-proxy`/`secret` and `naslos-agent`/`token` Secrets (generated once,
  `lookup`-preserved, or `auth.proxySecretName`/`agent.tokenSecret`), the
  `proxy-identity` Traefik Middleware, `PROXY_SHARED_SECRET`/`AGENT_TOKEN`/
  `TRAEFIK_CIDR`/`AUTH_DISABLED` env, and the new `auth.*` values. Also: the ingress
  templates were dead — they were gated on `traefik.enabled`, which the Traefik
  subchart's schema **rejects**, so no IngressRoute/Middleware had ever been
  deployed; they are now gated on `ingress.enabled` (default false).

**Live validation (2026-09-17, VM, chart-generated Secrets, gate armed)**

- Armed (`auth.disabled=false`): `/api/users` → **401** with no headers, **401** with
  a forged `Remote-User`, **401** with a wrong secret, **200** with the generated
  secret + user; `/api/ws/logs` → **401**; `/api/volumes/zfs` with the secret → **200**
  (proves the API→agent token reaches the agent); a typo'd header name is rejected.
- Agent: `/health` → 200; `/api/v1/pools` without/with a wrong token → **401**; with
  the generated token → 200. Negative checks (`-x` name, unknown topology, absent
  disk, member disk) → **400** with the right message and no state change; a
  traversal path never reached the handler.
- Dev posture restored (`auth.disabled=true`): the startup warning is logged, owner
  routes are served, and the full Playwright suite passes: **29 passed / 2 skipped /
  0 failed**.
- Local: `api` + `agent` `go build`/`vet`/`test` clean; `helm lint` passes;
  `svelte-check` 0 errors.
- Left open deliberately: dropping the agent's `hostNetwork` (hostPID removed);
  NAS-006/007 and NAS-009+ are out of this batch.

## Current branch: `master` (integration)

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
