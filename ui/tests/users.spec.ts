import { test, expect } from '@playwright/test';

test('users page loads and can open new user form', async ({ page }) => {
  await page.goto('/users');
  await expect(page.getByRole('heading', { name: 'Users', exact: true })).toBeVisible();
  await page.click('text=+ New User');
  await expect(page.getByRole('heading', { name: 'New User', exact: true })).toBeVisible();
  await page.fill('input[type="text"]', 'testuser');
  await page.fill('input[type="password"]', 'TestPass123!');
});
