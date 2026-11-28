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
    secret: true
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
  solver:                       # a webhook solver (OVH is not built into cert-manager)
    webhook:
      groupName: ovh.naslos.local
      solverName: ovh
      config:
        endpoint: "${cred.endpoint}"
        authenticationMethod: application
        applicationKeyRef: { name: "${secret}", key: applicationKey }
        applicationSecretRef: { name: "${secret}", key: applicationSecret }
        applicationConsumerKeyRef: { name: "${secret}", key: consumerKey }
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
- A provider may set `apiRights:` — a list of API permissions the operator must
  grant for its credentials. It is informational and shown in the Domains form's
  info bubble next to the "DNS-01 provider" selector; it does not affect solver
  rendering or validation.

### Generic HTTP templates

The `http` driver's `updateUrl` and `body` are mini templates: `{{fqdn .record
.zone}}`, `{{label .record .zone}}`, `{{.zone}}`, `{{.record}}`, `{{.type}}`,
`{{.ip}}`, `{{.ttl}}`, `{{.secret.KEY}}` and `{{.config.KEY}}`. Conditional
blocks are supported and are what the built-in providers use to select the
IPv4/IPv6 endpoint or parameter:

```yaml
updateUrl: >-
  https://dns.example/update?host={{if label .record .zone}}{{label .record .zone}}{{else}}@{{end}}&addr={{if eq .type "AAAA"}}v6={{.ip}}{{else}}v4={{.ip}}{{end}}
```

`{{if expr}}...{{else}}...{{end}}` accepts `eq A B`, `ne A B`, or a single
expression; truthy means non-empty (and not the string `false`). A missing
`.secret.KEY`/`.config.KEY` inside a condition is empty, so optional
credentials can be probed. A missing key in an output expression is an error.
Nested blocks and `{{else if ...}}` are not supported.

Every built-in `http` provider is tested to render both A (IPv4) and AAAA
(IPv6) URLs.

### Plaintext HTTP

A credential over plaintext `http` is refused. The generic provider exposes a
boolean `allowInsecureHttp` field; setting it to `true` is an explicit
acknowledgement, logged to the audit log (`ddns` /
`insecure_http_acknowledged`) and warned at send time. HTTPS remains the only
credential-safe transport.

### Where definitions come from

1. **Embedded defaults** (`rust/api/src/providers/builtin/`): `ovh`,
   `cloudflare`, `generic` (DDNS), `rfc2136`, `passthrough` (certificates).
2. **An override directory** (`DDNS_PROVIDERS_DIR`, set by `ddns.providersDir`).
   Files are read in name order; a file whose `name` matches a built-in replaces
   it, a new name adds a provider.

An invalid or unreadable override is **skipped**, logged, and reported by
`GET /api/providers` (`errors`); it never stops the API.

## Entries

`GET /api/ddns` returns `{entries, enabled, intervalSeconds}`. An entry is
`{id, provider, zone, record, recordType, ttl, enabled, providerConfig,
redactedProviderConfigKeys, credentialsSecret, credentialFields, lastIP,
lastStatus, lastError, lastRunAt, nextRunAt}`. `providerConfig` is projected
through the export-safe filter: credential-like keys (for example a legacy
`applicationKey`) and free-form templates (`updateUrl`, `body`, or any value
that embeds userinfo/a token) are omitted, and their names are listed in
`redactedProviderConfigKeys`. Those fields are write-only: send them in
`fields` to set them, omit them to keep them, or send an empty string to clear
them.

| Action | API |
| --- | --- |
| List / create | `GET`/`POST /api/ddns` |
| Read / update / delete | `GET`/`PUT`/`DELETE /api/ddns/{id}` |
| Force a run | `POST /api/ddns/{id}/run` |

The create/update body carries a `fields` map with every provider field. Secret
fields are written to a Secret named `naslos-ddns-<id>` in the **key-custody
namespace** (`datasetEncryption.keysNamespace`, default `naslos-keys`); the
response returns the Secret name and which fields are set — **never their
values**. An omitted secret field on update keeps the stored value; a secret
field submitted explicitly **empty** is deleted from the Secret, so clearing a
credential revokes it. This also removes a legacy credential key that older
entries stored in `providerConfig`.

- `record` is the subdomain label (`@` or empty for the apex).
- `recordType` is `A` or `AAAA`.
- `ttl` is `0` (provider default) or 60–86400.

## Credential model

- Credentials live in Kubernetes Secrets in the **key-custody namespace**
  (`datasetEncryption.keysNamespace`, default `naslos-keys`) — the same
  workload-free namespace that holds dataset keys and cloud-destination
  credentials. User apps and arbitrary Helm charts run in `naslos-apps` (and
  the Helm identity has `get`/`list` on Secrets there), so a Secret beside them
  can be read by `lookup` or mounted by any chart. DDNS credentials are only
  ever read by the API itself, so they must not live there (FR-DNS-04).
- Naslos writes `naslos-ddns-<id>` with
  `app.kubernetes.io/managed-by=naslos-api` and
  `naslos.io/ddns-entry=<id>`. Deleting an entry removes the Secret only when
  it is one Naslos created (the managed label or the generated name); an
  operator-provisioned Secret is left in place. An operator-supplied
  `credentialsSecret` must be a DNS-1123 name starting with `naslos-` **and
  already exist in the key-custody namespace**; it is written in place without
  the managed labels, so it is never deleted with the entry.
- No new RBAC is needed: the `naslos-dataset-keys` Role already grants the API
  Secret `get/create/update/patch/delete` in that namespace (no `list`/`watch`,
  which matches reading by name).
- No Secret access is added in the release namespace, so the proxy secret, LDAP
  bind password and Authelia keys remain unreadable by the API (see the
  `naslos-api-platform-read` Role and its assertion in `scripts/audit.sh`).
- App DNS records that point at a DDNS entry use the same state: a rename
  creates/updates the new CNAME first and deletes the old owned record
  afterwards; an adopted record is never owned and is never deleted.
- Domains are deliberately **not** moved: secret fields submitted through the
  Domains form are written to `naslos-domain-<domain>-creds` (or an existing
  `credentialsSecret`) in the apps namespace, because the cert-manager Issuer
  rendered for the domain references that Secret by name, so it has to be
  readable by cert-manager from the same namespace. Values are never returned.
  An explicit `credentialsSecret` must be a DNS-1123 name starting with
  `naslos-`; generated names always are, and the prefix keeps a request from
  targeting another app's Secret (SEC-2026-D12). See the
  [upgrade note](#upgrading-from-an-apps-namespace-secret) for the DDNS move.
- A domain update that omits the write-only `solver`, `providerConfig` or
  `credentialsSecret` preserves the stored values and validates the effective
  record. `solver: null` (or `{}`) clears a provider-rendered solver; a
  passthrough domain still requires a solver and returns 400.

### Upgrading from an apps-namespace Secret

Earlier releases wrote `naslos-ddns-<id>` into `naslos-apps`. On startup (and
before the first credential read) the API migrates each entry: when the Secret
exists only in the apps namespace it is copied into the key-custody namespace
(create-only, same keys), the copy is verified, and only then is the
apps-namespace copy deleted — and only when Naslos created it (the managed
label or the generated name). A Secret you provisioned yourself is copied but
left in place. The migration is idempotent, logs entry ids and names but never
values, and a keys-namespace copy always wins, so a re-run or a partial
failure is safe. Check the result with
`kubectl -n naslos-keys get secret -l naslos.io/ddns-entry`. Nothing is
required from the operator, and no values need re-entering.

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
one of the drivers above is pure YAML — no code change):

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
link-local, CGNAT, benchmarking, reserved or cluster service ranges) — an
admin-supplied URL cannot be used as an SSRF pivot. The host is resolved once,
every answer is checked, and the request connects to the vetted address
(`reqwest::ClientBuilder::resolve`), so a DNS rebind between the check and the
connection cannot redirect it (`AUD-2611-T7-2`).

## App records (FR-DNS-09)

A base domain can opt into automatic CNAMEs for exposed apps by naming a DDNS
entry in `appRecordsDdnsEntry`. Only `ovh` (ZoneDNS API keys) and `cloudflare`
(`api-token`) domains qualify, and the entry must not be a wildcard. Every
installed app with a non-empty `subdomain` on that domain then gets
`<subdomain>.<baseDomain>` → the DDNS entry's hostname (`record` `@`/empty uses
the zone itself).

| Aspect | Behavior |
| --- | --- |
| Credentials | The domain's cert-manager Secret in `naslos-apps`: OVH `applicationKey`/`applicationSecret`/`consumerKey` + `providerConfig.endpoint`, Cloudflare `api-token` |
| Zone discovery | The API walks `baseDomain` upward (`media.example.com` → `example.com`) and uses the first zone the provider serves; positive results are cached. OVH is probed with `GET /domain/zone/<zone>/record?subDomain=` (200 = served), because the cert-manager app key is scoped to `/domain/zone/<zone>/*` and the zone root answers 403 even for an owned zone |
| OVH writes | CNAME target absolute (`host.`), TTL 0 (zone default), then `POST /domain/zone/<zone>/refresh`. Record ids come from the list endpoint as bare numbers; the API fetches each record for its type/target |
| Cloudflare writes | `proxied:false`, TTL auto (`1`), content without a trailing dot |
| Ownership | Only records Naslos created (or adopted because an existing CNAME already had the exact target) are updated or deleted. Any other record at the name is left alone and the app records `dns.status: error` |
| Changes | Reconcile runs after install/exposure changes, on startup, when the entry or domain changes, on `POST /api/apps/{name}/dns/reconcile`, and on the catalog-refresh cadence while in error. A changed subdomain/base domain deletes the old record before creating the new one |
| Cleanup | Uninstall, exposure removal and domain removal delete owned records; a held record is reported as an error and retried rather than silently abandoned |

`GET /api/apps/{name}` and `GET /api/apps/{name}/exposure` return `dns`:
`{fqdn, target, provider, zone, recordId, owned, status, error, updatedAt,
baseDomain}`. `GET /api/domains` marks each domain with `appRecordsSupported`,
and `DELETE /api/ddns/{id}` is a **409** while a domain references the entry.

TLS is unaffected: the domain's wildcard certificate already covers
`*.<baseDomain>`, and the `_acme-challenge` TXT records cert-manager writes are
unrelated to app CNAMEs.

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
- **OVH certificates** need cert-manager **and** the OVH cert-manager webhook
  installed (OVH is not a cert-manager built-in DNS-01 solver). Naslos uses
  [aureq/cert-manager-webhook-ovh](https://github.com/aureq/cert-manager-webhook-ovh)
  with group name `ovh.naslos.local`; `make install-vm` installs both into the
  `cert-manager` namespace and grants the webhook read access to the domain
  credential Secrets in `naslos-apps`. The provider stores every OVH credential
  (application key, secret, consumer key) in that Secret; OVH **DDNS** updates
  call the OVH API directly and need no webhook.
- **Force a run:** `POST /api/ddns/{id}/run` updates even when the IP is
  unchanged; a failure returns `502` with the entry's `lastError`.
- **Adding a provider:** drop a `*.yaml` file in the override directory (or a
  ConfigMap key) and restart — no code change for a built-in solver or driver.

*Portions of the provider configurations, the detection model and the
`digitalocean`/`godaddy`/`porkbun` drivers are ported from
[qdm12/ddns-updater](https://github.com/qdm12/ddns-updater) (MIT); see
[CREDITS.md](../CREDITS.md).*
