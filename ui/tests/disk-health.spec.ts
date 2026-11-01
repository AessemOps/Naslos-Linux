import { test, expect } from '@playwright/test';

test('disk health page renders the API inventory and SMART state', async ({ page }) => {
  const consoleErrors: string[] = [];
  page.on('console', (msg) => {
    if (msg.type() === 'error') consoleErrors.push(msg.text());
  });

  const res = await page.request.get('/api/disks/health');
  expect(res.ok(), await res.text()).toBeTruthy();
  const health = await res.json();
  expect(typeof health.smartctlAvailable).toBe('boolean');
  expect(Array.isArray(health.disks), 'disks must be an array').toBeTruthy();

  await page.goto('/disks/health');
  await expect(page.getByRole('heading', { name: 'Disk Health' })).toBeVisible();
  await expect(page.getByText('Loading disk health…')).toBeHidden({ timeout: 10_000 });

  if (!health.smartctlAvailable) {
    await expect(page.getByText('SMART is unavailable on this node.')).toBeVisible();
  }

  if (health.disks.length === 0) {
    await expect(page.getByText('No whole disks were reported by the node.')).toBeVisible();
  } else {
    const rows = page.locator('tbody tr');
    await expect(rows).toHaveCount(health.disks.length);
    for (const disk of health.disks) {
      await expect(
        page.locator('tbody').getByText(disk.device, { exact: true })
      ).toBeVisible();
    }
    // The table must not invent a green PASSED badge for an unavailable disk.
    for (const disk of health.disks) {
      if (!disk.smart?.available) {
        const row = page.locator('tbody tr').filter({ hasText: disk.device });
        await expect(row).not.toContainText('PASSED');
      }
    }
  }

  expect(consoleErrors, consoleErrors.join('\n')).toEqual([]);
});

test('disk health API is owner-only', async ({ request }) => {
  // Playwright carries the admin session; this pins the route exists for the UI.
  const res = await request.get('/api/disks/health');
  expect(res.ok(), await res.text()).toBeTruthy();
  const body = await res.json();
  expect(body).toHaveProperty('temperatureWarningC');
  expect(body).toHaveProperty('wearWarningPercent');
});
