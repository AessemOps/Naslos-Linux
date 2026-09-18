import { defineConfig, devices } from '@playwright/test';

export default defineConfig({
  testDir: 'tests',
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,
  workers: 1,
  reporter: 'list',
  use: {
    // Point the suite at whichever instance is under test, e.g.
    //   PLAYWRIGHT_BASE_URL=http://192.168.1.117:30080 npx playwright test
    // The default is the long-standing single-node VM.
    baseURL: process.env.PLAYWRIGHT_BASE_URL ?? 'http://192.168.1.96:30080',
    trace: 'on-first-retry',
    screenshot: 'only-on-failure',
  },
  projects: [
    {
      name: 'chromium',
      use: { ...devices['Desktop Chrome'] },
    },
  ],
});
