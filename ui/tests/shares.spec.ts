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

test('a share can be restricted to an LDAP group from the form', async ({ page, request }) => {
  const name = 'grouppickertest';

  // The picker lists real directory groups, so one must exist.
  const groups = await (await request.get('/api/groups')).json();
  expect(Array.isArray(groups) && groups.length > 0, 'no LDAP groups to pick from').toBeTruthy();
  const groupName = groups[0].cn;

  try {
    await page.goto('/shares');
    await page.click('text=+ New Share');
    await expect(page.getByRole('heading', { name: 'New Share' })).toBeVisible();

    await page.fill('input[type="text"]', name);
    await page.locator('select').first().selectOption({ index: 1 });

    // The group checkbox is reachable by the group's own name.
    const groupBox = page.getByRole('checkbox', { name: groupName });
    await expect(groupBox).toBeVisible();
    await groupBox.check();

    await page.getByRole('button', { name: 'Create' }).click();
    await expect(page.getByRole('heading', { name: 'New Share' })).toBeHidden({ timeout: 10_000 });

    // Stored as a group, not as a user name.
    const shares = await (await request.get('/api/shares')).json();
    const created = shares.find((s: any) => s.name === name);
    expect(created, 'share was not created').toBeTruthy();
    expect(created.validGroups).toEqual([groupName]);
    expect(created.validUsers).toEqual([]);

    // And surfaced on the card so the access rule is visible at a glance.
    const card = page.locator('.card').filter({ hasText: name });
    await expect(card).toContainText(`groups ${groupName}`);

    // The rendered config must grant access to the group, not to a bare name.
    const conf = await (await request.get('/api/shares/config/samba')).text();
    expect(conf).toContain(`valid users = @${groupName}`);
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
