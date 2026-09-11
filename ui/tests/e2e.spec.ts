import { test, expect } from '@playwright/test';

const unique = () => Math.random().toString(36).substring(2, 8);

test('full user and group lifecycle', async ({ page }) => {
  const uid = 'jdoe' + unique();
  const cn = 'testgroup' + unique();

  page.on('dialog', dialog => dialog.accept());

  // Create a user
  await page.goto('/users');
  await page.click('text=+ New User');
  const textInputs = page.locator('input[type="text"]');
  await textInputs.nth(0).fill(uid);
  await textInputs.nth(1).fill('John');
  await textInputs.nth(2).fill('Doe');
  await textInputs.nth(3).fill('John Doe');
  await page.fill('input[type="email"]', `${uid}@example.com`);
  await page.fill('input[type="password"]', 'TestPass123!');
  await page.getByRole('button', { name: 'Create', exact: true }).click();
  await expect(page.locator('tr', { hasText: uid })).toBeVisible();

  // Create a group
  await page.goto('/groups');
  await page.click('text=+ New Group');
  await page.fill('input[placeholder="e.g. naslos_users"]', cn);
  await page.fill('input[placeholder="e.g. Standard users"]', 'Test group');
  await page.getByRole('button', { name: 'Create', exact: true }).click();
  await expect(page.locator('div.card', { hasText: cn })).toBeVisible();

  // Edit group to add user
  await page.locator('div.card', { hasText: cn }).getByRole('button', { name: 'Edit' }).click();
  await page.click('label:has-text("John Doe") input[type="checkbox"]');
  await page.getByRole('button', { name: 'Update' }).click();
  await expect(page.locator('div.card', { hasText: cn }).getByText('1 member')).toBeVisible();
  // Verify the full DN is NOT shown (the fix we made)
  await expect(page.locator('div.card', { hasText: cn }).getByText(/ou=people,dc=naslos/)).not.toBeVisible();

  // Delete the group
  await page.locator('div.card', { hasText: cn }).getByRole('button', { name: 'Delete' }).click();
  await expect(page.locator('div.card', { hasText: cn })).not.toBeVisible();

  // Delete the user
  await page.goto('/users');
  await page.locator('tr', { hasText: uid }).getByRole('button', { name: 'Delete' }).click();
  await expect(page.locator('tr', { hasText: uid })).not.toBeVisible();
});
