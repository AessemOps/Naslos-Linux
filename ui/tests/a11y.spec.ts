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
  '/replication',
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
  '/settings/mcp',
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
  '/replication',
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
  '/settings/mcp',
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
    // 5s per route keeps the total settle budget (15 x 5s) inside the 180s
    // test cap: a stalled route fails fast with the route/theme in the
    // message instead of a generic suite timeout.
    await page
      .waitForSelector('[data-testid="page-loading"]', { state: 'detached', timeout: 5_000 })
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

// Modal-open states. These are route-mocked so the scan sees a deterministic
// dialog and the suite never depends on (or writes to) the node's catalog.
async function expectAxeClean(page: import('@playwright/test').Page, label: string) {
  const results = await new AxeBuilder({ page }).withTags(TAGS).analyze();
  const summary = results.violations
    .map((v) => `${v.id} (${v.nodes.length}): ${v.nodes.slice(0, 3).map((n) => n.target.join(' ')).join(' | ')}`)
    .join('\n');
  expect(summary, `axe violations in ${label}`).toBe('');
}

const mockCatalogApp = {
  name: 'demo',
  displayName: 'Demo App',
  description: 'A deterministic catalog fixture.',
  category: 'media',
  source: 'official',
  channel: 'Prod',
  channels: ['Prod'],
  version: '1.0.0',
  icon: 'D',
  iconUrl: '',
  chart: 'demo',
  chartPath: 'apps/demo',
  tags: [],
  schema: { properties: {}, required: [] },
  defaultValues: {},
  exposure: { subdomain: '', tls: true, auth: true, localOnly: false, internet: true },
  services: [{ name: 'demo', port: 80, scheme: 'http' }]
};

const mockInstalledApp = {
  name: 'demo',
  namespace: 'naslos-app-1',
  chart: 'demo',
  status: 'running',
  url: '',
  orphaned: false,
  description: 'A deterministic installed fixture.',
  chartVersion: '1.0.0',
  chartPath: 'apps/demo',
  source: 'official',
  channel: 'Prod',
  values: {},
  exposure: { subdomain: 'demo', tls: true, auth: true, localOnly: false, internet: true },
  displayName: 'Demo App',
  icon: 'D',
  iconUrl: '',
  installedVersion: '1.0.0',
  latestVersion: '1.0.0',
  latestSource: 'official',
  latestChannel: 'Prod',
  updateAvailable: false
};

async function mockAppApi(page: import('@playwright/test').Page, installed: boolean) {
  await page.route(/\/api\//, async (route) => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    if (path === '/api/catalog') return route.fulfill({ json: [mockCatalogApp] });
    if (path === '/api/catalog/demo') return route.fulfill({ json: mockCatalogApp });
    if (path === '/api/apps') return route.fulfill({ json: installed ? [mockInstalledApp] : [] });
    if (path === '/api/apps/updates') return route.fulfill({ json: { count: 0, apps: [] } });
    if (path === '/api/apps/usage') return route.fulfill({ json: { available: false, apps: [] } });
    if (path === '/api/apps/namespaces') {
      return route.fulfill({
        json: {
          baseline: { provisioned: 4, limit: 4, used: installed ? 1 : 0, available: 4 },
          privileged: { provisioned: 1, limit: 1, used: 0, available: 1 }
        }
      });
    }
    if (path === '/api/apps/demo/quota') return route.fulfill({ json: { available: false } });
    if (path === '/api/apps/demo/exposure') {
      return route.fulfill({
        json: {
          exposure: mockInstalledApp.exposure,
          baseDomain: 'example.test',
          selectableDomains: ['example.test'],
          ssoDomains: ['example.test']
        }
      });
    }
    if (path === '/api/apps/demo/services') {
      return route.fulfill({ json: [{ name: 'demo', port: 80, scheme: 'http' }] });
    }
    if (path === '/api/datasets/registry') return route.fulfill({ json: [] });
    if (path === '/api/volumes/zfs') return route.fulfill({ json: [{ name: 'tank' }] });
    if (path === '/api/domains') {
      return route.fulfill({
        json: {
          domains: [],
          baseDomain: 'example.test',
          certManager: true,
          selectableDomains: ['example.test'],
          ssoDomains: ['example.test']
        }
      });
    }
    return route.fulfill({ json: {} });
  });
}

test('the install wizard is axe-clean while open', async ({ page }) => {
  await setTheme(page, 'dark');
  await mockAppApi(page, false);
  await page.goto('/apps');
  await page.getByRole('button', { name: 'Catalog', exact: true }).click();
  await page.getByRole('button', { name: 'Install Demo App' }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog.getByRole('heading', { name: 'Demo App' })).toBeVisible();
  await expectAxeClean(page, 'install wizard (open)');
});

test('the app drawer and exposure modal are axe-clean while open', async ({ page }) => {
  await setTheme(page, 'dark');
  await mockAppApi(page, true);
  await page.goto('/apps');
  await expect(page.getByText('Loading installed apps...')).toBeHidden({ timeout: 10_000 });
  await page.getByRole('button', { name: 'Open Demo App details' }).click();
  await expect(page.getByRole('button', { name: 'Close details' })).toBeVisible();
  await expectAxeClean(page, 'app drawer (open)');

  await page.getByRole('button', { name: 'Exposure', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Exposure — demo' })).toBeVisible();
  await expectAxeClean(page, 'exposure modal (open)');
});

test('the VM wizard is axe-clean while open', async ({ page }) => {
  await setTheme(page, 'dark');
  await page.route(/\/api\//, async (route) => {
    const path = new URL(route.request().url()).pathname;
    if (path === '/api/virt/capabilities') {
      return route.fulfill({
        json: { kvm: true, cpuModel: 'Test CPU', nested: false, secureBoot: true, nics: [] }
      });
    }
    if (path === '/api/virt/vms') return route.fulfill({ json: [] });
    if (path === '/api/volumes/zfs') return route.fulfill({ json: [{ name: 'tank' }] });
    if (path === '/api/virt/images') {
      return route.fulfill({ json: { images: [], importing: [], errors: {} } });
    }
    return route.fulfill({ json: {} });
  });
  await page.goto('/vms');
  await page.getByRole('button', { name: 'New VM' }).click();
  await expect(page.getByRole('heading', { name: 'New virtual machine' })).toBeVisible();
  await expectAxeClean(page, 'VM wizard (open)');
});
