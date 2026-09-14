import { test, expect } from '@playwright/test';

// Shares must sit on a ZFS dataset, so tests have to ask the API which paths are
// shareable rather than assuming one - the picker is dataset-driven, and a path
// that is only a directory under the base is refused (see the last test).
async function shareableDataset(request: any): Promise<string> {
  const res = await request.get('/api/shares/paths');
  expect(res.ok(), await res.text()).toBeTruthy();
  const body = await res.json();
  expect(Array.isArray(body.paths) && body.paths.length > 0,
    'no ZFS dataset available to share - create one first').toBeTruthy();
  return body.paths[0];
}

// The shares page must tell the operator how to reach a share. SMB is served by
// a hostNetwork pod on the node the UI is being browsed from, so the address is
// derived from the page's own host.
test('shares page shows a usable smb:// address for each SMB share', async ({ page, request }) => {
  const name = 'urlspectest';
  const path = await shareableDataset(request);
  const created = await request.post('/api/shares', {
    data: { name, path, protocol: 'smb', description: 'URL display test' }
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

test('shares page shows a usable nfs:// address for each NFS share', async ({ page, request }) => {
  const name = 'urlnfstest';
  const path = await shareableDataset(request);
  const created = await request.post('/api/shares', {
    data: { name, path, protocol: 'nfs', description: 'NFS URL display test' }
  });
  expect(created.status(), await created.text()).toBe(201);

  try {
    await page.goto('/shares');
    await expect(page.getByRole('heading', { name: 'Shares' })).toBeVisible();
    await expect(page.getByText('Loading shares...')).toBeHidden({ timeout: 10_000 });

    const card = page.locator('.card').filter({ hasText: name });
    await expect(card).toBeVisible();

    // NFS is served in userspace by Ganesha on the same node (:2049), and the
    // share is exported under a pseudo path equal to its name, so the address is
    // host:/<name> - exactly the source `mount -t nfs4` expects.
    const expected = `nfs://${new URL(page.url()).hostname}/${name}`;
    const address = card.locator('code');
    await expect(address).toHaveText(expected);

    const text = (await address.textContent())?.trim();
    expect(text).toBe(expected);
    expect(text).toMatch(/^nfs:\/\/[^/]+\/[A-Za-z0-9._-]+$/);
  } finally {
    await request.delete(`/api/shares/${name}`);
  }
});

test('an NFS share is exported to the node as a Ganesha export block', async ({ request }) => {
  const name = 'nfsconfigtest';
  const path = await shareableDataset(request);
  const created = await request.post('/api/shares', {
    data: { name, path, protocol: 'nfs', description: 'NFS config test' }
  });
  expect(created.status(), await created.text()).toBe(201);

  try {
    // The rendered config is what the nfs container actually loads, so it must
    // carry the export's real path, its pseudo path and the v4-only settings.
    const res = await request.get('/api/shares/config/nfs');
    expect(res.ok(), await res.text()).toBeTruthy();
    const conf = await res.text();

    expect(conf).toContain(`Path = ${path};`);
    expect(conf).toContain(`Pseudo = /${name};`);
    expect(conf).toContain('Protocols = 4;');
    expect(conf).toContain('Name = VFS;');
  } finally {
    await request.delete(`/api/shares/${name}`);
  }
});

test('a share can be linked to a new folder created from the share form', async ({ page, request }) => {
  const dataset = await shareableDataset(request);
  const folder = `uifolder${Date.now().toString().slice(-6)}`;
  const name = 'folderlinktest';
  const expected = `${dataset}/${folder}`;

  try {
    await page.goto('/shares');
    await page.click('text=+ New Share');

    const dialog = page.locator('.fixed.inset-0');
    await expect(dialog.getByRole('heading', { name: 'New Share' })).toBeVisible();

    // The picker starts on a dataset, which is what the share would use.
    await expect(dialog.locator('code')).toHaveText(dataset);

    // Create a folder inside the dataset: this is the "link the share to a new
    // folder in the pool" flow, and the picker must follow the new folder.
    await dialog.locator('input[placeholder="New folder name"]').fill(folder);
    await dialog.getByRole('button', { name: 'Create folder' }).click();
    await expect(dialog.locator('code')).toHaveText(expected, { timeout: 10_000 });

    // The folder has to be real on the node, not just rendered in the UI.
    const listing = await request.get(`/api/shares/folders?path=${encodeURIComponent(expected)}`);
    expect(listing.ok(), await listing.text()).toBeTruthy();

    // Save; the share must point at the folder that was just created.
    await dialog.locator('input[type="text"]').first().fill(name);
    await dialog.getByRole('button', { name: 'Create', exact: true }).click();
    await expect(dialog).toBeHidden({ timeout: 10_000 });

    const share = await (await request.get(`/api/shares/${name}`)).json();
    expect(share.path).toBe(expected);

    // Editing the share reopens the picker at that folder, so it can be
    // repointed at another folder later.
    await page.locator('.card').filter({ hasText: name }).getByText('Edit').click();
    const editDialog = page.locator('.fixed.inset-0');
    await expect(editDialog.getByRole('heading', { name: 'Edit Share' })).toBeVisible();
    await expect(editDialog.locator('code')).toHaveText(expected);
  } finally {
    await request.delete(`/api/shares/${name}`);
    await request.delete(`/api/shares/folders?path=${encodeURIComponent(expected)}`);
  }
});

test('folder creation is confined to the ZFS datasets', async ({ request }) => {
  // The folder endpoints are reachable from the UI, so they must refuse a path
  // outside the pool instead of becoming a file-creation primitive on the node.
  const outside = await request.post('/api/shares/folders', {
    data: { path: '/etc', name: 'nope' }
  });
  expect(outside.status(), await outside.text()).toBe(400);

  // Traversal is normalised away before the check.
  const traversal = await request.post('/api/shares/folders', {
    data: { path: '/var/mnt/test/../../etc', name: 'nope' }
  });
  expect(traversal.status(), await traversal.text()).toBe(400);

  // A share cannot be pointed at a folder that does not exist.
  const dataset = await shareableDataset(request);
  const missing = await request.post('/api/shares', {
    data: { name: 'missingfoldertest', path: `${dataset}/does-not-exist`, protocol: 'smb' }
  });
  expect(missing.status(), await missing.text()).toBe(400);
  expect(await missing.text()).toContain('does not exist');
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

    await page.getByRole('button', { name: 'Create', exact: true }).click();
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

// A share whose path is not on a dataset must be refused: Talos keeps /var on
// EPHEMERAL, so such data would live outside every pool (no checksums, no
// snapshots, no redundancy) and be lost on an upgrade - the reported
// "files disappeared" failure. The picker must not offer such paths either.
test('shares can only be created on a ZFS dataset', async ({ page, request }) => {
  const res = await request.post('/api/shares', {
    data: { name: 'trap', path: '/var/mnt/definitely-not-a-dataset', protocol: 'smb' }
  });
  expect(res.status()).toBe(400);
  expect(await res.text()).toMatch(/not on a ZFS dataset/);

  // And the form only offers dataset mountpoints. The picker is filled
  // asynchronously from /api/shares/paths, so wait for it.
  const paths = await (await request.get('/api/shares/paths')).json();
  await page.goto('/shares');
  await page.click('text=+ New Share');
  await expect(page.getByRole('heading', { name: 'New Share' })).toBeVisible();

  const pathSelect = page.locator('select').first();
  await expect(pathSelect).toContainText('/', { timeout: 10_000 });
  const options = await pathSelect.locator('option').allTextContents();
  const offered = options.filter(o => o.startsWith('/'));
  expect(offered.length).toBeGreaterThan(0);
  expect(offered.sort()).toEqual([...paths.paths].sort());
});

test('share form does not offer AFP, which the API rejects', async ({ page }) => {
  await page.goto('/shares');
  await page.click('text=+ New Share');
  await expect(page.getByRole('heading', { name: 'New Share' })).toBeVisible();

  const protocols = page.locator('select').nth(1);
  await expect(protocols.locator('option')).toHaveCount(2);
  await expect(protocols).not.toContainText('AFP');
});
