# Plan — grant Internet egress before the app install wait

## Problem

Installing `bazarr` failed after the Helm `--wait` deadline:

```
Error: INSTALLATION FAILED: release bazarr failed, and has been uninstalled
due to rollback-on-failure being set: resource Deployment/naslos-app-8/bazarr
not ready. status: InProgress, message: Available: 0/1 context deadline exceeded
```

The pod pulled and ran, but its readiness and liveness probes kept failing
(`dial tcp <pod>:6767: connection refused`) until Helm rolled the release back.

## Root cause

The API installs apps into dedicated baseline namespaces
(`naslos-app-1..8`) whose `naslos-app-isolation` NetworkPolicy is default-deny
for egress (DNS + app namespaces only). The additive `<app>-internet` policy is
reconciled by `Router::apply` **after** a successful install (`apply_route`).

Bazarr always performs blocking public-network calls during startup
(`check_releases` → api.github.com, `get_announcements_to_file` →
cdn.jsdelivr.net, then raw.githubusercontent.com). With egress denied those
connections hang through every candidate address, so the web server never binds
6767 before the liveness probe kills it. The app can never become Ready, so the
install can never succeed, so the Internet policy is never applied — a
chicken-and-egg deadlock. This affects any internet-dependent app, not just
Bazarr.

Live evidence (2026-11-08, node `talos-zgr-9yg`):

- reproducing the install left one outbound socket in `SYN_SENT` to
  `151.101.x.x:443` (jsdelivr/Fastly) and no listener on 6767;
- applying the exact `<app>-internet` policy to `naslos-app-8` made the pod
  become `1/1 Ready` within one restart.

## Fix

Grant the app's public-egress policy **before** the Helm install wait when the
app installs into a dedicated baseline namespace and exposure has
`internet: true`. Shared (`naslos-apps`) and privileged namespaces keep the
current behavior: their policy is namespace-scoped and must not be created
before the app exists.

- `routing.rs`: extract `internet_policy_for(name, namespace)` and keep
  `internet_policy(spec)` as a wrapper; label API-created policies
  (`naslos.local/managed-by=naslos-api`, `naslos.local/app=<release>`).
- `apps.rs`: add `Router::ensure_internet` / `Router::delete_internet` (default
  no-ops, implemented by `KubeRouter`); call `ensure_internet` in `install`
  after storage provisioning and before `helm install`. On any failure delete
  only the pre-created policy (never the release's route), and roll back an
  install whose record cannot be persisted.
- `apps.rs`: sweep labeled policies that no record backs before a reserved
  namespace is reused and at startup (`reconcile_routes`), so an API restart
  during the Helm wait cannot leave a namespace-wide egress allow for the next
  app.
- `spec.md` FR-APP-11 and `docs/api.md`: record the pre-install policy, the
  failure cleanup and the reuse/startup sweep.

## Verification

- `cargo test -p naslos-api` (routing + new helper tests), `cargo clippy`.
- Rebuild/push the API image, bump `values-vm.yaml`, `make install-vm`.
- Install `bazarr` through the API on the node and confirm it becomes Ready
  without a manual NetworkPolicy.
- `helm lint`/`scripts/audit.sh` unaffected (no chart change).
