# Catalog chart icons plan

Status: implemented (code + tests) · Release/deploy steps below still pending
Repos: `Naslos-Linux` only; `NaslosCharts` already ships `Chart.yaml` `icon`
metadata on `main` (`de27c85`).

## Problem

`NaslosCharts` charts now carry the standard Helm `icon` URL (33/33 charts),
but the catalog only read `naslos-app.yaml.icon` (short letter/emoji text) and
the UI rendered that text. The real chart logos never reached the browser.

## Fix

- API (`rust/api/src/catalog.rs`): read `Chart.yaml` once for `version` and
  `icon`; expose the icon URL as `iconUrl` on `/api/catalog` and
  `/api/catalog/{name}`. The manifest `icon` stays as the text fallback.
- UI: new `AppIcon.svelte` renders `iconUrl` as an `<img>` when it is an
  absolute `https` URL and falls back to the manifest text (or `📦`). Used by
  `CatalogBrowser` and `AppInstallModal`.
- Chart (`traefik-middleware.yaml`): allow `https:` in `img-src` so the remote
  icons load under the appliance CSP. No other content source opens up.
- Docs: `app-catalog.md` documents the precedence and `api.md` lists `iconUrl`.

## Tests

- `catalog::tests::chart_icon_is_exposed_for_the_card` covers the Chart.yaml
  icon and the list projection.
- Gates: `cargo fmt`/`clippy`/`test`, `npm run check`/`build`, `helm lint`,
  `scripts/audit.sh`.

## Deploy

- API and UI images need a new tag suffix; chart change ships in the next
  release pack. A catalog refresh is enough for newly merged `Chart.yaml`
  icons because the official source follows `main`.
