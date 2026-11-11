# Full audit — Naslos-Linux v0.3.21 + NaslosCharts (executor: DeepSeek)

Audit only. **No code fixes.** Output is a findings report and a remediation
plan, delivered as a PR that the owner triages.

## Decisions (agreed with the owner)

| Topic | Decision |
| --- | --- |
| Platform baseline | Tag `v0.3.21` = `3e79e6e7239b76615174f04faebc0c666079bd56` (equals `origin/master` on 2026-11-11). Release assets: `naslos-install-pack-0.3.21.tar.gz` + `.sha256`. |
| Catalog baseline | `AessemOps/NaslosCharts` `origin/main` = `aa0058b` (33 apps under `apps/`). The release follows `main` unpinned (`apps.officialSource.commit: ""`), so also record the commit the live node has cloned. |
| Previous audit | `docs/AUDIT-2026-10-24.md`, baseline `5e976f8`. Everything since (~95 commits, 170 files, +21k lines) is **new surface** and gets the deepest review. Also re-check `.kilo/plans/1793474557133-review-remediation.md`. |
| Live node | `https://naslos.local` (bare-metal `192.168.1.38`), owner states it runs **0.3.21**. Read-only inspection and **non-mutating** probes only. Any test that could create, modify, delete or restart something is written up as "needs approval" and **not run**. |
| Deliverables | `docs/AUDIT-2026-11-11.md` (report) + `.kilo/plans/2026-11-11-audit-remediation.md` (fix plan) in `Naslos-Linux`, on branch `audit/full-2026-11-11`, PR against `master`. NaslosCharts findings live in the same report; no commits to NaslosCharts. |

Paths below are relative to `/home/aessem/ApplicationProjects/Naslos/Naslos-Linux`
unless stated otherwise.

## Ground rules for the executor (read twice)

1. **Evidence or it did not happen.** Every finding quotes the code (≤10 lines,
   read from the `v0.3.21` tree — re-open the file and confirm the line numbers
   before writing them) or the exact command + trimmed output. A suspicion
   without evidence goes in the "Unconfirmed" list, never in the register.
2. **No secret values anywhere** — report, commits, notes, PR, chat. Cite the
   Secret/key name and file:line only. Never print `kubectl get secret -o yaml`,
   `talosctl get machineconfig`, `bootstrap/vm/*`, `ui/.env.playwright.local`,
   or `ui/tests/.auth/*`. If a probe needs a secret (proxy secret, agent token)
   and your shell cannot use it without echoing it, skip it and list it as
   "needs approval".
3. **Production node has real data.** Allowed live actions are exactly the list
   in Phase 4. No `kubectl apply/patch/delete/edit/rollout/scale/run/debug`, no
   `helm upgrade/install/uninstall/rollback`, no `talosctl` writes, no
   Playwright (it creates and deletes resources), no Authelia login attempts
   (regulation can ban the admin account), no app installs, no upgrade/reboot
   endpoints.
4. **Stop and ask the owner** if the node is not on 0.3.21, a probe has an
   unexpected side effect, you find a Critical, or you find live evidence of
   compromise.
5. **Re-verify, don't copy.** Prior IDs (`SEC-2026-*`) are re-checked at
   `v0.3.21` and get a verdict: fixed / still open / regressed / obsolete.
6. **Respect owner decisions** (not findings): Radarr deliberately has no
   downloads dataset mount; app datasets are `10001:10000` mode `2770`; catalog
   charts use floating image tags by design (report the risk once, not per app);
   the official source tracks `main` by product decision (D6, re-check only).
7. Keep context small: work one batch at a time, write raw tool output to
   `/tmp/kilo/audit/raw/`, append confirmed findings to the report at the end of
   each batch, and commit the report (do not push until Phase 6).

## Phase 0 — Setup and recon

1. In `Naslos-Linux` (currently on `feature/app-protected-paths`, clean):
   `git fetch --tags origin`, `git switch -c audit/full-2026-11-11 v0.3.21`.
   Copy this plan to `.kilo/plans/2026-11-11-full-audit-v0.3.21.md` and commit
   it first (`docs(plans): add the v0.3.21 full audit plan`).
2. NaslosCharts: `git -C ../NaslosCharts fetch origin` then
   `git -C ../NaslosCharts worktree add /tmp/kilo/audit/NaslosCharts aa0058b`
   (read-only use; remove the worktree at the end).
3. Verify the release pack: `gh release download v0.3.21 -R
   AessemOps/Naslos-Linux -D /tmp/kilo/audit/pack`, `sha256sum -c`, extract,
   list the digest-pinned images and the embedded chart; diff the embedded
   chart against `charts/naslos` at the tag.
4. Tools: put `~/.cargo/bin` on `PATH` (`cargo`, `cargo-audit`, `cargo-deny`,
   `cargo-machete`). Installed: `trivy`, `semgrep`, `helm`, `kubectl`,
   `talosctl`, `docker`, `npm`. Missing → run from containers: `gitleaks`
   (`zricethezav/gitleaks`), `hadolint`, `shellcheck` (`koalaman/shellcheck`),
   `kubeconform`, `kube-linter`, `checkov`, `yamllint`. A tool that cannot run
   is recorded as a coverage gap.
5. Live recon (read-only, `KUBECONFIG=bootstrap/vm/kubeconfig`): `helm -n naslos
   list`, `helm -n naslos get values naslos` (redact secret-looking values in
   notes), `kubectl get pods -A -o wide`, image digests of every naslos pod vs
   the pack, namespaces with PSA labels. **Gate:** chart/app version = 0.3.21
   and digests match the pack, otherwise stop (rule 4).
6. Write the inventory into the report: entrypoints (route table from
   `rust/api/src/server/mod.rs`, agent routes), trust boundaries (Traefik →
   Authelia → API → agent/kube/helm/talos), identities (service accounts and
   their RBAC), secrets (name → template → consumer), and data stores
   (`/var/lib/naslos/*.json`, PVCs).

## Phase 1 — Automated sweep (Batch A)

Run inside the repo at the tag; save outputs to `/tmp/kilo/audit/raw/`.

| Area | Command |
| --- | --- |
| Project gate | `sh scripts/audit.sh` (record every skipped check) |
| Rust (in `rust/`) | `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --locked`, `cargo audit`, `cargo deny check`, `cargo machete` |
| Advisory ignores | Every dated ignore in `rust/deny.toml` and `.trivyignore` — still needed? expiry passed? |
| UI (in `ui/`) | `npm ci`, `npm run check`, `npm audit --omit=dev`, `npm audit` (incl. dev, SEC-2026-E19), `npm outdated` |
| Secrets | gitleaks on the working tree (`.gitleaks.toml`) and on full history (`.gitleaks-history.toml`) |
| SAST | `semgrep --config=auto rust/ ui/src` (triage; most will be noise) |
| Dockerfiles | hadolint on every Dockerfile in `.github/image-matrix.json`; base-image digest pinning (G2) |
| Shell | shellcheck on `scripts/*.sh` and image entrypoints |
| Images | `trivy image` on each digest-pinned GHCR image from the 0.3.21 pack (HIGH/CRITICAL, plus unfixed count) |
| Chart | `helm lint` + `helm template` for both overlays (`values-vm.yaml`, `values-installer.yaml`) → kubeconform (`-ignore-missing-schemas` for CRDs), kube-linter, checkov |
| CI | Review `.github/workflows/*.yml`: `permissions:`, actions pinned by SHA, `pull_request_target`, secret exposure to forks, release guard, dispatch token scope |

## Phase 2 — Manual review of Naslos-Linux (Batches B–D)

Prioritise files changed since `5e976f8` (`git diff --stat 5e976f8 v0.3.21`).
For each track: list the files read, then the findings. Across all Rust code,
`panic = "abort"` means any reachable `unwrap`/`expect`/slice index/integer
overflow in a request path is a DoS — grep for them in handlers.

**Batch B — highest risk**

- **T1 AuthN/Z and routing** — `auth.rs`, `authelia.rs`, `routing.rs`,
  `server/mod.rs`, `server/ws.rs`, chart `traefik-middleware.yaml`,
  `authelia-config.yaml`, `authelia-sso.yaml`, `ingress*.yaml`. New: per-path
  Authelia protection (#145) — bypass via encoding (`%2e`, `%2f`), case, double
  slash, trailing slash, `..`, Traefik `Path`/`PathPrefix` and priority order;
  SSO portal routes on promoted domains (#131); whole-host forwardAuth. Check:
  every route behind the owner gate, CSRF layer on every unsafe method, WS
  Origin, `TRAEFIK_CIDR` = the whole pod CIDR (so the NetworkPolicy is the real
  boundary — confirm), Remote-* stripping, Authelia rules for app hosts, 2FA
  policy, elevated session (F24), `shadowExpire` filter (A6).
- **T2 App lifecycle and Kubernetes writes** — `apps.rs`, `app_usage.rs`,
  `catalog.rs`, `chartsrepo.rs`, `helm.rs`, `kube.rs`, `datasets.rs`,
  `server/apps.rs`, `server/app_jobs.rs`, `server/catalog.rs`,
  `server/dataset_registry.rs`, `server/pods.rs`; chart `app-isolation.yaml`,
  `rbac-apps.yaml`, `rbac-helm.yaml`, `api-pool-storage-rbac.yaml`,
  `apps-datasets.yaml`, `networkpolicy-apps.yaml`. Check: namespace-pool limit
  `PUT` bounds and races; per-app ResourceQuota (#146) — can a chart or the Helm
  identity delete/override it, is there a LimitRange, what happens to apps that
  exceed it; resource-limit edits (#144) validation; `pods`/`pods/log` scope —
  can `/api/apps/{name}/pods` or `/api/ws/logs` reach platform namespaces with a
  crafted name/namespace; egress-policy creation, labeling and orphan sweep
  (could it delete a policy it does not own?); dataset attach/detach — path
  traversal, attaching the Buddy store or another app's dataset, PV `hostPath`
  construction, the `datasetsLayout: v2` migration; numeric coercion vs
  JSON-schema validation (type confusion); privileged consent on **upgrade**
  (update badge → one-click upgrade when the new chart version turns
  `privileged: true` or adds hostPath); install values at rest (D19); chart
  provenance (D6); symlinked charts; anything still mounting the legacy shared
  `naslos-datasets` PV (D3/F1).
- **T3 System, upgrade and Talos** — `upgrade/*`, `bin/naslos-upgrade.rs`,
  `system.rs`, `talos.rs`, `pack.rs`, `semver.rs`, `server/system.rs`, chart
  `system-upgrade.yaml`. Check: upgrader Job RBAC breadth, how the pack/version
  is fetched and verified (checksum only? signature? downgrade allowed? TLS?),
  overrides injection into Helm values, reboot/upgrade authz + CSRF, Talos
  admin config exposure to the API pod (D15), status leakage.

**Batch C**

- **T4 Agent, ZFS and disks** — `rust/agent/src/**` (new `zfs/smart.rs`,
  `devices.rs`, `operations.rs`, `runner.rs`, `dataset.rs`), API `agent.rs`,
  `server/disks.rs`, `server/zfs.rs`, chart `agent-daemonset.yaml`. Check: argv
  construction and flag injection, device-path validation for `smartctl`,
  parser robustness on hostile tool output, timeouts, token + TLS enforcement,
  reachability of `:9090` (F15), pool-destroy confirmation, the pool-backed app
  storage provisioner.
- **T5 Shares, identity, LDAP** — `shares/*`, `identity*`, `server/shares.rs`,
  `server/users.rs`, `openldap/`, `samba/`, `nfs/`, chart templates. Re-check
  B5/H4 (symlink escape), H5, C13, C18, F20; the SMB canonical-uid fix (#123).
- **T6 Buddy** — `buddy/*`, `server/buddy*.rs`, `bin/buddyctl.rs`, receiver
  binary. Re-check C2 residual, C15, C19, C20, H17–H19; any new code in the
  send/restore path.
- **T7 DDNS, certs, notifications, providers** — `ddns*`, `providers*`,
  `certs.rs`, `notifications.rs`, `server/domains.rs`, `server/ddns.rs`,
  `ovh-webhook-rbac.yaml`. SSRF (C16), credential Secret naming (D12),
  provider YAML loading.

**Batch D**

- **T8 Platform chart hardening** — every template, `values*.yaml`,
  `values.schema.json`. PSA per namespace, securityContext/seccomp, probes,
  requests/limits, `automountServiceAccountToken`, RBAC least privilege (render
  every Role/ClusterRole into a table: subject → verbs → resources →
  namespaces), NetworkPolicy matrix (who can reach API, agent, LDAP :636,
  Samba, NFS, Authelia), `randAlphaNum` vs `lookup` persistence for every
  generated Secret, checksum annotations, hostPath/hostNetwork/hostPort
  justification, TLSOption + self-signed cert lifetime (F9), proxy secret in a
  CRD (A4), installer vs node overlay drift.
- **T9 UI** — `ui/src/**`, `ui/nginx.conf`. `{@html}`/`innerHTML`, rendering of
  streamed pod logs (control chars/ANSI, very long lines, memory growth in the
  drawer), usage polling/sparklines, secret masking in forms, CSP
  effectiveness, `index.html` caching (known open item), terminal WS lifecycle
  (E4).
- **T10 Quality and tests** — test coverage of every feature since `5e976f8`
  (does each fix have a failing-before test?), blocking calls in async code,
  unbounded collections/caches, swallowed errors, dead code, spawned tasks
  without shutdown.
- **T11 Docs and spec conformance** — `docs/spec.md` MUSTs touched since
  `5e976f8` (FR-APP-*, FR-INSTALL-*) vs code and tests; `docs/api.md` vs the
  route table; `CREDITS.md` vs `Cargo.toml`/`package.json`/Dockerfile bases;
  `AI_Handoff.md` (still says 0.3.19 deployed); README.

## Phase 3 — NaslosCharts catalog (Batch E)

Work in `/tmp/kilo/audit/NaslosCharts` at `aa0058b`. Read how the API consumes
it (`catalog.rs`, `chartsrepo.rs`, `apps.rs`) so findings reflect the real
install path (install as `naslos-helm`, API-owned routes, per-app baseline
namespace unless `privileged: true`).

1. Scripted sweep over all 33 apps (save a per-app table): `helm lint`,
   `helm template <app> apps/<app> -n naslos-app-1` with defaults →
   kubeconform + kube-linter; contract checks from `README.md` (folder = chart
   name = `naslos-app.yaml` `name`, DNS-1123, `services[].name` literal or
   `{{ .Release.Name }}`, port 1–65535); `version` equal in `Chart.yaml` and
   `naslos-app.yaml`.
2. Per-app checklist (one row per app, ✓/✗/n.a.):
   - PSA **baseline** compatible (no hostPath/hostNetwork/hostPID/privileged/
     added capabilities) unless `privileged: true` is declared; privileged apps
     justify each capability (qbittorrent/flaresolverr VPN `NET_ADMIN`,
     runners).
   - Install schema offers the dataset picker; no required hostPath field;
     `/var/mnt` only as chart fallback.
   - Runs as `10001:10000` (or documented otherwise); `runAsNonRoot`,
     `allowPrivilegeEscalation: false`, dropped capabilities where the image
     allows.
   - Requests/limits present and **fit the per-app ResourceQuota** from
     `app-isolation.yaml` (0.3.21 enforces it — an app without limits or above
     the quota may fail to schedule or upgrade). Most likely release
     regression; check every app and cross-check the installed apps live.
   - Generated secrets use `lookup` (stable across upgrades); no hard-coded
     default credentials; no secret in `values.yaml`.
   - Config changes trigger a rollout (checksum annotations cover the whole
     config, not only the secret — known SearXNG pitfall).
   - Probes present and sane; `latest` + `IfNotPresent` stale-image pattern
     reported once catalog-wide.
   - No Ingress/IngressRoute/Middleware/Certificate/RBAC objects (the API owns
     routes; `naslos-helm` cannot create them — a chart that does will fail).
   - Radarr/Sonarr/Prowlarr `disableAuth` (`AUTH__METHOD=External`): can an
     owner expose the app with Authelia off while app auth is disabled? Record
     the resulting unauthenticated exposure and whether the UI warns.
   - Runner charts (`forgejo-runner`, `gitea-runner`): Docker socket, DinD,
     privileged — what a CI job could reach.
3. Live: compare the catalog commit the node has cloned (read-only API `GET`
   for sources, or `kubectl -n naslos exec deploy/naslos-api -- git -C <clone
   dir> rev-parse HEAD` if `git` exists in the image) with `aa0058b`, and the
   installed app chart versions (`helm list -A`) with the catalog.

## Phase 4 — Live validation on naslos.local (Batch F)

Validate findings from Phases 1–3 against the node. One probe at a time; record
command, expected, observed. **Allowed (non-mutating):**

| ID | Probe |
| --- | --- |
| L1 | Anonymous `curl -sk -o /dev/null -w '%{http_code} %{redirect_url}'` on `https://naslos.local/`, sample `/api/*`, `/api/ws/exec`, `/traefik`, an unknown `/api/x`, each installed app host, and the per-path-protected paths with the T1 bypass variants. Expect redirect to Authelia / 401, never 200 on protected content. |
| L2 | Same requests with forged `Remote-User`, `Remote-Groups`, `X-Naslos-Proxy-Secret: x`, `X-Forwarded-*` from the workstation. Expect stripped → same as L1. |
| L3 | TLS/headers: `curl -skv`, `openssl s_client` (protocol/cipher floor, cert SAN/lifetime), CSP/HSTS/Permissions-Policy, Authelia cookie flags on the portal page (no login). |
| L4 | Read-only `kubectl get/describe` of NetworkPolicies, Roles/ClusterRoles/Bindings, ResourceQuotas/LimitRanges in app namespaces, PSA labels, pod securityContexts, IngressRoutes/Middlewares, Certificates; `kubectl auth can-i --list --as=system:serviceaccount:<ns>:<sa>` for the API, Helm and upgrader identities. |
| L5 | `helm -n naslos get manifest naslos` vs `helm template` of the tag with the installer profile → drift (live hotfixes, handoff gotcha 15). |
| L6 | Reachability from **existing** pods with existing tools only (`kubectl exec <app-pod> -- wget/curl` if present; no package installs): app pod → API port, agent `:9090` (no token), LDAP `:636`, Authelia, another app's Service, kube API. Expect NetworkPolicy deny. |
| L7 | Read-only API `GET`s from the trusted terminal pod per `docs/operations.md` (`/api/system/version`, `/api/apps`, `/api/apps/namespaces`, sources) — only if the headers can be supplied without printing the secret (rule 2). |
| L8 | Rejected-input requests (CSRF without `Origin`, malformed names, injection payloads) **only** when you can cite the code line proving rejection happens before any side effect. Otherwise → needs approval. |
| L9 | Log scan for secret/identity leakage: `kubectl logs --tail=2000` of API, agent, Authelia, Traefik; report the pattern and location, never the value. |
| L10 | `talosctl version` and `talosctl get extensions` only. |

**Needs approval (write up, do not run):** Playwright suite, any request
expected to succeed with a write, app install/upgrade/uninstall, probe pods or
namespaces, restarts, Authelia login/2FA tests, Buddy enrollment/replay/tamper,
agent write endpoints, upgrade/reboot endpoints, quota-exhaustion tests. For
each, give exact steps, expected result, and cleanup so the owner can approve
it as a follow-up.

## Phase 5 — Triage and re-validation

- Deduplicate; map every finding to prior IDs where relevant.
- Verdict table for every `SEC-2026-*` ID that was Open/Partial/Accepted in
  `docs/AUDIT-2026-10-24.md` and every item in
  `.kilo/plans/1793474557133-review-remediation.md`.
- Severity rubric (same as prior audits): **Critical** unauthenticated RCE,
  data destruction, credential exposure to the network; **High** auth bypass,
  privilege escalation, cross-app/namespace escape, authenticated destructive
  abuse, secret disclosure, backup integrity failure; **Medium**
  defense-in-depth gaps, info disclosure, DoS, reachable dependency CVEs;
  **Low/Info** hygiene, docs drift. Add a CVSS 3.1 vector for Medium and above.
- Mark each finding **live-confirmed**, **code-confirmed**, or
  **unconfirmed**.

## Phase 6 — Reporting and hand-off

1. `docs/AUDIT-2026-11-11.md`: baselines (SHAs, pack checksum, live helm
   revision + digests), toolchain versions, executive summary (top risks
   first), severity counts, inventory, findings register by track (`ID | Sev |
   Finding | Evidence file:line | Confirmation | Recommendation`), NaslosCharts
   per-app table, prior-ID verdict table, live-probe log, needs-approval test
   list, coverage gaps, unconfirmed list. IDs: `AUD-2611-<track>-<n>` (e.g.
   `AUD-2611-T2-3`; catalog `AUD-2611-N-<n>`).
2. `.kilo/plans/2026-11-11-audit-remediation.md`: ordered batches
   (Critical/High first), each with affected repo (Naslos-Linux or
   NaslosCharts), files, fix approach, the test that must fail before the fix,
   verification (gate + live check), and the spec/docs/CREDITS/handoff updates
   `AGENTS.md` requires.
3. Gates before pushing: `sh scripts/audit.sh` still passes (only docs
   changed); gitleaks clean on the branch diff.
4. Commit (`docs(audit): v0.3.21 full audit report and remediation plan`), push
   `audit/full-2026-11-11`, open a PR against `master` with the summary and
   severity counts. **Do not merge.** Remove the `/tmp/kilo/audit/NaslosCharts`
   worktree (`git -C ../NaslosCharts worktree remove ...`).

## Definition of done

- Tracks T1–T11 and the catalog sweep each have a "files reviewed" list and
  findings (or an explicit "no findings").
- Every register entry has evidence and a confirmation level; no secret values
  in any artefact.
- Every prior open ID has a verdict.
- Live probes logged with observed results; nothing outside the allowed list
  was run; needs-approval tests are written up with steps and cleanup.
- Report + remediation plan committed on `audit/full-2026-11-11`, PR open.

## Out of scope

- Fixing anything (separate, owner-approved remediation work).
- Naslos-Installer (except where the pack contract is part of the release).
- Talos/Kubernetes/CNI CVEs, hardware, physical security, upstream internals
  of Authelia/Traefik/OpenLDAP/app images (only their configuration here).
- Public-internet scanning of `*.shail.ovh` beyond the L1/L2 requests.

## Risks

- **Production impact:** bounded by the allowlist; the executor stops on any
  unexpected side effect.
- **Hallucinated evidence:** rule 1 (re-read before citing) plus a
  confirmation level per finding; spot-check a sample of file:line citations
  during PR review.
- **Context loss across a long audit:** batches commit the report
  incrementally; raw outputs stay in `/tmp/kilo/audit/raw/`.
- **Tool gaps:** container fallbacks; anything not run is a stated coverage
  gap.
