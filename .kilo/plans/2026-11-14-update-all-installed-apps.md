# Update all installed apps

## Problem

The Installed tab exposes one update button per app. When several apps have a
newer chart, the owner has to open and confirm each update one by one.

## Design

Add a header action to the Installed tab:

- Show **Update all (N)** when at least one listed app has
  `updateAvailable` and no lifecycle job is running.
- Open a confirmation modal that lists every target with
  `installed → latest`, repeats the source/channel switch warning for affected
  apps, and preserves the existing "values and datasets are kept" contract.
- On confirm, snapshot the target list, then enqueue one existing update path
  per app: `PUT /api/apps/{name}` with `{}`. Jobs are tracked by the shared
  `appJobs` drawer/poller. No new API endpoint and no change to FR-APP-22's
  upgrade action.
- Report the number of started updates and any enqueue failures (for example a
  `409` when a job started after the modal opened) in the Installed header.
- Apps already running a job are omitted when the action opens.

## Touched

- `ui/src/lib/components/AppUpdateAllModal.svelte` (new)
- `ui/src/lib/components/InstalledApps.svelte`
- `ui/tests/apps.spec.ts`
- `docs/app-catalog.md` (Installed tab behavior)
- `README.md` (feature list)

## Verification

- `cd ui && npm run check && npm run build`
- Playwright mock test: mocked list with two updateable apps, click
  **Update all (2)**, confirm, assert one `PUT /api/apps/<name>` per app and the
  started-count message.
- No Rust/chart changes, so no `cargo`/`helm` gates are required.
