import { test, expect } from '@playwright/test';

// The shares page must tell the operator how to reach a share. SMB is served by
// a hostNetwork pod on the node the UI is being browsed from, so the address is
// derived from the page's own host.
test('shares page shows a usable smb:// address for each SMB share', async ({ page, request }) => {
  const name = 'urlspectest';
  const created = await request.post('/api/shares', {
    data: { name, path: '/var/mnt/tank', protocol: 'smb', description: 'URL display test' }
  });
  expect(created.status(), await created.text()).toBe(201);

  try {
    await page.goto('/shares');
    await expect(page.getByRole('heading', { name: 'Shares' })).toBeVisible();
    await expect(page.getByText('Loading shares...')).toBeHidden({ timeout: 10_000 });

    const card = page.locator('.card').filter({ hasText: name });
    await expect(card).toBeVisible();

    // The address must include the host the operator is browsing on and the
    // share name, i.e. exactly what they would type into Finder/Explorer.
    const expected = `smb://${new URL(page.url()).hostname}/${name}`;
    const address = card.locator('code');
    await expect(address).toHaveText(expected);

    // It must be presentable to a client as-is (selectable, not truncated).
    const text = (await address.textContent())?.trim();
    expect(text).toBe(expected);
    expect(text).toMatch(/^smb:\/\/[^/]+\/[A-Za-z0-9._-]+$/);
  } finally {
    await request.delete(`/api/shares/${name}`);
  }
});

test('share form does not offer AFP, which the API rejects', async ({ page }) => {
  await page.goto('/shares');
  await page.click('text=+ New Share');
  await expect(page.getByRole('heading', { name: 'New Share' })).toBeVisible();

  const protocols = page.locator('select').nth(1);
  await expect(protocols.locator('option')).toHaveCount(2);
  await expect(protocols).not.toContainText('AFP');
});
