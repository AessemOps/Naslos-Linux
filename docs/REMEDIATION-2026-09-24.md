# Post-fix audit remediation (2026-09-24)

This records the implementation of
`.kilo/plans/1789809528712-postfix-audit-remediation.md`, the follow-up to
`docs/AUDIT-2026-09-21-POSTFIX.md`. It doubles as the upgrade/release note for
the deliberate behaviour changes.

The work landed on branch `remediation/postfix-audit-2026-09-21` (never directly
on `master`).

> **History was purged.** The Cilium CA/Hubble keys and the historical
> OpenLDAP password were removed from every ref with `git filter-repo`, and
> `master`, `audit/full-2026-09-19`, `refactor/remove-dev-endpoint` and this
> branch were force-pushed. `gitleaks dir .` and `gitleaks git .` are clean
> (the fresh-clone scan reports only the test/plan fixtures allowlisted in
> `.gitleaks.toml`). A rollback bundle of the pre-purge history is kept outside
> the repository at `/tmp/kilo/pre-purge-backup.bundle`.

---

## 1. Breaking / behaviour changes (read before upgrading)

1. **Buddy Backup protocol is now v2.** The chunk envelope is `NBC2`/`NB2`; the
   nonce is `prefix(6) || generation(2) || index(4)`; the manifest carries a
   per-chunk `generation` and a per-source `sequence`; request signatures are
   `BUDDY2` and bind to the receiver's identity.
   - Chains created by a v1 sender will **not** verify. Finish or discard
     in-flight chains, then start a new one.
   - Upgrade both ends (sender and receiver) together; a mixed fleet fails
     closed.
   - Give each receiver a distinct `BUDDY_NAME` — the signature audience is the
     receiver's name and the default `naslos` is identical everywhere.
2. **NFS/SMB defaults are restrictive now (PF-H4).**
   - A share with no `allowedHosts` is limited to `NASLOS_LAN_CIDR` (the chart
     passes `networkPolicy.nfsClientCIDR`); if that is unset it falls back to
     localhost only. Explicit `"*"` is the only way to export to everyone.
   - NFS squashes root by default. Set the new per-share **No root squash**
     toggle only when an admin client must write root-owned data; the UI warns.
   - SMB no longer runs shares as `force user = root`; the session uses the
     authenticated user's mapped uid, with masks `0660`/`0770`. Datasets whose
     existing ownership/ACLs assume root writes may need adjusting.
3. **The API↔agent channel is HTTPS by default (PF-M5).** The chart generates a
   CA + server certificate (`naslos-agent-tls`) and pins it in the API. Set
   `agent.tls.enabled=false` only for local development (plaintext).
4. **OpenLDAP serves LDAPS only (PF-L9).** The plaintext `ldap:///` listener is
   removed; `ldaps://636` and the local `ldapi:///` socket remain. All
   in-cluster consumers already use LDAPS.
5. **`TRAEFIK_CIDR` default narrowed to `10.244.0.0/16`** (the pod CIDR Traefik
   sources from), from the previous `10.0.0.0/8` (PF-M4).
6. **Grafana is removed from the chart** (PF-M3): dependency, vendored tarball
   and values blocks. A committed static admin password is gone for good.
7. **`/api/volumes` is removed** (PF-M8): it was a stub that answered `201`
   without applying anything; it now 404s. ZFS routes under `/api/volumes/zfs`
   are unchanged.
8. **Notification events trimmed** to the four implemented ones (`zfs_health`,
   `disk_failure`, `backup_success`, `backup_failure`); `zfs_health` and
   `disk_failure` are now actually emitted by a 5-minute health watcher (PF-M9).
9. **App-catalog install UI is disabled** (PF-H5): the feature cannot work, so
   the docs say so and the install action is gated off until the catalog
   refactor lands.
10. **`bootstrap/cilium/cilium.yaml` regenerated** without committed Secrets
    (PF-H1). It now sets `hubble.tls.auto.method=cronJob`, so the
    `hubble-generate-certs` job creates `cilium-ca` and `hubble-server-certs`
    in-cluster. A header comment documents the exact regeneration command.

## 2. Finding status

| Finding | Status | Where |
|---|---|---|
| PF-H1 committed Cilium keys | **Done**: secrets removed, cronJob certgen, manifest re-rendered, history purged across all refs and force-pushed. **CA rotation pending on the live VM** | `bootstrap/cilium/cilium.yaml`, `scripts/render-cilium.sh` |
| PF-H2 AES-GCM nonce reuse | Fixed: generation domain, per-chunk generation in manifest+AAD, resume re-seals the tail under a fresh generation | `envelope.go`, `client.go`, `store.go` |
| PF-H3 Restore not bound | Fixed: version/source/chain binding + monotonic publish sequence | `client.go` |
| PF-H4 NFS/SMB posture | Fixed: LAN-CIDR/`*`-opt-in default, Root_Squash default + toggle, no root forcing, tighter masks, SMB hardening | `shares/config.go`, chart, UI |
| PF-H5 catalog non-functional | Docs/UI marked unavailable; missing RBAC + bad repo URLs recorded in the refactor plan | `README.md`, `docs/app-catalog.md`, `docs/spec.md`, UI |
| PF-M1 gate aborts under sh | Fixed: bash shebang, usage, gitleaks `dir`+`git` | `scripts/audit.sh` |
| PF-M2 Go toolchain advisories | Fixed: `go 1.26.6` in both modules; accepted list documents the fixed set | `api/go.mod`, `agent/go.mod`, `scripts/audit.sh` |
| PF-M3 Grafana | Removed entirely; audit guard added | chart |
| PF-M4 proxy secret / CIDR | CIDR narrowed; signed-assertion redesign left as a suggestion | chart, docs |
| PF-M5 agent TLS | Fixed: chart-issued cert + CA pinning | agent, chart, `api/internal/agent/client.go` |
| PF-M6 agent timeouts | Fixed: header/read/idle deadlines, stream route clears its read deadline, `MaxBytesReader` | `agent/internal/server` |
| PF-M7 catalog aliasing | Fixed: `Catalog.Get` deep-copies; install merges into a clone | `catalog/catalog.go`, `apps.go` |
| PF-M8 `/api/volumes` stub | Removed; docs corrected | `disks.go`, `server.go`, docs |
| PF-M9 dead notification events | Wired `zfs_health`/`disk_failure`; removed the rest | `notifications`, `health_notify.go`, UI, docs |
| PF-M10 no default quota | Finite default quota at enrollment + free-space reserve | `http.go`, `store.go` |
| PF-M11 fail-open publish guards | Fail closed (force bypass), `ShortTail` surfaced | `buddy_jobs.go`, `client.go` |
| PF-M12 manifest cap | Raised to 64 MiB, pre-flight refusal, truncation detection | `http.go`, `client.go` |
| PF-M13 signatures not receiver-bound | Fixed: audience (receiver identity) in `BUDDY2` | `auth.go`, `http.go`, `client.go` |
| PF-M14 identity at rest | KEK can come from a Secret (`BUDDY_KEK`); at-rest exposure documented | `keys.go`, chart, docs |
| PF-L3 API securityContext | Added (non-root, no caps, RuntimeDefault) | chart |
| PF-L4 terminal SA token | `automountServiceAccountToken: false` | chart |
| PF-L5 folder symlink escape | `CleanFolderPath` resolves the host root and target | `agent/internal/shares/folders.go` |
| PF-L8 ntfy SSRF | http/https only, link-local refused, topic charset | `notifications/manager.go` |
| PF-L9 LDAP cleartext | Plaintext listener removed; `LDAPTLS_REQCERT` no longer `never` | `openldap/image/Dockerfile`, chart |
| PF-L12 OpenChunk prefix | Prefix + generation come from the signed manifest | `envelope.go`, `client.go` |

Tracked-open (unchanged here): PF-L1/L2/L6/L7/L10/L11/L13–L18 and the UI
accessibility appendix — see the plan §6.

## 3. Verification performed

- `bash scripts/audit.sh` reaches the end; the only failure is
  `gitleaks (git history)` (PF-H1, pending the gated purge).
- `go build`/`go vet`/`go test`/`go test -race`/`staticcheck` on both modules;
  `govulncheck` reports only the four accepted `Fixed in: N/A` advisories; `gosec`
  high severity clean; `helm lint`/`helm template` for both values files;
  `npm audit` and `svelte-check` clean.
- New negative tests that fail against the old code: nonce-generation change vs
  unchanged ciphertext, restore source/chain/stale-manifest rejection, resumed
  tail restore, signature audience binding, KEK from the environment, default
  enrollment quota, short-tail flagging, catalog deep-copy, symlink escape,
  notification URL hardening, SMB hardening defaults.

## 4. Pending live steps (need the VM / approval)

1. ~~Approve and run the history purge~~ — **done** (see the note at the top).
   The three residual `gitleaks git` findings on a bare clone are intentional
   test fixtures and a plan example, allowlisted in `.gitleaks.toml`.
2. **Rotate the Cilium CA** on any deployed cluster. The purge removes the keys
   from this repository, but they were exposed while committed, so rotation is
   what actually closes the exposure (all clones/forks keep the old objects).
3. **Redeploy to the VM** and confirm Cilium comes up with generated certs, the
   Hubble TLS listener works, the agent serves HTTPS, and existing mounts still
   connect (or are reconfigured for the new defaults).
4. **Run the Playwright suite** (not run here) and update any spec that assumed
   a working catalog install or `*` NFS exports.
