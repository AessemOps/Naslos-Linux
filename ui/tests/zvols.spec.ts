import { test, expect } from '@playwright/test';

// Block volumes (FR-ZVOL). The zvol created here is small, empty, and destroyed
// in the same test, so the live pool is unchanged afterwards.

async function firstPool(request: any): Promise<{ name: string }> {
  const res = await request.get('/api/volumes/zfs');
  expect(res.ok(), await res.text()).toBeTruthy();
  const pools = await res.json();
  expect(Array.isArray(pools) && pools.length > 0, 'no pool to test against').toBeTruthy();
  return pools[0];
}

test('a zvol can be created, resized and destroyed from the pool page', async ({ page, request }) => {
  const pool = await firstPool(request);
  const name = `e2e-zvol-${Date.now().toString().slice(-6)}`;
  const full = `${pool.name}/${name}`;

  try {
    await page.goto(`/pools/${pool.name}`);
    await expect(page.getByRole('heading', { name: pool.name, exact: true })).toBeVisible();
    await expect(page.getByRole('heading', { name: 'Block volumes (zvols)' })).toBeVisible();

    await page.getByRole('button', { name: 'New zvol' }).click();
    const dialog = page.locator('.fixed.inset-0');
    await expect(dialog.getByRole('heading', { name: 'New zvol' })).toBeVisible();

    await dialog.getByLabel('Name *', { exact: true }).fill(name);
    await dialog.getByLabel('Size *', { exact: true }).fill('64M');
    await dialog.getByRole('button', { name: 'Create zvol' }).click();
    await expect(dialog).toBeHidden({ timeout: 20_000 });

    const row = page.locator('div.bg-naslos-dark').filter({ hasText: full });
    await expect(row).toBeVisible();

    // The zvol must exist as a real block volume on the node.
    const listed = await (await request.get(`/api/zvols?pool=${encodeURIComponent(pool.name)}`)).json();
    const created = listed.find((z: any) => z.name === full);
    expect(created, `${full} was not created`).toBeTruthy();
    expect(created.volsizeBytes).toBe(64 * 1024 * 1024);

    // Growing is allowed; the dialog refuses to shrink only at the API, so the
    // UI sends the size and surfaces the refusal.
    await row.getByRole('button', { name: 'Resize' }).click();
    const resizeDialog = page.locator('.fixed.inset-0');
    await expect(resizeDialog.getByRole('heading', { name: `Resize ${full}` })).toBeVisible();
    await resizeDialog.getByLabel('New size *', { exact: true }).fill('128M');
    await resizeDialog.getByRole('button', { name: 'Resize', exact: true }).click();
    await expect(resizeDialog).toBeHidden({ timeout: 20_000 });

    await expect
      .poll(async () => {
        const after = await (await request.get(`/api/zvols?pool=${encodeURIComponent(pool.name)}`)).json();
        return after.find((z: any) => z.name === full)?.volsizeBytes;
      })
      .toBe(128 * 1024 * 1024);

    // Destroying needs the full name typed; a dismissed prompt sends nothing.
    const deletes: string[] = [];
    page.on('request', (req) => {
      if (req.method() === 'DELETE' && req.url().includes('/api/zvols')) deletes.push(req.url());
    });
    page.once('dialog', (d) => d.dismiss());
    await row.getByRole('button', { name: 'Delete' }).click();
    await expect(row).toBeVisible();
    expect(deletes).toHaveLength(0);

    page.once('dialog', (d) => d.accept(full));
    await row.getByRole('button', { name: 'Delete' }).click();
    await expect
      .poll(() => deletes.some((url) => url.includes(`confirm=${encodeURIComponent(full)}`)))
      .toBe(true);
    await expect(page.locator('div.bg-naslos-dark').filter({ hasText: full })).toHaveCount(0, {
      timeout: 20_000
    });
  } finally {
    await request.delete(
      `/api/zvols?name=${encodeURIComponent(full)}&confirm=${encodeURIComponent(full)}`
    );
  }
});

test('zvol creation is validated before anything reaches ZFS', async ({ request }) => {
  const pool = await firstPool(request);

  const cases = [
    { body: { pool: pool.name, name: 'x', size: 'lots' }, want: 'invalid zvol size' },
    { body: { pool: pool.name, name: '../escape', size: '1G' }, want: 'invalid dataset name' },
    {
      body: { pool: pool.name, name: 'x', size: '1G', options: { quota: '1G' } },
      want: 'dataset option'
    },
    {
      body: { pool: pool.name, name: 'x', size: '1G', options: { volblocksize: '3000' } },
      want: 'volblocksize'
    },
    { body: { pool: 'nosuchpool123', name: 'x', size: '1G' }, want: 'not found' }
  ];

  for (const tc of cases) {
    const res = await request.post('/api/zvols', { data: tc.body });
    expect(res.status(), JSON.stringify(tc.body)).toBe(400);
    expect(await res.text()).toContain(tc.want);
  }

  // A resize requires the full dataset path and cannot be a shrink.
  const name = `e2e-zvol-${Date.now().toString().slice(-6)}`;
  const full = `${pool.name}/${name}`;
  const created = await request.post('/api/zvols', {
    data: { pool: pool.name, name, size: '64M', options: { compression: 'lz4' } }
  });
  expect(created.ok(), await created.text()).toBeTruthy();
  try {
    const shrink = await request.put(`/api/zvols?name=${encodeURIComponent(full)}`, {
      data: { size: '32M' }
    });
    expect(shrink.ok()).toBeFalsy();
    expect(await shrink.text()).toContain('shrink');

    const confirmMismatch = await request.delete(
      `/api/zvols?name=${encodeURIComponent(full)}&confirm=wrong`
    );
    expect(confirmMismatch.status(), await confirmMismatch.text()).toBe(400);
    expect(await confirmMismatch.text()).toContain('confirm');
  } finally {
    await request.delete(
      `/api/zvols?name=${encodeURIComponent(full)}&confirm=${encodeURIComponent(full)}`
    );
  }
});
