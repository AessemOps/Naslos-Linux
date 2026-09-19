# UI link to the Traefik dashboard (admin-only)

Goal: an administrator signed in to Naslos gets a link in the sidebar to Traefik's
dashboard, served behind Authelia on the same host and restricted to
`naslos_admins` with 2FA. Non-admins (and anonymous users) cannot reach it.

Baseline: `feature/authelia-traefik-production` (commit `ad960af`), live on
192.168.1.117 at helm revision 11, production posture.

## How it will work

```
https://naslos.local/traefik/dashboard/   (link in the sidebar, new tab)
  -> Traefik IngressRoute (websecure)
     -> forwardauth-authelia   (admins: two_factor; others: 403)
     -> security-headers
     -> api@internal (TraefikService), Traefik API served under basePath /traefik
```

`api.insecure` stays **false**, so Traefik's API is not exposed on any port; the
IngressRoute is the only way in. `api.basePath: /traefik` makes the dashboard
frontend call `/traefik/api/*` (otherwise it would call `/api/*`, which the
Naslos API owns).

## Changes

### 1. `charts/naslos/values.yaml` — enable the dashboard API

Replace the `additionalArguments: ["--api.dashboard=false"]` line under
`traefik:` with the chart's typed options (the chart renders
`--api.dashboard=true` and `--api.basePath=/traefik`, verified in
`traefik/templates/_podtemplate.tpl:227-234`):

```yaml
  api:
    dashboard: true
    basePath: /traefik
```

`values-prod.yaml` needs no change (the API is only reachable via the
IngressRoute, which the prod profile enables).

### 2. `charts/naslos/templates/ingress.yaml` — keep `/traefik` off the SPA route

The UI route is
`` Host(domain) && !PathPrefix(`/api`) && !PathPrefix(`/authelia`) ``.
Add `` && !PathPrefix(`/traefik`) `` — otherwise the longer UI rule wins
priority and serves the SPA for `/traefik`.

### 3. `charts/naslos/templates/ingress-api.yaml` — the dashboard route

Add (inside the existing `ingress.enabled` guard):

```yaml
---
apiVersion: traefik.io/v1alpha1
kind: IngressRoute
metadata:
  name: naslos-traefik-dashboard
  namespace: {{ .Values.namespace }}
spec:
  entryPoints:
    - websecure
  routes:
    - match: Host(`{{ .Values.authelia.domain }}`) && PathPrefix(`/traefik`)
      kind: Rule
      middlewares:
        - name: forwardauth-authelia
        - name: security-headers
      services:
        - name: api@internal
          kind: TraefikService
  tls:
    secretName: {{ .Values.ingress.tls.secretName | default "naslos-tls" }}
```

No `proxy-identity` here: the secret proves a request came through the proxy for
the Naslos API's benefit; Traefik's own dashboard does not read it.

### 4. `charts/naslos/templates/authelia-config.yaml` — admin-only

The subject rule already gives `group:naslos_admins` two_factor on every path,
but the `one_factor` catch-all would still let a `naslos_users` account load the
dashboard. After the admin rule and before the catch-all, add:

```yaml
        # The Traefik dashboard exposes every router/service/middleware, so it
        # is operator-only. Admins already matched the subject rule above (and
        # got 2FA); everyone else is denied rather than merely one-factor.
        - domain: {{ .Values.authelia.domain | quote }}
          subject:
            - "group:naslos_users"
          policy: deny
          resources:
            - "^/traefik"
```

Order matters (first match wins): an account in both groups is allowed by the
admin rule first.

### 5. `ui/src/lib/components/Sidebar.svelte` — the link

The UI currently has no notion of the signed-in user. Add an `onMount` fetch of
`/api/auth/me` (returns `{username, groups: "a,b", email, displayName}`,
`users_extra.go:208-229`) and render the entry only when the groups contain
`naslos_admins`:

```svelte
  import { onMount } from 'svelte';

  let isAdmin = false;

  onMount(async () => {
    try {
      const res = await fetch('/api/auth/me');
      if (!res.ok) return; // dev posture (auth off) or anonymous: keep it hidden
      const data = await res.json();
      const groups = String(data.groups ?? '').split(',').map((g) => g.trim());
      isAdmin = groups.includes('naslos_admins');
    } catch {
      // Hidden when the identity cannot be read.
    }
  });
```

Render after the `{#each navItems}` block:

```svelte
    {#if isAdmin}
      <a
        href="/traefik/dashboard/"
        target="_blank"
        rel="noreferrer"
        class="flex items-center gap-3 px-3 py-2.5 rounded-lg transition-colors text-gray-400 hover:bg-naslos-border"
      >
        <span class="text-lg">🧭</span>
        <span class="font-medium">Traefik</span>
        <span class="ml-auto text-xs text-gray-600">↗</span>
      </a>
    {/if}
```

Use a relative `/traefik/dashboard/` (not an absolute URL) so it works on
whatever host the operator used.

### 6. Docs

- `docs/identity-sso.md`: add a row to the access-control table for `/traefik`
  (admins two_factor, `naslos_users` deny) and a line in the TLS/proxy section
  about the dashboard route and `api.basePath`.
- `docs/deployment.md`: mention the dashboard URL in the production section.
- `AI_Handoff.md`: add it to the production-posture summary.

## Build / deploy notes

- **Only the UI image changes**, so rebuild `naslos-ui` with a fresh tag
  (`0.1.0-r3`), push it, and bump `ui.image.tag` in
  `charts/naslos/values-vm.yaml` (the gotcha that bit us last time: a bare
  `make install-prod` uses the tags in that file). The API is untouched.
- Then `make install-prod` (curl+scp not needed; images go to
  192.168.1.2:30095).
- The chart change alone (dashboard API, routes, rules) is applied by the same
  upgrade; the Authelia config checksum annotation rolls Authelia automatically.

## Validation

Local:
```bash
helm lint charts/naslos -f charts/naslos/values.yaml -f charts/naslos/values-vm.yaml -f charts/naslos/values-prod.yaml
helm template naslos charts/naslos -n naslos -f ... --output-dir /tmp/kilo/rendered-traefik
# check: --api.dashboard=true --api.basePath=/traefik, the new IngressRoute,
#        the UI rule excludes /traefik, the Authelia deny rule parses
python3 -c "import yaml; ..."   # parse the rendered authelia configuration.yml
cd ui && npm run check
```

Live (after deploy):
| Check | Expected |
|---|---|
| `curl -skI https://naslos.local/traefik/dashboard/` | 302 to `/authelia/?rd=…` (anonymous) |
| `kubectl -n naslos get ingressroute` | includes `naslos-traefik-dashboard` |
| Traefik pod args | contain `--api.dashboard=true` and `--api.basePath=/traefik` |
| Browser as `admin` (2FA) | sidebar shows **Traefik**; the new tab loads the dashboard under `/traefik/dashboard/` |
| `naslos_users` account (if created) | `/traefik` → 403 |
| `https://naslos.local/api/...` | unchanged |

The Playwright suite runs against the dev posture, where `/api/auth/me` is 401,
so the link is intentionally absent there — note that in the handoff rather than
adding a test that cannot authenticate.

## Open question

- Link placement/label: currently proposed as a sidebar entry "🧭 Traefik" that
  opens in a new tab. Alternative: a card on the Dashboard page. Default is the
  sidebar entry.
