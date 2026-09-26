import { test, expect } from '@playwright/test';

test.describe('domains & SSL', () => {
  test('the domains page loads with the base domain and status', async ({ page }) => {
    await page.goto('/domains');
    await expect(page.getByRole('heading', { name: 'Domains & SSL' })).toBeVisible();
    await expect(page.getByText('Loading domains...')).toBeHidden({ timeout: 10_000 });
    await expect(page.getByRole('button', { name: '+ Add Domain' })).toBeVisible();
  });

  test('reports cert-manager availability from the API', async ({ page, request }) => {
    const res = await request.get('/api/domains');
    expect(res.ok(), await res.text()).toBeTruthy();
    const data = await res.json();
    expect(typeof data.certManager).toBe('boolean');
    expect(typeof data.baseDomain).toBe('string');
    // FR-APP-10/15: the exposure UI and the SSO toggle read these.
    expect(Array.isArray(data.selectableDomains)).toBe(true);
    expect(data.selectableDomains).toContain(data.baseDomain);
    expect(Array.isArray(data.ssoDomains)).toBe(true);
    expect(data.ssoDomains).toContain(data.baseDomain);

    await page.goto('/domains');
    if (data.certManager) {
      await expect(page.getByText('cert-manager is not installed')).toBeHidden();
    } else {
      await expect(page.getByText('cert-manager is not installed')).toBeVisible();
    }
  });

  test('exposes the SSO toggle for a non-primary domain', async ({ page, request }) => {
    const res = await request.get('/api/domains');
    const data = await res.json();
    const secondary = (data.domains || []).find((d: any) => d.baseDomain !== data.baseDomain);
    test.skip(!secondary, 'no secondary domain registered');

    await page.goto('/domains');
    const row = page.locator('.card', { hasText: secondary.baseDomain });
    await expect(row.getByRole('button', { name: /SSO/ })).toBeVisible();
  });

  test('opens the add-domain form', async ({ page }) => {
    await page.goto('/domains');
    await page.getByRole('button', { name: '+ Add Domain' }).click();
    await expect(page.getByRole('heading', { name: 'Add Domain' })).toBeVisible();
    await expect(page.getByLabel('Base domain')).toBeVisible();
    await page.getByRole('button', { name: 'Cancel' }).click();
  });

  test('shows the provider API-rights info bubble', async ({ page }) => {
    await page.goto('/domains');
    await page.getByRole('button', { name: '+ Add Domain' }).click();
    const info = page.getByRole('button', { name: 'API rights required' });
    await expect(info).toBeVisible();
    await info.click();
    await expect(page.getByRole('tooltip')).toContainText(/DNS.*Edit/);
    await page.getByRole('button', { name: 'Cancel' }).click();
  });

  test('validates a malformed domain', async ({ request }) => {
    const res = await request.put('/api/domains/not_a_domain', {
      data: {
        baseDomain: 'not_a_domain',
        dnsProvider: 'cloudflare',
        credentialsSecret: 'x',
        environment: 'staging'
      }
    });
    expect(res.status()).toBe(400);
  });
});
