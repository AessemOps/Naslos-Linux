# Full security & best-practice audit of master — post-Rust migration

## Goal

Audit `master` end to end for security and best practices, fix **every** finding,
prove each fix with a test and a controlled live check, and ship the result as a
release. The previous full audit (2026-09-19, remediation 2026-09-24) was written
against the Go API/agent; the entire API, agent and buddy tooling were rewritten
in Rust (Phases 1–3), so this audit must both re-verify the old fixes against the
new code and cover the new attack surface (CLI adapters, PTY exec, Kubernetes
writes, chart RBAC, image changes).

## Locked decisions (from the user)

1. **Full remediation** — fix all severities; anything not fixed must be recorded
   as accepted risk with an explicit sign-off, never silently dropped.
2. **Controlled live testing** on the VM (192.168.1.117): auth/CIDR/secret
   spoofing, WS/exec gating, NetworkPolicy probes, TLS checks — no pool/dataset
   wipe, no destructive data tests. Take a pre-test backup/snapshot and keep a
   reinstall rollback (installer pack v0.2.0+).
3. **Boundary**: this repository end to end (Rust, UI, chart, images, CI/release,
   install pack) plus the installer only through `docs/installer-contract.md`;
   Talos/Cilium reviewed as committed here, not the host's live machine config.
4. **Severity for internet-facing exposure**: assume 443 can be forwarded and
   hostile internet clients reach Traefik/Authelia (the DDNS + ACME features
   imply it), even while the node is LAN-only today.
5. **Permanent supply-chain gates** in CI/audit, not just recommendations.

## Non-goals

- `AessemOps/Naslos-Installer` application internals; physical attacks; multi-tenant
  isolation; Talos kernel/Cilium hardening beyond the committed config; performance
  tuning (unless a finding is a DoS vector).

## Threat model and trust boundaries

**Assets**: ZFS data; credentials (LDAP admin + bind password, SMB NT hashes in the
smbusers mirror, proxy secret, agent bearer token, Talos admin `talosconfig`, buddy
identity/KEK, DDNS/OVH/cert-manager tokens, ntfy token, Authelia JWT secret);
host root (via the privileged agent and terminal).

**Adversaries**: unauthenticated internet client; authenticated non-admin
(`naslos_users`); compromised app container / malicious chart or source; malicious
buddy peer; attacker with disk access; supply-chain compromise.

**Crown-jewel chain to keep in view**: the API mounts the Talos admin config and
holds `pods/exec` in the workload namespace, where the terminal is privileged with
`/host` mounted. A single auth bypass in the API is therefore host-root, not
"just" an API compromise. Every boundary below is rated accordingly.

| Boundary | Control today | Audit focus |
| --- | --- | --- |
| Internet → Traefik/Authelia | 2FA forwardAuth, TLS | bypasses, rate limits, header trust, session fixation, CSP |
| Traefik → API | `TRAEFIK_CIDR` + `X-Naslos-Proxy-Secret`, `Remote-*` headers | CIDR fail-open when peer address is unknown (`auth.rs` `is_trusted`), spoofing, constant-time compare |
| API → agent | bearer `AGENT_TOKEN` (+ TLS) | token compare, TLS verification, replay, timeouts, body limits |
| API → Kubernetes | namespaced Roles + two read-only ClusterRoles | secret access in app namespaces, exec-in-terminal, escalation via app pods |
| API → Talos | admin `talosconfig` mount | command construction, least privilege, credential exposure |
| API → Helm/git | bundled CLIs, chart sources | argument injection, untrusted chart values, SSH key handling, URL validation |
| API → LDAP | `ldap3` rustls, bind | DN/password handling, injection, TLS verification |
| Samba/NFS → LDAP | LDAPS + `libnss-extrausers` | uid mapping, share config injection, export scope |
| Terminal → host | privileged + `/host`, admin-only | SA token, hostPath scope, exec allowlist |
| Buddy peer → API | Ed25519 signatures, nonces, quota | replay/audience/sequence, path traversal, quota, crypto |
| Installer → cluster | digest-pinned pack | pack integrity, drift, rollback |

## Deliverables

1. `docs/AUDIT-2026-10-24.md` — executive summary, method, findings register
   (IDs `SEC-2026-NN`), severity, evidence, status, verification, remaining work.
2. Fix PRs, one per remediation batch (below), each with a failing-before test.
3. New permanent gates wired into `scripts/audit.sh` and `.github/workflows/ci.yml`.
4. Release (0.2.1, or 0.3.0 if a fix is breaking) with pack + installer dispatch.
5. Updated `AI_Handoff.md`, `docs/spec.md` (`[OPEN]` markers), `CREDITS.md`,
   `docs/deployment.md`, and any doc whose claim the audit disproves.

## Method and phases

### Phase 0 — Baseline and setup

- Branch `audit/full-2026-10-24`; do not touch `master` until a fix PR is green.
- Run `bash scripts/audit.sh`; record tool versions and current results.
- Snapshot the VM (ZFS snapshot of the buddy/share datasets if present, plus a
  reinstall checkpoint); record the deployed helm revision and image tags.
- Install the new scanners locally (`cargo-audit`, `cargo-deny`, `trivy`, `syft`)
  and record their versions in the report.

### Phase 1 — Boundary inventory

Map, with file:line evidence:
- every route and its layer (public / user / admin) from `server/mod.rs`;
- every RBAC rule and `securityContext` in the chart;
- every secret mount and env var the API, agent, UI, Samba, NFS, LDAP use;
- every `Command::new` and every place user input reaches a shell/CLI/URL/path;
- every filesystem write inside a shared/backed-up directory.

### Phase 2 — Static review workstreams

Each workstream ends with a findings list and, where already suspected, a
candidate finding. Do not start fixing here; keep discovery independent.

**A. Authentication, authorization, session**
`rust/api/src/auth.rs` (the `None => true` CIDR fallback is a fail-open candidate —
confirm every production path supplies `ConnectInfo`, then make it fail closed and
give tests a fake peer address), route layering, WS upgrade auth, buddy auth
(`auth.rs`: replay cache persistence, audience binding, timestamp window), agent
token compare and TLS, Authelia fragments/SSO promotion, UI login/logout/2FA and
session storage.

**B. Command, path and injection**
`kube.rs`, `helm.rs`, `talos.rs` (argument construction, user-controlled names and
values), `ws.rs` (shell allowlist, PTY, resize frames, command strings),
`agent/src/zfs/runner.rs` (chroot argv), `shares` renderers (smb.conf/ganesha.conf
injection), `chartsrepo.rs` (git URL validation, `GIT_SSH_COMMAND`, key file
permissions), `buddy/store.rs` + tar extraction (traversal), folder paths
(symlink escape), config parsing.

**C. Data handling and crypto**
`identity.rs` (DN construction, password modify, LDAP injection), `buddy/`
(envelope nonce/generation/sequence, KEK handling, signature audience, quota,
manifest cap, short-tail), `ddns.rs` (public-IP validation, SSRF, credential
handling), `notifications.rs` (SSRF, token write-only), `logsafe` and error
messages (secret leakage), `json` edge cases, temp-file permissions.

**D. Kubernetes/Helm trust**
App install/upgrade/uninstall (values injection, namespace selection, chart
provenance), routing/certs/secret writes, portal services, image refs, and RBAC
escalation: can an app pod or chart reach the terminal's host mount, the API's
Talos config, or another namespace's secrets?

**E. UI (Svelte/TS)**
CSRF posture, XSS (raw HTML/markdown, terminal output), CSP and security headers
(`routing.rs`), WebSocket auth and resize handling, auth state, open redirects,
`npm audit` dev+prod, Playwright auth setup.

**F. Chart and infrastructure**
RBAC least privilege, NetworkPolicies (default-deny, CNI semantics), PSA labels,
securityContexts (non-root, caps, seccomp), hostPath/privileged scope, Secret
generation and lookup paths, probes/limits, Traefik/Authelia config, cert-manager
issuers, OpenLDAP/Samba/NFS config templates, image pull policy vs digest.

**G. Supply chain and images**
`cargo audit`/`cargo deny` (advisories, licenses, sources), `npm audit`, Trivy
filesystem + each image, SBOM, base-image digests, verification of the downloaded
`kubectl`/`helm`/`talosctl` binaries (checksums/signatures), GitHub Actions
pinning (already SHA-pinned), release pack integrity.

**H. Spec and documentation conformance**
Walk `docs/spec.md` MUSTs: verified / violated / `[OPEN]`. Cross-check claims in
`docs/api.md`, `deployment.md`, `operations.md`, `buddy-backup.md`,
`installer-contract.md`, `AI_Handoff.md` against the code and the live node.

### Phase 3 — Regression map of the pre-Rust fixes

Verify each fix in its Rust implementation **and** that its test exists. Any item
that was silently lost in the port is a High finding.

| Prior finding | Rust location to verify |
| --- | --- |
| PF-H2 nonce reuse / generation | `buddy/envelope.rs`, `client.rs`, `store.rs` |
| PF-H3 restore binding + sequence | `buddy/client.rs` restore path |
| PF-H4 share defaults (CIDR, root squash, no root force, masks) | `shares/render.rs`, chart, UI |
| PF-M5 agent TLS + CA pinning | `agent/client.rs`, chart certs |
| PF-M6 agent timeouts / body limits | `agent/src/server`, `tower-http limit` |
| PF-M7 catalog deep-copy | `catalog.rs`, `apps.rs` |
| PF-M10 default quota + reserve | `buddy/peers.rs`, `store.rs` |
| PF-M11 fail-closed publish guards | `buddy/client.rs` |
| PF-M12 manifest cap / truncation | `buddy/client.rs`, `server/buddy.rs` |
| PF-M13 receiver-bound signatures | `buddy/auth.rs`, `client.rs` |
| PF-M14 KEK from Secret | `buddy/keys.rs`, chart |
| PF-L3 API securityContext / PF-L4 terminal SA token | chart |
| PF-L5 symlink escape | `agent/src/shares/folders.rs` |
| PF-L8 ntfy SSRF | `notifications.rs` |
| PF-L9 LDAPS-only | `openldap/`, chart, API client |
| PF-L12 prefix + generation from manifest | `buddy/envelope.rs` |

Also close or re-justify the previously tracked-open items (PF-L1/L2/L6/L7/L10/L11/
L13–L18) and the UI accessibility appendix.

### Phase 4 — Supply-chain gates to add

- `cargo audit` and `cargo deny check advisories bans licenses sources` for the
  workspace; allowlist with a dated rationale, no blanket ignores.
- `trivy fs` for the repo and `trivy image` per built image in CI (the image job
  must `load: true` or write a tar to scan); fail on High/Critical with an
  acknowledged-exception file.
- SBOM (syft or buildx attestation) per image, attached to the release.
- Base images pinned by digest (`alpine`, `debian`, `nginx-unprivileged`,
  distroless, `rust`, `node`) with Renovate/Dependabot to bump them.
- Checksums/signatures for `kubectl`, `helm` and `talosctl` downloads in
  `rust/api/Dockerfile` (fail the build on mismatch).
- Keep `gitleaks dir` + `git`, `npm audit`, `cargo fmt/clippy/test`, Helm and pack
  checks as they are.

### Phase 5 — Dynamic testing

**Local (every fix gets a test that fails before it):** header/secret/CIDR
spoofing, non-admin vs admin route matrix, WS upgrade without session, buddy
replay/audience/tamper, traversal (`..`, symlinks) in tar/store/folders, SSRF
targets, command metacharacters in app names/values/URLs, oversized bodies,
malformed YAML/manifests, race tests for job/schedule stores.

**Live, controlled (maintenance window, pre-test snapshot):**
1. From a pod outside `TRAEFIK_CIDR`, call the API with a stolen-looking secret →
   expect 401; with the real secret from the wrong CIDR → 401.
2. Through Traefik, send `Remote-User`/`X-Naslos-Proxy-Secret` headers directly →
   expect the proxy to strip/override, never reach the API.
3. Agent `/health` without token and with a wrong token → 401; HTTP (no TLS) →
   refused; wrong CA → refused.
4. WS `/api/ws/exec` as a non-admin and without a session → 403/401; shell
   allowlist rejects anything else.
5. NetworkPolicy probes between namespaces and to `:636`, `:9090`, `:2049`,
   `:445`.
6. Authelia: logout invalidates the session; 2FA cannot be skipped; no open
   redirect on `targetURL`.
7. Secrets: no Secret values in pod env, logs, API errors or the UI; the terminal
   pod has no SA token.
8. Buddy: enrollment closed refuses; replayed request refused across a restart;
   tampered chunk fails; quota enforced.
9. Restore the pre-test state and re-run the Playwright suite.

### Phase 6 — Findings register and report

Severity rubric (internet-facing):
- **Critical**: unauthenticated RCE/host root, auth bypass, data loss, secret
  disclosure to an unauthenticated party.
- **High**: authenticated privilege escalation to host root/Talos, cross-boundary
  secret read, backup integrity break.
- **Medium**: post-auth DoS, weak crypto, info leak, defense-in-depth gap with a
  plausible chain.
- **Low**: hardening, best-practice deviation, no demonstrated impact.
- **Info**: documentation/spec drift.

Finding format: `ID | title | severity | boundary | evidence (file:line, command,
test) | impact | fix | status`. Every finding gets a repro and, for fixes, the test
that proves it.

### Phase 7 — Remediation

Order: Critical → High → Medium → Low → Info. Batch PRs by domain so review stays
possible:

1. `fix(security): auth boundary` (fail-closed CIDR, header handling, WS).
2. `fix(security): command/path injection and SSRF` (CLI adapters, renderers,
   git/ssh, buddy store, tar, folders, ddns/notifications).
3. `fix(security): crypto and backup hardening` (buddy envelope/quota/manifest).
4. `fix(security): kubernetes trust` (RBAC, exec, app isolation, routing/certs).
5. `fix(security): chart and infrastructure` (NetworkPolicy, PSA, securityContext,
   secrets, probes).
6. `fix(security): UI` (CSRF/XSS/CSP/session).
7. `chore(security): supply-chain gates` (Phase 4).
8. `docs: audit report and spec status`.

Each batch: tests (unit + contract where applicable), `bash scripts/audit.sh`,
`helm lint`/`template`, Playwright for UI/API changes, and the relevant live check.
Never push to `master` directly; PR per batch.

### Phase 8 — Release and live validation

- Bump `Chart.yaml` + `ui/package.json` (0.2.1; 0.3.0 if breaking), release via the
  dry run (`publish=false`) then the tag.
- Fresh-install validation on the VM (or reinstall) and a live drill of the fixed
  boundaries; record results in `AI_Handoff.md`.
- Confirm no RAM/performance regression (the Rust baseline is in the plan).

## Definition of done

- Every finding is `fixed` with a failing-before test, or `accepted` with a signed
  rationale; zero unexplained Critical/High.
- The new gates are green in CI and run locally.
- `docs/AUDIT-2026-10-24.md` and the spec/doc updates are merged.
- The release is published, the pack is attached, the installer is dispatched, and
  the live drill passes.

## Risks and mitigations

- **False positives** → require a repro/evidence for every finding; cross-check
  with a second tool or a manual trace.
- **Breaking security fixes** (e.g., fail-closed CIDR, disabled feature) → document
  in the release note and, where needed, a chart value with a safe default.
- **Live-test damage** → pre-test snapshot, no data-destructive tests, reinstall
  rollback.
- **Scope creep into the installer/Talos host** → stop at the documented contract;
  raise separate findings for the user.

## Execution order at a glance

1. Baseline + snapshot (Phase 0).
2. Inventory + static review A–H (Phases 1–2).
3. Regression map (Phase 3).
4. Supply-chain gates (Phase 4) — early, so scanners inform the review.
5. Dynamic tests (Phase 5).
6. Report (Phase 6).
7. Fix PRs in severity order (Phase 7).
8. Release + live drill (Phase 8).
