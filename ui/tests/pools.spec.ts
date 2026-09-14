import { test, expect } from '@playwright/test';

// Pool management tests. They must never touch the existing vdevs: `zpool add`
// cannot be undone in general, so the add-drive path is verified by its refusals
// and by the fact that the pool's members are unchanged.
async function firstPool(request: any): Promise<{ name: string; disks: string[] }> {
  const res = await request.get('/api/volumes/zfs');
  expect(res.ok(), await res.text()).toBeTruthy();
  const pools = await res.json();
  expect(Array.isArray(pools) && pools.length > 0, 'no pool to test against').toBeTruthy();
  return pools[0];
}

test('a dataset can be created and destroyed from the pool page', async ({ page, request }) => {
  const pool = await firstPool(request);
  const name = `e2e${Date.now().toString().slice(-6)}`;
  const full = `${pool.name}/${name}`;

  try {
    await page.goto(`/pools/${pool.name}`);
    await expect(page.getByRole('heading', { name: pool.name, exact: true })).toBeVisible();
    await expect(page.getByRole('heading', { name: 'Datasets' })).toBeVisible();

    await page.click('text=New Dataset');
    const dialog = page.locator('.fixed.inset-0');
    await expect(dialog.getByRole('heading', { name: 'New Dataset' })).toBeVisible();

    await dialog.locator('input[type="text"]').first().fill(name);
    await dialog.getByRole('button', { name: 'Create Dataset' }).click();
    await expect(dialog).toBeHidden({ timeout: 15_000 });

    // The dataset must exist on the node, with a mountpoint the share picker can
    // use, not just appear in the list.
    const datasets = await (await request.get('/api/datasets')).json();
    const created = datasets.find((d: any) => d.name === full);
    expect(created, `${full} was not created`).toBeTruthy();
    expect(created.mountpoint).toContain('/var/mnt/');

    const paths = await (await request.get('/api/shares/paths')).json();
    expect(paths.paths).toContain(created.mountpoint);

    // Destroy it again from the row's Delete action.
    const row = page.locator('div.bg-naslos-dark').filter({ hasText: full });
    await expect(row).toHaveCount(1);
    await row.getByTitle('Destroy this dataset').click();
    // The row must disappear (asserting on the row, not the text: the mountpoint
    // contains the dataset name as a substring).
    await expect(page.locator('div.bg-naslos-dark').filter({ hasText: full })).toHaveCount(0, { timeout: 15_000 });

    const after = await (await request.get('/api/datasets')).json();
    expect(after.find((d: any) => d.name === full)).toBeFalsy();
  } finally {
    // Best effort: if a UI step failed halfway, the dataset would linger.
    await request.delete(`/api/datasets?name=${encodeURIComponent(full)}`);
  }
});

test('dataset creation is validated before anything reaches ZFS', async ({ request }) => {
  const pool = await firstPool(request);

  const cases = [
    { body: { pool: 'nosuchpool123', name: 'x' }, want: 'not found' },
    { body: { pool: pool.name, name: '../escape' }, want: 'invalid dataset name' },
    { body: { pool: pool.name, name: '' }, want: 'required' },
    { body: { pool: pool.name, name: 'x@snap' }, want: 'snapshot' },
    { body: { pool: pool.name, name: 'ok', options: { mountpoint: '/' } }, want: 'unsupported dataset option' },
    { body: { pool: pool.name, name: 'ok', options: { compression: 'fastest' } }, want: 'unsupported compression' },
    { body: { pool: pool.name, name: 'ok', options: { quota: 'lots' } }, want: 'invalid quota' },
  ];

  for (const tc of cases) {
    const res = await request.post('/api/datasets', { data: tc.body });
    expect(res.status(), JSON.stringify(tc.body)).toBe(400);
    expect(await res.text()).toContain(tc.want);
  }
});

test('adding drives is refused for disks the pool already owns', async ({ request }) => {
  const pool = await firstPool(request);

  // The pool's own disks must be refused, with or without force: -f would
  // overwrite their label and destroy the pool.
  for (const force of [false, true]) {
    const res = await request.post(`/api/volumes/zfs/${pool.name}/devices`, {
      data: { disks: [pool.disks[0]], topology: 'single', force }
    });
    expect(res.status(), await res.text()).toBe(400);
    expect(await res.text()).toContain('already belongs to pool');
  }

  const others = [
    { data: { disks: ['/dev/does-not-exist'], topology: 'single' }, want: 'not a disk this node can use' },
    { data: { disks: ['vdb'], topology: 'single' }, want: 'absolute path' },
    { data: { disks: [], topology: 'single' }, want: 'select at least one disk' },
    { data: { disks: [pool.disks[0]], topology: 'raidz9' }, want: 'unsupported topology' },
    { data: { disks: ['/dev/vdz'], topology: 'raidz2' }, want: 'needs at least 3' },
  ];
  for (const tc of others) {
    const res = await request.post(`/api/volumes/zfs/${pool.name}/devices`, { data: tc.data });
    expect(res.status(), JSON.stringify(tc.data)).toBe(400);
    expect(await res.text()).toContain(tc.want);
  }

  const unknownPool = await request.post('/api/volumes/zfs/nosuchpool123/devices', {
    data: { disks: ['/dev/vdb'], topology: 'single' }
  });
  expect(unknownPool.status()).toBe(400);
  expect(await unknownPool.text()).toContain('not found');

  // Nothing may have changed on the pool.
  const after = await firstPool(request);
  expect(after.disks).toEqual(pool.disks);
});

test('the add drive dialog shows in-use disks as unavailable', async ({ page, request }) => {
  const pool = await firstPool(request);

  await page.goto(`/pools/${pool.name}`);
  await page.click('text=Add Drive');

  const dialog = page.locator('.fixed.inset-0');
  await expect(dialog.getByRole('heading', { name: `Add Drive to ${pool.name}` })).toBeVisible();

  // Every disk this test's pool owns must be marked, unselectable, and the
  // submit button must stay disabled.
  for (const disk of pool.disks) {
    const row = dialog.locator('label').filter({ hasText: disk });
    await expect(row.getByText(`in pool ${pool.name}`)).toBeVisible();
    await expect(row.locator('input[type="checkbox"]')).toBeDisabled();
  }
  await expect(dialog.getByRole('button', { name: /^Add/ })).toBeDisabled();

  // The consequence of the selected topology is stated up front (the same words
  // also appear in the <option>, so target the hint paragraph).
  await expect(dialog.locator('p').filter({ hasText: 'NO redundancy' })).toBeVisible();
});

