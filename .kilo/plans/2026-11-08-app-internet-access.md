# Per-app Internet access (default on)

## Problem

Apps installed into a baseline app namespace have no outbound Internet: the
namespace NetworkPolicy allows only cluster DNS and app-to-app egress. Many
catalog apps need the Internet (SearXNG queries search engines, indexers,
updaters, API clients), so they install successfully but do not work. The
operator has no per-app choice.

Goal: apps get outbound Internet **by default**, with an **Internet access**
toggle (default on) in the install and exposure forms. Turning it off keeps the
app on the existing DNS + app-to-app-only policy. The choice is per app, not a
namespace tier.

## Design

Each app already gets its own isolated namespace when
`apps.isolation.enabled` is on (the install default: `naslos-app-N`). Per-app
Internet access therefore maps to one extra namespace-scoped `NetworkPolicy`
owned by the API:

- `internet: true` (default) → apply `<app>-internet`, an additive Egress policy
  allowing `0.0.0.0/0` / `::/0` except cluster, LAN and link-local ranges, plus
  the base DNS and app-to-app allows (so it is self-sufficient when
  `networkPolicy.enabled=false`).
- `internet: false` → delete `<app>-internet`; the namespace's base policy
  (DNS + app namespaces) remains, so egress is denied.

The opt-out is only enforceable where the app owns its namespace and the base
policy does not already allow Internet. Therefore:
- Isolated baseline namespaces (`naslos-app-N`): on/off enforced.
- Privileged namespaces: their base policy always allows Internet, so
  `internet: false` is rejected (400) instead of silently ignored.
- Legacy shared `naslos-apps`: a namespace-wide policy cannot target one app,
  so `internet: false` is rejected there too.

The API applies the policy through the existing `KubeRouter` on install,
exposure update and startup reconcile (`reconcile_routes`), and deletes it on
uninstall. Policy and route convergence are independent, so one failure does
not skip the other.

## Changes

### Rust API

- `rust/api/src/apps.rs`
  - `Exposure`: add `internet: bool`, serde default `true`; manual `Default`
    so old records (no field) and `Exposure::default()` are Internet-on.
  - `ExposurePatch`: add `internet: Option<bool>` and merge it.
  - Install `exposure` becomes an `ExposurePatch`, so an omitted field keeps
    the catalog default instead of forcing a value.
  - `exposure_for`: apply only the supplied patch fields.
  - Reject `internet: false` outside an isolated baseline namespace.
- `rust/api/src/catalog.rs`
  - `ExposureDefaults`: add `internet: bool` (serde default `true`) and a
    manual `Default`, so a manifest without an `exposure` block defaults on.
- `rust/api/src/routing.rs`
  - `Spec`: add `internet`.
  - Render `<name>-internet` (Egress-only policy excluding private ranges, plus
    DNS and app-to-app allows).
  - `KubeRouter::apply`: apply/delete the policy independently of the route;
    collect both errors instead of aborting one on the other's failure.
  - `KubeRouter::delete`: also delete `<name>-internet`.
- Tests: exposure default/patch, catalog default, policy render, install patch.

### Chart

- `charts/naslos/templates/app-isolation.yaml`: add
  `networking.k8s.io/networkpolicies` to the `naslos-api-app-isolation`
  ClusterRole. Keep the privileged namespace's Internet allow (its opt-out is
  rejected, and leaving it avoids an upgrade window without egress).
- `charts/naslos/templates/rbac-apps.yaml`: add `networkpolicies` to the shared
  `naslos-api-apps` Role so the API can reconcile in legacy (non-isolated)
  installs.
- `scripts/audit.sh`: keep the `naslos-apps-priv-allow` metadata-range
  assertion (the allow stays); the Rust test covers the API-rendered policy.

### UI

- `ExposureForm.svelte`: `internet` field + "Internet access" checkbox
  (default on) with a short explanation.
- `AppInstallModal.svelte`: default `internet: true`, take the catalog default,
  show it in Review.
- `AppExposureModal.svelte` and `InstalledApps.svelte`: type updates.

### Docs

- `docs/spec.md` FR-APP-10: add `internet` and the default/opt-out MUST.
- `docs/app-catalog.md`: example manifest + routing table row.
- `docs/api.md`: exposure/install body description.

## Compatibility

- Existing records deserialize with `internet: true`; the startup reconcile
  applies the policy, so the installed SearXNG starts working after upgrade.
- Per-app enforcement relies on one app per namespace. With
  `apps.isolation.enabled=false` (legacy shared `naslos-apps`) the policy is
  namespace-wide; documented.

## Verification

- `cd rust && cargo fmt --check && cargo clippy -p naslos-api -- -D warnings &&
  cargo test -p naslos-api`
- `cd ui && npm run check && npm run build`
- `helm lint charts/naslos -f charts/naslos/values.yaml -f
  charts/naslos/values-installer.yaml`
- Live check after deploy: SearXNG search returns results; toggling Internet
  off removes `<app>-internet` and egress is denied.
