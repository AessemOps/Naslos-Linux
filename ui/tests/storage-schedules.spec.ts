import { test, expect } from '@playwright/test';

// Scheduled storage maintenance (FR-STO-10/11). The schedules created here are
// removed in the same test, and the cadence is weekly so nothing fires while
// the suite runs. A scrub is never started: that is an IO-heavy operation on
// the live node.

async function firstPool(request: any): Promise<{ name: string }> {
  const res = await request.get('/api/volumes/zfs');
  expect(res.ok(), await res.text()).toBeTruthy();
  const pools = await res.json();
  expect(Array.isArray(pools) && pools.length > 0, 'no pool to test against').toBeTruthy();
  return pools[0];
}

test('a snapshot schedule can be created, listed and removed from the pool page', async ({
  page,
  request
}) => {
  const pool = await firstPool(request);
  const name = `e2e-sched-${Date.now().toString().slice(-6)}`;
  const full = `${pool.name}/${name}`;

  const created = await request.post('/api/datasets', {
    data: { pool: pool.name, name, options: { compression: 'lz4' } }
  });
  expect(created.ok(), await created.text()).toBeTruthy();

  try {
    await page.goto(`/pools/${pool.name}`);
    await expect(page.getByRole('heading', { name: pool.name, exact: true })).toBeVisible();

    const row = page.locator('div.bg-naslos-dark').filter({ hasText: full }).first();
    await expect(row).toBeVisible();
    await row.getByTitle('Schedule periodic snapshots').click();

    const dialog = page.locator('.fixed.inset-0');
    await expect(
      dialog.getByRole('heading', { name: `Snapshot schedule for ${full}` })
    ).toBeVisible();

    // Cadence and retention are explicit fields.
    await dialog.getByLabel('Cadence').selectOption('weekly');
    await dialog.getByLabel('Run at (UTC)').fill('Sun 03:30');
    // Changing the cadence rewrites `runAt` into the shape that cadence needs,
    // instead of leaving a weekly value that the API rejects for daily.
    await dialog.getByLabel('Cadence').selectOption('daily');
    await expect(dialog.getByLabel('Run at (UTC)')).toHaveValue('03:30');
    await dialog.getByLabel('Cadence').selectOption('weekly');
    await expect(dialog.getByLabel('Run at (UTC)')).toHaveValue('Sun 03:30');
    await dialog.getByLabel('Run at (UTC)').fill('Sun 03:30');
    await dialog.getByLabel('Keep newest').fill('3');
    await dialog.getByRole('button', { name: 'Save schedule' }).click();
    await expect(dialog).toBeHidden({ timeout: 15_000 });

    const list = await (await request.get('/api/storage/schedules')).json();
    const saved = list.schedules.find((s: any) => s.target === full && s.kind === 'snapshot');
    expect(saved, `${full} had no saved schedule`).toBeTruthy();
    expect(saved.cadence).toBe('weekly');
    expect(saved.keep).toBe(3);
    expect(saved.nextRun).toBeTruthy();

    // The page lists it with the schedule-owned naming rule spelled out.
    await expect(page.getByRole('heading', { name: 'Scheduled snapshots' })).toBeVisible();
    const card = page.locator('div.bg-naslos-dark').filter({ hasText: full }).filter({ hasText: 'keep 3' });
    await expect(card).toBeVisible();
    await card.getByRole('button', { name: 'Remove' }).click();
    const confirmDialog = page.locator('.fixed.inset-0');
    await expect(
      confirmDialog.getByRole('heading', { name: 'Delete schedule' })
    ).toBeVisible();
    await confirmDialog.getByRole('button', { name: 'Delete schedule' }).click();

    await expect
      .poll(async () => {
        const after = await (await request.get('/api/storage/schedules')).json();
        return after.schedules.some((s: any) => s.id === saved.id);
      })
      .toBe(false);
  } finally {
    const schedules = await (await request.get('/api/storage/schedules')).json().catch(() => null);
    for (const s of schedules?.schedules ?? []) {
      if (s.target === full) {
        await request.delete(`/api/storage/schedules?id=${encodeURIComponent(s.id)}`);
      }
    }
    await request.delete(`/api/datasets?name=${encodeURIComponent(full)}&confirm=${encodeURIComponent(full)}`);
  }
});

test('schedule validation happens before anything reaches ZFS', async ({ request }) => {
  const pool = await firstPool(request);

  // Snapshot targets are dataset paths.
  for (const target of ['not-a-dataset', '../tank/data', `${pool.name}@snap`]) {
    const res = await request.post('/api/storage/schedules', {
      data: { kind: 'snapshot', target, cadence: 'daily', runAt: '02:30' }
    });
    expect(res.status(), target).toBe(400);
  }

  // A scrub schedule is daily/weekly only and cannot retain snapshots.
  const hourly = await request.post('/api/storage/schedules', {
    data: { kind: 'scrub', target: pool.name, cadence: 'hourly' }
  });
  expect(hourly.status()).toBe(400);
  expect(await hourly.text()).toContain('daily or weekly');

  const keep = await request.post('/api/storage/schedules', {
    data: { kind: 'scrub', target: pool.name, cadence: 'weekly', runAt: 'Sun 02:30', keep: 3 }
  });
  expect(keep.status()).toBe(400);
  expect(await keep.text()).toContain('cannot keep snapshots');

  // An unknown pool is refused instead of scheduling a scrub that can never run.
  const unknown = await request.post('/api/storage/schedules', {
    data: { kind: 'scrub', target: 'nosuchpool123', cadence: 'weekly', runAt: 'Sun 02:30' }
  });
  expect(unknown.status()).toBe(400);
  expect(await unknown.text()).toContain('not known');
});
