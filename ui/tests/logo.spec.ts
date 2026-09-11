import { test, expect } from '@playwright/test';

test('sidebar shows the naslos logo in the top-left corner', async ({ page }) => {
  const response = await page.request.get('/logo.png');
  expect(response.ok()).toBeTruthy();
  // Must be a real PNG; nginx's SPA fallback would otherwise 200 with index.html.
  expect(response.headers()['content-type']).toContain('image/png');

  await page.goto('/');
  const logo = page.locator('aside').locator('img[alt="Naslos logo"]');
  await expect(logo).toBeVisible();
  // The logo sits in the top-left header block, next to the Naslos title.
  const header = logo.locator('xpath=..');
  await expect(header.getByRole('heading', { name: 'Naslos' })).toBeVisible();
  const box = await logo.boundingBox();
  expect(box).not.toBeNull();
  expect(box!.width).toBeLessThanOrEqual(64); // compact icon, not the raw 1254px image
});
