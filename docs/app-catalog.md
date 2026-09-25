# App Catalog & Apps

Apps are discovered from **git-based chart repositories**. The API clones each
repository with a pure-Go git client (`go-git`) into a cache on the state volume
and installs charts from the **local clone path** — the distroless image has no
`git` or `helm` binary.

## Repository contract

```
<repo-root>/
  naslos-repo.yaml            # optional: channel -> branch mapping, display name
  apps/
    jellyfin/
      Chart.yaml              # the folder IS the chart
      values.yaml
      templates/...
      naslos-app.yaml         # install-config (metadata + schema + defaults + services + exposure)
```

`naslos-repo.yaml` (optional, at the repository root) names the repo and maps
channels to branches; without it the default `Prod`/`Develop`/`Experimental`
mapping is used. A channel whose branch does not exist is simply not offered.

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
```

## Repositories (sources)

Each source is `{name, url, auth, credentialsSecret, channels, official}`:

| `auth` | Credential |
| --- | --- |
| `public` | none |
| `token` | HTTPS token in a Secret (`token` key) |
| `ssh` | deploy key in a Secret (`ssh-private-key` key) |

- The **official** source is seeded from `apps.officialSource` in the chart
  (`SOURCES_OFFICIAL_URL`); it cannot be recreated through the API as official.
- Admin-added sources are created via `POST /api/sources`. A source's channel
  map defaults to `Prod`/`Develop`/`Experimental` → same-named branches.
- On a name collision, a **user source overrides the official source**.
- Clones live in `CHARTS_CACHE_DIR` (`/var/lib/naslos/charts`) with one working
  tree per `(source, channel)`. A refresh uses `CHARTS_TTL` (default `15m`);
  when the remote is unreachable the stale clone is served and `lastError` is
  recorded. `POST /api/sources/refresh` forces a refresh.
- The clone guard rejects path traversal and symlinks that escape the clone,
  and refuses repositories beyond the size/file-count limits.

## Install / manage lifecycle

Apps install into the `naslos-apps` namespace (PSA `baseline`), separate from
the platform (`naslos`) and the privileged workloads (`naslos-privileged`).

| Action | API | Behavior |
| --- | --- | --- |
| List catalog | `GET /api/catalog` | Source/channel-tagged summaries |
| Catalog detail | `GET /api/catalog/{name}` | Full entry incl. `schema`, `services`, `exposure` |
| Install | `POST /api/apps {name, values, exposure, confirmed}` | `confirmed` MUST be `true`; defaults merged, chart loaded from the clone |
| List installed | `GET /api/apps` | Records + live status, orphan flag, URL |
| Detail | `GET /api/apps/{name}` | Record + status |
| Reconfigure | `PUT /api/apps/{name} {values}` | `helm upgrade` with the full value set |
| Exposure | `GET`/`PUT /api/apps/{name}/exposure` | Orthogonal toggles, re-renders the route |
| Uninstall | `DELETE /api/apps/{name}` | `helm uninstall` + route delete + record delete |

Installed-app records are persisted in `APPS_CONFIG` (`/var/lib/naslos/apps.json`).
A Helm release found in the platform namespace without a catalog match is
**backfilled as orphaned** (shown, but not reconfigurable).

## Exposure → routing

The API owns one `IngressRoute` per app plus the middlewares it references, all
in `naslos-apps`, and converges them on startup.

| Setting | Effect |
| --- | --- |
| `subdomain` | `Host(<subdomain>.<baseDomain>)`; empty ⇒ no route (cluster-internal only) |
| `tls` | on ⇒ `websecure` + TLS Secret; off ⇒ `web` (plain HTTP), no redirect |
| `auth` | on ⇒ Authelia forwardAuth middleware; only allowed on an SSO domain |
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

A domain record is
`{baseDomain, dnsProvider, credentialsSecret, acmeEmail, environment, primary}`.
For each non-primary domain the API renders an ACME DNS-01 `Issuer` and a
wildcard `Certificate` (`dnsNames: [<domain>, "*.<domain>"]`) into `naslos-apps`.
Providers: `cloudflare` (`api-token` key), `rfc2136` (`tsig-secret` key) or
`passthrough` (a raw cert-manager solver). The SSL page is gated on the
cert-manager CRDs; `make crds` installs them.

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

## Operational notes

- **Wildcard DNS is a prerequisite** for app subdomains: point `*.<domain>` at
  the node (router/dnsmasq/registrar). Without it, app hostnames do not resolve.
- **TLS apps need a certificate Secret** in `naslos-apps` (the wildcard
  Certificate's `secretName`, default `naslos-apps-tls`). A `.local` domain can
  never get a public cert — keep TLS off or self-sign for `.local`.
- **Third-party charts are privileged.** Any install runs arbitrary in-cluster
  manifests under the API's namespaced Role. Mitigations: dedicated namespace,
  PSA `baseline`, NetworkPolicy, no proxy-secret/Talos access, admin-only access
  and explicit confirmation. Commit pinning is deferred.
- SSH host-key pinning (a `known_hosts` Secret) is a known follow-up; the
  current SSH transport is configured by the appliance admin.
