import { defineConfig, devices } from '@playwright/test';
import { existsSync, readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

// Load ui/.env.playwright.local (untracked, mode 600) so the admin password and
// TOTP secret do not have to be exported by hand. Values already present in the
// environment win, so CI can inject them instead.
const here = dirname(fileURLToPath(import.meta.url));
const envFile = resolve(here, '.env.playwright.local');
if (existsSync(envFile)) {
  for (const line of readFileSync(envFile, 'utf8').split('\n')) {
    const match = /^\s*([A-Z0-9_]+)\s*=\s*(.*?)\s*$/.exec(line);
    if (!match || process.env[match[1]] !== undefined) continue;
    process.env[match[1]] = match[2].replace(/^["']|["']$/g, '');
  }
}

export default defineConfig({
  testDir: 'tests',
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,
  workers: 1,
  reporter: 'list',
  use: {
    // Point the suite at whichever instance is under test, e.g.
    //   PLAYWRIGHT_BASE_URL=https://naslos.local npx playwright test
    //   PLAYWRIGHT_BASE_URL=http://192.168.1.117:30080 npx playwright test
    // The default is the long-standing single-node VM.
    baseURL: process.env.PLAYWRIGHT_BASE_URL ?? 'http://192.168.1.96:30080',
    // The production posture serves the chart's self-signed certificate.
    ignoreHTTPSErrors: true,
    trace: 'on-first-retry',
    screenshot: 'only-on-failure',
  },
  projects: [
    // Logs into Authelia once and writes tests/.auth/admin.json. When the target
    // has no portal (dev posture) it writes an empty state instead.
    { name: 'setup', testMatch: /auth\.setup\.ts/ },
    {
      name: 'chromium',
      dependencies: ['setup'],
      use: { ...devices['Desktop Chrome'], storageState: 'tests/.auth/admin.json' },
    },
  ],
});
