# Authelia/Traefik: dev posture → production on 192.168.1.117

## Goal

Move the single-node instance from the dev posture (`auth.disabled=true`, UI on the
NodePort, no TLS, no proxy) to a proxy-authenticated production posture:
**https://naslos.local** served by Traefik on the node's 80/443, Authelia
forwardAuth with the portal at **/authelia**, 2FA on every admin surface, a
chart-managed TLS cert, `/api` routed straight to the API (so the terminal works),
and the NodePort removed.

Baseline: `master` `3821204` plus the committed branch `fix/correctness-secret-batch`
(`261aa3e`); live instance on helm revision 4 (images `0.1.0-r2`).

## Decisions (resolved with the user)

| Decision | Choice |
|---|---|
| Hostname | `naslos.local` (Avahi already publishes `host-name=naslos` → mDNS resolves it to .117, `samba/image/entrypoint.sh:154`; a router A record is optional redundancy for Windows) |
| TLS | Chart-managed self-signed, generated once and reused across upgrades via `lookup`; `ingress.tls.existingSecret` is the escape hatch for a real CA cert later |
| Traefik exposure | `hostPort` 80/443 on the node (Service stays ClusterIP) |
| Authelia portal | Subpath `https://naslos.local/authelia` (`server.path: /authelia`) |
| `/api` routing | Directly to `naslos-api:8080` through the middleware chain (UI route handles everything else) |
| Profile / cutover | New `charts/naslos/values-prod.yaml` layered on top of `values-vm.yaml`, one `helm upgrade` on release `naslos`; `values-vm.yaml` stays the dev/rollback profile |
| First admin | Dedicated uid `admin` in `naslos_admins`, created **before** the flip, TOTP/WebAuthn enrolled at first portal login |
| 2FA | `two_factor` on every admin-only prefix, not just the current five |

## Key facts from inspection

- `authelia-config.yaml` already fixed CR-01 (scoped bypass rules, ordered 2FA before
  one_factor) and has the `/api/health` and `/api/buddy/v1/` bypasses.
- `ingress.yaml` hardcodes `tls.secretName: naslos-tls` (the secret does not exist)
  and sends **all** paths to `naslos-ui`; nginx deliberately 403s `/api/pods`,
  `/api/namespaces`, `/api/ws/exec`, so the terminal cannot work in the proxy
  posture as charted.
- `traefik-middleware.yaml` sets `trustForwardHeader: true` (CR-38 / NAS-009).
- `values-vm.yaml` keeps `auth.disabled=true`, `ui.nodePort.enabled=true`,
  `authelia.domain=192.168.1.117`, `traefik.service.type=ClusterIP`.
- `auth.Middleware` requires the proxy secret + a source IP in `TRAEFIK_CIDR`
  (`10.0.0.0/8`); a direct Traefik→API request satisfies both (Traefik pod IP is
  10.244.x) and `RequireAdmin` already gates the admin routes (CR-03).
- Authelia docs confirm: with a subpath, the forwardAuth URL must stay
  `/api/authz/forward-auth` (the handler listens on both `/` and the configured
  path), and `authelia_url` is required for correct redirects when `server.path`
  is set.

## Changes

### 1. `charts/naslos/templates/authelia-config.yaml`

- `server.path: /authelia`.
- `default_redirection_url: https://{{ .Values.authelia.domain }}/`.
- Use `.Values.authelia.domain` for `totp.issuer` and `webauthn.display_name`
  (currently already the case; confirm they render `naslos.local`).
- `session.domain: {{ .Values.authelia.domain }}` (unchanged).
- `access_control.rules`:
  - keep `domain: "*"` bypass `^/api/health$`;
  - keep `domain: <domain>` bypass `^/api/buddy/v1/`;
  - **replace** the current Authelia-endpoint bypass (`^/api/(verify|authz|…)`)
    with the portal prefix `^/authelia` — the portal route is not forwardAuthed,
    so this is defensive only; removing the old rule avoids shadowing app paths;
  - **extend** the `two_factor` rule resources to the full admin surface:
    `^/api/users`, `^/api/groups`, `^/api/apps`, `^/api/volumes`, `^/api/datasets`,
    `^/api/disks`, `^/api/shares`, `^/api/pods`, `^/api/namespaces`, `^/api/ws`,
    `^/api/buddy/(send|restore|jobs|schedules|peers|identity)`;
  - keep the `one_factor` catch-all last.

### 2. `charts/naslos/templates/traefik-middleware.yaml`

- forwardAuth `address` →
  `http://authelia.{{ .Values.namespace }}.svc.cluster.local:9091/api/authz/forward-auth?authelia_url=https%3A%2F%2Fnaslos.local%2Fauthelia`
  (URL-encode the domain from `.Values.authelia.domain`; keep the authz path at the
  root as Authelia requires).
- **Remove `trustForwardHeader: true`** (defaults to false): Traefik then sets
  X-Forwarded-* itself, and the `authelia_url` pin removes the redirect
  dependency on a client-supplied X-Forwarded-Host.
- `security-headers`: drop `stsPreload` (a `.local`/self-signed site cannot be
  preloaded and it blocks the "proceed anyway" path); keep the rest.

### 3. `charts/naslos/templates/ingress.yaml`

- UI route match → ``Host(`{{ .Values.authelia.domain }}`) && !PathPrefix(`/api`) && !PathPrefix(`/authelia`)``
  so the API and portal routes always win regardless of Traefik priority.
- TLS `secretName` → `{{ .Values.ingress.tls.secretName | default "naslos-tls" }}`.
- Keep the `web` → `websecure` redirect route (`Host(domain)`, `redirect-https`);
  it covers every path, so `http://naslos.local/authelia` also upgrades.

### 4. New `charts/naslos/templates/ingress-api.yaml`

- IngressRoute `naslos-api` on `websecure`:
  - match ``Host(`{{ .Values.authelia.domain }}`) && PathPrefix(`/api`)``
  - middlewares: `forwardauth-authelia`, `proxy-identity` (only when
    `naslos.authEnabled` is true), `security-headers`
  - service `naslos-api` port `{{ .Values.api.service.port }}`
  - `tls.secretName` as above.
- IngressRoute `naslos-authelia-portal` on `websecure`:
  - match ``Host(`{{ .Values.authelia.domain }}`) && PathPrefix(`/authelia`)``
  - middleware: `security-headers` **only** (no forwardAuth, or it loops)
  - service `authelia` port `9091`; `tls.secretName`.
- `PathPrefix('/api')` covers the WS paths, so the terminal reaches the API and
  nginx's 403 refusals remain only on the dev NodePort path.

### 5. New `charts/naslos/templates/tls-secret.yaml`

- `{{- if and (eq (include "naslos.ingressEnabled" .) "true") (not .Values.ingress.tls.existingSecret) }}`
- `$secret := lookup "v1" "Secret" .Values.namespace (.Values.ingress.tls.secretName | default "naslos-tls")`
  — if it exists, re-emit its `tls.crt`/`tls.key`; otherwise
  `$gen := genSelfSignedCert .Values.authelia.domain (list .Values.authelia.domain) (list)`
  and render a `kubernetes.io/tls` Secret named `naslos-tls`.
- Note: `lookup` returns empty under `helm template`/`--dry-run`, so a dry-run
  shows a fresh cert — expected, and the guard means installs/upgrades keep the
  first one.

### 6. `charts/naslos/values.yaml`

- Add:
  ```yaml
  ingress:
    enabled: false
    tls:
      # Secret name Traefik's IngressRoutes reference. Empty existingSecret means
      # the chart generates a self-signed cert for authelia.domain and keeps it
      # across upgrades.
      secretName: naslos-tls
      existingSecret: ""
  ```
- Comment the `traefik.ports.*.hostPort` option next to the ports block.

### 7. New `charts/naslos/values-prod.yaml`

```yaml
# Production posture: Traefik on the LAN with Authelia forwardAuth.
ingress:
  enabled: true
auth:
  disabled: false
authelia:
  domain: naslos.local
ui:
  nodePort:
    enabled: false
traefik:
  ports:
    web:      { port: 80,  hostPort: 80,  expose: { default: true } }
    websecure: { port: 443, hostPort: 443, expose: { default: true } }
  service:
    type: ClusterIP
```
(Adjust the `ports` mapping to the schema `helm show values traefik` reports; if the
subchart rejects `hostPort`, fall back to `service.type: NodePort` with fixed
nodePorts and say so in the docs.)

### 8. `Makefile`

- Add `install-prod: crds` mirroring `install-vm` but with
  `-f values.yaml -f values-vm.yaml -f values-prod.yaml` (VM specifics first,
  production posture last) and a comment that the dev profile is `install-vm`.

### 9. Docs / records

- `docs/deployment.md`: the production install/cutover/rollback commands, the
  `naslos.local` prerequisite (router A record optional; mDNS covers Apple/Linux),
  and the cert-trust step.
- `docs/identity-sso.md`: portal at `/authelia`; the expanded 2FA table; header-trust
  section updated (secret + CIDR, `trustForwardHeader` off).
- `docs/CODE-REVIEW.md`: resolution notes for CR-01 (portal routed, trustForwardHeader)
  and CR-38's `trustForwardHeader` item.
- `AI_Handoff.md`: deployed posture and the new admin account.

## Cutover on 192.168.1.117

0. Prerequisite: add a router/local-DNS A record `naslos.local → 192.168.1.117`
   (optional), and trust the self-signed cert on the client (import `naslos-tls`).
1. **While auth is still disabled** (current NodePort/API): create the operator —
   `POST /api/users` `{"uid":"admin","password":"<strong>","groups":["naslos_admins"]}`;
   confirm `GET /api/users/admin` shows `groups:["naslos_admins"]`. Never put the
   password in the repo.
2. `helm lint` + `helm template` for `values.yaml`, `values-vm.yaml+values-prod.yaml`;
   confirm the TLS Secret, the three IngressRoutes and the Authelia config render,
   and no duplicate YAML keys (CR-11).
3. `make install-prod` (single-node: expect a short API/UI/Traefik outage).
4. Verify (below), then enroll TOTP/WebAuthn at first portal login.

### Verification

| Check | Expected |
|---|---|
| `kubectl -n naslos get ingressroute` | `naslos-ui`, `naslos-api`, `naslos-authelia-portal`, `naslos-redirect` |
| `kubectl -n naslos get secret naslos-tls` | present, `kubernetes.io/tls` |
| `curl -sk https://naslos.local/api/health` | 200 (bypass) |
| `curl -sk https://naslos.local/api/buddy/v1/status` | JSON body (bypass), not a redirect |
| `curl -skI https://naslos.local/api/users` | 302 to `/authelia` (or 401), **not** 200 |
| `curl -skI https://naslos.local/` | 302 to the portal when logged out |
| Browser | `https://naslos.local` → `/authelia?rd=…` → admin login → TOTP/WebAuthn → dashboard |
| Terminal page | pods list + shell work (proves the direct `/api` route) |
| Second account in `naslos_users` | dashboard 200, `/api/users` 403 (CR-03 still holds) |
| `http://192.168.1.117:30080` | no longer serves the app (NodePort disabled) |
| Direct pod API without `X-Naslos-Proxy-Secret` | 401 (NAS-001 unchanged) |
| `helm template ... --set ingress.enabled=false` | renders exactly the dev objects (no TLS/routes) |

## Rollback

`helm upgrade naslos charts/naslos -n naslos -f charts/naslos/values.yaml -f charts/naslos/values-vm.yaml`
(do **not** use `--reuse-values`: it would keep the production keys). State PVC,
pools and the buddy identity are untouched either way. If only the portal path
misbehaves, the narrow fallback is to set `server.path: ""` and drop the
`authelia_url` pin (portal back at the root) while keeping ingress/auth.

## Risks / technical checks for the implementer

- **Traefik chart 39 `hostPort`**: verify by rendering (`helm template … | grep hostPort`).
  Fallback is `traefik.service.type: NodePort` with fixed ports (don't reuse 30080,
  which the UI NodePort owns in dev).
- **Authelia subpath cookie**: docs say subpath + forwardAuth is supported and the
  authz URL must stay at `/api/authz/forward-auth`; the `authelia_url` query pin
  fixes redirects. If login loops, migrate `session` to the modern
  `session.cookies` form with `authelia_url` (or move the portal to a subdomain).
- **`trustForwardHeader: false`**: if Authelia redirects to the wrong host, the
  `authelia_url` pin should already cover it; do not re-enable blanket trust.
- **Self-signed cert**: browsers warn until trusted; `stsPreload` removed so the
  warning can be bypassed. A real cert can be dropped in via
  `ingress.tls.existingSecret` without chart changes.
- **Playwright suite**: it authenticates by sending `Remote-User` directly, so it
  cannot run against the production posture. Keep using the dev profile
  (`values-vm.yaml`) for suite runs and record that the prod cutover was verified
  manually.
- **mDNS `.local`**: Apple/Linux resolve it natively; Windows needs Bonjour or the
  router A record.
- **Single-node downtime** during the upgrade; no HA.

## Out of scope

- The rest of CR-38 (values.schema.json, digest pinning, `.Release.Namespace`,
  loopback LDAPS trust in the init container).
- cert-manager / internal-CA lifecycle, ACME.
- Multi-node, MetalLB, HA.
- Applying the same posture to the `.118` work server.
