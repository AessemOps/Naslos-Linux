# Full codebase review — Naslos (master)

## Baseline and deliverable

- **Baseline**: `master` (`2f3ece0`, PRs #9–#16 merged). Two commits are still
  pending on `feature/buddy-walk-bounds` (`d474475`, `4499109`): merge that PR first
  and review the new `master` tip, or include those files and label them "pending
  merge" — do not silently review a stale tree.
- **Deliverable (report only, no code changes)**: `docs/CODE-REVIEW.md`, written in
  the `security-best-practices` report format (executive summary → findings by
  severity, each numbered) with the `code-review-skill` severity vocabulary
  (🔴 blocking / 🟡 important / 🟢 optional, plus 💡 suggestion and 📚 learning).
- Every finding MUST carry: an ID (`CR-nn`), a title, a severity, `file:line`
  evidence, why it matters, a concrete suggested fix, an effort estimate, and
  whether it was **verified** (reproduced) or **read-only judgement**.
- **Nothing is fixed in this exercise.** Fixes are planned from the report
  afterwards.

## Inventory (what "everything" means here)

| Area | Size | What it is |
| --- | --- | --- |
| `api/` (Go) | ~840K, 22 test files | HTTP API for the UI, buddy sender, scheduler, job runner, shares/identity/buddy/talos/notifications clients, `cmd/buddyctl`, `cmd/buddy-receiver` |
| `agent/` (Go) | ~192K, 8 test files | Privileged, host-networked DaemonSet: ZFS (`zpool`/`zfs`/`wipefs`) and share config, plus the streaming backup endpoints |
| `ui/` | 208K src, 64K tests, 9 Playwright specs | Svelte **4** + TypeScript + Tailwind 3 + xterm, static build served by nginx; 5 s dashboard poll |
| `charts/naslos/` | ~820K | Helm chart: api/ui/agent/samba/nfs/terminal/openldap + Traefik & Authelia subcharts, Traefik CRD templates, secrets |
| `openldap/ samba/ nfs/ terminal/` | small | Per-component image (Dockerfile + entrypoint/config templates) |
| `scripts/`, `Makefile`, `bootstrap/` | small | `deploy-vm.sh`, image build/install targets, Talos VM bootstrap |
| `docs/` | ~304K | `spec.md` (normative MUSTs), architecture, api, deployment, operations, per-feature docs, `SECURITY-AUDIT.md`, `SECURITY-FIX-PLAN.md` |
| Tooling | — | **No CI** (`.github` absent), **no linter config** (no golangci-lint, no eslint/prettier), **no `make test`**, **no `.dockerignore` anywhere**; `govulncheck`/`staticcheck` not installed, `npm`/`docker`/`helm`/`kubectl` present |

## Method: seven passes, each governed by named skills

Use the `explore` subagent for inventory-style gathering inside each pass (it
keeps context for the judgement calls), then read the critical files directly. In
every pass: read the code **and** the norm it claims to satisfy (`docs/spec.md`,
`docs/api.md`, the feature doc), and file drift in both directions.

### Pass 0 — Context and evidence base (do this first; it prevents false positives)

Read `docs/spec.md` (MUSTs, §7 test mapping, §8 versioning), `AI_Handoff.md`
(decision log), `docs/SECURITY-AUDIT.md` + `SECURITY-FIX-PLAN.md`,
`docs/architecture.md`, `docs/deployment.md`, `docs/operations.md`.
Produce a **"deliberate decisions" list** to check findings against: single-node
product; NFSv4-only with AUTH_SYS; distroless images; privileged terminal
container; agent `hostNetwork` kept (hostPID removed); in-memory job history;
retention keeps a chain's dependencies; `auth.disabled` documented as dev-only;
no NetworkPolicy because the CNI (flannel) does not enforce it; no TLS inside the
cluster. A "finding" that merely restates one of these is not a finding.

### Pass 1 — Architecture and design

Skills: `code-review-skill` → `reference/architecture-review-guide.md`,
`reference/code-quality-universal.md`.
Targets: component boundaries (`ui → api → agent → host`), the api↔agent contract,
validation duplicated between API and agent (two sources of truth — e.g. pool/disk
rules exist in both, do they agree?), the buddy package as a shared library used by
three binaries, config surface (env vars), error taxonomy, `any`/`interface{}` use,
stringly-typed values, god files (`api/internal/server/server.go`, `buddy_*.go`,
`ui/src/routes/backups/+page.svelte` ~600 lines), coupling to concrete clients
(`*talos.Client`, `*agent.Client` — untestable seams?), SOLID/cohesion, dependency
direction, and whether the API is becoming a god service.
Output: structural findings + a short "shape of the system" summary for the report.

### Pass 2 — Go: `api/`

Skills: `code-review-skill` → `reference/go.md`, `code-quality-universal.md`,
`common-bugs-checklist.md`, `cross-cutting/error-handling-principles.md`,
`cross-cutting/async-concurrency-patterns.md`.
Targets:
- **Concurrency**: the job manager, scheduler, metrics collector, websocket
  handling. Goroutine leaks, shutdown paths, mutex discipline and lock ordering
  (`buddyJob.mu`, `Store.mu`, `PeerStore.mu`, `Auth.mu`, `Manager.mu`), data races,
  `context` propagation and cancellation, and whether the tests would catch a race
  (`go test -race` is not run anywhere).
- **Timeouts that bound streams** (verify, don't assume): `api/internal/agent/client.go`
  `DefaultTimeout` and `api/internal/buddy/client.go` `http.Client{Timeout: 5m}`
  apply to the *whole* request including a multi-gigabyte `zfs send`/`receive`
  stream — a long backup or restore would be killed mid-stream. Confirm the exact
  effect and file it.
- **HTTP layer**: method/limit checks, JSON decoding strictness, error→status
  mapping, body limits, pagination/DoS caps, response-shape stability vs
  `docs/api.md`.
- **Auth/authorization**: the middleware (fail-closed, constant-time), the route
  composition (owner vs public), and whether *any* authenticated user is an admin
  (is `Remote-Groups` / `naslos_admins` enforced anywhere, or is the identity
  header treated as "the operator"?).
- **Resource handling**: file handles (store temp files, the `.nonces` append),
  unclosed bodies, ignored errors (`_ =`, silent `continue`), atomic-write
  correctness.
- **Input validation**: coverage after NAS-003 (every handler, every field, numeric
  bounds, path handling), and the symlink/`filepath.Join` question for peer input.
- **Tests**: what the suite would *not* catch (no `-race`, no fuzzing, no coverage
  target, live-VM-dependent E2E, `httptest` stubs that encode the same assumptions
  as the code).

### Pass 3 — Go: `agent/`

Skills: same Go set, plus the privilege question.
Targets: the privileged boundary (root, `chroot /host`, `hostExec` argv
construction — any remaining interpolation from caller input?), ZFS command
construction, `wipefs` gating, path containment for share folders
(`agent/internal/shares/folders.go`), error classification
(`writeClientError`), secret redaction in logs (command lines, LDAP bind password),
what `/health` and `/api/v1/*` expose, and whether the agent trusts the API more
than it must (defense in depth on a privileged service).

### Pass 4 — UI: Svelte/TypeScript

Skills: `code-review-skill` → `reference/svelte.md`, `reference/typescript.md`,
`reference/css-less-sass.md`; the **`web-design-guidelines`** skill for the UI/UX
and accessibility review; **`webapp-testing`** to reproduce UI findings in the
browser.
Targets: Svelte **4** idioms (the skill's guide leans Svelte 5 — judge against 4
and only note a migration), reactivity correctness, `onDestroy`/interval cleanup
(the 5 s dashboard poll, the backups job poll: does polling stop on navigation?),
component size and duplication, per-component API type interfaces that drift from
the API (the `bytes`/`storedBytes` bug was exactly this), `any` casts, fetch error
and abort handling, `{@html}` usage, focus management in modals (focus trap, Escape,
restore focus), `aria-live` for polled updates, contrast/focus-visible/reduced
motion, and the deprecated `xterm`/`xterm-addon-fit` packages (renamed `@xterm/*`).

### Pass 5 — Chart, Kubernetes, images, scripts, operations

Skills: **`docker-expert`** (Dockerfiles) and **`devops-troubleshooter`**
(observability, failure modes, runbooks).
Targets:
- Helm: values schema (`values.schema.json` absent), required values, secret
  handling (generated vs operator-supplied; is any real secret in `values-vm.yaml`
  or in git?), probes (readiness vs liveness semantics), resources/limits,
  securityContexts, RBAC breadth (the API's cluster role), the
  `ingress.enabled` vs subchart condition trap (fixed — verify), the
  `naslos-internal-auth` rotation story, upgrade/rollback path, and what a
  `helm upgrade` silently keeps (digest vs tag interaction).
- Dockerfiles: base images and pinning, multi-stage, non-root users, image size,
  build args, secrets in layers, healthchecks, and the **missing `.dockerignore`**
  (the context currently includes `ui/node_modules` ≈104 MB, `.git`, `bin/`).
- `scripts/deploy-vm.sh` and `openldap/generate-secrets.sh`: `set -euo pipefail`,
  quoting, idempotency, retries, root/sudo use, secret generation and storage.
- Observability and failure modes: is anything scraping the API (Prometheus is
  deployed — what does it actually scrape?), what alerts exist, what a
  half-failed backup looks like to an operator, log volume/rotation, and the
  **recoverability of the buddy identity + KEK** (it lives on a PVC; losing it
  makes every stored backup unreadable — is that documented, is it exportable,
  is it backed up?).

### Pass 6 — Security design

Skills: `code-review-skill` → `reference/security-review-guide.md`, plus the
**`security-best-practices`** skill for its process and report format.
*Note:* that skill ships **no language reference files** in this environment
(`references/` is absent), so pair it with the review skill's security guide and
first principles, and say so in the report rather than implying framework-specific
guidance was followed.
Targets: threat model by reachability (LAN, UI NodePort, in-cluster, authorized
peer); trust boundaries (proxy secret, agent token, peer Ed25519 signatures,
enrollment token, LDAP, SMB/NFS); crypto review of the buddy envelope (what exactly
is signed, nonce/timestamp/canonicalisation, AES-GCM usage and nonce derivation,
DEK wrapping, KEK at rest); credential handling (NT hashes in `smbusers.json` are
password-equivalent — file mode, exposure, necessity; LDAP bind password; secrets
in logs); SSRF (the owner-supplied receiver URL is fetched by the API); DoS bounds;
authorization granularity; secret scanning of the working tree (and the caveat that
history was not scanned); and a check that each of the 22 NAS findings is either
fixed with a test or explicitly accepted.

### Pass 7 — Cross-cutting hygiene and documentation drift

Targets: missing CI (nothing builds or tests on push), missing linters, no
`make test`, no coverage measurement, no `-race` usage, Playwright's dependence on
a live VM (not hermetic), docs↔code drift both ways (MUSTs in `spec.md` with no
implementation or no test, and behaviour with no MUST), dependency health
(Svelte 4 vs 5, deprecated xterm, k8s client-go vs Talos machinery skew), repo
hygiene (`.kilo/` 3.4 MB of plans in the repo, the tracked
`support-naslos-vm.zip.age`, no `.dockerignore`), and the spec's §7 test-mapping
table being complete.

## `docs/CODE-REVIEW.md` structure

1. **Executive summary** — baseline commit, what was reviewed, method, headline
   findings, and an explicit statement of what was *not* reviewed.
2. **Scope and method** — the passes above, the skills consulted, and a table of
   what was executed (builds, tests, `-race`, linters, `npm audit`, `helm lint`,
   live probes) vs read.
3. **Findings** — grouped by severity, `CR-01…`, each with evidence, impact,
   suggested fix, effort, verified/read-only.
4. **Engineering hygiene gaps** — CI, linters, `make test`, `.dockerignore`,
   dependency deprecations.
5. **Deliberate decisions reviewed and accepted** — the Pass 0 list with a verdict.
6. **Previous audit status** — the 22 NAS findings: fixed + test / accepted + why /
   still open.
7. **Recommended fix order** — the short list worth fixing first, split into
   safe-and-quick vs structural, with the risk of each.
8. **Appendix** — commands run, raw evidence, and gaps in coverage with reasons.

## Validation of the review itself

- Every 🔴 and 🟡 finding is reproduced (a failing test, a command, or a code path
  traced to a concrete failure) or explicitly labelled read-only judgement. A
  finding without `file:line`, or one that restates a deliberate decision, is
  removed.
- Run the existing gates before judging: `api`/`agent` `go build ./... && go vet
  ./... && go test ./...`, plus `go test -race ./...`; `ui` `npm run check` and
  `npx playwright test` against the VM; `helm lint` and `helm template` for both
  value sets.
- Add the checks the repo lacks, **as evidence rather than as fixes**: install or
  `go run` `govulncheck` and `staticcheck`, run `npm audit`, and try
  `golangci-lint` if it can be fetched — recording results in the report even if
  we do not adopt the tools yet.
- Probe the live VM read-only where a finding depends on runtime behaviour
  (`kubectl get`, `describe`, API GETs, `webapp-testing` for UI findings). Do not
  mutate the cluster or the receive dataset during the review.
- Re-read the report against Pass 0 before closing: no re-litigating documented
  decisions, no style nits a linter should own (recommend the linter instead).

## Known candidates to confirm (so they are not missed)

These were observed earlier in the project but are **not** in `AI_Handoff.md`; the
review must verify and file them if they hold:
1. **Streaming timeouts** in `api/internal/agent/client.go` and
   `api/internal/buddy/client.go` bound whole requests, so a large backup/restore
   cannot finish.
2. **The buddy identity (KEK + private key) is not recoverable**: it lives on the
   shares-config PVC; losing that file makes every stored backup unreadable. Check
   whether that is documented, exportable, or backed up.
3. **Any authenticated user is an administrator** if nothing enforces
   `Remote-Groups`/`naslos_admins`.
4. **No `.dockerignore`** → ~104 MB `node_modules`, `.git` and build artefacts go
   into the image build context.
5. **No CI, no linters, no `make test`** — quality depends on manual discipline.

## Non-goals

Fixing anything (report only); re-deriving the security audit's findings; style
nits a linter should own; proposing multi-node; migrating to Svelte 5 (note only);
rewriting working subsystems to a different pattern; scanning git history for
secrets (noted as a gap instead); `cline-sdk` and `kilo-config` skills do not apply
to this repository (different product / repo-configuration concerns) — say so
rather than forcing them.

## Sequencing

Pass 0 → 1 → 2 → 3 → 4 → 5 → 6 → 7 → write `docs/CODE-REVIEW.md` → validate its
claims → record the outcome in `AI_Handoff.md` on a review branch (not `master`,
since the deliverable is a document). Roughly: context 10 %, Go 35 %, UI 15 %,
chart/ops 15 %, security 15 %, writing and validation 10 %.

## Risks

- **Context budget** on a ~1 500-line-of-code-per-component review: use `explore`
  subagents for gathering, read only what a pass needs, and keep the report's
  evidence terse.
- **False positives from a codebase with deliberate, documented trade-offs** — Pass 0
  exists to prevent this; the report must distinguish "wrong" from "chosen".
- **The VM may be down or changed**: mark VM-dependent findings as unverified live
  rather than guessing.
- **Scope creep into fixing**: the deliverable is a report; fixes get their own plan.
