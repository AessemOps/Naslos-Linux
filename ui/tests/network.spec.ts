import { test, expect } from '@playwright/test';

// Mutating network tests are opt-in only. The API suite runs against the live
// production node, so a default apply would reconfigure it. CI never sets this.
const APPLY_ENABLED = process.env.NETWORK_APPLY_TEST === '1';

test.describe('network settings', () => {
  test('the API reports an inventory with links, hostname and resolvers', async ({ request }) => {
    const res = await request.get('/api/network');
    expect(res.ok(), await res.text()).toBeTruthy();
    const data = await res.json();
    expect(Array.isArray(data.links)).toBe(true);
    expect(typeof data.primaryLink).toBe('string');
    expect(typeof data.hostname.value).toBe('string');
    expect(typeof data.hostname.source).toBe('string');
    expect(Array.isArray(data.resolvers.nameservers)).toBe(true);
    expect(typeof data.resolvers.inherit).toBe('boolean');
    expect(typeof data.resolvers.hostDns.enabled).toBe('boolean');
    for (const link of data.links) {
      expect(['dhcp', 'static']).toContain(link.config.addressing);
      expect(Array.isArray(link.addresses)).toBe(true);
    }
    expect(data.pending === null || typeof data.pending.phase === 'string').toBe(true);
  });

  test('the page renders the inventory', async ({ page }) => {
    await page.goto('/settings/network');
    await expect(page.getByRole('heading', { name: 'Network' })).toBeVisible();
    await expect(page.getByText('Loading network inventory...')).toBeHidden({ timeout: 15_000 });
    await expect(page.getByRole('heading', { name: 'Hostname' })).toBeVisible();
    await expect(page.getByRole('heading', { name: 'DNS resolvers' })).toBeVisible();
    await expect(page.getByRole('heading', { name: 'Links' })).toBeVisible();
    await expect(page.getByRole('heading', { name: 'Appliance' })).toBeVisible();
  });

  test('the Settings hub card navigates to the network page', async ({ page }) => {
    await page.goto('/settings');
    await page.getByRole('link', { name: /Network/ }).click();
    await expect(page).toHaveURL(/\/settings\/network$/);
    await expect(page.getByRole('heading', { name: 'Network' })).toBeVisible();
  });

  test('an invalid CIDR is rejected in the browser without a POST', async ({ page }) => {
    await page.goto('/settings/network');
    await expect(page.getByText('Loading network inventory...')).toBeHidden({ timeout: 15_000 });

    const edit = page.getByRole('button', { name: /^Edit / }).first();
    test.skip((await edit.count()) === 0, 'Talos reported no physical links');
    await edit.click();

    await page.getByLabel('Addressing').selectOption('static');
    await page.getByLabel('Addresses').fill('not-a-cidr');

    let posts = 0;
    page.on('request', (request) => {
      if (request.method() === 'POST' && request.url().includes('/api/network/changes')) {
        posts += 1;
      }
    });
    await page.getByRole('button', { name: 'Apply link' }).click();
    await expect(page.getByText(/not in CIDR form/)).toBeVisible();
    expect(posts).toBe(0);
  });

  test('a gateway outside the configured subnet is rejected without a POST', async ({ page }) => {
    await page.goto('/settings/network');
    await expect(page.getByText('Loading network inventory...')).toBeHidden({ timeout: 15_000 });

    const edit = page.getByRole('button', { name: /^Edit / }).first();
    test.skip((await edit.count()) === 0, 'Talos reported no physical links');
    await edit.click();

    await page.getByLabel('Addressing').selectOption('static');
    await page.getByLabel('Addresses').fill('10.123.0.10/24');
    await page.getByLabel('Gateway').fill('10.124.0.1');

    let posts = 0;
    page.on('request', (request) => {
      if (request.method() === 'POST' && request.url().includes('/api/network/changes')) {
        posts += 1;
      }
    });
    await page.getByRole('button', { name: 'Apply link' }).click();
    await expect(page.getByText(/not inside any configured subnet/)).toBeVisible();
    expect(posts).toBe(0);
  });

  test('applying an MTU change is opt-in only', async ({ page }) => {
    test.skip(!APPLY_ENABLED, 'set NETWORK_APPLY_TEST=1 to run the mutating drill');
    await page.goto('/settings/network');
    await expect(page.getByText('Loading network inventory...')).toBeHidden({ timeout: 15_000 });
    const edit = page.getByRole('button', { name: /^Edit / }).first();
    await edit.click();
    await page.getByLabel('MTU (optional)').fill('1400');
    await page.getByRole('button', { name: 'Apply link' }).click();
    await expect(page.getByRole('heading', { name: 'Confirm the network change' })).toBeVisible();
    await page.getByRole('button', { name: 'Apply', exact: true }).click();
    await expect(page.getByText(/Change started/)).toBeVisible();
  });
});
