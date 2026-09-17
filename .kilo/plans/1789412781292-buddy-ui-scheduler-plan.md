# Naslos — project status and continuation plan (2026-09-17)

## Repo facts (verified in git this session)

- Canonical branch is `origin/master` (`origin/HEAD → origin/master`); `origin/main`
  is a stale branch (68 behind master).
- **Already in master**: storage/ZFS (FR-STO), shares SMB+NFS (FR-SHR), users/groups +
  LDAP (FR-IDN), dashboard + metrics (FR-MET), terminal (FR-LOG), app catalog
  (FR-APP), SSO, zfs-host-support. `feature/users`, `feature/dashboard-and-metrics`,
  `feature/zfs-host-support`, `stage-6-sso` are 0 commits ahead of master (merged);
  `feature/web-terminal` was merged on the remote via PR #8.
- **Local master is 3 commits behind** `origin/master` (the PR #8 merge `3e454bc` and
  its two parents) — a plain fast-forward.
- **Only `feature/buddy-backup` is outstanding**: 10 commits ahead / 1 behind
  `origin/master` (the 1 behind is the #8 merge commit, so it merges cleanly). It is
  stacked on the terminal commits. Its 2 newest commits (`a5406f2`, `642df00`) are
  **not pushed** (`origin/feature/buddy-backup` is at `4ffeab7`).
- A linked worktree exists for `rural-drill` — leave it untouched.
- The live VM state could not be re-checked this session (plan mode restricts bash to
  the read-only allowlist). Last recorded deploy: api `0.1.0-b8`, ui `0.1.0-b4`
  (built from the buddy branch tip); re-verify pods and tags before the next deploy.
- Leftover VM test artifacts (optional cleanup): empty `test/docker-restored` (busy in
  the agent's mount namespace), `buddy-*` snapshots on `test/Backup`,
  `test/drill-restored`, and the standalone receiver's volumes at `~/buddy-standalone`.

## What works (and how it was verified)

| Area | State | Evidence |
| --- | --- | --- |
| Storage / ZFS | Merged; pools, datasets, add drives, import | PR #7 merge; `pools.spec.ts`; spec §7 live checks |
| Shares | Merged; SMB + NFSv4 via Ganesha, LDAP account mirror, group access, dataset-path enforcement, mDNS/WSD | PR #6; `shares.spec.ts`; live SMB login/group drills in `AI_Handoff.md` |
| Users / groups / LDAP | Merged; CRUD, NT hash, password-change timing, lazy reconnect, `/api/ready` | `users.spec.ts`, `groups.spec.ts`, `e2e.spec.ts`, identity Go tests, LDAP-outage drill |
| Dashboard / metrics | Merged; 5 s collector (`METRICS_INTERVAL_SECONDS`), live snapshot, auto-refresh | `dashboard.spec.ts`; spec FR-MET-02/03/08 |
| Terminal / logs | Merged; auth-gated exec, pod logs | `terminal.spec.ts` (exec tests skip without an interactive session) |
| App catalog | Merged; catalog entries, JSON-Schema forms, helm install/upgrade, start/stop | `/api/catalog`, `/api/apps`, `/apps` page; spec FR-APP-01…04 |
| Buddy Backup | Complete on the branch: sender, receiver, `buddyctl`, standalone container, async jobs, scheduler, `/backups` UI | 11 Go tests; 6 Playwright tests; VM drills + cross-flavour drill (VM ↔ container) recorded in `AI_Handoff.md` |
| Test suites | Green as of the last session | Go: `api`, `agent` build/vet/test clean; `svelte-check` 0 errors; Playwright **29 passed / 2 skipped / 0 failed** |

## What does not work / is open

**Security — the biggest gap (22 findings: 3 Critical, 5 High, 10 Medium, 4 Low/Info),**
plan written but **not implemented**: `docs/SECURITY-FIX-PLAN.md`
(`.kilo/plans/1789421985000-critical-security-fixes.md`). The three Criticals:

- NAS-001 — `RequireAuth`/`RequireAdmin` are dead code; most owner routes are
  unauthenticated at the API layer and security rests on the Traefik path alone.
- NAS-002 — the privileged agent has **no auth** at all (`:9090`, privileged,
  `hostPath /`); anyone in-cluster can destroy pools or wipe disks.
- NAS-003 — agent validation gaps: unvalidated pool names, disks, datasets, snapshots
  reach `zpool`/`zfs`/`wipefs` argv.

Coupled auth Highs: NAS-004 (terminal), NAS-005 (`/api/ws/logs` unauthenticated),
NAS-008 (NodePort proxies nearly all `/api/` and trusts a client header).

**Buddy (branch) — remaining work and defects found in the drills**
(`AI_Handoff.md` → "Findings to follow up", `docs/buddy-backup.md`):

1. **Cancel is not prompt**: `buddy.Client` HTTP calls are not context-bound, so
   `DELETE /api/buddy/jobs/{id}` only takes effect when the next stream read fails
   (observed 30 s against a stalled receiver).
2. **A send of a dataset not mounted in the host namespace reports `succeeded` while
   storing nothing** (the `mountPropagation: HostToContainer` trap on the send path —
   an empty 44 KB backup over a green result).
3. **`pruneKeep` can leave only unrestorable incrementals**: the send and the schedule
   report success, and only verify/restore later refuses (missing base chain).
4. Multi-buddy **fan-out** (one source → several receivers) is not implemented.
5. **Peer-exposure decision** still to be taken (dedicated listener vs Traefik +
   Authelia).
6. Medium/Low hardening still open for buddy: NAS-011 (enroll replayable across
   restarts, same-name key overwrite), NAS-012 (no manifest rollback protection),
   NAS-013 (quota race/undercount), NAS-014 + NAS-018 (32-bit job IDs, state-file
   races, loose schedule dataset match), NAS-021 (counter overflow, FS walks).
7. Deploy ergonomics: the chart passes neither `BUDDY_SCHEDULES` nor
   `BUDDY_SCHEDULER_INTERVAL_MS`; `buddy.*` is not in `values-vm.yaml` so upgrades
   must use `--reuse-values` or repeat the `--set` flags; the `chown 65532:65532`
   receive-dataset step is manual; a job can report a snapshot name it never created;
   a never-run schedule serialises `lastRun` as `0001-01-01T00:00:00Z`.

**Other product gaps**

- Notification settings are **in-memory only** (`notifications.NewManager("")` in
  `server.go`) — they reset on every API restart; no persistence path is wired.
- `/api/shares/status` returns empty values: the API's `AgentSharesStatus` struct
  still names the old agent fields (`sambaRunning`, …) while the agent reports
  `smbShareCount`/`nfsExportCount`.
- `naslos-openldap-backup` CronJob is in CrashLoopBackOff
  (`openldap/manifests/backup-cronjob.yaml`) — LDAP backups are failing.
- FR-MET-10 **[OPEN]**: per-interface IPs are not collected (interface names are);
  multi-node metric aggregation is explicitly out of the current single-node scope.
- NFS is NFSv4-only with AUTH_SYS (documented, by design given Talos).
- `svelte-check` reports 45 warnings (mostly a11y) — acceptable but worth clearing.
- Deploy fragility: reused `0.1.0` tags with `IfNotPresent` (NAS-022) can run stale
  images after a retag.

---

## Plan

Order: **Phase 0 consolidate → Phase 1 security gate → Phase 2 buddy completion →
Phase 3 product gaps.** Phase 0 is the agreed lead; Phase 1 gates any exposure beyond
the current dev-only VM posture.

### Phase 0 — Consolidate the mainline (do first)

1. `git fetch --all --prune`; confirm `origin/master` tip is `3e454bc` (#8).
2. On `master`: `git merge --ff-only origin/master` (3 commits, the PR #8 merge).
3. On `feature/buddy-backup`: push the two local commits
   (`git push origin feature/buddy-backup`).
4. Open **PR #9** `feature/buddy-backup → master` (`gh pr create`), title in the repo
   style, e.g. `feat(buddy): Buddy Backup — sender, receiver, scheduler and UI`;
   body summarising: buddy feature set, the security audit + fix plan docs, the VM and
   cross-flavour drill results, and the known follow-ups. Expect 10 commits; the 1
   merge-commit divergence resolves as a normal PR merge (history shape matches
   #6/#7/#8).
5. Merge the PR on GitHub; then `git checkout master && git pull --ff-only`.
6. Confirm consolidation: `git rev-list --count master..feature/buddy-backup` → `0`,
   and that `git branch --list` shows no branch ahead of master.
7. Verify the merged tree exactly as the repo expects: `cd api && go build ./... &&
   go vet ./... && go test ./...`; same for `agent/`; `cd ui && npm run check &&
   npx playwright test` (29 passing / 2 skipped baseline).
8. Deploy **from master** with fresh tags (api/ui, and agent if it changed) using
   `--reuse-values` or the full `--set` list; confirm pods, `/api/ready`, and re-run
   the buddy smoke: one `/api/buddy/send`, one scheduled run, `/backups` loads.
9. Update `AI_Handoff.md`: change the "current branch" line to `master`, record the PR
   number/merge commit, and move the buddy section from "delivered on a branch" to
   "in master".
10. Optional cleanup (decide, don't block): delete the merged local (and remote)
    branches `users`, `dashboard-and-metrics`, `zfs-host-support`, `stage-6-sso`,
    `web-terminal`, `feature/buddy-backup`. Do **not** touch the `rural-drill`
    worktree.

### Phase 1 — Security gate (execute `docs/SECURITY-FIX-PLAN.md`)

Use that document as the task list (tasks 1–9, rollout, validation) — do not
re-plan it here. Sequencing notes:

- API auth (NAS-001/004/005/008) + proxy secret + agent bearer token (NAS-002) + agent
  validation (NAS-003) land as one change with the API and agent deployed **together**
  (a new agent rejects an old API).
- The VM has no reachable Traefik, so `values-vm.yaml` must set `auth.disabled: true`
  with the loud warning; document that this keeps the NodePort a dev-only listener.
- NAS-008 and the buddy **peer-exposure** decision (Phase 2) are the same boundary:
  decide them together once the proxy secret exists.
- After the batch, take the remaining findings in risk order (NAS-006/007 injections,
  then NAS-010, 011–018, 019–022).

### Phase 2 — Buddy completion

1. **Peer-exposure decision** (recommended: keep the existing Traefik + Authelia path
   and the new proxy secret; no dedicated listener — it reuses the auth model Phase 1
   establishes, exposes no new port, and the peer `/api/buddy/v1/*` routes stay public
   by design).
2. **Fan-out** (one source → several receivers): schedule/job model takes a list of
   receivers or a named group; jobs fan out with per-receiver chains, per-receiver
   result state, failure isolation (one dead receiver must not fail the others), and an
   aggregate `lastResult`; UI shows each receiver independently.
3. **Cancel is prompt**: add a context to the buddy client
   (`PushOptions`/`RestoreOptions` + `http.NewRequestWithContext`) and thread the job
   context through so `DELETE` aborts an in-flight request; test with a deliberately
   slow receiver.
4. **Refuse to back up an unmounted dataset**: agent reports per-dataset mount state;
   the send returns a clear error when the dataset is not mounted in the host namespace
   (turns the silent empty backup into a fast failure); keep the documented pre-flight
   and add it to the chart as a check/job if practical.
5. **Retention correctness with incrementals**: retention counts the sequence depth (or
   refuses a keep-count that would orphan a base), and `/api/buddy/status` shows whether
   a source's stored chain set is restorable.
6. **Buddy hardening batch**: NAS-011/012/013/014/018/021 (persist `enrollUsed`,
   monotonic manifests, quota under lock, 128-bit job IDs + ownership, strict schedule
   dataset allowlist, state-file locking + fsync, counter-overflow refusal).
7. **Polish**: chart passes `BUDDY_SCHEDULES`/`BUDDY_SCHEDULER_INTERVAL_MS`; fix the
   reported-snapshot-before-creation and zero-value `lastRun`; persist notification
   settings (shared with Phase 3).
8. Spec/test updates in the same change (FR-BUD-15/16 rows + `backups.spec.ts`, plus a
   new `ui/tests/` case for fan-out).

### Phase 3 — Product gaps

- Notification settings persistence + any additional event sources the spec lists.
- Fix `AgentSharesStatus` so `/api/shares/status` reports the agent's real fields
  (`smbShareCount`/`nfsExportCount`).
- `naslos-openldap-backup` CronJob CrashLoopBackOff: root-cause and fix.
- FR-MET-10 per-interface IPs (multi-node remains out of scope).
- Clear the 45 `svelte-check` warnings (a11y), and bump the vulnerable/fixed
  dependencies (NAS-020) as a separately tested change.
- Pin image digests instead of reusing `0.1.0` tags (NAS-022) — or at least document
  the always-retag rule and add a guard.

## Validation (every phase)

- `api/` and `agent/`: `go build ./... && go vet ./... && go test ./...`; `gofmt` clean.
- `ui/`: `npm run check` 0 errors; `npx playwright test` against the live VM with the
  spec §7 mapping kept in sync.
- Live drill on the VM for anything touching the node (shares, LDAP, ZFS, buddy send →
  restore → verify), recorded in `AI_Handoff.md` with the deployed tags.
- Spec rule: a change that alters a MUST updates `docs/spec.md` and its test in the same
  change; `[OPEN]` items are removed only when verified.

## Risks

- Phase 1 touches every route: without `auth.disabled=true` on the VM (or a reachable
  Traefik) the UI returns 401 everywhere. Deploy API + agent together.
- The buddy PR is large (~3.5k lines) but was live-verified twice; re-run the suites on
  the merged tree before deploying.
- Tag reuse (`0.1.0` + `IfNotPresent`) can silently serve stale images; bump the suffix
  on every deploy.
- The agent's mount-namespace behaviour is the root of several buddy and share gotchas;
  any fix that assumes host-visible mounts must be verified with a fresh pod.
- The `rural-drill` linked worktree must not be disturbed by branch cleanup.

## Open questions

- Release tagging: introduce `v*` tags at the Phase 1 boundary, or keep branch-based
  deploys only?
- Does the NodePort (`:30080`) stay as an explicitly documented dev listener after
  NAS-008, or get removed from non-dev values?
- Delete the merged feature branches (local + remote) or keep them as history?
- Which agent/samba/nfs tags are live on the VM right now (needs a read-only cluster
  check before the next deploy)?
