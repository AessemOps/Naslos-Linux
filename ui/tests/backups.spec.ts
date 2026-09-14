import { test, expect } from '@playwright/test';

// The backups page is API-backed throughout: identity, schedules, jobs and the
// receiver report all come from /api/buddy/*. Tests assert rendered values
// against the API rather than placeholders.

async function buddyIdentity(request: any): Promise<any> {
  const res = await request.get('/api/buddy/identity');
  expect(res.ok(), await res.text()).toBeTruthy();
  return await res.json();
}

async function firstDataset(request: any): Promise<string | null> {
  const res = await request.get('/api/datasets');
  if (!res.ok()) return null;
  const body = await res.json();
  if (!Array.isArray(body) || body.length === 0) return null;
  return body[0].name;
}

test('backups page shows the instance identity', async ({ page, request }) => {
  const identity = await buddyIdentity(request);

  await page.goto('/backups');
  await expect(page.getByRole('heading', { name: 'Backups' })).toBeVisible();
  await expect(page.getByText('Loading backups...')).toBeHidden({ timeout: 10_000 });

  if (identity.exists) {
    // The fingerprint rendered on the page must be the API's own.
    await expect(page.getByText(identity.fingerprint)).toBeVisible();
    await expect(page.getByText(identity.publicKey.slice(0, 20))).toBeVisible();
  } else {
    await expect(page.getByRole('button', { name: 'Create identity' })).toBeVisible();
  }
});

test('schedules can be created and deleted', async ({ page, request }) => {
  const dataset = await firstDataset(request);
  test.skip(dataset === null, 'no ZFS dataset available for a schedule');

  await page.goto('/backups');
  await expect(page.getByRole('heading', { name: 'Backups' })).toBeVisible();
  const receiver = new URL(page.url()).origin;
  const source = `uitest/sched${Date.now().toString().slice(-6)}`;

  const created = await request.post('/api/buddy/schedules', {
    data: { dataset, source, receiver, cadence: 'daily', runAt: '02:30', pruneKeep: 3 }
  });
  expect(created.status(), await created.text()).toBe(200);
  const createdBody = await created.json();
  const id = createdBody.schedule.id;
  expect(id, 'schedule has no id').toBeTruthy();

  try {
    await page.reload();
    await expect(page.getByText(source)).toBeVisible({ timeout: 10_000 });

    // Deleting from the UI asks for confirmation; accept it.
    page.on('dialog', (d) => d.accept());
    const row = page.locator('tr').filter({ hasText: source });
    await row.getByRole('button', { name: 'Delete' }).click();
    await expect(page.getByText(source)).toBeHidden({ timeout: 10_000 });
  } finally {
    await request.delete(`/api/buddy/schedules?id=${encodeURIComponent(id)}`);
  }
});

test('receiver section reports free space or the setup pointer', async ({ page, request }) => {
  const res = await request.get('/api/buddy/status');

  await page.goto('/backups');
  await expect(page.getByRole('heading', { name: 'This buddy receives' })).toBeVisible({ timeout: 10_000 });

  if (res.status() === 503) {
    // Not configured: an info panel points at the docs, not an error.
    await expect(page.getByText(/docs\/buddy-backup\.md/)).toBeVisible();
  } else {
    expect(res.ok(), await res.text()).toBeTruthy();
    const status = await res.json();
    expect(status.freeBytes, 'receiver reports no free space').toBeGreaterThanOrEqual(0);
    await expect(page.getByText(/Free:/)).toBeVisible();
  }
});

test('a manual send reaches succeeded with chunks', async ({ page, request }) => {
  const identity = await buddyIdentity(request);
  test.skip(!identity.exists, 'no backup identity on this instance');
  const dataset = await firstDataset(request);
  test.skip(dataset === null, 'no ZFS dataset available for a send');

  await page.goto('/backups');
  const receiver = new URL(page.url()).origin;
  const source = `uitest/manual${Date.now().toString().slice(-6)}`;

  // Self-send: the instance backs itself up to its own receiver. When the
  // instance's key is not authorized there the job fails fast with an
  // unknown-key error, which is a setup state, not a page defect.
  const started = await request.post('/api/buddy/send', {
    data: { dataset, source, receiver }
  });
  expect(started.status(), await started.text()).toBe(202);
  const { jobId } = await started.json();

  let job: any = null;
  const deadline = Date.now() + 120_000;
  while (Date.now() < deadline) {
    const detail = await request.get(`/api/buddy/jobs/${jobId}`);
    expect(detail.ok(), await detail.text()).toBeTruthy();
    job = await detail.json();
    if (job.state !== 'running') break;
    await new Promise((r) => setTimeout(r, 1000));
  }
  test.skip(job.state === 'failed' && /unknown key|not authorized|not configured/i.test(job.error ?? ''),
    `receiver not set up for self-send: ${job.error}`);
  expect(job.state, job.error).toBe('succeeded');
  expect(job.result.chunks, 'send stored no chunks').toBeGreaterThan(0);
});
