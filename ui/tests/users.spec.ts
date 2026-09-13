import { test, expect } from '@playwright/test';

test('users page loads and can open new user form', async ({ page }) => {
  await page.goto('/users');
  await expect(page.getByRole('heading', { name: 'Users', exact: true })).toBeVisible();
  await page.click('text=+ New User');
  await expect(page.getByRole('heading', { name: 'New User', exact: true })).toBeVisible();
  await page.fill('input[type="text"]', 'testuser');
  await page.fill('input[type="password"]', 'TestPass123!');
});

// Changing a password does not reach SMB instantly and does not affect sessions
// that are already open, so the edit dialog has to say so - otherwise "the new
// password doesn't work" and "the old one still works" both look like bugs.
test('edit user dialog warns about share timing and stale sessions', async ({ page, request }) => {
  const uid = 'disclaimertest';
  const created = await request.post('/api/users', {
    data: {
      uid,
      displayName: 'Disclaimer Test',
      firstName: 'Disclaimer',
      lastName: 'Test',
      email: 'disclaimer@naslos.local',
      password: 'DisclaimerPass1',
    },
  });
  expect(created.status(), await created.text()).toBe(201);

  try {
    await page.goto('/users');
    await expect(page.getByText('Loading users...')).toBeHidden({ timeout: 10_000 });

    const row = page.locator('tr').filter({ hasText: uid });
    await expect(row).toBeVisible();
    await row.getByRole('button', { name: 'Edit' }).click();

    await expect(page.getByRole('heading', { name: 'Edit User' })).toBeVisible();

    const disclaimer = page.getByText('Applying a password to file shares takes a moment');
    await expect(disclaimer).toBeVisible();

    // The time it takes to reach SMB must be stated.
    await expect(page.getByText(/about 3 seconds/)).toBeVisible();

    // And the stale-session caveat, which only matters when editing.
    await expect(
      page.getByText(/already connected keep working with the old password until that client reconnects/)
    ).toBeVisible();

    // The user must be told to reconnect, since nothing server-side can force it.
    await expect(page.getByText(/disconnect and reconnect/i)).toBeVisible();
  } finally {
    await request.delete(`/api/users/${uid}`);
  }
});

// The stale-session note is irrelevant for a brand new account, so it is only
// shown when editing an existing user.
test('new user dialog states the timing but not stale sessions', async ({ page }) => {
  await page.goto('/users');
  await page.click('text=+ New User');
  await expect(page.getByRole('heading', { name: 'New User' })).toBeVisible();

  await expect(page.getByText(/about 3 seconds/)).toBeVisible();
  await expect(page.getByText(/already connected keep working with the old password/)).toHaveCount(0);
});
