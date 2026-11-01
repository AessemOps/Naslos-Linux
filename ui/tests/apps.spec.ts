import { test, expect, type APIRequestContext } from '@playwright/test';

// waitForJob polls the async job endpoint until the job reaches a terminal
// state (FR-APP-18). Returns the final state.
async function waitForJob(request: APIRequestContext, jobId: string, timeout: number): Promise<string> {
  let state = 'running';
  await expect
    .poll(
      async () => {
        const res = await request.get(`/api/apps/jobs/${jobId}`);
        if (!res.ok()) return `http-${res.status()}`;
        const job = await res.json();
        state = job.state;
        return state;
      },
      { timeout }
    )
    .not.toBe('running');
  return state;
}

// The installer profile disables the shared /var/mnt PV: an app must attach a
// registered dataset, and the API then creates its per-app claim. Keep this E2E
// test self-contained by creating and registering a small dataset on the pool.
async function ensureTestDataset(request: APIRequestContext): Promise<{ name: string; dataset: string } | null> {
  const res = await request.get('/api/datasets');
  if (!res.ok()) return null;
  const datasets = await res.json();
  if (!Array.isArray(datasets)) return null;
  const poolRoot = datasets.find(
    (d: any) =>
      d.mounted &&
      typeof d.mountpoint === 'string' &&
      d.mountpoint.startsWith('/var/mnt/') &&
      typeof d.name === 'string' &&
      !d.name.includes('/')
  );
  if (!poolRoot) return null;

  const name = 'e2e-apps';
  const dataset = `${poolRoot.name}/${name}`;
  const created = await request.post('/api/datasets', {
    data: { pool: poolRoot.name, name, options: {} }
  });
  // 409 is a leftover from an interrupted run; the dataset is usable.
  if (![201, 409].includes(created.status())) return null;

  const hostPath = `${poolRoot.mountpoint.replace(/\/$/, '')}/${name}`;
  const registered = await request.post('/api/datasets/registry', {
    data: { name, hostPath, shared: false }
  });
  if (![201, 409].includes(registered.status())) return null;
  return { name, dataset };
}

async function removeTestDataset(request: APIRequestContext, name: string, dataset: string) {
  await request.delete(`/api/datasets/registry/${encodeURIComponent(name)}`);
  await request.delete(`/api/datasets?name=${encodeURIComponent(dataset)}&recursive=true`);
}

test.describe('apps', () => {
  test('shows the catalog, installed and sources tabs', async ({ page }) => {
    await page.goto('/apps');
    await expect(page.getByRole('heading', { name: 'Apps', exact: true })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Catalog', exact: true })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Installed', exact: true })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Sources', exact: true })).toBeVisible();
  });

  test('the sources tab lists chart repositories', async ({ page }) => {
    await page.goto('/apps');
    await page.getByRole('button', { name: 'Sources', exact: true }).click();
    await expect(page.getByText('Loading sources...')).toBeHidden({ timeout: 10_000 });
    await expect(page.getByRole('button', { name: '+ Add Source' })).toBeVisible();
  });

  test('the catalog filters by channel when entries exist', async ({ page, request }) => {
    const res = await request.get('/api/catalog');
    const catalog = res.ok() ? await res.json() : [];
    test.skip(!Array.isArray(catalog) || catalog.length === 0, 'no catalog entries; add and refresh a source first');

    await page.goto('/apps');
    await expect(page.getByText('Loading catalog...')).toBeHidden({ timeout: 10_000 });
    await expect(page.getByRole('combobox', { name: 'Channel' })).toBeVisible();
  });

  test('GET /api/apps/jobs is a job list, not an app named jobs', async ({ request }) => {
    const res = await request.get('/api/apps/jobs');
    expect(res.ok(), await res.text()).toBeTruthy();
    const data = await res.json();
    expect(Array.isArray(data.jobs)).toBeTruthy();
  });

  test('installs and uninstalls an app from the official source', async ({ page, request }) => {
    // A cold catalog install pulls the app image server-side, so the async job
    // can exceed the default 30s test timeout.
    test.setTimeout(300_000);

    const res = await request.get('/api/catalog');
    const catalog = res.ok() ? await res.json() : [];
    test.skip(!Array.isArray(catalog) || catalog.length === 0, 'no catalog entries; add and refresh a source first');

    const app = catalog[0];
    const dataset = await ensureTestDataset(request);
    test.skip(!dataset, 'no ZFS pool dataset is available for the app claim');

    try {
      // A previous interrupted run may still own the release; remove it before
      // installing so the test is idempotent.
      const existing = await request.delete(`/api/apps/${app.name}`);
      if (existing.status() === 202) {
        const oldJob = await existing.json();
        await waitForJob(request, oldJob.jobId, 120_000);
      }

      // Install cluster-internal (no subdomain) so the test needs no DNS or cert.
      const created = await request.post('/api/apps', {
        data: {
          name: app.name,
          values: app.defaultValues || {},
          exposure: { subdomain: '', tls: false, auth: false, localOnly: false },
          confirmed: true,
          datasets: [dataset!.name]
        }
      });
      expect(created.status(), await created.text()).toBe(202);
      const installJob = await created.json();
      expect(await waitForJob(request, installJob.jobId, 280_000)).toBe('succeeded');

      await page.goto('/apps');
      await page.getByRole('button', { name: 'Installed', exact: true }).click();
      await expect(page.getByText('Loading installed apps...')).toBeHidden({ timeout: 10_000 });
      await expect(page.getByRole('heading', { name: app.name, exact: true })).toBeVisible();

      // The exposure modal offers a base-domain picker (FR-APP-10); the API
      // always returns the primary in selectableDomains, so the select renders.
      await page.getByRole('button', { name: 'Exposure', exact: true }).first().click();
      await expect(page.locator('#exposure-domain')).toBeVisible();
      await page.getByRole('button', { name: 'Cancel', exact: true }).click();
    } finally {
      const removed = await request.delete(`/api/apps/${app.name}`);
      // 404 when a previous run already removed it.
      expect([202, 404], await removed.text()).toContain(removed.status());
      if (removed.status() === 202) {
        const removeJob = await removed.json();
        expect(await waitForJob(request, removeJob.jobId, 120_000)).not.toBe('running');
      }
      await removeTestDataset(request, dataset!.name, dataset!.dataset);
    }
  });

  test('rejects an unconfirmed install', async ({ request }) => {
    const res = await request.post('/api/apps', {
      data: { name: 'no-such-app', values: {}, confirmed: false }
    });
    expect(res.status()).toBe(400);
  });

  test('the exposure endpoint is guarded', async ({ request }) => {
    const res = await request.get('/api/apps/does-not-exist/exposure');
    expect(res.status()).toBe(404);
  });
});
