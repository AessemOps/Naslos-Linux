# Buddy Backup UI + scheduler (FR-BUD-15/16): VM verification plan

## Status

The implementation plan that used to live here has **landed** on
`feature/buddy-backup`:

- `11b9dff` — `feat(buddy): async send jobs, scheduler and backups UI (FR-BUD-15/16)`
  (17 files, +2592/−99: `buddy_jobs.go`, `buddy_schedules.go`, jobs/scheduler
  tests, `/backups` Svelte page, `ui/tests/backups.spec.ts`, spec §3.8
  FR-BUD-15/16, docs, notifications events `backup_success`/`backup_failure`).
- `4ffeab7` — security-audit docs only (`docs/SECURITY-AUDIT.md`,
  `docs/SECURITY-FIX-PLAN.md`), **no code**; nothing to test from it here.

It is **not deployed**: live tags per `AI_Handoff.md` are api `0.1.0-b7`, ui
`0.1.0-b1`, agent `0.1.0-b2`. The agreed scope is to **deploy the update to the
VM and run the full drill**, then record findings.

VM: `192.168.1.96`, UI via NodePort `http://192.168.1.96:30080`, registry
`192.168.1.2:30095`, `TALOSCONFIG=bootstrap/vm/talosconfig`. All owner-facing
buddy endpoints sit behind `requireBuddyAdminAuth`, so curl needs
`-H 'Remote-User: admin'` (`BUDDY_REQUIRE_AUTH=true`).

## Known preconditions / traps (confirmed in code)

1. **Chart does not pass `BUDDY_SCHEDULES`** (or `BUDDY_SCHEDULER_INTERVAL_MS`):
   the implicit default `/var/lib/naslos/buddy-schedules.json` is used, which is
   on the `naslos-shares-config` PVC (`api.sharesConfig.enabled: true`,
   mountPath `/var/lib/naslos`, values.yaml:42) — so schedules persist. Record
   the missing explicit chart env as a finding (same for the interval override).
2. **Re-apply the `--set buddy.*` flags on upgrade.** `values-vm.yaml` does not
   set `buddy.*`; the release only has it enabled from the original
   `--set buddy.enabled=true --set buddy.receivePath=/var/mnt/test/naslos-buddy
   --set buddy.name=naslos-b`. A plain `-f values.yaml -f values-vm.yaml`
   upgrade would disable the receiver.
3. **Self-send needs the instance's own key authorized on its own receiver.**
   `ui/tests/backups.spec.ts` tolerates an `unknown key` failure by skipping, so
   the manual drill must add the instance key to `/api/buddy/peers` first,
   otherwise "succeeded" is never actually exercised.
4. **Notifications are in-memory.** `notifications.NewManager("")` (server.go:85)
   → settings reset to `Enabled:false` on every pod restart, and
   `MinSeverity` defaults to `warning`, so `backup_success` (Info) is filtered:
   set `minSeverity:"info"` for the success check. Pre-existing gap, note it.
5. **409 needs a send that lasts.** The conflict window is the run time, so use a
   large dataset (e.g. a ~0.5–1 GiB test dataset) and issue the second POST
   immediately.
6. **Catch-up cannot be triggered through the API** (every POST recomputes a
   future `nextRun`) and the API image is distroless (no shell in
   `kubectl exec`). Patch the PVC from a throwaway busybox pod (pattern already
   used in the handoff for `df`), then restart the API. To exercise the *real*
   runner without patching, use a `daily` schedule with `runAt` = now+2 min UTC
   and `kubectl set env deployment/naslos-api BUDDY_SCHEDULER_INTERVAL_MS=5000`.
7. **Restore confirmation is client-side only**: the page refuses unless the
   typed destination equals the dataset (backups/+page.svelte:312); the API
   accepts `restore` with no `confirm` field. Test the UI refusal, don't expect
   an API-level guard.

## Task list

### 0. Pre-flight (local, before touching the VM)

- `cd api && go build ./... && go vet ./... && go test ./...` — expect green,
  including `buddy_jobs_test.go` and `buddy_scheduler_test.go`.
- `cd ui && npm run check` — expect 0 errors.

### 1. Build, push, deploy

- Fresh tag suffixes per the handoff (`always retag`; registry serves
  `IfNotPresent`): `make api-image IMAGE_TAG=0.1.0-b8`, `make ui-image
  IMAGE_TAG=0.1.0-b2`, then `docker push` both. Do **not** parallelise build and
  push.
- `helm upgrade naslos charts/naslos -n naslos -f charts/naslos/values.yaml
  -f charts/naslos/values-vm.yaml --set api.image.tag=0.1.0-b8
  --set ui.image.tag=0.1.0-b2 --set buddy.enabled=true
  --set buddy.receivePath=/var/mnt/test/naslos-buddy --set buddy.name=naslos-b`
  with `TALOSCONFIG=bootstrap/vm/talosconfig`, adding `--force-conflicts` if the
  image field is owned by an earlier `kubectl set image`.
- Confirm: `kubectl -n naslos get pods`, the api/ui images show the new tags,
  `/api/ready` is 200, `http://192.168.1.96:30080/backups` renders "Backups".
- Note the release revision and tags in the findings.

### 2. Automated suites against the live VM

- `cd ui && npx playwright test` — full suite (9 specs incl.
  `backups.spec.ts`); expect the pre-existing 8 topics still pass.
- If `backups.spec.ts` skips the send (unknown key) or the schedule (no
  dataset), treat it as a setup gap and re-run after step 3.1's peer enrollment.

### 3. API drills through the NodePort (nginx path + `Remote-User`)

1. **Self-enrollment**: `POST /api/buddy/peers` with this instance's `name` and
   `publicKey` from `GET /api/buddy/identity` (scoped to the test source), so a
   self-send can succeed.
2. **Auth**: `GET /api/buddy/jobs`, `GET /api/buddy/schedules`,
   `POST /api/buddy/send` without `Remote-User` → 401 (SEC-7).
3. **Async happy path**: `POST /api/buddy/send {dataset,source,receiver}` → 202
   `{jobId}`; poll `GET /api/buddy/jobs/{id}`: `running` with `progress` ticks,
   then `succeeded` with the sync-era fields (`chunks>0`, `chain`,
   `plainBytes`, `incremental`, `resumed`, `durationSeconds`); the job appears in
   `GET /api/buddy/jobs`. Compare the reported `plainBytes`/digest against the
   node's own `zfs send -w | sha256sum` for that snapshot (as the earlier drills
   did).
4. **409 conflict**: with the large dataset from precondition 5, POST twice in a
   row → second is 409 naming the running job; also confirm a *different*
   dataset is still accepted in parallel.
5. **Cancel + resume**: start a large send, `DELETE /api/buddy/jobs/{id}` after
   ~1 s → `{"status":"cancelling"}`; GET reaches `cancelled`. Check the resume
   state file exists (`/var/lib/naslos/buddy-sends/*.json`) — read it from the
   busybox PVC pod or `kubectl cp` is unavailable, so list via the pod. Re-POST
   the same request → `resumed: true`, `skipped > 0`, and it completes.
6. **Validate + job list/404**: bad schedule cadence, bad `runAt`
   (`"25:00"`, `"Mon"` for weekly), absolute/`..` source, unknown dataset,
   bad receiver URL → 400 each; unknown job id → 404; `DELETE` of a finished
   job → 409.

### 4. Scheduler + retention + catch-up

1. **Due run (shell-free)**: `kubectl -n naslos set env deployment/naslos-api
   BUDDY_SCHEDULER_INTERVAL_MS=5000`; create a `daily` schedule with `runAt`
   ≈ now+2 min UTC and `pruneKeep:1`; watch `GET /api/buddy/schedules` move
   `lastRun`/`lastResult:"ok"`/`nextRun` and a job with `scheduleId` appear in
   `/api/buddy/jobs`; then `kubectl set env deployment/naslos-api
   BUDDY_SCHEDULER_INTERVAL_MS-` to restore the 1-minute default.
2. **Retention**: let that schedule run twice (second is incremental) with
   `pruneKeep:1` → the receiver's chain list for the source (owner
   `GET /api/buddy/status` or `buddyctl backups`) holds only the newest chain,
   and a verify still passes.
3. **Catch-up**: stop the API (`kubectl scale deploy/naslos-api --replicas=0` or
   `rollout restart`), rewrite that schedule's `nextRun` to a past timestamp in
   `/var/lib/naslos/buddy-schedules.json` from a busybox pod mounting the
   `naslos-shares-config` PVC, restart the API → exactly one job fires on
   startup (restart again → no second fire).
4. **Failure path**: schedule/manual send to a dead receiver
   (`http://192.168.1.96:1` or an unresolvable host) → job `failed`,
   `lastResult:"failed"` + `lastError` persisted, entry's `nextRun` advanced.

### 5. Notifications (ntfy)

- Run a capture listener reachable from the pod (small python HTTP server on the
  VM host; fallback: a throwaway public ntfy topic subscribed to
  `https://ntfy.sh/<topic>/json`).
- `PUT /api/notifications` with `enabled:true`, `serverURL` = capture base,
  `topic`, `minSeverity:"info"`, `enabledEvents` including `backup_success` and
  `backup_failure`.
- Trigger one success and one failure → exactly one POST each; then remove
  `backup_success` from `enabledEvents` and confirm a success produces **no**
  POST; re-check that a failure still does (gate behaviour, FR-BUD-15).
- Note the in-memory settings + restart reset (precondition 4) in findings.

### 6. UI / browser drill (Playwright + manual click-through)

- Identity card: fingerprint + public key match `GET /api/buddy/identity`; the
  `exists:false` → "Create identity" path if the VM has no identity.
- Schedules table: create via the form, see it rendered, delete with the
  confirmation dialog; corrupting the form (bad cadence/runAt) surfaces the 400.
- **Back up now**: progress bar advances (chunks/bytes), incremental/resumed
  badges, cancel button cancels, final result renders; a second concurrent start
  surfaces the 409 message.
- **Verify**: per-schedule Verify shows chain digests; compare with step 3.3's
  digest.
- **Restore**: the typed-confirmation gate refuses a wrong dataset (no API call),
  and a real restore of a test source into a throwaway dataset lands (check
  `zfs list` on the node), following the existing restore drill.
- Receiver half: free/used space, peers, stored backups + last-backup columns;
  the not-configured 503 state renders the docs pointer.

### 7. Persistence & regression

- `kubectl -n naslos rollout restart deployment/naslos-api` → schedules and
  identity survive (PVC); jobs list is empty (in-memory, expected); a retry of an
  interrupted send still resumes.
- Receive-side regression: `buddyctl enroll`/`push`/`status`/restore for one
  source, plus `/api/buddy/status` and `/api/buddy/peers` 401/200 — the receiver
  code is untouched but the deploy is the risk.
- `ui/tests/*` and the dashboard/shares/terminal smoke checks to catch UI
  regressions from the sidebar/notifications edits.

## Deliverable / findings

- Test report appended to `AI_Handoff.md` (buddy section): deployed tags +
  revision, which checks passed, any defects with the exact failing command and
  response.
- Defects found get fixed in the same branch (implementation agent), with the
  spec/test updated together if a MUST changed.
- Record these gaps even if everything passes: `BUDDY_SCHEDULES` /
  `BUDDY_SCHEDULER_INTERVAL_MS` not wired in the chart; notifications settings
  not persisted; the `--set buddy.*` flags not captured in `values-vm.yaml`.

## Out of scope

Multi-buddy fan-out; the peer-exposure decision; `buddyctl` changes; the
security-audit *fixes* (that repo change is docs-only).
