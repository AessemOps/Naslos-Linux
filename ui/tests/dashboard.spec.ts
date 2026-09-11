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

  // The agent is the source of truth for pools; whatever pools exist must be
  // rendered as links with an ONLINE health badge (don't hard-code names).
  const res = await page.request.get('/api/metrics');
  expect(res.ok()).toBeTruthy();
  const { zfs } = await res.json();
  const poolNames: string[] = (zfs?.pools ?? []).map((p: { name: string }) => p.name);
  if (poolNames.length === 0) {
    // No pools on this cluster: the empty state must offer pool creation.
    await expect(page.getByRole('link', { name: 'Create Your First Pool' })).toBeVisible();
    return;
  }
  await expect(page.getByText('No ZFS pools configured yet.')).toBeHidden();
  for (const name of poolNames) {
    await expect(page.getByRole('link', { name, exact: true })).toBeVisible();
  }
  await expect(page.getByText('ONLINE').first()).toBeVisible();
});

test('dashboard auto-refreshes every 5 seconds', async ({ page }) => {
  // Serve incrementing CPU values; the UI must re-render without a reload.
  let call = 0;
  await page.route('**/api/dashboard', async (route) => {
    call += 1;
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        cpu: { usage: call, cores: 6 },
        system: { hostname: 'test-node', uptime: 60 },
        updatedAt: new Date().toISOString(),
      }),
    });
  });

  await page.goto('/');
  await expect(page.getByText('Loading dashboard...')).toBeHidden();

  const cpuValue = page.locator('.card').filter({ hasText: 'CPU Usage' }).locator('.text-3xl');
  const first = (await cpuValue.textContent())?.trim();
  expect(first).toBe('1.0%');

  // A later poll tick must replace the rendered value within 10s (interval 5s).
  await expect(cpuValue).not.toHaveText(first!, { timeout: 10_000 });
  expect(call).toBeGreaterThanOrEqual(2);
});
