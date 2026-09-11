import { test, expect } from '@playwright/test';

test('groups page loads and can open new group form', async ({ page }) => {
  await page.goto('/groups');
  await expect(page.getByRole('heading', { name: 'Groups', exact: true })).toBeVisible();
  await page.click('text=+ New Group');
  await expect(page.getByRole('heading', { name: 'New Group', exact: true })).toBeVisible();
  await page.fill('input[type="text"]', 'testgroup');
});
