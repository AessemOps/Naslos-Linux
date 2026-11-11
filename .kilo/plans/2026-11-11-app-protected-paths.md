# Per-path Authelia protection for public apps (SearXNG settings)

## Problem

An app is either fully public (`auth: false`) or fully behind Authelia
(`auth: true`). SearXNG needs both: searches and static assets must stay open,
while `/preferences`, `/config` and `/stats` should require an Authelia session.
The Preferences link should also be hidden until the browser has an
`authelia_session` cookie, instead of sending anonymous visitors to a login page.

## Design

- Add `exposure.protectedPaths` to the catalog defaults, the installed `Exposure`
  record and `ExposurePatch`. The list is only meaningful when `auth` is false.
- `routing::render` emits two routes for a public app with protected paths:
  - a priority-100 route matching
    `Host(...) && (PathPrefix('/preferences') || ...)` with the security-headers
    and forwardAuth middlewares;
  - the existing priority-1 catch-all `Host(...)` route with only the security
    headers, so search and static assets stay public.
  - `auth: true` still protects the whole host and ignores `protectedPaths`.
- Validate path prefixes (`/` prefix, conservative alphabet) because they are
  interpolated into a Traefik rule.
- SearXNG chart: an init container copies the image's templates to `/templates`,
  wraps the `linkto_preferences` block in
  `{% if request.cookies.get('authelia_session') %}`, and sets
  `ui.templates_path: /templates`. Upstream template updates are picked up on
  every pod start because the copy happens at startup.
- The chart's `naslos-app.yaml` sets `auth: false` and
  `protectedPaths: [/preferences, /config, /stats]`. The install UI keeps the
  field because the exposure object is round-tripped; no new control is added.

## Changes

- `rust/api/src/catalog.rs`: `ExposureDefaults.protectedPaths`.
- `rust/api/src/apps.rs`: `Exposure.protectedPaths`, `ExposurePatch` merge,
  install/exposure validation.
- `rust/api/src/routing.rs`: `Spec.protectedPaths`, protected route rendering,
  `validate_protected_paths`, tests.
- `docs/api.md`: exposure endpoint and install payload.
- `NaslosCharts/apps/searxng/naslos-app.yaml`: public search, protected settings.
- `NaslosCharts/apps/searxng/templates/app.yaml` +
  `files/patch-templates.py`: guarded Preferences link.
- `NaslosCharts/README.md`: document `protectedPaths`.

## Verification

- `cargo fmt -p naslos-api`, `cargo clippy -p naslos-api --all-targets -- -D warnings`,
  `cargo test -p naslos-api`.
- `helm lint apps/searxng`, `helm template searxng apps/searxng`.
- Playwright against `https://search.shail.ovh`:
  - a search without a session succeeds and has no Preferences link;
  - `/preferences` redirects to the `shail.ovh` Authelia portal;
  - after login the `authelia_session` cookie is present, `/preferences` returns
    200 and the Preferences link is visible.

## Rollout

- PRs in `Naslos-Linux` and `NaslosCharts`; CI builds the images (no local
  registry).
- After the API image is released, `PUT /api/apps/searxng/exposure` with
  `{"exposure":{"protectedPaths":["/preferences","/config","/stats"]}}`, then
  upgrade the SearXNG chart.
- Existing installs need that exposure patch: catalog defaults do not rewrite an
  installed record.
