# Critical Security Fixes — Naslos (NAS-001/002/003 + coupled auth Highs)

## Status: implemented (2026-09-17, branch `feature/security-fixes`)

Tasks 1–9 are implemented; the deviations from this plan and the pieces left open
are listed here so the next session does not re-derive them.

- Done: proxy-secret middleware (constant-time, fail closed), owner/public route
  composition, per-handler switches removed, agent bearer token (transport-level
  on the API side so the streaming paths are covered), agent validation at every
  destructive sink, API-side pool/disk/import validation, chart Secret +
  `proxy-identity` Middleware + env, unit tests (auth middleware, route table,
  agent token, agent validation, API disk selection), docs.
- Deviation 1: the chart's ingress templates were gated on `traefik.enabled`,
  which the Traefik subchart's values schema **rejects** (`additional properties
  'enabled' not allowed`) - so `.Values.traefik.enabled` could never be true and
  no IngressRoute/Middleware had ever been deployed. They are now gated on a new
  `ingress.enabled` (default `false`). This is what makes the documented
  production posture (Traefik + Authelia) deployable at all.
- Deviation 2: the plan's `auth.proxySecret`/`proxySecretName` and
  `agent.tokenSecret` are implemented as specified, but the generated values are
  produced in a single template per Secret (`proxy-secret.yaml` also renders the
  `proxy-identity` Middleware, `agent-token.yaml` the agent token), because
  computing a random in two templates generates different values on first install.
- Not done (deliberately): dropping the agent's `hostNetwork`. `hostPID` is gone;
  `hostNetwork` is kept with a comment - changing it needs a live multi-node check.
- Not done (out of scope per this plan): NAS-006/007 injections and NAS-009+.

## Goal
Close the three Critical findings from `docs/SECURITY-AUDIT.md` and the auth Highs they are tangled with, so that:
- the API authenticates every owner request and fails closed;
- the privileged agent requires authentication;
- no user input reaches a privileged `zpool`/`zfs`/`wipefs` argv unvalidated.

Findings in scope: **NAS-001** (API auth dead code), **NAS-002** (unauthenticated agent), **NAS-003** (agent validation gaps), plus **NAS-004** (terminal auth), **NAS-005** (`/api/ws/logs` unauthenticated), **NAS-008** (NodePort bypass). Not in scope: NAS-006/007 injections, NAS-009+ (list in the audit's remediation checklist).

## Confirmed decisions
1. **API auth** — fail-closed `RequireAuth` on all owner routes; public allow-list is `/api/health`, `/api/ready`, `/api/buddy/v1/*` (peer-key auth), and static `/`. One dev opt-out: `AUTH_DISABLED` (default `false`, logged loudly at startup).
2. **Proxy trust** — the API additionally requires an unforgeable `X-Naslos-Proxy-Secret` that only the Traefik route injects, so a pod on the trusted CIDR cannot forge identity. `TRAEFIK_CIDR` stays as defense-in-depth.
3. **Agent auth** — shared bearer token from a chart-managed Secret, required on every agent route except `/health`. Drop `hostPID`; attempt to drop `hostNetwork` (revert with a comment if ZFS ops need it).
4. **Scope** — the three Criticals plus the coupled auth Highs above, in one change.

## Post-fix trust boundary
- Browser → Traefik (`websecure`, Authelia `forwardAuth` replaces `Remote-User`, then `proxy-identity` injects the shared secret) → UI nginx (`/`) / API (terminal, and `/api/*` proxied by nginx) → API. `RequireAuth` accepts only secret + non-empty user.
- NodePort → nginx → API carries no proxy secret ⇒ owner endpoints 401. NodePort works only with `auth.disabled=true` (dev).
- Peers → NodePort → nginx → `/api/buddy/v1/*`, authenticated by Ed25519 signatures (unchanged; must NOT be wrapped by `RequireAuth`).
- API ↔ agent: HTTP + `Authorization: Bearer <AGENT_TOKEN>`; agent rejects everything else (except `/health`).

## Header / env / value names (use these exactly)
| Thing | Name |
| --- | --- |
| Proxy secret header | `X-Naslos-Proxy-Secret` |
| API env: expected proxy secret | `PROXY_SHARED_SECRET` (from Secret `naslos-proxy`, key `secret`) |
| API env: dev bypass | `AUTH_DISABLED` (`"true"` only) |
| Traefik Middleware | `proxy-identity` (`headers.customRequestHeaders`) |
| Agent token header | `Authorization: Bearer <token>` |
| Agent/API env | `AGENT_TOKEN` (from Secret `naslos-agent`, key `token`) |
| Helm values | `auth.disabled`, `auth.proxySecret`, `auth.proxySecretName`, `auth.traefikCIDR`; `agent.tokenSecret` |

## Task list

### 1. Proxy-secret verification in the auth middleware
- `api/internal/auth/middleware.go`: extend `Middleware` with `proxySecret string`; `NewMiddleware(cidrs []string, proxySecret string)`. In `RequireAuth`, after the existing `isTrusted` + `Remote-User` checks, require `subtle.ConstantTimeCompare(r.Header.Get("X-Naslos-Proxy-Secret"), m.proxySecret) == 1`; else `401`. Keep the existing context injection (`WithUser`).
- Return a clear startup `log.Fatalf` if `PROXY_SHARED_SECRET` is empty while `AUTH_DISABLED` is false (fail closed, no accidentally-open deploy).

### 2. Route wiring + dev bypass (`api/internal/server/server.go:183-258`)
- Build an `owner := http.NewServeMux()` containing every current owner route (catalog, apps, disks, volumes, datasets, ws/logs, pods, namespaces, ws/exec, shares*, notifications, buddy owner endpoints, metrics, dashboard, users*, groups*, auth/me).
- Build a `public := http.NewServeMux()` containing `/api/health`, `/api/ready`, the buddy peer handler (`buddy.PathPrefix+"/"`), and the static `FileServer`.
- Compose: outer mux = `public` + `s.router.Handle("/api/", requireAuth(owner))` (ServeMux longest-pattern match makes `/api/health` and `/api/buddy/v1/` beat `/api/`). `requireAuth` is `s.auth.RequireAuth`, or a pass-through when `AUTH_DISABLED=true`.
- `server.go:152-157`: replace the `buddyRequireAuth`/`terminalRequireAuth`/`terminalAuthHeader` fields with `authDisabled bool`; set from `AUTH_DISABLED`. `New()` passes `getEnv("PROXY_SHARED_SECRET","")` to `auth.NewMiddleware` and `getEnv("TRAEFIK_CIDR","10.0.0.0/8")`.
- Log a prominent warning when `AUTH_DISABLED=true` ("authentication is disabled; do not expose this instance").

### 3. Remove the per-handler auth switches (NAS-004/NAS-005)
- Delete `requireTerminalAuth` and `terminalUsername` in `api/internal/server/pods.go:57-92`; in `handlePods` (`pods.go:134-136`), `handleNamespaces` (`pods.go:100-102`), `handleExecWS` (`websocket.go:318-320`) drop the gate (now provided by the middleware); `terminalUsername` becomes the user from `auth.UserFromContext` (fallback `"anonymous"`) for exec open/close logs (`websocket.go:431-432`).
- `handleLogsWS` (`websocket.go:240`) needs no code change beyond moving under the owner mux — it becomes authenticated by construction. Add a test asserting 401.
- Delete `requireBuddyAdminAuth` (`buddy.go:150-161`) and its call sites (`buddy.go:69-71`, `buddy_send.go:66-68,307`, `buddy_jobs.go:240-246,585,598`, `buddy_schedules.go:332-335`).
- `handleAuthMe` (`users_extra.go:225-246`): read the user from `auth.UserFromContext` instead of the raw `Remote-User` header; 401 if absent.
- Remove now-unused env plumbing in `charts/naslos/templates/api-deployment.yaml:85-92,129-132` and the `terminal.requireAuth`/`terminal.authHeader`/`buddy.requireAuth` values in `values.yaml`.

### 4. Chart: proxy secret + Traefik injection
- New template `charts/naslos/templates/proxy-secret.yaml`: if `auth.proxySecretName` is unset, render Secret `naslos-proxy` (key `secret`) using Helm `lookup` to preserve the value across upgrades (pattern: `lookup "v1" "Secret" .Release.Namespace "naslos-proxy"` → `b64dec` existing `secret`, else `randAlphaNum 48`); if `auth.proxySecret` is set, use it verbatim (dry-run/dev).
- `charts/naslos/templates/traefik-middleware.yaml`: add Middleware `proxy-identity` with `headers.customRequestHeaders: {X-Naslos-Proxy-Secret: <value>}` (same value source as the Secret).
- `charts/naslos/templates/ingress.yaml:15-17`: add `proxy-identity` to the UI route middlewares (order: `forwardauth-authelia`, `proxy-identity`, `security-headers`).
- `charts/naslos/templates/terminal.yaml:158-160`: add `proxy-identity` to the terminal route middlewares.
- `api-deployment.yaml`: add env `PROXY_SHARED_SECRET` (`secretKeyRef` `naslos-proxy`/`secret`) and `AUTH_DISABLED` from `auth.disabled`; remove the terminal/buddy auth envs.
- Note in the template comment: nginx forwards the injected secret with the proxied `/api/` request; the NodePort listener cannot supply it. (No nginx change required — the secret, not CIDR, is the proof; optionally strip `Remote-*` in `ui/nginx.conf` later as defense-in-depth.)

### 5. Agent bearer token (NAS-002)
- Chart: Secret `naslos-agent` (key `token`, `lookup`-preserved `randAlphaNum 48`, or `agent.tokenSecret`); env `AGENT_TOKEN` (`secretKeyRef`) on the agent DaemonSet (`agent-daemonset.yaml`) and the API deployment.
- `agent/cmd/main.go`: read `AGENT_TOKEN`; if empty, exit non-zero with a clear message (no silently-open agent). Pass it to `server.New`.
- `agent/internal/server/server.go`: add an auth middleware wrapping `s.router`: allow `GET /health` unauthenticated; otherwise require `Authorization: Bearer <token>` via `subtle.ConstantTimeCompare`; else `401`. Apply before `ListenAndServe` (`server.go:88-93`).
- `api/internal/agent/client.go`: `NewClient(baseURL, token string)`; wrap `http.DefaultTransport` in a `tokenTransport` that sets `Authorization: Bearer <token>` so both `c.http` (`client.go:70-72`) and the timeout-free `streamClient` (`backup.go:42`) authenticate. Update the single construction site in `api/internal/server/server.go:117` to pass `getEnv("AGENT_TOKEN","")`.
- `agent-daemonset.yaml:20`: drop `hostPID: true`. Then attempt to drop `hostNetwork: true` (`:21`); verify the headless Service still resolves and ZFS ops work; if not, restore it with an explicit comment. Keep `privileged` + hostPath mounts (needed).

### 6. Agent validation (NAS-003)
Apply existing validators at the sinks (reuse, do not invent new ones):
- `agent/internal/zfs/operations.go:101` `CreatePool`: `ValidatePoolName(cfg.Name)`; normalize topology with `NormalizeVDevTopology` (replace the ad-hoc switch at `:140-151`); validate every disk with `normalizeDiskPath` (`devices.go:87`) + reject duplicates + reject `PoolMembers()` hits; validate `cfg.Cache` the same way and reject it equalling a data disk; `ValidateDatasetOptions(cfg.Options)` (`dataset.go:114`).
- `operations.go:209` `DestroyPool`, `PoolStatus` (`:92`), `PoolHealth`: `ValidatePoolName` first.
- `operations.go:326` `ImportPool`: `ValidatePoolName` when `name != ""`.
- `agent/internal/zfs/dataset.go:39` `Datasets(pool)`: `ValidatePoolName`.
- `dataset.go:239` `Snapshots(dataset)`: `validateDatasetPath`. `dataset.go:231` `Snapshot(dataset,name)`: `validateDatasetPath` + `ValidateSnapshotName` (`backup.go:28`).
- Keep `AddVDev` (`devices.go:179-222`) as-is (already correct).

### 7. API-side validation mirror (`api/internal/server/zfs.go`)
- `handleZFSPools` POST (`zfs.go:37-80`): after the existing name/topology checks, validate disks and cache against the node with the existing `validateAddDisks` logic (refactor it to accept a disk list + optional cache), so a bad disk is a fast 400 and never reaches the agent.
- `handleZFSPoolDetail` (`zfs.go:120-156`): enforce `poolNamePattern` on the generic GET/DELETE path (currently only `/` is rejected).
- `handleZFSImport` POST (`zfs.go:96-109`): `poolNamePattern` when `req.Name != ""`.

### 8. Tests
- `api/internal/auth/middleware_test.go`: missing secret → 401; wrong secret → 401; secret + no user → 401; secret + user → 200 and context populated; `AUTH_DISABLED` passthrough.
- `api/internal/server/routes_test.go` (new, table-driven over the owner route list): every owner route without headers → 401; `/api/health`, `/api/ready`, `/api/buddy/v1/...` reachable; `/api/ws/logs` → 401 (regression for NAS-005).
- `agent/internal/server/server_test.go`: `/health` open; any other route without/with-bad token → 401; with token → not 401.
- `agent/internal/zfs/*_test.go`: table tests — `CreatePool` rejects `-x` name, `../` disk, non-`/dev`, duplicate, pool member, unknown option; accepts a valid mirror; `Snapshot` rejects `@`/traversal; `Datasets`/`PoolStatus`/`DestroyPool` reject bad names.
- `api/internal/server/zfs_test.go`: create rejects system/unknown/duplicate/member disk and bad topology; accepts a valid request.

### 9. Values + docs
- `charts/naslos/values.yaml`: add `auth: {disabled: false, proxySecret: "", proxySecretName: "", traefikCIDR: 10.0.0.0/8}` and `agent.tokenSecret: ""`; remove `terminal.requireAuth`/`terminal.authHeader`/`buddy.requireAuth`.
- `charts/naslos/values-vm.yaml`: set `auth.disabled: true` **with a clear warning comment** (the VM has no reachable Traefik, so this is the explicit dev opt-out). Add a commented-out alternative: expose Traefik (NodePort/LoadBalancer) and reach `https://192.168.1.96:<traefik-port>` through Authelia instead.
- Docs: update `docs/terminal.md` (global auth + proxy secret), `docs/api.md` (auth section: public vs owner routes, headers), `docs/deployment.md` (new values/Secrets, agent token, dev opt-out), `docs/operations.md` (rotate `naslos-proxy`/`naslos-agent`; if rotated, restart API + agent).

## Rollout / migration
1. `helm upgrade naslos charts/naslos -n naslos -f values.yaml -f values-vm.yaml --set <tags>` — the chart creates `naslos-proxy` and `naslos-agent` Secrets before the workloads.
2. Deploy API and agent images **together** and `--wait`. Ordering caveat: a new agent rejects an old API (no token) with 401, so let the agent and API roll in the same upgrade; then restart the API deployment if it started before the agent token secret existed.
3. Bump image tags per deploy (registry reuses `0.1.0` + `IfNotPresent` — always retag fresh).
4. On the VM, confirm `auth.disabled=true` is applied before the upgrade, or UI calls will 401 on the NodePort.

## Validation (must pass before calling the fix done)
- `cd api && go vet ./... && go test ./...`; `cd agent && go vet ./... && go test ./...`.
- Deployed checks:
  - `curl -i http://<node>:30080/api/users` → 401 (no proxy secret), even with `-H 'Remote-User: admin'`.
  - With `auth.disabled=true`: UI loads and all pages work through `:30080`; API logs the disabled-auth warning.
  - Through Traefik + Authelia (if configured): login works; `/api/users` 200.
  - `curl -i http://<agent>:9090/api/v1/pools` → 401; with `Authorization: Bearer <token>` → 200; `/health` → 200.
  - Pool create via the wizard still succeeds; `zpool destroy` via the UI still works; `/api/ws/logs` without headers → 401; terminal (via Traefik) still attaches.
- Negative agent checks: `POST /api/v1/pools` with `/dev/sda` (system disk), a member disk, `../`, or a `-`-prefixed name → 400 and no state change on the node.

## Risks / notes
- Dropping `hostNetwork` may affect multi-node agent discovery; keep the token regardless and revert only `hostNetwork` if needed.
- The proxy secret is stored in the `proxy-identity` Middleware CRD and a Secret; anyone with cluster read on those can spoof. Acceptable for a single-tenant cluster; the stricter path is an out-of-band `auth.proxySecretName` and tighter RBAC.
- Helm `lookup` regenerates the secret under `helm template`/`--dry-run` (no cluster state); never run a dry-run-rendered manifest against the live cluster.
- The API now holds both the proxy secret and the agent token; that is intended, but it makes the API the highest-value in-cluster target — NetworkPolicy and least-privilege RBAC (NAS-015) remain follow-ups.
- `naslos-api` still has no bound port to the internet in prod (ClusterIP); the NodePort is the dev-only listener.

## Open items (not blocking; decide during implementation)
- Optionally strip `Remote-*` in `ui/nginx.conf` as defense-in-depth (not required once the proxy secret is enforced).
- Optionally expose Traefik on the VM with a NodePort so the VM can run authenticated without `auth.disabled`.
- NetworkPolicy for API/agent ingress (may be unenforced under Talos' default CNI) — track with NAS-015.
