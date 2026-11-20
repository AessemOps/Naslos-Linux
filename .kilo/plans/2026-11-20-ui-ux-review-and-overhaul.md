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

---

## Phase 0 scorecard (audit results, 2026-11-20)

Severity-ordered. `P0` = a11y/functional defect, `P1` = consistency/UX debt,
`P2` = polish. The pre-audited findings #1–#12 above are confirmed and folded
into this table; line numbers re-verified on the audit pass.

### Token/theme design decisions (drives Phase 1)

The codebase has 460 `text-gray-*` uses, 78 inline status-color combos, and
~37 `text-white`/`hover:text-white` uses. Rewriting every call site is not
reviewable in one PR, so the theme layer is **CSS-variable palette remapping**:

1. Tailwind `naslos.*` colors become `rgb(var(--n-*) / <alpha-value>)` so every
   `bg-green-900/50`-style opacity keeps working.
2. The **used** shades of `gray/green/red/yellow/amber/blue/purple/emerald`
   remap to per-theme vars (dark = today's exact values, light = ≥4.5:1 ink
   variants for text shades, light tints for the 900-shade badge backgrounds).
   Usage was tallied first: `text-gray` uses slots 100–600 only,
   `bg-gray`/`border-gray` use 600/800/900 only (one `bg-gray-600` step dot),
   so slot roles never conflict except `gray-600` (4 text uses → retargeted to
   `text-gray-500` in Phase 5). The 460 existing muted-text call sites become
   light-readable with zero churn; status colors become light-readable the same
   way. Vivid button colors (`bg-red-600/700`, `bg-green-500`) stay fixed.
3. A new `naslos.strong` token (white in dark, near-black ink in light) replaces
   `text-white`/`hover:text-white` where the background is a theme surface
   (modal `×`, copy buttons, Toast dismiss, dashboard modal emphasis).
   `text-white` on colored buttons stays literal.
4. Dark theme keeps the exact current palette (no visual rebrand); the light
   palette is slate-based with white cards on `slate-50` page bg.

### Scorecard

| # | Sev | Evidence | Finding | Fix |
| --- | --- | --- | --- | --- |
| 1 | P0 | `pools/+page.svelte:149`, `pools/[name]/+page.svelte:394,444`, `settings/network/+page.svelte:722`, `system/+page.svelte:564,594`, `VmWizard.svelte:112`, `VmDetail.svelte:115` | 7 raw modals/drawers: no focus trap, no Escape, no `role=dialog` (system/network ones have no backdrop close either) | `Modal.svelte` wrapper (`use:modal` + backdrop self-click + 40px close button + heading); adopt in the 5 centered dialogs; VmWizard/VmDetail drawer get `use:modal` in place |
| 2 | P0 | `pools/+page.svelte:150`, `Dashboard.svelte:283` | `bg-naslos-card` class does not exist in the palette → import/delete modal panels render transparent over the backdrop | use `bg-naslos-surface` via `Modal.svelte` |
| 3 | P0 | `Sidebar.svelte:106` | Active state exact-match only; `/pools/tank`, `/settings/network` lose highlight | longest-prefix match |
| 4 | P0 | `app.html` | No theme bootstrap; a light preference would FOUC | inline pre-paint script → `data-theme` |
| 5 | P0 | `InfoBubble.svelte:54-56` | `group-hover:block` bypasses the `open` state → `aria-expanded` lies; hover and keyboard paths disagree | visibility purely from `open`; 24px trigger |
| 6 | P1 | 460 uses, worst: `Sidebar.svelte:96,140,155`, stat captions everywhere | `text-gray-500/600` captions fail 4.5:1 even in dark (≈3.7:1 on `#0f172a`); everything fails in light | var-remapped palette (dark keeps look but 500 maps to a passing slate; see decisions) |
| 7 | P1 | `Sidebar.svelte:12-29`, `settings/+page.svelte:3-16`, empty states ×7, `shares/+page.svelte:96-101`, `AppIcon.svelte:23` | Emoji as icons (cross-platform rendering varies, no size/weight control) | `Icon.svelte` Lucide set |
| 8 | P1 | `+layout.svelte:10` | Hardcoded `ml-64`; no collapse; 16-item flat nav | grouped collapsible sidebar + shared store + width var |
| 9 | P1 | 37 loader strings, 14 asserted in specs (`dashboard…`, `shares…`, `users…`, `system state…`, `network inventory…`, `domains…`, `dynamic DNS…`, `disk health…`, `backups…`, `virtual machines…`, `catalog…`, `installed apps…`, `sources…`, `pools/groups/health data`) | Inconsistent loader markup and copy (`...` vs `…` vs none), no `aria-busy` | shared `LoadingState` (`role=status`, `aria-busy`, `data-testid=page-loading`); **per-page label text kept identical** in this PR so all 14 spec assertions stay green (standardising copy would churn 14 specs for zero user value; component enforces the pattern) |
| 10 | P1 | 78 inline status combos, `appStatus.ts`, `pools/[name]:239-247`, `disks/health:145-156`, `vms:184-190`, `dns:81-87` | 6 divergent state→badge mappings; light-unreadable | `StatusBadge` + `.badge-*` theme classes |
| 11 | P1 | `Dashboard.svelte:101-111` vs `lib/format.ts` | Local KB/MB formatter contradicts shared KiB/MiB; `formatBytes(0)`='0 B' both ways so `dashboard.spec.ts:25` unaffected | delete local, import shared |
| 12 | P1 | `pools/+page.svelte:173`, `pools/[name]/+page.svelte:326` | `btn-sm` referenced, never defined | define `.btn-sm` |
| 13 | P1 | `Dashboard.svelte:255-259`, `pools/[name]/+page.svelte:344-348`, modal `×` ×13, `Toast.svelte:16`, `AppJobsDrawer.svelte:35` | Sub-40px hit areas on destructive/closing controls | 40px targets via `.icon-btn` class |
| 14 | P1 | `app.css` | No `prefers-reduced-motion`; hardcoded `#3b82f6` ring | reduce-motion rule + token ring |
| 15 | P1 | `settings/+page.svelte` | 14-card directory duplicates nav; no theme home | "Appearance" panel (theme toggle, non-navigating) above the grid + copy fixes |
| 16 | P1 | `Sidebar.svelte:156` | Version line `text-gray-600` ≈2.2:1 — unreadable | token text |
| 17 | P2 | `backups/+page.svelte:101-108`, `disks/health:170-180` | Two more local byte formatters (KB/MB) | shared `formatBytes` |
| 18 | P2 | `Dashboard.svelte:168-183` | Stat colors arbitrary green/blue/purple (light-unreadable as-is) | var-remapped families keep hues readable both themes |
| 19 | P2 | `users/+page.svelte:127`, `groups`, `shares`, `domains`, `dns` | Empty states use emoji + inconsistent card markup | `EmptyState` (copy preserved where specs assert it) |
| 20 | P2 | `AppJobsDrawer.svelte:37,45` | Text `▾/▸` toggle; state badge not tokenized | chevron icons + StatusBadge |
| 21 | P2 | `system/+page.svelte:365-373` | Message banner uses `class:border-*` only — no tone text/bg, invisible distinction | `.callout-*` classes |
| 22 | P2 | `pools/[name]/+page.svelte:133,138-143` | Dataset delete uses native `alert()`/`confirm()` (jarring, unstyled, untestable without dialog handlers) | keep `confirm()` this PR (specs accept dialogs); recorded as follow-up for a `ConfirmModal` |
| 23 | P2 | `terminal/+page.svelte:227-231` | xterm theme hardcoded dark colors (fine: terminal panes stay dark in light theme — deliberate) | keep; document |
| 24 | P2 | `shares/+page.svelte:96-103` | Protocol icons 🪟/🐧/🍎 (AFP unreachable in form anyway) | Lucide `monitor`/`server` icons via `Icon` |

### Spec-locked anchors (must not change)

Verified from all 19 specs — every one of these is load-bearing and Phase 3–5
work preserves them: `<aside>` + `img[alt="Naslos logo"]` + heading `Naslos`
(`logo.spec.ts`); `.card` class; `.fixed.inset-0` modal backdrops; `div.bg-naslos-dark`
dataset rows; h1 headings per page; `+ New User/Group/Share`, `+ Add Domain/Entry/Source`
labels; `Install {name}` / `Update all (n)` / step buttons in the app install
wizard; `cpu-details`, `cpu-core`, `cpu-temp-unavailable` testids; loader
strings (see #9); `Select` order on `/terminal` (namespace, pod, container);
`getByLabel('Addressing'/'Addresses'/'Gateway'/'MTU (optional)')` on network;
InfoBubble `aria-label="API rights required"` + `role="tooltip"` with
`DNS.*Edit` content; `sign out` role button; sidebar dots' `role="status"`.

### Deliberate deviations from the original plan text

1. **Loader copy stays per-page** (see #9) instead of one global string —
   14 green spec assertions stay untouched; the shared component still
   enforces one markup/aria pattern.
2. **`use:modal` callers are not converted** to `Modal.svelte` in this PR
   (they are already a11y-complete); conversion would be pure churn across 13
   components. `Modal.svelte` is adopted by the raw modals and new code.
3. **Native `confirm()` flows stay** (see #22): `e2e.spec.ts`,
   `groups.spec.ts`, `backups.spec.ts` accept native dialogs; replacing them
   would require rewriting their dialog plumbing. Follow-up task recorded.
4. **Icon count is ~34, not ~24** (nav 16 + Traefik + shell/theme 6 + states 9
   + settings-hub specific 6) to cover the audited surfaces.
