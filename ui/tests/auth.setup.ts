import { test as setup, expect } from '@playwright/test';
import { mkdirSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { totp } from './totp';

// Session captured here is reused by every test (playwright.config.ts sets it as
// the chromium project's storageState). Playwright's `request` fixture shares
// the browser context's cookies, so both page.goto and request.get('/api/…')
// are authenticated by it.
export const STATE = 'tests/.auth/admin.json';

function writeEmptyState(): void {
  mkdirSync(dirname(resolve(STATE)), { recursive: true });
  writeFileSync(STATE, JSON.stringify({ cookies: [], origins: [] }, null, 2));
}

function currentCode(): string {
  if (process.env.NASLOS_TOTP_CODE) {
    return process.env.NASLOS_TOTP_CODE;
  }
  const secret = process.env.NASLOS_TOTP_SECRET;
  if (!secret) {
    throw new Error(
      'No TOTP available: set NASLOS_TOTP_SECRET (base32) in ui/.env.playwright.local, or NASLOS_TOTP_CODE for a single run.',
    );
  }
  return totp(secret);
}

async function submit(page: import('@playwright/test').Page): Promise<void> {
  // Authelia's own tests use #sign-in-button on both factors; fall back to the
  // button's text if the id ever changes.
  const button = page.locator('#sign-in-button');
  if (await button.isVisible().catch(() => false)) {
    await button.click();
    return;
  }
  await page.getByRole('button', { name: /sign in|authenticate/i }).first().click();
}

// Log in once through the Authelia portal and persist the session. The setup is
// posture-aware: when the target has no auth in the path (the dev NodePort
// posture) it writes an empty state and the suite runs exactly as before.
setup('authenticate', async ({ page, context }) => {
  const user = process.env.NASLOS_ADMIN_USER ?? 'admin';
  const password = process.env.NASLOS_ADMIN_PASSWORD;

  if (!password) {
    writeEmptyState();
    return;
  }

  await page.goto('/');

  if (!page.url().includes('/authelia/')) {
    // Served the app without a challenge: no proxy auth on this target.
    writeEmptyState();
    return;
  }

  await page.locator('#username-textfield').fill(user);
  await page.locator('#password-textfield').fill(password);
  await submit(page);

  // Second factor. TOTP shows directly when it is the only registered method;
  // with WebAuthn as well, Authelia shows a method picker first.
  const otp = page.locator('#one-time-code-textfield');
  const picker = page.getByRole('button', { name: /one[- ]time password/i });
  const alert = page.getByRole('alert');

  await Promise.race([
    otp.waitFor({ state: 'visible', timeout: 20_000 }).catch(() => undefined),
    picker.waitFor({ state: 'visible', timeout: 20_000 }).catch(() => undefined),
    alert.waitFor({ state: 'visible', timeout: 20_000 }).catch(() => undefined),
  ]);

  // Fail with Authelia's own message rather than waiting for a code field that
  // a rejected first factor will never show. Repeated wrong attempts also trip
  // Authelia's regulation (3 tries -> 5 minute ban), so one clear failure is
  // better than a silent timeout.
  if (!(await otp.isVisible().catch(() => false)) && !(await picker.isVisible().catch(() => false))) {
    const notices = (await alert.allInnerTexts().catch(() => [])).join(' | ');
    throw new Error(
      `Authelia did not offer a second factor after the password (still on the portal). Messages: ${notices || '(none)'}`,
    );
  }

  if (await picker.isVisible().catch(() => false)) {
    await picker.click();
  }

  await otp.waitFor({ state: 'visible', timeout: 20_000 });
  await otp.fill(currentCode());
  await submit(page);

  await page.waitForURL((url) => !url.pathname.includes('/authelia'), { timeout: 30_000 });
  await context.storageState({ path: STATE });

  // Fail here, with a readable reason, if the session is not an administrator:
  // otherwise every later test 403s and the cause is buried.
  const me = await page.request.get('/api/auth/me');
  expect(me.status(), `GET /api/auth/me after login: ${await me.text()}`).toBe(200);
  expect(String((await me.json()).groups)).toContain('naslos_admins');
});
