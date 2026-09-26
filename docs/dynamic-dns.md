# Dynamic DNS & DNS providers

Naslos keeps one or more A/AAAA records pointed at the appliance's **public WAN
IP**. The API detects the IP on an interval, compares it with the last known
value, and calls the provider only when it changed. The same declarative
registry also drives the cert-manager DNS-01 solver used by
[Domains & SSL](app-catalog.md#domains--certificates), so a provider is defined
once.

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
| `cloudflare` | `GET /zones?name=…` → `GET/PUT/POST …/dns_records`; `Authorization: Bearer <apiToken>` |
| `http` | Templated generic request (see below) |

### `http` driver

Fields: `updateUrl` (required), `method`, `contentType`, `body`,
`authType` (`none|basic|bearer|header|query`), `username`, `headerName`,
`successStatus` (default `200`), `successContains`; secret fields `token`,
`password`, `headerValue`.

Templates use `{{.zone}}`, `{{.record}}`, `{{.type}}`, `{{.ip}}`, `{{.ttl}}`,
`{{.secret.<key>}}` and `{{.config.<key>}}`. Example:

```yaml
updateUrl: "https://api.example.com/update?host={{.record}}.{{.zone}}&ip={{.ip}}"
authType: bearer
token: <secret>
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
| `ddns.ipSource` | `https://api.ipify.org` | Public IPv4 source |
| `ddns.ipv6Source` | `https://api6.ipify.org` | Public IPv6 source |
| `ddns.intervalSeconds` | `300` | Reconcile interval |
| `networkPolicy.ddnsEgress` | `false` | Allow outbound HTTP/HTTPS for DDNS |
| `networkPolicy.ddnsEgressCIDRs` | `[]` → `0.0.0.0/0` | Narrow the DDNS egress |

API environment variables: `DDNS_ENABLED`, `DDNS_CONFIG`, `DDNS_PROVIDERS_DIR`,
`DDNS_IP_SOURCE`, `DDNS_IPV6_SOURCE`, `DDNS_INTERVAL_SECONDS`.

## Operations

- **Egress:** without `networkPolicy.ddnsEgress` (or another rule covering
  80/443) the reconciler records a detection error instead of silently doing
  nothing. On the VM profile `ddnsEgress: true`.
- **`.local` domains** can never hold a public certificate or a public DNS
  record; DDNS applies only to real public zones.
- **OVH certificates** need the OVH cert-manager webhook installed and
  registered with cert-manager (the `ovh` solver is a webhook solver); OVH
  **DDNS** updates call the OVH API directly and need no webhook.
- **Force a run:** `POST /api/ddns/{id}/run` updates even when the IP is
  unchanged; a failure returns `502` with the entry's `lastError`.
- **Adding a provider:** drop a `*.yaml` file in the override directory (or a
  ConfigMap key) and restart — no code change for a built-in solver or driver.
