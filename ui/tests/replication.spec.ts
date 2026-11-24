import { test, expect } from '@playwright/test';

// Local replication (FR-REP) UI. Two throwaway sibling datasets are created for
// the pair and destroyed afterwards; the job itself copies an empty dataset, so
// no user data is read or overwritten.

async function firstPool(request: any): Promise<{ name: string }> {
  const res = await request.get('/api/volumes/zfs');
  expect(res.ok(), await res.text()).toBeTruthy();
  const pools = await res.json();
  expect(Array.isArray(pools) && pools.length > 0, 'no pool to test against').toBeTruthy();
  return pools[0];
}

test('a local replication job runs to completion from the replication page', async ({
  page,
  request
}) => {
  // Job polling can legitimately take longer than the 30 s default test budget
  // on a busy node, and the budget must exceed the poll timeout below.
  test.setTimeout(90_000);
  const pool = await firstPool(request);
  const suffix = Date.now().toString().slice(-6);
  const source = `${pool.name}/e2e-rep-src-${suffix}`;
  const destination = `${pool.name}/e2e-rep-dst-${suffix}`;

  try {
    // Create the pair inside the try: a partially failed setup must still hit
    // the cleanup below instead of leaking the first dataset into the pool.
    for (const name of [source, destination]) {
      const res = await request.post('/api/datasets', {
        data: {
          pool: pool.name,
          name: name.slice(pool.name.length + 1),
          options: { compression: 'lz4' }
        }
      });
      expect(res.ok(), await res.text()).toBeTruthy();
    }

    await page.goto('/replication');
    await expect(page.getByRole('heading', { name: 'Replication', exact: true })).toBeVisible();
    await expect(page.getByRole('heading', { name: 'Running' })).toBeVisible();

    await page.getByRole('button', { name: 'New replication' }).click();
    const dialog = page.locator('.fixed.inset-0');
    await expect(dialog.getByRole('heading', { name: 'New replication' })).toBeVisible();

    const sourceSelect = dialog.getByLabel('Source dataset *');
    const destinationSelect = dialog.getByLabel('Destination dataset *');
    await sourceSelect.selectOption(source);

    // An identical pair is refused in the browser: the start button stays
    // disabled and no request is made.
    await destinationSelect.selectOption(source);
    await expect(dialog.getByText('must be different datasets')).toBeVisible();
    await expect(dialog.getByRole('button', { name: 'Start replication' })).toBeDisabled();

    await destinationSelect.selectOption(destination);
    await expect(dialog.getByRole('button', { name: 'Start replication' })).toBeEnabled();
    await dialog.getByRole('button', { name: 'Start replication' }).click();
    await expect(dialog).toBeHidden({ timeout: 20_000 });
    await expect(page.getByRole('status').filter({ hasText: 'Replication started' })).toBeVisible();

    // The API owns the job; poll it to a terminal state rather than racing the
    // page's refresh interval.
    await expect
      .poll(
        async () => {
          const res = await request.get('/api/replication/jobs');
          const data = await res.json();
          return data.jobs?.find((j: any) => j.source === source && j.destination === destination)
            ?.state;
        },
        { timeout: 40_000 }
      )
      .toBe('succeeded');

    const jobs = await (await request.get('/api/replication/jobs')).json();
    const job = jobs.jobs.find((j: any) => j.source === source && j.destination === destination);
    expect(job.result, JSON.stringify(job)).toBeTruthy();
    expect(['replicated', 'up-to-date']).toContain(job.result.status);
    expect(job.result.snapshot).toBeTruthy();

    // The page shows the finished job in its history with an explicit result.
    const history = page.getByRole('region', { name: 'History' });
    const row = history.locator('div.bg-naslos-dark').filter({ hasText: `${source} → ${destination}` });
    await expect(row).toBeVisible({ timeout: 20_000 });
    await expect(row.getByText(job.result.status, { exact: false })).toBeVisible();
  } finally {
    // A job still writing the destination would block the destroy; cancel it
    // first, then remove both datasets (replication-owned snapshots included).
    const listed = await (await request.get('/api/replication/jobs')).json().catch(() => null);
    for (const job of listed?.jobs ?? []) {
      if (job.state === 'running' && (job.source === source || job.destination === destination)) {
        await request.delete(`/api/replication/jobs/${job.id}`);
      }
    }
    for (const name of [source, destination]) {
      await request.delete(
        `/api/datasets?name=${encodeURIComponent(name)}&recursive=true&confirm=${encodeURIComponent(name)}`
      );
    }
  }
});

test('replication pairs are validated before any stream starts', async ({ request }) => {
  const pool = await firstPool(request);
  const path = `${pool.name}/e2e-rep-check-${Date.now().toString().slice(-6)}`;

  // Identical and nested pairs are refused by the API before it looks at the
  // datasets, so this leaves nothing behind.
  const identical = await request.post('/api/replication/send', {
    data: { source: path, destination: path }
  });
  expect(identical.status(), await identical.text()).toBe(400);
  expect(await identical.text()).toContain('different');

  const nested = await request.post('/api/replication/send', {
    data: { source: path, destination: `${path}/child` }
  });
  expect(nested.status(), await nested.text()).toBe(400);
  expect(await nested.text()).toContain('inside');

  const retention = await request.post('/api/replication/send', {
    data: { source: path, destination: `${pool.name}/other`, pruneKeep: 1001 }
  });
  expect(retention.status(), await retention.text()).toBe(400);
  expect(await retention.text()).toContain('at most 1000');
});
