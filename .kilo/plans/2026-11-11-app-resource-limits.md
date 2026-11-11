# Editable app resource limits in the Installed drawer

## Problem

The Installed-app detail drawer shows live CPU/RAM usage but the owner cannot
change the chart's container resources from the UI. Tuning a memory-starved app
meant editing the catalog chart or running `helm upgrade` by hand, which the
install-path rule discourages.

## Design

Add a **Resources** section to `AppDetailDrawer.svelte`:

- The drawer reads the app's recorded `values.resources` (already returned by
  `GET /api/apps` through the flattened record).
- Two shapes are supported, matching the catalog charts:
  - flat `resources.requests.{cpu,memory}` / `resources.limits.{cpu,memory}`
    (most charts), and
  - per-component `resources.<component>.requests|limits.{cpu,memory}` (e.g.
    `affine`, `calagopus`, `adventurelog`, `forgejo-runner`).
- "Adjust" exposes CPU/RAM request and limit fields per group. Saving sends
  `PUT /api/apps/{name}` with `{values:{resources}}`, which merges into the
  stored values and runs the normal upgrade job (the app restarts). Existing
  keys the form does not edit (e.g. `ephemeral-storage`) are preserved; blank
  fields are omitted.
- Orphaned records and running lifecycle jobs disable the editor.

The API validates the CPU/memory quantities in `resources` on both install and
upgrade (before Helm) so a typo such as `100mm` is a clear error instead of an
opaque `--wait` timeout and rollback.

## Touched

- `rust/api/src/apps.rs` (`validate_resource_quantities` + tests, called from
  `install` and `upgrade`)
- `ui/src/lib/appTypes.ts` (`values` on `AppView`)
- `ui/src/lib/components/AppDetailDrawer.svelte`
- `docs/spec.md` (FR-APP-25 + test map), `docs/api.md`, `docs/app-catalog.md`

## Verification

- `cargo fmt --check`, `cargo clippy -p naslos-api`, `cargo test -p naslos-api`
- `cd ui && npm run check && npm run build`
- Live: adjust an app's memory limit in the drawer, confirm the upgrade job
  succeeds and the Deployment carries the new limit.
