# Plan: Provider API-rights info bubble in the Domains interface

## Goal

In the Domains **Add/Edit Domain** form, show an accessible info bubble next to the
"DNS-01 provider" selector that tells the operator which API permissions/rights the
selected provider's credentials need. The text is **declarative** per provider (YAML),
surfaced by `GET /api/providers`, so overlay/custom providers can define it too.

## Decisions (agreed)

- Data source: new declarative provider field (not hardcoded in the UI, not reusing `description`).
- Field shape: a list of short permission strings (`apiRights:`), rendered as bullets.
- Surface: **Domains Add/Edit form only** (`ui/src/lib/components/DomainForm.svelte`).
  Do not change the Domains list rows, and do not touch the DDNS form/UI.
- Bubble: an accessible popover (ⓘ button; shows on hover/focus, togglable by click,
  closes on Escape/blur), not a native `title` tooltip.
- Informational UI only: no new API route, no storage, no behavior change to cert issuance.

## Scope / boundaries

- Cert-capable providers shown in the Domains form are `cloudflare`, `ovh`, `rfc2136`,
  `passthrough` (DomainForm filters `p.certManager`). Only these need `apiRights`.
- DDNS-only providers (`generic`, `duckdns`, etc.) are out of scope for this change.
- No image-tag/release bump, no Helm/chart changes.

## Branch / workflow

1. From a clean tree: `git fetch origin`, `git checkout master`, `git pull --ff-only`.
2. Create branch `feature/domain-provider-api-rights`.
3. Commit and push the branch; open a PR. Never push to `master` (requires explicit user approval).

## Tasks (ordered)

### 1. Backend — add the field to the provider model

`api/internal/providers/providers.go`:
- Add to `Provider` (near `Description`/`Icon`):
  ```go
  // APIRights are the provider API permissions an operator must grant for a
  // domain's DNS-01 solver. Informational; rendered as a UI info bubble.
  APIRights []string `yaml:"apiRights,omitempty" json:"apiRights,omitempty"`
  ```
- Optional: in `validate()`, reject empty-string entries (keep validation minimal).

`api/internal/server/ddns.go`:
- Add `APIRights []string \`json:"apiRights,omitempty"\`` to `providerView`.
- In `handleProviders`, set `APIRights: p.APIRights`.

### 2. Builtin provider YAML — populate `apiRights`

Add an `apiRights:` list to each cert-capable builtin. Content to use (verified against
cert-manager docs / cert-manager-webhook-ovh):

- `api/internal/providers/builtin/cloudflare.yaml`
  ```yaml
  apiRights:
    - "API token permission: Zone → DNS → Edit"
    - "API token permission: Zone → Zone → Read"
    - "Scope the token to the zone(s) hosted here (not All Zones unless intended)"
  ```
- `api/internal/providers/builtin/ovh.yaml` (ZoneDNS API mode used by the cert solver)
  ```yaml
  apiRights:
    - "ZoneDNS API app rights: GET /domain/zone/*"
    - "PUT /domain/zone/*"
    - "POST /domain/zone/*"
    - "DELETE /domain/zone/*"
    - "Restrict \"*\" to the target zone if possible"
  ```
- `api/internal/providers/builtin/rfc2136.yaml`
  ```yaml
  apiRights:
    - "No provider API: a TSIG key name + secret is used"
    - "The nameserver must permit that TSIG key to update the zone (allow-update / update-policy)"
  ```
- `api/internal/providers/builtin/passthrough.yaml`
  ```yaml
  apiRights:
    - "Not applicable: you supply the raw cert-manager DNS-01 solver"
    - "Grant whatever that solver's credentials require"
  ```

### 3. Backend tests

- `api/internal/providers/providers_test.go`: assert the builtins parse with non-empty
  `APIRights` for `cloudflare`, `ovh`, `rfc2136`, `passthrough` (extend `TestLoadBuiltins`),
  and that a YAML doc with `apiRights` round-trips through `Parse`.
- `api/internal/server/ddns_test.go` (`TestProvidersListIncludesBuiltins`): add
  `APIRights []string \`json:"apiRights"\`` to the anonymous payload struct and assert the
  cert-manager providers carry rights.

### 4. Frontend — reusable info bubble component

Create `ui/src/lib/components/InfoBubble.svelte` (match existing legacy Svelte syntax used
around the repo: `export let`, `on:*` handlers; Svelte 5 is in legacy mode):
- Props: `export let items: string[] = [];`, optional `export let label = 'API rights required';`.
- Markup: wrapper `<span class="relative inline-flex">` containing
  - a `<button type="button">` with an ⓘ glyph, `aria-label={label}`,
    `aria-expanded={open}`, `aria-controls={id}`,
  - a conditional bubble `<div id={id} role="tooltip" class="absolute z-10 ... w-72 ...">`
    with a bulleted `<ul>` of `items` (Tailwind theme tokens: `bg-naslos-surface`,
    `border-naslos-border`, `text-gray-300`).
- Interaction: open on `mouseenter`/`focus`, toggle on `click`, close on `mouseleave`,
  `blur`, and `Escape` (`on:keydown`). Type `button` so it never submits.
- Render nothing (or just the button) when `items.length === 0`.

### 5. Frontend — wire it into the Domains form

`ui/src/lib/components/DomainForm.svelte`:
- Import `InfoBubble`.
- Replace the plain `<label>DNS-01 provider</label>` with a header row that keeps the label
  outside the button (interactive content inside `<label>` can hijack the select):
  ```svelte
  <div class="flex items-center gap-2">
    <label class="label" for="domain-provider">DNS-01 provider</label>
    <InfoBubble items={currentProvider?.apiRights ?? []} />
  </div>
  <select id="domain-provider" ...> ... </select>
  ```
- Keep the existing `currentProvider?.description` paragraph (line 125) as-is.
- `currentProvider` is already reactive (line 38), so the bubble updates on provider change.

### 6. Frontend e2e test

`ui/tests/domains.spec.ts`: in the add-domain flow, assert the info button is visible and
that revealing it shows the default provider's rights. Use a robust assertion, e.g.
`page.getByRole('button', { name: 'API rights required' })` then
`expect(page.getByRole('tooltip')).toContainText(/DNS.*Edit/)`. Default cert provider is
`cloudflare` (first by name), so this text is stable.

### 7. Docs & credits

- `docs/dynamic-dns.md`: document the new provider key in the schema bullets, e.g.
  "`apiRights:` is a list of API permissions the operator must grant; it is informational
  and shown in the Domains form's info bubble."
- `docs/api.md`: note that `GET /api/providers` returns `apiRights` per provider.
- `CREDITS.md`: per this repo's convention (credits always kept current, provenance for
  adapted content), add attribution for the rights text derived from cert-manager's DNS-01
  docs and the `cert-manager-webhook-ovh` README (MIT). Update the builtin YAML provenance
  only if it is treated as adapted config.
- Optional (repo convention from prior sessions): refresh `AI_Handoff.md` with the new
  provider field.

## Validation

- API: `cd api && go build ./... && go test ./internal/providers/... ./internal/server/...`
- UI types: `cd ui && npm run check`
- E2E: `cd ui && npm run test:e2e` (requires the running stack; run the domains spec if a
  full run is not available).
- Manual: open Domains → Add Domain, confirm the ⓘ appears next to "DNS-01 provider",
  hovering/focusing/clicking reveals the bullets, and switching providers updates the text.

## Risks / notes

- Keep `apiRights` display-only; do not let it affect solver rendering or validation.
- `omitempty` on both YAML and JSON keeps providers without rights unchanged in the API
  payload and in tests.
- The label/button placement is deliberate to preserve the `<select>` label association.

## Open questions

None blocking — implementation-ready.
