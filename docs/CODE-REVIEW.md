# Naslos — full code review

**Baseline:** `master` `6227595` (PRs #9–#16 merged, buddy walk-bounds included).
**Date:** 2026-09-17. **Scope:** whole repository — Go API, Go agent, Svelte UI,
Helm chart and images, scripts, docs, tests. **Mode:** report only; nothing changed.

Severity policy used below: 🔴 **blocking** = security/privacy defect, a data-loss
path, or a documented feature that is broken or unsafe; 🟡 **important** =
correctness/reliability/maintainability defect that will bite; 🟢 **optional** =
polish or consistency. Each finding states whether it was **verified** (executed
or traced) or is **judgement**.

---

## 1. Executive summary

The code is in good shape for what this is: a single-node NAS appliance with two
unusual properties — a privileged host agent, and a zero-knowledge backup feature —
and, unusually valuable for a review, a **decision log** (`docs/spec.md` +
`AI_Handoff.md`) that a reviewer can hold the code against.

The recent security work holds up under a fresh look. The owner-route gate, the
agent bearer token, the escaping/validation batches, the receiver-integrity
hardening and the buddy envelope all survived re-examination, and the two things I
expected to be broken turned out not to be: the agent's streaming client correctly
has **no** total timeout (it is context-bounded, `api/internal/agent/backup.go:41`),
and retention itself keeps a chain's dependencies.

What the review did find is that **the wiring around the code is weaker than the
code**. Three items are blocking in my judgement:

1. **The chart's Authelia policy bypasses authentication for the entire domain**
   (CR-01). The 2FA/one-factor rules are unreachable, so the documented
   authentication guarantee does not exist; with the API's own proxy-secret gate
   still in place, a chart-configured install over the ingress also returns 401
   everywhere — the UI is unusable, which is how this would be noticed.
2. **`helm uninstall` deletes the namespace and, with it, data Helm does not own**
   (CR-02) — the OpenLDAP StatefulSet and PVCs applied outside the chart, the
   shares config volume, and the **buddy identity** whose loss makes every stored
   backup unreadable. `make uninstall` is a documented command.
3. **Authorization is not granular** (CR-03): `RequireAdmin`/`IsAdmin` exist and are
   never wired, so once authentication works, *any* authenticated user has full
   admin — including the root shell in the terminal.

Beyond those, the notable themes are: dependency exposure in the Helm/containerd
chain (**18 reachable Go vulnerabilities**, CR-05); a repository that cannot run
its own test suite under `-race` (CR-06); secrets that leave the API the wrong way
(the ntfy token in a browser response, CR-07; a world-readable password-hash file on
the host, CR-15); an observability story that is deployed but not wired (CR-16); and
a consistent pattern of **silent failure** in the UI (CR-18, CR-19, CR-22) and in
some handlers (CR-13).

The documentation-to-code drift is real but shallow: the specs are largely honest,
and the drift is concentrated in `docs/api.md` (§3), `docs/operations.md`, and
`AI_Handoff.md`, which at 1 227 lines has become an append-only log whose sections
contradict each other (CR-26).

**What was executed (not just read):** `go build`/`go vet`/`go test` (api, agent),
`go test -race` (api — **fails**, CR-06), `govulncheck` (api: 18 reachable; agent:
clean), `npm audit` (11 advisories), `npm run check` (0 errors/0 warnings),
`helm lint` (both value sets), `npx playwright test` against the VM (**30 passed /
2 skipped**). Everything else is a code trace, marked as such.

**Not covered:** git history (secret scanning), the live cluster's runtime
behaviour beyond the Playwright run, the two Go modules' build reproducibility, and
anything requiring the ability to install packages (no `golangci-lint`,
`staticcheck`).

---

## 2. Scope and method

| Area | Reviewed by | Basis |
| --- | --- | --- |
| `api/` (Go) | main reviewer | code trace + executed gates |
| `agent/` (Go) | subagent + main reviewer | code trace |
| `ui/` (Svelte 4/TS) | subagent | code trace against API shapes |
| `charts/`, images, `scripts/`, `Makefile` | subagent | file reads, `helm lint` |
| docs/spec/test mapping | subagent | greps + reads |
| Security design | main reviewer | traces + executed scans |

Skills consulted: `code-review-skill` (framework, severity labels, Go and
architecture references), `security-best-practices` (report format — note its
language reference files are **not installed** in this environment, so this review
used the framework and the review skill's security guide rather than claiming
framework-specific guidance), `web-design-guidelines` and `docker-expert` were
applied inside the UI and chart passes respectively. `cline-sdk` and `kilo-config`
do not apply to this repository.

---

## 3. Findings

### Blocking

#### CR-01 — The Authelia policy bypasses authentication for the whole domain 🔴

**Evidence:** `charts/naslos/templates/authelia-config.yaml:117-139`. The rule
`- domain: {{ .Values.authelia.domain }} / policy: bypass` (lines 126-127) has **no
`resources`**, so it matches every request on that host; Authelia applies the
**first** matching rule, so the later `one_factor` (129-130) and `two_factor`
(132-139) rules never apply.

**Impact:** the documented "authenticated session, 2FA for `/api/users`" guarantee
does not exist. forwardAuth returns 200 *without* `Remote-User`, and because the API
now also requires the proxy secret + identity header (`SEC-10`), every owner route
reached through the ingress answers 401: the deployment is broken rather than
silently open, which is the only reason this is not worse. Verified by reading; the
first-match ordering is Authelia's documented behaviour.

**Fix:** give the bypass rule `resources` scoped to Authelia's own endpoints
(`^/api/(verify|authz|firstfactor|secondfactor|reset-password|logout).*$`) and order
the specific `two_factor` rules before the generic `one_factor`. Effort S.

#### CR-02 — `helm uninstall` deletes the namespace and data Helm does not own 🔴

**Evidence:** `charts/naslos/templates/namespace.yaml:1-12` creates the namespace
with no `helm.sh/resource-policy: keep`; OpenLDAP (StatefulSet + PVCs) is applied
outside the chart (`openldap/manifests/statefulset.yaml:2-3`,
`scripts/deploy-vm.sh:338-350`), and the shares-config PVC holds
`shares.json`, `buddy-peers.json`, `buddy-schedules.json`, `notifications.json` and
`buddy-identity.json`.

**Impact:** `make uninstall` (`Makefile:165-166`) deletes the namespace, which
cascade-deletes the LDAP StatefulSet and its data volumes, the state PVC, and the
buddy identity — losing the identity makes **every stored backup unreadable**
(CR from the rook: `docs/buddy-backup.md:65-69`). Data-loss path from a documented
command. Verified by reading.

**Fix:** drop the namespace from the chart (rely on `--create-namespace`), or
annotate it `helm.sh/resource-policy: keep`, and say so in `docs/operations.md`.
Effort S.

#### CR-03 — Authorization is not granular: any authenticated user is an admin 🔴

**Evidence:** `api/internal/auth/middleware.go:112-127` (`RequireAdmin`) and
`api/internal/auth/context.go:31-33` (`IsAdmin`) have **no call sites**; the route
table wraps every owner route in `RequireAuth` only
(`api/internal/server/server.go:280-295`).

**Impact:** once CR-01 is fixed, every Authelia account — not just `naslos_admins` —
can manage users and groups, create and destroy pools, wipe disks, change share
paths, read logs and open the **root** terminal. The capability exists but is not
wired. Verified by grep and reading.

**Fix:** wrap the destructive surface (users, groups, pools/datasets/disks, terminal,
shares, app install) in `RequireAdmin`, or decide explicitly that every authenticated
user is an operator and record that in `docs/spec.md`. Effort M.

### Important

#### CR-04 — Share-folder operations follow symlinks; containment is only lexical 🟡

**Evidence:** `agent/internal/shares/folders.go:118-133` (`CleanFolderPath`:
`filepath.Clean` + prefix check only) versus `folders.go:63,75,98,109`
(`os.Stat`/`os.ReadDir`/`os.Mkdir`/`os.Remove` on the cleaned path, all of which
follow symlinks).

**Impact:** an authenticated SMB user who can create a symlink inside a share
(`mfsymlinks`) can make the root agent create — or remove an empty — directory
outside the share's dataset, e.g. `/etc/<name>`. Directory structure only, no file
content; still a privileged component acting on attacker-controlled links.
Judgement (traced); exploitability depends on the SMB client's symlink support.

**Fix:** `filepath.EvalSymlinks` the resolved parent and re-assert containment before
acting; refuse symlinked components. Effort S.

#### CR-05 — 18 reachable vulnerabilities in the API's dependency chain 🟡

**Evidence (executed):** `govulncheck ./...` in `api/` — *"Your code is affected by
18 vulnerabilities from 5 modules"*: `containerd@v1.7.12` (8, fixes available up to
v1.7.35), `helm.sh/helm/v3@v3.16.0` (4, fixes in v3.17.3/3.18.5),
`docker@v25.0.6+incompatible` (3), `moby/spdystream@v0.4.0` (1, fix v0.5.1),
`golang.org/x/crypto@v0.55.0` (1, no fix listed). All arrive transitively through the
Helm client used by the app catalog. `agent/` is clean (no external dependencies).

**Impact:** the reachable paths are chart handling, archive/media-type parsing and
HTTP plumbing; the practical exposure is DoS/parsing bugs reachable from
admin-triggered catalog operations, not unauthenticated code execution. Still a
patchable backlog with clear target versions.

**Fix:** bump `helm.sh/helm/v3` to ≥3.18.5 (it pulls newer containerd/docker) and
re-run `govulncheck`; wire it into CI. Effort S.

#### CR-06 — The repository's own suite fails under `-race` 🟡

**Evidence (executed):** `go test -race ./...` in `api/` →
`WARNING: DATA RACE` / `FAIL TestBuddySchedulerFiresDueEntryAndPrunes`. Write at
`api/internal/server/buddy_scheduler_test.go:62` (`h.server.notifications = mgr`)
racing a read at `api/internal/server/buddy_jobs.go:555` (`s.notifications` in
`notifyBuddyJob`) from the job goroutine.

**Impact:** production risk is low — `Server.notifications` is set once in `New`
and never mutated there — but the suite cannot be run with race detection, which
means real races elsewhere stay invisible, and the field is unsynchronised mutable
state. Verified by executing the suite and tracing both sides.

**Fix:** build the notification manager before starting any job in the test (or take
the field's value behind the existing mutex), then add `-race` to a `make test`.
Effort S.

#### CR-07 — `GET /api/notifications` returns the ntfy auth token 🟡

**Evidence:** `api/internal/server/notifications.go:14` writes
`s.notifications.GetSettings()` wholesale; `api/internal/notifications/manager.go:74-78`
returns the struct by value including `AuthToken` (`notifications.go:41`).

**Impact:** the ntfy publish token is disclosed to the browser of any admin session
and lives in page state (masked only visually). Anyone with the page open, or any
script running there, can read it — and it is the same credential that can publish
to the operator's topic. Verified by reading.

**Fix:** return `hasAuthToken: true/false` instead of the value, and only accept a
replacement when the operator types one. Effort S.

#### CR-08 — UI dependency advisories: 11 (1 high, 6 moderate, 4 low) 🟡

**Evidence (executed):** `npm audit` — `cookie <0.7.0` (high) via
`@sveltejs/kit`; `esbuild <=0.24.2` (moderate, dev-server request exposure) via
`vite`; `svelte <=5.55.6` (moderate: several SSR-XSS and one DOM-clobbering
advisory); plus low items in the same chain.

**Impact:** the shipped artefact is a **static** build served by nginx
(`ui/svelte.config.js` uses `adapter-static`), so SSR-only advisories do not apply
in production; the DOM-clobbering advisory and the dev-server ones are the parts
that matter, and they are build-time or conditional. Verified by executing.

**Fix:** schedule the Svelte 5 / Vite bump; at minimum move off the deprecated
`xterm` packages (CR-31) in the same pass. Effort M.

#### CR-09 — No continuous integration, linters, or `make test` 🟡

**Evidence (executed/verified):** `.github/` does not exist; no
`golangci-lint`/`.eslintrc`/`prettier` config anywhere; the `Makefile` has build and
image targets but **no test target** (`Makefile:36-80`, `:174-175`); no coverage or
`-race` usage.

**Impact:** quality rests on manual discipline — which has worked so far, but is why
CR-06 went unnoticed and why the Playwright suite's tolerance (CR-23) is invisible.
Verified by inventory.

**Fix:** one workflow running `go build/vet/test -race` for `api`+`agent`,
`npm run check`, `helm lint`, and the Playwright suite against a seeded environment;
add `make test`. Effort M.

#### CR-10 — No `.dockerignore`; build contexts are the repository root 🟡

**Evidence (verified):** no `.dockerignore` anywhere; `Makefile:55,58,61,80` build
`api`, `agent`, `ui` and the receiver with context `.`, so the context contains
`.git/`, `.kilo/`, and — untracked but present on disk —
`bootstrap/vm/controlplane.yaml` (machine + etcd CA private keys),
`bootstrap/vm/talosconfig*` and `support-naslos-vm.zip.age`.

**Impact:** today the Dockerfiles copy only their own subdirectory, so nothing leaks
into layers; the risk is a future `COPY . .`, a support bundle, or a slower build.
`git ls-files` confirms these secrets are **untracked**, so this is hygiene, not a
committed-secret incident. Verified by reading `COPY` lines and `git ls-files`.

**Fix:** add a root `.dockerignore` (`.git`, `.kilo`, `bootstrap`, `*.age`, `bin`,
`node_modules`, `ui/test-results`). Effort S.

#### CR-11 — Duplicate YAML keys in `values-vm.yaml` silently discard a block 🟡

**Evidence:** `charts/naslos/values-vm.yaml:33-34` (`talosConfigSecret` twice) and
`:40-41` (`agent:` empty then `agent:` mapping). Helm's YAML parsing is last-wins,
so the first `agent:` block is dead and edits there would be ignored; strict
parsers reject the file. Verified by reading.

**Fix:** remove the duplicates and run `helm lint` with both value files (it passes
today because lint is not strict about duplicates). Effort S.

#### CR-12 — A client disconnect does not cancel the agent's running ZFS command 🟡

**Evidence:** the agent builds every command with the agent-lifetime context
(`agent/internal/zfs/pool.go:130-134`, `agent/internal/zfs/backup.go:215-221`),
never the request's. The streaming handlers have no read timeout
(`agent/internal/server/backup.go:105-140`).

**Impact:** a caller that opens `/api/v1/zfs/receive/...` and stalls holds
`zfs receive -F` — including its rollback/overwrite effect — indefinitely, and
`Shutdown(context.Background())` then waits on it. The API's own cancel path works
(it kills the agent stream from the sender side, verified in the earlier drills),
so the exposure is a peer of the agent that misbehaves. Judgement (traced).

**Fix:** derive the command context from `r.Context()`, and set
`ReadTimeout`/`ReadHeaderTimeout` on the agent server. Effort M.

#### CR-13 — Handlers report the wrong class of failure 🟡

**Evidence:** `agent/internal/server/server.go:279` (any `AddVDev` error → 400,
including a broken host), `:330`, `:340` (dataset create/destroy), `backup.go:59`
(estimate), `shares.go:96,113,122` (any `ListFolders` error → 404). The agent has a
correct classifier (`writeClientError`, `server.go:395-402`) that these bypass.

**Impact:** the API and the UI cannot distinguish "you asked for something wrong"
from "the node failed", which is exactly the distinction the 400/500 split was added
for (NAS-003). Verified by reading.

**Fix:** route every backend error through `writeClientError`, and add a not-found
sentinel instead of blanket 404s. Effort S.

#### CR-14 — Partial state is possible in pool creation and share config apply 🟡

**Evidence:** `agent/internal/zfs/operations.go:130-227` — `CreatePool` can fail
*after* `zpool create` succeeded (mountpoint/option `zfs set`), leaving a created but
unconfigured pool that a retry rejects as "already exists".
`agent/internal/shares/shares.go:153-205` — `Apply` writes smb.conf, ganesha.conf,
smbusers, three extrausers files, the revision and directories sequentially with no
lock and no rollback, and writes the revision **before** the samba state dirs.

**Impact:** a mid-apply failure leaves a mix of revisions on disk while the UI shows
success (the apply result is not surfaced, see CR-22/CR-27); a failed pool creation
needs manual cleanup. Judgement (traced).

**Fix:** write the revision last and guard `Apply` with a mutex; on post-create
failure in `CreatePool`, either continue best-effort and report, or roll back
explicitly. Effort M.

#### CR-15 — The NSS shadow mirror is written world-readable 🟡

**Evidence:** `agent/internal/shares/shares.go:183` writes
`{"shadow", cfg.NSSShadow, 0644}` while the sibling `smbusers` file is deliberately
`0600` (`shares.go:165`).

**Impact:** password hashes (NT/LM) land in a `0644` file on the host, readable by
anything that can read that path, defeating the intent next to it. Verified by
reading the mode literal.

**Fix:** write the hidden hash files `0600`. Effort S.

#### CR-16 — Observability is deployed but nothing scrapes it 🟡

**Evidence (verified):** no `ServiceMonitor`, `PodMonitor`, `prometheus.io/scrape`
annotation or `additionalScrapeConfigs` anywhere in the chart; `/api/metrics` serves
**JSON** (`api/internal/server/metrics.go:48-56`), which Prometheus cannot ingest;
`docs/monitoring.md:64` claims Prometheus scrapes Kubernetes metrics; the Grafana
dashboard config is a title-only placeholder (`values.yaml:304-306`).

**Impact:** the Prometheus/Grafana stack in the chart shows nothing, and a failing
backup or a full pool is only visible in the UI. The docs promise otherwise.

**Fix:** expose a Prometheus text endpoint (or an exporter) and a
`ServiceMonitor`/scrape config; fix the doc claim. Effort M.

#### CR-17 — Server lifecycle hygiene: an unstoppable goroutine and an unbounded shutdown 🟡

**Evidence:** the metrics collector goroutine has no stop path
(`api/internal/server/metrics_collector.go:36-47`) and `Shutdown` does not stop it
(`api/internal/server/server.go:344-354`); `main` calls
`srv.Shutdown(context.Background())` with no deadline and then `os.Exit(0)`, which
skips the deferred `tc.Close()` (`api/cmd/main.go:34-41`); `Start` returns
`http.ErrServerClosed` on a normal shutdown and logs it as "Server stopped"
(`api/cmd/main.go:35-40`).

**Impact:** a graceful shutdown can hang on a long-lived terminal WebSocket or a
slow request until the pod's grace period kills it, and the collector keeps working
during shutdown. Low operational risk, real smell. Judgement (traced).

**Fix:** give the collector a stop channel, bound the shutdown context, log
`ErrServerClosed` as normal. Effort S.

#### CR-18 — Editing a user's groups silently does nothing 🟡

**Evidence:** `ui/src/lib/components/UserForm.svelte:63-90` PUTs a body including
`groups`; `api/internal/server/users.go:97-114` decodes `Groups` and never applies
it (only `UpdatePerson`; `AddMember` is used on the create path at `users.go:61`).

**Impact:** an operator unchecks a group in the edit dialog, gets a success, and the
membership is unchanged — a silent correctness bug in an access-control path.
Verified by tracing both sides.

**Fix:** apply membership deltas in the PUT path, or remove `groups` from the edit
form and point at the Groups page. Effort S.

#### CR-19 — The UI parses error bodies as if they were data 🟡

**Evidence:** unchecked `res.ok` before `res.json()` in
`ui/src/routes/notifications/+page.svelte:48-49` (then `.includes` on line 151
throws), `ui/src/lib/components/CatalogBrowser.svelte:32-33` (then `apps.filter`
throws), `InstalledApps.svelte:20-21`, `AppInstallModal.svelte:29-31`. Pages that do
check (`shares/+page.svelte:35`, `Dashboard.svelte:42`) show the pattern.

**Impact:** any API error turns into a render-time exception and a blank page
instead of a message — exactly when the operator most needs the message. Verified by
tracing each path.

**Fix:** check `res.ok`/`Array.isArray` before using the payload, as the other pages
already do. Effort S.

#### CR-20 — The terminal page can open a socket after it is disposed 🟡

**Evidence:** `ui/src/routes/terminal/+page.svelte:115-146` — `connect()` awaits a
preflight `fetch`, then writes to `term` and opens the WebSocket; the `onMount`
cleanup disposes the terminal and closes the socket without any mounted guard
(`:217-221`). `ws.onclose` (`:165-169`) also writes to a disposed terminal.

**Impact:** leaving the page during the preflight produces post-dispose writes and a
socket with no owner (and a thrown error in the console). Verified by tracing; the
exceptional path's exact symptom is unconfirmed.

**Fix:** keep a `disposed` flag plus an `AbortController` for the preflight and bail
out before touching `term`/`ws`. Effort S.

#### CR-21 — Modals are not dialogs: no role, focus trap, Escape or focus restore 🟡

**Evidence:** eight modals are plain `div`s — `ShareForm.svelte:240`,
`UserForm.svelte:104`, `GroupForm.svelte:82`, `AppInstallModal.svelte:64`,
`routes/pools/+page.svelte:149`, `routes/pools/[name]/+page.svelte:373,423`,
`Dashboard.svelte:194`. Grep finds no `role="dialog"`, no `aria-modal`, no Escape
handler, no focus management.

**Impact:** keyboard and screen-reader users cannot reliably open, navigate or close
a dialog; focus stays behind the overlay. Verified by grep/reading.

**Fix:** one small `Modal.svelte` with `role="dialog" aria-modal="true"`, initial
focus, Tab containment, Escape and focus restore, used by all eight. Effort M.

#### CR-22 — Mutations are fire-and-forget, so failures are invisible 🟡

**Evidence:** no status check on delete/enable/disable/uninstall/cancel in
`ui/src/routes/users/+page.svelte:52,62`, `routes/shares/+page.svelte:83`,
`InstalledApps.svelte:34`, `routes/backups/+page.svelte:227` (delete schedule),
`:318` (cancel job); several `.catch`-less `fetch` chains.

**Impact:** a rejected delete or a failed cancel looks like success until the page
is reloaded. Pairs with CR-19 as the dominant UI reliability theme. Verified by
tracing.

**Fix:** check `res.ok` and surface the error (the pages already have an error
banner). Effort S.

#### CR-23 — The test suite can pass while the feature is broken 🟡

**Evidence:** `ui/tests/backups.spec.ts:131` **skips** when the send failed with
`unknown key|not authorized|not configured|out of scope` — a regression matching
those words is reported as skipped; blanket skips for a missing identity/dataset at
`:53,102,104,139,141`; `ui/tests/terminal.spec.ts:38,87` self-skip in the default
NodePort posture so the terminal has no active coverage; `ui/playwright.config.ts:11`
hardcodes the VM base URL.

**Impact:** the suite's green tick is weaker than it looks, and CI (once added) would
inherit that. Verified by reading the specs.

**Fix:** seed fixtures (identity, dataset, peer scope) so the happy paths assert
instead of skipping; fail (not skip) when a precondition is missing in CI; make the
base URL configurable. Effort M.

#### CR-24 — Spec and `docs/api.md` claim things the code does not do 🟡

**Evidence:** `docs/spec.md:304` (FR-APP-04 start/stop "MUST scale replicas 0↔N")
versus `api/internal/server/apps.go:149-159` (`"not yet implemented"`) and a route
that is never registered (`server.go:218-219`) though `docs/api.md:81-82` documents
it; `docs/spec.md:191-193` (FR-IDN-05 "0 **or any past date** ⇒ disabled") versus
`api/internal/identity/persons.go:152-157` (only `"0"`); `docs/api.md:22-25` still
says authentication is "not enforced inside the API router today"; `docs/spec.md:519`
documents `PUT /api/users/{uid}/password` while the handler accepts **POST**
(`users_extra.go:19-22`); `docs/api.md` omits `/api/ready` and the shares/buddy
sub-routes; `KUBECONFIG` is documented (`api.md:220`) but never read — only a
`-kubeconfig` flag exists and it is not passed to `Server`
(`api/cmd/main.go:19`, `server.go:155-173`).

**Impact:** an integrator following the docs builds against endpoints that 405 or
misses the one that exists; the "not enforced" line actively misleads about the
security posture. Verified by reading both sides.

**Fix:** bring the spec and `api.md` in line with the routes, or mark the gaps
`[OPEN]` as the spec's own rule requires. Effort M.

#### CR-25 — Operational documentation describes a system that no longer exists 🟡

**Evidence:** `docs/operations.md:11-12` puts LDAP backups in
`/var/lib/ldap/backups` and uses `slapcat -n 0/-n 1`, while the CronJob writes
`/backups/ldap` with `slapcat -F /etc/ldap/slapd.d`
(`openldap/manifests/backup-cronjob.yaml:26,32-33`); `operations.md:17,24` exec
`deploy/naslos-openldap` but the workload is a **StatefulSet**; `docs/architecture.md:70,120,200-245`
still describes the `kubectl exec … pdbedit` NT-hash sync, while the live path is the
agent-pushed `smbusers` mirror and the old path is dead code
(`api/internal/identity/samba.go` — `NewSMBManager` is never called, like
`RequireAdmin`); `docs/deployment.md:247` credits `TALOS_ENDPOINTS` while the chart
sets `TALOSCONFIG`; `ui/nginx.conf:65-73` tells operators to set
`terminal.requireAuth=false`, a key that no longer exists (now `auth.disabled`);
`docs/notifications.md:21-28` omits the `backup_success`/`backup_failure` events;
`docs/deployment.md:11` and `docs/development.md:48` say "go 1.22+" while both
modules require **1.26.5**.

**Impact:** recovery and upgrade procedures that do not correspond to the system are
worse than none. Verified by reading.

**Fix:** a focused docs pass; delete the dead Samba path in the same change. Effort M.

#### CR-26 — `AI_Handoff.md` has become unusable as a handoff 🟡

**Evidence:** 1 227 lines / 73 KB; `:42` says "Nothing is outstanding" while
`:68-72` lists three open items; FR-MET-10 is marked `[OPEN]` at `:196,260,306,360`
and implemented at `:107`; `:945` says "Playwright: 8 tests" (now 32); `:1085,1200`
quote long-obsolete image tags; `:1210` names a dead "current work" branch.

**Impact:** the project's most important continuity artefact contradicts itself in
adjacent sections. Verified by reading.

**Fix:** replace it with a ≤1-page current-state summary plus a dated archive of the
old log. Effort S.

### Optional

| ID | Finding | Evidence | Fix |
| --- | --- | --- | --- |
| CR-27 | `FreeDisks` filters with `Mode().IsRegular()`, so block-device nodes never match on a real node | `agent/internal/zfs/devices.go:145` | Use `ModeDevice`; the one consumer (`agent/internal/server/server.go:261`) is an endpoint the API never calls today, so this is latent |
| CR-28 | TOCTOU between the `os.Stat` validation and the later `wipefs`/`zpool` exec | `agent/internal/zfs/devices.go:98` vs `operations.go:164,190,246` | Re-validate or open the device and pass the fd |
| CR-29 | Validation trims values but the raw value is executed | `agent/internal/zfs/dataset.go:121,175,189,218,242` | Normalise once and execute the normalised value |
| CR-30 | Agent `Status` returns agent-mount paths (`/host/var/...`) to the API | `agent/internal/shares/shares.go:105,211-212` | Translate to in-host paths |
| CR-31 | Deprecated packages and unused deps in the UI | `ui/package.json:21,28-29` (`xterm`, `xterm-addon-fit`, unused `adapter-auto`) | Migrate to `@xterm/*`; remove `adapter-auto` |
| CR-32 | Every component re-declares API types; one is already wrong | `routes/backups/+page.svelte:58` (`created` vs API `createdAt`, `api/internal/buddy/store.go:547`) | Add `ui/src/lib/types.ts` generated from (or checked against) the API |
| CR-33 | `any` and `catch (e: any)` in application code | `pools/[name]/+page.svelte:54`, `Dashboard.svelte:8`, `SchemaForm.svelte:3-29`, several `catch`es | Type the disk list (an interface already exists in `DiskWizard.svelte:4`); narrow catches |
| CR-34 | Six components exceed 300 lines and mix responsibilities | `backups/+page.svelte` (565), `pools/[name]/+page.svelte` (508), `DiskWizard.svelte` (396), `ShareForm.svelte` (394), `terminal/+page.svelte` (316), `Dashboard.svelte` (256) | Split along the seams already visible in the sections |
| CR-35 | No `aria-live` for polled data; validation feedback is only a disabled button | `Dashboard.svelte:118-154`, `backups/+page.svelte:500-516`, `UserForm.svelte:171` | `role="status" aria-live="polite"` for updates; inline field messages |
| CR-36 | `setTimeout` reset flags never cleared; pool-detail param not reactive | `backups/+page.svelte:173`, `shares/+page.svelte:64`, `pools/[name]/+page.svelte:62` | Clear on destroy; make the param reactive |
| CR-37 | Agent/UI have no probes; no PDBs; no NetworkPolicy; namespace-wide `privileged` PSA; over-broad RBAC (agent ClusterRole and bootstrap exec Role are unused) | `agent-daemonset.yaml:29-67`, `ui-deployment.yaml:51-58`, `namespace.yaml:8`, `agent-daemonset.yaml:111-119`, `openldap/manifests/bootstrap-job.yaml:98-109` | Add probes/PDBs; scope PSA; drop unused RBAC |
| CR-38 | Helm hardening gaps: no `values.schema.json`; mutable tags with `IfNotPresent`; hardcoded cluster-scoped names break multi-release; `.Values.namespace` instead of `.Release.Namespace`; `trustForwardHeader: true`; `LDAPTLS_REQCERT=never` | `values.yaml` (no schema), `_helpers.tpl:48-67`, `agent-daemonset.yaml:114,123`, `traefik-middleware.yaml:12`, `api-deployment.yaml:32-34` | Add a schema; prefer digests (already supported); prefix names; scope trust |
| CR-39 | Authelia JWT regenerates on every render; LDAP bind password and passwords travel via argv; `LDAP_ADMIN_PASSWORD=admin` is baked into the image ENV | `authelia-config.yaml:15,91`, `openldap/generate-secrets.sh:19-21`, `openldap/manifests/bootstrap-job.yaml:70,74`, `openldap/image/Dockerfile:13` | Generate-once via `lookup`; `--from-env-file`; `-y <file>`; remove the default |
| CR-40 | Container images are tag-pinned, the UI nginx runs as root with no healthcheck, and `go.sum` is not copied before `go mod download` | `api/Dockerfile:4,9-10,17`, `ui/Dockerfile:17-27`, all Dockerfiles | Pin digests; non-root nginx; copy `go.sum` first |
| CR-41 | Prometheus/Grafana ship without limits or retention sizing; no log rotation for the in-container service logs | `values.yaml:277-306`, `nfs/image/entrypoint.sh:15` | Set limits/retention; ship logs to stdout or cap |
| CR-42 | `docs/spec.md` §7 has no rows for ~20 requirements and mis-attributes the NT-hash test to `api/internal/shares` (it is in `api/internal/identity/nthash_test.go:11`) | `docs/spec.md:609-642` | Complete the mapping; the spec's own rule expects it |
| CR-43 | Packages with no tests: `api/internal/{agent,catalog,config,helm,metrics}`, `api/cmd/*`, `agent/cmd`; no fuzz/bench; no UI component tests | test-file inventory (31 Go files, 149 tests, 0 fuzz) | Cover the app-catalog/Helm path and the config parsers first |
| CR-44 | `docs/README`-level drift: `Makefile:31` points at a "rename note" that does not exist; `docs/development.md:29` calls `zsh-terminal/` the web-terminal image (the built one is `terminal/image/Dockerfile`) | as listed | Delete the stale references |
| CR-45 | Unused-import suppression hides a missing logger: `var _ = log.Printf` | `agent/internal/server/server.go:404` | Remove the import and the line, or use the logger where it was intended |

### Checked and found sound (so they are not re-investigated)

- **Streaming timeouts.** The agent's streaming client deliberately has **no** total
  timeout (`api/internal/agent/backup.go:41-42`), and the buddy client's 5-minute
  timeout bounds **one** chunk request, not a whole push
  (`api/internal/buddy/client.go:39,63`). A multi-gigabyte backup is not at risk.
- **Buddy envelope and auth.** Signature over method+path+body digest+timestamp+nonce,
  constant-time token compare, fail-closed replay cache, rollback refusal, dependency
  aware retention — verified again in passing (`api/internal/buddy/*`).
- **Agent command construction.** No shell anywhere; all `zpool`/`zfs`/`wipefs`
  execution goes through argv with validators in front
  (`agent/internal/zfs/pool.go:131`, `backup.go:216`).
- **No secrets in agent logs**, and the buddy private key/KEK are never served to the
  browser (`api/internal/buddy/keys.go:37-47`).
- **Product version is consistent** at `0.1.0` across spec, chart, UI and Makefile
  (VER-1 holds).
- **All test names cited in spec §7 resolve** to real test functions; no dangling
  references.

---

## 4. Engineering hygiene gaps

| Gap | Evidence | Suggested fix |
| --- | --- | --- |
| No CI | `.github/` absent | `build + vet + test -race`, `svelte-check`, `helm lint`, Playwright |
| No linters | no golangci-lint/eslint/prettier config | add them; fix the baseline once |
| No `make test` | `Makefile` has no test target | add `test` and `test-race` |
| No coverage / fuzzing | none anywhere | `-coverprofile` in CI; fuzz the parsers |
| No `.dockerignore` | CR-10 | add one |
| No `values.schema.json` | CR-38 | add types + required |
| Race detection unusable | CR-06 | fix the test race, then gate on `-race` |
| Dependency scanning not routine | CR-05, CR-08 were found only because this review ran the tools | run `govulncheck` and `npm audit` in CI |
| Docs hygiene | CR-24, CR-25, CR-26, CR-42, CR-44 | a focused docs pass, and treat `spec.md` as the gate |

---

## 5. Deliberate decisions reviewed and accepted

These are choices, not defects; the review checked they are still coherent rather
than re-litigating them.

| Decision | Where | Verdict |
| --- | --- | --- |
| Single-node product; multi-node aggregation out of scope | `docs/spec.md` §6, FR-MET-10 | Accepted; still consistent |
| NFSv4-only with AUTH_SYS | `docs/shares.md` | Accepted, documented |
| Distroless API/agent images (no shell) | Dockerfiles | Accepted; the OpenLDAP image is reused where a shell is needed |
| Privileged terminal container with `/host` | `terminal.yaml`, spec FR-LOG-02 | Accepted; gated by the owner gate, refused by nginx on the NodePort |
| Agent keeps `hostNetwork` (hostPID removed) | `agent-daemonset.yaml:29-33` | Accepted; needs a live multi-node check to change |
| In-memory job history | `buddy_jobs.go` | Accepted; the receiver's manifests are the durable record |
| Retention keeps a chain's dependencies | `buddy/store.go` | Accepted and tested |
| `auth.disabled` for the VM | `values-vm.yaml` | Accepted as a dev posture; the NAS-008 NodePort exposure is documented, not fixed (CR-03/CR-38) |
| Static UI build (no SSR server) | `svelte.config.js` | Accepted; reduces CR-08's impact |
| Live-VM Playwright suite | `playwright.config.ts` | Accepted for now; hermetic seeding would be better (CR-23) |

---

## 6. Previous audit status (22 NAS findings)

| ID | Title (short) | Status in this review |
| --- | --- | --- |
| NAS-001 | Owner API routes unauthenticated | **Fixed** (proxy secret + identity + CIDR; `SEC-10`), tests added |
| NAS-002 | Agent has no authentication | **Fixed** (bearer token, fail-closed), tests added |
| NAS-003 | Agent input validation gaps | **Fixed** (validators + refusal tests); CR-13/CR-28/CR-29 are refinements |
| NAS-004 | Terminal exec gated only at the edge | **Fixed** (owner gate + nginx refusal) |
| NAS-005 | `/api/ws/logs` unauthenticated | **Fixed** (route composition) |
| NAS-006 | LDAP filter/DN injection | **Fixed** (escaping + allowlist + tests) |
| NAS-007 | smb.conf/Ganesha injection | **Fixed** (field validation + tests); CR-15 is the sibling issue |
| NAS-008 | NodePort exposes the API | **Accepted/documented** (dev posture; `auth.disabled` warns) — still open in a real deployment |
| NAS-009 | WebSocket origin check trusts `X-Forwarded-Host` | **Not addressed** — see CR-38 (`trustForwardHeader`) |
| NAS-010 | Default/hardcoded credentials | **Partially** — placeholders remain (CR-39) |
| NAS-011 | Enrollment replayable across restarts | **Fixed** (persisted marker + no re-keying) |
| NAS-012 | Manifest rollback protection | **Fixed** (shape validation, rollback refusal, receiver-owned prune) |
| NAS-013 | Quota race/undercount/unlimited | **Fixed** (atomic quota, manifest accounting, validation) |
| NAS-014 | Nonce cache in memory/unbounded; 32-bit job IDs | **Fixed** (bounded + persisted cache; 128-bit IDs) |
| NAS-015 | Over-broad RBAC, privileged namespace | **Open** — CR-37 |
| NAS-016 | Verbose errors leak internals | **Open** — CR-13, CR-30 |
| NAS-017 | Samba/NSS file handling sharp edges | **Partially** — CR-15; entrypoint hardening still open |
| NAS-018 | Enumerable jobs, predictable snapshots, state races | **Mostly fixed** (random snapshots, 128-bit IDs, strict schedule dataset check) |
| NAS-019 | TLS/headers/session hardening | **Open** — CR-38 |
| NAS-020 | Known-vulnerable dependencies | **Open** — CR-05 (18 reachable Go), CR-08 (11 npm) |
| NAS-021 | Buddy crypto residual notes | **Addressed** (walk bounds, counter guard, DEK floor, recursive sources) |
| NAS-022 | Supply-chain/operations observations | **Partially** (digest pinning exists and is documented; tags still default, no CI — CR-09/CR-38) |

---

## 7. Recommended fix order

**First (safety and correctness, all small):**
1. CR-01 Authelia rule order — the authentication layer currently does nothing.
2. CR-02 namespace deletion on uninstall — cheapest data-loss fix in the repo.
3. CR-07 ntfy token in the API response; CR-15 `0644` shadow file.
4. CR-03 decide and wire authorization (or document "every authenticated user is an
   operator").
5. CR-19/CR-22 silent UI failures; CR-18 the groups edit no-op.

**Then (backlog that compounds if left):**
6. CR-05 dependency bump + CR-08 UI dependency bump, with CI scanning (CR-09).
7. CR-06 fix the test race and adopt `-race`; CR-23 remove the suite's tolerance.
8. CR-10 `.dockerignore`; CR-11 duplicate YAML keys; CR-38 chart hardening.
9. CR-24/CR-25/CR-26 docs truth-up; CR-16 observability wiring.
10. CR-04/CR-12/CR-13/CR-14 agent refinements.

**Structural (plan deliberately):** CR-03's authorization model, CR-16's metrics
endpoint, CR-21's shared modal, CR-34's component splits, CR-42/CR-43 test-mapping
and coverage work.

---

## 8. Appendix

**Commands executed** (baseline `6227595`):

```
api:    go build ./... ; go vet ./... ; go test ./... ; go test -race ./...   # -race FAILS (CR-06)
        go run golang.org/x/vuln/cmd/govulncheck@latest ./...                # 18 reachable (CR-05)
agent:  go run .../govulncheck@latest ./...                                  # clean
ui:     npm run check                                                        # 0 errors, 0 warnings
        npm audit                                                            # 11 advisories (CR-08)
        npx playwright test                                                  # 30 passed, 2 skipped
chart:  helm lint (defaults) ; helm lint (values-vm)                         # both pass
```

**Limitations.** Git history was not scanned for secrets (only the working tree).
The chart's vendored Traefik subchart schema could not be extracted, so the
`traefik.enabled`-condition observation (CR-38) is a read of the chart helpers, not a
rendered proof. Live cluster behaviour beyond the Playwright suite was not exercised;
agent findings that depend on runtime (CR-04, CR-12, CR-14) are traces, and are
labelled as such. The suite's `-race` state is the one place where the review changed
the picture: it fails, and that is the finding.
