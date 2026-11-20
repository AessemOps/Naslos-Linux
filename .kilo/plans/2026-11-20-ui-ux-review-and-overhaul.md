# UI/UX Review + Targeted Overhaul (Naslos-Linux `ui/`)

Date: 2026-11-20 · Branch: `feature/ui-ux-overhaul` (off `master`) · Owner repo: `Naslos-Linux`

> On execution, copy this plan file into `Naslos-Linux/.kilo/plans/` (same
> filename, `2026-11-20-ui-ux-review-and-overhaul.md`) and commit it there per
> the repo's plan rule; this location is where planning permissions allow
> writing.

## Goal

Run a full UX audit of the Svelte 5 UI (17 routes + shared components), record
a per-route findings scorecard inside this plan, then implement a targeted
overhaul: SVG icon system, sidebar IA + collapsible shell, CSS-variable design
tokens with a persisted **light/dark theme**, unified
modal/loading/empty/error/status components, and a11y fixes — keeping the
current dark visual identity (no rebrand). Audit findings live **inside this
plan file** (appended scorecard); no separate report doc.

## User-confirmed decisions

1. Depth: review + targeted overhaul; no rebrand, no layout redesign beyond the shell work below.
2. Icons: inline SVG `Icon.svelte` with ~24 hand-adapted Lucide paths (ISC), provenance header; **no new runtime npm deps**.
3. Responsive: collapsible sidebar + all routes usable down to ~1024px. Phones out of scope.
4. Light theme: add it — persisted toggle, `prefers-color-scheme` default, no-FOUC bootstrap.
5. A11y enforcement: add `@axe-core/playwright` (dev-only dep) + `tests/a11y.spec.ts` in the live-node drill.
6. Findings recorded in this plan file; priorities driven by the audit (user listed no personal pain points).

## Hard constraints (repo rules)

- **Svelte 5 legacy syntax only**: keep `on:click`, `slot`, `$app/stores`, `$:`; no runes migration.
- **Playwright runs against the live production node** (baseURL `https://naslos.local`, chromium, 1 worker). Specs assert exact loader strings (e.g. `getByText('Loading dashboard...')`) and `.card`/role/text anchors — every copy/markup change updates specs in the same commit.
- **No production identifiers in commits/PRs** (no `naslos.local`, no node IPs).
- `ui/package.json` changes (adding `@axe-core/playwright`) update `CREDITS.md` + its `Last reviewed` line in the same change.
- Preserve spec MUSTs: FR-MET-08 (5s dashboard auto-refresh), FR-MET-09 (visible error). No `docs/spec.md` change expected; if any MUST wording ends up touched, spec + its test change together.
- Never push to `master`; PR only, fetch first. Conventional Commits.

## Phase 0 — Audit (read-only; produces the scorecard)

Walk every route (`/`, `/pools`, `/pools/[name]`, `/datasets`, `/disks`,
`/disks/health`, `/shares`, `/users`, `/groups`, `/apps`, `/backups`, `/vms`,
`/terminal`, `/domains`, `/dns`, `/notifications`, `/settings`,
`/settings/network`, `/system`) and the shared components. Score each against:
a11y (contrast both themes, focus order, labels, keyboard nav, hit areas),
loading/empty/error states, consistency (tokens/spacing/status colors), nav/IA,
form UX (labels, inline errors, submit states), and responsive behavior at
1280/1024px. Append findings to this file as a severity-ordered table
(`file:line`, finding, fix). Optional grounding: the ui-ux-pro-max skill CLI
(`python3 ~/.agents/skills/ui-ux-pro-max/scripts/search.py "admin dashboard
dark professional" --design-system`, `--domain ux`, `--stack svelte`).

### Pre-audited findings (verified in code — fold into the scorecard)

| # | Evidence | Finding | Fix |
| --- | --- | --- | --- |
| 1 | `Sidebar.svelte:12-29`, `settings/+page.svelte:3-16` | Emoji icons throughout (16 nav items + 14 settings-hub cards) | `Icon.svelte` SVG set |
| 2 | `Sidebar.svelte:106` | Active state is exact `pathname === item.path`; detail routes (`/pools/tank`, `/settings/network`) lose highlight | Longest-prefix match |
| 3 | `Sidebar.svelte`, `+layout.svelte:10` | Flat 16-item nav, no grouping, no collapse; layout hard-codes `ml-64` | Grouped + collapsible sidebar, shared store |
| 4 | `settings/+page.svelte` | Settings is a duplicate 14-link nav hub, not settings; no home for a theme toggle | Add "Appearance" section (theme); keep hub as directory with clearer copy |
| 5 | 29 matches across routes/components | Text-only loaders, inconsistent copy (`Loading pools...` vs `Loading…`) | Shared `LoadingState` (skeleton + `aria-busy` + one copy pattern); specs updated in lockstep |
| 6 | `pools/+page.svelte:149`, `pools/[name]/+page.svelte:394,444`, `settings/network/+page.svelte:722` | Raw modals without `use:modal` (no focus trap/Escape/aria) while 13 others use it | Shared `Modal.svelte` wrapping `use:modal` |
| 7 | `tailwind.config.js`, `app.css` | 6 ad-hoc colors; status colors inline everywhere (`bg-green-900/50 text-green-400` ×many); dashboard stat colors arbitrary (green/blue/purple); dead `btn-sm` class referenced (`pools/+page.svelte:173`) but never defined | CSS-var token layer + semantic status classes + `.btn-sm` |
| 8 | `Dashboard.svelte:101-111` vs `lib/format.ts` | Local `formatBytes` (KB/MB) duplicates and contradicts shared (KiB/MiB) | Deduplicate into `lib/format.ts` (check `dashboard.spec.ts:25` `0 B / 0 B` assertion) |
| 9 | `app.html` | No theme bootstrap (FOUC risk for light theme) | Inline pre-paint script setting `data-theme` |
| 10 | `InfoBubble.svelte:54-56` | `group-hover:block` fights the JS `open` state; hover/focus open mismatch | Drive visibility purely from `open` state |
| 11 | `Dashboard.svelte:255-259` and similar | Sub-40px hit areas (`px-2 py-0.5` delete buttons, `×` closers) | Min 40px interactive targets |
| 12 | `app.css` | No `prefers-reduced-motion` handling; no transition duration conventions | Tokenized 150-300ms + reduced-motion rule |

## Phase 1 — Tokens + theme foundation

1. `tailwind.config.js`: `naslos.*` colors become CSS variables with
   `<alpha-value>` (e.g. `dark: 'rgb(var(--n-dark) / <alpha-value>)'` so
   existing `bg-green-900/50`-style opacities keep working); add semantic
   `success/warning/danger/info` tokens; define the light palette (slate-based,
   4.5:1 minimum body text — muted text ≥ slate-600 in light).
2. `app.css`: theme variables under `:root` (dark default) and
   `[data-theme='light']`; extend components: `.btn-sm`, `.badge-success` etc.,
   `.skeleton`, status utilities; `@media (prefers-reduced-motion: reduce)`
   rule; keep the `:focus-visible` ring token-based.
3. `app.html`: inline pre-paint script — `localStorage['naslos:theme']` else
   `prefers-color-scheme` → `document.documentElement.dataset.theme`. No CSP
   headers exist in `ui/nginx.conf`, so inline is safe; re-verify after deploy.
4. `ThemeToggle.svelte` (sun/moon icons, `aria-pressed`), mounted in the
   sidebar footer; "Appearance" card on `/settings`.

## Phase 2 — Icon system

1. `src/lib/components/Icon.svelte`: `export let name/size/label`; 24×24
   stroke-based `currentColor` paths, `aria-hidden` default, `aria-label` when
   standalone. Lucide (ISC) provenance header in the file.
2. Replace emojis: sidebar (16), settings hub (14), page decorations
   (grep for emoji ranges across `src/` to catch all).
3. `CREDITS.md`: Lucide attribution note + `Last reviewed` bump (adapted
   assets, no new dependency).

## Phase 3 — Shell + navigation IA

1. Sidebar: grouped sections — Overview (Dashboard, Notifications); Storage
   (Pools, Datasets, Disks, Disk Health); Access (Users, Groups, Shares);
   Workloads (Apps, Virtual Machines); Networking (Domains, Dynamic DNS);
   System (Backups, Terminal, Settings). Groups are a proposal; adjust if the
   audit says so. Collapse toggle (chevron, `aria-expanded`,
   `aria-controls`), persisted `naslos:sidebar`, width transition 200ms.
2. Active state: longest-prefix match so detail routes highlight their parent.
3. `+layout.svelte`: sidebar width from a store (no hardcoded `ml-64`);
   content reflow at ~1024px.
4. `PageHeader.svelte` (title, subtitle, actions slot) adopted on all routes.
5. Audit `md:`/`lg:` grid breakpoints so 4-col stat grids reflow cleanly at
   1024px.

## Phase 4 — Shared interaction components

1. `Modal.svelte`: wraps `use:modal` + backdrop-click + close button + heading
   slot; adopt in the 5 raw modals first, then the rest (keep each caller's
   API minimal).
2. `LoadingState.svelte`: skeleton block(s) + `role="status"` +
   `aria-busy="true"` on container; `EmptyState.svelte` (icon, title, CTA
   slot); `ErrorState.svelte` (`role="alert"` + retry); `StatusBadge.svelte`
   for pool/disk/app health (ONLINE/DEGRADED/OFFLINE…).
3. Buttons: min 40px hit area, spinner-in-button for in-flight submits,
   `cursor-pointer` audit.
4. Grep `tests/` for every loader/copy string touched and update specs in the
   same commit; prefer role/`data-testid` anchors going forward.

## Phase 5 — Route-by-route application

Apply Phase 1/2/4 primitives across all 17 routes per the Phase 0 scorecard
(severity order), including: deduplicated formatters (#8), consistent status
colors, form label/error patterns, per-route a11y fixes. Keep all `data-testid`
anchors (`cpu-details`, `cpu-core`, `cpu-temp-unavailable`, …) stable.

## Phase 6 — Verification gates (in order)

1. `cd ui && npm run check && npm run build`.
2. Add `@axe-core/playwright` (pinned devDep; `CREDITS.md` + Last reviewed in
   the same commit) and `tests/a11y.spec.ts`: AxeBuilder scan per route,
   tags `wcag2a/wcag2aa/wcag21a/wcag21aa`, failing on violations; document any
   deliberate `disable` entries inline.
3. Live-node drill (existing procedure — see `AI_Handoff.md`): the suite runs
   against the deployed node, so deploy the feature build first — `release`
   workflow with `publish=false` pushes only `:sha-<commit>` tags to GHCR,
   override the UI image tag in `values-vm.yaml` (bump the suffix per repo
   rule), `KUBECONFIG=bootstrap/vm/kubeconfig make install-vm`, then
   `./node_modules/.bin/playwright test` (full suite incl. `a11y.spec.ts`).
   Revert the tag override when the release lands.
4. Manual drill: keyboard-only pass (sidebar collapse, modals, forms, drawer),
   both themes at 1280px and 1024px, `curl -sk https://naslos.local/api/health`.
5. `sh scripts/audit.sh` from the repo root (it gates the push).
6. Docs in the same PR: `README.md` (feature list: light theme, collapsible
   sidebar), `AI_Handoff.md` (deployed state/tags/revision + drill results),
   `CREDITS.md` (Lucide adaptation + @axe-core/playwright).

## Risks

- **Playwright selector churn** — mitigated by lockstep spec updates + full
  drill before the PR; single loader pattern minimizes string assertions.
- **Light-theme contrast regressions** — mitigated by CSS-var tokens (audit is
  mechanical) + axe scan + manual dual-theme pass.
- **Legacy-syntax pitfalls** (accidental runes) — `svelte-check` gate; no
  `svelte:*` conversion.
- **17-route scope creep** — scorecard severity ordering; phones explicitly
  out of scope.
- **devDep supply chain** — pinned version, dev-only, never in the shipped
  image.

## Out of scope

Phone/mobile layouts, visual rebrand (palette/typography redesign), dashboard
recomposition, migrating to Svelte runes, any API/chart behavior change
(image tag override for the drill excepted, reverted at release).

## Open questions

None blocking. The Phase 0 scorecard may append per-route tasks; sidebar group
composition is adjustable during implementation.
