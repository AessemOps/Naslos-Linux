# Next work: correctness + secret-leak batch (CR-07, CR-15, CR-18, CR-19/22)

Status line: the transfer to `192.168.1.118` is done. This plan supersedes that
task and covers the next local work chunk chosen from `docs/CODE-REVIEW.md` §7,
after the three blockers (CR-01/02/03, merged in PR #20).

Goal: stop leaking the ntfy token, tighten the NSS shadow file, make the user
edit actually apply group changes, and surface API failures in the UI instead of
blank/looks-like-success states.

Baseline: `master = 3821204`, clean tree (only the plan file is untracked).

## Decisions already made

- **CR-07**: the API never returns the token value. GET returns
  `hasAuthToken: bool`; PUT treats an **omitted** `authToken` as "keep the stored
  one", `""` as "clear", any other value as "replace". This keeps the existing
  settings JSON on the PVC unchanged (no migration).
- **CR-18**: `groups` in `PUT /api/users/{uid}` is a **pointer** (`*[]string`);
  `null`/absent means "leave membership alone". Deltas: desired−current → add,
  current−desired → remove, then push the share-access mirror like create does.
- **CR-19/22**: guard `res.ok` before `res.json()`, and surface the failure in
  the page's existing error affordance; no new UI framework.

## Tasks (ordered; each lands with its tests)

### 1. CR-07 — stop returning the ntfy auth token

Files:
- `api/internal/notifications/manager.go:81` — change signature to
  `UpdateSettings(settings Settings, authToken *string) error`; only touch
  `m.settings.AuthToken` when `authToken != nil`.
- Update call sites: `api/internal/server/notifications.go:22`,
  `api/internal/notifications/manager_test.go:23,46`,
  `api/internal/server/buddy_scheduler_test.go:53` (pass `nil` where the token
  is not the subject of the test).
- `api/internal/server/notifications.go:11-31` — GET builds a local response
  struct with `enabled/serverUrl/topic/hasAuthToken/email/enabledEvents/minSeverity`
  (no `authToken`). PUT decodes a struct whose `AuthToken` is `*string`, builds a
  `notifications.Settings`, and calls `UpdateSettings(settings, req.AuthToken)`.
- `ui/src/routes/notifications/+page.svelte` — `Settings` interface: drop
  `authToken`, add `hasAuthToken: boolean`. Add local `authToken` (input, starts
  empty) and `clearAuthToken` (checkbox). `loadSettings` must check `res.ok`
  (also closes CR-19 here). `save()` body: include `authToken` only when the
  operator typed one, or `""` when `clearAuthToken`; after success clear the
  input, set `settings.hasAuthToken`, reload. Password input placeholder:
  "stored — leave blank to keep" when `hasAuthToken`.
- `docs/notifications.md:14` — document `hasAuthToken` (read-only) and
  `authToken` (write-only; omitted = keep, `""` = clear).

Tests:
- `api/internal/notifications/manager_test.go` — update signature; add cases:
  `nil` keeps the stored token, `&""` clears, `&"new"` replaces.
- New `api/internal/server/notifications_test.go` — with admin headers:
  GET body contains `"hasAuthToken":true` and no `authToken` key; PUT omitting
  `authToken` preserves; PUT with a value replaces; PUT with `""` clears.
- `api/internal/server/routes_test.go` `newTestServer` — add
  `t.Setenv("NOTIFICATIONS_CONFIG", filepath.Join(t.TempDir(), "notifications.json"))`
  so tests never touch `/var/lib/naslos`.

### 2. CR-15 — NSS shadow mirror is world-readable

- `agent/internal/shares/shares.go:183` — change the `shadow` mode from `0644`
  to `0600` (matches `smbusers`); update the comment.
- New `agent/internal/shares/shares_test.go` — set the package `hostRoot` var
  (`shares.go:28`) to `t.TempDir()` (restore with `defer`), call `Apply` with a
  config that fills `NSSShadow`, then `os.Stat` the files under
  `hostRoot+NSSDir`: `shadow` is `0600`, `smbusers` is `0600`, `passwd`/`group`
  are `0644`.

### 3. CR-18 — editing a user's groups does nothing

- `api/internal/server/users.go:97-114` — decode `Groups *[]string`; after
  `UpdatePerson`, when `req.Groups != nil`: load `current` via
  `s.identity.GetPerson(uid)`, compute `add, remove := groupDelta(current.Groups, *req.Groups)`,
  call `s.identity.AddMember(g, uid)` for adds and `s.identity.RemoveMember(g, uid)`
  for removes (methods at `api/internal/identity/groups.go:133,153`), then call
  `s.refreshShareAccess()` and log (not fail) on mirror error, matching
  `users.go:70-72`. Return a clear error if a membership call fails.
- Add pure helper `groupDelta(current, desired []string) (add, remove []string)`
  in `users.go` (lowercase, de-dupe, stable order).
- New `api/internal/server/users_test.go` — table test for `groupDelta`
  (add-only, remove-only, swap, no-op, case differences). No LDAP needed.
- Confirm the UI edit form actually submits groups (`UserForm.svelte`); it does
  per the report, so no UI change is required.

### 4. CR-19 / CR-22 — UI parses errors and ignores failed mutations

Pattern for reads: `const res = await fetch(...); if (!res.ok) throw new Error(...)`.
For writes: check `res.ok` and set the page's existing error state. Do not call
`res.json()` twice.

- `ui/src/lib/components/CatalogBrowser.svelte:30-39` — guard `res.ok` +
  `Array.isArray`; add an error message near the list (line ~77).
- `ui/src/lib/components/InstalledApps.svelte:17-39` — guard load; in
  `uninstallApp` check `res.ok` before reloading and surface the error.
- `ui/src/lib/components/AppInstallModal.svelte:27-37` — `loadApp` checks
  `res.ok` (it already has `error`).
- `ui/src/routes/users/+page.svelte:49-67` — `deleteUser`/`toggleUser` check
  `res.ok`; add an `error` state + banner (none exists today).
- `ui/src/routes/shares/+page.svelte:80-88` — `deleteShare` checks `res.ok`;
  add an error banner.
- `ui/src/routes/backups/+page.svelte:224-232,315-323` — `deleteSchedule`
  (has `error`) and `cancelJob` (has `jobError`) check `res.ok` before the
  follow-up reload/poll.

### 5. Records

- `docs/CODE-REVIEW.md` — mark CR-07, CR-15, CR-18, CR-19, CR-22 resolved using
  the same inline "Resolved in this change" note style as CR-26.
- `AI_Handoff.md` — remove the ntfy-token and UI-silent-failure items from the
  open list under "Where things stand"; keep it ≤1 page.
- `docs/spec.md` — if any MUST covers user-edit group membership or notification
  settings shape, update the spec and its test in the same change (VER-4); if
  none, say so in the commit rather than editing spec.

## Validation

```bash
cd api   && go build ./... && go vet ./... && go test ./...
cd agent && go build ./... && go vet ./... && go test ./...
cd ui    && npm run check
helm lint charts/naslos -f charts/naslos/values.yaml
```

- Manual API check: `GET /api/notifications` (admin headers) must not contain
  the token value; grep the JSON for the seeded token string and expect it absent.
- Optional live check when .117 is reachable: `npx playwright test` (the suite
  covers the notifications page; the new error paths are hard to trigger
  automatically, so a manual forced-500 check is acceptable evidence).
- `-race` is **out of scope** (CR-06 not fixed here), so do not add it to gates.

## Out of scope

- CR-06 (`-race`), CR-09 (CI), CR-05/CR-08 (dependency bumps), CR-16
  (observability), CR-21 (shared modal), and the remaining report items.
- No changes to the `.118` copy or to the `.117` deployment in this batch. If a
  live drill is wanted, it needs fresh image tag suffixes (see `AI_Handoff.md`
  gotcha 1) and is a separate follow-up.

## Risks

- Changing `UpdateSettings`' signature is source-breaking across the api module;
  the compiler enumerates all call sites (handler + 3 test call sites listed).
- Token-preservation semantics must not let a normal UI save wipe the token:
  the UI omits the field unless the operator types one, and only `""` clears.
- Group deltas run against LDAP; a missing group returns an error and the PUT
  fails rather than silently half-applying. Test the pure helper, not LDAP.
