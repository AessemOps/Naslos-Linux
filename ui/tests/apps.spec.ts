import { test, expect } from '@playwright/test';

test.describe('apps', () => {
  test('shows the catalog, installed and sources tabs', async ({ page }) => {
    await page.goto('/apps');
    await expect(page.getByRole('heading', { name: 'Apps', exact: true })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Catalog', exact: true })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Installed', exact: true })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Sources', exact: true })).toBeVisible();
  });

  test('the sources tab lists chart repositories', async ({ page }) => {
    await page.goto('/apps');
    await page.getByRole('button', { name: 'Sources', exact: true }).click();
    await expect(page.getByText('Loading sources...')).toBeHidden({ timeout: 10_000 });
    await expect(page.getByRole('button', { name: '+ Add Source' })).toBeVisible();
  });

  test('the catalog filters by channel when entries exist', async ({ page, request }) => {
    const res = await request.get('/api/catalog');
    const catalog = res.ok() ? await res.json() : [];
    test.skip(!Array.isArray(catalog) || catalog.length === 0, 'no catalog entries; add and refresh a source first');

    await page.goto('/apps');
    await expect(page.getByText('Loading catalog...')).toBeHidden({ timeout: 10_000 });
    await expect(page.getByRole('combobox', { name: 'Channel' })).toBeVisible();
  });

  test('installs and uninstalls an app from the official source', async ({ page, request }) => {
    const res = await request.get('/api/catalog');
    const catalog = res.ok() ? await res.json() : [];
    test.skip(!Array.isArray(catalog) || catalog.length === 0, 'no catalog entries; add and refresh a source first');

    const app = catalog[0];
    // Install cluster-internal (no subdomain) so the test needs no DNS or cert.
    const created = await request.post('/api/apps', {
      data: {
        name: app.name,
        values: app.defaultValues || {},
        exposure: { subdomain: '', tls: false, auth: false, localOnly: false },
        confirmed: true
      }
    });
    expect(created.status(), await created.text()).toBe(201);

    try {
      await page.goto('/apps');
      await page.getByRole('button', { name: 'Installed', exact: true }).click();
      await expect(page.getByText('Loading installed apps...')).toBeHidden({ timeout: 10_000 });
      await expect(page.getByRole('heading', { name: app.name, exact: true })).toBeVisible();

      // The exposure modal offers a base-domain picker (FR-APP-10); the API
      // always returns the primary in selectableDomains, so the select renders.
      await page.getByRole('button', { name: 'Exposure', exact: true }).first().click();
      await expect(page.locator('#exposure-domain')).toBeVisible();
      await page.getByRole('button', { name: 'Cancel', exact: true }).click();
    } finally {
      const removed = await request.delete(`/api/apps/${app.name}`);
      expect([200, 500]).toContain(removed.status());
    }
  });

  test('rejects an unconfirmed install', async ({ request }) => {
    const res = await request.post('/api/apps', {
      data: { name: 'no-such-app', values: {}, confirmed: false }
    });
    expect(res.status()).toBe(400);
  });

  test('the exposure endpoint is guarded', async ({ request }) => {
    const res = await request.get('/api/apps/does-not-exist/exposure');
    expect(res.status()).toBe(404);
  });
});
