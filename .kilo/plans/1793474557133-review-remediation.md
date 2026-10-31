# Review remediation plan — Naslos-Linux (2026-10-31)

Executor: an autonomous coding agent (DeepSeek). Owner: Aessem.
Companion plan (installer side): `Naslos-Installer/.kilo/plans/review-remediation.md`.

This plan fixes the **verified** findings of the 2026-10-31 code review and its
adversarial re-review. Every item below was re-checked against `origin/master`
`8a5112b`. Line numbers are from that commit; re-locate with `rg` if they drift.

---

## 0. Rules for the executor (read before any change)

1. Read `../AGENTS.md`, `AGENTS.md`, `AI_Handoff.md` ("Operational gotchas"),
   and `docs/spec.md` sections you touch. They override anything here.
2. **One PR per workstream** (W1, W2, ...). Branch from a freshly fetched
   `origin/master`: `git fetch origin && git switch -c fix/<workstream-slug> origin/master`.
   Push with `git push -u origin fix/<slug>` (never a bare `git push`; never to `master`).
   Open the PR with `gh pr create`. Do **not** merge PRs yourself.
3. Conventional Commits: `fix(agent): ...`, `fix(api): ...`, `fix(chart): ...`,
   `fix(ui): ...`, `ci(release): ...`, `docs(...): ...`, `test(...): ...`.
4. **Never create or push a `vX.Y.Z` tag.** Releasing is §6 and is the owner's call.
5. Gates for every PR (all must pass; paste the summary in the PR body):
   ```bash
   cd rust && cargo fmt --all -- --check
   cd rust && cargo clippy --workspace --all-targets -- -D warnings
   cd rust && cargo test --workspace --locked
   cd ui && npm run check && npm run build            # if ui/ changed
   helm lint charts/naslos -f charts/naslos/values.yaml -f charts/naslos/values-vm.yaml
   helm lint charts/naslos -f charts/naslos/values.yaml -f charts/naslos/values-installer.yaml
   sh scripts/audit.sh
   ```
6. Repo rules that apply to almost every item:
   - **Spec rule**: if a MUST in `docs/spec.md` changes, update the spec and its test in the same PR.
   - **Docs rule**: update `docs/api.md` for any endpoint/payload change, the per-topic doc,
     and `AI_Handoff.md` when behavior or deployed state changes.
   - **Credits rule**: any direct dependency change updates `CREDITS.md` + `Last reviewed`.
     Prefer **no new crates**; every fix below is designed to need none.
   - **Install-path rule**: chart/values/Makefile only. No live `kubectl`/`helm --set`.
   - Every behavior fix ships with a test that fails before the fix.
7. **Stop and ask the owner** (do not guess) when: a fix needs a live VM drill, an
   item is marked `DESIGN-NEEDED`, a change would alter the Buddy wire/manifest
   format, or a gate fails for a reason unrelated to your change.
8. Do not touch items in §5 ("Do NOT fix"). They were proven false or already fixed.

---

## 1. Priority order

| Order | Workstream | Why first |
|---|---|---|
| 1 | W1 Agent streaming timeout | Likely breaks every Buddy send/restore > 180 s |
| 2 | W2 Disk-wipe safety (API + agent) | Irreversible data loss |
| 3 | W3 Chart-source hardening | Confirmed secret exfiltration via symlink |
| 4 | W4 App install integrity | Namespace isolation race, consent bypass |
| 5 | W5 Auth/session surface | Terminal sessions never expire; dashboard leak |
| 6 | W6 API secret hygiene + startup correctness | Low effort, removes leak paths |
| 7 | W7 Buddy correctness (non-format) | Silent schedule failures, races |
| 8 | W8 Chart fixes | Render bug, version drift, hardening |
| 9 | W9 UI fixes | Unsafe exposure defaults, misleading states |
| 10 | W10 Agent hardening | Destroy/create safety, limits |
| 11 | W11 CI/release gates | Vacuous CI gate, re-run drift |
| 12 | W12 Test coverage | Contract and argv-boundary tests |
| — | W13 DESIGN-NEEDED | Owner decision first |

W1–W5 are release-blocking for 0.2.3. W6–W12 may land in 0.2.3 or 0.2.4.

---

## 2. Workstreams

### W1 — Agent client: streaming requests must not have a 180 s total deadline  (`fix/agent-stream-timeout`)

**Bug.** `rust/api/src/agent.rs:205` builds the only client with
`reqwest::Client::builder().timeout(DEFAULT_TIMEOUT)` (180 s). In reqwest this is a
total deadline **including body transfer**. `send_stream` (`:546-571`) and
`receive_stream_body` (`:607-630`) reuse it, so any `zfs send`/`receive` longer
than 180 s is cut. The agent side deliberately has no deadline (SEC-2026-D9).

**Change.**
1. In `Client`, keep `http` (180 s total, for JSON calls) and add `stream_http`
   built with the same CA/TLS settings but **no `.timeout()`**, plus
   `.connect_timeout(Duration::from_secs(10))` and
   `.read_timeout(STREAM_IDLE_TIMEOUT)` (reqwest 0.12.28 supports `read_timeout`;
   it is a per-read idle timeout). `STREAM_IDLE_TIMEOUT = 300 s`, overridable by
   env `AGENT_STREAM_IDLE_TIMEOUT_SECS` (parse; reject 0/garbage → default + warn).
2. Factor the TLS/CA builder into one fn so both clients pin the same CA.
3. Add `fn stream_request(...)` mirroring `request()` (bearer applied) and use it
   in `send_stream` and `receive_stream_body` only. `estimate_send` stays on `http`.
4. Add `pub(crate) fn with_timeouts(base_url, token, ca, total, idle)` for tests.

**Tests** (`rust/api/tests/zfs_agent.rs` or a new `agent_stream.rs`): spin an axum
server that streams a body in chunks with a 50 ms gap for ~1.5 s; build the client
`with_timeouts(total=500ms, idle=1s)`; assert the JSON client path times out but the
stream path completes and yields all bytes. Add a second test where the server
stalls > idle and assert the stream errors.

**Docs.** `docs/buddy-backup.md` (timeouts section), `docs/storage-zfs.md` if it
mentions the agent timeout; mention `AGENT_STREAM_IDLE_TIMEOUT_SECS` in
`docs/deployment.md` env table. Chart: no change unless you expose the env (optional
`api.agentStreamIdleTimeoutSecs` in `values.yaml`, default unset).

**Acceptance.** New tests pass; a live Buddy send of a dataset > 2 GiB completes
(owner runs the drill; note the result in `AI_Handoff.md`).

---

### W2 — Disk-wipe safety: fail closed in API and agent  (`fix/disk-wipe-safety`)

Two layers both fail open today.

**API layer** (`rust/api/src/server/zfs.rs:505-537`, `rust/api/src/talos.rs:337+`):
1. `usable` only excludes `system_disk`. Extend `TalosClient` with
   `get_discovered_signatures() -> HashMap<String /*whole-disk dev*/, Vec<String /*fs type*/>>`
   using `talosctl get discoveredvolumes -o json` (spec fields include `dev_path`,
   `parent_dev_path`/`parent`, `name` = filesystem type such as `zfs_member`,
   `xfs`, `vfat`). Map partitions to their parent disk. Write a parser unit test
   from a captured JSON sample (capture one from the VM read-only:
   `talosctl -n 192.168.1.117 get discoveredvolumes -o json` with the repo talosconfig;
   commit the sample under `rust/api/tests/fixtures/`, scrub serials).
2. `in_pool` uses `if let Ok(pools) = client.list_pools()` — **fail closed**:
   if listing pools fails, return an error ("cannot verify disk ownership").
3. In `validate_disk_selection`, refuse any selected disk that has a signature
   unless the request carries `wipeConfirmed: ["/dev/sdX", ...]` listing exactly
   that disk. A `zfs_member` signature is refused **even with** confirmation:
   "disk holds a ZFS pool label; import or destroy that pool first".
4. Surface signatures in the disk list API so the UI can show them
   (`GET` disks response: add `signatures: string[]`). Update
   `ui/src/lib/components/DiskWizard.svelte` (or the current disk wizard file) to
   show a warning badge and a per-disk "I understand this disk will be erased"
   checkbox that fills `wipeConfirmed`.

**Agent layer** (`rust/agent/src/zfs/operations.rs`, `devices.rs`):
5. `pools()` (`operations.rs:59-63`) swallows `pool_disks` errors. Add
   `pool_members_strict()` that returns `Err` if any pool's `zpool status` fails,
   and use it in `create_pool` and `add_vdev`/the add path (`devices.rs:209-231`).
6. Before `wipefs`, union members from `list_importable()` (`operations.rs:290`)
   — parse device names from the importable config — and refuse any overlap.
7. Canonicalize disk paths before comparing: resolve `join_host(host_root, dev)`
   with `std::fs::canonicalize`, require the result under `<host>/dev/`, and compare
   by `st_rdev` (`std::os::unix::fs::MetadataExt::rdev`) instead of strings
   (`devices.rs:83-101,165-184`). Reject partitions (existing `is_whole_disk` logic).
8. Add a process-wide `tokio::sync::Mutex<()>` (`mutation_lock` on `ZfsClient`)
   held across check→wipe→create in `create_pool` and across the add path.
   Re-read ownership **inside** the lock.

**Tests.** Agent (`rust/agent/tests/zfs_dataset.rs` or new `zfs_pool_safety.rs`)
using `FakeRunner` canned output:
- disk present only in `zpool import` output → create refused, no `wipefs` call recorded;
- `zpool status` fails for one pool → create refused;
- `/dev//sdb` alias of a member → refused (use a temp host root with device-like
  files; if `rdev` cannot be faked, compare canonical paths and document);
- two concurrent `create_pool` on the same disk → exactly one reaches `wipefs`.
API: `validate_disk_selection` table test for signature / `wipeConfirmed` / `zfs_member`
/ list_pools error.

**Docs.** `docs/storage-zfs.md`, `docs/api.md` (new `wipeConfirmed`, `signatures`),
spec FR for pool creation if it states the guard (update + `[OPEN]` rules).

**Acceptance.** Tests pass; owner live drill: attach a disk holding an exported
pool to the VM, confirm the wizard refuses it.

---

### W3 — Chart-source hardening  (`fix/chart-source-hardening`)

**Confirmed:** Helm v4.3.0 (the version in `rust/api/Dockerfile`) follows symlinks
out of a chart dir; a chart file `leak.txt -> /var/lib/naslos/buddy-identity.json`
plus `{{ .Files.Get "leak.txt" }}` renders the target into a ConfigMap.

1. Clone/fetch with symlinks disabled: add `-c core.symlinks=false` to every git
   invocation in `rust/api/src/chartsrepo.rs` (`GitCliBackend`, `:396-430` and the
   clone/fetch arg builders). Symlinks then check out as plain files containing the
   target path.
2. Defence in depth: before `install_dir`/`upgrade_dir` (`rust/api/src/helm.rs:134-180`,
   called from `apps.rs`), add `chartsrepo::assert_regular_tree(chart_dir)` that
   walks with `symlink_metadata` and rejects any entry that is not a regular file
   or directory (symlink, fifo, socket, device), and any file > 5 MiB, and more than
   5000 entries. Return a clear error naming the path.
3. Remove `git://` from `validate_source_url` (`chartsrepo.rs:188-194`); update its
   test at `:1007-1013` to assert rejection. At sync time, a persisted source whose
   URL starts with `git://` fails with "git:// sources are no longer supported;
   re-add the source over https:// or ssh://" (do not silently rewrite).
4. Do **not** change the token-in-URL code (`:450-451,607-613`); it is latent
   (`creds: None`). Add a `// TODO(W13-D5)` comment pointing to this plan.

**Tests.** Unit test for `assert_regular_tree` (temp dir with a symlink, a fifo via
`nix`-free `libc::mkfifo` or skip fifo, a regular tree OK). Unit test that the git
arg builder contains `core.symlinks=false`. Update URL validation tests.

**Docs.** `docs/app-catalog.md` (supported URL schemes, symlink rule),
`NaslosCharts/README.md` does not need a change (no chart uses symlinks — verify
with `find NaslosCharts/apps -type l`; if any exist, stop and report).

---

### W4 — App install integrity  (`fix/app-install-integrity`)

1. **Namespace reservation race** (`rust/api/src/apps.rs:241,263,292-318,367`).
   `_mu` is never locked and `allocate_namespace` reads persisted records only.
   - Rename `_mu` to `reservations: Mutex<HashSet<String>>`.
   - `allocate_namespace` computes `used = records ∪ reservations`, picks one,
     inserts it into `reservations` and returns a `NamespaceReservation` guard.
   - On success the record is stored, then the guard is dropped (remove from set).
     On any error path the guard drop also removes it. Implement `Drop` for the guard
     holding an `Arc` back to the set (use `std::sync::Mutex`, never held across `.await`).
   - Test: two concurrent `install` calls with a pool of 2 get distinct namespaces;
     with a pool of 1, the second fails with "no free per-app namespace".
2. **Privileged consent re-check** (`rust/api/src/server/apps.rs:89-122`, `apps.rs:361-368`).
   Add `privileged_confirmed: bool` to `InstallRequest`; set it from the HTTP request.
   In `Manager::install`, after resolving `app`, if `app.privileged && !req.privileged_confirmed`
   → error "app now requires privileged mode; re-confirm the install". Test it with a
   catalog swap between enqueue and run.
3. **Upgrade validation** (`apps.rs:433-471`): call `validate_install_values(&app, &merged)`
   before `upgrade_dir`. Test: an upgrade with a schema-invalid value is rejected.
4. **Secret values** (pairs with W9.2): add a helper `redact_secret_values(app, values)`
   that replaces values of schema fields with `format: password` (or `writeOnly: true`)
   by `null` and adds `"redactedKeys": [...]`. Apply it in `GET /api/apps` and
   `GET /api/apps/{name}` responses (`server/apps.rs:133-140`). On `PUT`/upgrade, a
   redacted key that the client sends back as `null`/absent keeps the stored value.
   Storage in `apps.json` stays as is for now (moving to Secrets is W13-D7).

**Docs.** `docs/api.md` (`privilegedConfirmed` semantics, `redactedKeys`),
`docs/app-catalog.md`.

---

### W5 — Auth and session surface  (`fix/auth-session-surface`)

1. **Terminal/exec/log WebSocket lifetime** (`rust/api/src/server/ws.rs:99-180,381-532`).
   - Env `TERMINAL_MAX_SESSION_SECS` (default 28800 = 8 h) and
     `TERMINAL_IDLE_TIMEOUT_SECS` (default 1800). Idle = no client→server message.
   - Wrap the receive loop in `tokio::select!` with a `sleep_until(deadline)` and an
     idle timer reset on each client message; on expiry send a close frame with a
     reason ("session expired"/"idle timeout"), then kill.
   - Per-user cap: `AppState.ws_sessions: Mutex<HashMap<String, usize>>`, max 4 exec
     sessions per `Remote-User` (env `TERMINAL_MAX_SESSIONS_PER_USER`); reject the
     upgrade with 429 above the cap; decrement via a guard on session end.
   - After `child.kill()` call `child.wait()` (on `spawn_blocking`, 5 s cap) to reap.
   - Make the PTY reader→ws channel non-blocking for the reader thread: when the
     socket task ends, drop the receiver so `blocking_send` returns `Err` and the
     thread exits; do not `await` the output task without a timeout (2 s).
   - Tests: unit-test the timer logic with `tokio::time::pause()`.
   - Expose the env vars in `charts/naslos/values.yaml` (`api.terminal.maxSessionSecs`,
     `idleTimeoutSecs`, `maxSessionsPerUser`) and `templates/api-deployment.yaml`.
2. **Traefik dashboard** (`charts/naslos/templates/authelia-config.yaml:212-229`).
   Replace the `group:naslos_users` deny with an unconditional rule placed **after**
   the admin `two_factor` rule and **before** the catch-all:
   ```yaml
   - domain: {{ .Values.domain | quote }}
     policy: deny
     resources:
       - "^/traefik"
   ```
   Admins still match the earlier subject rule first. Add an `audit.sh` rendered-chart
   assertion that the deny rule has no `subject`.
3. **Authelia `users_filter` requires a Naslos group** (`authelia-config.yaml:126`).
   The OpenLDAP image loads `memberof` (`openldap/image/entrypoint.sh:91-108`). Change to
   `(&(objectClass=inetOrgPerson)({username_attribute}={input})(|(memberOf=cn=naslos_users,<groups ou>)(memberOf=cn=naslos_admins,<groups ou>)))`
   using the chart's existing base-DN values. **Live drill required** (owner): log in as
   an admin, a user, and a group-less user (must fail). If memberOf is not populated
   for existing users (overlay added after creation), stop and report.
4. **`/api/ready` must not hit LDAP per request** (`server/handlers.rs:19-38`).
   Add a background task (in `main.rs` next to the metrics collector) that probes LDAP
   every 15 s with a 2 s timeout and stores `(status, checked_at)` in an
   `ArcSwap`-free `RwLock`/`Mutex` in `AppState`. `/api/ready` reads the cached value
   (stale > 60 s ⇒ "unknown"). Test: handler does not call identity.
5. **Host for CSRF/WS origin** (`server/ws.rs:319-347`, `auth.rs:240-248`): use `Host`
   only; ignore `X-Forwarded-Host`. Traefik preserves `Host`. Update unit tests and add
   one proving `X-Forwarded-Host: evil.tld` + `Origin: https://evil.tld` is refused.
6. **App routes strip client identity** (`rust/api/src/routing.rs:62-86,134-153`): put
   `strip-client-identity` (namespace = release namespace) first on every rendered app
   route regardless of `spec.auth`. Update routing tests/golden output.
7. **POSIX uid allocation** (`rust/api/src/identity.rs:137-146`, `identity/ops.rs:66-70`).
   New users: search `(uidNumber=*)` under the people OU, allocate `max(10000, max_uid)+1`
   while holding the identity connection mutex for the whole search+add. Existing users
   are untouched. Keep `hash_uid` only if referenced elsewhere; otherwise delete it.
   Test with the LDAP fake/mocks used in `tests/shares_users.rs`.

**Docs.** `docs/terminal.md`, `docs/authentication.md`, `docs/identity-sso.md`,
`docs/api.md` (`/api/ready` semantics), spec if a MUST covers WS/terminal.

---

### W6 — API secret hygiene and startup correctness  (`fix/api-secret-hygiene`)

1. `identity.rs:234-244`: remove the inline `LDAP_BIND_PASS` fallback. If only the
   inline var is set → startup error "set LDAP_BIND_PASS_FILE". Verify the chart already
   sets `LDAP_BIND_PASS_FILE` (`api-deployment.yaml`) for all profiles.
2. `identity.rs:22-31`: replace `#[derive(Debug)]` on `Config` with a manual `Debug`
   that prints `bind_pass: "<redacted>"`. Grep the crate for other `derive(Debug)` structs
   holding `token`, `secret`, `password`, `kek`, `dek`, `key` and do the same.
   Unit test: `format!("{cfg:?}")` does not contain the password.
3. `identity.rs:471-498`: when `use_tls` and `ca_cert_path` is empty → error at
   construction; remove the unused `server_name` binding or use it. When `use_tls=false`
   and host is not loopback → `tracing::warn!` once at startup.
4. `state.rs:172-176`: replace `.ok()` on `agent`/`identity` constructors with a match
   that logs the error at `error!` (still `None`).
5. `main.rs:11-21`: delete the unused `--kubeconfig`/`--talosconfig` flags (the chart
   does not pass them — verify with `rg -n "kubeconfig|talosconfig" charts/naslos/templates/api-deployment.yaml`).
   `parse_addr` (`main.rs:184-192`) returns `Result`; exit 1 on error; log the parsed
   `SocketAddr`.
6. Child-process environment: in `helm.rs:85`, `chartsrepo.rs:404`, `kube.rs:78`,
   `talos.rs` runner, and `server/ws.rs` kubectl spawns, call `.env_clear()` then pass
   an allowlist: `PATH`, `HOME`, `LANG`, `TZ`, `KUBECONFIG`, `TALOSCONFIG`,
   `HELM_*`, `XDG_*`, `GIT_*` (as already set by the code), `SSL_CERT_FILE`, `SSL_CERT_DIR`.
   Put the allowlist in one helper `crate::proc::scrubbed_env()`. Test: the helper drops
   `BUDDY_KEK` and `LDAP_BIND_PASS`.
7. Mutex poisoning: replace `lock().unwrap()` in `state.rs:100-106`, `metrics.rs:146-160,201`,
   and other shared-state locks with `lock().unwrap_or_else(|e| e.into_inner())`
   (no new dependency).

**Docs.** `docs/deployment.md` env table (LDAP file only), `docs/development.md`.

---

### W7 — Buddy correctness (no format change)  (`fix/buddy-correctness`)

1. **Record schedule results** (`server/buddy_schedules.rs:187-223` has no caller).
   In `server/buddy_send.rs` where the job reaches `JobState::Succeeded` (`:612`) or
   `Failed` (`:373` and other failure paths), if `schedule_id` is non-empty call
   `state.buddy_schedules.record_result(&schedule_id, &receiver, ok, &err)`. Make sure
   every terminal path (including panics → use a guard) records. Test: run a job with a
   fake agent that fails; `schedules_get` shows `lastResult=failed` and `lastError`.
2. **Atomic send-job de-dup** (`buddy_send.rs:329-334,762-816`, `buddy_schedules.rs:484-503`):
   move the `conflicting(receiver, source)` check inside `enqueue_job` under the same lock
   as `add`; make `enqueue_job` return `Result<JobPublic, JobPublic /*conflict*/>` and
   update both callers. Test: two concurrent enqueues → one conflict.
3. **Replay cache must not evict live nonces** (`buddy/auth.rs:285-293`): when a key has
   `MAX_NONCES_PER_KEY` unexpired nonces, reject the request with 429 instead of evicting.
   Serialize the append + compaction of the nonce file under one lock (`:151-190`).
   Tests for both.
4. **Receiver sequence monotonicity** (`buddy/store.rs:333-357`): for the **same** chain,
   reject a manifest whose `sequence <= current.sequence` with "stale manifest". This does
   not change the format. Test it.
5. **`PushOptions.save_state` non-optional** (`buddy/client.rs:126-130,724-729`): change
   the type to a required `&dyn Fn`; update `buddy_send.rs:575`, `bin/buddyctl.rs:580`, tests.
6. **Restore dataset validation** (`server/buddy_restore.rs:107-114`): validate `dataset`
   with the existing ZFS dataset-path validator before any work (reject `..`, empty
   components, leading `-`). Partial-apply staging is W13-D4.

**Docs.** `docs/buddy-backup.md`.

---

### W8 — Chart fixes  (`fix/chart-correctness`)

1. **Render bug** (`templates/api-deployment.yaml:384-443,446-518`): always render the
   `tmp` and `ldap-bind` volumes and their mounts; gate only optional ones. Add to
   `scripts/audit.sh` a `helm template` with every optional feature off
   (`--set api.sharesConfig.enabled=false --set api.talosConfigSecret= --set api.ldapCA.secretName= --set api.datasetsHostPath= --set agent.tls.enabled=false --set buddy.enabled=false`)
   and assert every `volumeMounts[].name` has a matching `volumes[].name` (small Python/PyYAML
   check, PyYAML is already an audit dependency).
2. **Installer profile tags** (`values-installer.yaml:28-64`): set every in-tree tag to the
   chart `appVersion`, and add to `scripts/check-release-version.sh` (and `audit.sh`) an
   assertion that all `values-installer.yaml` image tags equal `Chart.yaml` `appVersion`.
   Fix the header comment so it does not tell operators to `helm install` the in-tree file
   (point to the released pack).
3. `terminal.image.pullPolicy`: add `IfNotPresent` default in `values.yaml`.
4. `storage.localPath` (`values.yaml:492-500`): delete the dead block; document in
   `docs/deployment.md` that `local-path` is applied by `deploy-vm.sh`/the installer.
   Verify nothing in `scripts/` reads those values first (`rg -n "localPath" scripts Makefile`).
5. **Seccomp/escalation for network daemons** — `samba-daemonset.yaml`, `nfs-daemonset.yaml`,
   `openldap-statefulset.yaml`: add `seccompProfile: {type: RuntimeDefault}` and
   `allowPrivilegeEscalation: false`. Do **not** change users/capabilities in this PR.
   **Live drill required** (owner): SMB mount + write, NFS mount + write, LDAP login.
   Put this item in its own commit so it can be reverted alone.

**Docs.** `docs/deployment.md`, `docs/shares.md` (security context note).

---

### W9 — UI fixes  (`fix/ui-review`)

1. **Unsafe exposure** (`ui/src/lib/components/AppInstallModal.svelte:69-77,193-221`).
   On the confirm step, when `exposure.auth === false` or `exposure.tls === false`
   and a subdomain is set, show a red block explaining the consequence and require a
   second checkbox ("I understand this app will be reachable without Naslos login /
   without TLS") before Install enables. When both are false and `localOnly` is false,
   preselect `localOnly = true`. Same warning in the Exposure edit dialog of installed apps.
   (Catalog defaults in `NaslosCharts` are a separate repo; open an issue there, not a PR,
   suggesting `auth: true` for *arr/qBittorrent — list the files.)
2. **Mask secret values** (`AppInstallModal.svelte:134,197`): values whose schema field has
   `format: password` render as `••••••` in the review list; Configure form fields of that
   format use `type="password"` and show "unchanged" when the API returns them in
   `redactedKeys` (W4.4).
3. **App jobs** (`ui/src/lib/stores/appJobs.ts:76-80,100-121`, `AppJobsDrawer.svelte:64`):
   disable Dismiss while `state === 'running'`; on 404/410 from a poll, untrack the id
   and trigger an app-list refresh.
4. **Error states** (`routes/shares/+page.svelte:34-39`, `routes/users/+page.svelte:24-31`):
   check `res.ok`; show the existing error-banner pattern used by `InstalledApps`.
   `CertificateStatus.svelte:11-31`: check `res.ok`; retry with backoff (5 s → 60 s cap)
   instead of `stop()` after one failure.
5. **Terminal sockets** (`routes/terminal/+page.svelte:141-205`): guard `connect()` when a
   socket is open/connecting; capture the socket in a local and ignore handlers where
   `ws !== local`; close the old socket before reassigning. Show the server close reason
   (W5.1 "session expired"/"idle timeout").
6. **Version** (`Sidebar.svelte:122`): inject `package.json` version via Vite
   `define: { __APP_VERSION__: JSON.stringify(pkg.version) }` + a `declare const` in
   `src/app.d.ts`.
7. **Playwright**: extend `tests/` with (a) exposure warning requires the extra checkbox,
   (b) password field masked in review. Run against the VM only if the owner has
   `ui/.env.playwright.local` set up; otherwise state "not run" in the PR.

---

### W10 — Agent hardening  (`fix/agent-hardening`)

1. **Destroy confirmation** (`rust/agent/src/zfs/operations.rs:275-287`,
   `server/handlers.rs:130-144`; API `rust/api/src/server/zfs.rs:381-399`; UI pool destroy).
   Agent `DELETE /api/v1/pools/{pool}` requires query `confirm=<pool name>`; `-f` only when
   `force=true`. API forwards the same; UI already asks the user to type the name — pass it.
   Same for `zfs receive?force=true` (`backup.rs:166-178`): only Buddy restore with an
   explicit user "overwrite" checkbox may set it (check `buddy_restore.rs` and UI).
   Agent + API + UI ship in the **same chart release** (they are one deployable).
2. **`create_pool` atomicity** (`operations.rs:164-271`): wipe all disks first collecting
   results; if any wipe fails, return the list of wiped/unwiped disks. If `zpool create`
   exits non-zero, return the error even when `zpool list` then shows the pool, **and** say
   the pool exists. Post-create `zfs set` failures return `{created:true, failedStep, error}`
   (HTTP 207-like: use 500 with a structured JSON body `{error, poolCreated:true, step}`);
   update the API and UI to show "pool created but X failed". Document in `docs/api.md`.
3. **Plaintext opt-in** (`rust/agent/src/main.rs:73-91`): both TLS vars empty → exit 1
   unless `AGENT_ALLOW_PLAINTEXT=1`. Verify `values-vm.yaml` and `values-installer.yaml`
   keep `agent.tls.enabled: true`; the chart sets `AGENT_ALLOW_PLAINTEXT=1` only when
   `agent.tls.enabled=false`. (Closes the code half of F15; record in the audit doc.)
4. **Stream limits** (`server/mod.rs:116-132`, `handlers.rs:582-654`): `Arc<Semaphore>` with
   4 permits shared by send/receive (env `AGENT_MAX_STREAMS`); 503 when exhausted. In
   `zfs_receive`, wrap each `stream.next()` in `tokio::time::timeout(idle)` (300 s, env
   `AGENT_STREAM_IDLE_TIMEOUT_SECS`); on expiry shut stdin and abort the child.
5. **Import body** (`handlers.rs:94-115`): a body that fails to decode → 400. Import-all
   (empty name) requires `{"name":"", "all": true}`; update the API caller.
6. **Option trimming** (`zfs/dataset.rs:125-131,285-292`, `operations.rs:219-236`): validate
   and emit the same trimmed value; reject empty `quota`.
7. **Stderr** (`server/error.rs:26-32`): log full host output with a random 8-hex request id;
   return `"<stable message> (ref <id>)"` for 500s. Validation 400s unchanged.

**Tests** for each item in `rust/agent/tests/`. Docs: `docs/api.md`, `docs/storage-zfs.md`,
`docs/AUDIT-2026-10-24.md` (mark F15 code half closed).

---

### W11 — CI/release gates  (`ci/release-gates`)

1. `.github/workflows/release.yml:274-285` "Require green CI": use
   `gh api --paginate "repos/$GITHUB_REPOSITORY/commits/$GITHUB_SHA/check-runs" --jq '.check_runs[]'`
   piped to `jq -s`, keep the **latest** run per name, and require exactly the three names
   `audit`, `supply-chain`, `gitleaks` all `success`. Fail on missing names. Add a
   `scripts/` unit-style test (bash + fixture JSON) runnable in `audit.sh`.
2. `release.yml:190-191`: pin `pyyaml==6.0.2` (or the version already used in `ci.yml`).
3. `release.yml:137-146`: on `publish=true`, before pushing `:X.Y.Z`, run
   `docker buildx imagetools inspect ghcr.io/...:X.Y.Z`; if it exists and the release already
   has a pack attached, fail with "re-run would move :X.Y.Z away from the attached pack".
4. `release.yml:231-240` anonymous-pull check: run with `DOCKER_CONFIG=$(mktemp -d)`; drop
   `|| true` on logout.
5. `scripts/check-release-version.sh`: add the `values-installer.yaml` tag check (W8.2).

---

### W12 — Test coverage  (`test/contract-and-argv`)

1. `rust/api/tests/contract.rs`: table-driven over the full route list (extract from
   `server/mod.rs`): for each route/method assert status for (no secret, wrong secret,
   forged user without secret, plain user, admin) — exact codes, not `!=`. Add WS
   handshake tests for `/api/ws/exec` (401 without secret, 403 foreign Origin).
   Fix `same_origin_state_change_is_allowed` to assert the exact success code.
2. `rust/agent/tests/common/mod.rs`: `FakeRunner` records `Vec<String>` argv; add helpers
   `assert_argv(calls, &["zpool","create",...])`; keep the old substring helper only where
   needed. Key canned responses by argv prefix instead of call index. Implement
   `spawn_out`/`spawn_in` with `tokio::io::duplex` so send/receive paths are testable.
3. HTTP tests for `DELETE /api/v1/pools/{pool}`, `POST /api/v1/datasets/{pool}`,
   share folder create/delete traversal, oversized body.

---

### W13 — DESIGN-NEEDED (ask the owner before coding; write a short design note in this file)

| ID | Item | Evidence | Options |
|---|---|---|---|
| D1 | Agent `hostNetwork` :9090 reachable from LAN (AUDIT-M4) | `agent-daemonset.yaml:29`; `networkpolicy.yaml:70-77` | Cilium host firewall on by default; or drop hostNetwork; or mTLS |
| D2 | Release namespace PSA `privileged` (API hostPath) | `namespace.yaml:12-17` | static PV/PVC for `/var/mnt` view; helper in privileged ns |
| D3 | API cluster-wide PV create/delete | `app-isolation.yaml:84-95,123-137` | pre-provision PVs; agent creates PVs |
| D4 | Buddy restore staging (partial apply) | `buddy_restore.rs:190-235` | receive to temp dataset + rename |
| D5 | Chart credentials in URL | `chartsrepo.rs:450-451,607-613` | `http.extraHeader` / credential helper when creds are wired |
| D6 | Keyed per-chunk digest (manifest v2) | `buddy/envelope.rs:35-36,182` | HMAC with DEK-derived key; version bump; read v1 forever; interop tests |
| D7 | App secret values stored in `apps.json` | `apps.rs:101-112` | per-app Kubernetes Secret |
| D8 | Buddy receiver authentication + TLS | `buddy_restore.rs:32-41`, `client.rs:390-413` | require https for non-loopback; pin receiver key at enrolment |
| D9 | DDNS generic driver DNS rebinding | `ddns/drivers.rs:830-840,961-980` | `ClientBuilder::resolve` pin; ban hostnames |
| D10 | Legacy shared `naslos-datasets` volume + VM `allowBuddyStore` | `values-vm.yaml:99-103,183-185` | move store outside `/var/mnt` |
| D11 | Long-lived `naslos-helm` token | `rbac-helm.yaml:20-31` | TokenRequest per job |
| D12 | Authelia app-wildcard rules one_factor for admins | `authelia.rs:39-53` | intended? document or two_factor |

---

## 3. Operational follow-up (owner, not the executor)

- The installer previously put the admin password and proxy secret into `pods/exec` argv.
  Talos's default audit policy (`level: Metadata`) records the request URI, so every node
  installed with ≤ 0.2.2 has them in `/var/log/audit/kube/kube-apiserver.log` (30 days).
  After the installer fix (companion plan I1) ships, document in `docs/operations.md`:
  rotate the admin password and the proxy secret (`naslos-proxy-secret`) on existing nodes.
  The executor writes the doc section; the owner performs the rotation.

## 4. Verification checklist per PR (copy into the PR body)

- [ ] Gates in §0.5 green (paste counts).
- [ ] New tests fail on `origin/master` and pass on the branch (say how you checked).
- [ ] Docs updated (list files) / spec updated or "no MUST changed".
- [ ] `CREDITS.md` unchanged or updated with `Last reviewed`.
- [ ] Live drill: "required, not run — owner" or the drill result.

## 5. Do NOT fix (proven false / already fixed / out of scope)

- Installer `UpsertSecret` "422 without resourceVersion": **false** — Secrets allow
  unconditional update (`AllowUnconditionalUpdate() == true`).
- "Namespace race crosses the privileged boundary": false — pools are separate. (The
  same-pool race is real: W4.1.)
- Installer version-parity guard: already on installer `main` (PR #13).
- Do not change NaslosCharts from this plan (separate repo; open issues only).

## 6. Release (owner decides; executor prepares)

1. After W1–W5 merge (and whichever of W6–W12 are ready), the executor opens
   `chore(release): prepare 0.2.3` bumping `charts/naslos/Chart.yaml` version/appVersion,
   `ui/package.json`, `values-installer.yaml` tags (W8.2), and updates `AI_Handoff.md`.
2. Owner runs Actions → `release` with `publish=false`, then tags `v0.2.3`.
3. Only then does the installer companion plan release `naslos-v0.2.3` (version parity).
   The installer's exec/admin change (I1) depends on the terminal image providing
   `curl` ≥ 7.76 (`--fail-with-body`) — it does (alpine 3.24, `terminal/image/Dockerfile:21,36`).
