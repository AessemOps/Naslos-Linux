# App Catalog & Apps

Apps are discovered from **git-based chart repositories**. The API clones each
repository with a pure-Go git client (`go-git`) into a cache on the state volume
and installs charts from the **local clone path** — the distroless image has no
`git` or `helm` binary.

## Repository contract

```
<repo-root>/
  apps/
    jellyfin/
      Chart.yaml              # the folder IS the chart
      values.yaml
      templates/...
      naslos-app.yaml         # install-config (metadata + schema + defaults + services + exposure)
```

Channels map to git branches on the Naslos side: a source's channel map
(`apps.officialSource.channels`, or the `channels` field when adding a source)
maps a channel name to a branch in the repository. A repository with only a
`main` branch uses `{Prod: main}`. A channel pointing at a missing branch
reports an error for that source.

### `naslos-app.yaml`

```yaml
name: jellyfin                 # required, must equal the folder name (DNS-1123)
displayName: Jellyfin
description: Free media system
category: media
icon: "📺"
website: https://jellyfin.org
version: 10.9.0                # informational; Chart.yaml's version wins
tags: [media, streaming]
schema:                        # JSON Schema (object) driving the config form
  type: object
  properties:
    timezone: { type: string, title: Timezone, default: UTC }
  required: []
defaultValues:
  timezone: UTC
services:                      # what the subdomain routes to
  - name: "{{ .Release.Name }}"   # only the release name may be templated
    port: 8096
    scheme: http
exposure:                      # defaults shown in the install UI
  subdomain: jellyfin
  tls: true
  auth: true
  localOnly: false
# Optional. Install into the privileged apps namespace (PSA privileged) instead
# of naslos-apps (baseline). Required for a VPN sidecar that needs NET_ADMIN
# (e.g. gluetun in the Prowlarr/qBittorrent charts).
privileged: false
```

## Repositories (sources)

Each source is `{name, url, auth, credentialsSecret, channels, commit, official}`.
`commit`, when set, must be a full 40-character SHA: the clone is checked out at
exactly that commit and the sync fails if `HEAD` differs (SEC-2026-D6). The VM
and installer profiles pin the official `NaslosCharts` source to an audited
commit; bump `apps.officialSource.commit` when a release needs newer catalog
entries. Signature verification is still a follow-up.

| `auth` | Credential |
| --- | --- |
| `public` | none |
| `token` | HTTPS token in a Secret (`token` key) |
| `ssh` | deploy key in a Secret (`ssh-private-key` key) |

Allowed URL forms are `https://`, `ssh://` and `git@host:path`. `git://` is
refused: it is unauthenticated and unencrypted, so an on-path attacker could
serve an arbitrary chart to an install that runs with cluster identity. Other
transports (`file://`, `ext::`) are refused as well.

- The **official** source is seeded from `apps.officialSource` in the chart
  (`SOURCES_OFFICIAL_URL`); it cannot be recreated through the API as official.
- Admin-added sources are created via `POST /api/sources`. A source's channel
  map defaults to `Prod`/`Develop`/`Experimental` → same-named branches.
- On a name collision, a **user source overrides the official source**.
- Clones live in `CHARTS_CACHE_DIR` (`/var/lib/naslos/charts`) with one working
  tree per `(source, channel)`. A refresh uses `CHARTS_TTL` (default `15m`);
  when the remote is unreachable the stale clone is served and `lastError` is
  recorded. `POST /api/sources/refresh` forces a refresh.
- The clone guard rejects path traversal and refuses repositories beyond the
  size/file-count limits.
- **Charts may not contain links.** Git checks out with `core.symlinks=false`
  (a link becomes a plain file holding the target path), and before every
  install/upgrade the API walks the chart and refuses any symlink, hard link,
  device, fifo, file over 5 MiB, or tree over 5000 entries. This matters because
  the Helm loader reads chart files; a symlinked file would have let a chart
  render `/var/lib/naslos/buddy-identity.json` into a manifest.

## Install / manage lifecycle

Apps install into the `naslos-apps` namespace (PSA `baseline`), separate from
the platform (`naslos`) and the privileged workloads (`naslos-privileged`). An
app that declares `privileged: true` installs into `naslos-apps-priv` (PSA
`privileged`) instead, so only VPN-style apps get the relaxed profile.

| Action | API | Behavior |
| --- | --- | --- |
| List catalog | `GET /api/catalog` | Source/channel-tagged summaries |
| Catalog detail | `GET /api/catalog/{name}` | Full entry incl. `schema`, `services`, `exposure` |
| Install | `POST /api/apps {name, values, exposure, baseDomain, confirmed, privilegedConfirmed?, datasets?}` | `confirmed` MUST be `true`; a catalog entry with `privileged: true` also requires `privilegedConfirmed: true`, and the UI shows a second, red confirmation (SEC-2026-D8). Defaults are merged and the result is validated against the catalog's JSON schema before Helm runs; a violation is a 400 (SEC-2026-D7). `datasets` selects registered datasets; with `apps.isolation.enabled` the app gets its own namespace and a claim per dataset, and the chart receives `datasetsClaim`/`datasetsClaims` (baseline) or `hostPath` (privileged). Chart loaded from the clone. Enqueues an install job → `202 {jobId, state}` |
| List installed | `GET /api/apps` | Records + live status, orphan flag, URL |
| Detail | `GET /api/apps/{name}` | Record + status |
| Reconfigure | `PUT /api/apps/{name} {values}` | Enqueues a `helm upgrade` job with the full value set → `202 {jobId, state}` |
| Exposure | `GET`/`PUT /api/apps/{name}/exposure` | Orthogonal toggles + `baseDomain`, re-renders the route (synchronous) |
| Route targets | `GET /api/apps/{name}/services` | Services the release rendered (discovery fallback / UI picker) |
| Uninstall | `DELETE /api/apps/{name}` | Enqueues an uninstall job (`helm uninstall` + route delete + record delete) → `202 {jobId, state}` |
| Job list | `GET /api/apps/jobs` | Running plus the last ~20 finished lifecycle jobs |
| Job detail | `GET /api/apps/jobs/{id}` | `state` (`running`/`succeeded`/`failed`), `stage` (`preparing`/`installing`/`finalizing`), `message`, `error` |

Install/upgrade/uninstall run as **background jobs** (FR-APP-18): a cold
install's Helm `--wait` can take minutes, so the handler validates, enqueues and
answers `202` while the job runs under a server-owned context — a closed modal
or a navigated-away page no longer cancels it. The UI keeps a global job drawer
and a completion toast. A second job for an app already running one is `409`.
Jobs are in-memory: an API restart abandons a running job (records/routes
re-converge on boot via `reconcileApps`), and a partial Helm release may need a
manual retry.

If a manifest declares no `services[]`, the API discovers the release's
Services after install (matched by the Helm release annotation/label) and routes
to the first; `/api/apps/{name}/services` lists the candidates for the exposure
UI's picker.

Installed-app records are persisted in `APPS_CONFIG` (`/var/lib/naslos/apps.json`).
A Helm release found in the platform namespace without a catalog match is
**backfilled as orphaned** (shown, but not reconfigurable).

## Exposure → routing

The API owns one `IngressRoute` per app plus the middlewares it references, all
in `naslos-apps`, and converges them on startup.

| Setting | Effect |
| --- | --- |
| `baseDomain` | the domain the route hangs off; selectable among the configured domains (primary first), default the primary |
| `subdomain` | `Host(<subdomain>.<baseDomain>)`; empty ⇒ no route (cluster-internal only) |
| `tls` | on ⇒ `websecure` + TLS Secret; off ⇒ `web` (plain HTTP), no redirect |
| `auth` | on ⇒ Authelia forwardAuth middleware; only offered when the selected `baseDomain` is in the effective SSO list |
| `localOnly` | on ⇒ `IPAllowList` limited to `EXPOSURE_LOCAL_ONLY_CIDR` |

The API's `security-headers` middleware deliberately omits
`stsIncludeSubdomains`, so a TLS-off app subdomain is not HSTS-forced by the
main UI. Auth uses an Authelia forwardAuth middleware created in `naslos-apps`
that points at the Authelia Service FQDN (no cross-namespace Traefik reference).

## Domains & certificates

| Action | API |
| --- | --- |
| List / add domain | `GET`/`POST /api/domains` |
| Read / update / delete | `GET`/`PUT`/`DELETE /api/domains/{domain}` |
| Certificate status | `GET /api/domains/{domain}/certificate` |
| Promote / demote SSO | `POST /api/domains/{domain}/sso {enabled}` |
| Providers | `GET /api/providers` |

A domain record is
`{baseDomain, dnsProvider, credentialsSecret, providerConfig, acmeEmail,
environment, primary, sso}`. For each non-primary domain the API renders an ACME
DNS-01 `Issuer` and a wildcard `Certificate`
(`dnsNames: [<domain>, "*.<domain>"]`) into `naslos-apps`. The **provider** and
its solver come from the declarative registry
([dynamic-dns.md](dynamic-dns.md)): built-in `cloudflare` (`api-token` key),
`rfc2136` (`tsig-secret` key), `ovh` (the OVH cert-manager webhook solver —
endpoint in `providerConfig`, application key/secret and consumer key in the
Secret) and `passthrough` (a raw cert-manager solver on the record). The Domains
form lists
the registry's certificate-capable providers and renders each provider's fields;
secret fields are written to a Secret in `naslos-apps` and never returned. The
SSL page is gated on the cert-manager CRDs; `make crds` installs them.

### SSO promotion

The effective SSO list is the primary domain plus any registered domain whose
`sso` flag is set, unioned with the chart's `sso.domains` (`SSO_DOMAINS`) as a
floor. The chart entry cannot be demoted from the UI (edit values), but a
store-flagged domain can: `POST /api/domains/{domain}/sso {enabled}` sets the
flag and takes effect live. The API renders the session cookies and the
`access_control` rules into the `naslos-authelia-sso` ConfigMap (mounted at
`/config-sso`, injected by Authelia's `fileContent` template filter) and restarts
Authelia, which reads its configuration only at startup. The restart is a delete
of the deterministic `naslos-authelia-0` pod (the StatefulSet recreates it), so
the API's RBAC never includes workload write. Because Authelia's session cookie
is scoped per domain, a promoted domain also needs its own portal: the API
renders an IngressRoute per non-primary SSO domain in `naslos-apps` (where it
already has Service/IngressRoute rights) targeting an ExternalName Service that
aliases the Authelia Service in the release namespace, with the domain's wildcard
TLS Secret. Traefik needs `providers.kubernetesCRD.allowExternalNameServices`
(the chart sets it). The ConfigMap is
`helm.sh/resource-policy: keep` and the API reconciles it on startup, so a
promotion survives an API restart and a `helm upgrade`. Promoting the primary is
rejected (400 — it is always SSO), and demotion is refused (409, naming the apps)
while any installed app has `auth` on that domain.

## Schema-driven forms

`GET /api/catalog/{name}` returns `schema` (JSON Schema); the UI renders
`SchemaForm` from it:

| Schema feature | UI control |
| --- | --- |
| `type: string` | input |
| `format: password` / `email` | password / email input |
| `type: boolean` | toggle |
| `type: object` (nested) | nested section |
| `required`, `default` | marked / prefilled |

## Adding an app

1. Create `apps/<name>/` in the chart repository with `Chart.yaml`,
   `values.yaml`, templates and `naslos-app.yaml`.
2. Set `name` equal to the folder, declare `services` and `exposure` defaults.
3. Commit to the appropriate branch (`Prod` for stable) and refresh from the UI
   or `POST /api/sources/refresh`.
4. See [development.md](development.md#extending--add-a-catalog-app) for the
   local checklist.

## Storage for apps

Apps use `local-path-provisioner`, the only provisioner installed; the
`naslos-zfs` ZFS LocalPV storage class was removed (AUDIT-M13) — see
[storage-zfs.md](storage-zfs.md#storage-classes--app-data).

PSA `baseline` (the `naslos-apps` namespace) forbids `hostPath` volumes, but
Naslos datasets are host directories. The chart therefore publishes them as a
static RWX `naslos-datasets` PV/PVC (`apps.datasets`, backed by
`apps.datasets.hostPath`, default `/var/mnt`) that managed apps mount via a
normal `persistentVolumeClaim` volume. Privileged apps (`naslos-apps-priv`) may
mount the host path directly and share the same data.

## Operational notes

- **The cluster must be able to reach the git host.** A clone runs from the API
  pod, so enable `networkPolicy.gitEgress: true` (on in `values-vm.yaml`) to
  allow outbound git (HTTPS 443 / SSH 22); narrow
  `networkPolicy.gitEgressCIDRs` (default `0.0.0.0/0`) for a restricted network.
  A failed clone is surfaced as the source's `lastError`.
- **Wildcard DNS is a prerequisite** for app subdomains: point `*.<domain>` at
  the node (router/dnsmasq/registrar). Without it, app hostnames do not resolve.
- **TLS apps need a certificate Secret** in `naslos-apps`. The route references
  the base domain's wildcard Certificate `secretName` (from its domain record);
  `APPS_TLS_SECRET` is only the fallback when no domain record exists. A `.local`
  domain can never get a public cert — keep TLS off or self-sign for `.local`.
- **Third-party charts are privileged, but cannot own routing.** Helm runs as
  the reduced `naslos-helm` ServiceAccount (SEC-2026-D1/D4): it can create
  workloads, Services, ConfigMaps, Secrets, PVCs and Ingresses but **not**
  Traefik `IngressRoute`/`Middleware` objects, cert-manager objects, RBAC or
  cluster-scoped resources. The API creates and reconciles the app route and
  certificate itself. A chart that ships its own `IngressRoute` now fails
  (403) instead of silently becoming same-origin with the UI.
- Charts can still create Secrets/PVCs and mount the shared `naslos-datasets`
  PVC; per-app dataset isolation is the remaining open item (D3/F1). The threat
  model, options and migration plan are in
  [datasets-isolation.md](datasets-isolation.md).
- SSH chart sources require a pinned `known_hosts` (SEC-2026-B1).

## Install integrity

- **Namespace isolation is reserved, not raced.** The record used to be written
  only after Helm finished, so two concurrent installs of different apps both
  picked the same free `naslos-app-N`. Each install now reserves a namespace
  under the manager lock (records ∪ reservations) and releases it on success or
  failure; the record takes over once written.
- **Privileged consent is re-checked at run time.** `POST /api/apps` validates
  `privilegedConfirmed` against the catalog snapshot the operator saw, but the
  job re-resolves the (refreshable) catalog. `Manager::install` refuses a
  privileged app whose request did not carry the confirmation, so a catalog
  refresh between confirm and execution cannot silently escalate.
- **Upgrade validates values.** `PUT /api/apps/{name}` now runs the same
  `schema` validation as install before calling Helm.
- **Secret values are not returned.** Catalog fields marked
  `format: password` (or `writeOnly: true`) are masked as `null` in
  `GET /api/apps` and `GET /api/apps/{name}`, with the names listed in
  `redactedKeys`. Echoing `null` back on upgrade keeps the stored value. The
  values are still stored in `apps.json`; moving them to a Kubernetes Secret is
  tracked as D7.

## Exposure safety in the UI

The install wizard reflects the backend rules instead of silently installing an
unsafe exposure. A catalog entry with `exposure.auth: false` or
`exposure.tls: false` shows a red warning in review and requires a second,
explicit checkbox before Install enables; an entry with both off is preselected
as cluster-internal (`localOnly`). Password fields are masked in the review
step. Dismissing a running app job is disabled (it only hid the "already
running" conflict), and a job evicted from the server's list stops polling.
