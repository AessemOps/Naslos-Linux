# Dynamic DNS & DNS providers

Naslos keeps one or more A/AAAA records pointed at the appliance's **public WAN
IP**. The detection model follows
[qdm12/ddns-updater](https://github.com/qdm12/ddns-updater): the API fetches the
public IP, **resolves each record over DNS**, and calls the provider only when
the resolved addresses do not contain the public IP. That detects manual edits
and avoids provider rate limits. The same declarative registry also drives the
cert-manager DNS-01 solver used by [Domains & SSL](app-catalog.md#domains--certificates),
so a provider is defined once.

## Detection

Each interval (`ddns.intervalSeconds`, default 300) the reconciler:

1. Fetches the public IP, trying `ddns.ipSources` / `ddns.ipv6Sources` in order.
   A source is an HTTP(S) URL or a DNS fetcher (`dns:opendns`, `dns:google`).
2. For each enabled record, resolves its name (A/AAAA) and **skips the update
   when the public IP is already among the answers**. A resolution failure
   updates rather than silently skipping.
3. Applies a per-record **cooldown** (`ddns.updateCooldownSeconds`, default 300)
   after each successful update.
4. Proxied records (Cloudflare `proxied: true`) cannot be checked by DNS — the
   lookup returns the proxy's address — so they compare against the stored
   `lastIP` instead.

`POST /api/ddns/{id}/run` forces one reconcile, bypassing both the DNS pre-check
and the cooldown.

## Provider registry

A provider is a YAML document with metadata, credential fields, an optional
cert-manager solver and an optional DDNS driver:

```yaml
name: ovh                       # required, DNS-1123 label, unique
displayName: OVH
description: OVH DNS
icon: "🟦"
fields:                         # drives the UI form; secret fields go to a Secret
  - key: endpoint
    label: API endpoint
    type: enum                  # string | bool | enum
    default: ovh-eu
    enum: [ovh-eu, ovh-ca, ovh-us]
  - key: applicationKey
    label: Application key
    type: string
    required: true
  - key: applicationSecret
    label: Application secret
    type: string
    required: true
    secret: true
  - key: consumerKey
    label: Consumer key
    type: string
    required: true
    secret: true
certManager:                    # optional: how a Domain builds its DNS-01 solver
  solver:
    ovh:
      endpoint: "${cred.endpoint}"
      applicationKey: "${cred.applicationKey}"
      applicationSecretSecretRef: { name: "${secret}", key: applicationSecret }
      consumerKeySecretRef: { name: "${secret}", key: consumerKey }
ddns:                           # optional: DDNS support
  driver: ovh                   # ovh | cloudflare | http
  defaults: { endpoint: ovh-eu }
```

- `${secret}` is replaced with the entry's / domain's Secret name.
- `${cred.<key>}` is replaced with the non-secret provider config value (or the
  field's default).
- `passthrough` has `certManager.passthrough: true` and no solver: the raw
  solver comes from the domain record itself.
- A secret field may set `secretKey:` to write under a different Secret key
  (Cloudflare's solver expects `api-token`).
- A field may set `scope: cert` or `scope: ddns` to appear only in the Domains
  form or the Dynamic DNS form (OVH's DynHost `mode`/`username`/`password` are
  `ddns`-scoped so they do not clutter the certificate form).
- A field may set `showIf: { key: mode, value: api }` to appear only when
  another field (in the same scope) has that value — OVH's ZoneDNS fields show
  only for `mode: api`.

### Where definitions come from

1. **Embedded defaults** (`api/internal/providers/builtin/`): `ovh`,
   `cloudflare`, `generic` (DDNS), `rfc2136`, `passthrough` (certificates).
2. **An override directory** (`DDNS_PROVIDERS_DIR`, set by `ddns.providersDir`).
   Files are read in name order; a file whose `name` matches a built-in replaces
   it, a new name adds a provider.

An invalid or unreadable override is **skipped**, logged, and reported by
`GET /api/providers` (`errors`); it never stops the API.

## Entries

`GET /api/ddns` returns `{entries, enabled, intervalSeconds}`. An entry is
`{id, provider, zone, record, recordType, ttl, enabled, providerConfig,
credentialsSecret, credentialFields, lastIP, lastStatus, lastError, lastRunAt,
nextRunAt}`.

| Action | API |
| --- | --- |
| List / create | `GET`/`POST /api/ddns` |
| Read / update / delete | `GET`/`PUT`/`DELETE /api/ddns/{id}` |
| Force a run | `POST /api/ddns/{id}/run` |

The create/update body carries a `fields` map with every provider field. Secret
fields are written to a Secret named `naslos-ddns-<id>` in the apps namespace
(`naslos-apps`); the response returns the Secret name and which fields are set —
**never their values**. An omitted or empty secret field on update keeps the
stored value.

- `record` is the subdomain label (`@` or empty for the apex).
- `recordType` is `A` or `AAAA`.
- `ttl` is `0` (provider default) or 60–86400.

## Credential model

- Credentials live in Kubernetes Secrets in the **apps namespace**, where the
  API already has namespaced Secret CRUD for Helm releases.
- No Secret access is added in the release namespace, so the proxy secret, LDAP
  bind password and Authelia keys remain unreadable by the API (see the
  `naslos-api-platform-read` Role and its assertion in `scripts/audit.sh`).
- Domains use the same model: secret fields submitted through the Domains form
  are written to `naslos-domain-<domain>-creds` (or an existing
  `credentialsSecret`) and never returned.

## Drivers

| Driver | Protocol |
| --- | --- |
| `ovh` | Signed OVH API: `GET/PUT` `/1.0/domain/zone/{zone}/record` + `POST …/refresh`; `$1$` + SHA1 signature |
| `cloudflare` | `GET /zones?name=…` → `GET/PUT/POST …/dns_records`; `Authorization: Bearer <apiToken>`; honours `proxied` |
| `digitalocean` | `GET …/records?name=…` then `PUT …/records/{id}`; Bearer token |
| `godaddy` | `PUT /v1/domains/{zone}/records/{type}/{owner}`; `Authorization: sso-key key:secret` |
| `porkbun` | `POST …/retrieveByNameType/…` then `…/create/…` or `…/edit/{id}`; apikey/secretapikey |
| `http` | Templated generic request (see below) |

### Built-in DDNS providers

The embedded `builtin/` definitions ported from ddns-updater (a provider using
one of the drivers above is pure YAML — no Go change):

| Provider | Driver | Notes |
| --- | --- | --- |
| `cloudflare` | `cloudflare` | API token; optional `proxied` |
| `ovh` | `ovh` | DynHost (`mode: dynamic`, username/password — default) or ZoneDNS API (`mode: api`, endpoint + application/consumer keys) |
| `duckdns` | `http` | token; `DUCKDNS` subdomain label |
| `dynu` | `http` | username/password (+ optional location) |
| `noip` | `http` | username/password (Basic) |
| `freedns` | `http` | per-record update token |
| `namecheap` | `http` | DDNS password (IPv4 only) |
| `desec` | `http` | token (Basic, hostname:token) |
| `spdyn` | `http` | token or username/password |
| `selfhostde` | `http` | username/password (Basic; 204 = no change) |
| `dynv6` | `http` | token |
| `digitalocean` | `digitalocean` | token; record must already exist |
| `godaddy` | `godaddy` | API key + secret |
| `porkbun` | `porkbun` | API key + secret API key |
| `generic` | `http` | fully custom template |

### `http` driver

Fields: `updateUrl` (required), `method`, `contentType`, `body`,
`authType` (`none|basic|bearer|header|query`), `username`, `headerName`,
`successStatus` (a code or comma list, e.g. `200,204`),
`successContains`, `successAny`, `errorAny`; secret fields `token`, `password`,
`headerValue`.

Templates use `{{.zone}}`, `{{.record}}`, `{{.type}}`, `{{.ip}}`, `{{.ttl}}`,
`{{.secret.<key>}}`, `{{.config.<key>}}`, plus the helpers `{{fqdn .record
.zone}}` (full name) and `{{label .record .zone}}` (label, empty at the apex),
and Go template control flow (`{{if eq .type "AAAA"}}…{{end}}`). Example:

```yaml
updateUrl: "https://api.example.com/update?host={{fqdn .record .zone}}&ip={{.ip}}"
authType: bearer
token: <secret>
successAny: "good,nochg"
errorAny: "badauth,notfqdn"
```

The driver refuses a target that is not a public address (loopback, private,
link-local or cluster service ranges) — an admin-supplied URL cannot be used as
an SSRF pivot.

## Configuration (chart)

| Value | Default | Meaning |
| --- | --- | --- |
| `ddns.enabled` | `true` | Wire the reconcile loop |
| `ddns.stateFile` | `/var/lib/naslos/ddns.json` | Entry store (shares-config PVC) |
| `ddns.providersDir` | `/etc/naslos/ddns-providers` | Override directory |
| `ddns.providersConfigMap` | `""` | ConfigMap mounted at `providersDir` |
| `ddns.ipSources` | ipify, icanhazip, ifconfig.io, `dns:google` | Public IPv4 sources, tried in order |
| `ddns.ipv6Sources` | api6.ipify, ipv6.icanhazip, `dns:google` | Public IPv6 sources |
| `ddns.intervalSeconds` | `300` | Reconcile interval |
| `ddns.updateCooldownSeconds` | `300` | Minimum time between successful updates of one record |
| `networkPolicy.ddnsEgress` | `false` | Allow outbound HTTP/HTTPS + DNS for DDNS |
| `networkPolicy.ddnsEgressCIDRs` | `[]` → `0.0.0.0/0` | Narrow the DDNS egress |

API environment variables: `DDNS_ENABLED`, `DDNS_CONFIG`, `DDNS_PROVIDERS_DIR`,
`DDNS_IP_SOURCES`, `DDNS_IPV6_SOURCES`, `DDNS_INTERVAL_SECONDS`,
`DDNS_UPDATE_COOLDOWN_SECONDS` (the legacy single `DDNS_IP_SOURCE` /
`DDNS_IPV6_SOURCE` still work as fallbacks).

## Operations

- **Egress:** without `networkPolicy.ddnsEgress` (or another rule covering
  HTTP/HTTPS and DNS 53) the reconciler records a detection error instead of
  silently doing nothing. On the VM profile `ddnsEgress: true`.
- **`.local` domains** can never hold a public certificate or a public DNS
  record; DDNS applies only to real public zones.
- **OVH certificates** need the OVH cert-manager webhook installed and
  registered with cert-manager (the `ovh` solver is a webhook solver); OVH
  **DDNS** updates call the OVH API directly and need no webhook.
- **Force a run:** `POST /api/ddns/{id}/run` updates even when the IP is
  unchanged; a failure returns `502` with the entry's `lastError`.
- **Adding a provider:** drop a `*.yaml` file in the override directory (or a
  ConfigMap key) and restart — no code change for a built-in solver or driver.

*Portions of the provider logic and detection model are ported from
[qdm12/ddns-updater](https://github.com/qdm12/ddns-updater) (MIT).*
