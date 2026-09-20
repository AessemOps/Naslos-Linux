# Charts repository + app install system refactor

Status: implementation-ready. Target agent: implementation-capable (edits Go/TS/Helm).
Scope: replace the hard-coded Go app catalog with a git-based chart-repository
system (official repo + user-added repos), a per-app install-config file, and a
per-app exposure UI (subdomain / TLS / auth / local-only) backed by an
API-owned Traefik routing layer and a cert-manager ACME DNS-01 certificate UI.

---

## 0. Decisions already made (do not re-litigate)

| Area | Decision |
| --- | --- |
| Chart repo ingestion | **Pure-Go `go-git`** clone into a PVC cache; install the chart from the **local path**. No `git`/`helm` binary (image is distroless). |
| App folder contract | Folder **is** the chart root (`Chart.yaml`, `values.yaml`, `templates/`) plus a sibling **`naslos-app.yaml`** install-config file. |
| Install-UI config | `naslos-app.yaml` holds display metadata, `schema` (JSON Schema), `defaultValues`, `services[]`, and exposure defaults. |
| Exposure settings | **Orthogonal toggles**: subdomain, TLS, auth, local-only. |
| Route target | Declared in `naslos-app.yaml` (`services[]`, release-name templated); fallback = discover Services from release labels and let the admin pick. |
| Routing ownership | **API owns** the `IngressRoute` + middlewares, generated from a persisted app record; reconciled on startup. |
| Repo auth | Public, **HTTPS token**, or **SSH deploy key**; credentials in Secrets. |
| Channels | Per-repo branch mapping (default `Prod`/`Develop`/`Experimental`); per-app channel; **user repos override** the official repo on name collision. |
| Catalog sync | PVC cache, TTL refresh + manual refresh + **stale fallback** when the remote is unreachable. |
| Namespace + RBAC | Apps install into a new **`naslos-apps`** namespace; API gets a namespaced Role there. |
| Builtins | **Replace** `builtin*.go`; backfill existing Helm releases into app records ("orphaned" when unmatched). |
| Domain model | **Multiple base domains**; one primary serves UI/Authelia. |
| Platform vs app domain | **Apps dynamic** (API-owned); the primary/platform domain serving UI + Authelia stays **Helm-owned**. |
| Multi-domain SSO | Chart declares an **SSO domain list**; Authelia gets a cookie entry + wildcard `*.<domain>` rule per listed domain. |
| DNS-01 providers | **Cloudflare + RFC2136 + raw cert-manager solver passthrough**. |
| cert-manager delivery | Optional **subchart dependency** (`certManager.enabled`) + extend `make crds`. |
| Third-party trust | Harden (dedicated ns, NetworkPolicy, PSA, no proxy-secret access) + **explicit admin confirmation** per install; no commit pinning in this phase. |

---

## 1. Verified current state (drives the work)

- **Catalog is hard-coded Go**: `api/internal/catalog/catalog.go` loads `builtin.go` + `builtin_extra.go` (8 apps) then optional `*.json` from `catalogPath`; `catalog.New("")` is called with an empty path (`api/internal/server/server.go:82`), so external JSON is dead in production.
- **Install path ignores `Repository`**: `api/internal/server/apps.go:73` calls `helm.Install(ctx, name, catalogApp.Chart, values)` with a bare chart ref like `jellyfin/jellyfin`, but no Helm repo is ever added: `helm.AddRepo`/`UpdateRepos` (`api/internal/helm/repo.go`) rewrite a bare `repo.NewFile()` on each call and have no callers; the Client uses `$TMP/naslos-helm-cache` (`api/internal/helm/client.go:27`). **Installs cannot resolve charts today.**
- **No RBAC for the API**: the only bindings for SA `naslos-api` are pods/logs/exec in `naslos-privileged` and `namespaces` get/list (`charts/naslos/templates/terminal.yaml:89-138`). The Helm SDK uses the `secret` storage driver (`client.go:45`), so it needs Secret CRUD plus workload/IngressRoute/certificate permissions. Nothing grants them (`Makefile`, `scripts/deploy-vm.sh` grant no RBAC).
- **Distroless runtime**: `api/Dockerfile:17` → `gcr.io/distroless/static-debian12:nonroot`; no shell, no git; Helm is a Go library (`api/go.mod`), not a binary.
- **Routing is static and single-domain**: 4 IngressRoutes in `charts/naslos/templates/ingress.{yaml,api}.yaml`, matching `Host({{ .Values.domain }})`. Authelia is single-domain (`session.domain`, `access_control` rules) in `charts/naslos/templates/authelia-config.yaml`; the `security-headers` middleware sets `stsIncludeSubdomains` (`traefik-middleware.yaml:41`). TLS is a chart-generated self-signed cert for `domain` only (`tls-secret.yaml:44-45`, no SANs).
- **UI is close**: `ui/src/routes/apps/+page.svelte` (Catalog/Installed tabs), `CatalogBrowser.svelte`, `AppInstallModal.svelte`, `SchemaForm.svelte`, `InstalledApps.svelte` (its Configure button is `disabled`). No source/channel/exposure concept anywhere.
- **Tests**: `docs/spec.md` §3.4 FR-APP-01…04 are normative; no Playwright coverage of the apps page (Go tests only, `routes_test.go:32-33,162,176`). `scripts/audit.sh:189-197` asserts the exact current RBAC, so broadening it will fail the sweep until updated.

---

## 2. Target architecture

### 2.1 Repo contract (NaslosCharts and any user repo)

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

`naslos-app.yaml` (canonical keys):

```yaml
name: jellyfin                 # required, must equal the folder name
displayName: Jellyfin
description: Free media system
category: media
icon: "📺"
website: https://jellyfin.org
version: 10.9.0                # informational; the chart Version wins for Helm
tags: [media, streaming]
schema:                        # JSON Schema (object) driving SchemaForm
  type: object
  properties:
    timezone: { type: string, title: Timezone, default: UTC }
  required: []
defaultValues:
  timezone: UTC
services:                      # what the subdomain routes to (fallback: discovery)
  - name: "{{ .Release.Name }}"   # templated with the release name
    port: 8096
    scheme: http
exposure:                      # defaults shown in the install UI
  subdomain: jellyfin
  tls: true
  auth: true
  localOnly: false
```

`naslos-repo.yaml` (optional, at repo root):

```yaml
name: NaslosCharts
channels:
  Prod: Prod
  Develop: Develop
  Experimental: Experimental
```

- Branch names are **not** hard-coded in the API; a repo that lacks a channel
  (no matching branch) simply does not offer that channel.
- `name` must be validated: DNS-1123 label, no path separators, must equal folder
  name; `services[].name` templating is limited to the release name (sanitised).

### 2.2 API components (new/changed)

- `api/internal/chartsrepo/` (**new**): source registry + `go-git` clone/pull into
  a PVC cache; credential resolution (public / HTTPS token / SSH key from
  Secrets); TTL + manual refresh; stale fallback; per-source file listing.
- `api/internal/catalog/` (**rewrite**): load entries from cloned repos
  (`naslos-app.yaml`), merge sources with user-override precedence, add
  `source`/`channel`/`chartPath`/`services`/`exposure` to `App`; keep the JSON
  API shape backward compatible. Delete the builtin loading.
- `api/internal/apps/` (**new**): app install records (JSON beside
  `shares.json`), install/upgrade/uninstall orchestration, release backfill,
  route reconciliation.
- `api/internal/routing/` (**new**): render Traefik `IngressRoute` + middlewares
  from an exposure record (dynamic client); create/update/delete in
  `naslos-apps`; reconcile on startup.
- `api/internal/certs/` (**new**): domain records, cert-manager `Issuer`
  (ACME DNS-01) + wildcard `Certificate` CRs, DNS provider credential Secrets,
  status/renewal reporting.
- `api/internal/server/` (**new routes**): sources, refresh, install preview/confirm,
  app exposure, domains/SSL.
- **RBAC**: add a Role/RoleBinding for SA `naslos-api` in `naslos-apps`
  (see §4) and update `scripts/audit.sh` assertions.

### 2.3 Exposure → routing mapping

Per app record with release `R` in ns `naslos-apps`, target `svc:port`:

| Setting | Effect |
| --- | --- |
| `subdomain` | `Host(`<label>.<baseDomain>`)`; empty ⇒ **no IngressRoute** (cluster-internal only). |
| `tls` | on ⇒ route on `websecure` with `tls.secretName` = base domain's cert Secret; off ⇒ route on `web` (plain HTTP), **no** HTTPS redirect. |
| `auth` | on ⇒ attach `forwardauth-authelia` (existing Middleware, referenced by FQDN/namespace); off ⇒ no forwardAuth. |
| `localOnly` | on ⇒ attach a Traefik `IPAllowList` middleware limited to a configurable LAN CIDR; off ⇒ any source. |

- The API always attaches `security-headers` **without** `stsIncludeSubdomains`
  (see §5) so a TLS-off app is not HSTS-forced by the main UI.
- `auth` is only selectable when `<baseDomain>` is in the chart's SSO domain
  list; otherwise the UI disables it and explains why (Authelia cookie/ACL
  constraint).

### 2.4 Domains & SSL

- Domain record: `{ baseDomain, dnsProvider, credentialsSecretRef, acmeEmail,
  environment: staging|production, primary: bool, certStatus }`.
- Primary domain (serves UI + Authelia) remains a Helm value; the SSL UI may
  request/attach its certificate but switching it stays a Helm change.
- App subdomain = DNS-1123 label + base domain; validated and collision-checked.
- Cert: cert-manager `Issuer` (ACME DNS-01) + `Certificate` with
  `dnsNames: [<domain>, "*.<domain>"]`, written into a TLS Secret in
  `naslos-apps`; IngressRoutes reference that Secret.

---

## 3. Ordered tasks

1. **Spec + docs first (conformance rule).** Update `docs/spec.md` §3.4 with new
   FR-APP requirements (sources, channels, install-config, exposure, admin
   confirmation, multi-domain SSO) and mark the migration; update `docs/api.md`
   (all new routes), `docs/app-catalog.md`, `docs/development.md`,
   `docs/deployment.md` (cert-manager prerequisite, wildcard DNS), and the
   `docs/README.md` index.
2. **`chartsrepo` package + tests.** Source model, `go-git` clone/pull, TTL,
   credentials, stale fallback, path traversal defence (reject `..`, absolute
   paths, symlinks outside the clone), repo size/file-count guard.
3. **Catalog rewrite + tests.** `naslos-app.yaml` parse/validate,
   source/channel merge with user-override precedence, orphan handling, JSON
   response compatibility with `CatalogEntry`.
4. **RBAC + namespace.** Add `naslos-apps` (labels, PSA `baseline`) and the
   API's namespaced Role/RoleBinding; extend `make crds` for cert-manager CRDs;
   update `scripts/audit.sh` RBAC assertions; add namespacing to NetworkPolicy.
5. **App records + install rewrite + backfill.** Persisted record store; local
   chart load (`loader.LoadDir`); install/upgrade/uninstall into `naslos-apps`;
   one-time backfill of releases in `naslos`; `values` round-trip for
   reconfigure.
6. **Routing reconciler.** Exposure → IngressRoute + middlewares; startup
   reconcile; delete on uninstall; idempotent updates.
7. **Certs subsystem + cert-manager subchart.** `Issuer`/`Certificate` CRs,
   provider credential Secrets, staging/production, status; gate the SSL page on
   CRD presence.
8. **API routes + tests.** `GET/POST/DELETE /api/sources`, `POST /api/sources/refresh`,
   `GET /api/apps/{name}/exposure` + `PUT`, `GET/POST/PUT/DELETE /api/domains`,
   `GET /api/domains/{d}/certificate`; keep `POST /api/apps` with an added
   confirmed-install step. 405/4xx contracts per API-02/03.
9. **UI.** `ui/src/routes/apps/+page.svelte` gains a **Sources** tab; new
   `SourceList.svelte`/`SourceForm.svelte`; rewrite `CatalogBrowser.svelte` to show
   source/channel badges and a channel filter; rewrite `AppInstallModal.svelte`
   into steps **Config → Exposure → Review → Confirm** (new
   `ExposureForm.svelte`); enable `InstalledApps.svelte` Configure + add
   exposure editing and the app URL; new `ui/src/routes/domains/+page.svelte`
   (+ Sidebar entry) with `DomainForm.svelte` and `CertificateStatus.svelte`.
   `npm run check` must be clean.
10. **E2E + live drill.** New `ui/tests/apps.spec.ts` (source add/refresh,
    install with exposure, exposure change reflected in the IngressRoute,
    uninstall; skip cleanly when no source/domain is configured) and
    `ui/tests/domains.spec.ts`. Live drill against the VM: real clone of
    `git@github.com:AessemOps/NaslosCharts.git`, an install reachable at
    `<app>.<domain>`, an unauthenticated app, a local-only app, and a
    Let's Encrypt staging cert via DNS-01.
11. **Cutover.** Deploy fresh image tags; run the backfill; verify the existing
    `buddy`/platform releases still list; run `scripts/audit.sh` + both Go test
    suites + Playwright.

---

## 4. RBAC to grant the API (namespaced, `naslos-apps`)

Role `naslos-api-apps` in `naslos-apps`, bound to SA `naslos-api`/`naslos`:

- core: `services`, `configmaps`, `secrets`, `persistentvolumeclaims`,
  `serviceaccounts`, `pods`, `pods/log`, `events` → get/list/watch/create/update/patch/delete
- apps: `deployments`, `statefulsets`, `daemonsets`, `replicasets` → full namespaced verbs
- batch: `jobs`, `cronjobs` → full namespaced verbs
- `traefik.io`: `ingressroutes`, `middlewares` → full namespaced verbs
- `cert-manager.io`: `certificates`, `issuers`, `clusterissuers`(get only) → full/read as needed
- **Not granted**: `naslos-proxy`/Talos-config access, cluster-scoped writes,
  `pods/exec`, `nodes`, CRDs.

`ingressClass`/IngressRoute service refs stay in-namespace; the Authelia
forwardAuth middleware is created in `naslos-apps` and points at the Authelia
Service **FQDN** (no cross-namespace Traefik reference).

---

## 5. Risks / accepted trade-offs

- **Wildcard subdomains need wildcard DNS.** Document `*.<domain>` as a
  prerequisite (router/dnsmasq/registrar). Without it, app hostnames do not
  resolve and the requirement's subdomain feature is only partially usable.
- **Remove `stsIncludeSubdomains`** from `security-headers` (currently
  `traefik-middleware.yaml:41`); otherwise visiting the main UI pins HSTS for
  all subdomains and a TLS-off app cannot load.
- **Third-party charts are a privileged trust boundary.** Any install grants
  arbitrary in-cluster manifests under the API's namespaced Role. Mitigations in
  scope: dedicated namespace, PSA, NetworkPolicy, no proxy-secret/Talos access,
  admin-only gate, explicit confirmation, source/branch/commit shown. Commit
  pinning is deferred.
- **SSO is limited to Helm-declared domains.** Apps on base domains outside
  `sso.domains` can be TLS/local-only, never Authelia-protected. The install UI
  must say so rather than silently dropping auth.
- **Wildcard cert caveats.** LE wildcard requires DNS-01 and the provider's
  credentials; `.local` mDNS names can never get a public cert, so the default
  self-signed posture must keep working when no ACME domain is configured.
- **Primary domain stays Helm-owned**, so "add a domain" covers app domains and
  certificates, not relocating the UI/portal dynamically.
- **Chart repo is assumed to exist** at
  `git@github.com:AessemOps/NaslosCharts.git` with `Prod`/`Develop`/`Experimental`.
  Its initial contents and CI (which branch is packaged) are out of scope here
  but block the live drill.
- **`helm upgrade --reuse-values`** ignores `-f`; new values must be passed with
  `--set` (existing operational gotcha, repeated in `AI_Handoff.md`).
- **Image tag suffixes** must be bumped for every deploy (`IfNotPresent`)
  (`AI_Handoff.md` gotcha #1).

---

## 6. Validation

- `cd api && go build ./... && go vet ./... && go test -race ./...`
- `cd agent && go build ./... && go vet ./... && go test -race ./...`
- `cd ui && npm run check && npx playwright test`
- `helm lint charts/naslos -f charts/naslos/values.yaml` and with `values-vm.yaml`
- `sh scripts/audit.sh` (after updating its RBAC assertions)
- Unit tests to add: `chartsrepo` (clone/TTL/stale/path-traversal/credentials),
  catalog merge precedence + orphans, exposure→IngressRoute rendering,
  cert-manager CR rendering, install-record round-trip, backfill.
- Live drill (VM `192.168.1.117`): real clone, install reachable at
  `<app>.<domain>`, auth-on vs auth-off, local-only denial from off-LAN,
  TLS staging cert via DNS-01, uninstall removes routing.
- Spec conformance: every changed FR-APP MUST has its test in the same change.

---

## 7. Open questions (non-blocking)

1. **NaslosCharts repo contents/CI** — exact repo layout (`apps/` vs repo root),
   which branch is "the stable one", and whether `naslos-repo.yaml` is wanted.
   Recommended: `apps/<name>/` layout as in §2.1, `Prod` stable.
2. **Commit pinning** — deferred; revisit if third-party trust needs tightening.
3. **LAN CIDR source for `localOnly`** — reuse `networkPolicy.nfsClientCIDR`
   (`values-vm.yaml:110`) or add `exposure.localOnlyCIDR`. Recommended: a
   dedicated `exposure.localOnlyCIDR` with `nfsClientCIDR` as fallback.
