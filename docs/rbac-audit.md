# RBAC roles and audit log (design)

Status: **design only (P1-11)**. No runtime enforcement or audit sink is
implemented in this change. The route surface is being edited by the open P0
pull requests (scheduled snapshots, replication, encryption, ACLs, external
directory, alerting, config export), so per-route classification and the
middleware split land **after** those PRs settle. Requirements are tracked as
`FR-RBAC-01…` and `FR-AUD-01…` in [spec.md](spec.md) and are `[OPEN]` until
implemented.

“RBAC” here means **application authorization for Naslos API users**, not
Kubernetes RBAC (the chart's `Role`/`ClusterRole` objects are unrelated and
keep their current least-privilege rules).

## 1. Goals and non-goals

Goals:

- Add three privileged roles — **read-only**, **operator**, **admin** — on top
  of the existing authenticated baseline, without weakening the SEC-3/SEC-10
  proxy gate or the existing owner/admin gate.
- Enforce authorization in **one middleware after authentication**, with every
  route explicitly classified and deny-by-default for anything unclassified.
- Keep identity management, credentials, security configuration, destructive
  platform operations and the audit trail **owner-only** (admin).
- Record every mutating action in an append-only, tamper-evident audit log with
  actor, source, timestamp, outcome and request correlation.
- Make an audit-write failure **fail closed**: a mutation is never accepted
  without a durable audit record.

Non-goals for this stage:

- No runtime code, chart change, LDAP group creation or Authelia rule change.
- No per-dataset or per-share ACL model (that is P0 item 4; this design only
  assigns the future ACL editor routes a minimum role).
- No SIEM/log-shipping integration, no external audit database, no Prometheus.
- No replacement of the MCP token scopes (FR-MCP-03) or the MCP call audit
  (FR-MCP-07); the durable audit log complements them.

## 2. Current state

| Layer | Today |
| --- | --- |
| Proxy authentication | Traefik forwardAuth → Authelia → OpenLDAP; the API trusts `Remote-*` headers only from `TRAEFIK_CIDR` with the proxy secret (SEC-3/SEC-10) |
| Groups | `naslos_admins` (owner gate) and `naslos_users` (authenticated baseline). Authelia's `users_filter` admits only these two groups |
| Route groups | `public` (`/api/health`, `/api/ready`), the Buddy peer API and the `/mcp` endpoint with their own guards, `user_routes` (`/api/auth/me`, `/api/dashboard`, `/api/metrics`), `owner_routes` (every other `/api/*` route, behind `require_admin`), and the in-process `internal_router` used only by MCP dispatch |
| MCP | Bearer tokens with `read` < `write` < `full` scopes; `tools/list` and `tools/call` re-check the scope; the dispatcher injects a synthetic admin identity into the internal router |
| Audit | Only MCP `tools/call` is audited: a structured log line plus a bounded in-memory ring surfaced at `GET /api/mcp` (FR-MCP-07). Arguments are never recorded |

Important current detail: the UI labels `naslos_users` as “read-only”, but that
group only reaches the dashboard, metrics and `/api/auth/me`; every inventory
GET (shares, pools, apps, disks) is on the owner router and is admin-only. The
new **read-only role is a new, privileged group**, not `naslos_users`. The
misleading UI label must be corrected when the role model ships.

## 3. Role model

Roles are ordered; a higher role is always a superset of the lower ones.

| Role | Group (new) | Intent |
| --- | --- | --- |
| **Authenticated baseline** | `naslos_users` | Signed-in user with no privileged role: dashboard, metrics, own identity. Not part of the privileged hierarchy |
| **Read-only** | `naslos_readonly` | Inspect operational state and configuration that contains no secret material |
| **Operator** | `naslos_operators` | Read-only plus day-2, workload-scoped operations that are reversible or confirmed and scoped to a named workload |
| **Admin** | `naslos_admins` | Everything, including identity, credentials, security configuration, destructive platform operations and the audit trail |

### 3.1 Mapping from Authelia

- Role is derived **per request** from the trusted `Remote-Groups` header,
  case-insensitively. The header is already only accepted with the proxy secret
  from the Traefik CIDR (SEC-3/SEC-10).
- Precedence: `naslos_admins` → admin; else `naslos_operators` → operator; else
  `naslos_readonly` → read-only; else authenticated baseline, regardless of
  other groups.
- A user in several of the new groups gets the highest role. `naslos_admins`
  never loses its current access.
- No role is inferred from email, display name, path, query string or request
  body. No client-supplied header other than the proxy-injected `Remote-*`
  family can grant a role.
- Roles are not cached in a session or a process-global map: the header set is
  re-evaluated on every request. Revoking a group takes effect on the next
  request (an already-issued Authelia session still lives until its normal
  expiry/inactivity limit, which is the existing SSO caveat).
- Until `naslos_operators`/`naslos_readonly` are added to Authelia's
  `users_filter`, a user in only one of those groups cannot log in at all.
  Until they are added to the two-factor subject list, an operator can open a
  one-factor session. Both are deliberate future auth changes, not part of
  this design-only pass.

### 3.2 What each role may do

| Area | Read-only | Operator | Admin |
| --- | --- | --- | --- |
| Dashboard, metrics, own identity | yes | yes | yes |
| Inventory/status GETs (pools, datasets, disks, shares, apps, jobs, VMs, images, network, DDNS entries without credentials, domains/cert metadata, user/group listings) | yes | yes | yes |
| Log streams (`/api/ws/logs`) | no | yes | yes |
| Terminal exec (`/api/ws/exec`) | no | no | yes |
| VM create/update/start/stop/restart/snapshot/restore, image import/upload/delete, VNC console | no | yes | yes |
| Share create/update/delete/apply and folder operations | no | yes | yes |
| Dataset create/snapshot, dataset registry add/remove, pool import | no | yes | yes |
| Pool destroy, device add/remove, dataset destroy | no | no | yes |
| App install/update/configure (non-privileged), app jobs, catalog reads | no | yes | yes |
| VM delete, app uninstall, privileged install, exposure/SSO/auth changes, chart sources, namespace pool | no | no | yes |
| Dynamic-DNS entry create/update/delete (credentials), provider config | no | no | yes |
| Domains, ACME certificates, SSO promotion | no | no | yes |
| Notification settings and test push | read only | test/enable non-secret routing | credentials and secret-bearing channels |
| Host network changes, system upgrade/reboot/operations | no | no | yes |
| Users/groups/passwords/enable/disable | listings only | listings only | yes |
| MCP settings and tokens | no | no | yes |
| Buddy peers/identity/restore | no | no | yes |
| Buddy send, jobs cancel, schedules | reads | run/cancel/schedule | yes (plus peers/identity/restore) |
| Audit log read/verify/export | no | no | yes |
| Config export/import | no | no | yes |
| Dataset encryption keys, external directory/Kerberos config | no | unlock/lock (future) | key custody and directory config |

This table is the **policy**; the implementation keeps the authoritative
per-route `(method, path) → minimum role` table in code next to the router and
tests it against the route inventory. A prose table would drift.

## 4. Route classification

### 4.1 Existing route groups

| Router | Routes | Minimum role |
| --- | --- | --- |
| `public` | `/api/health`, `/api/ready` | none |
| Buddy peer API | `/api/buddy/v1/*` | peer Ed25519 key, not the role model |
| MCP endpoint | `/mcp`, `/mcp/` | MCP bearer token scope, not the role model |
| `user_routes` | `/api/auth/me`, `/api/dashboard`, `/api/metrics` | authenticated baseline |
| `owner_routes` | every other `/api/*` route, including the `/api/{*rest}` 404 catch-all | **admin today**; split into read-only / operator / admin routers in Phase 2, then routes move between buckets in Phase 4 |
| `internal_router` | the same handlers without HTTP auth, used only by MCP dispatch | MCP scope re-check (never mounted on the listener) |

Classification is by **(HTTP method, path pattern)**, not path alone:

- A path may have methods at different minimum roles (for example
  `GET /api/apps/{name}` read-only, `PUT` operator, `DELETE` admin).
- Safe methods (`GET`, `HEAD`, `OPTIONS`) never exceed read-only on a route
  that read-only may use; an unsafe method must be explicitly assigned
  operator or admin. A non-mutating `POST` (for example the topology advisor)
  needs an explicit read-only exception and a comment.
- An unknown route or an unclassified method falls into the **admin** bucket,
  never the public or read-only bucket. The `/api/{*rest}` fallback stays
  behind `require_auth` and answers 404, so route existence is not leaked.
- `OPTIONS` is not a CORS preflight surface (there is no CORS); it follows the
  same role check as the path.

### 4.2 Future P0 routes

The P0 PRs will add route families; this is the intended minimum role, to be
confirmed when each PR's final route table is known.

| P0 item | Planned surface | Read-only | Operator | Admin |
| --- | --- | --- | --- | --- |
| 1. Snapshot/scrub/SMART schedules | schedule CRUD, run-now, scrub status, SMART test scheduling | GET schedules/runs/results | create/update/delete policies, run snapshot, start scrub, schedule short test | long-test policy changes if they can affect availability; pool device changes remain admin |
| 2. Replication / rsync / cloud sync | tasks, credentials, runs, retry/cancel | GET tasks/jobs/results | create non-secret task, run/retry/cancel/pause | endpoints and credentials (secret material), destructive target changes |
| 3. Dataset encryption | create encrypted dataset, unlock/lock, key custody | encryption status | unlock/lock an existing dataset | create with key custody choice, export/rotate keys, change passphrase policy |
| 4. ACL editor | dataset/share ACL and effective-access preview | GET ACL/effective access | apply presets/ACL entries within guardrails | owner/inheritance changes, anything that can lock out the service account |
| 5. External directory / Kerberos | external LDAP/AD config, keytabs, idmap | status/health only | none | all configuration and credentials |
| 6. Alert channels / reporting | SMTP/webhook/SNMP/syslog channels, history | GET channels/history | test send, non-secret routing | channel credentials and secret-bearing URLs |
| 7. Config export/import | export bundle, import/restore | none | none | export and import (contains config/secrets; import is destructive) |

### 4.3 Owner-only routes stay owner-only

The following surfaces require **admin** and must never be delegated to
read-only or operator, even if a future route is a GET:

- Users, groups, passwords, enable/disable, and any identity write.
- MCP settings and token create/revoke.
- Dynamic-DNS entries, provider credentials and any secret-bearing setting.
- Domains, ACME certificates and SSO promotion.
- Chart sources, app exposure/SSO/auth changes, privileged installs, app
  uninstall, namespace pool changes.
- Host network changes, system upgrade/reboot/operations.
- Pool destroy, device add/remove, dataset destroy.
- Buddy peer authorization, identity key creation/overwrite, restore.
- Audit log read/verify/export.
- Config export/import.
- Dataset encryption key custody and external directory/Kerberos config.
- Terminal exec (the host shell is effectively admin-level access).

Reads that remain admin-only include anything that reveals credentials, key
material, token metadata, peer identities, or the audit trail. User/group
**listings** (no hashes or secrets) may be read-only because operators need
them to configure share access; every user/group **mutation** stays admin.

## 5. Enforcement design

### 5.1 Middleware order

```
Traefik forwardAuth (Authelia, 2FA for privileged groups)
  → require_auth           (SEC-3/SEC-10, CSRF SEC-15, JSON body rule)
  → audit_begin            (unsafe methods only; after identity, before role)
  → require_role(min)      (read-only | operator | admin)
  → handler
  → audit_end              (same correlation id; denied outcomes included)
```

- `require_auth` and the SEC checks are unchanged. **No route bypasses them**,
  including the new role layers.
- `audit_begin` runs before `require_role` so a **denied mutation is audited**
  with the authenticated actor and the refusal.
- The role check runs after authentication, so a missing identity is `401`
  (unchanged) and an authenticated user with too low a role is `403` with the
  existing JSON error shape (`{"error":"Forbidden"}`). A 401 must never be
  turned into a 403 or vice versa.
- Handlers must not perform ad-hoc role checks. A handler that needs the actor
  reads the stashed `UserInfo`/role for audit fields only; enforcement is the
  middleware.
- Public routes and the Buddy peer API keep their own guards; `/mcp` keeps
  its bearer guard and scope re-checks.

### 5.2 Route registration and deny-by-default

Implementation shape (after the P0 merges):

- Split `owner_routes()` into `read_routes()`, `operator_routes()` and
  `admin_routes()` (names illustrative), each layered with
  `require_role(MinimumRole::…)` after `require_auth`. `internal_router()`
  merges all of them for MCP dispatch without the HTTP auth layers.
- Keep the current admin bucket as the default destination for every existing
  owner route. Phase 4 moves routes out of it one family at a time; nothing is
  moved to read-only or operator without a test.
- The `/api/{*rest}` catch-all stays auth-gated and role-checked by the router
  it belongs to (admin bucket by default), so an unknown path cannot become a
  probe.
- Add a contract test that walks the route inventory and fails if any route
  has no minimum role, or if an unsafe method is assigned read-only without a
  documented exception.

### 5.3 Method-level matrix

| Method/route class | Read-only | Operator | Admin |
| --- | --- | --- | --- |
| Safe GET/HEAD on non-sensitive inventory | yes | yes | yes |
| Safe GET/HEAD on secret/security surfaces (users/groups is the listing exception) | no | no | yes |
| Unsafe method on a workload the role owns (VM, share, app, dataset create, snapshot, schedule) | no | yes | yes |
| Unsafe method on platform/security/identity/credential surfaces | no | no | yes |

### 5.4 MCP parity

MCP bypasses Authelia and the HTTP role middleware by design (a programmatic
client cannot complete 2FA), so the dispatcher must not become a privilege
escalation path:

- Existing scope rules stay authoritative: `read` cannot call write tools,
  `write` cannot call destructive tools, and `full` tools still require the
  `confirm` argument (FR-MCP-03/04).
- When the role model ships, the tool registry's declared level must be
  compared with the token scope **and** the route's minimum role. A `read`
  token can only reach tools whose route is read-only; a `write` token cannot
  reach an owner-only admin tool. `full` remains administrator-equivalent
  without 2FA and must be documented as such.
- The dispatcher must keep re-checking on `tools/call`; `tools/list` filtering
  is not enforcement.
- The synthetic identity injected into `internal_router` must carry the
  effective role for audit, but the internal router must stay unreachable from
  the HTTP listener (proved by
  `forged_identity_cannot_bypass_the_public_auth_layers`).

## 6. Migration and compatibility

### 6.1 Phases

| Phase | Change | Behavior change |
| --- | --- | --- |
| 0 (this PR) | Design doc + `[OPEN]` requirements | none |
| 1 | Audit sink and generic audit middleware (method/path based; no role classification) | mutations are audited; an audit failure refuses the mutation |
| 2 | Role parser and `require_role` middleware; split routers; **every current owner route starts in the admin bucket**; `naslos_users` stays baseline | none: `naslos_admins` keeps full access, `naslos_users` still gets 403 on owner routes |
| 3 | Add `naslos_operators` and `naslos_readonly` as optional LDAP groups; extend Authelia `users_filter` and the 2FA subject list; `/api/auth/me` adds `role` (additive); UI labels corrected | users without the new groups see no change; admins are unaffected |
| 4 | Reclassify route families from the admin bucket to read-only/operator per §4, one family per PR, with tests and docs | only users placed in the new groups gain access |
| 5 | Role-aware UI (hide/disable controls; the API still enforces) and the audit viewer | presentation only |

Phase 2 is intentionally a no-op release gate: it proves the middleware and
admin compatibility before any route is delegated.

### 6.2 Compatibility matrix

| Groups on the user | Effective role | Owner routes today |
| --- | --- | --- |
| `naslos_admins` | admin | unchanged: allowed |
| `naslos_users` | baseline | unchanged: 403 |
| `naslos_users` + `naslos_operators` | operator | Phase 2: 403 (routes still admin); Phase 4: operator routes |
| `naslos_readonly` (once admitted by Authelia) | read-only | Phase 2: 403; Phase 4: read-only routes |
| no group | baseline (Authelia rejects login) | n/a |
| admin + operator + read-only | admin | unchanged: allowed |

Guarantees for existing admins:

- `naslos_admins` is checked first and always maps to admin; no new group is
  required.
- The current owner gate is the default bucket in Phase 2, so every route
  remains reachable exactly as before until deliberately reclassified.
- The 401/403 status split, the JSON error shape and the SEC-10 proxy gate do
  not change.
- `/api/auth/me` only gains a `role` field; existing fields keep their names
  and shape.
- MCP tokens, Buddy peer keys and the installer's admin login are untouched.

### 6.3 Rollback

- Remove a user from `naslos_operators`/`naslos_readonly`; they fall back to
  baseline immediately on the next request (subject to the Authelia session).
- Deleting the new LDAP groups is safe: no existing route depends on them.
- The role middleware itself has no global bypass. If a reclassification is
  wrong, revert that family's classification commit; admin access is never
  affected.
- Audit-log data is never deleted to roll back a deployment. If the audit
  subsystem is broken, mutations fail closed and the incident is handled as
  an outage, not by disabling the log.

## 7. Audit log design

### 7.1 Storage and format

- Location: `/var/lib/naslos/audit/` on the existing state volume (the
  `naslos-shares-config` PVC mounted at `/var/lib/naslos`). Directory mode
  `0700`, files `0600`, owned by the API's non-root user.
- Format: JSON Lines, one record per line, UTF-8, no pretty printing.
  Segments are named `segments/audit-<20-digit-seq>.jsonl` and are opened with
  `O_APPEND`; a record is written and `fsync`ed before the mutation proceeds.
- The active segment is never edited. Rotation seals a segment (flushes and
  `fsync`s it, writes its digest to the checkpoint file) and opens the next
  one. There is no update/delete API and no log-editing UI.
- State PVC sizing: the current default is **64 MiB shared with every other
  Naslos state file**. When audit ships, the chart must either raise
  `api.sharesConfig.size` (recommended: 512 MiB) or add a dedicated
  `naslos-audit` PVC mounted at `/var/lib/naslos/audit`. Until then the
  default budget is small and retention prunes early; that is a deployment
  decision, not a silent bypass.
- The audit directory is separate from the application log. Container stdout
  logs remain for operators/debugging and are not the audit trail.

### 7.2 Record shape

Every record is a canonical JSON object. Field names are stable API contract.

```json
{
  "v": 1,
  "seq": 1042,
  "id": "<record-id>",
  "ts": "2026-11-21T23:15:13.123456789Z",
  "requestId": "<request-id>",
  "prev": "sha256:<hex>",
  "hash": "sha256:<hex>",
  "actor": {
    "kind": "user",
    "name": "<username>",
    "role": "operator",
    "groups": ["naslos_users", "naslos_operators"],
    "auth": "authelia"
  },
  "source": {
    "method": "POST",
    "path": "/api/shares",
    "route": "/api/shares",
    "peer": "<peer-address>",
    "client": null,
    "userAgent": "<truncated>"
  },
  "action": {
    "name": "share.create",
    "target": "<share-name>",
    "phase": "begin",
    "params": {}
  },
  "outcome": {
    "state": "attempted",
    "httpStatus": null,
    "errorCode": null,
    "error": null,
    "durationMs": null
  },
  "correlation": {
    "mcpCallId": null,
    "jobId": null,
    "operationId": null
  }
}
```

| Field | Meaning |
| --- | --- |
| `v` | Record schema version (starts at 1) |
| `seq` | Monotonic sequence number across the whole chain; never reused |
| `id` | Unique record id (ULID/UUIDv7-class, unguessable enough for correlation) |
| `ts` | UTC RFC 3339 with nanosecond precision |
| `requestId` | Validated or generated request id; also echoed as `X-Request-ID` |
| `prev` | Hash of the previous record; all-zero genesis for the first retained record |
| `hash` | `sha256` over the canonical record with `hash` omitted |
| `actor.kind` | `user`, `mcp`, `system`, `peer` |
| `actor.name` | Username for users; token name for MCP; component/job for system; peer name for peers |
| `actor.role` | Effective role for users; token scope for MCP; omitted for system/peer |
| `actor.auth` | `authelia`, `mcp-token`, `system`, `buddy-key` |
| `source.method`/`path`/`route` | HTTP method, concrete path and route template; `route` makes aggregation safe |
| `source.peer` | Peer address seen by the API (the proxy address unless a trusted client-IP header is configured) |
| `source.client` | Original client address only if the proxy is configured to overwrite and the API is configured to trust it; otherwise `null` |
| `source.userAgent` | Truncated user agent; no cookies or other headers |
| `action.name` | Stable dotted action name (`share.create`, `user.password`, `system.reboot`, …) |
| `action.target` | The resolved target (share/dataset/app/VM/user/pool name); never a secret |
| `action.phase` | `begin` or `end`; the pair shares the `requestId` and the action/correlation fields |
| `action.params` | Only allow-listed, non-secret scalars for the action; usually empty |
| `outcome.state` | `attempted`, `succeeded`, `failed`, `denied` |
| `outcome.httpStatus` | Handler/refusal HTTP status |
| `outcome.errorCode` | Stable sanitized code; no stack traces or secrets |
| `outcome.error` | Sanitized message (bounded length) |
| `outcome.durationMs` | End-record duration |
| `correlation.*` | MCP call id, async job id, system operation id where applicable |

`begin` is written before the mutation and `end` after it. The chain includes
both, so a crash between them still leaves evidence of the attempt.

### 7.3 What is audited

| Class | Examples | Record |
| --- | --- | --- |
| Every unsafe `/api` request that reaches a handler | POST/PUT/PATCH/DELETE on shares, users, pools, datasets, apps, VMs, domains, DDNS, network, system, MCP settings | begin + end, including 4xx/5xx outcomes |
| Authorization denials with a known actor | operator calls an admin route; read-only calls a mutation | begin + `denied` end |
| MCP calls | every `tools/call`, all scopes | one durable record plus the existing in-memory ring; no arguments |
| Terminal and console sessions | `/api/ws/exec`, `/api/ws/vm/{name}/vnc` | open and close; target namespace/pod/VM; no keystrokes or frames |
| Buddy peer mutations | enroll, chunk/manifest PUT, prune | actor `peer` with the peer key fingerprint |
| Background mutations | scheduled snapshot/scrub/SMART, replication/cloud sync, DDNS reconcile changes, certificate issue/renew, Buddy scheduled sends, network watchdog revert, system upgrade/reboot steps | actor `system:<component>`, `jobId`/`operationId` correlation |
| Security-relevant reads | audit read/verify/export, config export | begin + end (the audit viewer is itself audited) |

Not audited durably:

- `/api/health`, `/api/ready`, dashboard, metrics and ordinary safe reads
  (they would flood the log without adding accountability).
- Unauthenticated 401s. They have no actor and are trivially flappable; they
  remain in structured logs/metrics and are rate-limited by the proxy.
- Request or response bodies, MCP arguments, cookies, authorization headers,
  passwords, hashes, tokens, private keys and secret values (see §7.5).

### 7.4 Actor, source, time, outcome and correlation

- **Actor**: derive from the same trusted `UserInfo` the handlers see. MCP
  records the token id/name and scope (never the token or its hash). System
  jobs record the component and job id. Buddy peers record the peer name and
  public-key fingerprint.
- **Source**: method, concrete path, route template, peer address, an optional
  trusted client address, and a truncated user agent. The proxy secret and
  `Remote-*` values are never recorded.
- **Timestamp**: server UTC clock at begin/end, RFC 3339 nanoseconds. If the
  clock jumps backwards, `seq` remains monotonic and `ts` is recorded as
  observed; verification never assumes time ordering.
- **Outcome**: begin is `attempted`; end is `succeeded`, `failed` or `denied`
  with the HTTP status and a sanitized error. A crash between begin and end
  leaves `attempted` as the last record, which is intentional evidence.
- **Request correlation**: accept `X-Request-ID` for correlation only if it
  matches `[A-Za-z0-9._:-]{1,128}`; otherwise generate one. It is never used
  for authorization. Echo it on every API response (including errors) and
  share it with the structured log span. For
  MCP, also record a generated `mcpCallId` plus the token id; JSON-RPC ids are
  client-controlled and are not trusted as the audit key. For async work, the
  job id/operation id is the correlation id.
- **Client IP**: the API currently sees the proxy's peer address. Recording a
  client address requires the proxy to overwrite `X-Forwarded-For` (or an
  equivalent header) and the API to trust it only from the proxy CIDR with the
  secret. Until that is configured, `client` is `null`; the design must not
  pretend a spoofable header is trustworthy.

### 7.5 Redaction

- Never record request/response bodies, MCP arguments, query strings or
  headers wholesale. Record only the route template plus an allow-listed
  `action.target` and scalar params.
- A recursive redactor applies to any structured value that is copied into
  `action.params`: keys matching `password`, `passphrase`, `secret`, `token`,
  `key`, `credential`, `authorization`, `cookie`, `hash`, `private`, `seed`,
  `kek`, `dek` are replaced with `<redacted>`.
- Explicitly forbidden values: LDAP/SMB passwords and NT hashes, TOTP/WebAuthn
  secrets, proxy secret, MCP bearer tokens and their hashes, Buddy KEKs/DEKs
  and private keys, TLS private keys, DDNS/cloud/SMTP/webhook credentials,
  dataset encryption keys/passphrases, config-export contents.
- Error messages are sanitized and bounded; no stack traces, command lines or
  upstream response bodies in the audit record.
- Tests seed canary strings into every secret-bearing path and assert they are
  absent from the serialized log and the audit query responses.

### 7.6 Tamper evidence

- The hash chain is the baseline: `hash = sha256(canonical(record without
  hash))` and `prev` is the previous record's hash. Editing, reordering,
  deleting or inserting a record breaks verification from the first retained
  record.
- A checkpoint file records each sealed segment's first/last sequence, record
  count and head hash. On startup the API verifies the retained chain; an
  admin-only `GET /api/audit/verify` repeats the check on demand.
- A plain hash chain cannot stop a writer with full volume access from
  rewriting the whole chain. Recommended hardening: an HMAC key mounted from a
  Kubernetes Secret **outside** the state volume (`AUDIT_HMAC_KEY_FILE`), or a
  periodic checkpoint copied off-box. The design records the residual risk
  explicitly rather than claiming cryptographic immutability.
- Verification failure marks the subsystem degraded, raises an alert and
  refuses further mutations (§7.8). It never truncates or "repairs" the log
  automatically.
- Recovery after tamper/corruption is an explicit admin action: preserve the
  files, export what verifies, then start a new chain with an
  `audit.recovery` record that records the reason and the last verified
  sequence/hash. Old files are archived, not deleted.

### 7.7 Retention and size bounds

- Proposed defaults: `AUDIT_RETENTION_DAYS=90`,
  `AUDIT_MAX_BYTES=67108864` (64 MiB), `AUDIT_SEGMENT_BYTES=8388608`
  (8 MiB). These are design defaults; the implementation PR owns the final
  names and chart wiring.
- When the age or byte bound is exceeded, delete the **oldest sealed**
  segment(s) only. Never delete the active segment, the newest sealed
  segment, or the checkpoint. Write an `audit.retention.prune` record to the
  active segment listing the pruned segments and their head hashes, so the
  surviving chain has a verifiable bridge.
- The state volume is shared. The audit budget must be enforced by the audit
  subsystem, not by filling the volume and taking down the API. If the volume
  is full despite pruning, mutations fail closed (§7.8); no emergency
  truncation.
- Admin export is available before pruning (`GET /api/audit/export`), and the
  retention action is audited.

### 7.8 Failure semantics

The hard rule: **an audit write failure must not silently allow a mutation.**

1. `audit_begin` writes and `fsync`s the begin record **before** the handler
   runs. If that write fails (ENOSPC, EIO, read-only volume, verification
   failure), the API returns `503` with an `audit unavailable` error and the
   handler is **not** invoked. No mutation happens without a durable attempt
   record.
2. The handler runs only after the begin record is durable.
3. `audit_end` writes and `fsync`s the end record. If it fails, the outcome is
   no longer silently trusted: the API marks audit degraded, raises an alert,
   refuses **all subsequent mutations** until repaired, and returns an
   explicit `audit_write_failed` error stating that the action may have been
   applied. It must not return a success response with an unaudited outcome.
4. While degraded, reads keep working; every unsafe method is refused with
   `503`. `/api/ready` reports the audit state (for example
   `{"status":"ok","ldap":"up","audit":"degraded"}`), and the existing ntfy
   alerting path is used for the transition.
5. Missing/unwritable audit storage at startup is a fatal configuration error
   for the API (fail closed), the same posture as a missing proxy secret.
   A development-only ephemeral sink, if ever added, must be explicit
   (environment name, startup log, `/api/ready` reporting) and must not be
   reachable in a chart-rendered install.
6. Logging the failure must not leak the failing mutation's secrets; audit
   errors record only the operation name, target and sanitized error.

### 7.9 Audit access API (future)

- `GET /api/audit` — admin-only, owner-gated (SEC-10), paginated, bounded
  (`from`, `to`, `actor`, `action`, `outcome`, `limit`); returns records, never
  deleted/pending secrets.
- `GET /api/audit/verify` — admin-only; verifies the retained chain and
  returns `{ok, firstSeq, lastSeq, firstHash, lastHash, records, errors}`.
- `GET /api/audit/export` — admin-only JSONL stream for a time range; the
  export itself is audited.
- No `PUT`/`PATCH`/`DELETE`; there is no API to edit or purge the log. The
  UI is a viewer only.

## 8. Testing plan

Unit/contract (Rust, future):

| Planned test | Verifies |
| --- | --- |
| `role_precedence_is_admin_operator_readonly`, `role_mapping_ignores_untrusted_identity_sources`, `naslos_users_stays_baseline` | mapping, case-insensitivity, admin superset, unknown group → baseline (FR-RBAC-01/02) |
| `every_route_declares_a_minimum_role`, `unsafe_methods_are_never_readonly_without_an_exception` | route inventory has no unclassified `(method, path)`; unsafe methods are explicit (FR-RBAC-04) |
| `unclassified_route_falls_into_the_admin_bucket`, `role_hierarchy_is_monotonic` | deny-by-default and monotonic hierarchy (FR-RBAC-03/04/10) |
| `owner_only_routes_refuse_operator`, `owner_only_routes_refuse_readonly`, `operator_cannot_mutate_owner_only_surfaces`, `readonly_cannot_mutate` | owner-only surfaces and method floor (FR-RBAC-05/06) |
| `admin_group_keeps_full_access` | migration compatibility, including the existing owner-route sweep (FR-RBAC-08) |
| `forged_identity_cannot_bypass_role` | forged extensions/headers still fail the proxy/role gates (FR-RBAC-03, extends `forged_identity_cannot_bypass_the_public_auth_layers`) |
| `mcp_scope_is_never_weaker_than_the_route_role` | MCP parity (FR-RBAC-07) |
| `audit_record_shape_and_chain` | canonical JSON, field contract, begin/end pairing (FR-AUD-02) |
| `audit_chain_detects_tampering` | edited/deleted/truncated records fail verification (FR-AUD-06) |
| `audit_write_failure_refuses_the_mutation` | failing begin → 503, handler not called (FR-AUD-08) |
| `audit_end_failure_degrades_and_refuses_followups` | no silent success after an end-write failure (FR-AUD-08) |
| `audit_redacts_canary_secrets_and_never_records_mcp_arguments` | redaction/allow-list (FR-AUD-05) |
| `audit_retention_prunes_only_sealed_segments_and_records_it` | retention/size bound (FR-AUD-07) |
| `audit_request_id_is_validated_and_echoed` | request correlation (FR-AUD-04) |
| `audit_survives_restart_and_appends` | no truncation on restart (FR-AUD-01) |
| `denied_mutation_is_audited`, `background_mutations_record_a_system_actor` | authorization denials and system actors (FR-AUD-03) |
| `audit_never_records_mcp_arguments`, `buddy_peer_mutation_records_the_key_fingerprint` | MCP and peer actor coverage (FR-AUD-10) |
| `audit_routes_are_admin_only`, `audit_read_is_itself_audited` | audit access control (FR-AUD-09) |

Playwright (future, after the API ships):

| Test | Verifies |
| --- | --- |
| `rbac.spec.ts` | admin sees every control; operator sees operational controls but no identity/credential/security controls; read-only sees no mutation controls; forced API calls return 403 (FR-RBAC-06/09) |
| `audit.spec.ts` | admin can view/filter/verify; operator/read-only get 403; records show actor/action/outcome; no canary secret appears (FR-AUD-05/09) |

Live drill (after implementation, on a disposable test instance — never part
of this design PR):

1. Create the new groups and assign one user per role; verify admin, operator,
   read-only and baseline logins and 403/200 behavior for representative
   routes.
2. Confirm an operator cannot create/delete users, read MCP tokens, change
   domains, reboot or destroy a pool; confirm admin still can.
3. Verify the 2FA policy applies to operators and that removing a group
   revokes access on the next request.
4. Perform a mutation, restart the API, verify the log appends without
   truncation and `verify` stays green.
5. Tamper with a copied log in a scratch directory; verify detection and the
   mutation-refusal/degraded behavior.
6. Simulate a full audit volume in a scratch instance; verify mutations return
   503 and reads still work.

## 9. Open decisions

These are deliberately left to the implementation PRs after the P0 route
surface settles:

- Whether operator may delete a VM or uninstall an app (workload-scoped but
  data-destroying). Recommendation: keep both admin; expose a scoped
  “delete workload” action later with `confirm` and audit if operators need
  it.
- Whether read-only requires 2FA or only one-factor. Recommendation: operator
  and admin get 2FA; read-only may stay one-factor, reviewed with the threat
  model.
- Whether the new groups are created by the bootstrap job (idempotent empty
  groups) or by an admin after install. Recommendation: bootstrap them
  empty so membership can be assigned immediately; existing installs get a
  migration note.
- Audit HMAC key custody and whether the checkpoint is also copied off-box.
- Audit storage sizing: raise the shared state PVC or add a dedicated PVC.
- The exact audit query/export parameters and pagination limits.
- Whether `/api/auth/me` returns `role` as a string, an ordered list, or both.
  Recommendation: additive `role` string plus the existing raw `groups`.

## 10. What waits for the P0 merges

- The final per-route classification table and the `read_routes`/
  `operator_routes`/`admin_routes` split: the P0 PRs add and change routes in
  `rust/api/src/server/mod.rs`, so editing that file now would conflict and
  the classification would be stale on merge.
- The route-inventory contract test and the role-aware Playwright specs.
- The chart/Authelia changes (new groups in `users_filter`, operator in the
  2FA subject list, audit volume size/env).
- The UI role-aware navigation and the audit viewer.
- The `/api/auth/me` additive `role` field and the UI label correction for
  `naslos_users`.

The audit middleware is route-surface independent (it keys off method and
path), but it touches the same router module; land it after the P0 PRs to keep
the merge surface small. The design and the `[OPEN]` requirements are stable
now.

## 11. References

- [spec.md §2.2](spec.md) — SEC-3/SEC-10 trust boundaries.
- [spec.md §3.14](spec.md) — existing MCP scopes and call audit.
- [identity-sso.md](identity-sso.md) — Authelia/OpenLDAP groups and 2FA rules.
- [api.md](api.md) — current route tables.
- [terminal.md](terminal.md) — why the exec surface stays admin-only.
- [mcp.md](mcp.md) — token scopes and the `confirm` rule.
