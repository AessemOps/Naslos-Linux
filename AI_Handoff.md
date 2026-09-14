# AI Handoff — Naslos

## Current branch: `feature/shares` (SMB shares + LDAP account sync)

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
