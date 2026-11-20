# App updates when an upstream image is released

## Problem

Installed catalog apps are updated only when the catalog chart's `version`
changes. The official charts ship floating image tags (`latest`, `stable`,
`release`) with `imagePullPolicy: IfNotPresent`, so an upstream image release is
invisible to the Installed tab and a plain Helm upgrade with unchanged values
does not re-pull the image. Every installed app can therefore run a stale image
indefinitely.

## Design

1. **Detect** — a new `images` module resolves the digest the registry currently
   serves for a chart image's tag (anonymous Docker Registry v2, including the
   Bearer-token challenge) and compares it with the digest the running pod
   pulled (`status.containerStatuses[].imageID`). Multi-arch indexes resolve to
   the `linux/amd64` manifest the node uses. Pinned (`repo@sha256:…`) images are
   left to the chart-version path.
2. **Cache** — digest lookups cache for `IMAGE_CHECK_TTL` (default 1h), and the
   per-app drift snapshot is refreshed in the background on the existing
   `CHARTS_TTL` catalog cycle, so `/api/apps` and `/api/apps/updates` stay
   network-free.
3. **Expose** — `GET /api/apps` adds `imageUpdateAvailable`, `imageCurrent` and
   `imageLatest`; `updateAvailable` keeps its chart-version meaning.
   `/api/apps/updates` includes a cached image-only update (`imageUpdate: true`)
   so the tab badge and sidebar dot see it.
4. **Apply** — after a successful Helm upgrade with a known drift the API
   strategic-merges `imagePullPolicy: Always` plus a `naslos.io/image-refresh`
   pod annotation onto only the drifted containers of the release
   Deployments/StatefulSets, waits for the bounded rollout, and pulls the new
   digest. A failed pull/rollout is recorded as `lastError` and the drift is
   re-offered on the next cycle instead of failing an already-applied release.
5. **UI** — rows, drawer and both update modals treat either update kind as
   updateable; image-only updates read "new image" rather than
   `v0.1.6 → v0.1.6`. Update all includes them.
6. **RBAC/egress** — the isolation ClusterRole gains workload read/patch, and
   the chart's workload egress gains a port-443 registry allow
   (`networkPolicy.imageEgress`, on by default).

## Touched

- `rust/api/src/images.rs` (new: reference parsing, registry resolution, cache)
- `rust/api/src/apps.rs` (drift snapshot, view fields, summary, force re-pull)
- `rust/api/src/kube.rs` (strategic merge patch)
- `rust/api/src/state.rs`, `rust/api/src/main.rs` (background refresh)
- `rust/api/src/server/apps.rs` (refresh on Check now)
- `charts/naslos/templates/{app-isolation,networkpolicy}.yaml`, `values.yaml`
- `ui/src/lib/appTypes.ts`, `InstalledApps.svelte`, `InstalledAppRow.svelte`,
  `AppDetailDrawer.svelte`, `AppUpdateModal.svelte`, `AppUpdateAllModal.svelte`
- `docs/spec.md` (FR-APP-22 wording + FR-APP-27), `docs/app-catalog.md`,
  `README.md`, `AI_Handoff.md`

## Verification

- `cd rust && cargo fmt --all --check && cargo clippy -p naslos-api --all-targets -- -D warnings && cargo test -p naslos-api`
- `cd ui && npm run check && npm run build`
- `helm lint charts/naslos -f charts/naslos/values.yaml -f charts/naslos/values-vm.yaml`
- `helm lint charts/naslos -f charts/naslos/values.yaml -f charts/naslos/values-installer.yaml`
- Live drill (after release): at least one app shows "new image", the update job
  re-pulls it, and the drift clears on the next catalog cycle.
