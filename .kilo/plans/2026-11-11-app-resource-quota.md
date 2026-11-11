# Size app resources against the namespace quota

## Problem

The new drawer resource editor accepted a Jellyfin `limits.cpu: "8"`, but the
per-app namespace `ResourceQuota` (`naslos-app-quota`) still capped
`limits.cpu` at `4`. Helm could not create the pod
(`exceeded quota: naslos-app-quota, requested: limits.cpu=8, limited:
limits.cpu=4`), the Deployment never became ready, and the upgrade rolled back
after the 5-minute wait. The editor had no idea the quota existed.

## Design

- Make the baseline quota configurable in the chart
  (`apps.isolation.quota.requestsCpu|requestsMemory|limitsCpu|limitsMemory`)
  and raise `limitsCpu` to `8` (requests/memory keep their current values) so a
  normal media server can use more than 4 CPU limits.
- The API reads `resources` totals from the merged Helm values and checks them
  against the namespace quota **before** Helm on both install and upgrade,
  failing with a clear message (`limits.cpu 8 exceeds the namespace quota 4`)
  instead of an opaque wait timeout. The check fails open if the quota cannot
  be read, so a rolling API/chart upgrade is not blocked.
- Add `GET /api/apps/{name}/quota` (`{namespace, hard}`) and show the quota in
  the drawer's Resources section. The editor validates the draft totals and
  disables Save before the request ever reaches the API.
- The API's isolation ClusterRole gains read-only `resourcequotas` access.

## Touched

- `charts/naslos/values.yaml`, `values.schema.json`,
  `templates/app-isolation.yaml`
- `rust/api/src/apps.rs` (totals, quota check, quota read, tests),
  `server/apps.rs`, `server/mod.rs`
- `ui/src/lib/appTypes.ts`, `AppDetailDrawer.svelte`
- `docs/spec.md` (FR-APP-25 update), `docs/api.md`, `docs/app-catalog.md`,
  `docs/deployment.md`

## Verification

- `helm lint`/`helm template` shows the rendered quota at 8 CPU.
- `cargo fmt/clippy/test -p naslos-api` (quantity and totals tests, quota
  handler shape).
- `cd ui && npm run check && npm run build`.
- Live: after upgrading the platform, Jellyfin accepts `limits.cpu: "8"`;
  values above the quota are rejected before Helm with the quota name.
