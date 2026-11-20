import { test, expect } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';

// Axe scan of every route in both themes. The suite runs against the live
// node through the authenticated proxy (same storageState as the other
// specs). Any deliberate exception must be documented inline at the scan
// call site — none so far.

const ROUTES = [
  '/',
  '/pools',
  '/datasets',
  '/disks',
  '/disks/health',
  '/shares',
  '/users',
  '/groups',
  '/apps',
  '/backups',
  '/vms',
  '/terminal',
  '/domains',
  '/dns',
  '/notifications',
  '/settings',
  '/settings/network',
  '/system'
];

const THEMES = ['dark', 'light'] as const;

const TAGS = ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa'];

// Routes whose first paint is a LoadingState; those must settle before the
// scan or axe would judge the skeleton page. Routes that render immediately
// (settings hub, terminal, the disk wizard) have no page-level loader.
const LOADED_ROUTES = new Set([
  '/',
  '/pools',
  '/datasets',
  '/disks/health',
  '/shares',
  '/users',
  '/groups',
  '/apps',
  '/backups',
  '/vms',
  '/domains',
  '/dns',
  '/notifications',
  '/settings/network',
  '/system'
]);

async function setTheme(page: import('@playwright/test').Page, theme: 'dark' | 'light') {
  await page.goto('/');
  await page.evaluate((t) => {
    try {
      localStorage.setItem('naslos:theme', t);
    } catch {
      // Storage unavailable: the pre-paint default (dark) applies.
    }
    document.documentElement.dataset.theme = t;
  }, theme);
}

async function scan(
  page: import('@playwright/test').Page,
  route: string,
  theme: (typeof THEMES)[number]
) {
  await page.goto(route);
  if (LOADED_ROUTES.has(route)) {
    await page
      .waitForSelector('[data-testid="page-loading"]', { state: 'detached', timeout: 20_000 })
      .catch(() => {
        // A route that failed to load is still worth scanning; its error
        // state is part of the UI contract (FR-MET-09).
      });
  }

  const results = await new AxeBuilder({ page }).withTags(TAGS).analyze();
  const summary = results.violations
    .map((v) => `${v.id} (${v.nodes.length}): ${v.nodes.slice(0, 3).map((n) => n.target.join(' ')).join(' | ')}`)
    .join('\n');
  expect(summary, `axe violations on ${route} in ${theme} theme`).toBe('');
}

test('every route is axe-clean in the dark theme', async ({ page }) => {
  test.setTimeout(180_000);
  await setTheme(page, 'dark');
  for (const route of ROUTES) {
    await scan(page, route, 'dark');
  }
});

test('every route is axe-clean in the light theme', async ({ page }) => {
  test.setTimeout(180_000);
  await setTheme(page, 'light');
  for (const route of ROUTES) {
    await scan(page, route, 'light');
  }
});
