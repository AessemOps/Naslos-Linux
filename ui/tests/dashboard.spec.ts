import { test, expect } from '@playwright/test';

test('dashboard shows live node metrics', async ({ page }) => {
  await page.goto('/');

  await expect(page.getByRole('heading', { name: 'Dashboard' })).toBeVisible();
  await expect(page.getByText('Loading dashboard...')).toBeHidden({ timeout: 10_000 });

  // --- API sanity: the collector must have published a real snapshot ---
  const res = await page.request.get('/api/metrics');
  expect(res.ok()).toBeTruthy();
  const metrics = await res.json();
  expect(metrics.system.hostname, 'hostname collected from Talos').not.toBe('');
  expect(metrics.cpu.cores, 'CPU cores collected from Talos').toBeGreaterThan(0);
  expect(metrics.memory.total, 'memory total collected from Talos').toBeGreaterThan(0);
  expect(metrics.updatedAt).not.toContain('0001-01-01');

  // --- UI: stat cards show the collected values ---
  const cpuCard = page.locator('.card').filter({ hasText: 'CPU Usage' });
  await expect(cpuCard.getByText(/cores$/)).not.toHaveText('0 cores');

  const memCard = page.locator('.card').filter({ hasText: 'Memory' });
  await expect(memCard).not.toContainText('0 B / 0 B');

  // --- UI: system information is populated ---
  const hostnameValue = page
    .getByText('Hostname', { exact: true })
    .locator('xpath=following-sibling::div[1]');
  await expect(hostnameValue).not.toHaveText('—');

  const updatedValue = page
    .getByText('Updated', { exact: true })
    .locator('xpath=following-sibling::div[1]');
  await expect(updatedValue).not.toHaveText('—');
  await expect(updatedValue).not.toContainText('0001-01-01');
});

test('dashboard shows zfs pools from the agent', async ({ page }) => {
  await page.goto('/');

  await expect(page.getByRole('heading', { name: 'ZFS Pools' })).toBeVisible();
  // The VM has a "tank" pool managed by the naslos-agent
  await expect(page.getByRole('link', { name: 'tank' })).toBeVisible();
  await expect(page.getByText('ONLINE')).toBeVisible();
});
