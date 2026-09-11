import { test, expect } from '@playwright/test';

const unique = () => Math.random().toString(36).substring(2, 8);

test('groups page loads and can open new group form', async ({ page }) => {
  await page.goto('/groups');
  await expect(page.getByRole('heading', { name: 'Groups', exact: true })).toBeVisible();
  await page.click('text=+ New Group');
  await expect(page.getByRole('heading', { name: 'New Group', exact: true })).toBeVisible();
});

test('can create a group with empty description', async ({ page }) => {
  const cn = 'testgroup' + unique();
  page.on('dialog', dialog => dialog.accept());

  await page.goto('/groups');
  await page.click('text=+ New Group');
  await page.fill('input[type="text"]', cn);
  // Leave description empty
  await page.getByRole('button', { name: 'Create', exact: true }).click();
  await expect(page.locator('div.card', { hasText: cn })).toBeVisible();
  // Clean up
  await page.locator('div.card', { hasText: cn }).getByRole('button', { name: 'Delete' }).click();
  await expect(page.locator('div.card', { hasText: cn })).not.toBeVisible();
});
