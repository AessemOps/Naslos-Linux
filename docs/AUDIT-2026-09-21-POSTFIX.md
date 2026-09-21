# Naslos — post-fix audit (2026-09-21)

Independent re-audit of `master` at `b9c3321`, performed **after** the
remediation recorded in `docs/AUDIT-2026-09-19-REPORT.md`. Static analysis and
local tooling only — **no live target, no cluster, nothing deployed**. Every
finding below is cited to a file and line in this tree and was verified by hand.

- **Scope:** `api/`, `agent/`, `ui/`, `charts/`, `bootstrap/`, `scripts/`,
  `samba/`, `nfs/`, `openldap/`, `terminal/`, `Makefile`, `docs/`.
  `.kilo/worktrees/` is a git worktree duplicate and was excluded.
- **Method:** full read of the security-relevant source; `go build` / `go vet` /
  `go test` / `go test -race` / `staticcheck` / `gosec` / `govulncheck` on both
  Go modules; `npm audit` on the UI; `helm lint` + `helm template` on the chart;
  `gitleaks` over the working tree and all 185 commits; manual verification of
  each prior-audit claim.
- **Tooling note:** the Go toolchain, `helm`, `gosec`, `staticcheck`,
  `govulncheck` and `gitleaks` were **not present** on this machine and were
  installed for the audit. `trivy`, `semgrep`, `talosctl`, `kubectl` and
  `docker` were not available, so image-layer CVE scanning and any live check
  were out of scope.

## 1. Executive summary

The remediation work is real and the security *architecture* is now good: the
owner gate (`proxy secret` + trusted CIDR + `Remote-User` + `naslos_admins`) is
fail-closed and well tested, the agent has a constant-time bearer-token gate,
ZFS/share/LDAP input validation is allow-list based throughout, and the
two-namespace split plus the Cilium NetworkPolicy set are genuinely in the
chart.

However, this audit found **20 new findings that the previous audit did not
report**, including **one High-severity secret committed by the remediation
itself**, **two High-severity flaws in the Buddy Backup crypto/restore path**,
and **the app catalog being non-functional in three independent ways** while
still documented as working. It also found that **the repository's own audit
gate (`scripts/audit.sh`) aborts on line 230 under `/bin/sh`** — the exact
invocation its own header documents — which is the most likely reason several
of these regressions shipped.

| Severity | Count |
|---|---|
| Critical | 0 |
| **High** | **5** |
| **Medium** | **14** |
| **Low** | **18** |
| Info / quality | 9 |

### The five Highs

| ID | Finding | Where |
|---|---|---|
| **PF-H1** | A **valid Cilium CA RSA private key** and the Hubble server TLS key are committed in the clear | `bootstrap/cilium/cilium.yaml:47,66`; duplicated into `bootstrap/vm/naslos-vm.yaml:181,198` |
| **PF-H2** | **AES-GCM nonce reuse** on backup resume: the interrupted tail is re-encrypted under the same `(DEK, nonce)` with different plaintext | `api/internal/buddy/client.go:696-702` + `envelope.go:79-86` |
| **PF-H3** | `Restore` never binds the returned manifest to the requested source/chain — a malicious buddy can silently substitute an older or unrelated backup | `api/internal/buddy/client.go:853-871` |
| **PF-H4** | **NFS exports are unauthenticated, world-reachable and `No_Root_Squash`** by default | `api/internal/shares/config.go:157-187` |
| **PF-H5** | The **app catalog cannot install anything**, in three independent ways, while README/docs/spec present it as working | `api/internal/helm/repo.go`, `charts/naslos/templates/terminal.yaml:89-138`, `api/internal/server/apps.go:73` |

---

## 2. Verification results (reproducible)

| Gate | Result |
|---|---|
| `go build ./...` (api, agent) | **pass** |
| `go vet ./...` (api, agent) | **pass**, no diagnostics |
| `go test ./...` (api, agent) | **pass**, all packages green |
| `go test -race -count=1 ./...` (api, agent) | **pass**, no races |
| `staticcheck ./...` (api, agent) | **clean**, zero findings |
| `helm lint` + `helm template` (values + values-vm) | **pass** (only the "icon is recommended" INFO) |
| `npm audit` (prod) | **0 vulnerabilities**; 4 dev-only (vite/esbuild, GHSA-4w7w-66w2-5vf9, GHSA-67mh-4wv8-2f99) |
| `govulncheck` (api) | **10 affecting advisories** — 6 are **new** since the recorded baseline |
| `govulncheck` (agent) | **3 affecting advisories**, all new, all fixed upstream |
| `gosec` (api) | 95 issues (20 HIGH G703, 21 MEDIUM, 43 LOW/G104) |
| `gosec` (agent) | 17 issues (0 HIGH, 7 MEDIUM) |
| `gitleaks dir .` | **20 leaks**; `gitleaks git .` **11 leaks** — see PF-H1 |
| `sh scripts/audit.sh` | **aborts at line 230** (`Syntax error: "(" unexpected`) |
| `bash scripts/audit.sh` | completes, **2 checks FAIL** (govulncheck, gitleaks) |
| Go test coverage | api `auth` 90.0%, `logsafe` 100%, `buddy` 71.8%, `shares` 67.0%, `server` 42.2%, `notifications` 43.2%, `agent` 17.2%, `identity` 13.3%, `talos` 7.5%; `catalog`/`helm`/`metrics`/`config` **0%** |

---

## 3. High severity

### PF-H1 — A real Cilium CA private key is committed to the repository

**Where:** `bootstrap/cilium/cilium.yaml:38-66` (`cilium-ca` Secret, `ca.key`;
`hubble-server-certs`, `tls.key`), re-embedded verbatim into
`bootstrap/vm/naslos-vm.yaml:173-198` by `scripts/render-cilium.sh`.
Introduced by commit `dc62d071` — i.e. **by the AUDIT-M4 remediation**.

**Verified:**
```
$ openssl rsa -in ca.key -noout -check          → RSA key ok
$ openssl rsa  -in ca.key -noout -modulus | md5 → e563af3f…350d
$ openssl x509 -in ca.crt -noout -modulus | md5 → e563af3f…350d   (match)
subject=CN=Cilium CA   notBefore=Sep 20 13:20:42 2026   notAfter=Sep 19 13:20:42 2029
```
This is a live, matching 2048-bit RSA private key, not a placeholder. Both files
are mode `0644`. `gitleaks` flags it in the tree **and** in history.

**Impact.** Every Naslos install built from this repo receives the *same*
Cilium CA and the same Hubble server key. Anyone with repo read access can mint
certificates trusted by Cilium's mTLS/Hubble trust domain, impersonate the
Hubble server or relay, and decrypt/MITM Hubble observability traffic on any
deployment. It also invalidates two explicit prior-audit claims: *"No secret
values appear … in any committed artefact"* (§ preamble) and AUDIT-M2
(*"`bootstrap/vm/*` chmod 600"*).

**Fix.**
1. Treat both keys as compromised: rotate the Cilium CA on any deployed cluster.
2. Purge the material from history (`git filter-repo`), then force-push.
3. Stop checking in a rendered manifest containing Secrets. Either render Cilium
   at install time (`helm template cilium/cilium …` in `make`), or strip the
   `cilium-ca` / `hubble-server-certs` Secrets from the committed manifest and
   let Cilium's `certgen` job create them in-cluster
   (`tls.auto.enabled=true, tls.auto.method=cronJob|helm` at install time).
4. Add a `gitleaks` pre-commit hook — the check already exists in `audit.sh`
   and would have caught this had the script not aborted first (PF-M1).

---

### PF-H2 — AES-GCM nonce reuse when a Buddy Backup resumes an interrupted chunk

**Where:** `api/internal/buddy/client.go:686-702`, with
`api/internal/buddy/envelope.go:79-86` and `:139-163`.

```go
// client.go:675 — dek and prefix come from the persisted ChainState
sealed, sha, err := SealChunk(dek, prefix, opts.Source, state.Chain, index, plain)
...
// client.go:696-702
if storedDigest != digestOf(sealed) {
    if published || index != tailIndex {
        return result, fmt.Errorf("the receiver already holds different bytes …")
    }
    // Fall through and upload: this is the interrupted tail.
}
```
```go
// envelope.go:79-86 — the nonce is a pure function of (prefix, index)
func chunkNonce(prefix []byte, index int) []byte {
    var nonce [12]byte
    copy(nonce[:8], prefix)
    binary.BigEndian.PutUint32(nonce[8:], uint32(index))
    return nonce[:]
}
```

`dek` and `prefix` are loaded from the same persisted `ChainState`
(`client.go:625-632`), so a resumed upload of chunk *tailIndex* encrypts a
**different plaintext** under an **identical `(key, nonce)`** pair.

**Reachability.** `Push` treats `io.ErrUnexpectedEOF` as a clean stream end
(`client.go:666-672`), so a `zfs send` that dies mid-chunk stores a short tail;
`BeforePublish` then vetoes publication (`buddy_jobs.go:490-495`), leaving an
unpublished chain with a partial tail; the retry resumes the same chain and
re-seals that index with the full 1 MiB. `buddy_send_test.go:501` constructs
exactly this precondition.

**Impact.** The receiver — the explicitly declared adversary — holds both
ciphertexts at the same nonce. `C1 ⊕ C2` leaks `P1 ⊕ P2` (a direct plaintext
disclosure of up to 1 MiB whenever the source is not byte-deterministic:
`buddyctl push --dir` re-tars a changed tree, `--file`, any `--kind raw`).
Independently, two valid tag/ciphertext pairs under one nonce allow solving for
the GHASH subkey `H`, giving universal forgery at that nonce.
`docs/buddy-backup.md:175-176` and `client.go:588-595` both assert the opposite
property.

**Fix.** Extend the nonce counter domain and bind it: nonce =
`prefix(6) || uint16BE(generation) || uint32BE(index)`, with `generation`
incremented in `ChainState` on every resume, carried in the signed manifest and
in the AAD. The cheap alternative is to make `client.go:697` always error and
require a fresh chain, at the cost of re-uploading one unpublished chain. Also
tighten `Store.mayReplace` (`store.go:369-386`) to refuse same-index
replacement, and pass `manifest.StreamPrefix` into `OpenChunk` (see PF-L12).

---

### PF-H3 — `Restore` accepts any validly signed manifest the receiver returns

**Where:** `api/internal/buddy/client.go:853-871`.

```go
manifest, err := c.Manifest(opts.Source, opts.Chain)
...
if !opts.SkipSignatureCheck {
    if err := manifest.VerifySignature(c.Identity.PublicKey); err != nil { … }
}
dek, err := UnwrapDEK(kek, manifest.DEKWrapped, manifest.Source, manifest.Chain)
result := &RestoreResult{Source: manifest.Source, Chain: manifest.Chain, …}
```

There is **no** `manifest.Source != opts.Source` or
`manifest.Chain != opts.Chain` check. Every downstream operation
(`fetchChunk`, `OpenChunk`, the digest comparison at `client.go:894`) uses
`manifest.Source`/`manifest.Chain`, so a substituted manifest is internally
self-consistent and passes every check.

**Impact.** A compromised or malicious buddy answers a restore request with last
month's (validly signed) manifest, or with a *different source's* manifest.
`POST /api/buddy/restore` streams it into `zfs receive -F` for the first chain
(`buddy_send.go:506-508`) and reports `{"status":"restored"}`. Signature,
contiguity and per-chunk digests all verify; the operator gets no signal, at the
one moment when the original data is gone. `verify: true`
(`buddy_send.go:465-498`) is fooled identically.

**Fix.** Immediately after `c.Manifest(...)`: reject unless
`manifest.Source == opts.Source`, and unless
`opts.Chain == "" || manifest.Chain == opts.Chain`. For freshness, sign a
monotonic sequence number into the manifest and persist the last published value
per `(receiver, source)` on the sender, refusing anything older.

---

### PF-H4 — NFS shares are exported unauthenticated, to every host, with `No_Root_Squash`

**Where:** `api/internal/shares/config.go:139-187` (`ganeshaExport`).

```go
sb.WriteString("    Squash = No_Root_Squash;\n")
sb.WriteString("    SecType = sys;\n")          // AUTH_SYS: client-asserted uid/gid
...
} else {                                         // no allowedHosts → the default
    sb.WriteString("    CLIENT {\n")
    sb.WriteString("        Clients = *;\n")
    sb.WriteString(fmt.Sprintf("        Access_Type = %s;\n", access))
```

`allowedHosts` defaults to empty (`manager.go:127-160`), NFSv4 with
`SecType = sys` performs no authentication whatsoever (the client simply asserts
its uid), and `No_Root_Squash` maps a client's root to the server's root.

**Impact.** On a default-configured Naslos, **any host that can reach the node's
:2049 can mount every NFS share and read, modify or delete every file as
uid 0** — no credential, no Kerberos, no host restriction. The
`naslos-nfs-ingress` NetworkPolicy does not help: the NFS DaemonSet is
`hostNetwork: true` (`charts/naslos/templates/nfs-daemonset.yaml`), and
Kubernetes pod-level NetworkPolicy does not govern host-network pods — the same
residual the prior audit recorded for the agent's `:9090`.

This posture *is* documented (`docs/shares.md:447-470`), which makes it a
deliberate choice rather than an oversight — but it is documented in a reference
appendix, not surfaced where a user creates a share, and it is the single
largest data-exposure surface in the product.

**Related, same area (Medium):** `GenerateSambaConfig` sets
`force user = root` / `force group = root` on **every** SMB share
(`config.go:83-85`). Every authenticated SMB session therefore performs all file
operations as root, which nullifies the POSIX/LDAP/extrausers uid mapping the
project builds elsewhere: one compromised SMB account has root-equivalent access
to the entire share tree, and all created files are root-owned.
`map to guest = Bad User` (`config.go:25`) additionally maps unknown users to
guest rather than rejecting them.

**Fix.**
1. Default `allowedHosts` to the node's LAN CIDR (the chart already knows it as
   `networkPolicy.nfsClientCIDR`) and require the operator to opt into `*`.
2. Default to `Squash = Root_Squash` and expose `No_Root_Squash` as an explicit
   per-share toggle with a warning.
3. Replace `force user = root` with the mapped LDAP uid (the extrausers/NSS
   plumbing already exists for exactly this) and set `create mask = 0660`,
   `directory mask = 0770`.
4. Set `map to guest = Never`, `server min protocol = SMB3`, and
   `smb encrypt = desired` in the `[global]` block.
5. Surface the effective access posture in the UI when a share is created.

---

### PF-H5 — The app catalog cannot install anything (three independent breakages)

Advertised in `README.md:24` ("deploy and configure apps via Helm with
schema-driven forms"), `docs/app-catalog.md:48-57` and normatively in
`docs/spec.md` §3.4 FR-APP-01…04.

1. **No Helm repository is ever added.** `apps.go:73` calls
   `s.helm.Install(ctx, req.Name, catalogApp.Chart, values)` with a bare ref such
   as `jellyfin/jellyfin`. `helm.AddRepo`/`UpdateRepos` (`api/internal/helm/repo.go`)
   have **no callers**, and `UpdateRepos` is a literal no-op loop
   (`repo.go:26-37`: `_ = re // In production: use repo.NewChartRepository`).
   `LocateChart` therefore cannot resolve the reference.
2. **The API ServiceAccount has no Helm RBAC.** The only bindings for SA
   `naslos-api` in the entire repository are
   `charts/naslos/templates/terminal.yaml:89-138`: ClusterRole
   `naslos-api-namespaces` (`namespaces` get/list) and Role `naslos-api-terminal`
   in `naslos-privileged` (`pods`, `pods/log`, `pods/exec`). The Helm SDK uses the
   `secret` storage driver (`helm/client.go:43`), so even `helm list` needs
   `list secrets` — which is not granted. `install.CreateNamespace = true`
   (`operations.go:23`) additionally needs cluster-scoped namespace create.
   Verified against the full rendered chart: no other Role/ClusterRole exists.
3. **Three of the eight catalog entries point at URLs that are not Helm
   repositories at all** — `https://github.com/MoJo2600/pihole-kubernetes`
   (`builtin_extra.go:53`), `https://syncthing.net` (`:115`),
   `https://immich.app` (`:142`) — so they would still fail after (1) and (2).

**Impact.** Install, upgrade, uninstall and *list* all fail at runtime with
`Forbidden` / `chart not found`. The UI shows a catalog and an install modal
that cannot work. Also: `helm.List` has no release filter
(`operations.go:94-125`), so once RBAC is granted the `naslos` umbrella release
itself will appear in **Installed Apps** with a live **Uninstall** button.

The project's own `.kilo/plans/1789943277180-charts-repo-and-app-install-refactor.md:39`
independently reaches the same conclusion — but `README.md`, `docs/api.md`,
`docs/app-catalog.md` and `docs/spec.md` still present the feature as working.

**Fix.** Ship the planned refactor, or at minimum: add a namespaced Role for
`naslos-api` in a dedicated `naslos-apps` namespace, wire `AddRepo`/`UpdateRepos`
into the install path, correct the three bad repository URLs, filter the
umbrella release out of `helm.List`, and mark the feature as unavailable in the
docs and the UI until it is.

---

## 4. Medium severity

### PF-M1 — `scripts/audit.sh` aborts under `/bin/sh`, so most of the security gate never runs

`scripts/audit.sh:1` is `#!/bin/sh`, its own header documents
`Usage:  sh scripts/audit.sh`, but line 230 uses bash process substitution:

```sh
new=$(comm -23 <(printf '%s\n' $found | sort -u) <(printf '%s\n' $accepted | tr ' ' '\n' | sort -u))
```

```
$ sh scripts/audit.sh
scripts/audit.sh: 230: Syntax error: "(" unexpected      # /bin/sh → dash
$ bash -n scripts/audit.sh                               # syntactically fine under bash
```

Everything after line 230 therefore never executes on any Debian/Ubuntu host:
`gosec`, the agent `go vet`/`go test -race`, `npm audit`, `svelte-check`,
`helm lint`, the **AUDIT-M3 Authelia-LDAP-Secret regression guard**, the
**AUDIT-M4 NetworkPolicy guard**, the **Cilium/Talos-patch guard**, and
**`gitleaks`**. This is almost certainly why PF-H1 shipped: run under `bash`,
the gitleaks check fails and flags the committed CA key.

Run under `bash`, the sweep reports **2 failures**:
```
NEW advisory (not in the AUDIT-H3 accepted set): GO-2026-5026 GO-2026-5972
  GO-2026-6089 GO-2026-6090 GO-2026-6091 GO-2026-6218
FAILED: govulncheck (api; fails on a NEW advisory)
...
leaks found: 11
FAILED: gitleaks (tree + history)
```

Two further defects in the same script: `gitleaks detect --source=.`
(line 297) scans **history only** despite the label "(tree + history)"; and the
`accepted` advisory allow-list (line 229) is hardcoded, so it will keep failing
until manually curated.

**Fix.** Change the shebang to `#!/usr/bin/env bash` and the documented usage to
`bash scripts/audit.sh` (or rewrite line 230 POSIX-ly with temp files). Replace
`gitleaks detect --source=.` with both `gitleaks dir .` and `gitleaks git .`.
Re-add the CI workflow, or at minimum a pre-commit hook for the gitleaks step.

### PF-M2 — Go toolchain floor is pinned to a version with 6 fixable stdlib CVEs

`api/go.mod:3` and `agent/go.mod:3` declare `go 1.26.5`, with no `toolchain`
directive. `govulncheck` against that toolchain reports six *reachable* stdlib
advisories, **all fixed in go1.26.6**:

| ID | Package | Reached from |
|---|---|---|
| GO-2026-6218 | `net/url` quadratic `resolvePath` | api |
| GO-2026-6091 | `html/template` JS regexp context | api |
| GO-2026-6090 | `crypto/tls` post-handshake message flood | api + **agent** (`agent/internal/server/server.go:130`) |
| GO-2026-6089 | `net/http` `ReadHeaderTimeout` on h2c check | api + **agent** |
| GO-2026-5972 | `encoding/asn1` recursion depth | api + **agent** |
| GO-2026-5026 | `net/http` / `x/net/idna` punycode | api |

The four remaining advisories (GO-2026-5932 `x/crypto/openpgp`, and three
containerd CRI issues) are `Fixed in: N/A` and match the accepted AUDIT-H3 set.

**Fix.** Bump the `go` directive to `1.26.6` (or add `toolchain go1.26.6`) in
both modules so every build gets a patched stdlib, and update the `accepted`
list in `audit.sh`.

### PF-M3 — AUDIT-H4 is not fully closed: a default Grafana admin password is still committed

`charts/naslos/values-vm.yaml:120-122`:
```yaml
# Grafana admin password — change in production.
grafana:
  adminPassword: naslos-admin
```
The prior audit records AUDIT-H4 as *"Fixed by removal"* and `README.md:32` says
*"Grafana was removed"*. In fact `charts/naslos/Chart.yaml:38-41` still declares
the dependency, `Chart.lock` still pins it, `charts/naslos/charts/grafana-7.0.0.tgz`
is still vendored, and only `values.yaml:335 enabled: false` keeps it off. A
single `--set grafana.enabled=true` deploys Grafana with `admin` / `naslos-admin`.

**Fix.** Remove the dependency from `Chart.yaml`, regenerate `Chart.lock`, delete
the vendored tarball and the `grafana:` blocks from both values files.

### PF-M4 — The proxy shared secret is stored in plaintext in a Traefik CRD

`charts/naslos/templates/proxy-secret.yaml:57-59`:
```yaml
spec:
  headers:
    customRequestHeaders:
        X-Naslos-Proxy-Secret: {{ required "…" $proxySecret | quote }}
```
The value that, combined with `Remote-User`, grants full admin on the API is
written verbatim into a `Middleware` CRD — not a Secret. It is readable by
anything with `get middlewares.traefik.io` in the namespace, by `helm get
manifest`, and by any CRD backup or GitOps mirror. Kubernetes RBAC for Secrets is
typically far tighter than for arbitrary CRDs, so the in-code comment
("the same trust domain as the API's own configuration") understates it.

This compounds with `TRAEFIK_CIDR: 10.0.0.0/8` (rendered) — the trusted-source
check effectively means "any pod" — and with the API's NetworkPolicy admitting
the **entire** `naslos-privileged` namespace (rendered `naslos-api-ingress`,
`namespaceSelector` with no `podSelector`), which is where the privileged web
terminal runs.

**Fix.** Read the value with Traefik's `customRequestHeaders`-from-Secret support
where available, or have the API accept a *signed* short-lived proxy assertion
instead of a static shared secret. Narrow `TRAEFIK_CIDR` to Traefik's actual pod
CIDR, and narrow the API's NetworkPolicy to the specific pods in
`naslos-privileged` that need it rather than the whole namespace.

### PF-M5 — The privileged agent serves plaintext HTTP on the node's :9090

`agent/cmd/main.go:27` (`-listen ":9090"`) and
`agent/internal/server/server.go:125-131` (`http.Server` + `ListenAndServe`, no
TLS). The DaemonSet is `hostNetwork: true` and `privileged: true`, so this binds
every node interface.

Consequences: the bearer token that authorises pool destruction and `zfs receive
-F` crosses the LAN in cleartext on every API→agent call; and every byte of
`zfs send`/`receive` traffic (backup and restore payloads) is unencrypted unless
the dataset itself uses ZFS native encryption. Because the pod is host-network,
`naslos-agent-ingress` (rendered) does not constrain it — the token is the only
control, and it is exposed on the wire.

**Fix.** Serve TLS on :9090 with a chart-issued cert and pin it in
`api/internal/agent/client.go`; or bind the listener to the node's internal
address only; or move the agent off `hostNetwork` if the hostPath mounts allow it.

### PF-M6 — Agent HTTP server has no header/read/write timeouts (gosec G112)

`agent/internal/server/server.go:126-129` sets only `Addr` and `Handler`,
while the API correctly sets `ReadHeaderTimeout: 10s`, `ReadTimeout: 30s`,
`IdleTimeout: 120s` (`api/internal/server/server.go:367-369`). A Slowloris or
slow-body client can pin connections on the most privileged component in the
system. There is also no `http.MaxBytesReader` on any agent handler.

**Fix.** Mirror the API's settings, with a long or disabled `ReadTimeout` only on
`/api/v1/zfs/receive/`.

### PF-M7 — Install-form values permanently poison the shared in-memory catalog

`api/internal/server/apps.go:67-69`:
```go
values := catalogApp.DefaultValues   // map reference, not a copy
for k, v := range req.Values {
    values[k] = v                    // writes into the catalog's own map
}
```
`Catalog.Get` returns the stored `*App` (`catalog/catalog.go:107-113`), and
`DefaultValues` is a `map[string]interface{}`, so the assignment copies only the
map header. The mutation happens **before** the Helm call, so it persists even
when the install fails.

`SchemaForm.svelte:7-12` renders `format: "password"` fields as password inputs.
Whatever an admin types (a Nextcloud admin password, a Plex claim token) is
written into the catalog default and then served by `GET /api/catalog/{name}`
(`apps.go:20-34`) and pre-filled into the next admin's form
(`AppInstallModal.svelte:33-34`). Cross-admin credential disclosure, for the
lifetime of the API process.

**Fix.** `values := maps.Clone(catalogApp.DefaultValues)` (handling nil), and have
`Catalog.Get` return a deep copy.

### PF-M8 — `/api/volumes` is a stub that reports success without doing anything

`api/internal/server/disks.go:158-181`:
```go
case http.MethodGet:
    writeJSON(w, http.StatusOK, map[string]string{"status": "listing volumes"})
case http.MethodPost:
    ...
    _, err := talos.UserVolumeConfig(req.Name, req.FSType, req.MinSize)   // result discarded
    ...
    writeJSON(w, http.StatusCreated, map[string]string{"status": "volume created"})
```
GET returns a hardcoded string; POST builds a `UserVolumeConfig` document, throws
it away, and answers `201 {"status":"volume created"}`. `docs/spec.md:528` lists
it as "Volumes overview" with no caveat and `docs/storage-zfs.md:5-6` says
`UserVolumeConfig` "is exposed separately via `/api/volumes`".

**Fix.** Either implement it (apply the document via the Talos client) or remove
the route and the documentation.

### PF-M9 — Six of the eight advertised notification events are never sent

`api/internal/notifications/notifications.go:14-19` defines `zfs_health`,
`zfs_scrub`, `app_status`, `disk_failure`, `system_update`, `share_access`, and
`ui/src/routes/notifications/+page.svelte:33-41` offers all of them as
checkboxes. The **only** `Manager.Send` call sites in the entire codebase are
`api/internal/server/buddy_jobs.go:590` and `:601` (backup success/failure).

This breaks `docs/notifications.md:24-29`, `README.md:31` and FR-NTF-01
(`docs/spec.md:399-400`). A Naslos instance never alerts on a degraded pool or a
failed disk — for a NAS, that is the headline monitoring feature.

**Fix.** Wire `Send` into the ZFS health poll, the disk-discovery path and the
app-status path, or remove the unimplemented event types from the settings UI
and the docs.

### PF-M10 — Buddy Backup: enrolled peers get unlimited quota, and free space is never checked

`api/internal/buddy/http.go:266-271` creates the peer without setting
`QuotaBytes`, and `peers.go:278-292` documents `0` as "unlimited".
`Store.FreeSpace()` (`store.go:934-950`) is reported in `/status` but consulted on
**no** write path. `docs/buddy-backup.md:423-425` claims the opposite ("A sender
cannot write … beyond its quota").

One enrollment token therefore permits unbounded writes until the receiving ZFS
pool is full — which on a NAS typically also hosts the shares and the API's own
state volume.

**Fix.** Apply a finite default quota at enrollment (configurable), and refuse
writes in `reserveUsage` (`store.go:335-353`) when free space would drop below a
reserve.

### PF-M11 — Buddy Backup: a truncated `zfs send` can be published as a complete backup

`api/internal/buddy/client.go:666-672` treats `io.ErrUnexpectedEOF` — exactly
what a severed `zfs send` HTTP body produces — as a clean EOF. Both compensating
guards are conditional and fail *open*:

- `buddy_jobs.go:491`: `if expected > 0 && …` — `expected` is 0 whenever
  `EstimateSend` fails, and that failure is only logged (`:464-467`).
- `buddy_jobs.go:501-505`: `if d := s.datasetMountState(dataset); d != nil` —
  `datasetMountState` returns nil on any `ListDatasets` error
  (`buddy_send.go:303-307`), silently skipping the AV-8 contentless-backup check.

A transient agent hiccup during estimation is enough for a 5%-complete send to be
signed, published and become `current.json`; the next incremental chains off it.
The corruption surfaces only at restore.

**Fix.** Make both guards fail closed (refuse to publish when the estimate or the
mount state is unavailable, unless explicitly overridden), and surface
`ErrUnexpectedEOF` distinctly from `Push`.

### PF-M12 — Buddy Backup: 8 MiB manifest cap silently caps a chain at ~58 GiB

`api/internal/buddy/http.go:23-25` (`maxManifestBytes = 8 << 20`, commented
"small even for multi-terabyte chains") and `client.go:71`. One compact
`ManifestChunk` is ≈140 bytes, so 8 MiB ≈ 59,900 chunks ≈ **58 GiB** per chain.
`store.go:88-92` claims a 4 PiB bound; `docs/buddy-backup.md:352` promises
multi-terabyte; `ui/nginx.conf:87` caps the body at 8m too.

Past that limit the sender uploads **every chunk** (consuming the receiver's
disk) and then fails on the final manifest `PUT` with 413 — on every retry,
forever. Two related defects: `client.do` *truncates* oversize responses instead
of erroring (`client.go:71-74`), and the resume chunk-list endpoint is silently
truncated above ~160k chunks, after which a resume re-uploads the whole chain.

**Fix.** Raise the limit and stream/paginate the manifest and the chunk list;
make `client.do` detect truncation; pre-flight the projected manifest size in
`Push` before uploading anything.

### PF-M13 — Buddy Backup: request signatures are not bound to the receiver

`api/internal/buddy/auth.go:40-49` signs
`"BUDDY1\n" + METHOD + "\n" + path + "\n" + bodyDigest + "\n" + ts + "\n" + nonce`
— no Host, no receiver identity. The replay cache is per-process
(`auth.go:91`), so a nonce burned on buddy A is fresh on buddy B.

With fan-out schedules (`buddy_schedules.go:44`, `docs/buddy-backup.md:513-519`)
pushing the same source to several receivers with one identity, buddy A can
capture a signed `POST /prune/<source>` and replay it verbatim to buddy B inside
the 5-minute skew window, destroying B's retained chains.

**Fix.** Add an audience line (the receiver's own key fingerprint, published in
`/status`) to `CanonicalRequest`, and bump the tag to `BUDDY2`.

### PF-M14 — Buddy Backup: the private key and the data KEK sit unencrypted in one file

`api/internal/buddy/keys.go:54-82` writes an **unencrypted** OpenSSH Ed25519
private key and the raw base64 KEK into a single JSON document. There is no KDF
anywhere in the subsystem — no passphrase, no Argon2/scrypt/PBKDF2, no envelope
encryption, and no separation between the network-authentication key and the
data-encryption key. The resume state duplicates each chain's DEK in plaintext
(`client.go:410-418`, `buddy_send.go:183-192`).

File permissions themselves are correct (0600 files in 0700 dirs, atomic writes).
The issue is that any arbitrary-file-read in the API, any snapshot of the state
volume, or any admin terminal session yields both the ability to impersonate the
sender to every buddy **and** the ability to decrypt every backup on every buddy.
`docs/buddy-backup.md:64-69` warns about *losing* the file but never says it is
unencrypted.

**Fix.** Seal the identity with an operator-supplied passphrase (Argon2id) or a
Secret-sourced wrapping key; at minimum split the KEK from the signing key, and
document the at-rest exposure.

---

## 5. Low severity

| ID | Finding | Where |
|---|---|---|
| PF-L1 | UI image **deletes `package-lock.json`** then runs `npm install`, so the shipped bundle is not built from pinned versions and `npm audit` on the repo lockfile does not describe it. The comment claims "the lock still pins the versions" — it does not. | `ui/Dockerfile:14-15` |
| PF-L2 | Base images use floating tags (`golang:1.26-alpine`, `alpine:3.24`, `node:20-alpine`, `debian:trixie-slim`); none is digest-pinned. | all Dockerfiles |
| PF-L3 | The `api` container has **no** `securityContext`: no `runAsNonRoot`, `allowPrivilegeEscalation: false`, `capabilities: drop: [ALL]`, `readOnlyRootFilesystem` or `seccompProfile`. Both namespaces enforce PSA `privileged`, so nothing else enforces it either. Same for agent/samba/nfs/terminal. | `charts/naslos/templates/api-deployment.yaml:104-107`; rendered chart |
| PF-L4 | `naslos-terminal` (privileged, `hostPath: /`) does not set `automountServiceAccountToken: false`. | `charts/naslos/templates/terminal.yaml:44-46` |
| PF-L5 | `CleanFolderPath` confines by string prefix and never resolves symlinks, so a symlink created inside a share (by an SMB/NFS user) lets `ListFolders`/`CreateFolder`/`DeleteFolder` operate outside `/var/mnt`. | `agent/internal/shares/folders.go:115-131` |
| PF-L6 | `FreeDisks` filters entries with `info.Mode().IsRegular()`, which is false for block devices, so it always returns empty on a real node. Recorded as CR-27 in the archived review and still unfixed; the endpoint has no caller. | `agent/internal/zfs/devices.go:137-141` |
| PF-L7 | `SMBManager` (135 lines) shells out to `kubectl`, which does not exist in the distroless API image — and the type is **never instantiated anywhere**. Dead code that reads like a live privileged path (gosec G204). | `api/internal/identity/samba.go:19-130` |
| PF-L8 | ntfy `ServerURL` is admin-settable with no scheme allow-list or private-IP check, and `Topic` is interpolated into the URL unescaped. The configured bearer token is sent to whatever host is set. | `api/internal/notifications/manager.go:113-133` |
| PF-L9 | OpenLDAP serves plaintext `ldap:///` on 389 with `LDAP_TLS_ENFORCE=false`; only the NetworkPolicy keeps callers on 636. The API's `wait-for-ldap` init container sets `LDAPTLS_REQCERT: never` when no CA secret is configured. | `openldap/image/Dockerfile:16,68`; `charts/naslos/templates/api-deployment.yaml:41-42` |
| PF-L10 | Buddy: nonce-cache eviction (`maxNoncesPerKey`) drops live entries above ~6.8 req/s, permitting replay inside the 5-minute window; `docs/buddy-backup.md:420` states flatly that replay is impossible. | `api/internal/buddy/auth.go:311-321` |
| PF-L11 | Buddy: `ChunkDigests` re-reads and SHA-256s the **entire** stored chain on every resume probe (a 4 KB GET costs O(chain size) disk+CPU), and the response is unbounded. | `api/internal/buddy/store.go:454-468` |
| PF-L12 | Buddy: `OpenChunk`'s nonce check takes the prefix from the blob it is checking, so bytes 8..16 compare against themselves; the signed `manifest.StreamPrefix` is never enforced at restore. | `api/internal/buddy/envelope.go:184-188`; `client.go:890` |
| PF-L13 | Buddy: enrollment lets the peer declare its own `Name` and `AllowedSources`; empty means "any source" and there is no server-side way to constrain it. `handleBuddyPeers` also never runs `ValidateSource` on operator-supplied scopes. | `api/internal/buddy/http.go:250-271`; `server/buddy.go:96-102` |
| PF-L14 | Buddy: `TouchSeen` rewrites the whole peer registry on **every** authenticated request, and the nonce file is fully rewritten on every request above 1024 live entries — gigabytes of parasitic I/O per backup. | `api/internal/buddy/peers.go:255-263`; `auth.go:184-201` |
| PF-L15 | Buddy: the instance receiver's `ReadTimeout: 30s` bounds the whole request body, so a 1 MiB chunk needs ≈280 kbit/s sustained. The standalone receiver deliberately uses 15 minutes. | `api/internal/server/server.go:368` vs `api/cmd/buddy-receiver/main.go:82-85` |
| PF-L17 | `helm.NewClient("naslos")` **hardcodes** the release namespace while every other consumer derives it from `NASLOS_NAMESPACE`. A release deployed to any other namespace would install apps into a namespace it does not own. | `api/internal/server/server.go:79` vs `:137` |
| PF-L18 | `catalog.New("")` is called with an empty path, so the documented "drop a `*.json` into `catalogPath`" extension mechanism is dead in production. | `api/internal/server/server.go:82`; `docs/app-catalog.md` |
| PF-L16 | UI: `bg-naslos-card` is not a defined Tailwind colour, so the **Delete Pool** confirmation and the **Import Pool** dialog render with no background over a `bg-black/60` scrim. Every other modal uses `bg-naslos-surface`. | `ui/src/lib/components/Dashboard.svelte:195`; `ui/src/routes/pools/+page.svelte:150`; `ui/tailwind.config.js:6-15` |

**Additional Low UI/correctness items** (full detail retained in the working
notes): pool detail captures `$page.params.name` non-reactively so `zpool add`
can target a stale pool after client-side navigation
(`pools/[name]/+page.svelte:62`); `Dashboard.svelte:118-120` starts `fetch` and
`setInterval` in the instance body rather than `onMount`, so SSR throws and the
first paint shows an error; creating a group silently discards the selected
members (`GroupForm.svelte:57-59`); `connectURL` ignores `share.enabled`, making
the "Disabled — not reachable" branch unreachable and advertising a dead address
(`shares/+page.svelte:47-59`), and it emits an `nfs://` URL that no client
accepts; nine destructive API paths interpolate names without
`encodeURIComponent`; no modal sets `role="dialog"`, traps focus or handles
Escape; `.input` removes the focus outline for a ~1.9:1 border change (WCAG
1.4.11); `/settings` is a "Coming soon" placeholder linked from the primary nav;
the **Configure** button on every installed app is hard-disabled; the users page
swallows load failures into `console.error` and renders "No Users" during an LDAP
outage; the catalog grid order is non-deterministic (map iteration); `btn-sm` is
undefined; and there is no Content-Security-Policy.

---

## 6. Verdict on the previous audit's claims

| Prior claim | Verdict | Evidence |
|---|---|---|
| "No secret values … in any committed artefact" | **DOES NOT HOLD** | PF-H1: a valid Cilium CA private key in `bootstrap/cilium/cilium.yaml:47` |
| AUDIT-H1 — LDAP bind password from a Secret | **HOLDS** | `AUTHELIA_AUTHENTICATION_BACKEND_LDAP_PASSWORD_FILE` + matching mount render correctly; no inline `password:` in `authelia-config` |
| AUDIT-H2 — buddy receive path | **HOLDS** | `buddy.receiveHostPath` set, dedicated hostPath volume |
| AUDIT-H3 — Go vulnerabilities | **PARTIAL** | The 4 accepted `Fixed in: N/A` advisories remain, but 6 **new, fixable** stdlib advisories appeared (PF-M2) and the gate that would catch them is broken (PF-M1) |
| AUDIT-H4 — Grafana default password removed | **PARTIAL** | `grafana.adminPassword: naslos-admin` still committed; dependency, lockfile and tarball still present (PF-M3) |
| AUDIT-M1 — secret-bearing local refs purged | **HOLDS** | `git for-each-ref` shows 0 non-branch/tag/remote refs; the one historical `openldap/manifests/secrets.yaml` hit is a `CHANGE_ME` placeholder |
| AUDIT-M2 — local secret files 0600 | **DOES NOT HOLD** | `bootstrap/vm/naslos-vm.yaml` is `0644` and now contains a private key |
| AUDIT-M3 — jwt_secret persisted / LDAP pw in a Secret | **HOLDS** (Low residual as recorded) | `naslos-authelia-jwt` Secret renders; `jwt_secret` is still in the ConfigMap, as documented |
| AUDIT-M4 — Cilium + enforced NetworkPolicy | **HOLDS, with the recorded residual** | default-deny in both namespaces, 12 NetworkPolicies + 2 CiliumNetworkPolicies render; hostNetwork pods (agent :9090, samba, **nfs :2049**) remain uncovered — and PF-H4 shows why that residual is larger than "Low" |
| AUDIT-M5 — unused agent ClusterRole removed | **HOLDS** | The only Naslos RBAC in the repo is `terminal.yaml:89-138`; no agent ClusterRole exists |
| AUDIT-M6 — namespace split | **HOLDS, partially** | `naslos-privileged` exists and carries agent/samba/nfs/terminal; **both** namespaces still enforce PSA `privileged`, and the API's NetworkPolicy admits the whole privileged namespace (PF-M4) |
| AUDIT-M11 — Svelte 5 / xterm migration | **PARTIAL** | Dependencies are on Svelte 5 and `@xterm/*`, but 100% of components are Svelte-4 legacy syntax in compatibility mode; `AI_Handoff.md:18` overstates it |
| AUDIT-M12 — no CI; run the sweep by hand | **WORSE THAN RECORDED** | The manual sweep aborts under its own documented invocation (PF-M1) |
| Batch 6 — "buddy crypto deep-dive, no findings" | **DOES NOT HOLD** | PF-H2 (GCM nonce reuse) and PF-H3 (unbound restore manifest) are both in the reviewed code |
| CR-27 — `FreeDisks` / `IsRegular` | **STILL OPEN** | `agent/internal/zfs/devices.go:137-141` (PF-L6) |

---

## 7. Verified clean

These were examined specifically and found correct.

**Authentication and authorisation**
- The owner gate is fail-closed by construction: an empty `PROXY_SHARED_SECRET`
  rejects everything, the trusted-CIDR check and the constant-time secret
  comparison both run before any identity header is read, and a non-empty
  `Remote-User` is required (`api/internal/auth/middleware.go:74-113`). `New()`
  `log.Fatalf`s without the secret (`server.go:122-125`).
- Route composition is safe by default: only `/api/auth/me`, `/api/dashboard`
  and `/api/metrics` are non-admin; every other `/api/` route is mounted behind
  `requireOwnerAuth(requireAdmin(...))` (`server.go:294-302`), so a new handler
  added to the `owner` mux is admin-only unless someone deliberately moves it.
  `routes_test.go:50` pins this.
- Traefik's forwardAuth lists `Remote-User`/`Remote-Groups`/`Remote-Email`/
  `Remote-Name` in `authResponseHeaders` (so client-supplied values are
  replaced) and leaves `trustForwardHeader` false
  (`charts/naslos/templates/traefik-middleware.yaml:18-28`).
- The agent's bearer gate is constant-time, fails closed on an empty token, and
  only `/health` sits outside it (`agent/internal/server/server.go:56-96`).
- Authelia's `access_control` uses `default_policy: deny` with narrowly scoped
  bypasses; the `^/api/buddy/v1/` bypass is correct and intentional, and Go's
  `ServeMux` path-cleans-then-**redirects** rather than dispatching, so
  `/api/buddy/v1/../users` cannot reach an admin route unauthenticated.

**Injection**
- No shell is ever invoked. Every host command uses an argv slice through
  `exec.CommandContext(ctx, "chroot", hostRoot, name)` (`agent/internal/zfs/pool.go:132-139`,
  `backup.go:216-222`).
- Allow-list validation throughout: `poolNamePattern`, `datasetNamePattern`,
  `sizePattern`, `snapshotNamePattern`, the `datasetOptionKeys` property
  allow-list, `NormalizeVDevTopology`, `normalizeDiskPath` (absolute `/dev/`
  + stat through `/host` + no `..`), `normalizeDiskSet` (no duplicates, no
  cross-pool), the shell allow-list at `websocket.go:100-122`, and
  `validateKubeName` (DNS-1123) at `pods.go:191-205`.
- smb.conf / ganesha.conf injection is properly closed (NAS-007):
  `validateShareName`, `validateShareFields` and `validateNoControlChars`
  (`api/internal/shares/manager.go:324-385`) reject newline, tab, NUL, `;` and
  `"` in every interpolated field.
- LDAP is escaped correctly everywhere via go-ldap's `EscapeDN`/`EscapeFilter`
  (`api/internal/identity/escape.go:29-37`); no filter or DN is built from raw
  input.
- `buddyctl`'s tar extractor is traversal-safe (`Abs`+`Join`+explicit prefix
  check, non-regular entries skipped, modes masked) — `cmd/buddyctl/main.go:704-755`.

**Buddy Backup (the parts that are sound)**
- The gosec G703 path-traversal suppression is **justified**: every peer-supplied
  `source`, `chain` and `kind` passes `ValidateSource`/`validateChainName` before
  reaching `filepath.Join`, and the remaining taints come from `os.ReadDir` entry
  names, which cannot contain `/` or be `..`. The links-file contents are
  re-validated through `chainDir` on the way back in.
- **Peer isolation holds.** Every store path derives from
  `keyDir(peer.Fingerprint)` where the fingerprint is always server-derived from
  the authorised key — never peer-supplied. `PeerStore.Add` refuses both
  fingerprint shadowing and key substitution.
- Manifest publication is properly gated: same-key signature, URL/manifest source
  match, version match, all chunks present, and `Chunks[i].Index == i` — so
  reorder, duplicate, gap and truncation are all rejected. Rollback protection
  and the prune dependency walk are correct and tested.
- Quota accounting is race-free (charged under the same mutex that reads it,
  with deferred release), and `TestQuotaIsAtomicUnderConcurrentWrites` covers it.
- The signature genuinely covers method, the full path including the mount
  prefix, the query string, the body digest, timestamp and nonce; nonces are
  burned only after signature verification and survive restart.
- Primitives and randomness are correct: AES-256-GCM with enforced key length,
  96-bit nonces, Ed25519 with an explicit length check, SHA-256, and
  `crypto/rand` for **every** key, nonce, prefix, chain id and job id. No
  `math/rand` anywhere in the subsystem.
- The scheduler has no DST/timezone bug (UTC by construction), no retry storm
  (`NextRun` advanced once at start), no overlapping runs (`conflicting()`
  excludes by receiver+source), and catch-up fires once.

**TLS and transport**
- No `InsecureSkipVerify` anywhere in the repository.
- The LDAP client sets `ServerName`, `MinVersion: tls.VersionTLS12` and pins the
  CA when `CACertPath` is set (`api/internal/identity/client.go:64-81`).
- Every outbound `http.Client` has an explicit `Timeout`.
- `normalizeReceiverURL` restricts buddy receiver URLs to `http`/`https` with a
  non-empty host, blocking `file://`/`gopher://` SSRF primitives.

**Frontend**
- **No XSS sink exists**: no `{@html}`, `innerHTML`, `outerHTML`, `eval`,
  `new Function` or `document.write` anywhere in `ui/src`. Terminal output goes
  through `term.write`, not the DOM. `app.html` is minimal with no injected data.
- **No credential is stored client-side**: no `localStorage`, `sessionStorage`,
  `document.cookie` or bearer token. The session is the Authelia HttpOnly cookie
  only. The ntfy token is write-only — the API returns just `hasAuthToken`.
- WebSocket lifecycle is correct: `disconnect()` on both `onDestroy` and the
  `onMount` cleanup, `term.dispose()`, `resizeObserver.disconnect()`,
  `ws = null` after close, readyState guards before every send.
- Timers are cleaned up with in-flight guards against stacking; no global
  `window`/`document` listeners are added; every `fetch` is inside `try/catch`
  or has a `.catch()`.

**Kubernetes and ingress configuration**
- The API's RBAC is genuinely least-privilege: a namespaced `Role` for
  `pods`/`pods/log` (get,list,watch) plus `pods/exec` (create) in
  `naslos-privileged`, and one read-only `ClusterRole` for `namespaces`
  (`charts/naslos/templates/terminal.yaml:89-138`). Nothing cluster-wide, no
  `*` verbs, no secret access, no `escalate`/`bind`/`impersonate`.
- Authelia's `access_control` is correctly scoped: `default_policy: deny` with
  narrowly-resourced bypasses only for `^/api/health$` and `^/api/buddy/v1/`
  (`charts/naslos/templates/authelia-config.yaml:178-225`), and admins are
  matched by subject so 2FA is required on every path.
- The chart renders a default-deny NetworkPolicy in **both** namespaces plus 10
  per-workload allows and 2 `CiliumNetworkPolicy` host rules; every selector
  matches a real workload label in the rendered output.
- `traefik.api.dashboard` with `basePath: /traefik` (`values.yaml:233-240`)
  makes the sidebar's `/traefik/dashboard/` link resolve without a StripPrefix
  middleware, and the dashboard route is behind `forwardauth-authelia`.
- `ui/nginx.conf` is sound: the `map $http_upgrade $connection_upgrade` block is
  the correct keep-alive-safe pattern, `/api/ws/` gets its own 3600s location,
  and `/api/buddy/` raises `client_max_body_size` with
  `proxy_request_buffering off` for chunk streaming.
- `deploy-vm.sh` uses `set -euo pipefail`, requires `REGISTRY_HTTP_SECRET` with
  no fallback (`:24`), and quotes its variables.

**Tests**
- `ui/tests/totp.ts` is a correct RFC 6238 implementation, validated against all
  six RFC Appendix B vectors (`totp.spec.ts:10-17`).
- `auth.setup.ts` handles Authelia's three OTP form variants, respects the
  3-try regulation limit, `chmod 600`s the state file, and asserts
  `naslos_admins` membership before the suite runs.
- `users.spec.ts:57-95` is a genuine regression test for the "PUT silently
  ignored the password" bug, asserting on the outgoing request rather than on a
  toast.
- 7 runtime-conditional `test.skip`s exist (`backups.spec.ts`, `terminal.spec.ts`);
  `AI_Handoff.md:231` warns to treat a skip as a setup gap, but nothing asserts
  the skip count is zero.

**Secrets and supply chain**
- Chart-generated secrets use sprig's crypto-backed `randAlphaNum 48` with a
  `lookup`-based upgrade path, so they are strong and stable across upgrades
  (`agent-token.yaml`, `proxy-secret.yaml`).
- `ui/tests/*` contains no real credentials: `auth.setup.ts` reads everything
  from the environment and throws when absent, and `totp.spec.ts:8` is the
  RFC 6238 Appendix B test vector.
- `.dockerignore` correctly excludes `.git`, `.kilo`, `node_modules`,
  `bootstrap/vm`, `bootstrap/out` and the Playwright auth state.
- `openldap/manifests/secrets.yaml` is a deprecated comment-only file with no
  live values.
- All 17 `#nosec` annotations were reviewed individually and each is
  legitimate and correctly scoped.

---

## 8. Recommended order of work

1. **PF-H1** — rotate the Cilium CA, purge it from history, stop committing
   rendered Secrets.
2. **PF-M1** — fix `scripts/audit.sh` (shebang + gitleaks invocation) and re-add
   CI. Everything else depends on having a working gate.
3. **PF-H2 / PF-H3** — the two Buddy Backup crypto/restore flaws. Both are small,
   localised changes with large consequences.
4. **PF-H4** — default NFS to `Root_Squash` and a host allow-list; stop forcing
   SMB shares to `force user = root`.
5. **PF-M2** — bump the Go directive to 1.26.6 in both modules.
6. **PF-H5** — either ship the app-install refactor or mark the feature
   unavailable in the UI and the docs.
7. **PF-M3, PF-M7, PF-M8, PF-M9** — the committed Grafana password, the catalog
   map mutation, the `/api/volumes` stub, and the six dead notification events.
8. **PF-M4, PF-M5, PF-M6** — proxy-secret storage, agent TLS, agent timeouts.
9. **PF-M10…PF-M14** and the Low set.
10. **Documentation** — `docs/api.md:22-25` currently states "Authentication is
    **not enforced inside the API router today**", which is flatly false and
    dangerously misleading; correct it first. Then reconcile the app start/stop,
    snapshot, log-streaming and `/api/volumes` claims, and add the ~9 live routes
    that `api.md` omits.

---

## 9. Coverage and residual risk of *this* audit

- **Not covered:** any live/runtime behaviour, container image layer CVEs
  (`trivy` unavailable), `semgrep`, Talos machine-config semantics beyond a read,
  the vendored Traefik/Authelia/Prometheus subcharts' own code, and browser-level
  testing.
- **Lowest-confidence areas:** `api/internal/talos` (7.5% test coverage) and
  `api/internal/identity` (13.3%) received a read-through but no dynamic
  exercise; `catalog`, `helm`, `metrics` and `config` have **zero** tests.
- The two High Buddy Backup findings were reproduced by hand against the source
  before being recorded here; the Cilium key was validated with `openssl`.
