# Remediation plan — Naslos v0.3.21 audit (2026-11-11)

Companion to [`docs/AUDIT-2026-11-11.md`](../docs/AUDIT-2026-11-11.md).
Ordered so the High item and everything that is cheap and gate-related lands
first. Each batch is independently shippable and must leave the release gate
green. Every fix needs the test named here (it must fail before the change),
plus the docs the repo rules require (spec, API, handoff, CREDITS, README).

Repos: **NL** = `Naslos-Linux`, **NC** = `NaslosCharts`.

## Batch R1 — High (do first)

### R1.1 — Stop a Buddy-disabled install from crashing the API (`AUD-2611-T6-1`, NL) — **DONE on `fix/buddy-disabled-routes`**

Implemented as designed: `build_router` mounts the peer routes only when
`state.buddy.is_some()`; otherwise `/api/buddy/v1` and `/api/buddy/v1/{*rest}`
answer `503`, and router tests cover both the 503 and the public health route.

- Files: `rust/api/src/server/mod.rs`, `rust/api/src/server/buddy.rs`,
  `charts/naslos/templates/api-deployment.yaml` (only if the route gate moves
  there).
- Fix: mount `/api/buddy/v1/*` only when `state.buddy` is `Some`, or change every
  handler to take `State<Arc<AppState>>` and return `503 Service Unavailable`
  when the receiver is absent. Remove the `.expect(...)` from the `BuddyState`
  `FromRef` implementation (it is the panic source). Do not rely on
  `panic = "unwind"`.
- Failing-before test: build the router with `buddy: None` and assert
  `GET /api/buddy/v1/status` returns `503` (today it panics/aborts; a
  `#[should_panic]`/process test would not be usable because of `abort`). Add a
  router-construction test that asserts the routes are not present when
  `buddy` is `None`.
- Verify: `cargo test -p naslos-api`; then, on a node with
  `buddy.enabled: false`, `curl -s -o /dev/null -w '%{http_code}'
  https://naslos.local/api/buddy/v1/status` from an authenticated session
  returns 503/404, not a pod restart. **Do not probe the live node before the
  fix** — the request restarts the API pod.
- Docs: `docs/buddy-backup.md`, `AI_Handoff.md` (operational note), this report.

### R1.2 — Prowlarr (and the *arr family) must not default to public + no login (`AUD-2611-N-2`, NC)

- Files: `apps/prowlarr/naslos-app.yaml`, `apps/radarr/naslos-app.yaml`,
  `apps/sonarr/naslos-app.yaml`, `apps/qbittorrent/naslos-app.yaml`,
  `apps/jackett/naslos-app.yaml`.
- Fix: with `disableAuth: true`, `exposure.auth` MUST default to `true`. Add a
  catalog contract check (script + README) that rejects a manifest where the
  chart's `disableAuth` default is true and `exposure.auth` is false. Keep
  `disableAuth` user-overridable; if the user disables Authelia they must also
  re-enable app auth (the install schema should make the pair explicit:
  `disableAuth` cannot be true while `auth` is off).
- Failing-before test: a `NaslosCharts` contract test / `scripts` sweep entry
  that parses every `naslos-app.yaml` and asserts
  `!(disableAuth == true && exposure.auth == false)`. It fails at `aa0058b`.
- Verify: `helm lint apps/prowlarr`; re-run the catalog sweep; install Prowlarr
  on a scratch namespace and confirm the default exposure shows Authelia required.
- Docs: `NaslosCharts/README.md` (contract), `docs/AUDIT-2026-11-11.md` verdict.

### R1.3 — Make the release gate green (`AUD-2611-A-1`, `AUD-2611-T3-1`, NL) — **DONE in PR #147**

Implemented by upgrading `hickory-resolver` to 0.26.3 (`rust/api/Cargo.toml`),
adapting `detect_dns_ip` to the 0.26 builder API, removing the obsolete
`RUSTSEC-2026-0119` ignore, and bumping `CREDITS.md`. `cargo audit`,
`cargo deny check` and `sh scripts/audit.sh` are green on the branch.

- Files: `rust/Cargo.lock`, `rust/deny.toml`, `scripts/audit.sh`,
  `.trivyignore` (if a dated ignore is chosen).
- Preferred fix: bump `hickory-resolver` to a 0.26.x line (or vendor a patched
  resolver) and refresh the lock; re-run `cargo audit` and `cargo deny`.
  If the upgrade is too invasive for a patch release, add **dated** ignores for
  `RUSTSEC-2026-0487/0489/0490/0491` with a reachability note (zone-file parsers
  are unreachable; 0491 is the wire-format one) plus a re-check date, and add
  the same IDs to `scripts/audit.sh`'s ignore list.
- Failing-before test: `sh scripts/audit.sh` currently exits non-zero with
  `2 check(s) failed`; it must exit 0 after the change.
- Verify: `cargo audit`, `cargo deny check advisories bans licenses sources`,
  `sh scripts/audit.sh`.
- Docs: `docs/AUDIT-2026-11-11.md`, `CREDITS.md` if a dependency changes.

## Batch R2 — Medium (security and correctness)

### R2.1 — Revoke access when an account is disabled (`AUD-2611-T1-1` / `A6`, NL)

- Files: `charts/naslos/templates/authelia-config.yaml`,
  `rust/api/src/identity/ops.rs`, `openldap/image/entrypoint.sh`.
- Fix: enforce disablement at the IdP. Options, in order of preference:
  1. add `(!(shadowExpire<=<today>))`-equivalent via an LDAP ppolicy lock
     (`pwdAccountLockedTime`) on disable and filter locked accounts; or
  2. add a Naslos-owned attribute (e.g. `naslosEnabled`) set to `FALSE` on
     disable and require it in `users_filter`.
  Do not rely on `shadowExpire` in `users_filter` unless the value is computed
  at Authelia parse time (it is not). Also drop disabled users from
  `naslos_users`/`naslos_admins` and keep the SMB `DU` mirror.
- Failing-before test: two-part. Unit-test the rendered `users_filter` for the
  disabled predicate; add a Playwright/API integration test that disables a
  fixture user and asserts an Authelia login is refused (needs the approval in
  the report).
- Verify: render the Authelia config, `authelia config validate` in a container,
  and a live drill (create → login → disable → login fails → enable → login works).
- Docs: `docs/operations.md` (account lifecycle), `docs/spec.md` FR-IDN,
  `AI_Handoff.md` gotchas.

### R2.2 — Canonicalise share paths before apply (`AUD-2611-T5-1` / `B5` / `H4`, NL) — **DONE on `fix/share-symlink`**

Implemented with a no-follow component walk (`symlink_metadata`) in
`validate_path`; a symlink at any level below the ZFS base is rejected. A
regression test covers a direct and an intermediate link. Residual TOCTOU
(after validation) is documented in the report.

- Files: `rust/api/src/shares/mod.rs`, `rust/api/src/shares/render.rs`,
  `rust/agent/src/shares/folders.rs`.
- Fix: resolve symlinks and re-check containment before a share is persisted or
  rendered. The API container's `/var/mnt` view is not the host view, so the
  authoritative check must happen where the path is consumed: either route the
  create/update through the agent's `resolve + strip_prefix` check, or reject
  any component that is a symlink (`symlink_metadata` per component) and
  re-validate at apply time. Keep the agent's existing canonicalise-and-recheck
  as the second gate.
- Failing-before test: create a temp tree with `link -> /etc`, call
  `validate_path`/create with `/var/mnt/.../link`, assert rejection. It fails at
  `v0.3.21` (`metadata` follows the link and `/etc` is a directory).
- Verify: unit test, plus a live drill on a scratch share (create symlink via
  NFS, attempt share create, expect 400; remove symlink).
- Docs: `docs/shares.md`, `docs/spec.md` FR-SHR-05.

### R2.3 — Do not report a successful change when the agent did not apply it (`AUD-2611-T10-1`, `T10-2`, NL)

- Files: `rust/api/src/server/users.rs`, `rust/api/src/server/shares.rs`,
  `rust/api/src/shares/*`.
- Fix: propagate `apply_shares_config` / SMB-mirror failures as errors (502 with
  the agent message), or persist a `pendingApply` state and retry with backoff;
  never return `200` for a user/share mutation that did not reach the node.
  For delete, apply SMB removal first or mark the entity `disabling` until the
  apply succeeds.
- Failing-before test: mock/agent-down harness that makes `apply` fail and
  asserts the handler returns non-2xx and the entity stays visible as pending
  (currently returns 200). The live startup log
  (`failed to apply shares config … agent: …`) proves the failure path exists.
- Verify: unit/integration test with an unreachable agent; live drill by
  stopping the agent (needs approval) and confirming the UI reports the failure.
- Docs: `docs/api.md` (error contract), `docs/shares.md`, `AI_Handoff.md`.

### R2.4 — Reject a malformed ZFS import body (`AUD-2611-T4-1`, NL)

- Files: `rust/api/src/server/zfs.rs`.
- Fix: return `400` on a decode error; require an explicit `{"all": true}` for
  import-all, matching the agent's contract.
- Failing-before test: POST `/api/volumes/zfs/import` with `{}`/empty body and
  assert `400` and that `client.import_pool` is never called (it currently calls
  import-all).
- Verify: unit test; `cargo test -p naslos-api`; no live write.
- Docs: `docs/api.md`.

### R2.5 — Keep secrets out of `apps.json` (`AUD-2611-T2-6` / `D19`, NL)

- Files: `rust/api/src/apps.rs` (`Record.values`, `view()`, store save),
  `charts/naslos/templates/app-isolation.yaml` (app Secret), catalog charts that
  put credentials in env.
- Fix: store secret-valued fields only in a per-app Kubernetes Secret (or a
  sealed/encrypted file), keep a reference in the record, and inject at install
  via `valueFrom`. For the VPN charts, move `vpn_wireguardPrivateKey` /
  `vpn_openvpnPassword` into a Secret and reference it from the gluetun env.
- Failing-before test: install a fixture app with a password field, assert
  `apps.json` contains no plaintext value while `view()` still redacts and the
  rendered Deployment uses `valueFrom.secretKeyRef`.
- Verify: unit test, `helm template` on the two VPN charts, live install on a
  scratch namespace.
- Docs: `docs/AUDIT-2026-11-11.md`, `docs/api.md`, `NaslosCharts/README.md`
  (secret fields must become Secret refs).

### R2.6 — Restart Authelia on platform config change (`AUD-2611-T8-1`, NL)

- Files: `charts/naslos/values.yaml`, `charts/naslos/templates/authelia-config.yaml`.
- Fix: set a pod annotation on the Authelia workload that hashes the platform
  `authelia-config` ConfigMap (and the SSO fragment), e.g. via
  `authelia.pod.annotations`:
  `checksum/platform-config: {{ include (print $.Template.BasePath "/authelia-config.yaml") . | sha256sum }}`.
  (The subchart's own `checksum-config` hashes a ConfigMap it does not mount.)
- Failing-before test: `helm template --set authelia.enabled=true` and change a
  rule in the ConfigMap; assert the Authelia pod-template annotation changes
  (currently it does not).
- Verify: `helm template` diff, `scripts/audit.sh` security-rendering check.
- Docs: `docs/deployment.md`, `AI_Handoff.md` gotchas.

### R2.7 — Keep the Buddy KEK on uninstall (`AUD-2611-T8-2`, NL)

- Files: `charts/naslos/templates/buddy-kek.yaml`.
- Fix: add `helm.sh/resource-policy: keep` to `naslos-buddy-kek` (matching the
  openldap secrets), and document that the KEK must be backed up out of band.
- Failing-before test: `helm template` asserts the annotation is present.
- Verify: `helm template`; a `helm uninstall` dry-run in a scratch namespace
  (needs approval) leaves the Secret.
- Docs: `docs/buddy-backup.md`, `docs/operations.md`.

### R2.8 — Close the Buddy-store exposure on the node overlay (`AUD-2611-T8-3` / `D3`/`F1`, NL)

- Files: `charts/naslos/values-vm.yaml`, `docs/datasets-isolation.md`,
  `charts/naslos/templates/apps-datasets.yaml`.
- Fix: move `buddy.receiveHostPath` outside `/var/mnt` on the production node,
  then set `apps.datasets.allowBuddyStore: false`; keep the render guard.
- Failing-before test: `helm template -f values-vm.yaml` currently renders the
  shared `/var/mnt` PV with the Buddy store nested; the guard must refuse.
- Verify: `helm template`/`helm lint` both overlays; a live drill that an app
  PVC cannot see the Buddy path.
- Docs: `docs/datasets-isolation.md`, `AI_Handoff.md`.

### R2.9 — Refresh the handoff page (`AUD-2611-T11-1`, NL)

- Files: `AI_Handoff.md`.
- Fix: update to chart 0.3.21 / revision 21 / digest-pinned images, add the
  0.3.20 and 0.3.21 notes (resource-limit edits, per-path Authelia protection,
  quota enforcement), and the current open items from this audit.
- Verify: `grep 0.3.19 AI_Handoff.md` is empty; the page matches `helm list`.
- Docs: this is the doc change.

### R2.10 — Catalog hardening defaults (`AUD-2611-N-1`, `N-3`, NC)

- Files: `apps/*/naslos-app.yaml`, `apps/*/templates/app.yaml`, `README.md`.
- Fix: default `exposure.auth: true` (or require an explicit install-time choice
  with no silent default); set `runAsNonRoot: true`, `allowPrivilegeEscalation:
  false`, `capabilities.drop: [ALL]` (add back only what an image needs),
  `readOnlyRootFilesystem` where the image allows, and
  `automountServiceAccountToken: false` on every app pod. Add a catalog checklist
  row per app.
- Failing-before test: extend the catalog sweep to assert the fields; it fails
  for 33/33 apps today.
- Verify: `helm lint` + a scripted render check for all apps; install two
  representative apps on a scratch namespace.
- Docs: `NaslosCharts/README.md` (required pod-security fields),
  `docs/AUDIT-2026-11-11.md`.

### R2.11 — DNS resolver CVE follow-up (`AUD-2611-T3-1`)

Covered by R1.3. If an ignore is chosen instead of an upgrade, add a dated
re-check and a reachability note.

### R2.12 — Notification webhook SSRF (`AUD-2611-T7-1`, NL)

- Files: `rust/api/src/notifications.rs`.
- Fix: resolve the host once, reject loopback/private/link-local/ULA/CGNAT/
  `0.0.0.0/8`, pin the resolved IP for the request (reqwest `resolve`), and keep
  `Policy::none()`. Apply the same `blocked_ip` helper the DDNS driver uses.
- Failing-before test: `notification_url`/send with `http://127.0.0.1/`,
  `http://10.0.0.1/` and a hostname resolving to `127.0.0.1` must fail; today
  only link-local fails.
- Verify: unit tests; no live send.
- Docs: `docs/api.md` (notifications), `docs/operations.md`.

### R2.13 — DDNS DNS-rebinding guard (`AUD-2611-T7-2`, NL)

- Files: `rust/api/src/ddns/drivers.rs`.
- Fix: resolve once, validate every address, and connect to the vetted IP
  (reqwest `ClientBuilder::resolve` / a custom connector) so the guard and the
  connection cannot disagree; extend `blocked_ip` to the reserved ranges above.
- Failing-before test: a hostname that resolves to `127.0.0.1` only on the
  second lookup must be rejected (today the guard sees one answer and reqwest
  gets another). Reserved-range table test.
- Verify: unit tests with a stub resolver.
- Docs: `docs/dynamic-dns.md`.

## Batch R3 — Low / Info (schedule normally)

| ID | Repo | Fix | Test |
| --- | --- | --- | --- |
| T6-2 / C20 | NL | Stop returning the peer-name list to non-owner peers on `/status`. | unit test |
| T6-3 | NL | Enforce `Content-Length` and stream the receiver response through `take(MAX_MANIFEST_BYTES)` instead of `resp.bytes()`. | unit test with an oversized body |
| T6-4 | NL | Require a minimum enrollment-token length/entropy (refuse to start with a weak token) and rate-limit `/enroll`. | unit test + config check |
| T6-5 | NL | Verify the remote manifest signature before using `to_guid` as the incremental base. | unit test with a forged manifest |
| T6-6 / C15 | NL | Drop the empty-audience acceptance on `/status`; bind it to the receiver audience. | unit test |
| T7-3 | NL | Read provider/public-IP responses through `take(limit)` (no full buffer). | unit test with an oversized body |
| T7-4 | NL | Always run `validate_credentials_secret`/`acmeEmail` and make `fields` required; restrict the passthrough solver to a typed allowlist. | unit test |
| T1-2 | NL | Put `security-headers` before `forwardauth-authelia` on the UI route (or add a `redirect-headers` middleware) so the auth 302 carries HSTS. | render assertion on middleware order |
| T1-3 | NL | Document that `protectedPaths` must match the app's routing case; optionally add a case-insensitive matcher or normalize to the app's route. | live drill (needs approval) |
| T2-1 | NL | Require `privilegedConfirmed` on upgrade when the catalog flips `privileged` or adds hostPath; compare old/new chart values. | unit test on the diff path |
| T2-2 | NL | Drive the privileged-pool quota from values like the baseline pool. | `helm template` |
| T3-4 / T8-6 / F9 | NL | Add the `naslos-tls` TLSOption to the UI route; document cert rotation (delete + re-apply) and the 3650-day lifetime. | render assertion |
| T4-2 | NL | Timeout the post-EOF `zfs receive` wait and kill the child. | unit test with a hung child |
| T4-3 / F15 | NL | Restrict host firewall 9090 to the API pod IP; document rotation. | live `nft`/Cilium check (needs approval) |
| T5-2 | NL | Escape/validate `SMB_DISCOVERY_NAME` before the `sed`. | entrypoint test |
| T5-3 | NL | Re-validate share names on load. | unit test |
| T8-4 / F22 | NL | Update the stale "inert on flannel" comment; tighten the `naslos-privileged` → API peer to the terminal pod label and scope app east-west if desired. | render check |
| T8-5 / F12 | NL | Add container securityContexts to openldap, the bootstrap Job and the backup CronJob; requests/limits for the jobs; probes for the terminal. | `kube-linter` diff |
| T8-7 | NL | Extend the upgrader admission policy to volumes/args/env. | admission-policy unit tests |
| T9-1 | NL | Strip OSC/C1/`\r` in `stripAnsi`. | unit test with a hostile log buffer |
| T9-2 | NL | Move to a CSP nonce (requires SvelteKit config) or document the inline exception. | CSP assertion |
| T9-3 | NL | `Cache-Control: no-cache` for `index.html`, immutable for hashed assets. | nginx config test |
| T9-4 / E4 | NL | Abort the pending preflight on unmount. | component test |
| T9-5 | NL | Restrict catalog `icon` URLs to a proxy or allowlisted hosts. | render check |
| T10-3 | NL | Move `assert_regular_tree` and store writes to `spawn_blocking`. | benchmark/lint |
| T10-4 | NL | Log (not discard) rollback/status failures; surface them in the job result. | unit test |
| T10-5 | NL | Add the missing tests for #146 rejection, #131 portal caller, #126/#139 chart assertions. | new tests |
| T11-2 | NL | Add the missing routes to `docs/api.md` (and the agent routes). | doc review |
| T11-3 / G2 / F13 | NL | Digest-pin base images; add Helm/git/openssh to CREDITS; fix the K8s pin line; add Renovate/Dependabot. | CI + doc check |
| T11-4 | NL | Make `update_available` semver-only (no string fallback) for FR-APP-22. | unit test with `release-10`/`release-9` |
| T11-5 | NL | Fix the README `make all` description. | doc review |
| N-5 | NC | Document the floating-tag risk once (owner decision) and prefer dated tags for reproducibility. | doc review |
| C13 | NL | Reject an empty bind-password file; add a bind timeout. | unit test |
| C20 | NL | Stop returning the full peer-name list on `/status`; return only the caller's view. | unit test |
| F11 | NL | Narrow the app egress `serviceCIDR` to the specific services apps need, or remove it. | `helm template` + live L6 |
| F20 | NL | Either implement `LDAP_TLS_ENFORCE`/`olcSecurity` or delete the misleading comment. | config test |
| F21 | NL | Provision local-path PVC dirs `0770` instead of `0777`. | live stat (needs approval) |
| F23 | NL | Raise/justify the API 256 Mi limit (it forks CLIs). | values |
| F24 | NL | Document the elevated-session `skip_second_factor` trade-off or require the factor again. | config review |
| G10 / G13 / G14 | NL | Cosign images/pack; re-sign-off the HTTP registry or migrate it; verify vendored chart/CRD provenance at install. | release pipeline |
| H14 | NL | Remove AFP or stop rendering it as SMB. | unit test |
| H16 | NL | Add explicit read/idle timeouts. | config review |

## Batch R4 — Live drills to approve (from the audit)

Each needs the owner's go-ahead because it creates or changes state. Run them
against a scratch namespace / throwaway user, then clean up:

1. Per-path protection bypass matrix (install a protected-path app).
2. Disabled-account revocation (R2.1).
3. Share symlink rejection (R2.2).
4. Agent-down apply failure surfacing (R2.3).
5. Privileged consent on a synthetic chart upgrade (T2-1).
6. Quota exhaustion in a scratch namespace.
7. Buddy enroll/replay/tamper on two nodes.
8. Full Playwright suite.

## Definition of done for the remediation

- `sh scripts/audit.sh`, `cargo audit`, `cargo deny`, `npm audit` (incl. dev)
  and the catalog sweep are all green.
- No High/MEDIUM finding in this audit remains without either a fix or a dated,
  owner-signed acceptance recorded in `docs/AUDIT-2026-11-11.md`.
- `AI_Handoff.md`, `docs/spec.md`, `docs/api.md`, `CREDITS.md`, `README.md` and
  the `NaslosCharts/README.md` reflect the changes (repo rules).
- A fresh `helm upgrade`/reinstall reproduces every fix; no live-only patches.
