# Dataset isolation for installed apps (D3/F1)

Status: **implemented for new installs (batch 11); legacy apps still use the
shared root until migrated.** This document records the model, the implemented
per-app isolation, the catalog-compatibility caveat and the migration.

## 1. Current model

- The platform exposes the whole data disk as a static `hostPath` PV
  `naslos-datasets` (`apps.datasets.hostPath`, default `/var/mnt`), an RWX PVC of
  the same name in `naslos-apps` (`charts/naslos/templates/apps-datasets.yaml`).
  PSA `baseline` forbids `hostPath`, so the PV/PVC is how baseline apps get a
  mount at all.
- Every installed chart lives in the **same namespace** (`naslos-apps`; the
  privileged apps share `naslos-apps-priv`). Any pod in that namespace may mount
  any PVC in that namespace, and the Helm identity may create pods/workloads
  there.
- Catalog baseline apps set `datasetsClaim: naslos-datasets`; privileged apps
  (qBittorrent, Prowlarr) mount `/var/mnt` directly because PSA `privileged`
  allows it. Both see the entire data disk, including datasets unrelated to the
  app and — in the VM profile — the Buddy receive store at
  `/var/mnt/test/naslos-buddy`.
- Helm runs as `naslos-helm` (batch 6) with workload/Secret/PVC rights but no
  routes, certificates or RBAC. That closes host hijack; it does **not** narrow
  filesystem access.

### Threat model

A malicious or compromised chart (or an app exploit) running in
`naslos-apps` can read, modify or delete every share dataset on the node, and
can read the encrypted Buddy chunk store (the DEKs/KEK remain in Secrets, so
those chunks are not directly decryptable). This is a **confidentiality and
integrity** issue for all NAS data; it is the reason D3/F1 is High.

What does **not** help:

- Per-app PVCs in the same namespace: any pod may mount any PVC in its
  namespace, so a per-app claim is not an isolation boundary.
- A `subPath` chosen by the chart: the chart controls its own pod spec.
- RWX → RWO or `readOnly` mounts: they reduce integrity damage but not
  disclosure, and downloads need writes.
- Removing `hostPath` without a replacement: PSA `baseline` rejects the pod.

## 2. Phase 1 (implemented) — managed dataset registry

Operators can register specific datasets instead of exposing `/var/mnt`:

- `GET/POST /api/datasets/registry`, `DELETE /api/datasets/registry/{name}`.
- A record is `{name, hostPath, shared, warningAcknowledged}`. The API checks
  that the path is an absolute, normalized ZFS mountpoint below `/var/mnt` and
  does not overlap `buddy.receiveHostPath`; `name` is a DNS-1123 label.
- Marking a record `shared` requires `warningAcknowledged:true` and the UI shows
  a red warning plus a separate checkbox. The registry page lists every record
  and its private/shared state.

This is the input for the per-app provisioning below; it does not mount
anything yet.

## 3. Phase 2 (implemented) — per-app namespaces and claims

With `apps.isolation.enabled=true` the chart creates a fixed pool of
namespaces (`apps.isolation.namespaces` baseline + `privilegedNamespaces`
privileged). Each has PSA labels, a quota/limit range, a default-deny
NetworkPolicy (DNS, Traefik, and `naslos.local/apps=true` peers only) and
RoleBindings to two ClusterRoles:

- `naslos-helm-workloads` — the reduced Helm identity (no routes, certificates
  or RBAC);
- `naslos-api-app-isolation` — routes, certificates, Services (read) and
  per-app PVCs/Secrets for the API.

The API receives the pool in `APPS_NAMESPACE_POOL` /
`APPS_PRIVILEGED_NAMESPACE_POOL`, assigns a free namespace to each new install,
and records it in `apps.json` (`namespace`). For each selected registry dataset
it creates:

- a `PersistentVolume` with `hostPath` = that dataset only and
  `storageClassName: naslos-app-data` (no provisioner);
- a `PersistentVolumeClaim` in the app namespace bound to it by `volumeName`;
- Helm values `datasetsClaim` (first claim) and `datasetsClaims` (all claims).

Privileged apps receive the specific `hostPath` value instead (their charts
mount it directly; they are already in a privileged namespace).

A copy of the TLS Secret is written into the app namespace before the
IngressRoute, because Traefik only reads a route's Secret from its own
namespace. The source is the domain Certificate's Secret in the shared apps
namespace; when no domain Certificate exists, the platform `naslos-tls` Secret
is copied instead (the API has `get` on that one Secret only). The copy is
refreshed whenever the route is re-applied (startup reconcile, exposure update,
upgrade).

`apps.isolation.enabled` is true in the VM and installer overlays; existing
records without a `namespace` stay in the legacy shared namespace. Fresh
installs set `apps.datasets.enabled=false`, so the shared `/var/mnt` PV/PVC is
not created at all: register a dataset under `/datasets` and select it when
installing the app. The VM overlay keeps the shared PV for legacy records until
they are migrated.

### Catalog compatibility

The claim's root is the selected dataset, not `/var/mnt`. A chart that mounts
`datasetsClaim` at `/data` therefore sees the dataset at `/data`, so catalog
values that assumed `/data/<pool>/<dataset>` must use `/data/<subdir>` (or the
dataset root). The current NaslosCharts values that default to `/var/mnt` must
be pointed at the selected dataset path. This is a catalog change per app, not
a platform one; until it lands, an isolated install may need value overrides.

## 4. Options

### A. Per-app namespace + per-dataset PV/PVC (recommended)

Each app gets `naslos-app-<release>` with its own PSA labels, NetworkPolicy,
ServiceAccount and PVCs. The API (not the chart) creates the namespace, the
RoleBinding for `naslos-helm`, and one PVC per declared dataset, bound to a
PV whose `hostPath` is **that dataset only** (e.g.
`/var/mnt/tank/media`, not `/var/mnt`). The catalog manifest declares the
dataset names/paths and the chart mounts the claims by name. Routes still point
at the app namespace.

- **Pros:** a chart cannot mount another app's claim (namespaces differ), and
  no app can reach the Buddy store or an undeclared dataset.
- **Cons:** the API needs cluster-scoped PV create/delete (it already has
  Talos root and `pods/exec`, so this is not a new trust class), namespace
  lifecycle and RBAC per app, and catalog charts must accept per-dataset claims.
  Cross-namespace routing/NetworkPolicy/reverse-proxy work is required.
- **Migration:** create the new namespace/PV/PVC for an app, copy or bind the
  existing dataset, update `datasetsClaim`/`hostPath` values, reinstall the
  chart, then delete the old release. The shared PV/PVC is removed only after
  every app has migrated.

### B. Curated datasets root (intermediate)

Keep one namespace, but replace the PV's `hostPath` with a curated root
(e.g. `/var/mnt/naslos-apps`) that contains bind mounts of only the datasets
the operator exposes. The Buddy store stays outside that root. This excludes
unrelated datasets and the Buddy store with far less churn, but apps still see
each other's exposed data.

- **Pros:** no per-app namespaces; catalog paths can stay the same if the
  bind-mount layout mirrors the pool tree (`/var/mnt/naslos-apps/tank` →
  `/data/tank`).
- **Cons:** bind mounts are not persistent across a reboot; the agent or a
  Talos extension must recreate them at boot (the agent is privileged but its
  mount namespace is private, so this needs a hostPID/nsenter helper or an
  installer-managed system extension). The API must validate every exposed
  path and reject the Buddy path.

### C. Explicit shared-root opt-in (accepted risk)

Keep the current model but make it an explicit, documented operator choice and
keep the Buddy store outside the exposed root. This does not close D3/F1; it
records the residual and is only acceptable if the operator treats installed
charts as trusted code.

### Migrating an existing app

Existing records have no `namespace` and stay in `naslos-apps` on the shared
PVC. To isolate one:

1. Register the dataset it actually uses (`/datasets`).
2. Reinstall it with the dataset selected (the UI install flow). The API
   allocates a per-app namespace and claim; chart values must use the claim
   root (see the catalog caveat).
3. Verify the app and its route, then uninstall the legacy release (the shared
   PVC is untouched). Repeat for every app.
4. When no record lacks a namespace, delete the `naslos-datasets` PVC/PV and set
   `apps.datasets.enabled=false` (the legacy path can then be removed from the
   chart in a later release).

Data is not moved by the API: the registered dataset's `hostPath` is mounted
directly, so the new claim sees the same data.

## 5. Recommended sequence

1. **Move the Buddy receive store out of the shared root.** On the VM the store
   is `test/naslos-buddy` under `/var/mnt`. The cleanest migration is to change
   that ZFS dataset's mountpoint to a directory outside the exposed root
   (e.g. `/var/lib/naslos-buddy`) and update
   `charts/naslos/values-vm.yaml:buddy.receiveHostPath`; the dataset and its
   chains move with the mount, so no copy is needed. Do this in a maintenance
   window with a pre-test snapshot, then verify `buddyctl list`.
2. **Add the catalog `datasets` schema** (name, hostPath under an allowlisted
   root, mountPath, readOnly) and have the API reject paths outside
   `/var/mnt/<pool>/...` and any path under `buddy.receiveHostPath`.
3. **Per-app namespace and per-dataset PVC/PV** (Option A), one app at a time,
   with the shared PV/PVC removed only after the last migration.
4. **Docs and UI:** show the selected datasets and their access mode in the
   install confirmation, and make the shared-root mode visibly degraded.

### Migration runbook — Buddy store (VM profile)

This is a maintenance-window operation; take a pre-test snapshot and keep the
last release pack as the rollback. The store is a ZFS dataset, so changing its
mountpoint moves the existing chains without copying.

```bash
# 1. Stop the API so nothing holds the old mount.
kubectl -n naslos scale deploy/naslos-api --replicas=0

# 2. On the node, create the new mountpoint and move the dataset.
#    (Talos: run these from a privileged debug pod with /var/mnt and /var/lib
#     mounted, or via `talosctl` with the same effect.)
mkdir -p /var/lib/naslos-buddy
zfs set mountpoint=/var/lib/naslos-buddy test/naslos-buddy

# 3. In charts/naslos/values-vm.yaml:
#      buddy.receiveHostPath: /var/lib/naslos-buddy
#    and delete apps.datasets.allowBuddyStore (the guard then passes).
#    helm upgrade naslos charts/naslos -n naslos -f values.yaml -f values-vm.yaml

# 4. Verify before scaling the API back up.
kubectl -n naslos scale deploy/naslos-api --replicas=1
buddyctl status --identity /var/lib/naslos-buddy-identity.json
```

If the dataset cannot be moved (busy mount), use `zfs send | zfs receive` into a
new dataset with the new mountpoint and swap the paths after verifying the
copy. Rolling back is the reverse `zfs set mountpoint` plus the previous values.

## 6. Interim guardrails already in place

- The chart rejects an obviously unsafe `apps.datasets.hostPath` (`/`, `/var`,
  `/etc`, `/home`, `/root`; see `apps-datasets.yaml`) and refuses to render when
  `buddy.receiveHostPath` is inside the exposed root unless
  `apps.datasets.allowBuddyStore=true` is set explicitly. The VM overlay sets
  that flag with a comment until the Buddy dataset is remounted; the audit also
  prints a notice for that profile.
- Helm cannot create Traefik/cert-manager/RBAC objects, and the app Jobs/UI
  require explicit install confirmation (including a separate privileged
  confirmation).
- The Buddy store is encrypted at rest: chunks are AES-GCM sealed, DEKs are
  wrapped by a KEK in a Secret that apps cannot read. An app that reads the
  store gains ciphertext, not plaintext.

## 7. Residual risk

New installs are isolated: a chart in `naslos-app-*` cannot mount another app's
claim (different namespaces) and only sees the datasets attached to it. A
shared dataset is intentionally visible to every app it is attached to.

Legacy records in `naslos-apps`/`naslos-apps-priv` still see the shared
`/var/mnt` PVC (and the encrypted Buddy store on the VM) until they are
reinstalled into an isolated namespace and the shared PV/PVC is removed. This
residual is recorded as **D3/F1 open for legacy apps** in
`docs/AUDIT-2026-10-24.md`.
