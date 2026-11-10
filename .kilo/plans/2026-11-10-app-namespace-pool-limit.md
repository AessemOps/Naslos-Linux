# Runtime app-namespace pool limit

## Problem

The per-app namespace pool was a fixed chart value (`apps.isolation.namespaces`,
last set to 12). Installing a ninth baseline app failed with `no free per-app
namespace is available; raise apps.isolation.namespaces`, and the only fix was
a platform upgrade. The 0.3.17 updater then reset the value to the pack default,
dropping the live fix.

## Design

The chart provisions more namespaces than are usable by default, and the owner
sizes the usable limit from the UI:

- `apps.isolation.namespaces` stays the **provisioned** pool (default 32).
- New `apps.isolation.defaultLimit` (default 20) is the initial usable count,
  passed to the API as `APPS_NAMESPACE_LIMIT`.
- The API persists the owner's choice in `APPS_POOL_CONFIG`
  (`/var/lib/naslos/app-pool.json`, on the state volume) and hands out only the
  first `limit` namespaces of `APPS_NAMESPACE_POOL`.
- `GET /api/apps/namespaces` reports
  `{baseline:{provisioned, limit, used, available}, privileged:{...}}`;
  `PUT {limit}` validates `1..=provisioned` and `limit >= used`, persists, and
  applies immediately.
- The Installed tab shows `used/limit` with the provisioned count and an
  "Adjust limit" form (min = installed apps, max = provisioned).
- Records keep their namespace when the limit is lowered; `is_isolated` and the
  routing reconcile still consider the whole provisioned pool.

No new cluster privileges: namespace creation stays with the chart, so the
API never needs cluster-wide namespace/RBAC create rights (the pool design in
`docs/datasets-isolation.md` §3 is preserved).

## Touched

- `charts/naslos/values.yaml`, `values-vm.yaml`, `values.schema.json`
- `charts/naslos/templates/app-isolation.yaml`, `templates/api-deployment.yaml`
- `rust/api/src/apps.rs` (limit state, persistence, allocation, summary)
- `rust/api/src/state.rs`, `server/mod.rs`, `server/apps.rs`
- `ui/src/lib/appTypes.ts`, `ui/src/lib/components/InstalledApps.svelte`
- `docs/spec.md` (FR-APP-20), `docs/api.md`, `docs/app-catalog.md`,
  `docs/deployment.md`, `docs/datasets-isolation.md`

## Verification

- `cargo fmt/clippy/test -p naslos-api`, `npm run check`, `helm lint`,
  `helm template` renders 32 baseline + 4 privileged namespaces and the two env
  vars.
- Release `v0.3.18`; the node upgrade should re-provision the pool at 32 with a
  20-slot default and the existing apps untouched.
