# Naslos — audit and fix report (2026-09-19)

This is the consolidated record of the audit and the remediation, and the only
current audit document. The detailed working papers are archived under
`docs/archive/`: `AUDIT-2026-09-19.md` (findings), `AUDIT-2026-09-19-FIXPLAN.md`
(batches and progress), `CODE-REVIEW.md` (per-finding review list) and the
superseded `SECURITY-AUDIT.md` + `SECURITY-FIX-PLAN.md` (2026-09-14).

- **Audit baseline:** `master` = `85ae874` (PR #23; the dev endpoint had just
  been removed). The 2026-09-14 audit is superseded and archived.
- **Fix branch:** `audit/full-2026-09-19`, 16 commits `1e4ae2b` → `d2f891a`,
  pushed. `master` was never pushed to directly.
- **Live target:** `192.168.1.117`, helm revision **24** — `naslos-api`
  `0.1.0-r6`, `naslos-agent` `0.1.0-r3`, `naslos-ui` `0.1.0-r6`, Traefik
  hostPort 80/443, Traefik v3.7.13 (chart 41.6.0) and Authelia 4.39.24 (chart
  0.11.22), `naslos.local`.
- **No secret values appear in this report or in any committed artefact.**

## 1. Executive summary

A full audit of the mainline covered code quality, security and secret use, using
static review plus `gitleaks`, `trufflehog`, `govulncheck`, `gosec`, `npm audit`
and read-only live inspection. It found **0 Critical, 4 High, 14 Medium, 10 Low**.

Remediation then closed **all four Highs and every Medium except four**:

| Status at 2026-09-19 | Count |
|---|---|
| High closed | 4 of 4 |
| Medium closed / no-change-needed / excluded | 8 + 1 + 1 |
| Medium deferred (with reason) | 4 |
| Low closed / accepted | 5 + 1 |
| Low deferred | 3 (L2, L3, L8) |
| Findings not yet run (Batch 6) | image/SBOM scan, SAST breadth, 8 active tests, buddy crypto deep-dive, `.118` |

The instance now has a single authenticated entry point, no default or committed
credentials, verified LDAP TLS, backups on their own dataset, a persisted session
secret, and a runnable (if not yet wired) audit sweep.

## 2. Method

- **Static review** with the `code-review-skill` references (Go, Svelte,
  Security, Architecture, Universal quality) and `security-best-practices`.
- **Scanners**, none of which were installed, run via Docker or `go run`:
  `gitleaks detect` (full history + tree), `trufflehog git --only-verified`,
  `go run golang.org/x/vuln/cmd/govulncheck@latest`, `go run
  github.com/securego/gosec/v2/cmd/gosec@latest`, `npm audit` (prod and full).
- **Live inspection**, read-only: services, RBAC, PSA labels, Secrets (names
  only), pod specs, rendered args, and targeted probes (`curl`, `ldapwhoami`).
- **Constraints honoured:** non-destructive; no credential values printed or
  committed; active tests bounded and cleaned up; the repository is private
  (unauthenticated GitHub API returns 404), so the committed credentials found
  were insider-exposure, not internet-exposure.

## 3. Findings and status

### High

| ID | Finding | Status | Evidence |
|---|---|---|---|
| AUDIT-H1 | Live LDAP **service** credential committed in `values-vm.yaml` and deployed | **Fixed** — `4e47884`, rev 22 | Authelia reads it from the `naslos-openldap` Secret via `lookup`; new value binds, old rejected, suite 35 passed |
| AUDIT-H2 | Buddy receive path misconfigured; chunks on the 64 MiB shares PVC | **Fixed** — `d4e068f`, rev 21 | `buddy.enabled=true` + `receiveHostPath=/var/mnt/test/naslos-buddy`; chunks on the dataset (96K), backups suite green |
| AUDIT-H3 | 18 reachable Go vulnerabilities (helm, containerd, docker, spdystream, x/crypto) | **Fixed** — `23bca3a`, api `0.1.0-r5` | govulncheck 18 → 4, all `Fixed in: N/A` and not on an exercised path |
| AUDIT-H4 | Committed default Grafana admin password | **Fixed by removal** — `6b25c1a`, rev 17 | No `naslos-grafana` workload/service/SA/secret/ClusterRoleBinding |

### Medium

| ID | Finding | Status | Where |
|---|---|---|---|
| M1 | Talos machine configs + `talosconfig` (private keys) in local `refs/cline/checkpoints/*` | **Fixed** — `421711a` | 104 refs deleted + gc; gitleaks 89 → 4 (documented false positives); never on a branch or remote |
| M2 | Local secret files world-readable | **Fixed** — `421711a` | `bootstrap/vm/*` and the captured session `chmod 600`; `auth.setup.ts` enforces 0600 itself |
| M3 | `jwt_secret` regenerated every render; secrets in a ConfigMap | **Partial** — `421711a` | Persisted in `naslos-authelia-jwt` (proven across an upgrade). Residual: Authelia's config is a ConfigMap, so its `jwt_secret`/LDAP password are readable there |
| M4 | No NetworkPolicy; flannel does not enforce one; privileged hostNetwork agent `:9090` open to every pod | **Open** | Needs a policy CNI or host firewall — its own window |
| M5 | Unused agent ClusterRole (nodes/pods/pods-log cluster-wide) | **Fixed** — `8555925` | ClusterRole + binding removed; the agent has no client-go |
| M6 | Namespace PSA `enforce: privileged` cluster-wide | **Open** | Scope per-workload or split namespaces |
| M7 | API server had no `ReadHeaderTimeout` (Slowloris) | **Fixed** — `3b2a65b` | ReadHeaderTimeout 10 s, ReadTimeout 30 s, IdleTimeout 120 s; `WriteTimeout` 0 for WS |
| M8 | Config files carrying secrets were `0644` in `0755` dirs | **Fixed** — `3b2a65b` | `notifications.json` (ntfy token), peers, schedules, shares → 0600 in 0750 |
| M9 | Buddy store path-taint reports | **Fixed** — `3b2a65b` | One `validateChainName` allowlist + traversal regression test |
| M10 | Mutable image tags, `IfNotPresent`, no digests, plain-HTTP registry | **Excluded** at the operator's request | — |
| M11 | 11 dev-only npm advisories (Svelte/Vite/cookie) | **Open** | Build-chain upgrade, needs a tested bump |
| M12 | No CI | **Partial** — `421711a`, workflow removed `b1f7365` | `scripts/audit.sh` packages the sweep; nothing runs it automatically yet |
| M13 | Dead `storage.zfsLocalPV` config | **Fixed** — `8555925` | Block removed; local-path is the only provisioner |
| M14 | Stale audit docs; no lockout runbook | **Fixed** — `421711a`, `65d9b6d` | Superseded banner, status tables, operator-lockout runbook |

### Low

| ID | Finding | Status |
|---|---|---|
| L1 | No `.dockerignore` | **Fixed** — `8555925` (UI context 96 MB → API 717 kB) |
| L2 | UI nginx runs root, writable rootfs | **Open** |
| L3 | privileged/hostPath justification only in comments | **Open** (documented) |
| L4 | Swallowed apply errors | **No change needed** — `applySharesConfig` already logs (`shares.go:370`) |
| L5 | Log-injection findings | **Fixed** — `internal/logsafe.Field` sanitises every request/peer-derived value before it reaches a log line (rev 27) |
| L6 | Integer-conversion findings | **Fixed** — buddy free-space clamps instead of overflowing, `SealChunk` bounds the length field, tar modes are masked (rev 27) |
| L7 | `md4` for the Samba NT hash | **Accepted** — protocol requirement |
| L8 | `go test -race` fails (CR-06) | **Open** — CI keeps it opt-in |
| L9 | CR-38 remainder | **Fixed** — `LDAPTLS_REQCERT=never` replaced with CA verification (`d2f891a`, rev 24); all 44 `.Values.namespace` references migrated to `.Release.Namespace`, the value removed, and `values.schema.json` added (rev 25) |
| L10 | gitleaks false positives | **Informational** |

### NAS-010 (default credentials) — **closed**

Beyond the audit's own IDs, the older 2026-09-14 audit's NAS-010 was completed:

- `generate-secrets.sh` now generates random admin/service passwords instead of
  `naslos-admin` / `CHANGE_ME_SERVICE_PASSWORD` (`7a9eb3a`).
- The OpenLDAP entrypoint requires `LDAP_ADMIN_PASSWORD` (no `admin` fallback).
- New users get an unguessable random placeholder instead of `TempPass123!`, and
  `POST /api/users` rejects a passwordless create with 400 (verified).
- `deploy-vm.sh` requires `REGISTRY_HTTP_SECRET` (no `secret` fallback).
- The **running** directory's admin password was rotated too: `olcRootPW`
  replaced in both `cn=config` databases over `ldapi://`/SASL EXTERNAL, new value
  in the Secret, bootstrap re-run green (`71c6d5a`).

## 4. Security fixes — detail

**Authenticated-only posture (pre-audit, `9123f7a`).** Removed the UI NodePort
and the `auth.disabled` / `AUTH_DISABLED` / `AGENT_AUTH_DISABLED` bypasses; the
API and agent refuse to start without their credential. `values-prod.yaml` and
`install-prod` folded into `values-vm.yaml`.

**H1 — LDAP service credential.** The Authelia config now takes the password from
the `naslos-openldap` Secret (`lookup`), `values` default to empty with a
fail-closed message, and the VM profile carries none. Rotated live (Secret patch
+ bootstrap re-run) and verified both ways.

**H2 — backup durability.** `values-vm.yaml` sets `buddy.enabled=true` and
`receiveHostPath=/var/mnt/test/naslos-buddy`; the API's init container chowns the
dataset to 65532. Chunks verified on `test/naslos-buddy`, not the shares PVC.

**H3 — dependencies.** `helm.sh/helm/v3` → v3.18.5 (k8s 0.33.3), containerd
v1.7.35, `moby/spdystream` v0.5.1, `oras.land/oras-go/v2` v2.6.1. Remaining four
are `Fixed in: N/A` (x/crypto openpgp reached only through Helm provenance init /
`ssh.ParseAuthorizedKey`; three containerd CRI checkpoint issues absent from the
pull path).

**H4 — Grafana.** Disabled (`grafana.enabled: false`) and the literal
`adminPassword` deleted; the workload, Service, SA, Secret and ClusterRoleBinding
are gone from the cluster.

**M1/M2 — secret hygiene.** Talos PKI and the old `talosconfig` existed only in
local Cline/Kilo checkpoint refs: deleted and garbage-collected, after confirming
no branch or remote ever carried them (the current `talosconfig` blob differs).
Local secret files are 0600.

**M3 — session stability.** `jwt_secret` is generated once into
`naslos-authelia-jwt` and read back with `lookup`; an upgrade no longer
invalidates every session (verified by comparing the value across a full
upgrade).

**M5/M7/M8/M9/M13 — chart and API hardening.** Unused agent RBAC dropped; API
read timeouts added; secret-bearing files 0600; chain ids validated with a single
allowlist; dead ZFS LocalPV config removed.

**L9 — LDAP TLS and chart portability.** The API `wait-for-ldap` init and both
bootstrap containers now mount the `naslos-openldap-tls` CA and set
`LDAPTLS_CACERT`; `REQCERT=never` only remains when no CA secret is configured.
Verified: init logs "OpenLDAP is reachable." and a full bootstrap completes. The
44 `.Values.namespace` references were also migrated to `.Release.Namespace`
(the value removed), so the chart installs under any release namespace — a
render with `-n other` produces `other.svc.cluster.local` everywhere and no
`naslos.` DNS — and `charts/naslos/values.schema.json` now type-checks the
chart's own values (without blocking subchart keys). Live at revision 25: E2E
35 passed.

**NAS-010 — live rotation.** Service password (rev 22) and admin `olcRootPW`
(rev 23+) rotated; old values rejected by `ldapwhoami`.

## 5. Code and correctness fixes

From the pre-audit correctness batch (commit `261aa3e`, PR #21) and the audit:

- **ntfy token no longer returned** by `GET /api/notifications` (`hasAuthToken`
  instead); PUT keeps/clears/replaces via a pointer field.
- **NSS `shadow` mirror 0600**, re-applied on content-identical upgrades.
- **User group edits apply** (`PUT /api/users/{uid}` deltas via a shared
  `membershipDelta`, validated names, mirror pushed only on change).
- **UI silent failures removed**: `res.ok`/`Array.isArray` checks and surfaced
  errors across the pages.
- **Agent streaming token**: `SendStream`/`ReceiveStream` shared a package-level
  client without the auth transport, so every backup 401'd under auth;
  `client_test.go` pins both clients.
- **Password change was a silent no-op** in the edit form (the PUT ignores it);
  it now calls `POST /api/users/{uid}/password`.
- **User creation requires a password**; the placeholder is random.
- **Slowloris bounds, 0600 config modes, buddy chain-name validation** (M7/M8/M9).
- **`.dockerignore`** added; UI build context dropped from ~96 MB to 340 kB.

## 6. Operations and infrastructure

- Dev endpoint and its whole posture removed; single authenticated entry point.
- Grafana removed; Prometheus + Alertmanager remain.
- Buddy receive dataset mounted and verified.
- CI sweep written (`scripts/audit.sh`); the GitHub Actions workflow was added
  and then removed at the operator's request, so the sweep is manual for now.
- **Log safety and conversions (rev 27):** new `api/internal/logsafe` package
  (`Field`, with tests) strips control characters and caps length; every
  request/peer-derived value in a `log.Printf` now goes through it, and gosec
  G706 is annotated where the sanitiser is applied (gosec cannot see across the
  call). The five G115 conversions are gone: buddy free space clamps instead of
  overflowing `int64`, `SealChunk` refuses a plaintext larger than the uint32
  length field, `chunkNonce` is documented as bounded by `validateChunkIndex`,
  and the tar extraction masks to `0777` before narrowing. `gosec
  -include=G706,G115` is clean.
- **Subchart upgrades (2026-09-19, rev 26):** Traefik chart 39.0.0 → **41.6.0**
  (app **v3.7.13**) and Authelia chart 0.10.0 → **0.11.22** (app **4.39.24**).
  The tightened schemas forced two value migrations: Traefik's logging key
  `logs` → `log`, and our custom `authelia.domain`/`authelia.server` moved out of
  the subchart's namespace to a top-level `domain` (the 0.11 schema rejects
  unknown keys under `authelia`). Authelia migrated its SQLite schema (20 → 29)
  on start. The Playwright setup needed one selector fix: Authelia now labels the
  six OTP boxes "Enter One-Time Password" instead of "Digit N". Verified: both
  pods on the new versions, anonymous `/api/*` → 302, and the suite 35 passed.
- Docs reconciled: the archived `CODE-REVIEW.md` and `SECURITY-AUDIT.md` carry
  dated status tables and `SECURITY-FIX-PLAN.md` is marked superseded; live docs
  no longer describe the NodePort, `auth.disabled`, Grafana or `zfsLocalPV`.
  The working papers now live under `docs/archive/`, leaving this report as the
  single current audit document.

## 7. Verification

| Gate | Result |
|---|---|
| `go build`/`go vet`/`go test` (api) | clean, including `client_test.go`, `TestGroupDelta`, `TestNotificationsNeverReturnTheAuthToken`, the chain-name test |
| `go build`/`go vet`/`go test` (agent) | clean, including the shadow-mode test |
| `svelte-check` | 0 errors, 0 warnings |
| `helm lint` (+ `helm template`) | clean (rendering needs `--set openldap.bindPassword=…` only because the chart now fails closed without the Secret) |
| Playwright vs `https://naslos.local` | **35 passed** (repeatedly; Authelia login + 2FA, terminal interactive, backups self-send, users/groups/shares/pools) |
| Live unauthenticated | `/api/*` → 302 to the portal; `/api/health` 200; `:30080` refuses; pod without the proxy secret 401; agent without the token 401 |
| Credentials | new LDAP service and admin bind; old service value and `naslos-admin` rejected |
| Backups | chunks on `test/naslos-buddy`; share PVC unaffected |

## 8. Remaining work

| Item | Severity | Why deferred | Next step |
|---|---|---|---|
| AUDIT-M4 — no NetworkPolicy / unenforced CNI | Medium | Replacing the CNI or adding host firewall rules on a single-node appliance needs a window | Install Cilium/Calico or a host rule limiting API→agent `:9090`; then NetworkPolicies |
| AUDIT-M6 — PSA `privileged` namespace | Medium | Changes scheduling/security context of live workloads | Split namespaces or label only agent/terminal privileged |
| AUDIT-M11 — Svelte/Vite dev advisories | Medium | Breaking major bumps; needs a tested UI upgrade | Bump and re-run `npm audit` + the suite (CR-08/CR-31) |
| AUDIT-M3 residual — Authelia config in a ConfigMap | Medium | Requires subchart support to mount a Secret-based config | Move `configuration.yml` to a Secret |
| AUDIT-L2 — nginx non-root | Low | Image change + rebuild; watch file-permission needs | Non-root user, read-only rootfs, tmpfs cache |
| ~~AUDIT-L9 remainder~~ — **fixed at revision 25**: `.Release.Namespace` migration + `values.schema.json` | Low | — | — |
| ~~AUDIT-L5/L6~~ — **fixed at revision 27**: `logsafe.Field` sanitises log arguments and the conversions are bounded/clamped | Low | — | — |
| AUDIT-L8 / CR-06 — `go test -race` | Low | Known failing scheduler race | Fix the race; enable `NASLOS_AUDIT_RACE=1` in CI |
| Batch 6 — image/SBOM scan, semgrep/staticcheck, AV-5…AV-12, buddy crypto deep-dive, `.118` | Coverage | Time-boxed session | Run `trivy`, `semgrep`, `staticcheck`, the bounded active tests, and the `.118` read-only checks |
| AUDIT-M10 — digests / registry TLS | Medium | Excluded by request | Revisit when wanted |

## 9. Reproducing the audit and fixes

```bash
# audit sweep (tools installed separately; skips what is missing)
sh scripts/audit.sh

# full sweep by hand
docker run --rm -v "$PWD":/repo zricethezav/gitleaks:latest detect --source=/repo --redact
(cd api && go run golang.org/x/vuln/cmd/govulncheck@latest ./...)
(cd api && go run github.com/securego/gosec/v2/cmd/gosec@latest -quiet ./...)
(cd ui && npm audit --omit=dev && npm audit)

# the authenticated end-to-end suite (credentials in ui/.env.playwright.local)
PLAYWRIGHT_BASE_URL=https://naslos.local \
NASLOS_RECEIVER_URL=http://naslos-api.naslos.svc.cluster.local:8080 \
  npx playwright test
```

## 10. Timeline

| Commit | Change | Live revision |
|---|---|---|
| `9123f7a` | Remove the dev endpoint (authenticated-only) | 16 |
| `1e4ae2b` | Audit report + fix plan | — |
| `6b25c1a` | Grafana removed | 17 |
| `23bca3a` | Dependencies (18 → 4 reachable) | 18 |
| `3b2a65b` | API timeouts, 0600 config modes, chain validation | 18 |
| `8555925` | Agent RBAC, dead ZFS config, `.dockerignore` | 18 |
| `421711a` | jwt persistence, PKI refs pruned, audit sweep, docs | 19–20 |
| `f63cdd4`, `65d9b6d` | Progress + doc reconciliation | — |
| `b1f7365` | CI workflow removed (operator request) | — |
| `d4e068f` | Buddy receive dataset | 21 |
| `4e47884` | LDAP service credential rotated/de-committed | 22 |
| `7a9eb3a` | NAS-010 defaults removed (code) | 23 |
| `71c6d5a` | LDAP admin password rotated (live) | 23 |
| `d2f891a` | LDAP TLS verification (L9 part) | 24 |
