# Attach multiple datasets to an app

## Goal

An app may attach any number of operator-registered datasets, both at install
and when reconfigured. For each selected dataset the API creates a PV/PVC
(baseline apps) or exposes the dataset's host path (privileged apps), and the
catalog charts mount every attached dataset at `/datasets/<name>`. Existing
apps with datasets are auto-upgraded once to the new mount layout.

## Confirmed decisions

- **Mount contract:** platform-chosen paths. Every attached dataset is mounted
  at `/datasets/<name>`; charts iterate a new `datasets` Helm value.
- **Attach timing:** install and reconfigure (`PUT /api/apps/{name}`).
- **No `/data` compatibility mount:** when `datasets` is non-empty the charts
  mount only `/datasets/<name>`. When no dataset is attached, each chart keeps
  its current fallback (`datasetsClaim` for baseline, `hostPath` for privileged).
- **Catalog charts:** all NaslosCharts apps that currently consume
  `datasetsClaim`/`hostPath` are updated in the same change.
- **Existing apps:** auto-upgrade once on API startup after the catalog is
  refreshed, marked by a new `datasetsLayout: "v2"` record field. A chart is only
  migrated when its templates reference `.Values.datasets`, so a premature API
  deploy cannot lock in the old layout.

## Helm / API contract

New Helm value (injected by the API, never trusted from a client):

```json
"datasets": [
  { "name": "media",     "mountPath": "/datasets/media",     "claim": "naslos-ds-sonarr-media" },
  { "name": "downloads", "mountPath": "/datasets/downloads", "hostPath": "/var/mnt/tank/downloads" }
]
```

- Baseline entries carry `claim`; privileged entries carry `hostPath`.
- `datasetsClaim` (first claim) and `datasetsClaims` (all claims) stay for
  backward compatibility with older chart releases.
- The API request/record shape stays `datasets: Vec<String>` (registry names).
- The API always strips `datasets`, `datasetsClaim` and `datasetsClaims` from
  client-supplied `values` and re-injects them from the reconciled record, so a
  client cannot point a chart at an arbitrary claim or host path.

## Ordered tasks

### 1. API: dataset provisioning and reconciliation

Files: `rust/api/src/apps.rs`, `rust/api/src/server/apps.rs`,
`rust/api/src/server/app_jobs.rs`, `rust/api/src/main.rs`.

1. **Remove the one-dataset guard** in `provision_datasets` (`apps.rs:995`).
2. **Normalise selection**: trim/dedupe names while preserving order; reject an
   empty name; every name must exist in the registry. The existing
   `Registry::attach` still enforces private/shared semantics.
3. **Extract pure helpers** (unit-testable):
   - `claim_name(app, dataset) -> String` using the existing
     `naslos-ds-{app}-{name}` form.
   - `dataset_helm_values(privileged, names, registry) -> Map<String, Value>`
     building the `datasets` entries plus `datasetsClaim`/`datasetsClaims`
     (`datasetsClaim` = first claim; privileged also receives the first
     `hostPath` for old charts).
   - `retain_client_values` / `strip_dataset_values` removing the three
     platform-managed keys before merge and JSON-schema validation.
4. **Refactor `provision_datasets`** to take the previously attached names
   (`existing: &[String]`):
   - Apply the PV/PVC for every selected dataset (idempotent), including
     unchanged ones, but only treat *newly created* claims as rollback
     candidates.
   - On failure, delete only new claims and detach only newly attached registry
     entries; never detach a dataset the app already had.
   - Keep `/datasets/<name>` as the `mountPath`; it is safe because registry
     names are DNS-1123 labels.
5. **Guard claim-name collisions** before `apply`: read the existing PV/PVC; if
   it exists with a different `naslos.local/app` or
   `naslos.local/managed-dataset` label, fail with a clear error instead of
   overwriting another app's PV.
6. **`install`**: normalise the selection, provision, inject the dataset values,
   store `datasets` and `datasets_layout = "v2"` when non-empty.
7. **`upgrade`**: change the signature to
   `upgrade(name, values, datasets: Option<&[String]>, progress)`.
   - `datasets = None` keeps `rec.datasets` and re-injects its values.
   - `datasets = Some(list)` reconciles adds/removes.
   - Reject a dataset change for a legacy record with an empty `namespace`
     ("reinstall the app into an isolated namespace to attach datasets").
   - Merge user values, strip platform keys, validate against the catalog
     schema, provision, inject dataset values, then run Helm.
   - On Helm failure, roll back newly created claims and attachments; on
     success, delete/detach removed datasets (best effort, log failures) and
     persist the new `datasets`, `values` and `datasets_layout = "v2"`.
8. **`migrate_dataset_layout`** (new public `Manager` method):
   - Skip orphaned records and records without datasets, or already
     `datasetsLayout == "v2"`.
   - Resolve the chart; skip (and log) when its `templates/` tree does not
     mention `.Values.datasets` (chart not on the channel yet).
   - Call `upgrade(name, None, None, progress)` sequentially. A failure leaves
     the record unmarked and is logged; the next API start retries.
   - Spawn it from `main.rs` *after* the startup catalog refresh stores the new
     catalog (`main.rs:48-59`), not before.
9. **HTTP/job plumbing**:
   - `UpgradeBody` gains `#[serde(default)] datasets: Option<Vec<String>>`
     (`server/apps.rs`).
   - `AppJob`/`enqueue` gain an optional dataset list; `run_job` passes it to
     `Manager::upgrade`. Install/uninstall pass `None`/their existing request.
   - Keep `datasets` omitted = unchanged; `[]` = detach all.

### 2. API tests

Files: `rust/api/src/apps.rs` (`#[cfg(test)]`), `ui/tests/apps.spec.ts`.

- Unit-test `normalise`/dedupe, `claim_name`, `dataset_helm_values` (baseline
  claim entries, privileged hostPath entries, mount paths, first-claim
  compatibility) and `strip_dataset_values`.
- Unit-test the chart-support scanner with a temp chart tree containing and
  omitting `.Values.datasets`.
- Extend the Playwright install test: create/register two datasets, install the
  first catalog app with `datasets: [a, b]`, expect the job to succeed and
  `GET /api/apps/{name}` to report both; clean both up.
- Live drill (not CI): `kubectl -n <app-namespace> get pvc` shows one claim per
  dataset and `kubectl exec ... -- mount`/`ls /datasets/<name>` shows both.

### 3. UI

Files: `ui/src/lib/components/AppInstallModal.svelte`,
`ui/src/lib/components/AppConfigureModal.svelte`,
`ui/src/lib/components/InstalledApps.svelte`.

- **Install modal:** replace the single `selectedDataset` with
  `selectedDatasets: string[]` and a checkbox list (`bind:group`). Keep the
  host path and shared warnings; show one warning per selected shared dataset.
  Disable a private dataset already attached to another app (show
  `attached to <app>`). Review shows `name → /datasets/name`. POST
  `datasets: selectedDatasets`.
- **Configure modal:** fetch `/api/datasets/registry` alongside the catalog and
  record; initialise from `installed.datasets ?? []`; render the same checkbox
  list; PUT `{ values, datasets: selectedDatasets }`. For legacy records (empty
  `namespace`) show a note that datasets require a reinstall.
- **Installed list:** add `datasets?: string[]` to `AppView` and show the
  attached dataset names/count on the card.

### 4. Chart quota

Files: `charts/naslos/values.yaml`, `charts/naslos/templates/app-isolation.yaml`.

- Add `apps.isolation.pvcQuota` (default `12`) and use it for both the baseline
  and privileged `ResourceQuota` `persistentvolumeclaims` values (currently
  hard-coded to `4`; several charts already use 2–3 PVCs before dataset
  claims).

### 5. Catalog charts

Files: `NaslosCharts/README.md` and the 12 charts that consume datasets:
`audiobookshelf`, `bazarr`, `calibre-web`, `cleanuparr`, `gamevault`,
`gameyfin`, `jellyfin`, `radarr`, `sonarr` (baseline; `datasetsClaim`) and
`jackett`, `prowlarr`, `qbittorrent` (privileged; `hostPath`).

- Add `datasets: []` to each values file with a comment.
- In each template, replace the single dataset volume/mount with a branch:

```yaml
# volumeMounts
{{- if .Values.datasets }}
{{- range .Values.datasets }}
- { name: ds-{{ .name }}, mountPath: {{ .mountPath }} }
{{- end }}
{{- else }}
- { name: data, mountPath: /data }      # keep the chart's existing path/mount name
{{- end }}

# volumes
{{- if .Values.datasets }}
{{- range .Values.datasets }}
- name: ds-{{ .name }}
  {{- if .claim }}
  persistentVolumeClaim:
    claimName: {{ .claim }}
  {{- else }}
  hostPath:
    path: {{ .hostPath | quote }}
    type: Directory
  {{- end }}
{{- end }}
{{- else }}
# existing fallback: datasetsClaim (baseline) or hostPath (privileged)
{{- end }}
```

- Keep each chart's existing legacy mount path in the `else` branch
  (`/data`, `/audiobooks`, `/downloads`, etc.) so zero-dataset installs are
  unchanged.
- Update `NaslosCharts/README.md` with the new contract, the fallback rule and
  the fact that attached datasets appear at `/datasets/<name>`.
- Remove the now-stale "mounts it via `datasetsClaim`" guidance from the
  multi-dataset section.

### 6. Docs and spec

- `docs/api.md`: `POST /api/apps` accepts any number of `datasets`;
  `PUT /api/apps/{name}` accepts `datasets` (omitted = unchanged, `[]` = detach
  all); document the `datasets` Helm value and mount path.
- `docs/app-catalog.md`: replace "a claim per dataset" wording with the new
  container contract; document reconfigure and auto-migration.
- `docs/datasets-isolation.md`: update phase 2 (multiple claims, `/datasets/<name>`),
  the catalog compatibility section (charts now read `datasets`, old path needs
  an in-app update), and the migration notes.
- `docs/spec.md` FR-APP-20: state that an app MAY select multiple registered
  datasets, each mounted at `/datasets/<name>`, and that the API injects the
  authoritative list. Update the linked test row to cover multiple datasets.
- `AI_Handoff.md`: update only when the change is actually deployed (image tags,
  helm revision, migration result).

## Failure handling and safety

- Install/reconfigure rollback only removes resources created by the current
  operation; pre-existing claims/attachments are never detached on failure.
- Removed datasets are detached/deleted only after the Helm upgrade succeeds.
- A claim-name collision with another app/dataset fails closed.
- Private datasets still accept one app; shared datasets still require
  `warningAcknowledged` and show the per-dataset warning in the UI.
- Legacy records (no namespace) cannot gain datasets without a reinstall; the
  API returns a clear error and the UI says so.
- Auto-migration never runs for orphaned apps or charts without
  `.Values.datasets`; it is idempotent via `datasetsLayout`.

## Risks

- **In-app paths change.** After migration every attached dataset is at
  `/datasets/<name>`, so existing library/download paths inside the app must be
  updated. Data itself does not move (same hostPath). This is the accepted
  trade-off of the chosen contract.
- **Release order.** Merge/deploy the NaslosCharts change and refresh the
  catalog before deploying the API, otherwise the startup migration skips until
  the charts are visible (or the API is restarted).
- **Startup work.** Auto-migration performs sequential Helm upgrades; it runs in
  the background after startup, but a large number of apps will take time and
  restart their pods.
- **Quota.** The per-namespace PVC quota must be raised before multiple claims
  are created.

## Validation

```bash
cd rust && cargo fmt --check && cargo clippy -p naslos-api --all-targets -- -D warnings && cargo test -p naslos-api
cd ui && npm run check && npm run build
cd .. && helm lint charts/naslos -f charts/naslos/values.yaml -f charts/naslos/values-vm.yaml
helm lint charts/naslos -f charts/naslos/values.yaml -f charts/naslos/values-installer.yaml
sh scripts/audit.sh
cd ../NaslosCharts && helm lint apps/sonarr apps/audiobookshelf apps/qbittorrent
```

- `helm template` each updated chart with two `datasets` entries and assert both
  `/datasets/<name>` mounts and the correct `claim`/`hostPath` source; with no
  `datasets`, assert the legacy mount is unchanged.
- Playwright: `cd ui && ./node_modules/.bin/playwright test tests/apps.spec.ts`.
- Live drill: install/reconfigure an app with two datasets, verify both mounts
  inside the pod, then remove one and verify its PV/PVC and registry attachment
  are gone while the other keeps working.

## Out of scope

- Read-only dataset attachments and per-dataset mount-path overrides.
- Catalog-declared dataset slots.
- Moving data between datasets.
- Releasing a new version or deploying to the production node (follow the repo
  release/deploy process when requested).
