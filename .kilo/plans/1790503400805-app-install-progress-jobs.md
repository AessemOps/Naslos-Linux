# App install progress + background jobs

## Goal

Make app lifecycle operations (install / upgrade / uninstall) run as **async
jobs** with a **live progress view**, so a cold install (up to 5 min of Helm
`--wait`) no longer blocks a request or dies with the operator's connection.
The UI keeps a **global job drawer** and fires a **toast** on completion, so a
backgrounded install stays visible across pages.

Owned by branch `feature/app-install-progress` (create from current `master`).

## Decisions (resolved with user)

- Install **and** upgrade **and** uninstall become async jobs.
- `POST /api/apps` now returns `202 {jobId, state}`; the synchronous contract is
  replaced and all callers updated (Playwright test, `docs/api.md`, UI modal).
- Progress granularity: **real stages + indeterminate bar** (preparing →
  installing → finalizing). No fabricated percentage: Helm's `--wait` gives no
  byte/percent progress.
- In-app toast only. **No** new ntfy event types.
- A second job for an app already running a job → **409**.
- **No cancel endpoint** (a cancelled Helm install can leave a partial release).

## Non-goals

- Start/stop (`FR-APP-04`, stays `[OPEN]`).
- Persistent job history (jobs are in-memory, like buddy jobs; the app record
  remains the durable outcome).
- Byte-level or k8s-event-level install progress.
- ntfy pushes for installs.

## Design

### Backend — new job runner (`api/internal/server/app_jobs.go`)

Mirror the proven buddy pattern in `buddy_jobs.go:33-340` (state machine,
`buddyJobManager` with ring-keep, `snapshot()`, `context.Background()` so a
disconnect cannot kill the job, mutex-guarded progress).

```go
type appJobState string // running | succeeded | failed
type appJobKind  string // install | upgrade | uninstall

type appJobStage string // preparing | installing | finalizing

type appJobPublic struct {
    ID         string      `json:"id"`
    Kind       appJobKind  `json:"kind"`
    App        string      `json:"app"`
    State      appJobState `json:"state"`
    Stage      appJobStage `json:"stage"`
    Message    string      `json:"message,omitempty"`
    StartedAt  time.Time   `json:"startedAt"`
    FinishedAt time.Time   `json:"finishedAt,omitempty"`
    BaseDomain string      `json:"baseDomain,omitempty"`
    Error      string      `json:"error,omitempty"`
}
```

- `appJob` embeds `appJobPublic` plus `mu`, `ctx`, `cancel` (kept internally even
  though no HTTP cancel), `done`.
- `appJobManager`: `jobs map[string]*appJob`, `order []string`, methods
  `add/get/list/conflicting(app)` and `ensureAppJobs()` on `Server` (lazy, like
  `ensureBuddyJobs` at `buddy_jobs.go:235`). Keep running + last ~20 finished.
  Reuse `randomJobID()` (`buddy_jobs.go:124`).
- `conflicting(app)` returns a running job for the same app name → handler
  answers 409.

### Staged install (`api/internal/apps/apps.go`)

Add a progress callback to the manager so the runner drives `stage/message`:

```go
type ProgressFunc func(stage, message string)
```

- New `InstallWithProgress(ctx, req, progress ProgressFunc) (*View, error)`;
  `Install` becomes a thin wrapper calling it with a no-op (keeps existing
  `apps_test.go` and any other caller working).
- Emit stages:
  - `preparing` — validate name → `catalog.Get` → `resolveChart`
    (git clone/refresh, `apps.go:232,564`).
  - `installing` — the blocking Helm `InstallDir` (`apps.go:240`,
    `helm/operations.go:36-53`).
  - `finalizing` — record upsert + `applyRoute` + `view` (`apps.go:245-273`).
- Same wrapper approach for upgrade/uninstall if they factor cleanly; otherwise
  the runner sets `installing`/`finalizing` around the existing `Upgrade`
  (`apps.go:279`) and `Uninstall` (`apps.go:371`) calls.

### Handlers (`api/internal/server/apps.go`)

- `POST /api/apps` (`apps.go:62-92`): keep validation (confirmed, base domain)
  synchronous, then enqueue and return `202 {jobId, state:"running"}` instead of
  calling `Install` inline.
- `PUT /api/apps/{name}` (`apps.go:129-142`): enqueue an `upgrade` job, return
  `202 {jobId, state}`.
- `DELETE /api/apps/{name}` (`apps.go:144-152`): enqueue an `uninstall` job,
  return `202 {jobId, state}`.
- New routes registered next to the existing apps routes (`server.go:498-499`):
  - `GET /api/apps/jobs` → `{jobs:[...]}` (list; running + recent finished).
  - `GET /api/apps/jobs/{id}` → `appJobPublic` (poll target).
- Route-order gotcha: `/api/apps/jobs` must be matched **before** the
  `/api/apps/{name}` handler. Add an explicit `if path == "jobs"` branch at the
  top of `handleAppDetail` (or register `/api/apps/jobs` before `owner.Handle`
  on `/api/apps`), so `jobs` is never treated as an app name.

### Runner (`app_jobs.go`)

- `runAppJob(job)` picks install/upgrade/uninstall, sets `stage` via the
  progress callback, and on completion sets `state`, `FinishedAt`, and records
  success/failure. Install/upgrade hold the resulting `View`.
- Jobs use the **job's own** `context.Background()`-derived context (not
  `r.Context()`), so a closed modal / navigated-away page cannot cancel a
  running install.
- No startup recovery needed: a job interrupted by an API restart simply stops;
  on restart `reconcileApps` (`server.go:633`) re-converges records/routes and
  the Helm release status surfaces via `GET /api/apps`.

### Frontend

- **Global job store** — `ui/src/lib/stores/appJobs.ts` (Svelte store):
  `jobs[]`, `track(jobId)`, a single `setInterval` poller (1 s, reuse the
  `backups/+page.svelte:241-293` pattern; never overlap with an
  `inFlight` guard), stop-on-not-running, and a `dismiss(jobId)`.
- **Toast** — `ui/src/lib/components/Toast.svelte` + store: fire once when a job
  transitions to `succeeded`/`failed` ("Jellyfin installed" / "Install of X
  failed: …"). Mounted globally in `ui/src/routes/+layout.svelte:6-10`
  alongside `Sidebar`.
- **Job drawer** — `AppJobsDrawer.svelte`: small fixed panel (e.g. bottom-right)
  listing tracked jobs with app name, kind, current stage, an **indeterminate
  progress bar** while `running`, and a link to `/apps` (Installed tab) on
  success. Collapsible; auto-dismiss of finished rows left to the user.
- **`AppInstallModal.svelte:89-108`** — `install()` changes to POST, read
  `jobId`, hand it to the store, then `dispatch('close')` immediately. The modal
  no longer blocks. Keep the confirm gate (`:170`) and the "Installing…" label
  only as a brief submit state.
- **`AppConfigureModal.svelte` (PUT, `:36-55`)** and
  **`InstalledApps.svelte` uninstall (`:49-63`)** — same change: POST/PUT/DELETE,
  enqueue, close, let the store/drawer/toast follow.
- **`InstalledApps.svelte`** — when the store has a running job for an app,
  render it with a `pending`/progress badge (reuse `statusClass` at `:65-73`)
  and disable its action buttons until the job finishes; refetch the list when a
  job completes.

### Helm indirection (optional, keep it small)

Only needed if a cleaner seam than the callback is wanted: add
`HelmOps interface { InstallDir; UpgradeDir; Uninstall }` and hold it on
`apps.Config`. Prefer the `ProgressFunc` wrapper unless the runner reads better
through an interface. Do not change `api/internal/helm` behavior
(`Wait=true`, `Timeout=5m` stay).

## Tasks (ordered)

1. Create branch `feature/app-install-progress` from up-to-date `master`.
2. `api/internal/apps/apps.go`: add `ProgressFunc`; `InstallWithProgress`
   emitting `preparing`/`installing`/`finalizing`; keep `Install` as wrapper.
   Add the same callback seam to `Upgrade`/`Uninstall`.
3. `api/internal/server/app_jobs.go`: job types, manager, `ensureAppJobs`,
   `runAppJob`, handlers `handleAppJobs`/`handleAppJobDetail`.
4. Wire handlers in `api/internal/server/apps.go` (202 responses) and routes in
   `api/internal/server/server.go` (jobs before `{name}`); call
   `s.ensureAppJobs()` in `Start()` (`server.go:646-649` area).
5. Go tests: `api/internal/server/app_jobs_test.go` (enqueue install returns
   202+jobId; detail reports running→succeeded; 409 on conflicting second job;
   failed install reports the error, using the existing apps test fixtures);
   `api/internal/apps/apps_test.go` (stages fire in order).
6. UI store `appJobs.ts`, `Toast.svelte`, `AppJobsDrawer.svelte`; mount in
   `+layout.svelte`.
7. Update `AppInstallModal.svelte`, `AppConfigureModal.svelte`,
   `InstalledApps.svelte` to enqueue + hand off to the store.
8. Docs (same change, per AGENTS docs rule): `docs/spec.md` §3.4 — new
   **FR-APP-18** ("install/upgrade/uninstall MUST run as background jobs with
   observable stage; POST/PUT/DELETE enqueue and return a job handle; a second
   concurrent job for an app MUST be rejected") with its test reference;
   `docs/api.md:70-110` — async rows for POST/PUT/DELETE plus the new job
   routes; fix the stale "installed into the `naslos` namespace" note
   (`docs/api.md:305`); `docs/app-catalog.md:81-107` lifecycle table.
9. `AI_Handoff.md` — record the new async contract and job endpoints.
10. Update `ui/tests/apps.spec.ts:29-64` — POST now 202; poll
    `/api/apps/jobs/{id}` until `succeeded`, then assert the Installed tab and
    the toast/drawer; keep the uninstall at the end (`DELETE` now 202 → poll).

## Validation

- `cd api && go build ./... && go vet ./... && go test -race ./...`
- `cd ui && npm run check && npm run build`
- `helm lint charts/naslos -f charts/naslos/values.yaml -f charts/naslos/values-vm.yaml`
- Playwright against the VM (needs `ui/.env.playwright.local`):
  `cd ui && ./node_modules/.bin/playwright test tests/apps.spec.ts`
- Live drill: install a catalog app with a cold image pull, verify the modal
  closes immediately, the drawer shows `preparing → installing → finalizing`,
  and a success toast fires; confirm the app appears as `running` in the
  Installed tab.
- Deploy per AGENTS rules (bump image tags in `values-vm.yaml`, `make install-vm`)
  before the live drill; the image is built by CI / install path, no live
  `kubectl` patch.

## Risks

- **API break**: POST/PUT/DELETE semantics change (201/200 → 202). Any external
  script breaks; audit for other callers before merging.
- **Route collision**: `/api/apps/jobs` vs `/api/apps/{name}` — must be matched
  first (explicit test for it).
- **Interrupted job**: an API restart mid-install abandons the job; records/routes
  re-converge on startup but a partial Helm release may need a manual retry.
  Acceptable, documented.
- **No cancel**: a wedged install can only be waited out (5-min Helm timeout).
- **Poller cost**: one shared 1 s poll while any job runs; stop it when none are
  running.

## Open questions

None — decisions above are final unless the user changes them.
