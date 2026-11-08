# Store app PVCs in a chosen ZFS pool

## Goal

Let an app install target a ZFS pool. Every chart-created persistence PVC
(config, database, uploads) is provisioned under `/var/mnt/<pool>/apps`, while
the existing managed-dataset selector still controls the app's media/data
claim.

## Design

- The install dialog gains a **Storage pool** selector (default: local-path).
  `POST /api/apps` carries `pool`; the API validates the pool against the
  agent's pool list.
- For a non-empty pool the API ensures a pool-scoped local-path provisioner:
  - ConfigMap `<class>-config` and Deployment `<class>-provisioner` in
    `local-path-storage`, reusing the existing provisioner ServiceAccount;
  - StorageClass `<class>` (`naslos-pool-<slug>-<hash>`, lowercase, hash from
    the exact pool name) with `provisioner: <class>`;
  - path `/var/mnt/<pool>/apps`, so the provisioner's hostPath PVs land in the
    pool.
- The API walks the merged chart values and sets every `storageClass` under a
  `persistence` object to `<class>`. Charts already honour
  `persistence.<key>.storageClass`; the managed-dataset claim keeps
  `naslos-app-data`.
- The provisioner/StorageClass are idempotent (server-side apply). The API
  waits for the provisioner Deployment to become ready before Helm runs.
- RBAC: the API gains `storageclasses` write and Deployment/ConfigMap write in
  `local-path-storage`; the provisioner keeps its existing ServiceAccount.

## Safety

- Pool names are validated and are not allowed to contain `/`; the path is
  always `/var/mnt/<pool>/apps` (never the datasets root).
- A missing pool is a 400, not a pending PVC.
- The existing local-path default is unchanged for installs without a pool.
- No chart changes are required; all catalog charts already expose
  `persistence.<key>.storageClass`.

## Verification

- Rust tests: `pool_storage_class`, `set_persistence_storage_class`, pool
  name/path validation.
- `npm run check && npm run build` for the UI; `helm lint` for the chart.
- Live: install a small app into `ssd-storage`, confirm its PVC/PV hostPath is
  under `/var/mnt/ssd-storage/apps`, then uninstall.
