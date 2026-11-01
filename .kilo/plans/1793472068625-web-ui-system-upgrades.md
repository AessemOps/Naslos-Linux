# Upgrade Naslos, Talos and Kubernetes from the web UI

Status: proposed · Target release: **0.3.0** (both repos, version parity)
Commit this plan to `Naslos-Linux/.kilo/plans/` when implementation starts.
Repos: `Naslos-Linux` (API, upgrader, chart, UI, pack, spec/docs) → then
`Naslos-Installer` (pack-pinned Kubernetes, resume-converge bootstrap path)

## Decisions (confirmed)

| # | Decision |
| --- | --- |
| D1 | Talos target = the `talosVersion` + `schematicId` **pinned by the installed Naslos release**. Never an arbitrary Factory version. |
| D2 | Only **installer-profile** deployments are web-upgradable. A `make install-vm` deployment shows versions, disables Upgrade, and points to `make install-vm`. |
| D3 | Source/trust = GitHub releases of `AessemOps/Naslos-Linux`, newest strict `vX.Y.Z` (same rule as the installer), pack sha256 + per-member checksums. Pack signing is a follow-up for installer and node together. |
| D4 | Kubernetes is upgraded too (`talosctl upgrade-k8s`), to a `kubernetesVersion` **pinned in the pack**. |
| D5 | Order is always **Naslos → Talos → Kubernetes**, one operation at a time, each pinned by the installed release. |

## Facts that constrain the design (verified in code)

- The API has **no** mutating Talos call (`rust/api/src/talos.rs` is read-only `talosctl`), no release check, and no system page.
- The API **cannot** upgrade its own release by design: no write access to `naslos` (`charts/naslos/templates/rbac-apps.yaml:86-163`), and `naslos-helm` is limited to the app namespaces (`rbac-helm.yaml`). Its own pod is also replaced mid-upgrade. → upgrades run as an **in-cluster Job**.
- The installer stores **fully merged values** in the release (`MergeValues` in `Naslos-Installer/internal/helm/helm.go`), including the old image digests. Reusing `helm get values` would silently keep the old images. → the upgrader rebuilds only the engine overrides (`helm.Overrides`, `helm.go:137-158`): `domain`, `sso.domains`, `shares.discovery.name`, `openldap.host`, `api.talosConfigSecret`, `networkPolicy.{nodeCIDR,ingressPluginsCIDR,nfsClientCIDR}`. Every other value comes from the new pack.
- `templates/openldap-bootstrap-job.yaml` is a `post-install,post-upgrade` hook → same rule as the engine: Helm without `--wait`, then explicit workload waits.
- The installer profile already allows API egress on 443 (`gitEgress`/`ddnsEgress`), and the API image already pins `helm`, `kubectl` and `talosctl` (`rust/api/Dockerfile:31-51`).
- The API already holds a full-admin talosconfig (`docs/architecture.md:321`), so a cluster-admin upgrader Job adds no trust boundary the API doesn't already cross. The admission policy below is defense in depth.

## Architecture

1. **Release facts on the node.** `scripts/build-install-pack.sh` injects into the pack's `values-installer.yaml` (the same step as digest pinning):
   `release: {profile: installer, naslosVersion, talosVersion, schematicId, kubernetesVersion}`. The chart renders these as API env vars. Every install/upgrade path carries them, with no engine-side state. No `release.profile` ⇒ not web-upgradable (D2).
2. **Update check (API).** A background task (daily) plus `POST /api/system/updates/check` lists GitHub releases, keeps strict `vX.Y.Z` tags newer than the running version, downloads the candidate pack and its `.sha256`, verifies both, reads `metadata.json`, and caches the result in `/var/lib/naslos/updates/`. The source base URL is a chart value (`system.updates.source`) so drills can use a local HTTP mirror — **never throwaway semver tags on the real repo**.
3. **Stepping rule.** Offer the newest release whose `talosVersion` and `kubernetesVersion` are each **≤ 1 minor** ahead of what is running. Otherwise offer the intermediate release and say why. A Naslos upgrade is blocked while the node's Talos or Kubernetes is behind the *current* release's pins ("finish pending system upgrades first").
4. **Operations = one Job each.** The API creates `Job naslos-system-op-<id>` in `naslos`, SA `naslos-upgrader` (ClusterRoleBinding `cluster-admin`), mounting `naslos-talosconfig`. The command is the new binary `naslos-upgrade <op> …`, built into the API image:
   - `naslos --version X --sha256 S` runs on the **target** release's API image (digest from the verified pack `images.api`), so the new release migrates itself.
   - `talos` / `kubernetes` run on the **current** API image, whose `talosctl` matches the current pins (release guard below).
5. **Status/lock.** ConfigMap `naslos-system-operation` in `naslos` holds `{id, op, from, to, phase, steps[], message, startedAt, finishedAt, result}`. The Job writes it; the API reads it. Its presence with `phase ∉ {succeeded, failed}` is the lock (409). It survives API restarts and node reboots.
6. **Admission guard.** A `ValidatingAdmissionPolicy` + binding rejects any Pod using SA `naslos-upgrader` unless its image matches `ghcr.io/aessemops/naslos-api@sha256:*` and its command is `naslos-upgrade`.

### Operation steps (contract: `docs/installer-contract.md` new §7, mirrors the engine)

**`naslos`**
1. Re-download the pack. Require sha256 == the value the admin approved (pinned in the Job args), then verify per-member checksums.
2. Preflight: `release.profile == installer`, `to > from`, stepping rule, no app/Buddy job running (the API checks before creating the Job; the Job re-checks the lock).
3. Take an etcd snapshot (`talosctl etcd snapshot`) to `/var/lib/naslos/backups/etcd-<ts>.db` (PVC; keep the last 3).
4. Converge the prerequisites exactly like the engine: apply local-path → apply cert-manager CRDs (server-side) → install/upgrade `cert-manager` (`crds.enabled=false`) and `cert-manager-webhook-ovh`. This also brings older nodes that lack them up to date.
5. `helm upgrade naslos` from the pack chart with `values.yaml` + `values-installer.yaml` + the rebuilt overrides, no `--wait`, then explicit waits (OpenLDAP, Authelia, API, UI). Then check that the running images equal the pack's `images` digests.
6. On failure: `helm rollback naslos <previous revision>`, wait, set `result=failed` with the step and error.

**`talos`** (allowed only when the running Talos version < `release.talosVersion`)
1. Preflight: Naslos already at the release, ZFS pools healthy and no scrub/resilver running (agent API), no Buddy/app job running, etcd snapshot.
2. Write `phase=rebooting`, then `talosctl upgrade --image factory.talos.dev/installer/<schematicId>:<talosVersion>`, non-blocking. **Verify the exact v1.14 flags (`--wait`, `--stage`, preserve semantics) against `talosctl upgrade --help` before coding.**
3. The Job pod dies with the reboot. The Job retries after boot (`backoffLimit` > 0, idempotent). If the running version == target: wait for node Ready + pools imported → `succeeded`. If it is still the old version: Talos A/B rolled back → `failed` with the reason.
4. Align `machine.install.image` with the new installer image (`talosctl patch mc`) so a later reinstall/upgrade uses the same image. Verify whether 1.14 already does this.

**`kubernetes`** (allowed only when Talos == the release pin and the running Kubernetes < `release.kubernetesVersion`)
1. Preflight + etcd snapshot. Run `talosctl upgrade-k8s --to <ver> --dry-run`, then the real upgrade.
2. Verify the kubelet and control-plane versions. There is no automatic rollback; on failure, report the step and link the manual recovery doc.

### API (admin-only, owner router)

- `GET /api/system/version` → running Naslos/chart revision, Talos, Kubernetes, schematic, profile, `upgradable`.
- `GET /api/system/updates`, `POST /api/system/updates/check` → cached check plus the stepping decision and release notes URL.
- `POST /api/system/operations {op, version?}` → `202 {operationId}`. `409` if an operation, app job or Buddy job is running. `412` if a precondition fails (profile, order, stepping, pool health).
- `GET /api/system/operations/current` → the ConfigMap state plus a log tail from the Job pod.
- RBAC additions for `naslos-api` in `naslos`: `jobs` create/get/list/delete; `pods`/`pods/log` get/list for those Jobs; `configmaps` get on `naslos-system-operation` only.

### UI

- New **Settings → System** page:
  - version table (Naslos, chart revision, Talos, Kubernetes);
  - "Check now";
  - the available release with a notes link;
  - three ordered actions with disabled reasons ("upgrade Naslos first", "dev VM profile — use `make install-vm`").
- Confirm dialogs: type the version. The Talos dialog warns that shares and apps go offline during the reboot.
- Progress panel driven by `/operations/current`. While the API is unreachable during a reboot it shows "node rebooting, reconnecting…" and keeps polling; after login it shows the final result.
- Sidebar/dashboard badge when an update is available. Optional ntfy notification through the existing notification settings.

## Ordered tasks

### Naslos-Linux (release first)
1. Spec: add `FR-UPD-01..` covering D1–D5, the stepping rule, the lock, rollback, the admission guard and the profile gate. Mark each `[OPEN]` until its test lands.
2. Pack: inject `release.*` into the pack's `values-installer.yaml`, add `kubernetesVersion` to `metadata.json` (additive, `formatVersion` stays 1), and extend `check_install_pack`.
3. Release/audit guards: `talosctl` version in `rust/api/Dockerfile` == pack `TALOS_VERSION`; `kubectl` within ±1 minor of `kubernetesVersion`; the pinned `kubernetesVersion` matches what the installer deploys.
4. Chart: `release.*` → API env; `naslos-upgrader` SA + ClusterRoleBinding; `ValidatingAdmissionPolicy`(+Binding); API RBAC additions; `system.updates.{enabled,source,intervalHours}`.
5. `rust/api`:
   - new `naslos-upgrade` bin (`naslos` / `talos` / `kubernetes` ops, status ConfigMap writer, override rebuild, helm/kubectl/talosctl adapters reused);
   - update checker + stepping rule;
   - `server/system.rs` endpoints;
   - unit tests: override rebuild from a real merged-values fixture, semver filter, stepping rule, status state machine, Job spec rendering.
6. `ui`: System page, progress panel, badge. `npm run check`, Playwright spec `tests/system.spec.ts` (renders versions; disabled states on the VM profile).
7. Docs: `installer-contract.md` §7 (upgrade steps + `release.*` + `kubernetesVersion` + the override key list), `operations.md` "Upgrading" + manual recovery (helm rollback, Talos A/B rollback, failed `upgrade-k8s`), `api.md`, `architecture.md` (fix the "machinery client"/"Helm SDK" drift while there), README feature list, `CREDITS.md` if dependencies change, `AI_Handoff.md`.

### Naslos-Installer (after the 0.3.0 pack is released)
8. Generate the machine config with the pack's `kubernetesVersion` (explicit, not the machinery default).
9. `--resume` converges: when the deployed chart version < the pack's, re-run the helm step (the engine recomputes overrides from its inputs, which is correct). This is the **bootstrap path for 0.2.x nodes** to the first web-upgradable release, and a CLI upgrade path for nodes without UI access.
10. Require `--admin-password` only when the `admin` step is pending, so the step-9 resume needs no throwaway value. This fixes the defect recorded in `AI_HANDOFF.md`.
11. Docs: README (upgrade via `--resume`), `AI_HANDOFF.md`, plan in `.kilo/plans/`.

### Release
12. Bump both repos to 0.3.0 → dry run → tag → auto-dispatch. Then upgrade the VM: installer 0.3.0 `--resume` (bootstrap), and from then on only the web UI.

## Validation

- Unit/CI: the Rust tests above, `helm template` asserts (RBAC, policy, env), audit guards, `make install-pack`, `scripts/audit.sh`, installer `make check`.
- Drill source: a local HTTP mirror (`system.updates.source`) serving two pack builds (e.g. `0.3.0` and a locally built `0.3.1` via `PACK_VERSION`). **No throwaway `vX.Y.Z` tag on GitHub.**
- Live drills on `192.168.1.117`, each recorded in the handoff:
  1. Naslos 0.3.0 → 0.3.1 via UI. Images == pack digests, overrides preserved (domain, SSO, CIDRs), cert-manager untouched, OpenLDAP data intact.
  2. Forced failure (bad image digest in the drill pack) → automatic helm rollback, UI shows `failed`.
  3. Talos patch upgrade (pack pinning a newer v1.14.x) → reboot, pools re-imported, UI reconnects and shows `succeeded`.
  4. Kubernetes patch upgrade via `upgrade-k8s`.
  5. Lock: a second operation → 409. An app job running → 409. VM profile → Upgrade disabled.
  6. Admission guard: a Pod with SA `naslos-upgrader` and another image is rejected.

## Risks

- **Talos upgrade bricks boot**: Talos A/B rollback, etcd snapshot, and the stepping rule (≤1 minor). The Job detects a rollback after reboot.
- **Override drift**: the rebuild uses the installer's exact key list. A unit fixture from a real 0.2.6 release guards it; the contract doc lists the keys next to `helm.Overrides`.
- **Upgrader Job is cluster-admin**: same blast radius as the existing admin talosconfig. Narrowed by the admission policy, admin-only endpoints with Authelia 2FA, and the approved sha256 pinned in the Job args.
- **Self-replacement**: handled by running the Job on the target image, outside the API pod.
- **In-memory app/Buddy jobs** are lost on an API restart; the API checks them before creating an operation, and the operation lock is in etcd, not memory.

## Out of scope

Pack signing (follow-up), offline/air-gapped pack upload, automatic or scheduled upgrades, downgrades, multi-node clusters, upgrading `make install-vm` deployments (D2).
