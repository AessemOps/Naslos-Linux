# Buddy Backup: UI page + scheduler (plan-order items 1–2)

> **Historical plan — implemented.** Both items (the UI backup page and the
> scheduler with retention + ntfy) landed; see [buddy-backup.md](../buddy-backup.md)
> §9 and the AUDIT report. The branch/URLs below are the state at planning time
> (the UI NodePort no longer exists; use `https://naslos.local`).

Branch: `feature/buddy-backup` (clean). The receiver, `buddyctl`, the standalone
receiver and the instance-side send/restore (FR-BUD-01…14) are delivered and
drilled live. This plan covers the two next items from `docs/buddy-backup.md` §9
and `AI_Handoff.md`: the **UI backup page** and the **scheduler with retention +
ntfy notifications**. Multi-buddy fan-out and the peer-exposure decision are
**out of scope**.

## Decisions (agreed with the user)

1. **Async send jobs.** `POST /api/buddy/send` (api/internal/server/buddy_send.go:307)
   is synchronous today and is deliberately killed when the caller's request
   context goes away. Convert it to a job model: start the send with a
   server-owned context, answer `202 {jobId}` immediately, expose progress and
   cancellation via job endpoints. One job model — no parallel sync path.
   (Only tests and curl drills use the synchronous shape today; no UI or
   `buddyctl` depends on it.)
2. **Scheduler.** Interval enum (hourly / daily / weekly + run-at time), entries
   persisted as JSON beside the peer registry, managed over the API, with
   catch-up of a run missed while the API pod was down. No cron parser, no K8s
   CronJob.
3. **UI.** Route `/backups` with a sender half (identity, schedules, Back up now
   with live progress, ad-hoc send, verify) and a receiver half (the existing
   `/api/buddy/status` payload). No restore-from-the-UI in this round.

## Tasks (in order)

### A. Job runner (API)

1. New `api/internal/server/buddy_jobs.go`:
   - `buddyJob`: id (short random hex), kind "send", request params
     (dataset/source/receiver/pruneKeep/raw/force), state
     (running|succeeded|failed|cancelled), startedAt/finishedAt, mutex-guarded
     progress snapshot (`buddy.PushProgress` — the callback seam already exists,
     client.go:485), result payload (the fields handleBuddySend returns today),
     error string, cancel func.
   - Jobs live in memory on the Server; keep the last ~20 finished jobs plus
     running ones. Restart clears history — the receiver's manifests are the
     durable record; acceptable.
   - **Mutual exclusion**: refuse (409) a new send job for the same
     (receiver, source) while one runs (they share one resume-state file,
     `sendStateName`), and refuse a second job touching the same dataset while
     one runs (snapshot races).
   - Jobs use `context.WithCancel(background)`; cancel on `DELETE`, and cancel
     all running jobs on server shutdown (mirror how the metrics collector is
     stopped in `Server.Start`/`Stop`).
2. Convert `handleBuddySend`: keep today's validation + `requireBuddyAdminAuth`,
   then enqueue and return `202 {"jobId":…, "status":"started"}`. Move the send
   body into the job function; `ctx` becomes the job context instead of
   `req.Context()`. Wire `PushOptions.Progress` into the job's snapshot.
   The resume-state semantics are unchanged: a cancelled/killed send keeps its
   state file, so a retry continues the same chain.
3. Routes (all behind `requireBuddyAdminAuth`):
   - `GET /api/buddy/jobs` — list (running + recent finished)
   - `GET /api/buddy/jobs/{id}` — detail incl. progress
   - `DELETE /api/buddy/jobs/{id}` — cancel
4. Tests in `api/internal/server/buddy_send_test.go` (the harness with the fake
   agent + real receiver already exists):
   - start → 202 + id; job reaches `succeeded` with the same result fields the
     sync handler produced; manifest published.
   - second job for the same (receiver, source) while running → 409.
   - DELETE mid-send → `cancelled`, resume state file still present, retry
     resumes (`resumed: true`, chunks skipped).
   - job list/detail shapes.

### B. Scheduler (API)

5. Schedule store, persisted at `BUDDY_SCHEDULES` (default
   `/var/lib/naslos/buddy-schedules.json`), modeled on `buddy.NewPeerStore`
   (load/save pattern, api/internal/buddy/peers.go). Entry:
   `id, dataset, source, receiver, cadence (hourly|daily|weekly), runAt
   (HH:MM UTC, or weekday+HH:MM for weekly), pruneKeep, enabled, lastRun,
   lastResult (ok|failed + error), nextRun`.
6. Endpoints behind `requireBuddyAdminAuth`:
   `GET /api/buddy/schedules`, `POST /api/buddy/schedules` (create/update),
   `DELETE /api/buddy/schedules?id=`. Validation reuses `buddy.ValidateSource`
   and `normalizeReceiverURL`, and refuses a dataset the agent does not know.
   No separate "run now" endpoint — the UI's Back up now button POSTs
   `/api/buddy/send` with the entry's parameters, sharing A's mutual exclusion.
7. Runner goroutine started from `Server.Start()` (pattern:
   api/internal/server/metrics_collector.go): tick every minute; for each
   enabled entry with `nextRun <= now`, start a send job through the same path
   as A; on completion update `lastRun`/`lastResult`/`nextRun` and persist.
   **Catch-up**: at startup, an entry whose `nextRun` is in the past fires once
   immediately.
8. Notifications on failure: add `EventBackupFailure EventType =
   "backup_failure"` to api/internal/notifications/notifications.go and send
   via the existing `Manager.Send` (manager.go:90) with `SeverityError`, title
   naming dataset + receiver. Gate on the settings' `EnabledEvents` containing
   the new event type (check the settings before calling `Send`, which today
   filters only on enabled/min-severity). Add the checkbox to the
   Notifications settings UI page.
   Note: `Manager.Send` holds the manager mutex during the HTTP POST — verify
   the manager's http.Client has a sane timeout; if not, set one.
9. Tests: store round-trip; runner fires a due entry (short cadence),
   records success and passes `pruneKeep` through; a failing receiver
   (httptest 500) records the failure and produces one ntfy POST to a mock
   server; catch-up fires once after "restart"; disabled entries never fire.

### C. UI

10. `ui/src/lib/components/Sidebar.svelte`: add "Backups" → `/backups`.
11. `ui/src/routes/backups/+page.svelte`, following existing page conventions
    (Tailwind, fixed table layout, `Array.isArray` guards, error banner):
    - **Sender half** — identity card from `GET /api/buddy/identity`
      (fingerprint, copyable public key, create button via POST when
      `exists:false`, the back-it-up warning); schedules table (dataset, buddy,
      cadence/run-at, last run + result, next run, Back up now, delete) with a
      create form (dataset picker from the existing pools API, receiver URL,
      source, cadence, pruneKeep); ad-hoc send form (dataset, receiver, source).
      Back up now / ad-hoc send → `POST /api/buddy/send` → poll
      `GET /api/buddy/jobs/{id}` (~1 s, in-flight guard like Dashboard's poll)
      and render a progress bar (chunks, plain/sealed bytes,
      incremental/resumed badges, final result or error; surface a 409 as
      "a backup of this source is already running").
    - **Receiver half** — `GET /api/buddy/status`: free/used bytes,
      enrollment-open badge, peers table, backups table with last-backup
      columns. The 503 "not configured" case renders an info panel pointing at
      docs/buddy-backup.md §5.1 instead of an error.
    - **Verify** — a Verify button per schedule/source driving
      `POST /api/buddy/restore {verify:true}`, showing the chain digests.
      No restore form in this round.
12. Playwright `ui/tests/backups.spec.ts` against the VM (https://naslos.local),
    asserting API-backed values (not placeholders), following the existing
    specs: page loads with identity card; create a schedule on a test dataset;
    Back up now → job reaches succeeded with chunks > 0; receiver section shows
    free space; full existing suite still passes.

### D. Docs + spec (same change, per the spec's own rule)

13. `docs/spec.md` §3.8: add **FR-BUD-15** (scheduled backups: interval
    cadence, catch-up after downtime, prune on success, notify on failure) and
    **FR-BUD-16** (owner UI: receiver status, manual send with live progress,
    verify), plus rows in the test mapping. Update the "No scheduler yet" bullet
    in docs/buddy-backup.md §7 and mark items 1–2 done in §9; document the
    async `/api/buddy/send` shape in §5.3. Update `AI_Handoff.md`'s buddy
    section (delivered / not-done lists) when the work lands.

## Validation

- `go build ./... && go vet` in `api/` (agent untouched).
- `go test ./internal/server/ ./internal/buddy/ ./internal/notifications/` in `api/`.
- `cd ui && npx playwright test` (new spec + full suite).
- Live drill on the VM, per the handoff's deployment notes (fresh image tag
  suffixes, `helm upgrade … --force-conflicts`): create a schedule on a test
  dataset → scheduled/triggered run succeeds and prunes to `pruneKeep`;
  cancel a run mid-stream via the UI, retry, confirm `resumed: true`; point a
  schedule at a dead receiver URL → failure recorded + ntfy message arrives;
  Verify digest equals the node's `zfs send -w | sha256sum`.

## Risks / notes

- Converting the send endpoint changes its contract — update the existing
  `buddy_send_test.go` drills to the job shape in the same change.
- Jobs now outlive requests, so cancellation (DELETE + shutdown) is mandatory;
  without it a stuck send runs forever.
- Scheduler and manual Back up now share the (receiver, source) exclusion —
  the 409 must be a first-class UI state.
- The receive dataset's host-namespace mount gotcha (AI_Handoff, "second
  gotcha") is still open on the VM; it does not block sender-side work.

## Out of scope

Multi-buddy fan-out; peer-exposure decision (dedicated listener vs
Traefik + Authelia); restore UI; `buddyctl` changes.
