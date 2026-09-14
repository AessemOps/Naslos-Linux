# Buddy Backup: UI page + scheduler (+ restore form)

Branch: `feature/buddy-backup` (clean at resume). Delivered: receiver, `buddyctl`, standalone receiver, instance-side send/restore FR-BUD-01…14, drilled live. Source plan: `docs/plans/buddy-ui-scheduler-plan.md`.
This plan covers `docs/buddy-backup.md` §9 items 1–2: **UI backup page** and **scheduler with retention + ntfy**.

## Agreed decisions
1. **Scope: keep UI + scheduler.** Multi-buddy fan-out and peer-exposure (dedicated listener vs Traefik+Authelia) stay out of scope.
2. **Async-only send jobs.** `POST /api/buddy/send` (`api/internal/server/buddy_send.go:307`, sync today, killed with request ctx) becomes `202 {"jobId","status":"started"}` with server-owned `context.WithCancel(background)`. No parallel sync path. Rewrite `buddy_send_test.go` drills to job shape.
3. **Scheduler: interval enum.** `hourly|daily|weekly` + `runAt` (HH:MM UTC, weekday+HH:MM for weekly), JSON store at `BUDDY_SCHEDULES` (default `/var/lib/naslos/buddy-schedules.json`) modeled on `buddy.NewPeerStore`. Minute tick from `Server.Start()` (pattern `metrics_collector.go`), catch-up once on startup when `nextRun` in past. No cron parser, no K8s CronJob.
4. **UI `/backups` + restore form (expanded per user).** Sender half (identity, schedules, Back up now with live progress, ad-hoc send, verify) + receiver half (`GET /api/buddy/status`) + **restore form**: source + receiver + dataset + optional chain/force, same job-progress polling as send, **typed confirmation** required because `zfs receive` overwrites destination.
5. **Notifications on both outcomes.** Add `EventBackupFailure="backup_failure"` and `EventBackupSuccess="backup_success"` (`api/internal/notifications/notifications.go`), send via `Manager.Send` gated on `EnabledEvents`, checkbox(es) in Notifications settings UI. Success = info severity, failure = `SeverityError` naming dataset+receiver. Verify `Manager` http.Client timeout (it holds mutex during POST).
6. **Validation: full incl. live VM drill** (192.168.1.96:30080, fresh image tags, `helm upgrade --force-conflicts`).

## Tasks (ordered)
### A. Job runner (API)
- A1. New `api/internal/server/buddy_jobs.go`: `buddyJob` {id short hex, kind "send", params dataset/source/receiver/pruneKeep/raw/force, state running|succeeded|failed|cancelled, startedAt/finishedAt, mutex-guarded progress from `buddy.PushProgress` (`client.go:485`), result (today's handleBuddySend fields), error, cancel}. In-memory on Server, keep last ~20 finished + running. Restart clears history (manifests are durable). 409 if same (receiver,source) running (shared `sendStateName` state file) or same dataset running (snapshot race). Cancel on `DELETE`, cancel all on `Server.Stop()`.
- A2. Convert `handleBuddySend`: keep validation + `requireBuddyAdminAuth`, enqueue, return 202. Move body to job func with job ctx; wire `PushOptions.Progress`. Resume-state semantics unchanged (cancel keeps state file → retry resumes same chain/snapshot/key).
- A3. Routes behind `requireBuddyAdminAuth`: `GET /api/buddy/jobs`, `GET /api/buddy/jobs/{id}`, `DELETE /api/buddy/jobs/{id}`.
- A4. Tests (`buddy_send_test.go` harness): 202→succeeded with manifest; 409 on duplicate; DELETE→cancelled + state file kept + retry `resumed:true`; list/detail shapes.

### B. Scheduler (API)
- B1. Store + entry `{id,dataset,source,receiver,cadence,runAt,pruneKeep,enabled,lastRun,lastResult{ok|failed,error},nextRun}`. Validate with `buddy.ValidateSource`, `normalizeReceiverURL`, agent-known dataset.
- B2. Endpoints (`requireBuddyAdminAuth`): `GET /api/buddy/schedules`, `POST /api/buddy/schedules` (create/update), `DELETE /api/buddy/schedules?id=`. No separate run-now — Back up now POSTs `/api/buddy/send` (shares 409).
- B3. Runner tick 1m; on due start job via A-path; on completion update lastRun/lastResult/nextRun + persist; pass `pruneKeep` through.
- B4. Notifications both outcomes per Decision 5.
- B5. Tests: round-trip; due entry fires + prunes; 500 receiver → failure + 1 ntfy POST; restart catch-up fires once; disabled never fires.

### C. UI
- C1. `Sidebar.svelte`: "Backups" → `/backups`.
- C2. `ui/src/routes/backups/+page.svelte` (Tailwind, fixed layout, `Array.isArray` guards, error banner):
  - Sender: identity card (`GET /api/buddy/identity`, fingerprint, copyable key, create POST, backup warning); schedules table + create form (dataset picker from pools API, receiver, source, cadence, pruneKeep); ad-hoc send; Back up now → POST send → poll `GET /api/buddy/jobs/{id}` ~1s with in-flight guard, progress bar (chunks, plain/sealed bytes, incremental/resumed badges), 409 → "already running".
  - Receiver: free/used, enrollment badge, peers + backups tables; 503 → info panel → `docs/buddy-backup.md` §5.1.
  - Verify per source (`POST /api/buddy/restore {verify:true}` → chain digests).
  - Restore form (new): source, receiver, dataset picker/input, chain optional, force checkbox, typed-confirm (type dataset name), POST `/api/buddy/restore`, poll progress, show applied chains/error. Refuse without confirmation.
- C3. `ui/tests/backups.spec.ts`: identity card, create schedule, Back up now→succeeded chunks>0, receiver free space, verify shows digests, restore form requires confirmation; full suite passes.

### D. Docs + spec
- D1. `docs/spec.md` §3.8: FR-BUD-15 (schedule, catch-up, prune, notify success+failure), FR-BUD-16 (UI: status, manual send w/ progress, verify + restore w/ confirmation) + test-mapping rows. `docs/buddy-backup.md`: clear "No scheduler yet" §7, mark §9 items 1–2 done, document async send + restore-confirm in §5.3. Update `AI_Handoff.md` delivered/not-done.

## Data flow / failure modes
- Send: snapshot (`buddy-<UTC>-<4hex>`) → GUID-matched incremental decision → state file 0600 → estimate → stream `zfs send` → encrypt+PUT chunks → shortfall guard → publish manifest → delete state. Shortfall/incomplete → no manifest, retry resumes. Cancel → state kept.
- Restore: `RestoreSequence` (full + incrementals by GUID links, refuse missing base) → verify (hash only) or `zfs receive` streams in order (`-F` first only). UI confirmation guards overwrite.
- Scheduler/notify failures: dead receiver → lastResult failed + ntfy; stuck job → DELETE/shutdown cancel.

## Risks
- Breaking `/api/buddy/send` contract; shutdown-cancel mandatory or stuck sends run forever; 409 must be first-class UI state; `Manager.Send` mutex+timeout; Talos host-namespace mount gotcha still open (doesn't block sender work).

## Validation
- `go build ./... && go vet` in `api/`; `go test ./internal/server/ ./internal/buddy/ ./internal/notifications/`; `cd ui && npx playwright test`.
- VM drill: schedule run succeeds + prunes to keep; cancel mid-stream via UI → retry `resumed:true`; dead receiver → failure + ntfy; verify digest == `zfs send -w | sha256sum`; restore form without confirm refused, with confirm restores.

## Out of scope
Multi-buddy fan-out; peer-exposure decision; `buddyctl` changes.
