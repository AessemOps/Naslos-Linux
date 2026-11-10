# Live app logs in the Installed drawer

## Problem

The Installed tab showed status, versions and live usage, but diagnosing a
broken app meant leaving the UI and running `kubectl logs`. The detail drawer
should show the app's logs live.

## Design

Reuse the owner-gated `/api/ws/logs` socket (tail 200 + follow), which already
streams `kubectl logs -f` from the API pod, and add a small discovery endpoint:

- `GET /api/apps/{name}/pods` lists pods and containers for the record's
  namespace. Isolated apps own their namespace; a legacy record in the shared
  `naslos-apps` namespace is filtered by `app.kubernetes.io/instance`.
  Running pods sort first and status container data (readiness, restarts) falls
  back to the spec while a pod is pending.
- The drawer's `AppLogs.svelte` loads that list, opens
  `/api/ws/logs?namespace&pod&container`, strips ANSI error codes, caps the
  buffer at ~200 KB, keeps a follow toggle (scroll-away pauses it) and reloads
  pods on demand. The component remounts per app (`{#key app.name}`), so
  switching apps closes the old socket.

No new privilege beyond `pods/log: get` in the `naslos-api-app-isolation`
ClusterRole (bound per pool namespace by RoleBinding); the shared apps Role
already had it. The audit script asserts the grant stays `get`-only.

## Touched

- `charts/naslos/templates/app-isolation.yaml` (pods/log get), `scripts/audit.sh`
- `rust/api/src/apps.rs` (`record_namespace`), `server/apps.rs`
  (`GET /api/apps/{name}/pods` + `pod_summary` tests), `server/mod.rs`
- `ui/src/lib/components/AppLogs.svelte`, `AppDetailDrawer.svelte`,
  `ui/src/lib/appTypes.ts`
- `docs/spec.md` (FR-APP-24), `docs/api.md`, `docs/app-catalog.md`

## Verification

- `cargo fmt/clippy/test -p naslos-api`, `npm run check`, `npm run build`,
  `helm lint`, `scripts/audit.sh`.
- Live: `kubectl auth can-i get pods --subresource=log --as
  system:serviceaccount:naslos:naslos-api -n naslos-app-1` turns from `no` to
  `yes`; the drawer streams logs. Release `v0.3.19`.
