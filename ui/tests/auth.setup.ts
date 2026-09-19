import { test as setup, expect } from '@playwright/test';
import { totp } from './totp';

// Session captured here is reused by every test (playwright.config.ts sets it as
// the chromium project's storageState). Playwright's `request` fixture shares
// the browser context's cookies, so both page.goto and request.get('/api/…')
// are authenticated by it. There is no unauthenticated target any more, so a
// missing credential or a non-challenged response is an error, not a fallback.
export const STATE = 'tests/.auth/admin.json';

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

// Log in once through the Authelia portal and persist the session. Every route
// is authenticated now, so credentials are mandatory and a target that does not
// challenge is an error.
setup('authenticate', async ({ page, context }) => {
  // Waiting out a TOTP window on a rejected-but-already-used code can take up to
  // 30 s per attempt, well past the default test timeout.
  setup.setTimeout(180_000);

  const user = process.env.NASLOS_ADMIN_USER ?? 'admin';
  const password = process.env.NASLOS_ADMIN_PASSWORD;

  if (!password) {
    throw new Error(
      'NASLOS_ADMIN_PASSWORD is required: the appliance only serves the authenticated proxy. Put it (and NASLOS_TOTP_SECRET) in ui/.env.playwright.local.',
    );
  }

  await page.goto('/');

  if (!page.url().includes('/authelia/')) {
    // The target served the app without a challenge, which the production
    // posture never does: fail loudly rather than run the suite anonymously.
    throw new Error(`expected the Authelia portal, landed on ${page.url()}`);
  }

  await page.locator('#username-textfield').fill(user);
  await page.locator('#password-textfield').fill(password);
  await submit(page);

  // Second factor. Authelia 4.39 renders six single-digit inputs (aria labels
  // "Digit 1".."Digit 6") and submits when the last one is filled; older builds,
  // and the single-field variant their tests reference, use
  // #one-time-code-textfield. With WebAuthn as well, a method picker comes
  // first.
  const otp = page.locator('#one-time-code-textfield');
  const otpDigits = page.getByRole('textbox', { name: /Digit \d/ });
  const picker = page.getByRole('button', { name: /one[- ]time password/i });
  const alert = page.getByRole('alert');

  await Promise.race([
    otp.waitFor({ state: 'visible', timeout: 20_000 }).catch(() => undefined),
    otpDigits.first().waitFor({ state: 'visible', timeout: 20_000 }).catch(() => undefined),
    picker.waitFor({ state: 'visible', timeout: 20_000 }).catch(() => undefined),
    alert.waitFor({ state: 'visible', timeout: 20_000 }).catch(() => undefined),
  ]);

  if (await picker.isVisible().catch(() => false)) {
    await picker.click();
  }

  const singleField = await otp.isVisible().catch(() => false);

  if (!singleField && !(await otpDigits.first().isVisible().catch(() => false))) {
    // Fail with Authelia's own message rather than waiting for a field that a
    // rejected first factor will never show. Repeated wrong attempts also trip
    // Authelia's regulation (3 tries -> 5 minute ban), so one clear failure is
    // better than a silent timeout.
    const notices = (await alert.allInnerTexts().catch(() => [])).join(' | ');
    throw new Error(
      `Authelia did not offer a second factor after the password (still on the portal). Messages: ${notices || '(none)'}`,
    );
  }

  async function enterCode(code: string): Promise<void> {
    if (singleField) {
      await otp.fill(code);
      await submit(page);
      return;
    }
    // Six single-digit inputs; the form submits itself once the last is filled.
    const digits = page.getByRole('textbox', { name: /Digit \d/ });
    const count = Math.min(6, await digits.count());
    for (let i = 0; i < count; i++) {
      await digits.nth(i).fill(code[i]);
    }
    if (count < 6) {
      await submit(page);
    }
  }

  const leftPortal = (url: URL) => !url.pathname.includes('/authelia');

  async function attemptLogin(code: string): Promise<boolean> {
    await enterCode(code);
    try {
      await page.waitForURL(leftPortal, { timeout: 8_000 });
      return true;
    } catch {
      return false;
    }
  }

  if (!(await attemptLogin(currentCode()))) {
    const notices = (await alert.allInnerTexts().catch(() => [])).join(' | ');

    if (/already used|reuse|incorrect|invalid/i.test(notices)) {
      // Authelia refuses a TOTP it has already accepted, so a second login inside
      // the same 30 s window (a repeated suite run) cannot reuse the code. Wait
      // for the next window and try exactly once more: a third wrong submission
      // would trip regulation and lock the account out.
      await page.waitForTimeout(31_000 - (Date.now() % 30_000));
      if (!(await attemptLogin(currentCode()))) {
        const again = (await alert.allInnerTexts().catch(() => [])).join(' | ');
        throw new Error(`Authelia rejected the one-time code twice. Messages: ${again || notices || '(none)'}`);
      }
    } else {
      // Some builds want an explicit submit instead of auto-submitting.
      await page.keyboard.press('Enter').catch(() => undefined);
      try {
        await page.waitForURL(leftPortal, { timeout: 5_000 });
      } catch {
        throw new Error(`Authelia login did not complete. Messages: ${notices || '(none)'}`);
      }
    }
  }

  await page.waitForURL(leftPortal, { timeout: 30_000 });
  await context.storageState({ path: STATE });

  // Fail here, with a readable reason, if the session is not an administrator:
  // otherwise every later test 403s and the cause is buried.
  const me = await page.request.get('/api/auth/me');
  expect(me.status(), `GET /api/auth/me after login: ${await me.text()}`).toBe(200);
  expect(String((await me.json()).groups)).toContain('naslos_admins');
});
