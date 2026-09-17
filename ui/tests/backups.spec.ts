import { test, expect } from '@playwright/test';

// The backups page is API-backed throughout: identity, schedules, jobs and the
// receiver report all come from /api/buddy/*. Tests assert rendered values
// against the API rather than placeholders.
//
// The sender half is gated the same way the terminal is (SEC-7): with
// buddy.requireAuth=true every owner-facing /api/buddy/* call must carry the
// identity header the authenticating proxy injects. The suite reaches the VM
// through the NodePort, where no proxy runs, so the header is set here - it is
// exactly what Traefik's forwardAuth adds in production. (On this listener the
// header is also client-controlled, which is NAS-008; that is a deployment
// posture question, not a page defect.)
test.use({ extraHTTPHeaders: { 'Remote-User': 'admin' } });

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
  // A send streams one dataset and the agent refuses the pool root
  // ("dataset must be <pool>/<name>"), so prefer a child dataset when there is
  // one - which is also all the page's dataset pickers offer.
  const child = body.find((d: any) => typeof d?.name === 'string' && d.name.includes('/'));
  return (child ?? body[0]).name;
}

test('backups page shows the instance identity', async ({ page, request }) => {
  const identity = await buddyIdentity(request);

  await page.goto('/backups');
  await expect(page.getByRole('heading', { name: 'Backups', exact: true })).toBeVisible();
  await expect(page.getByText('Loading backups...')).toBeHidden({ timeout: 10_000 });

  if (identity.exists) {
    // The fingerprint rendered on the page must be the API's own. It also shows
    // up in the peer table, so scope the assertion to the identity card's code.
    await expect(page.locator('code').filter({ hasText: identity.fingerprint }).first()).toBeVisible();
    await expect(page.locator('code').filter({ hasText: identity.publicKey.slice(0, 20) }).first()).toBeVisible();
  } else {
    await expect(page.getByRole('button', { name: 'Create identity' })).toBeVisible();
  }
});

test('schedules can be created and deleted', async ({ page, request }) => {
  const dataset = await firstDataset(request);
  test.skip(dataset === null, 'no ZFS dataset available for a schedule');

  await page.goto('/backups');
  await expect(page.getByRole('heading', { name: 'Backups', exact: true })).toBeVisible();
  const receiver = new URL(page.url()).origin;
  const identity = await buddyIdentity(request);
  const source = `${identity.name ?? 'uitest'}/uitest-sched${Date.now().toString().slice(-6)}`;

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
  // The receiver only accepts sources inside the authorized peer's scope, so use
  // this instance's own name as the prefix: that is what a self-enrolled peer
  // allows, and what a user of the page would pick too.
  const source = `${identity.name ?? 'uitest'}/uitest-manual${Date.now().toString().slice(-6)}`;

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
  test.skip(job.state === 'failed' && /unknown key|not authorized|not configured|out of scope/i.test(job.error ?? ''),
    `receiver not set up for self-send: ${job.error}`);
  expect(job.state, job.error).toBe('succeeded');
  expect(job.result.chunks, 'send stored no chunks').toBeGreaterThan(0);
});

test('Back up now drives a job from the page and Verify reads it back', async ({ page, request }) => {
  const identity = await buddyIdentity(request);
  test.skip(!identity.exists, 'no backup identity on this instance');
  const dataset = await firstDataset(request);
  test.skip(dataset === null, 'no ZFS dataset available for a send');

  await page.goto('/backups');
  const receiver = new URL(page.url()).origin;
  const source = `${identity.name ?? 'uitest'}/uitest-now${Date.now().toString().slice(-6)}`;

  const created = await request.post('/api/buddy/schedules', {
    data: { dataset, source, receiver, cadence: 'daily', runAt: '02:30' }
  });
  expect(created.status(), await created.text()).toBe(200);
  const id = (await created.json()).schedule.id;

  try {
    await page.reload();
    const row = page.locator('tr').filter({ hasText: source });
    await expect(row).toBeVisible({ timeout: 10_000 });

    // The button starts the same async job the API drill uses; the page polls it
    // and renders the finished chain, so this exercises the progress path.
    await row.getByRole('button', { name: 'Back up now' }).click();
    await expect(page.getByText(/Chain [0-9a-f]{16}: \d+ chunks/)).toBeVisible({ timeout: 60_000 });

    // Verify decrypts and hashes the stored chain without touching ZFS.
    await row.getByRole('button', { name: 'Verify' }).click();
    await expect(page.getByText(`Verify ${source} on ${receiver}:`)).toBeVisible({ timeout: 30_000 });
    await expect(page.getByText(/[0-9a-f]{64}/)).toBeVisible({ timeout: 30_000 });
  } finally {
    await request.delete(`/api/buddy/schedules?id=${encodeURIComponent(id)}`);
  }
});

test('restore from the page requires typing the destination dataset', async ({ page }) => {
  await page.goto('/backups');
  const card = page.locator('.card').filter({ has: page.getByRole('heading', { name: 'Restore', exact: true }) });
  await expect(card).toBeVisible();

  await card.getByPlaceholder('Source name').fill('naslos-vm/does-not-matter');
  await card.getByPlaceholder('Buddy URL').fill('http://192.168.1.96:30080');
  await card.getByPlaceholder('Destination dataset').fill('test/should-not-happen');

  // No confirmation typed: the page must refuse before any request is made.
  await card.getByRole('button', { name: 'Restore' }).click();
  await expect(page.getByText(/Type the destination dataset name to confirm/)).toBeVisible();

  // A confirmation that does not match the destination is refused too.
  await card.getByPlaceholder('Type destination to confirm').fill('test/wrong-name');
  await card.getByRole('button', { name: 'Restore' }).click();
  await expect(page.getByText(/Type the destination dataset name to confirm/)).toBeVisible();
});
