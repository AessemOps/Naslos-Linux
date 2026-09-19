# Naslos Security Audit — 2026-09-14

> **SUPERSEDED (2026-09-19).** This audit predates the fix batch and the
> authenticated-only change; several findings below are already closed (agent
> token enforcement, the NodePort/auth bypass removal, the streaming-client
> token, CR-01). Read it for history, not for current risk. The current picture
> is in `docs/AUDIT-2026-09-19.md`, with its remediation in
> `docs/AUDIT-2026-09-19-FIXPLAN.md`; the living per-finding list is
> `docs/CODE-REVIEW.md`. The NAS-* IDs here are kept for traceability.
>
> **Status of the headline findings at 2026-09-19:**
>
> | Finding | Status |
> |---|---|
> | NAS-001 API trusts whoever can reach it | **Fixed** — the owner gate is wired to the router; `TestOwnerRoutesRequireAuth` covers it |
> | NAS-002 unauthenticated agent | **Fixed** — bearer token required and compared in constant time; `api/internal/agent/client_test.go` pins both clients (it found the streaming-client gap) |
> | NAS-003 agent validation gaps | **Partial** — escaping/validation batches landed; CR-27/28/29 remain open |
> | NAS-004 terminal unauthenticated, NAS-005 `/api/ws/logs` unauthenticated | **Fixed** — owner gate; the nginx 403 path is gone with the NodePort |
> | NAS-008 NodePort exposes the API | **Fixed** — the listener and the whole dev posture were removed |
> | NAS-009 `X-Forwarded-Host` trust | **Fixed** — `trustForwardHeader` removed from the forwardAuth middleware |
> | NAS-010 default credentials | **Mostly fixed** — Grafana removed; the LDAP service password rotated and de-committed (AUDIT-H1); `generate-secrets.sh` now generates random admin/service passwords instead of `naslos-admin`/`CHANGE_ME_*`; the OpenLDAP entrypoint no longer defaults `LDAP_ADMIN_PASSWORD` to `admin`; new users get an unguessable random placeholder instead of `TempPass123!` and the API rejects a passwordless create (400); `deploy-vm.sh` requires `REGISTRY_HTTP_SECRET`. **Residual:** the *running* instance's LDAP admin password is still `naslos-admin` (it was initialised with it) — rotating it means modifying `olcRootPW` in the `cn=config` database, so it is a deliberate, separate operation |
> | NAS-014 jwt regenerated per render, NAS-017 agent client token | **Fixed** — jwt secret persisted; streaming client carries the token |
> | everything else | historical; see the status table in `docs/CODE-REVIEW.md` |

- **Branch:** `feature/buddy-backup`
- **Commit:** `11b9dffa65cefc27cf95f6b6d9c4f9cc64d8df2f`
- **Scope:** Full codebase — `api/`, `agent/`, `api/internal/buddy/`, `ui/`, `charts/naslos/`, `samba/`, `nfs/`, `openldap/`, `terminal/`, `bootstrap/`, plus Go deps (`api/go.mod`) and UI deps (`ui/package.json`).
- **Methodology:** Static code review + Kubernetes manifest/RBAC review + secrets grep + `go vet` + `govulncheck` + `npm audit` (prod + full). No live exploitation, no destructive cluster actions, no credential rotation.
- **What was NOT tested:** Live dynamic probing (401/403 curl checks against the VM), fuzzing, TLS/cert validation on a live deploy, backup-restore integrity beyond code paths, UI XSS/CSRF in a browser.

## Executive summary

| Severity | Count |
| --- | --- |
| Critical | 3 |
| High | 5 |
| Medium | 10 |
| Low / Info | 4 |
| **Total** | **22** |

**Top 3 risks in plain language:**

1. **The API trusts whoever can reach it.** The hardened `RequireAuth` middleware (CIDR + `Remote-User` check) exists but is never wired to any route (`api/internal/server/server.go:183-258`). Everything except a handful of terminal/buddy owner endpoints is unauthenticated at the API layer. Security rests entirely on reaching the API through Traefik + Authelia — but the UI NodePort (`ui-nodeport.yaml`, `ui/nginx.conf:76-90`) proxies almost all `/api/` paths straight to the API, where a client-supplied `Remote-User` header is believed.
2. **The agent is unauthenticated privileged execution-as-a-service.** `agent/internal/server/server.go:44-61` has no token/mTLS/auth; it listens on `:9090` (`agent/cmd/main.go:27`) inside a `hostNetwork + hostPID + privileged` DaemonSet with `hostPath /,/dev,/run,/var`. Any in-cluster caller (or node-adjacent network) can destroy pools, wipe disks via `CreatePool`, overwrite datasets via `zfs receive -F`, and write root-owned share configs. Several destructive handlers additionally skip input validation.
3. **Terminal exec + weak boundaries amplify the above.** The API holds `pods/exec create` (`charts/naslos/templates/terminal.yaml:89-113`) and the terminal container is `privileged + hostPath /,/var/mnt,/dev`. Meanwhile `/api/ws/logs` has no auth at all, the WS origin check trusts client-controlled `X-Forwarded-Host`, and LDAP/share config renderers lack injection escaping — so one bypass tends to become cluster/host compromise.

> No secrets are pasted in this report. Key names and file:line references are given; rotate values out-of-band.

## Findings (ordered Critical → Low)

### NAS-001 — API `RequireAuth`/`RequireAdmin` dead code; most mutating routes unauthenticated at the API layer — **Critical**

- **Files:** `api/internal/auth/middleware.go:58-101`, `api/internal/server/server.go:183-258` (all `HandleFunc`/`Handle` bare, `s.auth` constructed `server.go:104-109` but never used).
- **Description:** `RequireAuth` correctly checks `RemoteAddr ∈ TRAEFIK_CIDR` + non-empty `Remote-User`, and `RequireAdmin` checks `naslos_admins`. Neither wraps any route. Only `handlePods`/`handleNamespaces`/`handleExecWS` (via `pods.go:70-84` `requireTerminalAuth`) and buddy owner endpoints (`buddy.go:150-161`, `buddy_send.go:66-68,307`, `buddy_jobs.go:240-246,585,598`, `buddy_schedules.go:332-335`) perform any header check — and those checks do **not** verify source CIDR. Everything else (`/api/catalog`, `/api/apps*`, `/api/disks*`, `/api/volumes*`, `/api/datasets`, all `/api/shares*`, `/api/notifications*`, `/api/metrics`, `/api/dashboard`, `/api/users*`, `/api/groups*`, `/api/buddy/status`, `/api/auth/me`) has no auth at registration and no in-handler auth.
- **Exploit precondition:** Direct reachability to `naslos-api:8080` (ClusterIP from any pod, or NodePort via UI nginx `/api/` proxy). No credentials needed.
- **Recommendation:** Wire `s.auth.RequireAuth` (and `RequireAdmin` where appropriate) into `routes()` for every owner-facing endpoint; keep only `/api/health`, `/api/ready`, and peer-key-authenticated `/api/buddy/v1/*` outside it. Alternatively enforce auth in each handler + add a test asserting no new bare route can be added (e.g. a route-table unit test). Keep the terminal/buddy header checks as defense-in-depth, but add the CIDR check there too.

### NAS-002 — Agent has zero authentication; plain HTTP privileged API on every node — **Critical**

- **Files:** `agent/internal/server/server.go:44-61` (routes), `:88-93` (`ListenAndServe`, no TLS), `agent/cmd/main.go:27` (`:9090` all interfaces), `api/internal/agent/client.go:21,68-104` (plain `http://naslos-agent.<ns>.svc:9090`, no auth header), `charts/naslos/templates/agent-daemonset.yaml:20-21,27,35-61,69-84`.
- **Description:** No bearer/basic/token/mTLS/middleware; grep for auth in `agent/` returns nothing. Headless Service + `hostNetwork:true` exposes it as node-IP `:9090` and cluster DNS. Combined with privileged + host mounts (see NAS-015), any compromised pod or node-adjacent caller gets root ZFS + host file writes on all nodes.
- **Exploit precondition:** Network path to `naslos-agent:9090` (any pod in cluster; node network if hostNetwork reachable).
- **Recommendation:** Add agent auth (shared bearer token from a Secret, or mTLS via cert-manager), bind to PodIP/ClusterIP only (drop `hostNetwork` if possible), and add a `NetworkPolicy` allowing only `naslos-api` SA → agent `:9090`. Log + alert on auth failures.

### NAS-003 — Agent validation gaps: arbitrary disk wipe, pool destroy, unvalidated snapshot/dataset targets — **Critical**

- **Files:** `agent/internal/zfs/operations.go:101-153,166-200` (`CreatePool`: name only `!= ""`, disks only prefix-normalized, cache same, options merged with no allowlist), `:209-213` (`DestroyPool` only `!= ""`, no `ValidatePoolName`), `:351-352,428` (`PoolHealth/Status`: zero validation), `:326-333` (`ImportPool`: no validation), `agent/internal/zfs/dataset.go:39-40` (`Datasets(pool)` unchecked), `:239-240` (`Snapshots`), `:229-231` (`Snapshot(ds,snap)`: neither side validated — contrast `backup.go:60` which validates both), `agent/internal/server/server.go:165-190,253-329`.
- **Description:** `exec` is `exec.CommandContext("chroot",...)` (no shell, so no shell-split — but `-`-prefixed names become flag injection, and destructive targets need no flags). `POST /api/v1/pools` can pass arbitrary `/dev/*` (e.g. OS disk) to `wipefs --all` + `zpool create -f`, arbitrary `zfs set k=v` keys, and `-`-prefixed pool names. `DELETE /api/v1/pools/{pool}` → `zpool destroy -f <pool>` with no name check. Well-validated endpoints (`AddVDev` at `devices.go:179-222`, dataset create/delete at `dataset.go:150-156,204-207`, send/receive at `backup.go:61-62,99-101,143,177`) prove the validators exist — they are just not applied on the paths above.
- **Exploit precondition:** Same as NAS-002 (unauthenticated agent call). No special input needed beyond a valid-looking JSON body.
- **Recommendation:** Apply `ValidatePoolName` to every pool-name sink (`CreatePool`, `DestroyPool`, `PoolHealth/Status`, `ImportPool`), `normalizeDiskPath` + pool-membership check to `CreatePool` disks/cache (reuse `AddVDev` logic), `validateDatasetPath` + `ValidateSnapshotName` to `Snapshot/Snapshots/Datasets`, and `ValidateDatasetOptions` allowlist to `CreatePool` options. Add negative unit tests (leading `-`, `..`, non-`/dev`, unknown disk, oversized name).

### NAS-004 — Terminal exec is cluster RCE gated only by edge proxy; API holds `pods/exec create` — **High**

- **Files:** `api/internal/server/websocket.go:315-449` (`handleExecWS`, shell allowlist `bash|sh|ash|zsh` at `:98` — good), `api/internal/server/pods.go:70-84` (header-present check only, `TERMINAL_REQUIRE_AUTH=false` bypass at `:71-73`), `charts/naslos/templates/terminal.yaml:54-56` (`privileged:true, runAsUser:0, hostPath /,/var/mnt,/dev`), `:89-113` (`Role pods,pods/log get/list/watch + pods/exec create`), `ui/nginx.conf:63-74` (NodePort 403 for 3 paths — good but narrow, see NAS-008), `charts/naslos/templates/traefik-middleware.yaml:10-17`, `terminal.yaml:148-165`.
- **Description:** Shell allowlist, `validateKubeName:219`, and exact container resolution `:192` are good. But authorization is "any non-empty header" — trustworthy only if Traefik `forwardAuth` replaced the value. Direct-to-API callers supply their own. Success yields a root shell in a privileged host-mounted container + Kubernetes exec into any pod the API SA can reach.
- **Exploit precondition:** Direct API reachability (same as NAS-001) + knowledge of a namespace/pod (listable via NAS-005-adjacent endpoints once authed, or guessable `naslos-terminal`).
- **Recommendation:** Same fix as NAS-001 (CIDR-checked middleware on terminal paths), remove the `TERMINAL_REQUIRE_AUTH=false` escape hatch or gate it behind an explicit allowlisted source range, scope the `Role` to the terminal namespace/pod only if possible, and consider dropping `privileged`/`hostPath /` if the shell does not strictly need host root.

### NAS-005 — `/api/ws/logs` streams any pod's logs with no auth — **High**

- **Files:** `api/internal/server/server.go:208`, `api/internal/server/websocket.go:240-306` (no `requireTerminalAuth`; validates pod/namespace then `upgrader.Upgrade :267-271`).
- **Description:** Sibling endpoints `handlePods` (`pods.go:134-136`), `handleNamespaces` (`pods.go:100-102`), and `handleExecWS` (`websocket.go:318-320`) all gate; `handleLogsWS` does not. Pod logs commonly contain secrets, tokens, stack traces, and user data.
- **Exploit precondition:** Direct API reachability; needs namespace/pod names (often guessable: `naslos-api`, `naslos-openldap`, etc.).
- **Recommendation:** Add the same `requireTerminalAuth` gate as `handleExecWS` (first statement, before validation/upgrade). Add a regression test asserting 401 without the header.

### NAS-006 — LDAP filter + DN injection (no `EscapeFilter`/`EscapeDN` anywhere) — **High**

- **Files:** `api/internal/identity/persons.go:13,41,46` (`uid=%s` DN + `(uid=%s)` filter), `persons_extra.go:16,41,72-73,130,142`, `groups.go:13-31,54,124-125,136-137,148` (`cn=%s` DN + `(cn=%s)` filter). Grep for `EscapeFilter|EscapeDN` in `api/internal/identity` returns zero hits; `normalizeUID:284-286` is only `ToLower+TrimSpace`.
- **Description:** `* ( ) \ NUL` alter search filters (info disclosure / auth-confusion depending on caller); `, + " \ < > ; =` break DNs (`CreateGroup` takes raw `cn`). Inputs originate from API users/groups endpoints (NAS-001: unauthenticated).
- **Exploit precondition:** Ability to call `/api/users` or `/api/groups*` with crafted `uid`/`cn`.
- **Recommendation:** Use `ldap.EscapeFilter` for every filter interpolation and validate/escape DN components (`ldap.EscapeDN` or strict allowlist `^[a-z0-9][a-z0-9._-]{0,63}$`). Add unit tests with `*)(uid=*`, `,ou=admin`, NUL payloads.

### NAS-007 — `smb.conf` / Ganesha config injection via unescaped share fields — **High**

- **Files:** `api/internal/shares/config.go:54-56` (`[%s]`, `path = %s`, `comment = %s` unescaped), `:71,76` (`hosts allow`, `valid users`), `:149,151,174` (Ganesha `Path`, `Pseudo`, `Clients`), `:197-201` (FNV export-ID, collision possible), `manager.go:311-325` (`validateShareName` blocks ` / \ [ ] " ' : * ? < > = + ; ,` but **not** `\n \r \0 \t`), `:330-345` (`normalizeList` trim/dedupe only; `Description/AllowedHosts/ValidUsers/ValidGroups` otherwise unvalidated), `:217-256` (`accessList` strips `@` but not `\n\r;`), `smbusers.go:152-194,238-239,343,360,395-396` (`UID`/`ntHash`/gecos/group/members uninterpolated; `Upsert:153` trims only; hash only upper-cased at `:157-159`, no `^[0-9A-F]{32}$` check).
- **Description:** Example: `Description: "x\nvalid users = alice"` or `AllowedHosts: ["x\n[evil]"]` renders as new directives; `;`, `"`, `\n` in Ganesha fields break out of `EXPORT/CLIENT` blocks. `Path` preserves `\n` when still under base. Corrupted `passwd/smbpasswd/group` lines via `:`/`\n` in UID. Combined with NAS-001 (unauthenticated shares endpoints), any direct API caller can inject Samba/Ganesha directives that the privileged daemons then consume (testparm gates Samba at `samba/image/entrypoint.sh:196-201,235-242` — good — but Ganesha has no pre-reload gate at `nfs/image/entrypoint.sh:79-115`).
- **Exploit precondition:** Direct API reachability + `POST /api/shares` (or LDAP uid with `:`/`\n` for the passdb variant).
- **Recommendation:** Reject `[\r\n\0]` (and `;`/`"` where grammatically meaningful) in `Name/Path/Description/AllowedHosts/ValidUsers/ValidGroups/UID/ntHash` before `GenerateSambaConfig/GenerateGaneshaConfig/Render*`; enforce `ntHash ^[0-9A-F]{32}$`; quote/escape Ganesha strings; add a Ganesha config syntax gate before reload; add unit tests with newline/semicolon payloads.

### NAS-008 — NodePort UI exposes unauthenticated API surface (only 3 terminal paths blocked) — **High**

- **Files:** `charts/naslos/templates/ui-nodeport.yaml:1-21`, `charts/naslos/values-vm.yaml:42-44` (`:30080`), `ui/nginx.conf:63-74` (403 for `= /api/ws/exec`, `= /api/pods`, `= /api/namespaces` — correct and narrow), `:76-90` (`location /api/` proxies everything else to `naslos-api:8080` with client headers preserved), `api/internal/server/users_extra.go:234` (`handleAuthMe` trusts `Remote-User` with no CIDR check), `pods.go:70-84` / `buddy.go:150-161` (header-present checks, see NAS-001).
- **Description:** The 3 terminal paths are correctly refused on this listener, but **all other** `/api/` paths (users/groups/shares/volumes/apps/buddy owner endpoints trusting any non-empty header) are proxied with attacker-controlled headers intact. Authelia `two_factor` rules (`authelia-config.yaml:117-139`) and Traefik `forwardAuth` (`traefik-middleware.yaml:10-17`, `terminal.yaml:148-165`) only apply on the Traefik route — not here.
- **Exploit precondition:** Network path to node `:30080` (documented dev/VM access path).
- **Recommendation:** Either remove the NodePort in any non-dev values, or enforce API-side auth (NAS-001) so security does not depend on ingress path. At minimum extend the nginx deny to all mutating/identity endpoints if the NodePort must remain, and document that `:30080` is unauthenticated-by-design for dev only.

### NAS-009 — WebSocket origin check trusts spoofable `X-Forwarded-Host`, allows empty `Origin` — **Medium**

- **Files:** `api/internal/server/websocket.go:29-55` (`CheckOrigin`: `"" → true :32-35`; else hostname match against `r.Host` **or** `X-Forwarded-Host :42-45`), `:58-66` (`sameHostname` strips ports by design `:25-28`).
- **Description:** `X-Forwarded-Host` is client-controlled unless an edge proxy overwrites it; accepting it as an origin oracle weakens CSRF protection for the WS endpoints. Empty-`Origin` accept is needed for CLI/Playwright but also admits non-browser WS clients unconditionally (acceptable only because auth — NAS-005 — must be fixed first).
- **Exploit precondition:** Victim browser + attacker site; requires an authenticated WS session to be useful (exec is gated, logs currently is not — NAS-005).
- **Recommendation:** Only trust `X-Forwarded-Host` when the TCP peer is the trusted proxy (reuse `isTrusted`), or drop it from the allow-list and compare against a configured public hostname. Keep empty-`Origin` allow only for non-browser `User-Agent`s if possible; document the Playwright exception.

### NAS-010 — Default / hardcoded credentials ship working — **Medium**

- **Files:** `charts/naslos/values.yaml:256` + `values-vm.yaml:82` (`grafana.adminPassword: naslos-admin`), `charts/naslos/values.yaml:296` + `values-vm.yaml:67` (`openldap.bindPassword: CHANGE_ME_SERVICE_PASSWORD`, rendered at `authelia-config.yaml:91`), `openldap/generate-secrets.sh:13-14` (`ADMIN_PASSWORD:-naslos-admin`, `SERVICE_PASSWORD:-CHANGE_ME_SERVICE_PASSWORD`), `openldap/image/entrypoint.sh:12` (`LDAP_ADMIN_PASSWORD:-admin`), `api/internal/identity/persons.go:31` (`userPassword: TempPass123!` for every new user), `scripts/deploy-vm.sh:11,23` (`REGISTRY_HTTP_SECRET:-secret`).
- **Description:** Fresh installs work with public defaults; `TempPass123!` is predictable per-user initial password; docs (`docs/deployment.md:200-201,247`, `docs/monitoring.md:48`) already warn to rotate but nothing enforces it.
- **Exploit precondition:** Fresh deployment where operator skipped rotation; new-user password known until changed.
- **Recommendation:** Fail closed on placeholder bind passwords (refuse to start or generate random + print once), force password change on first login (or generate per-user random initial password), require `grafana.adminPassword` override, and remove `:-default` fallbacks for admin/service secrets.

### NAS-011 — Buddy enrollment single-use is restart-replayable; token enumerable; `EnrollOpen` name-hijack — **Medium**

- **Files:** `api/internal/buddy/http.go:44-51,215-281` (`EnrollToken`, `EnrollOpen=false` default, `mu+enrollUsed`; `ConstantTimeCompare :235` — good; errors distinguish `invalid token` vs `already used`), `auth.go:82-87,150-167` (in-memory only), `peers.go:166-189` (`Add` rejects same-fingerprint/different-name but silently overwrites same-name/different-fingerprint), `api/cmd/buddy-receiver/main.go:50`, `api/internal/server/buddy.go:81-123` (manual peer-add path does not validate `AllowedSources` though enroll does), `http.go:200` (`Status.EnrollOpen` reports `EnrollToken!=""`, not the flag).
- **Description:** `enrollUsed` and the nonce `seen` map are in-memory → restart re-arms a "single-use" token indefinitely. No rate-limit on token guessing; distinct errors aid enumeration. In `EnrollOpen` mode a second enrollee can hijack an existing peer name. Status endpoint cannot distinguish single vs multi-use.
- **Exploit precondition:** Network path to `/api/buddy/v1/enroll` + knowledge/timing of token lifecycle (or restart window).
- **Recommendation:** Persist `enrollUsed` (store dir, `0600`), add rate-limit + uniform error strings, reject same-name/different-key (or require explicit replace), validate `AllowedSources` on manual peer-add, and fix `Status.EnrollOpen` to report the flag.

### NAS-012 — Buddy manifest has no rollback protection; sender-controlled timestamps select survivors — **Medium**

- **Files:** `api/internal/buddy/store.go:335-359` (`PutManifest` overwrites `chains/<id>/manifest.json` + `current.json` unconditionally), `:624` (`Prune` keeps newest by `CreatedAt`), `api/internal/buddy/envelope.go:212-261` (`Sign/VerifySignature` — good, but no monotonicity), `http.go:511-547` (receiver checks `Version/Source/Chain/Chunks-present` but not `ChunkPlainSize/StreamPrefix/CreatedAt/Kind/GUIDs/DEKWrapped/Sha256Plain/duplicates/SealedBytes`).
- **Description:** A compromised (or stale) sender key can re-publish an older manifest and move `current` backwards; prune survivorship depends on sender-stated `CreatedAt`. Receiver cannot decrypt (zero-knowledge by design) so it cannot detect content rollback — it must enforce pointer monotonicity itself.
- **Exploit precondition:** Valid peer key (or stolen key) for the source.
- **Recommendation:** Enforce monotonic `current` (refuse older `CreatedAt`/shorter chain unless explicit `force` + audit log), validate manifest field formats/sizes/duplicates server-side, and log manifest rotations.

### NAS-013 — Buddy quota: check-then-act race, resume false-413, manifest undercount, unlimited-by-default — **Medium**

- **Files:** `api/internal/buddy/http.go:388-399` (pre-check `Usage+len(body)>quota → 413` before `PutChunk`), `store.go:177-194` (idempotent re-upload returns nil on digest match — but pre-check already charged it), `:501-548` (cached `dirSize` + `addUsage`; manifest bytes counted in `dirSize` but never via `addUsage`), `:635-637`, `peers.go` (`QuotaBytes<=0` = unlimited, no max-cap validation).
- **Description:** Concurrent PUTs overshoot quota (no lock between check and write); resuming a full chain fails even when no new bytes are needed; quota cache undercounts after manifest publish (small bypass); negative/zero quota means unlimited with no guardrail.
- **Exploit precondition:** Valid peer key; concurrency or a full receiver.
- **Recommendation:** Move quota check inside the store lock (or serialize per-key writes), exempt digest-equal re-uploads from the pre-check, account manifest bytes in `addUsage`, and validate `QuotaBytes` (`>0` required, upper bound sane default).

### NAS-014 — Buddy nonce replay cache is in-memory only; unbounded per-key growth; 32-bit job IDs enumerable — **Medium**

- **Files:** `api/internal/buddy/auth.go:27,31,82-87,105,109-119,140-167` (`MaxClockSkew=5m`, `nonceTTL=10m`, burn-after-verify `:140-144` — correct — but `seen` in-memory, `O(n)` sweep `:154-159`, no length/format check beyond non-empty `:105`, no per-key cap), `api/internal/server/buddy_jobs.go:120-126` (`randomJobID` 4-byte hex = 32-bit, listable via `GET /jobs`; any admin can cancel any job `:597-628`), `buddy_send.go:408`, `buddy_jobs.go:398` (snapshot suffix 16-bit entropy; collision fails safe).
- **Description:** Restart wipes `seen` → a captured request within the 5-minute window replays successfully. A valid key can bloat the map until TTL. Job IDs are enumerable (low impact given admin-gating, but no per-job ACL/audit beyond `log.Printf`).
- **Exploit precondition:** Receiver restart window (replay); valid peer key (map bloat); admin header (job cancel).
- **Recommendation:** Persist recent nonces (or persist a restart counter into the signed payload), cap `seen` per key + total with LRU eviction, use 128-bit job IDs, and add per-job ownership/audit logging.

### NAS-015 — Over-broad RBAC + `privileged` namespace + host-reaching workloads — **Medium**

- **Files:** `charts/naslos/templates/namespace.yaml:8` (`pod-security enforce: privileged` whole namespace), `agent-daemonset.yaml:20-21` (`hostPID/hostNetwork`), `:26-27` (`privileged: true` via `values.yaml:102`), `:33-61` (`hostPath /,/dev,/run,/var`, `HostToContainer`), `:93-112` (`ClusterRole get,list,watch nodes,pods,pods/log` + binding), `samba-daemonset.yaml:27,29,37-46,104-115` (`hostNetwork`, `runAsUser:0` + 6 caps, `hostPath shares/extrausers//var/mnt`), `nfs-daemonset.yaml:26-27,37-47,88-95` (same + `DAC_READ_SEARCH`), `terminal.yaml:54-56,72-83,89-137` (`privileged`, `hostPath /,/var/mnt,/dev`, `Role pods/exec create` — namespaced, good — + `ClusterRole namespaces get/list`), `api-deployment.yaml:207-220` (`hostPath /var/mnt` RO + `buddy-data` RW; `fsGroup` does not apply to `hostPath` — hence the `chown 65532` requirement documented in `docs/buddy-backup.md`).
- **Description:** Each privilege is individually justifiable (ZFS needs host mounts; Samba needs `hostNetwork :445`; NFS-Ganesha needs `open_by_handle_at`), but the combination with NAS-001/NAS-002 means any API/agent bypass lands directly on host-root-equivalent surfaces. `nodes` + `pods/log` for the agent and `namespaces get/list` for the API are broader than needed.
- **Exploit precondition:** Any prior API/agent bypass; no additional exploit needed.
- **Recommendation:** Scope agent `ClusterRole` down (drop `nodes`, drop `pods/log` unless proven needed; prefer namespaced `Role`), scope API namespace listing (or cache a fixed namespace), consider `pod-security: baseline` + per-workload exceptions instead of namespace-wide `privileged`, and document each `hostPath/hostNetwork/privileged` with a "why least-privilege cannot drop it" comment.

### NAS-016 — Verbose errors leak LDAP/agent internals to API clients — **Medium**

- **Files:** `api/internal/server/users_extra.go:30,41,46,86,108,119,125,146,156,162,209`, `users.go:18,34,45,52,56,63`, `shares.go:25,33,37,42,81,84,91,99,125-126,157,182,194-196,283`, `identity/client.go:138,145` (`connecting/binding to LDAP <addr> as <bindDN>` surfaces via `identityUnavailable` at `server.go:323-336`), `shares.go:243-245` (agent error forwarded verbatim).
- **Description:** Raw `err.Error()` (LDAP dial/bind strings, agent command failures, dataset paths) reaches unauthenticated clients (NAS-001), aiding reconnaissance.
- **Exploit precondition:** Any API call that triggers an error (bad input, LDAP down).
- **Recommendation:** Return generic messages (`"identity unavailable"`, `"share update failed"`) with a correlation ID; log details server-side. Add a lint/test asserting handlers do not interpolate `%v` of internal errors into responses.

### NAS-017 — Samba/NSS file handling + entrypoint sharp edges — **Medium**

- **Files:** `samba/image/entrypoint.sh:32` (`mkdir -p .../private` → `0755`/umask; `passdb.tdb` under `private/` not forced `0700`), `:12-20` (`SMB_CONF_PATH` required — correct but brittle; dropped env = split-brain passdb), `:133,138` (unquoted `sed "s/.../${name}/"` with `SMB_DISCOVERY_NAME/INTERFACE` from `values.yaml:152-158`; `/ & \ newlines` break/re-inject `avahi-daemon.conf`; `wsdd -n "$name" :164,166` quoted — ok), `nfs/image/entrypoint.sh:53-77,79-115` (path warnings only; no `ganesha.conf` syntax gate before `ganesha.nfsd -f`/SIGHUP; boot exits `:84-88` on bad render), `api/internal/identity/samba.go:30-130` (dead/duplicate `kubectl exec pdbedit --user <uid> --set-nt-hash <hash>` via `exec.Command` — no shell, but no `--` separator; `uid` starting with `-` = flag injection; appears unused — live path is `RenderSMBPasswd` + `pdbedit -i smbpasswd:` at `entrypoint.sh:48`), `api/internal/shares/smbusers.go:97-118,111-113` + `agent/internal/shares/shares.go:102-133,163-167` (atomic temp+rename+`Sync`, `0600` — exemplar, keep).
- **Description:** Individually moderate, collectively they weaken the share-provisioning chain that NAS-007 already targets.
- **Exploit precondition:** Control of Helm values (discovery name/iface) or reaching the dead `SMBManager` code path; misconfiguration for the `private/` mode.
- **Recommendation:** `chmod 700 private/`; quote/escape `sed` replacements (or rewrite via printf to temp file); add Ganesha pre-reload validation; delete or harden (`--` + strict `uid` allowlist) `api/internal/identity/samba.go`; assert `SMB_CONF_PATH` at startup and fail closed if unset.

### NAS-018 — Buddy operational weaknesses: enumerable jobs, predictable snapshots, state-file races — **Medium**

- **Files:** `api/internal/server/buddy_jobs.go:120-144` (`randomJobID` 32-bit; `conflicting(receiver,source,dataset)` mutual exclusion — good), `:130-144,458-521` (scheduler `NextRun` advance at start + completion — good; `validateSchedule:281-325` dataset check is loose pool-prefix match allowing sibling datasets), `buddy_send.go:110-117` (`replace` guard — good), `:213-216,220-260` (server state key `sha256(receiver|source)` truncated 64-bit, predictable + listable; `0700/0600` perms — good; no `Sync`/`fsync(dir)`, no MAC, no locking), `client.go:378-420` (same; CLI `buddyctl/main.go:582-589` keys state by source only → same source to two receivers collides), `envelope.go:68-98` (8-byte prefix per chain, 32-bit counter with no overflow check — wraps after ~4 PiB — unrealistic but unchecked; deterministic resume ciphertext reveals equality — intentional for dedup, note only).
- **Description:** No single item is directly exploitable without a valid admin/peer credential, but together they permit job enumeration/cancel by any admin, cross-receiver resume corruption (CLI), crash-lost state, and overly permissive schedule dataset matching.
- **Exploit precondition:** Admin header (jobs/schedules) or CLI multi-receiver use (state collision).
- **Recommendation:** 128-bit job IDs + ownership checks; strict dataset allowlist for schedules; key CLI state by `(receiver,source)`; `fsync` + file locking for state files; 64-bit stream counter or explicit overflow refusal.

### NAS-019 — TLS/headers/session hardening gaps — **Low**

- **Files:** `api/internal/identity/client.go:64-91` (`MinVersion TLS1.2 :69` — good; no `InsecureSkipVerify` — good), `charts/naslos/templates/authelia-config.yaml:72` (`minimum_version: TLS1.2` — good), `openldap/image/entrypoint.sh:70` (`olcTLSProtocolMin: 3.3` = TLS1.2 — good), `charts/naslos/templates/api-deployment.yaml:32-33` (`LDAPTLS_REQCERT=never` in `wait-for-ldap` probe — verification disabled), `openldap/image/Dockerfile:14-16` (`LDAP_TLS_ENFORCE=false`, `VERIFY_CLIENT=never`), `openldap/manifests/bootstrap-job.yaml:26,41,70,74` (`LDAPTLS_REQCERT` + `ldapmodify -w $(cat ...)` command line visible in `ps`), `charts/naslos/templates/traefik-middleware.yaml:26-34` (`browserXssFilter, contentTypeNosniff, frameDeny, stsSeconds 31536000` — good; no `forceSTSHeader/contentSecurityPolicy`), `charts/naslos/templates/ingress.yaml:1-52` (`websecure` + `web→websecure` redirect for UI — good), `authelia-config.yaml:15` (`jwt_secret: randAlphaNum 32` per render — good randomness, bad stability: every `helm upgrade` invalidates sessions), `api/internal/identity/persons_extra.go:50-66,152-161` (NT hash `MD4(UTF-16LE)` correct; vectors pinned `nthash_test.go:11-28`; but plaintext kept in Go string, buffer not zeroed; empty password → well-known `31D6CFE0…` at `nthash_test.go:20`, policy only in Authelia `:146-154`).
- **Recommendation:** Verify LDAP in the probe (`REQCERT=demand` + mount CA), enforce TLS (`LDAP_TLS_ENFORCE=true`) where clients support it, pass bind passwords via env/file (not `-w` argv), pin a stable `jwt_secret` in a Secret, add a minimal CSP, zero password buffers, and enforce non-empty/password-policy at the API layer.

### NAS-020 — Known-vulnerable dependencies: Go (reachable) + UI dev-only — **Low (patchable)**

- **Files:** `api/go.mod` (direct: `go-ldap/ldap/v3 v3.4.8`, `gorilla/websocket v1.5.3`, `talos/machinery v1.14.0`, `x/crypto v0.55.0`, `helm/v3 v3.16.0`, `k8s.io/* v0.31.0`), `ui/package.json` (prod: `xterm`, `xterm-addon-fit`; dev: `vite ^5`, `svelte ^4`, `@sveltejs/kit ^2`, `@playwright/test`).
- **Evidence (Go):** `go vet ./...` clean (exit 0). `govulncheck ./...` (via `go run golang.org/x/vuln/cmd/govulncheck@latest`): **"Your code is affected by 17 vulnerabilities from 5 modules"** (exit 3), plus 5 imported-but-uncalled and 4 required-but-uncalled. Called-vulnerable modules: `golang.org/x/crypto` (GO-2026-5932, fix N/A — check for newer advisory), `github.com/containerd/containerd` (GO-2026-5758 fixed `v1.7.33`, GO-2026-5622 N/A, GO-2026-5475 fixed `v1.7.33`, GO-2026-5338 N/A, GO-2026-5064 N/A, GO-2025-4108/4100 fixed `v1.7.29`, GO-2025-3528 fixed `v1.7.27`), `github.com/moby/spdystream` (GO-2026-4958 fixed `v0.5.1`), `github.com/docker/docker` (GO-2026-4887/4883 N/A, GO-2025-3829 fixed `v25.0.13+incompatible`), `helm.sh/helm/v3` (GO-2025-3888/3887 fixed `v3.18.5`, GO-2025-3602/3601 fixed `v3.17.3`). Full output saved by auditor at `/tmp/kilo/govuln.txt` during the run (not in repo).
- **Evidence (UI, run 2026-09-14 after npm became available):** `npm audit --omit=dev` → **found 0 vulnerabilities** (exit 0; prod `xterm` chain clean). Full `npm audit` (incl. dev) → **11 vulnerabilities (4 low, 6 moderate, 1 high)**, all build-time only: `cookie <0.7.0` (GHSA-pxg6-pf52-xh8x, out-of-bounds chars; via `@sveltejs/kit 1.0.0-next.0–2.70.3` ← `adapter-auto/adapter-static`; fix = breaking `@sveltejs/kit@0.0.30` downgrade path per audit, so pinning needs care), `esbuild <=0.24.2` (GHSA-67mh-4wv8-2f99, dev-server request forgery; via `vite <=6.4.2` ← `vite-plugin-svelte`; fix = breaking `vite@8.3.0`), `svelte <=5.55.6` (6× SSR/XSS advisories: GHSA-crpf-4hrx-3jrp, GHSA-m56q-vw4c-c2cp, GHSA-f7gr-6p89-r883, GHSA-phwv-c562-gvmh, GHSA-rcqx-6q8c-2c42, GHSA-pr6f-5x2q-rwfp; fix = breaking `svelte@5.57.0`). UI ships via `@sveltejs/adapter-static` (pre-rendered static + nginx), so SSR XSS blast radius is limited to build/dev, not the served bundle — still patch on the next dependency pass.
- **Exploit precondition:** Go: depends on advisory; most are transitive via helm/docker/containerd/k8s client — reachable only if the vulnerable code path is exercised (govulncheck says the 17 *are* called). UI: dev-server/SSR paths only; prod bundle has 0 known vulns.
- **Recommendation:** `go get` the fixed Go versions above (separately testable `go.mod` bump), schedule the breaking UI bumps (`svelte`, `vite`, `@sveltejs/kit` chain) as their own tested upgrade, re-run both scanners, and add `govulncheck` + `npm audit` to CI.

### NAS-021 — Buddy crypto construction is sound; residual notes (no fix required unless hardening) — **Info**

- **Files:** `api/internal/buddy/auth.go:18-23,37-53,68,104-167` (headers, `BUDDY1\nMETHOD\npath\nbodyDigest\nts\nnonce`, `bodyDigest=hex(sha256)`, full-URI path binding at `http.go:114-116` — prevents prefix-strip reinterpretation — good), `envelope.go:21-26,68-89,122-174,179-210` (`ChunkPlainSize 1MiB`, `MaxSealed 1MiB+4096`, nonce `prefix[8]||BE32(index)`, AAD `NB1|src|chain|idx|len`, `magic NBC1|len|nonce|ct+tag`, `OpenChunk` index check + length check — good; DEK wrap AAD `NB1|dek|src|chain` — good), `keys.go:53-138,176-218` (Ed25519 OpenSSH, `FingerprintSHA256` key-id, KEK 32B rand, `0600` + `Sync` — exemplar), `envelope.go:220-261` (manifest `Sign/VerifySignature`, `KeyID==fingerprint` — good), `store.go:53-56,94-125` (fingerprint hashed to `k+hex(sha256/16)` — fixes prior `/` bug — good), `peers.go:17,21-35,53-67` (source regex `^[a-z0-9][a-z0-9._/-]*$`, `..` + length + leading-`/` rejects — good; allows `a//b`, `a/./b`, trailing `/`, single `.` — cosmetic), `http.go:23,375,496` (chunk/manifest limits — good), `client.go:64` (`LimitReader(8MiB)` without truncation error — confusing failures only), `store.go:317-331` + `http.go` FS walks (unbounded work per huge chain — availability note), `buddy_send.go:110-117` (identity `replace` guard — good).
- **Description:** No shell interpolation in Go paths; `chroot` argv construction is shell-free; chunk tamper (flip/truncate/reorder/cross-chain) is caught by GCM + AAD per prior live drills noted in `AI_Handoff.md`. Residual hardening: 32-bit stream counter overflow check, `UnwrapDEK` length-floor cosmetic (`len<12+32` vs true `12+32+16`), deterministic-resume equality disclosure (intentional), `sourcesIn` 2-level walk hiding `a/b/c` from listings while quota-counting it (`store.go:60-92`), `Status.EnrollOpen` misreporting (see NAS-011).
- **Recommendation:** Add counter-overflow refusal, fix the 2-level walk or reject 3+ level sources, surface truncation errors, and bound FS-walk work (pagination). No re-design needed.

### NAS-022 — Supply-chain / operations observations — **Info**

- Registry tag reuse (`0.1.0` + `IfNotPresent`, per `AI_Handoff.md`) risks running stale images after a retag; `helm upgrade --force-conflicts` + `kubectl set image` ownership notes show image provenance is fragile.
- `openldap/manifests/secrets.yaml:10-12` placeholders are docs-only (not live) — keep it that way; ensure no real `*-password` file is ever committed.
- No `BEGIN *PRIVATE KEY` / cloud API keys found in code (only `docs/buddy-backup.md:56` placeholder + Talosconfig-via-file in `scripts/deploy-vm.sh`).
- Host-namespace mount gotcha (`AI_Handoff.md`: `HostToContainer` one-way propagation; receive dataset quota/isolation not in effect until host-namespace mount + API restart) remains a correctness/availability caveat, not a vulnerability — but quota-based assumptions (NAS-013) must not be relied on until the mount check (`df` names `<pool>/naslos-buddy`) passes.

## Appendix A — Route → auth matrix (verified against `server.go:183-258`)

| Method | Path | Handler | Auth |
| --- | --- | --- | --- |
| * | `/api/health` | `handleHealth` | none (unconditional `ok`) |
| * | `/api/ready` | `handleReady` | none (reports ldap only) |
| * | `/api/catalog`, `/api/catalog/` | `handleCatalog*` | none |
| * | `/api/apps`, `/api/apps/` | `handleApps*` | none |
| * | `/api/disks`, `/api/disks/recommend` | `handleDisks*` | none |
| * | `/api/volumes`, `/api/volumes/zfs`, `/api/volumes/zfs/import`, `/api/volumes/zfs/` | `handleVolumes/ZFS*` | none |
| * | `/api/datasets` | `handleDatasets` | none |
| GET | `/api/ws/logs` | `handleLogsWS` | **none (NAS-005)** |
| GET | `/api/pods` | `handlePods` | header-present (`pods.go:134-136`) |
| GET | `/api/namespaces` | `handleNamespaces` | header-present (`pods.go:100-102`) |
| GET | `/api/ws/exec` | `handleExecWS` | header-present (`websocket.go:318-320`) |
| * | `/api/shares`, `/paths`, `/folders`, `/status`, `/apply`, `/config/samba`, `/config/nfs`, `/` (detail) | `handleShares*` | none |
| * | `/api/notifications`, `/test` | `handleNotifications*` | none |
| * | `/api/buddy/v1/*` | `buddy.Handler` | peer Ed25519 (`buddy/http.go:91-125`); `/enroll` explicitly unsigned (`:215-218`) |
| GET | `/api/buddy/status` | `handleBuddyStatus` | none (`buddy.go:15-60`) |
| GET/POST/DELETE | `/api/buddy/peers` | `handleBuddyPeers` | header-present (`buddy.go:69-71`) |
| GET/POST | `/api/buddy/identity` | `handleBuddyIdentity` | header-present (`buddy_send.go:66-68`) |
| POST | `/api/buddy/send` | `handleBuddySend` | header-present (`buddy_jobs.go:240-246`) |
| * | `/api/buddy/restore` | `handleBuddyRestore` | header-present (`buddy_send.go:307`) |
| * | `/api/buddy/jobs`, `/jobs/` | `handleBuddyJobs*` | header-present (`buddy_jobs.go:585,598`) |
| GET/POST/DELETE | `/api/buddy/schedules` | `handleBuddySchedules` | header-present (`buddy_schedules.go:332-335`) |
| * | `/api/metrics`, `/api/dashboard` | `handleMetrics/Dashboard` | none |
| * | `/api/users`, `/users/` (`/password`, `/enable`, `/disable`, detail) | `handleUsers*` | none |
| GET/POST | `/api/groups` | `handleGroups` | none |
| GET/PUT/DELETE | `/api/groups/` | `handleGroupDetail` | none |
| GET | `/api/auth/me` | `handleAuthMe` | trusts `Remote-User`, no CIDR check (`users_extra.go:225-246`) |
| * | `/` | `FileServer(/var/naslos/ui)` | none |

Agent (`agent/internal/server/server.go:44-61`): `/health`, `/api/v1/pools*`, `/datasets*`, `/snapshots/*`, `/shares/config`, `/shares/status`, `/shares/folders`, `/zfs/send/*`, `/zfs/receive/*`, `/zfs/snapshots/*` — **all none (NAS-002)**.

## Appendix B — Privileged / RBAC table

| Workload | Privilege | File |
| --- | --- | --- |
| namespace `naslos` | `pod-security enforce: privileged` | `namespace.yaml:8` |
| `naslos-agent` | `hostPID, hostNetwork, privileged:true, hostPath /,/dev,/run,/var (HostToContainer)` | `agent-daemonset.yaml:20-27,33-61` |
| `naslos-agent` SA | `ClusterRole get,list,watch nodes,pods,pods/log` | `agent-daemonset.yaml:93-112` |
| `naslos-samba` | `hostNetwork, runAsUser:0, CHOWN,DAC_OVERRIDE,FOWNER,FSETID,SETGID,SETUID, hostPath shares/extrausers//var/mnt` | `samba-daemonset.yaml:27-46,104-115` |
| `naslos-nfs` | `hostNetwork, runAsUser:0, +DAC_READ_SEARCH, hostPath shares//var/mnt` | `nfs-daemonset.yaml:26-47,88-95` |
| `naslos-terminal` | `runAsUser:0, privileged:true, hostPath /,/var/mnt,/dev` | `terminal.yaml:54-83` |
| `naslos-api` SA | `Role pods,pods/log get/list/watch + pods/exec create` (namespaced — narrowest viable) | `terminal.yaml:89-113` |
| `naslos-api` SA | `ClusterRole namespaces get/list` | `terminal.yaml:118-137` |
| `naslos-api` | `hostPath /var/mnt RO (datasetsHostPath, values.yaml:37)` + `buddy-data hostPath RW` | `api-deployment.yaml:207-220` |
| Ingress | UI `websecure` + `web→websecure` redirect (`tls: naslos-tls`); terminal/API paths `websecure + forwardauth-authelia + security-headers` | `ingress.yaml:1-52`, `terminal.yaml:148-165`, `traefik-middleware.yaml:10-34` |

## Appendix C — Secrets / defaults table (values redacted)

| Secret / default | Location | Note |
| --- | --- | --- |
| `grafana.adminPassword` default | `charts/naslos/values.yaml:256`, `values-vm.yaml:82` | working default, must override (NAS-010) |
| `openldap.bindPassword` default | `charts/naslos/values.yaml:296`, `values-vm.yaml:67` → `authelia-config.yaml:91` | `CHANGE_ME_*` placeholder ships |
| generator defaults | `openldap/generate-secrets.sh:13-14`, `openldap/image/entrypoint.sh:12` | `:-naslos-admin`, `:-CHANGE_ME_*`, `:-admin` |
| new-user initial password | `api/internal/identity/persons.go:31` | same predictable value for every user |
| registry secret default | `scripts/deploy-vm.sh:11,23` | `:-secret` (piped via stdin — ok, but predictable) |
| LDAP bind wiring | `api-deployment.yaml:103-107` (`secretKeyRef`), `server.go:88-95` (env) | correct mechanism, weak default value |
| enroll token | `api-deployment.yaml:135` (`BUDDY_ENROLL_TOKEN`), `buddy/http.go:44-51` | single-use in-memory only (NAS-011) |
| JWT secret | `authelia-config.yaml:15` (`randAlphaNum 32`) | random but unstable across upgrades (NAS-019) |
| No private keys / cloud keys in code | grep: no `BEGIN *PRIVATE KEY` except docs placeholder | ok |

## Appendix D — Tool output

- `go vet ./...` (in `api/`): exit 0, no findings.
- `govulncheck ./...` (via `go run golang.org/x/vuln/cmd/govulncheck@latest`): exit 3 — **"Your code is affected by 17 vulnerabilities from 5 modules"**, plus 5 imported-but-uncalled and 4 required-but-uncalled. See NAS-020 for the module/fix list. Auditor-saved raw output (`/tmp/kilo/govuln.txt`) is ephemeral; re-run `govulncheck ./...` in `api/` to reproduce.
- `npm audit --omit=dev` (in `ui/`): **found 0 vulnerabilities** (exit 0).
- `npm audit` full (incl. dev, same run): **11 vulnerabilities (4 low, 6 moderate, 1 high)** — `cookie <0.7.0` (GHSA-pxg6-pf52-xh8x via `@sveltejs/kit`), `esbuild <=0.24.2` (GHSA-67mh-4wv8-2f99 via `vite`), `svelte <=5.55.6` (6× SSR/XSS GHSA-crpf-4hrx-3jrp, GHSA-m56q-vw4c-c2cp, GHSA-f7gr-6p89-r883, GHSA-phwv-c562-gvmh, GHSA-rcqx-6q8c-2c42, GHSA-pr6f-5x2q-rwfp); all fixes are breaking (`kit@0.0.30` / `vite@8.3.0` / `svelte@5.57.0` per audit output). Prod bundle unaffected (static adapter); schedule as a tested upgrade.
- Secrets grep (`CHANGE_ME|naslos-admin|TempPass123|InsecureSkipVerify|LDAPTLS_REQCERT=never`): 14 matches listed in NAS-010/NAS-019; no `InsecureSkipVerify`, no live private keys.

## Appendix E — Open questions / needs-live-verification (read-only)

1. Is `naslos-api:8080` reachable from a non-`naslos` pod / node network in the target cluster (confirms NAS-001 blast radius)? Check via `kubectl -n <other> run ... -- curl` (read-only GETs only).
2. Is `naslos-agent:9090` / node-IP `:9090` reachable cross-namespace (confirms NAS-002)? Same read-only check against `/health` only.
3. Is UI NodePort `:30080` enabled outside dev (`values-vm.yaml:42-44`)? If yes, confirm `/api/users` without headers returns data (confirms NAS-008).
4. Does a `NetworkPolicy` exist in the live cluster (none found in chart)? `kubectl get networkpolicy -A`.
5. Confirm `BUDDY_REQUIRE_AUTH` / `TERMINAL_REQUIRE_AUTH` / `TERMINAL_AUTH_HEADER` effective values in the deployed `api-deployment.yaml` env (kill-switch posture).
6. Confirm rotation status of `grafana.adminPassword`, `openldap.bindPassword`, and any `TempPass123!` users still present.

## Remediation checklist (ordered by risk)

- [ ] NAS-001 — Enforce CIDR-checked auth on every owner-facing API route; add route-table regression test.
- [ ] NAS-002 — Add agent auth (token/mTLS) + NetworkPolicy (only `naslos-api` SA → `:9090`); drop `hostNetwork` if viable.
- [ ] NAS-003 — Apply existing validators to all agent destructive sinks; add negative unit tests.
- [ ] NAS-004 — CIDR-check terminal auth; remove/constrain `TERMINAL_REQUIRE_AUTH=false`; scope terminal `Role`; justify `privileged/hostPath /`.
- [ ] NAS-005 — Gate `handleLogsWS` with `requireTerminalAuth`; regression test 401.
- [ ] NAS-006 — `EscapeFilter`/`EscapeDN` (or strict allowlists) for all LDAP interpolations; injection unit tests.
- [ ] NAS-007 — Reject `[\r\n\0]` (+`;`/`"` where structural) in share/LDAP-to-config fields; enforce `ntHash` format; quote Ganesha strings; add Ganesha pre-reload gate.
- [ ] NAS-008 — Remove NodePort outside dev, or rely on API-side auth (NAS-001); document `:30080` trust level.
- [ ] NAS-009 — Only trust `X-Forwarded-Host` from the proxy; pin public hostname for WS origin.
- [ ] NAS-010 — Fail closed on placeholder secrets; per-user random initial passwords + forced change; require grafana override.
- [ ] NAS-011 — Persist `enrollUsed`; rate-limit + uniform enroll errors; reject same-name/different-key; validate `AllowedSources` on manual add; fix `Status.EnrollOpen`.
- [ ] NAS-012 — Monotonic manifest `current`; validate manifest field formats; log rotations.
- [ ] NAS-013 — Quota check under store lock; exempt digest-equal resume; account manifest bytes; require `QuotaBytes>0`.
- [ ] NAS-014 — Persist nonces (or restart counter); per-key/total caps with LRU; 128-bit job IDs + ownership/audit.
- [ ] NAS-015 — Least-privilege RBAC pass (drop `nodes`, `pods/log`, narrow namespace listing); `baseline` + exceptions instead of namespace `privileged`.
- [ ] NAS-016 — Generic client errors + correlation IDs; log details server-side.
- [ ] NAS-017 — `chmod 700 private/`; fail closed on missing `SMB_CONF_PATH`; escape `sed` values; Ganesha gate; remove/harden dead `SMBManager` kubectl path.
- [ ] NAS-018 — 128-bit job IDs + ownership; strict schedule dataset matching; key CLI state by `(receiver,source)`; fsync + locking; counter-overflow refusal.
- [ ] NAS-019 — Verified-LDAP probe; `TLS_ENFORCE=true` where viable; password-via-file; stable `jwt_secret`; minimal CSP; zero password buffers; API password policy.
- [ ] NAS-020 — Bump fixed Go modules; schedule breaking UI bumps (svelte/vite/kit chain); keep `govulncheck` + `npm audit` in CI.
- [ ] NAS-021 — Counter-overflow check; fix 2-level source walk; truncation errors; bound FS walks.
- [ ] NAS-022 — Pinned digests instead of reused `0.1.0` tags; verify host-namespace dataset mount before relying on quota.
