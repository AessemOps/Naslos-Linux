# Reconcile stale audit/review docs with master

The two historical reports still read as current, and several live docs still
document things that were removed (`AUTH_DISABLED`, the UI NodePort,
`auth.disabled`, `values-prod.yaml`, Grafana, `zfsLocalPV`, the old
`naslos-internal-auth` Secret name). This plan marks the reports as historical
and fixes the live docs. Baseline: `master` `85ae874`.

No behaviour changes — documentation only. One branch, one commit per group,
pushed for review (never master).

## 1. `docs/CODE-REVIEW.md` — mark the 2026-09-17 body historical

Insert a **"Status at 2026-09-19"** block right after the header (before the
severity policy, line 7) containing:

- The baseline moved `6227595` → `85ae874` (PRs #21–#23): the dev endpoint
  (NodePort, `auth.disabled`, `AUTH_DISABLED`/`AGENT_AUTH_DISABLED`,
  `values-prod.yaml`) was removed, Grafana was removed, and a separate audit ran
  (`docs/AUDIT-2026-09-19.md` + `-FIXPLAN.md`).
- A sentence that the findings from §2 onward describe the old baseline, so line
  numbers and posture statements are historical; anything mentioning a NodePort
  or `auth.disabled` describes a configuration that no longer exists.
- A status table:

| CR | Status |
|---|---|
| CR-01 Authelia bypass, CR-02 uninstall data loss, CR-03 `RequireAdmin` | **Fixed** (PR #20) |
| CR-05 dependency exposure | **Fixed** (`23bca3a`; govulncheck 18 → 4, all `Fixed in: N/A`, unexercised) |
| CR-06 `-race` | **Open** (CI opt-in `NASLOS_AUDIT_RACE=1`) |
| CR-07 ntfy token, CR-15 shadow mirror, CR-18/19/22 UI silent failures | **Fixed** (`261aa3e`) |
| CR-09 no CI | **Fixed** (`421711a`) |
| CR-10 `.dockerignore` | **Fixed** (`8555925`) |
| CR-31 xterm, CR-32–36 UI typing/size/a11y/timers, CR-41 monitoring, CR-42 spec §7, CR-44 doc drift, CR-45 unused logger | **Open** |
| CR-37 RBAC/PSA/NetworkPolicy/probes | **Partial**: unused agent ClusterRole removed (`8555925`); probes/PDBs/NetworkPolicy/PSA open (AUDIT-M4/M6) |
| CR-38 Helm hardening | **Partial**: `trustForwardHeader` removed; schema, `.Release.Namespace`, `LDAPTLS_REQCERT`, digests open (AUDIT-L9/M10) |
| CR-39 jwt churn / argv secrets | **Partial**: jwt persisted (`421711a`); the live LDAP credential is AUDIT-H1 |
| CR-40 image pinning / root nginx | **Partial**: digests excluded by request (M10); nginx non-root open (L2) |
| CR-43 missing tests | **Partial**: `api/internal/agent/client_test.go`, auth-aware E2E |

- Point the executive summary's three "blocking" bullets at the table so a reader
  is not misled before scrolling.

## 2. `docs/SECURITY-AUDIT.md` — status for the headline findings

Keep the existing superseded banner; add a compact status table after it:

| NAS | Status at 2026-09-19 |
|---|---|
| NAS-001 API auth dead code | **Fixed** — owner gate wired; `TestOwnerRoutesRequireAuth` covers it |
| NAS-002 unauthenticated agent | **Fixed** — bearer token required, constant-time; `client_test.go` pins it (and found the streaming-client gap) |
| NAS-003 agent validation gaps | **Partial** — escaping/validation batches landed; CR-27/28/29 remain |
| NAS-004 terminal unauthenticated, NAS-005 `/api/ws/logs` | **Fixed** — owner gate; the nginx 403 path is gone with the NodePort |
| NAS-008 NodePort exposure | **Fixed** — listener and the whole dev posture removed |
| NAS-009 `X-Forwarded-Host` trust | **Fixed** — `trustForwardHeader` removed |
| NAS-010 default credentials | **Partial** — Grafana removed; the LDAP service password, `TempPass123!` initial passwords and the registry `secret` remain (AUDIT-H1/M14) |
| NAS-014/017 | **Fixed** — jwt secret persisted; streaming client token |
| others | historical; see `docs/CODE-REVIEW.md` status table |

## 3. `docs/SECURITY-FIX-PLAN.md` — banner

It is a completed plan whose central mechanism (`AUTH_DISABLED`, the dev opt-out)
no longer exists. Add: "Completed then superseded on 2026-09-19: the
`AUTH_DISABLED` opt-out and the NodePort it relies on were removed, and the
remaining work moved to `docs/AUDIT-2026-09-19-FIXPLAN.md`. Kept for history."

## 4. Live docs that document removed things

| File:line | Stale content | Change |
|---|---|---|
| `docs/api.md:225,228` | `AUTH_DISABLED`, `AGENT_AUTH_DISABLED` rows | delete both rows |
| `docs/api.md:226,227` | "required unless AUTH_DISABLED=true", Secret `naslos-internal-auth` | "required"; Secret `naslos-proxy` (API) / `naslos-agent` (agent token) |
| `docs/api.md:222` | `BUDDY_RECEIVE_PATH` default presented as the deployed one | note it is unset in the live release, so chunks land on the shares PVC (AUDIT-H2) |
| `docs/terminal.md:60` | nginx "refuses these paths outright" | the refusal block was removed; `/api` is routed straight to the API behind Authelia |
| `docs/terminal.md:61` | Secret `naslos-internal-auth` | `naslos-proxy` |
| `docs/terminal.md:62` | "refuses to start without PROXY_SHARED_SECRET unless auth.disabled" | "refuses to start without it; there is no opt-out" |
| `docs/terminal.md:75-98` | "Current state: no IngressRoute, node port is the only way" + the `--set auth.disabled=true` option | rewrite: the ingress/portal posture is the only one; delete option 2 |
| `docs/monitoring.md:46-61` | `grafana:` block with `adminPassword` | delete; add "Grafana is not deployed (`grafana.enabled: false`)" |
| `docs/deployment.md:220-221` | "default passwords unchanged (grafana.adminPassword…, openldap.bindPassword…)" | Grafana removed; the LDAP service credential is AUDIT-H1 and must be rotated |
| `docs/deployment.md:396,405` | `grafana.*` rows in the values/rotation tables | remove/replace |
| `docs/storage-zfs.md:161-167` | `zfsLocalPV` values block | remove; note local-path only (AUDIT-M13) |
| `docs/operations.md:81` | "NodePort or a port-forward" | the portal, or a port-forward for cluster access |
| `docs/shares.md:225` | "the UI's NodePort" | the UI's ingress listener |
| `docs/buddy-backup.md:242` | "through the NodePort or ingress you already expose" | "through the ingress" |
| `docs/architecture.md:107` | Grafana in the diagram | remove the box (and fix alignment) |
| `docs/spec.md:668` | "through the UI's NodePort" | mark as a historical note, or reword to the ingress |

Also sweep for every remaining occurrence of `naslos-internal-auth` (the Secret
is `naslos-proxy` / `naslos-agent` now) and fix each outside `docs/archive/`.
`docs/archive/*` is explicitly historical: leave it.

## 5. Verification

- `grep -rn "AUTH_DISABLED\|AGENT_AUTH_DISABLED\|auth\.disabled\|naslos-internal-auth\|nodePort\|NodePort\|values-prod\|install-prod" docs/` returns only `docs/archive/**`, the new "historical" status text, and the audit/fix-plan files (where the removal is the subject).
- `grep -rn "grafana" docs/` returns only the "not deployed" note and the audit history.
- `grep -rn "zfsLocalPV" docs/` returns only the audit/fix-plan mention.
- No code or chart change, so no build/deploy; `scripts/audit.sh` is unaffected.

## Out of scope

- Rewriting every finding in `CODE-REVIEW.md`/`SECURITY-AUDIT.md`; the status
  tables are the source of truth and the bodies stay as the record of that date.
- `docs/archive/**`.
