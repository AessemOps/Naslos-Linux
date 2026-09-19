# Playwright suite against the production posture (Authelia + TOTP)

Goal: `npx playwright test` passes against `https://naslos.local` (Traefik +
Authelia, admin account with 2FA) as well as the dev NodePort posture. Today the
suite only works on the dev posture: it reaches the NodePort where auth is off,
relies on nginx's 403 for the terminal assertions, and injects `Remote-User` by
hand — none of which exist behind the proxy.

Baseline: `feature/authelia-traefik-production` @ `bd7ca71`, live at helm
revision 13. Dev baseline: 30 passed / 2 skipped (terminal interactive tests
skipped because the NodePort refuses `/api/pods`).

## Approach

Log in to the Authelia portal once in a Playwright **setup project** and reuse
the session cookie via `storageState` for every test. Playwright's built-in
`request` fixture shares the browser context's cookies, so both `page.goto` and
`request.get('/api/…')` are authenticated by the same state. The setup detects
the posture: if `/` does not redirect to `/authelia/`, there is no auth in the
path (dev), and it writes an empty state so the suite behaves exactly as today.

Authelia's login DOM ids (from its own Rod e2e suite, `internal/suites/action_login.go`):
`#username-textfield`, `#password-textfield`, `#sign-in-button`,
`#remember-checkbox`. Second factor uses `#one-time-code-textfield` (the
implementer verifies and adjusts against the live portal during the run).

## Credentials (never committed, never pasted in chat)

The suite reads these from the environment, and the config loads
`ui/.env.playwright.local` if it exists:

| Var | Meaning |
|---|---|
| `NASLOS_ADMIN_USER` | default `admin` |
| `NASLOS_ADMIN_PASSWORD` | required for the prod posture |
| `NASLOS_TOTP_SECRET` | base32 shared secret; lets every run mint a code (preferred) |
| `NASLOS_TOTP_CODE` | alternative one-off current code; a run is then not repeatable |

The user creates the file (mode 600) themselves. If only a one-off code is
available, the suite still works for a single run but cannot be automated
afterwards — resolved with the user before the production run.

## Changes

### 1. `ui/playwright.config.ts`
- `use.ignoreHTTPSErrors: true` (the chart cert is self-signed).
- Keep `baseURL: process.env.PLAYWRIGHT_BASE_URL ?? 'http://192.168.1.96:30080'`.
- Add a `setup` project (`testMatch: /auth\.setup\.ts/`) and make `chromium`
  `dependencies: ['setup']` with `use.storageState: 'tests/.auth/admin.json'`.
- Load `ui/.env.playwright.local` at the top with a ~10-line parser (no new
  dependency; `dotenv` is not in the tree).

### 2. New `ui/tests/auth.setup.ts`
```
test('authenticate', async ({ page, context }) => {
  const state = 'tests/.auth/admin.json';
  if (!process.env.NASLOS_ADMIN_PASSWORD) { await writeEmptyState(state); return; }

  await page.goto('/');
  if (!page.url().includes('/authelia/')) { await writeEmptyState(state); return; } // dev posture

  await page.locator('#username-textfield').fill(user);
  await page.locator('#password-textfield').fill(pass);
  await page.locator('#sign-in-button').click();

  // Second factor: TOTP input (or a method chooser -> One-Time Password).
  if (await page.locator('#one-time-code-textfield').isVisible().catch(() => false)) {
    await page.locator('#one-time-code-textfield').fill(code());
    await page.locator('#sign-in-button').click();
  }

  await page.waitForURL((u) => !u.pathname.includes('/authelia'), { timeout: 30_000 });
  await context.storageState({ path: state });

  // Fail fast and clearly if the session is not an administrator: otherwise
  // every later test 403s and the cause is buried.
  const me = await page.request.get('/api/auth/me');
  expect(me.status()).toBe(200);
  expect(String((await me.json()).groups)).toContain('naslos_admins');
});
```
- Authelia regulation bans after 3 failures (2 min window / 5 min ban), so a bad
  password/code must abort with a readable message, not retry blindly.

### 3. New `ui/tests/totp.ts`
- RFC 6238 TOTP with `node:crypto` (`createHmac('sha1', key)`, 30 s, 6 digits,
  base32 decode). ~25 lines, no new dependency (the repo's review flags adding
  packages). Unit-check it against `oathtool`/a known vector before relying on it.

### 4. `ui/tests/terminal.spec.ts` — posture-aware refusal assertion
The first test proves the terminal is unreachable without a session. Through the
NodePort that is a 403 with nginx's `authenticated` text; through Traefik it is a
302 to the portal. Rework it to use an **anonymous** context and accept either:
```ts
const anon = await request.newContext({
  baseURL: test.info().project.use.baseURL as string,
  ignoreHTTPSErrors: true,
});
for (const path of TERMINAL_PATHS) {
  const res = await anon.get(path);
  expect(res.status(), `${path} answered ${res.status()}`).not.toBe(200);
  expect([302, 401, 403]).toContain(res.status());
}
await anon.dispose();
```
Drop the `toContain('authenticated')` assertion (dev-nginx wording), or keep it
only in the 403 branch. `terminalReachable()` is unchanged; once authenticated it
returns 200, so the interactive tests now run in production instead of skipping.

### 5. `ui/tests/backups.spec.ts`
- Remove `test.use({ extraHTTPHeaders: { 'Remote-User': 'admin' } })` (line 14).
  The session cookie is the identity now, and `trustForwardHeader` is off, so
  Traefik overwrites any such header. It was a no-op in dev too (AUTH_DISABLED
  passes through), so removing it keeps the dev baseline.
- No other test injects headers.

### 6. `.gitignore`
- `ui/tests/.auth/` (session cookies) and `ui/.env.playwright.local` (password +
  TOTP secret).

### 7. Scripts / docs
- `ui/package.json`: add `"test:e2e": "playwright test"`.
- `Makefile` (optional): `test-e2e-prod` target wrapping
  `PLAYWRIGHT_BASE_URL=https://naslos.local npx playwright test` from `ui/`.
- `docs/deployment.md`: a "Running the E2E suite" subsection — the env file, the
  TOTP secret, the two postures, and the explicit warning that the suite mutates
  the live instance (creates/deletes users, groups, shares, datasets, buddy
  schedules, and performs real buddy sends).
- `docs/CODE-REVIEW.md`: update CR-43 (no E2E coverage of the authenticated
  posture).

## Validation

1. `cd ui && npm run check` — setup/helper type-check.
2. TOTP helper unit check against a known RFC 6238 vector.
3. **Dev posture** (regression): the suite must behave exactly as before — setup
   sees no portal, writes an empty state, 30 passed / 2 skipped.
   `PLAYWRIGHT_BASE_URL=http://192.168.1.117:30080 npx playwright test`
   (use `make install-vm` if the instance is on the prod posture, or point at
   another dev instance).
4. **Production posture**: with `ui/.env.playwright.local` present,
   `PLAYWRIGHT_BASE_URL=https://naslos.local npx playwright test`
   → setup logs in (password + TOTP), storageState written, `/api/auth/me`
   confirms `naslos_admins`, then fix whatever the authenticated run surfaces.
   Expected differences from the dev run: the terminal interactive tests execute
   and pass (pods list, shell prompt, `echo`, `id -u`, `zpool list`); the
   unauthenticated-refusal test asserts 302.
5. Re-run the production command a second time without touching the authenticator
   to prove repeatability (this is why `NASLOS_TOTP_SECRET` is preferred).

## Risks / notes

- **Destructive against live data**: users, groups, shares and datasets are
  created then deleted; the two buddy tests perform real self-sends and leave
  chunks on the receiver. Accepted per the request, but called out in the docs.
- **Authelia regulation**: 3 bad attempts ban the account for 5 minutes; the
  setup must not loop on failure.
- **Session lifetime**: 1 h expiry / 5 min inactivity; the setup runs once and
  the suite is short, so one login covers a run. A very long run may need a
  re-login (re-run the command).
- **Cookie domain**: the `authelia_session` cookie is `naslos.local`, so the
  base URL must use that name (not the IP) for storageState to apply.
- **Selectors** are from Authelia's own tests; verify against the live portal and
  adjust if 4.39 renamed an id.

## Decision (resolved)

The user will provide the **TOTP shared secret** (base32) via
`ui/.env.playwright.local`, so `auth.setup.ts` mints a fresh code per run and the
suite is repeatable/unattended. `NASLOS_TOTP_CODE` remains supported as a
one-off fallback but is not the intended path.

The user creates the env file themselves (mode 600):

```
NASLOS_ADMIN_USER=admin
NASLOS_ADMIN_PASSWORD=<admin password>
NASLOS_TOTP_SECRET=<base32 secret from the enrolment QR / otpauth URI>
```

Before the production run I confirm the file exists and is git-ignored, and I
never print its contents.
