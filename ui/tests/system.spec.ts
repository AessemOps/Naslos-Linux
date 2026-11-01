import { test, expect } from '@playwright/test';

test('system page reports versions and gates the upgrade operations', async ({ page, request }) => {
  const res = await request.get('/api/system/version');
  expect(res.ok(), await res.text()).toBeTruthy();
  const version = await res.json();

  await page.goto('/system');
  await expect(page.getByRole('heading', { name: 'System' })).toBeVisible();
  await expect(page.getByText('Loading system state...')).toBeHidden({ timeout: 10_000 });

  const table = page.locator('.card').filter({ hasText: 'Versions' });
  await expect(table).toContainText(version.naslosVersion);
  if (version.talosVersion) await expect(table).toContainText(version.talosVersion);
  if (version.kubernetesVersion) await expect(table).toContainText(version.kubernetesVersion);

  const naslos = page.getByRole('button', { name: 'Upgrade Naslos' });
  if (!version.upgradable) {
    // The live VM is either a `make install-vm` deployment or an install from
    // a pre-0.3.0 pack: versions render, upgrades are disabled with the path.
    await expect(page.getByText(/no installer release facts/)).toBeVisible();
    await expect(naslos).toBeDisabled();
  } else {
    // An installer node may or may not have an update; either way the button is
    // present and its disabled reason is rendered next to it.
    await expect(naslos).toBeVisible();
  }
});

test('system update check returns the cache shape', async ({ request }) => {
  const res = await request.get('/api/system/updates');
  expect(res.ok(), await res.text()).toBeTruthy();
  const updates = await res.json();
  expect(updates).toHaveProperty('enabled');
  expect(updates).toHaveProperty('available');
  expect(updates).toHaveProperty('stepping');
  if (updates.available) {
    expect(updates.available.version).toMatch(/^\d+\.\d+\.\d+$/);
    expect(updates.available.talosVersion).toMatch(/^v?\d+\.\d+\.\d+$/);
    expect(updates.available.kubernetesVersion).toMatch(/^v?\d+\.\d+\.\d+$/);
  }
});
