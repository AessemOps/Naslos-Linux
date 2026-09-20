# Naslos — audit and fix report (2026-09-19)

This is the consolidated record of the audit and the remediation, and the only
current audit document. The detailed working papers are archived under
`docs/archive/`: `AUDIT-2026-09-19.md` (findings), `AUDIT-2026-09-19-FIXPLAN.md`
(batches and progress), `CODE-REVIEW.md` (per-finding review list) and the
superseded `SECURITY-AUDIT.md` + `SECURITY-FIX-PLAN.md` (2026-09-14).

- **Audit baseline:** `master` = `85ae874` (PR #23; the dev endpoint had just
  been removed). The 2026-09-14 audit is superseded and archived.
- **Fix branch:** `audit/full-2026-09-19`, 43 commits `4e47884` → `77cdf4a`,
  pushed. `master` was never pushed to directly.
- **Live target:** `192.168.1.117`, helm revision **42** — `naslos-api`
  `0.1.0-r9`, `naslos-agent` `0.1.0-r4`, `naslos-ui` `0.1.0-r10`; samba/nfs/
  terminal `0.1.0-r3`, openldap `0.1.0-r4` (all Debian 13). Traefik hostPort
  80/443, Traefik v3.7.13 (chart 41.6.0) and Authelia 4.39.24 (chart 0.11.22),
  `naslos.local`. Node: Talos **v1.14.1** (kernel 6.18.51-talos), **Cilium
  v1.20.2** replacing flannel and kube-proxy, with the NetworkPolicy set
  **enforced** (see M4 below).
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
| Low closed / accepted | 8 + 1 |
| Low deferred | 0 |
| Batch 6 coverage | **done** — `trivy` image (all 7 tags) + config, `semgrep`, AV-5…AV-12 active tests, buddy crypto deep-dive; 2 config findings fixed (DS-0031, KSV-0053), 2 low AV-8 findings recorded |

The instance now has a single authenticated entry point, no default or committed
credentials, verified LDAP TLS, backups on their own dataset, a persisted session
secret, and an audit sweep (`scripts/audit.sh`) that runs the Go/UI/chart checks
and gates on `govulncheck`, `gosec` and `staticcheck`.

## 2. Method

- **Static review** with the `code-review-skill` references (Go, Svelte,
  Security, Architecture, Universal quality) and `security-best-practices`.
- **Scanners.** `gitleaks detect` (full history + tree) and `trufflehog git
  --only-verified`; `govulncheck`, `gosec`, `staticcheck`, `npm audit` (prod and
  full). For Batch 6, `trivy` (image and config), `semgrep --config=auto` and the
  Go SAST tools were installed and run directly; the audit sweep re-runs
  `govulncheck`/`gosec`/`staticcheck` when present.
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
| M3 | `jwt_secret` regenerated every render; secrets in a ConfigMap | **Partial** — `421711a`; the LDAP bind password moved out of the ConfigMap at revision 41 (see below) | Persisted in `naslos-authelia-jwt` (proven across an upgrade). The LDAP bind password now reaches Authelia from the `naslos-openldap` Secret via `AUTHELIA_AUTHENTICATION_BACKEND_LDAP_PASSWORD_FILE`. Residual: `jwt_secret` still sits in the ConfigMap because the subchart has no Secret-backed config mount |

**AUDIT-M3 follow-up — the LDAP password now comes from a Secret (revision 41).**
The revision-38 attempt was reverted; the follow-up found *two* independent
reasons it could never have worked:

1. The subchart only renders `AUTHELIA_AUTHENTICATION_BACKEND_LDAP_PASSWORD_FILE`
   when `configMap.authentication_backend.ldap.enabled` is true (its default is
   false). The attempt set only `password.secret_name`, so no env var rendered
   and Authelia started without a bind password.
2. The mount and the env path are built independently: the volume mounts at
   `/secrets/<additionalSecrets[key].path>`, while the env helper builds
   `/secrets/<password.secret_name>/<password.path>`. With
   `path: service-password` the file landed at
   `/secrets/service-password/service-password`, not at the env path. Setting
   the mount `path` to the Secret **name** makes the two coincide.

The fix in `values.yaml` sets `authentication_backend.ldap.enabled: true`,
`password.secret_name: naslos-openldap` + `path: service-password`, and lists the
Secret under `secret.additionalSecrets` with `path: naslos-openldap` and an
`items` entry that mounts only the `service-password` key. `authelia-config.yaml`
no longer writes `password` at all; the `lookup` stays only as a fail-early
presence check.

This time it was proved on the rendered manifest **before** the live upgrade:
`scripts/audit.sh` now asserts the env var, its
`/secrets/naslos-openldap/service-password` value, the matching `mountPath`, and
the absence of an inline `password:` in `authelia-config` — that one check would
have caught both bugs above. Live at revision 41: Authelia rolled out Ready with
the env var and mount, the ConfigMap no longer contains the bind password,
anonymous requests still 302 to the portal (`/api/health` 200), and
`auth.setup` plus the LDAP-backed users/groups specs pass (8 passed).
| M4 | No NetworkPolicy; flannel does not enforce one; privileged hostNetwork agent `:9090` open to every pod | **Partial** — `charts/naslos/templates/networkpolicy.yaml` records the intent (default-deny + per-workload allow policies, agent `:9090` API-only); **unenforced until a policy CNI is installed**, and the agent/samba/nfs are hostNetwork, which pod-level policy cannot cover | Enforcement needs a node-level rule for hostNetwork pods (Talos/nftables or a Cilium host firewall keyed to the API pod IP), plus the CNI swap |
| M5 | Unused agent ClusterRole (nodes/pods/pods-log cluster-wide) | **Fixed** — `8555925` | ClusterRole + binding removed; the agent has no client-go |
| M6 | Namespace PSA `enforce: privileged` cluster-wide | **Open** | Scope per-workload or split namespaces |
| M7 | API server had no `ReadHeaderTimeout` (Slowloris) | **Fixed** — `3b2a65b` | ReadHeaderTimeout 10 s, ReadTimeout 30 s, IdleTimeout 120 s; `WriteTimeout` 0 for WS |
| M8 | Config files carrying secrets were `0644` in `0755` dirs | **Fixed** — `3b2a65b` | `notifications.json` (ntfy token), peers, schedules, shares → 0600 in 0750 |
| M9 | Buddy store path-taint reports | **Fixed** — `3b2a65b` | One `validateChainName` allowlist + traversal regression test |
| M10 | Mutable image tags, `IfNotPresent`, no digests, plain-HTTP registry | **Excluded** at the operator's request | — |
| M11 | 11 dev-only npm advisories (Svelte/Vite/cookie) | **Fixed (mostly)** — Svelte 4 → 5.57, svelte-check 4, vite-plugin-svelte 4, and xterm → `@xterm` (CR-31) at rev 31; `npm audit` 11 → 4, the 4 left being the dev-server-only esbuild/vite chain. The Svelte/kit `untrack`/`fork`/`settled` build warnings and the xterm deprecation are gone |
| M12 | No CI | **Partial** — `421711a`, workflow removed `b1f7365` | `scripts/audit.sh` packages the sweep; nothing runs it automatically yet |
| M13 | Dead `storage.zfsLocalPV` config | **Fixed** — `8555925` | Block removed; local-path is the only provisioner |
| M14 | Stale audit docs; no lockout runbook | **Fixed** — `421711a`, `65d9b6d` | Superseded banner, status tables, operator-lockout runbook |

### Low

| ID | Finding | Status |
|---|---|---|
| L1 | No `.dockerignore` | **Fixed** — `8555925` (UI context 96 MB → API 717 kB) |
| L2 | UI nginx runs root, writable rootfs | **Fixed** — the UI image is `nginxinc/nginx-unprivileged` (uid 101, listens on 8080) and the pod drops all capabilities with `runAsNonRoot` (rev 29) |
| L3 | privileged/hostPath justification only in comments | **Fixed** — `docs/operations.md` now has a "Privileged workloads and why" table covering every privileged/hostPath workload, and the stale agent-ClusterRole claim there is corrected |
| L4 | Swallowed apply errors | **No change needed** — `applySharesConfig` already logs (`shares.go:370`) |
| L5 | Log-injection findings | **Fixed** — `internal/logsafe.Field` sanitises every request/peer-derived value before it reaches a log line (rev 27) |
| L6 | Integer-conversion findings | **Fixed** — buddy free-space clamps instead of overflowing, `SealChunk` bounds the length field, tar modes are masked (rev 27) |
| L7 | `md4` for the Samba NT hash | **Accepted** — protocol requirement |
| L8 | `go test -race` fails (CR-06) | **Fixed** — the buddy scheduler's notification race is gone (`notificationsMu` guard); `go test -race ./...` passes in both modules, the sweep runs it unconditionally, and api `0.1.0-r8` is live at revision 28 |
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
upgrade). The LDAP bind password was also moved out of the ConfigMap at
revision 41 (see the follow-up above); only `jwt_secret` remains there.

**M5/M7/M8/M9/M13 — chart and API hardening.** Unused agent RBAC dropped; API
read timeouts added; secret-bearing files 0600; chain ids validated with a single
allowlist; dead ZFS LocalPV config removed.

**M6 — Pod Security: the privileged workloads were split out.** Live at revision
55. `naslos-privileged` now holds **naslos-agent** (hostNetwork + hostPath),
**naslos-samba** and **naslos-nfs** (hostNetwork) and **naslos-terminal**
(`privileged: true`); these were previously co-resident with the authenticated
services. The win is isolation: a compromise of api/ui/traefik/authelia no longer
shares a namespace with host-network pods, and the namespace boundary is now a
real one enforced by the default-deny policies rather than a label.

Moving them needed more than a namespace field:

- **Services move with the workloads**, and the API reaches the agent through the
  new namespace: `AGENT_BASE_URL` is now
  `http://naslos-agent.naslos-privileged.svc.cluster.local:9090`.
- **The exec RBAC moves to where the terminal is** — a secretKeyRef cannot cross
  namespaces, so the `naslos-api-terminal` Role/RoleBinding moved to
  `naslos-privileged` with the API (in `naslos`) as the subject. Verified with
  `kubectl auth can-i`: yes for `--subresource=exec` in the privileged namespace
  and **no** in kube-system (least privilege holds).
- **The agent-token Secret is now rendered in both namespaces** from one resolved
  value (the agent reads it beside itself; the API reads its copy beside itself).
  Without this the agent pod crash-looped with `secret "naslos-agent" not found`.
- **The NetworkPolicies were split too**: the privileged namespace gets its own
  default-deny, egress policy and CiliumNetworkPolicy, and the API's ingress
  peers into it became `namespaceSelector` peers. Without the egress copy the
  moved workloads would have lost DNS, LDAP and node access.
- **The UI hardcoded a preference for `naslos`** in the terminal's namespace
  picker, so it landed on a namespace with no shell pod and showed "No pods".
  It now probes the candidates and selects the one that actually holds the
  terminal container (UI rebuilt as `0.1.0-r11`).
- **`naslos` keeps `privileged`**: the API mounts hostPath volumes for the shares
  view and buddy receive, and `baseline` forbids hostPath — the API pod failed
  admission with `FailedCreate: violates PodSecurity baseline: hostPath volumes`
  and the rollout deadlocked. So the split is what M6 actually buys: the
  privileged/hostNetwork set is isolated, not eliminated.

`scripts/audit.sh` now asserts the split (namespace, the four workloads in it,
both Secret copies, the exec Role/RoleBinding placement and subject, and the API
URL) so it cannot silently re-merge. All **35** Playwright specs pass under it,
including the interactive terminal exec into the privileged namespace.

**M4 — NetworkPolicy intent recorded (enforcement still open).** The chart now
renders ten `NetworkPolicy` objects in `charts/naslos/templates/networkpolicy.yaml`:
a namespace default-deny (ingress + egress), per-workload ingress allows for
Traefik (the entry point, hostPort 80/443), API, UI, Authelia, OpenLDAP (636,
API/Authelia/Samba), Samba and NFS, and one namespace-wide egress policy (cluster
DNS + in-cluster pod identities + the service and node CIDRs; no blanket
`0.0.0.0/0`). Enabled by default, gated on `networkPolicy.enabled`, with the
non-pod CIDRs in values (`nodeCIDR`, `ingressPluginsCIDR`, `probeCIDR`,
`serviceCIDR`, `dnsCIDR`, `nfsClientCIDR`).

Two corrections were made after a review of the first cut, both of which would
have broken the appliance the moment enforcement landed: the egress policy relied
on a single `10.0.0.0/8` block, but the node/Talos/agent address
(`192.168.1.117`) and the Service CIDR (`10.96.0.0/12`) fall outside it — egress
now uses `podSelector` peers plus explicit `serviceCIDR`/`nodeCIDR`, and in-cluster
traffic is no longer expressed as an ipBlock (which Cilium does not match to
Pod/Service identities); and `probeCIDR` defaulted to the pod CIDR, which would
have admitted **every pod** to the API/UI/Authelia — it now defaults to empty and
is set explicitly per profile.

**These are inert on the current flannel install** (no policy controller), and
**not a complete fix even after a CNI swap**: the agent, samba and nfs are
`hostNetwork`, and policy CNIs do not apply pod-level NetworkPolicy to
host-network pods, so `naslos-agent-ingress` cannot by itself restrict `:9090`.
Closing M4 needs a node-level rule (Talos/nftables or a Cilium host firewall keyed
to the API pod IP). The policies were verified to render as valid YAML with
selectors matching the live pod labels, and `scripts/audit.sh` now parses the
rendered agent policy and asserts the pod selector, the single API peer, the
`:9090` port, no CIDR peer, and that no policy admits the pod CIDR — a grep-level
check would have passed on an inverted or widened rule.

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
- **Race fix (CR-06, rev 27):** the buddy scheduler test swapped
  `Server.notifications` while a finishing job goroutine read it. The field is
  now guarded (`notificationsMu` + `notificationManager()` /
  `setNotificationManager()`), every reader uses the accessor, and
  `go test -race ./...` passes in both modules; `scripts/audit.sh` runs it
  unconditionally. This was the last blocker to a clean race sweep. Deployed as
  api `0.1.0-r8` at helm revision 28.
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
| Authelia LDAP password is a Secret mount (M3 follow-up) | `scripts/audit.sh` renders the chart and asserts the `AUTHELIA_AUTHENTICATION_BACKEND_LDAP_PASSWORD_FILE` env var, its `/secrets/naslos-openldap/service-password` value, the matching mount, and no inline `password:` in `authelia-config`; live at revision 41 |
| NetworkPolicy intent (M4) | `scripts/audit.sh` parses the rendered agent policy and asserts pod selector, single API peer, `:9090` only, no CIDR peer, and that no policy admits the pod CIDR; 10 policies render as valid YAML with selectors matching the live pod labels; **not enforced under flannel, and hostNetwork pods need a node-level rule** |
| Playwright vs `https://naslos.local` | **35 passed** (repeatedly; Authelia login + 2FA, terminal interactive, backups self-send, users/groups/shares/pools) |
| Live unauthenticated | `/api/*` → 302 to the portal; `/api/health` 200; `:30080` refuses; pod without the proxy secret 401; agent without the token 401 |
| Credentials | new LDAP service and admin bind; old service value and `naslos-admin` rejected |
| Backups | chunks on `test/naslos-buddy`; share PVC unaffected |

## 8. Remaining work

| Item | Severity | Why deferred | Next step |
|---|---|---|---|
| AUDIT-M4 — ~~unenforced CNI~~ **flannel replaced by Cilium; NetworkPolicy enforced**; agent `:9090` hostNetwork rule still open | Low (was Medium) | Cilium **v1.20.2** replaced flannel (Talos `KubeFlannelCNIConfig` deleted, Cilium embedded as a `KubeInlineManifestConfig`, kube-proxy also replaced). Enforcement is live and proven: the terminal pod is now denied at `:8080`/`:9091`/`:9100` by the cilium monitor, while all 7 auth/LDAP Playwright specs pass. **What enforcement exposed and this fixed:** the NetworkPolicy set had no way to allow pods to reach the node/API server (Cilium uses the reserved `host` identity, which `ipBlock` cannot match), so enforcement initially broke every pod — Authelia, Traefik, CoreDNS all failed on `-> <node>:6443 policy denied`; a `CiliumNetworkPolicy` (`naslos-allow-host`, `toEntities: host`/`kube-apiserver`) now covers it. **The one residual:** the agent (and samba/nfs) are `hostNetwork`, and pod-level policy does not cover host-network pods, so `:9090` is still reachable from other pods (~1.5 ms) | **Attempted and rolled back** (see below): a `CiliumClusterwideNetworkPolicy` + `enable-host-firewall` locks the node out if the selector or allow-list is wrong, and recovering needed console access. The template is fixed and **off by default**; do it with console access, in audit mode, and with `hostFirewallAdminCIDR` set |
| AUDIT-M6 — PSA `privileged` namespace | **Fixed** — the hostNetwork/privileged workloads (agent, samba, nfs, terminal) moved to a dedicated `naslos-privileged` namespace; `naslos` still enforces `privileged` **by necessity** (the API mounts hostPath volumes, which `baseline` forbids — verified live), but is no longer co-resident with host-network pods | — | — |
| ~~AUDIT-M11~~ — **fixed**: Svelte 5 + svelte-check 4 + vite-plugin-svelte 4 (`npm audit` 11 → 4, the rest dev-server only), and the xterm → `@xterm` migration (CR-31) is **also done** (`@xterm/xterm ^6.0.0` in `ui/package.json`) | Medium | — | — |
| AUDIT-M3 residual — `jwt_secret` still in the `authelia-config` ConfigMap | Low | The Authelia subchart only mounts a ConfigMap for `configuration.yml` (no Secret equivalent); the LDAP bind password was moved to a Secret at revision 41 | If the chart gains a Secret-backed config mount, move the whole file; otherwise template the pod from the naslos chart |
| AV-8 finding — `zfs send` captures only the agent's mount namespace | Low | Data written from a terminal-pod namespace is invisible to the agent's `zfs snapshot`, so a "successful" backup can be metadata-only; the send/verify path reports success regardless | Document, or add a content/byte sanity check against the holding dataset; the supported flow (writes through the agent/Samba path) is unaffected |
| AV-8 finding — a dataset once mounted in a pod namespace can become undestroyable | Low | `zfs destroy` reports `dataset is busy` with `mounted=false`, no snapshots, no children and no share, and it survives a node reboot; the API/agent expose no `zfs unmount`/`-f` path, so it cannot be cleared through the product | Add a force/`-f` destroy (or an unmount endpoint) to the agent; a stray stub dataset is the only impact |
| ~~AUDIT-L2~~ — **fixed at revision 29**: unprivileged nginx (uid 101, port 8080), all capabilities dropped, `runAsNonRoot` | Low | — | — |
| ~~AUDIT-L9 remainder~~ — **fixed at revision 25**: `.Release.Namespace` migration + `values.schema.json` | Low | — | — |
| ~~AUDIT-L5/L6~~ — **fixed at revision 27**: `logsafe.Field` sanitises log arguments and the conversions are bounded/clamped | Low | — | — |
| ~~AUDIT-L8 / CR-06~~ — **fixed at revision 27**: the scheduler race is gone and the sweep runs `go test -race` | Low | — | — |
| Batch 6 — **staticcheck, the `trivy` image + config scan, `semgrep`, the AV-5…AV-12 active tests and the buddy crypto deep-dive all done; two config findings fixed (DS-0031, KSV-0053)** | Coverage | Time-boxed session | Remaining is unchanged and unrelated to Batch 6: M4 enforcement, the M6 split and the M3 residual; the trixie base-CVE backlog is upstream (no Debian fix) |

### Batch 6 — staticcheck (API), results

`staticcheck ./...` on the API returned 7 findings, no high severity. **All are
now resolved and the scan is clean:**

| Finding | Resolution |
|---|---|
| `server/apps.go:152` `handleAppStartStop` unused (`U1000`) | **Deleted** — it was a "not yet implemented" stub with no route and no UI caller (the UI never requests `/start` or `/stop`). A comment records what a real implementation needs. |
| `identity/client.go:279` `base64Encode`, `server/users_extra.go:232` `contains`, `users_extra.go:267` `smbUsers` unused (`U1000`) | **Deleted**, along with the now-unused `encoding/base64` import. |
| `internal/talos/client.go:221,272` loop unconditionally terminated (`SA4004`) | **Rewritten** as `if len(...) > 0 { m := ...[0]; … }`, which says what it means. |
| `identity/persons_extra.go` `md4` deprecated (`SA1019`) | **Annotated** with `//lint:ignore SA1019` and the reason: MD4 is required by the SMB NT-hash protocol (L7), not used for security. |

`go build`/`vet`/`test` and `staticcheck` are all green.

#### Batch 6 — trivy (API image), results

`trivy image --severity HIGH,CRITICAL naslos-api:0.1.0-r8`:

- **Debian base (`debian 12.15`): 0 vulnerabilities.**
- **`app/naslos-api` (Go binary): 2 HIGH, 0 CRITICAL** — both in
  `oras.land/oras-go/v2 v2.6.1` (CVE-2026-50163, CVE-2026-85731: crafted-tarball
  hardlink information disclosure / arbitrary file access), **fixed in 2.6.2**.
  `govulncheck` had not flagged these (no reachable call path), which is why the
  image scan matters.
- **Fixed:** the dependency is bumped to **v2.6.2**; `go build/vet/test` pass. The
  image must be rebuilt (`0.1.0-r9`) for the fix to reach production.

`trivy image --severity HIGH,CRITICAL naslos-ui:0.1.0-r9` reports **37 (35 HIGH,
2 CRITICAL)** — all in the **alpine 3.21.3 base** of
`nginxinc/nginx-unprivileged:1.27-alpine`, none in our own content:

- OpenSSL `3.3.3-r0` (including CVE-2026-31789, the CRITICAL), libexpat 2.7.0,
  libpng 1.6.47, libxml2 2.13.4, musl 1.2.5-r9, nghttp2 1.64.0, zlib 1.3.1-r2,
  c-ares 1.34.5 — all `fixed` by newer Alpine packages.
- **Practical exposure is low**, which is why this is recorded rather than
  hot-fixed: this nginx serves static files over **plain HTTP inside the
  cluster** (Traefik terminates TLS, so nginx never parses certificates), and
  expat/libpng/libxml2 are not on its request path (it neither decodes images
  nor parses XML). The CRITICAL OpenSSL issue is a 32-bit-only heap overflow and
  we run x86_64.
- **Fixed and re-scanned (rev 32, ui `0.1.0-r10`):** the base moves from
  `nginx-unprivileged:1.27-alpine` (alpine 3.21.3) to **`1.30.5-alpine` (alpine
  3.24.1)**, which carries the patched openssl/libcrypto3, libexpat, libpng,
  libxml2, musl, nghttp2 and zlib in one step. `trivy` on the rebuilt image:
  **0 HIGH/CRITICAL**.

**`naslos-agent` (fixed, rev 33, `0.1.0-r4`):** the final stage moved from
`alpine:3.21` (the same generation as the old UI base) to **`alpine:3.24`**;
`trivy` on the rebuilt image reports **0 HIGH/CRITICAL** on both the base and the
Go binary. The DaemonSet rolled out cleanly on the privileged hostNetwork agent.

**`naslos-samba` (`0.1.0-r1`): 146 findings (137 HIGH, 9 CRITICAL)** — all in
Debian bookworm packages (samba 4.17.12, python3.11, util-linux, perl, pcre2,
zlib), none in our own content. Two things matter here:

1. **The rebuild does not help, and the earlier "stale image" reading was
   wrong.** I rebuilt `0.1.0-r2` with `--no-cache` (so `apt-get update` fetched
   the current bookworm lists and pulled the newest point releases — libssl3
   `3.0.20-1~deb12u2`, libexpat1 `deb12u3`, libxml2 `deb12u6`, python3.11
   `deb12u8`, krb5 `deb12u5`, libarchive13 `deb12u5`) and re-scanned: **the
   counts are unchanged at 137 HIGH / 9 CRITICAL**. Trivy's `fixed` value means a
   fix exists in a **newer Debian release**, not that bookworm has one; Debian 12
   is still carrying these. So `r2` was not deployed (no benefit), and the real
   options are a **distro jump to Debian 13 (trixie)** — which brings Samba
   4.17 → 4.22/4.23 and needs its own validation, not a drop-in — or accepting
   the set with the triage below. The base image itself (`debian 12.15`) is
   current, so there is no staleness to fix.
2. **Several look scarier than they are here**, and the triage matters before
   acting:
   - `CVE-2026-58221` (LDB special DNs → domain takeover), `CVE-2026-58222`
     (AD LDAP Compare filter injection) and `CVE-2026-6949` (TSIG DNS crash) are
     **AD-DC** issues. This Samba is a standalone file server (workgroup
     `NASLOS`): no AD DC role, no internal DNS/TSIG, and LDAP is the API/Authelia
     identity backend, not a Samba backend.
   - The CRITICAL `zlib` `CVE-2023-45853` is `will_not_fix` in Debian and affects
     `zipOpenNewFileInZip4_6`, i.e. the `zip` utility path, not the zlib
     decompression Samba uses.
   - The `util-linux` HIGHs (`mount` TOCTOU, `nsenter`, `X-mount.*`) need
     privileged mount operations, which this container does not perform.

**Debian 13 (trixie) migration — done for samba, rev 34.** Trixie dropped the
Python `wsdd` package, so the jump needed more than a base tag:

- `samba/image/Dockerfile` moves to `debian:trixie-slim` and installs **`wsdd2`**
  (the C implementation of the same WSD daemon, `wsdd2 1.8.7`).
- `entrypoint.sh` picks the binary and its flags per base — `wsdd2 -n … -w …`
  (no `-4`/`-s`) or `wsdd -n … -w … -4 -s` — so the same source still runs on
  bookworm if anyone reverts the base.
- The image now carries **Samba 4.22.11** (from 4.17.12).

**Verified live at revision 34:** the DaemonSet rolled out, the logs show the
full startup (`imported SMB accounts`, `all 1 mirrored accounts resolve through
NSS`, `starting smbd`, `advertising _smb._tcp as naslos.local (mDNS)`,
`advertising via WSD (wsdd2)`), and `smbclient -L localhost -N` lists `IPC$`
with "SMB1 disabled" (expected; shares are created per-share by the API).

**Security outcome, `trivy` HIGH/CRITICAL: 146 → 62** (137 HIGH + 9 CRITICAL →
**58 HIGH + 4 CRITICAL**), the remainder being findings that Debian 13 still
marks `affected`/`fix_deferred` (notably inside Samba 4.22 itself). The AD-DC
triage above still applies to most of those. The same migration is now a known
recipe (base + any removed package + a flag review) for `terminal`, `nfs` and
`openldap`.

**The other three Debian images: `terminal` rolled, `nfs` and `openldap`
rolled back — Debian 13 does not work for them as-is (rev 35).** All three
Dockerfiles built on `debian:trixie-slim` and their `0.1.0-r3` images push and
run; the difference is the runtime:

- **`terminal` — fine.** The shell image runs on trixie (e2fsprogs 1.47, curl
  8.14, bind9-dnsutils) and is live at `r3`.
- **`nfs` — root-caused and fixed (`0.1.0-r3`, rev 37).** Ganesha 6.5 was not a
  config problem: it refuses to start because of a startup `prctl`, and like
  slapd it hides the reason in syslog. Running it against the real config with
  `-F -L /dev/stdout` showed:

  ```
  FATAL :Failed to PR_SET_IO_FLUSHER with EPERM. Take a look at config option
  allow_set_io_flusher_fail to see if you should allow it
  ```

  Ganesha 6.x marks itself an IO flusher at startup; `PR_SET_IO_FLUSHER` needs
  **`CAP_SYS_RESOURCE`**, which the container did not have (the bookworm image's
  Ganesha 4.x never called it, which is why this only appeared now). The
  DaemonSet adds that capability, and Ganesha 6.5 then starts with the *same*
  rendered `ganesha.conf` — no config migration needed. Verified live: the
  rollout completed and the logs show `ganesha.nfsd running (pid 9), NFSv4 on
  :2049`.

  The honest caveat: raising a capability is a real (if small) privilege change
  for a container that already runs hostNetwork with several file-ownership
  capabilities, and it is the alternative to Ganesha's own
  `allow_set_io_flusher_fail` option (which is not portable back to 4.x, which is
  why the capability was chosen).
- **`openldap` — root-caused and fixed in the image (`0.1.0-r4`).** The failure
  was **not** the PVC data: a fresh pod with no volumes failed identically.
  Debian's slapd logs to syslog, so the container showed only `Starting slapd...`
  and exit 1; running it with `slapd -d 1` revealed the cause:

  ```
  unable to open pid file "/var/run/slapd/slapd.pid": 2 (No such file or directory)
  ```

  Debian 13's slapd defaults `olcPidFile` to `/var/run/slapd/slapd.pid` and relies
  on `systemd-tmpfiles` to create the directory — which never runs in a
  container. Bookworm did not need it. The Dockerfile now creates
  `/var/run/slapd` (owned by `openldap`) **after** the `slapd` package install
  (that is what creates the user — the first attempt failed with
  `chown: invalid user`). Verified: a fresh `r4` pod becomes **Ready** with no
  volumes. It is pushed; the live StatefulSet still runs `r1`.
  **Flipped live (rev 38).** The StatefulSet, the bootstrap Job and the backup
  CronJob now pin `r4`, the pod is **Ready against the existing PVC with 0
  restarts**, and the LDAP-backed specs pass (users, groups, password change:
  7 passed). Two slapds must never open the same LMDB directory, which is why
  the pre-flip probe deliberately used no volumes; the rollback path is
  re-applying the `r1` manifest and deleting the pod. Note for future Job edits:
  a Job's `spec.template` is immutable, so the bootstrap Job must be deleted
  before re-applying it (it is recreated by `deploy-vm.sh`).

#### Batch 6 — active tests (AV-5…AV-12), results

AV-5/AV-6/AV-7/AV-9/AV-10 run as Go tests against the real router with the owner
auth gate armed (`api/internal/server/active_tests_test.go`); AV-7 also has a
live check, AV-8/AV-11/AV-12 are live drills (driven against the running VM).
All pass, and each unit guard was mutation-tested to confirm it is not vacuous.

| Test | What it pins | Result |
|---|---|---|
| AV-5 — IDOR / path traversal | A caller-supplied object name on a path route (`/api/shares/`, `/api/apps/`, `/api/catalog/`, `/api/volumes/zfs/`) with `../`, encoded slashes and double-encoding never returns 2xx | pass (10 payloads × 4 routes) |
| AV-6 — injection into Kube params | `namespace`/`pod` on `/api/pods` and `/api/ws/exec` carrying a slash, whitespace or shell metacharacters is refused with an early **400**; loosening `validateKubeName` makes it fail | pass; mutation-confirmed |
| AV-7 — WebSocket origin spoof | `sameHostname` rejects a foreign Origin and accepts the proxy-hop port differences (existing `TestSameHostname`); live, the proxy answers 302 to a forged-origin upgrade before any handshake | pass |
| AV-9 — terminal scoping | `/api/ws/exec` with no namespace uses the API's own namespace and refuses a missing pod; a different namespace never resolves to 200 | pass |
| AV-10 — secret leakage | The proxy secret, agent token and LDAP password never appear in the reachable bodies (`/api/buddy/status`, `/api/notifications`, `/api/shares/config/samba`, `/api/health`); live anonymous probes of `/api/health`, `/api/ready`, `/api/buddy/v1/status` and the 302 error bodies are clean | pass |

**AV-8 (buddy) — live drill, pass, with two platform findings.** Driven against
the running VM through the API (terminal pod → API with the proxy secret, the
same stack a page send uses):

- The **full send → verify → restore → read-back** round-trip passes live. On a
  dataset whose content is written in the **agent's** mount namespace
  (`test/audit-av8b` with `probe-dir` created through the agent's shares API),
  the send succeeded, the verify reported both chains with their SHA-256 digests
  (`d7f6e516a9bc54af` / `c28056acd496f098`), the restore reported `restored`, and
  the restored dataset (`test/audit-av8b-restore`) **contains `probe-dir`** —
  confirmed through the agent's own namespace. All 7 of `ui/tests/backups.spec.ts`
  also pass with `NASLOS_RECEIVER_URL=http://naslos-api:8080` (without it, the two
  self-send specs fail on `lookup naslos.local on 10.96.0.10:53: no such host` —
  the documented mDNS limitation, not a code fault).
- The **replay / tamper / oversize / nonce / quota** protections are unit-covered
  in `internal/buddy` and were reviewed in the deep-dive below
  (`TestEnvelopeRejectsTampering`, `TestReceiverRefusesTamperedChunk`,
  `TestReceiverRefusesChainRollback`, `TestNoncesSurviveARestart`,
  `TestQuotaIsAtomicUnderConcurrentWrites`).
- **The drill proved the mount-propagation guard fires correctly.** A dataset
  created from inside a pod is offered by the agent as `mounted=false`, and
  `requireMountedDataset` refused the send with the documented message rather than
  capturing an empty filesystem. After a node reboot the same dataset reports
  `mounted=true` and the send proceeds — the documented remedy, verified.

Two findings the drill surfaced (both operational, neither a crypto issue):

1. **`zfs send` snapshots only what the *agent* namespace sees.** Seeding a
   dataset's content from the terminal pod writes to the terminal's mount
   namespace; the agent's `zfs snapshot` then captures an empty filesystem and the
   "backup" is metadata only (44 KB) — while every API call reports success. This
   is the same trap `requireMountedDataset` guards for the *dataset* but not for
   the *content*: the guard checks `mounted`, and the dataset can be `mounted` in
   the agent's view yet still hold files written where the agent cannot see them.
   Worth a doc note (and, arguably, a content check); recorded here rather than
   fixed, since writing share data through the agent/Samba path is the supported
   flow.
2. **A dataset once mounted inside a pod namespace can become undestroyable.**
   `test/audit-av8` now reports `mounted=false` with no snapshots, no children and
   no share referencing it, but `zfs destroy` fails with `dataset is busy` — and
   it **survives a node reboot** (ZFS replays the stale mount record). The API and
   agent expose no `zfs unmount`/`-f` path, so it cannot be cleared through the
   product. Low severity (it leaks a stub dataset, no data, no exposure), but a
   real gap: there is no supported way to force-destroy a dataset whose mount
   record outlives its namespace. Candidate residual for a follow-up window.

**AV-11 (TLS/session) — live, pass.** TLS 1.3 is the only protocol offered
(`-tls1`/`-tls1_1` are refused); the cert is the `naslos-local-ca`-issued
`CN=naslos.local` with the matching SAN (valid to 2036); `Strict-Transport-Security:
max-age=31536000; includeSubDomains`, `X-Frame-Options: DENY`,
`X-Content-Type-Options: nosniff` and `Referrer-Policy: strict-origin-when-cross-origin`
are set; HTTP 301-redirects to HTTPS; and a forged `X-Forwarded-Host` or a
client-supplied `Remote-User`/`Remote-Groups` still gets 302 (the proxy secret
is the only trust proof — NAS-009 holds).

**AV-12 (rolling restart) — live, pass.** `kubectl rollout restart
deploy/naslos-api` rolled out cleanly; continuous `/api/health` polling showed a
single ~1s gap during the pod swap and 200s on either side, with no crash-loop or
stuck rollout. The new pod (0 restarts) serves `/api/health` 200, and the
LDAP-backed Playwright specs (`auth.setup` + `users.spec`, 5 passed) still log in
and edit users afterwards, so the restart did not break identity.

#### Batch 6 — buddy crypto deep-dive (`envelope.go`, `keys.go`, `auth.go`, `store.go`)

A line-by-line review of the Buddy Backup crypto and storage layers against the
security-review guide, plus the existing `-race` test suite. **No findings.**
The design is sound and the asymmetric trust model holds.

| Area | What was checked | Assessment |
|---|---|---|
| Chunk confidentiality | AES-256-GCM (`aeadFor` refuses any key that is not 32 bytes); nonce is `prefix(8)‖counter(4)` so it is unique per (DEK, index); `validateChunkIndex` refuses index ≥ 2³² with the counter-wrap named as the reason (NAS-021) | sound |
| Chunk integrity / binding | AAD is `NB1\|source\|chain\|index\|plainLen`, so a chunk cannot be reordered, moved between sources or chains, or have its declared length changed; `OpenChunk` also checks the nonce matches the index, the magic, the declared length ≤ `ChunkPlainSize`, and the decrypted length. Cross-source, cross-chain and wrong-index swaps are pinned by `TestEnvelopeRejectsTampering` | sound |
| Data-key wrapping | `WrapDEK`/`UnwrapDEK` seal the per-segment DEK under the owner's KEK with AAD bound to source+chain; the receiver holds no KEK, and `TestDEKWrapNeedsTheOwnersKEK` proves a stranger's key (and a different chain) cannot unwrap it — the zero-knowledge property | sound |
| Manifest authenticity | `Sign`/`VerifySignature` over the canonical form (signature blanked), Ed25519; the verifier checks the manifest's `keyId` equals the fingerprint of the authorized key, so a manifest cannot claim another key. `validateManifestShape` refuses a duplicate/out-of-order index, an impossible chunk size, a missing/mis-encoded prefix or wrapped key, and an unknown kind (NAS-012) | sound |
| Identity at rest | Ed25519 in OpenSSH form; `Save` writes 0600 atomically via a temp file + rename; `KEKBytes` enforces 32 bytes | sound |
| Request authentication | Ed25519 over `BUDDY1\nMETHOD\npath\ndigest\ntimestamp\nnonce` — method, path, body digest, timestamp and nonce are all signed, so a captured request cannot be retargeted; ±5 min clock skew; the receiver rebuilds `path` from the mount prefix rather than trusting the request line | sound |
| Replay defence | Nonce is burned only *after* the signature verifies (so junk cannot exhaust a peer's cache); format bounded (16–64 decoded bytes, ≤128 chars); per-key cap of 4096 with soonest-expiry eviction; persisted to `.nonces` and reloaded unexpired on restart (NAS-014) | sound |
| Storage / path safety | Key directories are `sha256(fingerprint)` (not the raw fingerprint, whose `/` once created nested dirs); `ValidateSource` and `validateChainName` reject traversal, absolute paths and a `..` run (AUDIT-M9, NAS-021); quota is reserved under the store lock before the write, so concurrent uploads cannot both pass a pre-write check | sound |

Residual noted, not a finding: the per-key nonce cap means a peer that sends more
than 4096 requests inside the 10-minute TTL evicts its own soonest-to-expire
nonces, so a replay of an evicted nonce is theoretically possible in that window.
Every operation a replay could repeat is idempotent (a chunk write is
digest-checked; a manifest cannot roll the pointer back — pinned by
`TestReceiverRefusesChainRollback`), which is why the bound is the right
trade-off; it is documented in `auth.go` rather than left implicit.

#### Batch 6 — trivy image scan, all seven images (trivy 0.73.0)

`trivy image --scanners vuln --severity HIGH,CRITICAL` against the live tags:

| Image | Base | HIGH/CRITICAL | Where |
|---|---|---|---|
| `naslos-api:0.1.0-r9` | debian 12.15 | **0** | base + Go binary clean |
| `naslos-ui:0.1.0-r10` | alpine 3.24.1 | **0** | base clean |
| `naslos-agent:0.1.0-r4` | alpine 3.24.2 | **0** | base + Go binary clean |
| `naslos-terminal:0.1.0-r3` | debian 13.7 | 65 | base libs only |
| `naslos-samba:0.1.0-r3` | debian 13.7 | 62 | base libs only |
| `naslos-nfs:0.1.0-r3` | debian 13.7 | 47 | base libs only |
| `naslos-openldap:0.1.0-r4` | debian 13.7 | 45 | base libs only |

**None of the 219 findings on the four trixie images is in a service package** —
they are all Debian base libraries (`util-linux`, `systemd`, `ncurses`, `perl`,
`expat`, `curl`, `libxml2`, `libacl`), and **every one reports `Fixed in: None`**:
Debian 13 has no published fix yet, so a rebuild cannot reduce the count. This is
the same shape the report already records for bookworm: the table's "fixed
version" column is the version in a *newer* Debian release, not a fix in trixie.
The trixie Dockerfiles now run `apt-get dist-upgrade` before the package install
so a future security-pocket update is picked up on the next build; today the
installed versions already equal the newest trixie offers
(`util-linux 2.41.5-0+deb13u1`, `libsystemd0 257.13-1~deb13u1`).

#### Batch 6 — trivy config scan (chart, Dockerfiles, openldap manifests)

Two genuine findings, both fixed:

- **DS-0031 (CRITICAL) — `ENV LDAP_ADMIN_PASSWORD="admin"` in
  `openldap/image/Dockerfile`.** A baked default means a deployment that fails
  to wire the Secret silently runs with a documented credential — the NAS-010
  class. The StatefulSet supplies it from the `naslos-openldap` Secret and
  `entrypoint.sh` requires it (`:?`), so the default is removed; the image now
  fails closed. Verified: the rebuilt image carries no `LDAP_ADMIN_PASSWORD`.
- **KSV-0053 (HIGH) — the `naslos-openldap-bootstrap` Role granted
  `get/list pods` and `create pods/exec`.** The Job only runs
  `ldapmodify`/`ldapsearch` and sets `automountServiceAccountToken: false`, so
  neither rule was used; `pods/exec` is a known escalation path. The Role and its
  RoleBinding are deleted, leaving the ServiceAccount with no permissions.

Recorded, not fixed (they need a live window or are inherent):

- **KSV-0014 / KSV-0118 (HIGH) on the openldap workloads** — no
  `readOnlyRootFilesystem` and a default (root) security context. slapd writes
  `/var/run/slapd/slapd.pid`, which is not a mounted volume, so
  `readOnlyRootFilesystem: true` would break it unless an emptyDir is added;
  openldap/samba are root by design. These belong with the M6 PSA work, where
  the security contexts are set deliberately.
- **DS-0002 (HIGH) on every Dockerfile** — the last `USER` is root (or there is
  none). The agent is privileged, samba needs setuid to map SMB sessions, and the
  terminal is root by design (documented in AUDIT-L3); the API, UI and buddy
  receiver already run non-root.

#### Batch 6 — semgrep (`--config=auto`)

Four findings, **all false positives**, each verified by hand:

| Rule | Location | Why it is not a finding |
|---|---|---|
| `decompression_bomb` | `api/cmd/buddyctl/main.go:743` (`io.Copy` in the tar restore) | `buddyctl` is a local owner-run tool restoring the owner's own backup; the tar header declares the entry size. Noted as a possible hardening, not a vulnerability |
| `no-direct-write-to-responsewriter` | `api/internal/buddy/http.go:353` | the body is a sealed ciphertext served as `Content-Type: application/octet-stream`, never HTML |
| `no-direct-write-to-responsewriter` | `api/internal/server/shares.go:383,393` | both handlers set `Content-Type: text/plain`; the generated samba/ganesha config is not rendered as HTML |
| `last-user-is-root` | `agent/Dockerfile:23` | inherent: the agent is privileged by design (AUDIT-L3) |

#### Batch 6 — the audit sweep now gates on SAST

`scripts/audit.sh` runs `govulncheck` and `gosec` again (both were installed).
Two gates were made honest rather than left failing on accepted residuals:

- **govulncheck** reports the four AUDIT-H3 advisories (all `Fixed in: N/A`) and
  **fails only when the advisory set changes** — a new ID is a new reachable
  vulnerability. Negative-tested: dropping one accepted ID from the allow-list
  makes the sweep fail, naming the ID.
- **gosec** excludes `G101` (flags the `X-Naslos-Proxy-Secret` header *name* as a
  credential; `#nosec` with the reason is also in the source) and `G703`
  (path-traversal taint false-positives in `buddy/store.go`: the tainted chain
  names come from `os.ReadDir`, which cannot yield `/` or `..` components, and
  peer-supplied names pass `validateChainName` first). A new HIGH rule still
  fails the gate.

#### AUDIT-M4 — the flannel network-policy path was tried and does not work on this build

Aimed at closing M4 without a CNI swap, `kubeNetworkPoliciesEnabled: true` was
added to the `KubeFlannelCNIConfig` document (Talos 1.13+ documents this as
enabling NetworkPolicy enforcement for flannel). It was applied live, the node
was rebooted, and the Talos installer was upgraded v1.14.0 → v1.14.1 with the
same schematic. Result: **no enforcement.** The evidence, each checked directly:

- The setting is accepted and persisted (`talosctl get machineconfig` shows
  `kubeNetworkPoliciesEnabled: true`), and the node reports Talos **v1.14.1**.
- The applied `05-flannel` manifest renders only five objects
  (`ClusterRole`/`ClusterRoleBinding`/`ServiceAccount`/`ConfigMap`/`DaemonSet`
  flannel) — **no `kube-network-policies` DaemonSet**.
- The node's image bundle carries no companion image (`talosctl image list`
  shows `ghcr.io/siderolabs/flannel` only; a grep for `network` is empty), so
  even a rendered companion could not start.
- Live probe: the terminal pod reaches the agent `:9090` in ~1.5 ms with the
  policies deployed, i.e. still inert.

So on this Talos build the flannel policy feature is a no-op, and the setting was
removed from `bootstrap/vm/controlplane.yaml` (with the reason in a comment) so
the repo does not claim enforcement it does not get. Closing M4 needs the Cilium
or Calico swap (or a Talos build whose bundle ships the companion), plus a
node-level rule for the hostNetwork agent. This is the documented outcome of the
enforcement attempt, not a regression — the 10 policies remain deployed and
correct, and enforce the moment a policy-capable CNI lands.

#### AUDIT-M4 — flannel replaced by Cilium (enforced, verified live)

Following that failed flannel-policy attempt, the CNI was replaced. The Talos
`KubeFlannelCNIConfig` document was deleted, `forwardKubeDNSToHost` set to
`false` (the documented Cilium+masquerade CoreDNS issue), and **Cilium v1.20.2**
embedded as a `KubeInlineManifestConfig` so it is re-applied on every control
plane boot; kube-proxy was replaced too (`kubeProxyReplacement: true`, after
kube-proxy looped on stale nftables chains once flannel was removed).

Two things had to be cleaned up that a config-only change does not cover:

- **Stale flannel datapath.** `flannel.1` and `cni0` (`10.244.0.1/24`) survived
  the CNI switch and shadowed Cilium's `cilium_host` routing. They are in-kernel
  state, not config, so they were deleted from the host network namespace (via a
  privileged `hostPID` pod); after that the node had the single correct route
  `10.244.0.0/24 via cilium_host`.
- **The policies did not allow the node.** This is the substantive finding.
  Plain Kubernetes `NetworkPolicy` **cannot** allow pods to reach the node /
  API server: Cilium classifies that traffic with the reserved `host` identity,
  which an `ipBlock` does not match, and `toEntities` is not in the upstream API.
  Turning enforcement on therefore blocked every pod from `https://<node>:6443`
  — Authelia, Traefik, CoreDNS and the operators all failed, and the cilium
  monitor showed `Policy denied ... -> 192.168.1.117:6443 tcp SYN` repeatedly.
  A `CiliumNetworkPolicy` (`naslos-allow-host`: `toEntities: [host,
  kube-apiserver, remote-node]` plus the CoreDNS entity) fixes it; the upstream
  egress rule for DNS was also widened from the link-local CIDR to a
  namespaceSelector on kube-system, because under Direct Routing an `ipBlock`
  alone does not match CoreDNS.

**Verified live, enforced:** `/` → 302, `/api/users` → 302, `/api/health` → 200;
the terminal pod is now denied at `:8080`/`:9091`/`:9100` in the cilium monitor
(previously reachable); all 7 auth + LDAP Playwright specs pass under
enforcement. `scripts/audit.sh` asserts the `CiliumNetworkPolicy` and its
`host`/`kube-apiserver` entities so the allow cannot silently disappear.

**Residual (Low):** the agent, samba and nfs are `hostNetwork`, and pod-level
policy does not apply to host-network pods, so the agent's `:9090` is still
reachable from other pods (~1.5 ms).

**Host-firewall attempt — FAILED and rolled back (2026-09-20).** The documented
way to restrict a hostNetwork pod is Cilium's host firewall
(`enable-host-firewall`) plus a `CiliumClusterwideNetworkPolicy` with a
`nodeSelector`. It was attempted live and **locked the node out**:

- The host firewall was enabled and the policy validated in audit mode, which
  showed **zero** would-be denials for ingress, DNS, LDAP, the agent, kubelet
  and the SMB/NFS/443 LAN probes.
- But the `nodeSelector` (`kubernetes.io/os: linux`) **matched no host
  endpoint**: Cilium derives only a subset of node labels onto the
  `reserved:host` endpoint, and the well-known `kubernetes.io/*` labels are not
  among them (`cilium-dbg endpoint list` showed only the Talos extension labels
  and `node-role.../control-plane`).
- The Cilium agent was then restarted to pick up a new node label — and this is
  the trap: **audit mode does not survive an agent restart** (the Cilium docs
  warn about exactly this), so the policy began enforcing immediately. With the
  selector unmatched, node ingress became default-deny, dropping the **Talos API
  (50000)** and the **k8s API (6443)** and locking out both `talosctl` and
  `kubectl`.

Recovery needed out-of-band console access (VNC): the VM had also rebooted from
the attached installer ISO rather than its disk, so it had to boot from
`/dev/vda`, after which the CCNP was deleted, `enable-host-firewall` set back to
`false`, and the node label removed. The cluster recovered fully (all workloads
1/1, `/` → 302, `/api/health` → 200, the auth/LDAP specs pass) and `:9090` is
back to the documented Low residual.

The template is kept (it is the right construct) but **defaults to off**, with
the two lessons encoded: select a **dedicated** node label (not
`kubernetes.io/os`) and verify `POLICY (ingress) Enabled` on the `reserved:host`
endpoint, and set `hostFirewallAdminCIDR` so the operator's 50000/6443 path
survives a mistake. A safer alternative not yet tried is a **Talos host rule**,
which does not depend on Cilium's label propagation.

**Second attempt (same day) — got further, still did not attach.** Retried
without the lockout this time (no CCNP existed when the agent restarted, and
`hostFirewallAdminCIDR` kept `kubectl`/`talosctl` alive throughout; the revert
was a clean one-liner with no console needed). Findings:

- **`enable-node-selector-labels: true` is required** for node labels to reach
  the host endpoint; with it off (the default) the endpoint shows only the Talos
  extension labels, which is why the first attempt's selector never matched.
- The label then appears on the `reserved:host` endpoint, but **only after an
  agent restart** (the `CiliumNode` carries it; the endpoint derives it on
  restart).
- **The remaining blocker is a label-source mismatch**: the endpoint label is
  `k8s:naslos.io/host-firewall=true`, while Cilium stores the policy's
  `nodeSelector` as `any:naslos.io/host-firewall` (`cilium-dbg policy get`), so
  the host endpoint still matches nothing (`ingress: {}`) even with the label
  present. Neither `naslos.io/host-firewall` nor `k8s:naslos.io/host-firewall`
  as the selector matched. Resolving this needs Cilium's node-selector
  label-source semantics, which the docs do not spell out — the next attempt
  should start there rather than re-running the rollout.
- **No Talos fallback exists:** Talos v1.14.1 exposes no `HostFirewallConfig`,
  `IngressFirewallConfig` or `NetworkRuleConfig` resource, so the "Talos host
  rule" alternative is unavailable on this version.

State after the second attempt is the known-good one: no CCNP, host firewall and
node-selector-labels both `false`, node label removed, `/` → 302 and
`/api/health` → 200.

**Fresh-install parity.** The CNI change lives in the machine config, which
`talosctl gen config` regenerates and which is gitignored, so it was landed where
a fresh install actually reads it:

- `bootstrap/vm/naslos-vm.yaml` (tracked) is the `--config-patch` `make
  bootstrap-vm` applies. It now carries the `KubeFlannelCNIConfig` `$patch:
  delete`, `KubeProxyConfig.enabled: false`, `ResolverConfig.hostDNS.
  forwardKubeDNSToHost: false`, and the `KubeInlineManifestConfig` for Cilium.
- `bootstrap/cilium/cilium.yaml` (tracked) is the pinned Cilium v1.20.2 manifest
  (KPR, KubePrism `localhost:7445`, kube-proxy-replacement).
- `scripts/render-cilium.sh` splices that manifest into the patch as the inline
  document; `make bootstrap-vm` runs it first, so a fresh install ships Cilium
  with no manual step. It is idempotent and has a `--check` mode.
- `scripts/deploy-vm.sh` waits for the `cilium` DaemonSet to be Ready before the
  first `kubectl`/`helm` call (flannel is gone, so there is no pod network until
  Cilium starts).
- The installer is pinned to **v1.14.1**, matching the node the swap was
  validated on.

Verified by simulating a fresh generation (`talosctl gen config … --config-patch
@bootstrap/vm/naslos-vm.yaml`): the result has **no** `KubeFlannelCNIConfig`,
kube-proxy disabled, host DNS forwarding off, and all **25** Cilium objects
embedded. `scripts/audit.sh` asserts the patch carries all of these and is
current with `cilium.yaml`, so a regression fails the sweep rather than silently
producing a stock flannel cluster.

### Debian 13 migration complete — all four images live

`terminal r3`, `samba r3`, `nfs r3` and `openldap r4` are all on
`debian:trixie-slim` at revision 38, and **the chart defaults were moved to the
live tags** (api `r9`, agent `r4`, ui `r10`, samba/nfs/terminal `r3`, openldap
`r4`) so a fresh `helm install`/`make install-vm` — not just this release —
picks up the migrated images instead of the stale `0.1.0` placeholders.

Two findings from the migration are worth carrying into any future base bump:
slapd and Ganesha both **log to syslog**, so a container that exits silently must
be run with debug (`slapd -d 1`, `ganesha.nfsd -F -L /dev/stdout`); and the
failure is usually a **startup assumption the package's systemd/tmpfiles would
have satisfied**, not the app config (`/var/run/slapd` for slapd,
`CAP_SYS_RESOURCE` for Ganesha's `PR_SET_IO_FLUSHER`).
  The earlier recovery sequence, for the record:
  for the record: `rollout undo` alone did **not** help because the StatefulSet
  is `kubectl apply`-managed (its revision history has no usable prior image);
  the working sequence was `kubectl set image` back to `r1`, then **delete the
  stale crash-looping pod** — with `podManagementPolicy: OrderedReady` the
  controller will not replace it on its own while it is not Ready. slapd is
  Ready on `r1` again (0 restarts), and `values-vm.yaml` plus the three
  `openldap/manifests` files are reverted to `r1` so no re-apply reintroduces it.

Lesson for the remaining migration: the base bump is not the risk, the
**daemon's config/data compatibility** is. samba was cheap (one removed
package + a flag), nfs and openldap need their own config/state migration and a
window each, exactly as feared.

**Remaining image scans:** `naslos-terminal`
(`0.1.0-r1`), `naslos-samba` (`0.1.0-r1`), `naslos-nfs` (`0.1.0-r1`) and
`naslos-openldap` (`0.1.0-r1`). `trivy image` accepts one target per run, so run
it per image:

```bash
for img in agent:0.1.0-r3 terminal:0.1.0-r1 samba:0.1.0-r1 nfs:0.1.0-r1 openldap:0.1.0-r1; do
  docker run --rm -v /var/run/docker.sock:/var/run/docker.sock aquasec/trivy:latest \
    image --quiet --no-progress --severity HIGH,CRITICAL "192.168.1.2:30095/naslos-${img}"
done
```

They are the custom images built from `terminal/`, `samba/`, `nfs/` and
`openldap/`; the API image is clean apart from the fixed oras-go pair and the UI
image is now clean, so the expectation is base-image findings of the same shape
(fixable by bumping the base tag, as the UI showed).
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
