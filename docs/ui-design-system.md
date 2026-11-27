# UI design system

The web UI is Svelte 5 with **legacy syntax** (`export let`, `$:`, `on:`,
slots) and Tailwind v4 (JS config via `@config`). This document is the reference
for new pages and for the work packages that migrate the remaining pages. The
normative product requirements stay in [spec.md](spec.md); this file explains
how to build a screen that looks and behaves like the rest of the console.

## Principles

- **Data-dense technical console.** Small type, clear rows, compact controls,
  and no decorative chrome that pushes data off-screen.
- **Dark first.** Dark is the default theme; light is a persisted preference.
  Both themes are first-class and must pass WCAG 2.1 AA.
- **Theme through tokens.** Never hard-code a Tailwind shade that is not mapped
  in `ui/tailwind.config.js`. An unmapped shade keeps its fixed Tailwind value
  and stops flipping with `data-theme`.
- **One job per component.** Pages compose the shared components instead of
  re-implementing headers, dialogs, form labels, tables, loading/error/empty
  states, or confirmations.

## Tokens and theme

`ui/src/app.css` defines every token. Values are `R G B` triplets so Tailwind
can apply opacity modifiers (`bg-red-900/40`).

| Token | Role | Dark | Light |
| --- | --- | --- | --- |
| `--n-primary` | filled primary action (AA with white text) | `#2563EB` | `#2563EB` |
| `--n-secondary` | primary hover | `#1D4ED8` | `#1D4ED8` |
| `--n-accent` | focus ring, active nav accent | `#3B82F6` | `#2563EB` |
| `--n-dark` | page background / inset fields | `#0B1120` | `#F8FAFC` |
| `--n-surface` | cards, sidebar, modal | `#0F172A` | `#FFFFFF` |
| `--n-surface-raised` | table header, inset rows | `#111827` | `#F8FAFC` |
| `--n-border` | 1px separators / cards | `#1E293B` | `#E2E8F0` |
| `--n-border-strong` | form controls, hover borders | `#334155` | `#CBD5E1` |
| `--n-strong` | strongest text on a theme surface | white | `#0F172A` |
| `--n-link` | link / accent text | `#60A5FA` | `#1D4ED8` |

Semantic surface variables (`--n-ok-bg`, `--n-ok-text`, `--n-warn-*`,
`--n-bad-*`, `--n-info-*`, `--n-neutral-*`) power badges and callouts. Body
text uses the `gray-100…500` scale; 600/800/900 are background/border roles.

`color-scheme` is set per theme, so native selects, date pickers and scrollbars
render in the active theme.

### Fonts

- **Fira Sans** (400/500/600/700) for UI text: `font-sans`.
- **Fira Code** (400/500) for identifiers, paths, IPs and sizes: `font-mono`.
- Both are self-hosted from `@fontsource/*`; the app never requests a font CDN.
  The chart CSP allows `font-src 'self' data:`. Body line-height is 1.5.

### Tailwind color mapping

`ui/tailwind.config.js` maps only the shades the UI actually uses:
`gray` 100–600/800/900, `green` 200/300/400/500/700/800/900, `red`
200–900/950, `yellow` 300/400/500/700/900, `amber` 100/200/300/400/700/800/900/950,
`blue` 300/400/900, `purple` 300/400/900, `emerald` 200/300/700/900. To add a
new shade, add the variable to **both** theme blocks in `app.css` and list it in
the config.

## Shell

- `>= lg` (1024 px): fixed sidebar, persisted expanded/collapsed state
  (`lg:w-64` / `lg:w-16`), content offset by `lg:pl-64` / `lg:pl-16`.
- `< lg`: sticky `h-14` top bar with the hamburger (`aria-label="Open
  navigation"`, `aria-expanded`, `aria-controls="sidebar-nav"`). The sidebar is
  an off-canvas drawer with a backdrop; Escape, backdrop click and navigation
  close it, and focus returns to the hamburger.
- `main#main-content` has `tabindex="-1"`; client-side navigation focuses it.
  A "Skip to main content" link is the first focusable element.
- The sidebar wordmark is **not** a heading. Each page owns exactly one `<h1>`.

### z-index scale

| Layer | z-index | Notes |
| --- | --- | --- |
| Sidebar (desktop) | 30 | `z-30` |
| Mobile backdrop / drawer | 40 | `z-40` |
| Modal | 50 | `z-50`, overlay keeps `.fixed.inset-0` |
| Toast | 60 | `z-[60]` |
| Skip link | 70 | only while focused |

## Spacing and shape

- Page padding: `p-4 sm:p-6 lg:p-8`; page header bottom margin `mb-8`.
- Cards: `rounded-xl`, `p-6`, 1px `--n-border`, subtle shadow.
- Buttons/inputs: `rounded-lg`, minimum 40 px high; `.btn-sm` is 36 px.
- Icon-only controls: `.icon-btn` keeps a 40×40 hit area.
- Transitions are 150–250 ms and respect `prefers-reduced-motion`.

## Components

All components use legacy Svelte syntax and typed props. Import paths are under
`$lib/`.

### `PageHeader.svelte`

Props: `title: string`, `subtitle?: string`, `breadcrumbs?: {label: string;
href?: string}[]`. Slots: `actions`. Sets
`<title>{title} · Naslos</title>`. Renders the page's single `<h1>`.

```svelte
<PageHeader title="Pools" subtitle="Monitor and manage your ZFS pools."
  breadcrumbs={[{ label: 'Storage' }, { label: 'Pools' }]}>
  <div slot="actions"><Button variant="secondary">Import Pool</Button></div>
</PageHeader>
```

### `Button.svelte`

Props: `variant?: 'primary' | 'secondary' | 'danger' | 'ghost'` (default
`primary`), `size?: 'sm' | 'md' | 'icon'` (default `md`), `type?: 'button' |
'submit' | 'reset'` (default `button`), `loading?: boolean`, `disabled?:
boolean`, `href?: string`. Forwards `click`. Renders `<a>` when `href` is set.
Extra attributes pass through. While loading it renders a spinner, sets
`aria-busy="true"` and is disabled.

### `FormField.svelte`

Props: `label: string`, `id: string`, `required?: boolean`, `hint?: string`,
`error?: string`. Default slot: the control. The label text is exactly
`{label}` plus ` *` when required (so `getByLabel('Name *')` works). Hint id is
`${id}-hint`; error id is `${id}-error` and carries `role="alert"`. Wire
`aria-describedby` on the control when both are present.

### `DataTable.svelte`

No props; default slot. Wraps a real `<table>` in a horizontal-scroll frame
with rounded borders, muted uppercase headers and hover rows (`.data-table` in
`app.css`).

### `AsyncState.svelte`

Props: `loading?: boolean`, `error?: string`, `empty?: boolean`,
`loadingText?: string` (default `Loading…`), `emptyTitle?: string`,
`emptyDetail?: string`, `emptyIcon?: string`, `onRetry?: (() => void) | null`.
States are mutually exclusive in this order: loading → error → empty → default
slot. `LoadingState` keeps `data-testid="page-loading"` and renders the exact
string passed.

### `LoadingState.svelte`

Prop: `label?: string` (default `Loading…`). `role="status"`,
`aria-busy="true"`, `data-testid="page-loading"`.

### `ErrorState.svelte`

Props: `message?: string`, `retry?: (() => void) | null`. `role="alert"` and a
Retry button when `retry` is set.

### `EmptyState.svelte`

Props: `icon?: string`, `title?: string`, `detail?: string`. Slots: `action`
(preferred) or default. The card copy is caller-owned.

### `Skeleton.svelte`

Props: `width?: string` (default `w-full`), `height?: string` (`h-4`),
`rounded?: string` (`rounded-md`), `label?: string`. The pulse collapses under
`prefers-reduced-motion`.

### `Modal.svelte`

Props: `open?: boolean` (default `true`), `title: string`, `onClose: () =>
void`, `size?: 'sm' | 'md' | 'lg' | 'xl'`, `wide?: boolean` (legacy alias for
`size="lg"`). Slots: default body, `footer` (right-aligned; full-width stacked
below `sm`). The overlay keeps `.fixed.inset-0` and `z-50`. `use:modal` applies
`role="dialog"`, `aria-modal`, `aria-labelledby` from the title, focus trapping,
Escape, focus restoration, body scroll lock and `inert` on the background.
Below `sm` the dialog is a full-screen sheet.

### `ConfirmDialog.svelte` + `$lib/stores/confirm`

`confirmAction(options): Promise<boolean>` replaces `window.confirm()`.
Options: `title`, `message`, `confirmLabel?`, `cancelLabel?`, `tone?: 'danger' |
'primary'`, `typedConfirmation?: string`. With `typedConfirmation`, the confirm
button stays disabled until the exact text is typed. `ConfirmDialog` is mounted
once in `+layout.svelte`; pages only await the promise. Store exports:
`confirmAction`, `resolveConfirm`, `confirmState`, types `ConfirmOptions`,
`ConfirmRequest`, `ConfirmTone`.

```svelte
if (!(await confirmAction({
  title: 'Delete share',
  message: `Delete “${name}”? The dataset is kept.`,
  tone: 'danger'
}))) return;
```

### `Toast.svelte`

Renders `$appToasts` from `$lib/stores/appJobs`. Informational toasts use
`role="status"`, error toasts `role="alert"`. Width is
`min(20rem, calc(100vw - 2rem))`.

### `AppJobsDrawer.svelte`

The app jobs drawer shows a role `progressbar` with an indeterminate animation.
The API exposes no numeric percentage, so **never** add `aria-valuenow` or a
fake width.

### `StatusBadge.svelte`

Props: `state?: string`, `label?: string`, `tone?: 'ok' | 'warn' | 'bad' |
'info' | 'neutral' | ''`, `unknownTone?: Tone`. The state → tone table is in
`$lib/status.ts` (`toneFor(value, fallback)`), the single mapping. Use it for
non-badge surfaces too.

### `Icon.svelte`

Props: `name: string`, `size?: number` (20), `label?: string`. Icons are
inline Lucide-style paths; use `label` only when the icon conveys information
without adjacent text. No emoji icons.

## Helpers and stores

| Module | Exports |
| --- | --- |
| `$lib/api.ts` | `api<T>(path, init?): Promise<T>`, `ApiError { status, message }`. Throws `ApiError` for non-2xx, decodes JSON, supports `signal`. |
| `$lib/format.ts` | `formatBytes`, `formatBytesPerSec`, `formatMillicores`, `formatUsage`, `fmtTime(ts)`, `fmtRelative(ts)` |
| `$lib/status.ts` | `Tone`, `toneFor(value, fallback?)` |
| `$lib/stores/sidebar.ts` | `sidebarCollapsed`, `toggleSidebar`, `mobileNavOpen`, `openMobileNav`, `closeMobileNav`, `toggleMobileNav` |
| `$lib/stores/confirm.ts` | `confirmAction`, `resolveConfirm`, `confirmState` |
| `$lib/stores/systemUpdates.ts` | `systemUpdates`, `refreshSystemUpdates()`, `SystemUpdates` |
| `$lib/actions/modal.ts` | `modal` action, `lockBodyScroll`, `unlockBodyScroll` |
| `$lib/actions/drawer.ts` | `mobileDrawer` action (mobile navigation focus trap) |

## State patterns

1. Load with `loading = true`, clear the previous error, `await api(...)`, then
   render data.
2. Render `<AsyncState loading={loading} error={error} empty={items.length === 0}
   loadingText="Loading users..." onRetry={load}>` around the real content.
3. Report mutations with a Toast. Do not use `alert()`.
4. Confirm destructive actions with `confirmAction()`; do not use
   `window.confirm()`. Use `typedConfirmation` for irreversible deletes when
   the name matters.
5. Keep background jobs in the existing job stores; never fake progress.

## Forms

- Every control has a visible `<label>` bound with `for`/`id`; no
  placeholder-only labels.
- Use a real `<form on:submit|preventDefault={submit}>` and a submit button
  (`type="submit"`) so Enter works and password managers recognize the form.
- Wrap controls in `FormField`; required fields get `required` and the browser's
  native validation where appropriate. Show server errors in the field's
  `error` prop and in a page-level `role="alert"` for non-field failures.
- Mark buttons that only close a dialog as `type="button"`.

## Tables

- Use `DataTable` with a real `<table>`, `<thead>`, `<tbody>` and `<th
  scope="col">`.
- Numbers and sizes are right-aligned and use `font-mono`; status columns use
  `StatusBadge`.
- Empty results render `EmptyState` (or `AsyncState empty`), not a bare header
  row.

## Breakpoints

Tailwind defaults: `sm` 640, `md` 768, `lg` 1024, `xl` 1280, `2xl` 1536.
The shell switches at `lg`; dialogs switch from sheet to centered at `sm`;
cards commonly go 1 → 2 → 3 columns at `sm`/`lg`.

## Accessibility checklist

- One `<h1>` per route; headings descend without skipping levels.
- Skip link is first; `main#main-content` is the focus target after navigation.
- All interactive elements show the 2px accent `:focus-visible` outline.
- Icon-only buttons have `aria-label`; the hamburger also sets
  `aria-expanded`/`aria-controls`.
- Dialogs trap focus, close on Escape, restore focus, lock body scroll and
  inert the background. Nested dialogs close top-down.
- `aria-current="page"` marks the active route; the active style is not
  color-only (left accent bar).
- Errors use `role="alert"`; loading uses `role="status"` + `aria-busy`.
- Text meets 4.5:1 and non-text controls 3:1 in both themes; status is never
  conveyed by color alone.
- Motion respects `prefers-reduced-motion`; transitions stay 150–250 ms.
