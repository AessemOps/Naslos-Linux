# AI Handoff — Naslos

## Current branch: `feature/dashboard-and-metrics` (dashboard & metrics work)

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
