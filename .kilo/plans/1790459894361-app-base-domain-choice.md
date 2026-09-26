# Fix: app exposure has no base-domain choice + Domains-page SSO promotion

## Goal

Two related gaps:

1. **Base-domain choice.** The install wizard's Exposure step and the per-app
   Exposure modal hard-code the domain suffix to the primary domain
   (`.naslos.local` in the screenshot). Let the operator pick any configured
   base domain (primary + domains registered on the Domains page), and fix the
   latent bugs that silently revert a chosen domain to the primary.
2. **Promote a domain to SSO.** On the Domains page a registered domain must be
   promotable to an SSO domain (multiple allowed) so apps on it can use Authelia
   auth. Today the SSO list is chart-only (`sso.domains` → `SSO_DOMAINS`) and
   Authelia's config is rendered at install time.

Branch: `fix/app-base-domain-choice` from `origin/master` (`git fetch` first; do
**not** branch from `fix/post-merge-followups`). Never push to `master`; open a
PR. Two logical commits (workstream A, workstream B); one branch is fine.

## Decisions

- Base-domain choice is limited to configured domains (primary first, then
  `certs.Store`), de-duplicated. No free-text domain.
- **SSO promotion takes effect live** (user-chosen). The domain store becomes
  the source of truth for SSO membership; the API reconfigures Authelia and
  restarts it on change.
- Effective SSO list = primary + registered domains with `sso: true`, unioned
  with `SSO_DOMAINS` from the chart (kept as a seed/floor so existing
  `values.yaml` installs keep working). Env-listed domains cannot be demoted via
  the UI (edit values); store-flagged ones can.
- `auth` is offered only when the selected domain is in the effective SSO list
  (FR-APP-10); switching to a non-SSO domain clears and disables the toggle.
- Install wizard and post-install Exposure modal share one domain control.

## Constraint found during planning

- Authelia reads config only at startup (no reload) and validates the
  `access_control` rules as one file (splitting `rules` across files via
  `--config` merge is explicitly unsupported). So the dynamic parts must be a
  single coherent source and Authelia must be restarted on change.
- The vendored `authelia` subchart (0.11.22) supports `pod.extraVolumes`,
  `pod.extraVolumeMounts`, `pod.env`, `pod.kind`, and sets
  `X_AUTHELIA_CONFIG_FILTERS=template` by default (config templating is already
  active in this install — see the `{input}` comment in
  `charts/naslos/templates/authelia-config.yaml:124`). Authelia's stable
  Go-template filter exposes `fileContent` + `nindent`, which lets the chart keep
  the security-critical rules and the API own only the domain list fragments.
- The API's release-namespace Role is read-only today
  (`charts/naslos/templates/rbac-apps.yaml:89-122`). Live promotion therefore
  needs a **scoped** write grant (one ConfigMap + the Authelia workload). The
  fragment ConfigMap contains no credentials, so the jwt/LDAP secrets stay out
  of the API's reach.

## Workstream A — base-domain choice

### Backend

1. `api/internal/server/server.go` — add `selectableDomains() []string`:
   `s.baseDomain` first, then `s.domains.List()` base domains, dedup/skip empty.
2. `api/internal/server/domains.go` (`GET /api/domains`) — add
   `"selectableDomains"` and `"ssoDomains"` to the response (keep `domains`
   unchanged so the Domains page is unaffected).
3. `api/internal/server/apps.go` (`GET /api/apps/{name}/exposure`) — return
   `baseDomain` = `rec.BaseDomain` fallback primary (**fixes the revert bug**),
   plus `primaryDomain`, `selectableDomains`, `ssoDomains`, and
   `authAllowed` = `AuthAllowed(rec.BaseDomain)` (was `""`).
4. `api/internal/server/apps.go` (`POST /api/apps`, `PUT /api/apps/{name}/exposure`)
   — when `baseDomain` is non-empty, reject it if not in `selectableDomains()`
   (400).
5. `api/internal/apps/apps.go` (`urlFor`) — host from `rec.BaseDomain` fallback
   `m.baseDomain`.

### UI

6. `ui/src/lib/components/ExposureForm.svelte` — keep bindable `baseDomain`,
   replace the static `.{baseDomain}` suffix with a `<select id="exposure-domain">`
   of `selectableDomains`; new props `selectableDomains`, `ssoDomains`; drop the
   `authAllowed` prop and derive it. Add a `chooseDomain(value)` handler (no
   reactive rewrite of `exposure`) that clears `exposure.auth` when the domain
   is not in `ssoDomains`. Fall back to the static suffix when the list is empty.
7. `ui/src/lib/components/AppInstallModal.svelte` — read `selectableDomains`
   (fallback `[baseDomain, ...domains.map(d => d.baseDomain)]`) and `ssoDomains`
   (fallback `[baseDomain]`) from `/api/domains`; default `baseDomain` to the
   primary; pass `bind:baseDomain`, `selectableDomains`, `ssoDomains`.
8. `ui/src/lib/components/AppExposureModal.svelte` — read `baseDomain`,
   `selectableDomains`, `ssoDomains` from the exposure GET; pass and bind them.

## Workstream B — SSO promotion (live)

### Model + effective list

9. `api/internal/certs/certs.go` — add `SSO bool \`json:"sso,omitempty"\`` to
   `Domain` (persisted in `domains.json`).
10. `api/internal/server/server.go` + `api/internal/server/domains.go` — add
    `effectiveSSODomains() []string`: primary + store `SSO` domains +
    `SSO_DOMAINS` env, deduped. Replace the static `s.ssoDomains` field with this
    method where it feeds the app manager / routing.
11. `api/internal/apps/apps.go` — change `Config.SSODomains []string` to a
    provider `SSODomains func() []string`; update `NewManager`, `Manager`,
    `AuthAllowed`/`authAllowed`, `SSODomains()`, `ReconcileRoutes`,
    `applyRoute` (routing still receives a materialised `[]string`).

### Authelia reconfiguration (new `api/internal/authelia`)

12. `Fragments(domains []string) (cookiesYAML, rulesYAML []byte)` — pure,
    unit-tested. `cookies.yml`: one `- domain: <d>` / `authelia_url:
    https://<d>/authelia/` per domain. `rules.yml`: for each non-primary domain
    an apex `one_factor` rule and a `*.` wildcard rule for **every** domain
    (marshal with `gopkg.in/yaml.v3`, already a direct dep).
13. `Reconciler.Sync(ctx, domains)`: read ConfigMap `naslos-authelia-sso` in
    `s.namespace`; if the fragments differ, update keys `cookies.yml`/`rules.yml`;
    then restart Authelia by patching StatefulSet `naslos-authelia` pod-template
    annotation `naslos.local/sso-revision=<unixnano>`. No-op when unchanged;
    idempotent. Env-overridable names (`AUTHELIA_SSO_CONFIGMAP`,
    `AUTHELIA_WORKLOAD`, `AUTHELIA_NAMESPACE`). Uses `s.kubernetesClient`.
14. Wire `Sync` on startup (after domains load) and after every SSO change;
    best-effort with `LastError`-style logging, never fatal.

### Endpoint

15. `POST /api/domains/{domain}/sso` body `{"enabled": bool}` (owner-gated).
    - Primary domain → 400 (always SSO).
    - Disable when any installed app has `Exposure.Auth` and
      `BaseDomain == domain` → 409 listing the apps (avoids breaking live routes).
    - Otherwise set the flag, upsert, `Sync`, and return the updated domains +
      `ssoDomains`.
    Register the route in `routes()` and add it to `ownerPaths` in
    `api/internal/server/routes_test.go`.

### Chart

16. `charts/naslos/templates/authelia-config.yaml`:
    - `session`: always use `cookies:`, populated by
      `fileContent "/config-sso/cookies.yml"` (single-domain `domain:` branch
      removed).
    - `access_control.rules`: keep the static primary rules (`health`, `buddy`,
      `/authelia` bypass, admins `two_factor`, traefik deny, apex `one_factor`);
      delete the static `range $ssoDomains` loops and append the dynamic rules
      via `fileContent "/config-sso/rules.yml"`.
    - **Delimiter trap:** Authelia and Helm share `{{ }}`. Emit Authelia actions
      as Helm raw-string literals, e.g.
      ``{{ `{{- fileContent "/config-sso/cookies.yml" | nindent 4 }}` }}``; verify
      indentation with `helm template` and `authelia config template` in a live
      drill.
17. `charts/naslos/templates/authelia-sso.yaml` (new) — ConfigMap
    `naslos-authelia-sso` with `cookies.yml`/`rules.yml` seeded from
    `.Values.sso.domains`, rendered only when absent (`lookup`) and annotated
    `helm.sh/resource-policy: keep`, so Helm never clobbers the API's runtime
    state (nor deletes it once it stops rendering).
18. `charts/naslos/values.yaml` (authelia) — pin `pod.kind: StatefulSet`
    (deterministic restart target) and add `pod.extraVolumes` +
    `pod.extraVolumeMounts` mounting `naslos-authelia-sso` at `/config-sso`
    (read-only). Confirm `X_AUTHELIA_CONFIG_FILTERS=template` is still emitted by
    the subchart; add it via `pod.env` only if not.
19. `charts/naslos/templates/rbac-apps.yaml` — add a scoped Role/Binding
    `naslos-api-authelia-sso` in the release namespace: `configmaps`
    `get,update,patch` on `naslos-authelia-sso`; `statefulsets` `patch` on
    `naslos-authelia`. Leave `naslos-api-platform-read` read-only.

### UI

20. `ui/src/routes/domains/+page.svelte` — per non-primary domain an
    "SSO"/"Make SSO" toggle (primary shows a locked "SSO (primary)" badge) that
    calls `POST /api/domains/{domain}/sso`, surfaces 409 reasons, and reloads.
    Show the current `sso` state from each record and an info note that
    promoting restarts Authelia.

### Spec / docs (spec rule)

21. `docs/spec.md`:
    - **FR-APP-10** — add: the base domain MUST be selectable among configured
      domains (default primary); `auth` offered only on the effective SSO list.
    - **FR-APP-15** — reword from "chart-declared SSO domain list" to the
      **runtime SSO list** (primary + promoted domains), applied to Authelia.
    Update the referenced tests in the same change (see below).
22. `docs/app-catalog.md` (Exposure → routing, add `baseDomain`; Domains section:
    SSO promotion + Authelia restart) and `docs/api.md` (`/api/domains` returns
    `selectableDomains`/`ssoDomains`; exposure GET returns the app's own
    `baseDomain`; new `POST /api/domains/{domain}/sso`).

## Tests

23. `api/internal/apps`: `urlFor` uses `rec.BaseDomain`; `AuthAllowed` follows the
    provider function (changes when the provider's list changes).
24. `api/internal/authelia`: `Fragments` golden tests (0/1/many domains,
    primary-only, wildcard coverage) and `Sync` idempotence + restart-on-change
    with a fake clientset.
25. `api/internal/certs`: `SSO` round-trips through the store.
26. `api/internal/server`: refactor `newTestServer` into a seedable helper
    (writes `APPS_CONFIG`/`DOMAINS_CONFIG` before `New`); then
    `GET /api/domains` exposes `selectableDomains`/`ssoDomains`; exposure GET
    returns the record's `baseDomain`; `POST .../sso` enable works, primary is
    400, disabling a domain with an auth app is 409; `PUT` exposure with an
    unknown `baseDomain` is 400.
27. Chart: `helm template` assertions that the seed ConfigMap renders and that
    `authelia-config` contains the `fileContent` hooks and no static SSO loops
    (extend `scripts/audit.sh` or add a render check).
28. Playwright: `apps.spec.ts` exposure modal shows the domain `<select>`;
    `domains.spec.ts` exposes `selectableDomains`/`ssoDomains` and the SSO
    toggle is present.

## Deploy plumbing

29. Bump `ui` `0.1.0-r22`→`0.1.0-r23` and `api` `0.1.0-r29`→`0.1.0-r30` in
    `charts/naslos/values-vm.yaml`; build/push with fresh `IMAGE_TAG`s.
30. Verify on the VM, then update `AI_Handoff.md` → "Deployed right now".

## Validation gates

```bash
cd api   && go build ./... && go vet ./... && go test -race ./...
cd ui    && npm run check && npm run build
helm lint charts/naslos -f charts/naslos/values.yaml -f charts/naslos/values-vm.yaml
make -n install-vm
cd ui    && ./node_modules/.bin/playwright test tests/apps.spec.ts tests/domains.spec.ts
```

Live drill: register a secondary domain, promote it, confirm Authelia restarts
and an app on it with `auth` challenges via forwardAuth; demote is refused while
an auth app exists; `helm upgrade` does not clobber the promoted state (seed
ConfigMap is `keep` + API reconciles on startup).

## Risks / notes

- **Helm/Authelia `{{ }}` collision** (step 16) is the main implementation trap;
  `helm template` plus an Authelia `config template` check must pass.
- Restarting Authelia briefly interrupts auth (single replica) — the promoting
  admin initiated the change; acceptable.
- Patching the Authelia StatefulSet adds an out-of-band annotation Helm may drop
  on upgrade; harmless (the next change re-patches).
- ConfigMap fragment must always be valid/non-empty or Authelia fails to start;
  the chart seeds it and the API validates before writing.
- No direct dependency change → no `CREDITS.md` change required.
- `SSO_DOMAINS` remains the chart seed; if 0.11.22 turns out not to support a
  needed `pod.*` key, fall back to the subchart `configMap.extraConfigs` /
  `secret.additionalSecrets` mount for the fragment file (same API contract).

## Out of scope

- Free-text base domains (unregistered domain → broken DNS/cert).
- Per-domain Authelia 2FA policy overrides beyond the existing admin rule.
- Demoting a chart `SSO_DOMAINS` entry from the UI (requires editing values).
