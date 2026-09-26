# Dynamic DNS + declarative DNS providers

## Goal

Add a **Dynamic DNS (DDNS)** feature to Naslos that keeps one or more A/AAAA
records pointed at the appliance's current public IP, and make **DNS providers
declarative**: a provider is described by a YAML file (metadata, credential
fields, a cert-manager DNS-01 solver, and a DDNS driver), so adding a provider is
a config change, not a code change. Ship built-in drivers `ovh`, `cloudflare`
and `http` (generic), and use the same registry to **add OVH as a provider for
domains/certificates** (replacing the hardcoded `certs.solverFor` switch).

## Branch / base

PR #28 is merged into `origin/master` (`b2e816c`, merge 2026-09-26); the local
`master` branch (`8ac3036`) is stale. Branch from the **remote** master:

```bash
git fetch origin
git checkout -b feature/ddns-dynamic-dns origin/master
```

(If `origin/master` has moved, re-check `git log -1 --format='%H %P' origin/master`
that the PR #28 merge is an ancestor before starting.)

## Decisions (locked)

1. **Hybrid provider registry.** A provider registry is loaded from embedded
   YAML defaults plus an optional mounted override directory. The YAML declares
   metadata, credential fields, the cert-manager DNS-01 solver (with
   `${secret}` / `${cred.<key>}` substitution), and the DDNS driver + defaults.
   Adding a cert-manager provider (any solver cert-manager supports) or a DDNS
   provider that uses the `http` driver is **YAML-only**; only a genuinely new
   DDNS protocol needs a new Go driver.
2. **DDNS scope.** Maintain A/AAAA records for the appliance's **public WAN IP**,
   detected from a configurable public-IP URL on an interval; update the provider
   only when the IP changes. (No static/arbitrary-IP mode.)
3. **Credentials = API-managed Kubernetes Secret.** Secret fields submitted
   through the UI are written to a Secret `naslos-ddns-<id>` in the **apps
   namespace** (`naslos-apps`), where the API already has namespaced Secret CRUD
   (`charts/naslos/templates/rbac-apps.yaml`). GET never returns credential
   values — only the Secret name and which fields are set. Non-secret fields live
   in the entry's `providerConfig`. No secret access is added in the release
   namespace, so `naslos-proxy` / LDAP / Authelia keys stay unreadable by the API.
4. **Domains adopt the same registry.** The Domains form/provider list and the
   cert-manager solver are driven by the registry; OVH is supported. Existing
   `cloudflare`, `rfc2136`, `passthrough` behavior is preserved (as built-in
   providers), and an explicit `credentialsSecret` on a domain record still works.
5. **UI**: a new **Dynamic DNS** page + sidebar entry; the Domains form fetches
   provider definitions instead of a hardcoded `<option>` list.

## Architecture

```
api/internal/providers/     NEW  registry: embedded + override YAML, validation,
                                 cert-manager solver rendering, field model
api/internal/ddns/          NEW  entry store, public-IP detection, reconciler,
                                 drivers: ovh, cloudflare, http; secret writer
api/internal/certs/         CHG  solverFor/Validate -> providers.Registry lookup;
                                 Domain gains ProviderConfig
api/internal/server/        CHG  GET /api/providers; /api/ddns* routes; wiring
ui/src/routes/dns/          NEW  Dynamic DNS page
ui/src/lib/components/      CHG  DdnsForm.svelte (new), DomainForm.svelte (chg)
charts/naslos/              CHG  ddns values, env, ConfigMap mount, egress
docs/                       CHG  dynamic-dns.md (new) + api/app-catalog/README/
                                 architecture/spec/handoff
```

Provider registry (shared by DDNS and domains):

```
api/internal/providers/
  providers.go            # types, Load(embeddedFS, overrideDir), Registry
  providers_test.go
  builtin/ovh.yaml
  builtin/cloudflare.yaml
  builtin/generic.yaml
  builtin/rfc2136.yaml    # cert-manager only (preserve current behavior)
  builtin/passthrough.yaml# cert-manager only (raw solver)
```

### Provider YAML schema

```yaml
name: ovh                       # required, DNS-1123 label, unique
displayName: OVH
description: OVH DNS (ovh-eu / ovh-ca / ovh-us)
icon: "🟦"
fields:                         # credential fields → drives the UI form
  - key: endpoint
    label: API endpoint
    type: string                # string | bool | enum
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
    secret: true                # → Secret; else → entry providerConfig
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
  defaults: { endpoint: ovh-eu } # driver defaults merged into providerConfig
```

- `cloudflare.yaml`: field `apiToken` (secret); `certManager.solver.cloudflare.apiTokenSecretRef`;
  `ddns.driver: cloudflare`.
- `generic.yaml`: `ddns.driver: http`, no cert-manager solver. Fields (all
  non-secret unless noted): `updateUrl` (template), `method`, `contentType`,
  `body` (template), `authType` (none|basic|bearer|header|query),
  `username`, `headerName`; secret fields `token`/`password`/`headerValue`;
  `successStatus`, `successContains`. Templates use `{{.zone}}`, `{{.record}}`,
  `{{.type}}`, `{{.ip}}`, `{{.ttl}}` and `{{.secret.<key>}}` /
  `{{.config.<key>}}`.
- `rfc2136.yaml` / `passthrough.yaml`: cert-manager only, preserving today's
  `solverFor` output exactly (see `certs.go:171-188`). `passthrough` has no
  fields and is only valid with an inline `solver` on the Domain.
- Override merge: the mounted dir (`ddns.providersDir`) is read after the
  embedded FS; a file whose `name` matches a built-in replaces it, a new `name`
  adds a provider. Invalid files are skipped with a logged error and surfaced in
  `GET /api/providers` (never fatal).

### Driver interface (`api/internal/ddns`)

```go
type UpdateRequest struct {
    Zone, Record, RecordType, IP string
    TTL int
    Config map[string]string   // providerConfig
    Secret map[string]string   // resolved secret fields
}
type Driver interface { Update(ctx context.Context, r UpdateRequest) error }
```

- `ovh`: signed `GET/PUT https://{endpoint}/1.0/domain/zone/{zone}/record` +
  `POST .../refresh`; signature `$1$` + SHA1(AS+CK+METHOD+URL+body+ts).
- `cloudflare`: `GET /zones?name={zone}` → `GET .../dns_records?type&name` →
  `PUT`/`POST .../dns_records`; `Authorization: Bearer <apiToken>`.
- `http`: render the URL/body templates, apply `authType`, send, assert
  `successStatus` / `successContains`.
- Shared `*http.Client` with a timeout and a `naslos-api/<version>` User-Agent.
  Drivers take an injectable base URL / client so tests use `httptest`.

## Ordered tasks

1. **Spec + docs first (repo conformance rule).** Add a spec section
   `### 3.9 Dynamic DNS & DNS providers (FR-DNS)` in `docs/spec.md` with:
   FR-DNS-01 providers MUST be YAML-declared (built-in + override dir) and a
   provider using a built-in driver or cert-manager solver MUST need no Go
   change; FR-DNS-02 entry CRUD MUST NOT return credential values; FR-DNS-03 a
   background reconciler MUST detect the public IP on an interval and update only
   on change, recording last status/error; FR-DNS-04 credentials MUST come from
   Kubernetes Secrets and MUST NOT be created where the API can read
   proxy/LDAP/Authelia secrets; FR-DNS-05 cert-manager solvers MUST be rendered
   from the registry and OVH MUST be supported; FR-DNS-06 a force-run endpoint
   MUST exist. Cite the new tests in each. Update `docs/app-catalog.md` §Domains
   to point at the registry. Add `docs/dynamic-dns.md` (provider YAML reference,
   how to add a provider, credential Secret model, egress). Update
   `docs/api.md` (routes + env vars), `docs/architecture.md` (outbound
   connections: IP-detect + provider APIs), `docs/README.md` index. Update
   `AI_Handoff.md` state at the end.
2. **`api/internal/providers` + tests.** Types, `go:embed builtin/*.yaml`,
   `Load`, validation (unique DNS-1123 name, field keys unique, enum default in
   enum, driver known), override merge, `Solver(secretName, config)` rendering
   with `${secret}`/`${cred.*}` substitution. Tests: parse both built-ins, an
   override replacing `ovh`, an invalid file skipped, OVH solver JSON shape.
3. **`api/internal/ddns` + tests.** Entry model + JSON store (model on
   `buddyScheduleStore`: atomic write, 0600). Public-IP detection
   (`net.ParseIP` validation; `IPSource`/`IPv6Source`). Drivers `ovh`,
   `cloudflare`, `http` with injectable client/base URLs. `Manager.Reconcile`
   (startup + ticker `DDNS_INTERVAL_SECONDS`, default 300) that skips an
   unchanged IP, records `lastIP/lastStatus/lastError/lastRunAt/nextRunAt`, and
   a `Run(id, force)` path for the force endpoint. Secret writer
   (`writeSecret` create-or-update in `apps.namespace`). Tests: each driver
   against `httptest` (request URL/method/body/signature), skip-when-unchanged,
   store round-trip, IP parse reject, secret/value separation in the GET view.
4. **Refactor `api/internal/certs` to the registry.** Replace the `switch` in
   `Validate` (`certs.go:69-92`) and `solverFor` (`certs.go:160-188`) with a
   registry lookup + `Provider.Solver(...)`. Keep `passthrough` and `rfc2136`
   output byte-identical. Add `ProviderConfig map[string]string` to `Domain`
   (non-secret provider fields) and, when secret fields are submitted, have the
   server write the Secret and set `CredentialsSecret`. Keep `Spec`/`Store`
   behavior; add `TestSpecOVH` and a passthrough regression test.
5. **Server wiring + routes.** Register `GET /api/providers` and the DDNS routes
   on the **owner** mux in `server.go/routes()` (mirror `domains.go`):
   `GET/POST /api/ddns`, `GET/PUT/DELETE /api/ddns/{id}`,
   `POST /api/ddns/{id}/run`, `GET /api/providers`. Add config in
   `setupChartRepos`/`New`: load the registry (`DDNS_PROVIDERS_DIR`), build the
   `ddns.Manager` with the cached `kubernetesClient()` and `appsNamespace`, start
   the reconciler from `Start()` and stop it in `Shutdown()`. Validate every
   body (size limit, provider exists, record DNS-1123 label, zone, type A/AAAA,
   TTL range). Add ddns test env vars to `newTestServer` (`routes_test.go:14`).
   Extend `ownerPaths` (`routes_test.go:35`) with `/api/providers`, `/api/ddns`,
   `/api/ddns/x`, `/api/ddns/x/run`. Add contract tests (unknown provider 400,
   unknown entry 404, GET hides secret values).
6. **Chart.** `values.yaml`: `ddns:` block (`enabled`, `stateFile:
   /var/lib/naslos/ddns.json`, `providersDir: /etc/naslos/ddns-providers`,
   `providersConfigMap: ""`, `ipSource: https://api.ipify.org`,
   `ipv6Source: https://api6.ipify.org`, `intervalSeconds: 300`). Extend
   `values.schema.json` (`ddns` object, `additionalProperties: true`).
   `api-deployment.yaml`: env `DDNS_CONFIG`, `DDNS_PROVIDERS_DIR`,
   `DDNS_IP_SOURCE`, `DDNS_IPV6_SOURCE`, `DDNS_INTERVAL_SECONDS`; mount
   `providersConfigMap` (keys → `*.yaml`) when set; state file lands on the
   existing `shares-config` PVC. `networkpolicy.yaml`: add `ddnsEgress` /
   `ddnsEgressCIDRs` (default `0.0.0.0/0`, TCP 443 and 80) in **both** the
   release and privileged-namespace egress blocks. `values-vm.yaml`:
   `ddns.enabled: true`, `networkPolicy.ddnsEgress: true`. No new RBAC (apps-NS
   Secret CRUD already granted) — add a comment saying so. `scripts/audit.sh`:
   assert `DDNS_CONFIG` and `DDNS_PROVIDERS_DIR` env are wired, and that the new
   `ddnsEgress` ports are 80/443 only.
7. **UI.** New `ui/src/routes/dns/+page.svelte` (page "Dynamic DNS"): list
   entries with provider/zone/record/type/lastIP/lastStatus/lastError and add
   `{ name: 'Dynamic DNS', path: '/dns', icon: '🌍' }` to `Sidebar.svelte`.
   New `DdnsForm.svelte`: provider `<select>` from `GET /api/providers`, fields
   rendered from the provider's `fields` (secret fields as password inputs),
   record/type/TTL/enabled, "Update now" action. Refactor
   `DomainForm.svelte:66-70` to fetch `/api/providers` and render the selected
   provider's `fields` (keep the passthrough JSON textarea). `npm run check`
   must be clean.
8. **E2E.** New `ui/tests/ddns.spec.ts`: page loads, provider list is
   non-empty, the add form opens and the provider switch changes the rendered
   fields; skip cleanly when `/api/ddns` reports disabled. Keep the existing
   `domains.spec.ts` green (the malformed-domain PUT test at line 34 must still
   return 400).
9. **Cutover / live drill.** Bump image tags (`api` → next `0.1.0-rNN`, `ui` →
   next `rNN`) in `values.yaml` and `values-vm.yaml` (tags are immutable with
   `IfNotPresent`). `helm lint`; deploy with `make install-vm`; create a DDNS
   entry against a real zone (Cloudflare token or OVH keys in a Secret created
   by the API), verify the A record changes, and request a Let's Encrypt
   **staging** cert for an OVH-managed domain to prove the OVH solver. Run
   `sh scripts/audit.sh`, both Go suites with `-race`, `svelte-check`, and the
   Playwright suite.

## Risks / notes

- **`http` driver is operator-configured egress.** Treat the URL/body templates
  as admin-only input; validate the scheme is http/https and reject a request
  that resolves to the cluster/service CIDRs (basic SSRF guard).
- **OVH wildcard certs** need the OVH secret keys (`applicationSecret`,
  `consumerKey`) and the plain `applicationKey`/`endpoint`; cert-manager reads
  the Secret from `naslos-apps`, which is where the API writes it — verify the
  cert-manager `Issuer` namespace matches.
- **Public-IP detection** requires 443 egress to the IP source; without
  `ddnsEgress` (or `gitEgress`) the reconciler records a detection error rather
  than silently doing nothing.
- **`.local` domains** can never get a public cert; DDNS/OVH applies only to
  real public zones.
- **Registry load is best-effort at startup**; a bad override must not stop the
  API from serving, and must be visible via `GET /api/providers`.
- Do not read Secrets in the release namespace: the audit's platform-read Role
  deliberately excludes them (`scripts/audit.sh:255-263`); keep it that way.

## Validation checklist

- `cd api && go build ./... && go vet ./... && go test -race ./...`
- `cd agent && go build ./... && go vet ./... && go test -race ./...` (unchanged)
- `cd ui && npm run check`
- `helm lint charts/naslos -f charts/naslos/values.yaml`
- `sh scripts/audit.sh`
- Live: DDNS record updates on an IP change; force-run; OVH staging cert issues;
  `GET /api/ddns` never returns a token; Playwright `ddns.spec.ts` + `domains.spec.ts`.
