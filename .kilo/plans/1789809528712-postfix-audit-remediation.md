# Remediation plan — post-fix audit (2026-09-21)

Source: `docs/AUDIT-2026-09-21-POSTFIX.md` (commit `36ccb6d`, reviewing `master`
at `b9c3321`). Target agent: implementation-capable (Go / Svelte / Helm / git).

This plan is the execution follow-up to an audit produced by another model. I
re-verified every load-bearing claim by hand against the working tree before
writing it; see §0 for the two places my verification disagrees with the audit.

## 0. Verification notes (read first)

Everything cited below was confirmed at the quoted file:line, **except** two
corrections that change severity or labeling:

1. **PF-H1 impact is real but overstated by the audit.** The committed key is
   genuine (`bootstrap/cilium/cilium.yaml:38-46`, mode 0644, valid RSA). But
   Cilium's `certgen` templates are marked
   `cilium.io/helm-template-non-idempotent: "true"` (`cilium.yaml:43`), so a
   **fresh** install regenerates the CA/hubble certs rather than ingesting the
   committed ones. The live exposure is the committed private key itself plus the
   `hubble-server-certs` key, not "MITM on every deployment". Still High — a
   committed private key must be rotated and purged — but plan the fix, and phrase
   the report, accordingly. Note `hubble-disable-tls: "false"` (`cilium.yaml:259`)
   means the committed hubble key genuinely backs a live TLS listener today.
2. **PF-M4 mislabels a deliberate change as a defect.** The API NetworkPolicy's
   broad `namespaceSelector` on `naslos-privileged` was added in `b985e03` as an
   M6 correction. Treat the audit's advice to narrow it as a **hardening
   suggestion**, not a regression; do not "revert" it.

PF-H5 is handled by deferral (see §5): an implementation-ready refactor plan
already exists at `.kilo/plans/1789943277180-charts-repo-and-app-install-refactor.md`.

## 1. Scope decisions (already agreed)

- **PF-H1 history purge is in scope**, but the `git filter-repo` + force-push step
  is **gated**: the implementation agent must stop and get explicit user approval
  immediately before rewriting history and force-pushing. Everything before that
  step can proceed.
- **PF-H5**: defer to the existing refactor plan. Here we only (a) make the docs
  and UI stop claiming the catalog works, and (b) record the missing RBAC.
- **Low findings**: fix only the security-relevant ones (PF-L3, L4, L5, L8, L9,
  L12). The remaining UI/cosmetic Lows are appended as known-open (§6).

## 2. Ordered work

### Step 1 — PF-M1: make the security gate actually run (do this first)

`scripts/audit.sh:1` is `#!/bin/sh` with documented `Usage: sh scripts/audit.sh`,
but line 230 uses bash process substitution:

```sh
new=$(comm -23 <(printf '%s\n' $found | sort -u) <(printf '%s\n' $accepted | tr ' ' '\n' | sort -u))
```

Under dash this aborts, so every check after line 230 (gosec, agent `go vet`/
`-race`, npm audit, svelte-check, helm lint, and the M3/M4/Cilium regression
guards and gitleaks) never runs — which is how PF-H1 shipped.

- Change shebang to `#!/usr/bin/env bash` and the printed usage to
  `bash scripts/audit.sh`.
- Or rewrite line 230 POSIX-ly with two `mktemp` files + `comm -23`. Pick one;
  the shebang change is smaller and matches `deploy-vm.sh`'s existing style.
- Fix the gitleaks invocation (audit §PF-M1): replace
  `gitleaks detect --source=.` (history only, mislabeled "tree + history") with
  both `gitleaks dir .` and `gitleaks git .`.
- Re-run under its documented invocation and confirm it reaches the end.

### Step 2 — PF-H1: rotate/stop committing the Cilium key (code), then purge (gated)

Code/config, do first:
- Stop checking in rendered Secrets. Set Cilium to generate its certs at install:
  `tls.auto.enabled=true` with `tls.auto.method=helm|cronJob`. Re-render
  `bootstrap/cilium/cilium.yaml` so the `cilium-ca` Secret
  (`:36-46`) and `hubble-server-certs` (`:49-63`) are **gone**, and re-run
  `scripts/render-cilium.sh` to refresh `bootstrap/vm/naslos-vm.yaml`.
- Since `render-cilium.sh` splices `cilium.yaml` verbatim (see its content), the
  keys must be removed at the source manifest, not stripped downstream.
- Verify: `gitleaks dir .` reports zero, and `render-cilium.sh --check` passes.

History purge (**GATE: stop for explicit user approval before this step**):
- Confirm working tree is clean and pushed; create a safety branch/tag.
- `git filter-repo --path bootstrap/cilium/cilium.yaml --path bootstrap/vm/naslos-vm.yaml --invert-paths` is **not** right (files must stay) — instead
  `git filter-repo --replace-text` with the two base64 blobs, or
  `--path-glob` + `--replace-text` to scrub the key material from the two files
  across history.
- Force-push the affected branches and coordinate re-clone for any other user.
- Rotate the Cilium CA on any deployed cluster (the live VM) after deploy.

### Step 3 — PF-H2: fix AES-GCM nonce reuse on resume

`api/internal/buddy/client.go:697-702` falls through and re-uploads the
interrupted tail under the **same** `(dek, prefix, index)` — `chunkNonce`
(`envelope.go:79-86`) is a pure function of `(prefix, index)`. The receiver thus
holds two ciphertexts at one nonce.

- Chosen fix: extend the nonce domain and bind it. Nonce becomes
  `prefix(6) || uint16BE(generation) || uint32BE(index)`; add `Generation` to
  `ChainState`, increment it on every resume, and carry it in the signed manifest
  and in `chunkAAD`.
- Update `chunkNonce`, `SealChunk`, `OpenChunk` (`envelope.go`), and the
  `MaxChunkIndex`/`validateChunkIndex` bounds note (`envelope.go:88-100`).
- Also tighten `Store.mayReplace` (`store.go:369-386`) to refuse same-index
  replacement, and pass `manifest.StreamPrefix` into `OpenChunk` (ties into
  PF-L12, Step 8).
- Cheap alternative if the protocol bump is undesirable: make `client.go:697`
  always error and require a fresh chain. **Recommend the nonce-domain fix**, as
  the alternative re-uploads an unpublished chain and the audit prefers the bind.
- Tests: resume-with-different-plaintext must produce a distinct nonce; add a
  negative test that would fail under the old nonce scheme.

### Step 4 — PF-H3: bind `Restore` to the requested source/chain

`client.go:853-871` verifies the signature but never checks the manifest matches
the request, so a malicious buddy can substitute an older/other manifest and every
downstream check (`fetchChunk`, `OpenChunk`, digest compare) uses the
substituted values.

- Immediately after `c.Manifest(...)`: reject unless
  `manifest.Source == opts.Source`, and unless
  `opts.Chain == "" || manifest.Chain == opts.Chain`.
- For freshness, sign a monotonic sequence into the manifest, persist the last
  published value per `(receiver, source)` on the sender, and refuse anything
  older. (Sequence field is a manifest-format addition — coordinate with the
  PF-H2 manifest/AAD changes so both land in one format bump.)
- Test: a validly-signed manifest for a different source/chain must be rejected.

### Step 5 — PF-H4: NFS/SMB default posture

`api/internal/shares/config.go:139-187` defaults to `Clients = *`,
`Squash = No_Root_Squash`, `SecType = sys`; SMB sets `force user = root` /
`force group = root` on every share (`config.go:86-87`).

- Default `allowedHosts` to the node's LAN CIDR (chart already exposes
  `networkPolicy.nfsClientCIDR`); require an explicit opt-in for `*`.
- Default `Squash = Root_Squash`; expose `No_Root_Squash` as a per-share toggle
  with a warning in the UI when selected.
- Replace `force user = root` with the mapped LDAP uid (extrausers/NSS plumbing
  already exists); set `create mask = 0660`, `directory mask = 0770`.
- Global SMB hardening: `map to guest = Never`, `server min protocol = SMB3`,
  `smb encrypt = desired`.
- Note in the report that hostNetwork pods (nfs :2049) are outside
  NetworkPolicy reach — this is the residual from AUDIT-M4, now deserving a
  higher rating.
- Tests: config-render assertions for the new defaults and the opt-in path.

### Step 6 — PF-M2: Go toolchain floor

`api/go.mod:3` and `agent/go.mod:3` declare `go 1.26.5`; six reachable stdlib
advisories are fixed in 1.26.6.

- Bump both `go.mod` files to `go 1.26.6` (or add `toolchain go1.26.6`).
- Update the `accepted` allow-list in `scripts/audit.sh:229` to include the six
  now-fixed IDs so the (now-working) gate passes: GO-2026-5026, 5972, 6089, 6090,
  6091, 6218 — or better, remove them once the bump makes them disappear.
- Confirm `govulncheck ./...` on both modules is clean under the new toolchain.

### Step 7 — PF-M3 / PF-M7 / PF-M8 / PF-M9

- **PF-M3** Grafana: remove the dependency from `charts/naslos/Chart.yaml`,
  regenerate `Chart.lock`, delete `charts/naslos/charts/grafana-7.0.0.tgz`, and
  remove the `grafana:` blocks from `values.yaml` and `values-vm.yaml:120-122`.
  (`README.md:32` already claims it was removed — make that true.)
- **PF-M7** catalog map mutation: `apps.go:67` aliases
  `catalogApp.DefaultValues`. Use `maps.Clone` (nil-safe) before the merge, and
  make `Catalog.Get` return a deep copy (`catalog/catalog.go:107-113`). This
  fixes cross-admin credential disclosure via `GET /api/catalog/{name}`.
- **PF-M8** `/api/volumes` stub (`disks.go:157-181`): either implement it (apply
  the `UserVolumeConfig` via the Talos client) or remove the route and its docs
  (`docs/spec.md:528`, `docs/storage-zfs.md:5-6`). **Recommend: remove the route
  + docs** unless the Talos apply path is already wired; a stub that returns 201
  is worse than 404.
- **PF-M9** dead notification events: either wire `Manager.Send` into the ZFS
  health poll / disk discovery / app status, or remove the six unimplemented
  event types from `notifications.go:14-19` and the settings UI
  (`notifications/+page.svelte:33-41`). **Recommend: wire `zfs_health` and
  `disk_failure`** (the headline NAS monitoring), remove the rest for now, and
  correct `docs/notifications.md`.

### Step 8 — PF-M4 / M5 / M6 + security-relevant Lows

- **PF-M5** agent TLS: serve TLS on `:9090` with a chart-issued cert pinned in
  `api/internal/agent/client.go`; or at minimum bind to the node's internal
  address. (hostNetwork means NetworkPolicy cannot help; the bearer token
  currently crosses the LAN in cleartext.)
- **PF-M6** agent timeouts: mirror the API's `ReadHeaderTimeout`/`ReadTimeout`/
  `IdleTimeout` (`agent/internal/server/server.go:126-129`), with a long/disabled
  `ReadTimeout` only on `/api/v1/zfs/receive/`; add `http.MaxBytesReader`.
- **PF-M4** proxy secret: narrow `TRAEFIK_CIDR` from `10.0.0.0/8` to Traefik's
  actual pod CIDR, and consider a signed short-lived assertion instead of the
  static secret in a `Middleware` CRD. (Treat the NetworkPolicy-narrowing part as
  optional hardening per §0.2.)
- **PF-L3** add `securityContext` (`runAsNonRoot`, `allowPrivilegeEscalation:
  false`, `capabilities.drop: [ALL]`, `seccompProfile`) to the **api** container
  (and any workload that does not need privilege). Note both namespaces enforce
  PSA `privileged`, so nothing else enforces it.
- **PF-L4** set `automountServiceAccountToken: false` on `naslos-terminal`.
- **PF-L5** make `CleanFolderPath` (`agent/internal/shares/folders.go:115-131`)
  resolve symlinks (`filepath.EvalSymlinks` on the root, then a containment check
  on the resolved path) instead of a string-prefix test.
- **PF-L8** ntfy `ServerURL`: scheme allow-list (`http`/`https`), reject private
  IPs, and `url.QueryEscape` the topic (`notifications/manager.go:113-133`).
- **PF-L9** OpenLDAP: set `LDAP_TLS_ENFORCE=true` and stop the `wait-for-ldap`
  init container defaulting to `LDAPTLS_REQCERT: never`.
- **PF-L12** `OpenChunk` nonce check (`envelope.go:184-188`) compares the blob's
  own bytes against themselves; take the prefix from the signed
  `manifest.StreamPrefix` and enforce it. Land with the PF-H2 format change.

### Step 9 — PF-H5: docs + UI truthfulness (defer the refactor)

- Mark the app catalog as **not yet available** in `README.md:24`,
  `docs/app-catalog.md`, `docs/api.md`, `docs/spec.md` §3.4, and disable the
  install UI (`AppInstallModal.svelte`) or gate it behind a feature flag.
- Record the missing RBAC and the three non-repo URLs as inputs to the existing
  refactor plan; do not re-design here.
- Correct `docs/api.md:22-25` ("Authentication is **not** enforced inside the API
  router today") — it is false and misleading.

### Step 10 — Buddy Mediums beyond H2/H3

- **PF-M10** finite default quota at enrollment (`http.go:266-271`) and refuse
  writes in `reserveUsage` (`store.go:335-353`) below a free-space reserve.
- **PF-M11** make both publish guards fail **closed**: the shortfall check
  (`buddy_jobs.go:491`) and the AV-8 contentless check (`:501-505`) currently
  skip when the estimate/mount state is unavailable. Surface
  `ErrUnexpectedEOF` distinctly from `Push`.
- **PF-M12** raise/stream the manifest cap; make `client.do` (`client.go:71-74`)
  detect truncation; pre-flight projected manifest size in `Push`.
- **PF-M13** add the receiver audience (key fingerprint) to `CanonicalRequest`,
  bump to `BUDDY2`.
- **PF-M14** seal the identity file with an operator passphrase (Argon2id) or a
  Secret-sourced wrapping key; at minimum split the KEK from the signing key.

## 3. Validation

- `bash scripts/audit.sh` reaches the end and is green (Step 1 makes this
  meaningful again). This is the primary gate.
- `gitleaks dir .` and `gitleaks git .` clean (post-purge).
- `go build ./...`, `go vet ./...`, `go test -race ./...`, `staticcheck` on both
  modules; `govulncheck` clean under the bumped toolchain.
- `helm lint` + `helm template` for both values files; add assertions to
  `scripts/audit.sh` for the new NFS/SMB defaults (Step 5) and the removed
  Grafana dependency (Step 7).
- Full Playwright suite (35 specs) still passes; update specs touched by the UI
  changes (apps disabled, share-creation warning).
- For the buddy crypto changes: add targeted Go tests for the nonce-generation
  and restore-binding behaviour, including negative tests that fail against the
  old code.
- Live: redeploy to the VM, rotate the Cilium CA, confirm Cilium comes up with
  generated certs and the Hubble TLS listener still works.

## 4. Risks and failure modes

- **History rewrite** is destructive: unrelated clones diverge, and any CI/PR
  refs break. Mitigate with a safety tag, a clean-tree precondition, and the
  approval gate. The key is already exposed, so rotation matters more than the
  purge — do not let the purge block the rotation.
- **Buddy protocol bump** (PF-H2/H3 manifest + AAD + generation) is a
  compatibility boundary: in-flight chains created before the change won't
  verify. Decide whether to version the envelope (`EnvelopeVersion`) or require a
  fresh chain; check `docs/buddy-backup.md` and any deployed buddy peers.
- **NFS default change** (`*` → LAN CIDR, `Root_Squash`) is a behaviour change
  that can break existing mounts; surface it in release notes and the UI.
- **Go bump to 1.26.6** may pull changed stdlib behaviour; run the full suite.
- **PF-M8/M9 removals** reduce surface but may contradict `docs/spec.md`; update
  the spec in the same change so the docs and code agree.

## 5. Out of scope (explicit)

- The app-catalog refactor itself — see
  `.kilo/plans/1789943277180-charts-repo-and-app-install-refactor.md`.
- Live/container image-layer CVE scanning (`trivy` was unavailable to the audit;
  note it is installed now and could be added as a follow-up gate).
- The vendored Traefik/Authelia/Prometheus subchart internals.

## 6. Known-open Low findings (tracked, not fixed here)

PF-L1 (UI Dockerfile deletes the lockfile), PF-L2 (floating base tags), PF-L6
(`FreeDisks`/`IsRegular` — also CR-27), PF-L7 (dead `SMBManager`), PF-L10/L11/L13/
L14/L15/L17/L18 (buddy/helm robustness), PF-L16 (modal colour), and the UI
correctness/accessibility appendix in audit §5 (reactive params, SSR `onMount`,
group members discarded, `connectURL`, missing `encodeURIComponent`, no
`role="dialog"`, focus outline, `/settings` placeholder, catalog order, CSP).

## 7. Documentation reconciliation (audit §8.10)

Correct `docs/api.md` first (the auth claim), then reconcile the app
start/stop, snapshot, log-streaming and `/api/volumes` claims, and add the ~9
live routes the doc omits.
