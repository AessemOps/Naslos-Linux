import { test, expect, type Page, type Route } from '@playwright/test';

// Fully mocked cloud-backup page. Every /api/ path is answered here (with a
// state machine for destinations, jobs and chains), so this spec runs against
// `vite preview` with no live node and no Authelia session. The `mocked`
// project in playwright.config.ts selects it; the live chromium project
// ignores it.
//
// Run: npm run test:mocked   (build + vite preview :4173 + --project=mocked)

test.use({ storageState: { cookies: [], origins: [] } });

// A bare `playwright test` also selects this project, but the live suite has no
// preview server on :4173 and may point PLAYWRIGHT_BASE_URL at the node. Skip
// there; the spec only has meaning against the local preview that
// `npm run test:mocked` starts.
test.beforeAll(async ({ baseURL, request }) => {
  if (!baseURL || !/^http:\/\/(localhost|127\.0\.0\.1):\d+$/.test(baseURL)) {
    test.skip(true, 'cloud-backup mocks require the local vite preview');
  }
  try {
    const res = await request.get('/');
    if (!res.ok()) test.skip(true, 'the local vite preview is not serving the app');
  } catch {
    test.skip(true, 'the local vite preview is not running');
  }
});

interface DestinationView {
  id: string;
  name: string;
  endpoint: string;
  region: string;
  bucket: string;
  prefix: string;
  pathStyle: boolean;
  allowInsecureHttp: boolean;
  enabled: boolean;
  credentialsManaged: boolean;
  hasAccessKey: boolean;
  hasSecretKey: boolean;
  hasSessionToken: boolean;
  conditionalCreate: boolean;
  createdAt?: string;
  lastTestAt?: string;
  lastError?: string;
}

function destination(overrides: Partial<DestinationView> = {}): DestinationView {
  return {
    id: 'd1',
    name: 'b2-main',
    endpoint: 'https://s3.us-west-004.backblazeb2.com',
    region: 'us-west-004',
    bucket: 'naslos-test',
    prefix: '',
    pathStyle: false,
    allowInsecureHttp: false,
    enabled: true,
    credentialsManaged: false,
    hasAccessKey: true,
    hasSecretKey: true,
    hasSessionToken: false,
    conditionalCreate: true,
    createdAt: '2026-01-01T00:00:00Z',
    lastTestAt: '2026-01-01T00:00:00Z',
    lastError: '',
    ...overrides
  };
}

function chain(overrides: Record<string, unknown> = {}) {
  return {
    chain: 'chain-1',
    lineage: 'lineage-1',
    kind: 'full',
    fromSnapshot: '',
    toSnapshot: 'snap-1',
    fromGuid: '',
    toGuid: 'guid-to',
    sequence: 1,
    createdAt: '2026-01-01T00:00:00Z',
    chunks: 4,
    plainBytes: 4096,
    raw: false,
    ...overrides
  };
}

function createCloudMock() {
  const state = {
    destinations: [] as DestinationView[],
    schedules: [] as any[],
    chains: [] as any[],
    jobs: new Map<string, any>(),
    jobPolls: new Map<string, number>(),
    destinationPosts: [] as any[],
    destinationPuts: [] as Array<{ id: string; body: any }>,
    destinationDeletes: [] as string[],
    schedulePosts: [] as any[],
    sendPosts: [] as any[],
    verifyPosts: [] as any[],
    restorePosts: [] as any[],
    jobDeletes: [] as string[],
    failDestinations: 0,
    sendConflict: false,
    longSend: false,
    testCategory: '',
    nextId: 1,
    nextJob: 0
  };

  function newJob(kind: string, extra: Record<string, unknown> = {}) {
    const id = `${kind}-${++state.nextJob}`;
    const job = {
      id,
      kind,
      destination: 'd1',
      source: 'tank/media',
      dataset: 'tank/media',
      state: 'running',
      startedAt: new Date().toISOString(),
      progress: { chunk: 0, totalChunks: 4, plainBytes: 0, uploaded: 0, skipped: 0 },
      ...extra
    };
    state.jobs.set(id, job);
    state.jobPolls.set(id, 0);
    return job;
  }

  async function handle(route: Route) {
    const request = route.request();
    const url = new URL(request.url());
    const path = url.pathname;
    const method = request.method();
    const json = (body: unknown, status = 200) => route.fulfill({ status, json: body });
    let body: any = null;
    if (method !== 'GET' && method !== 'HEAD') {
      try {
        body = request.postDataJSON();
      } catch {
        body = null;
      }
    }

    if (path === '/api/auth/me') return json({ username: 'admin', groups: 'naslos_admins' });
    if (path === '/api/system/updates') {
      return json({ available: null, stepping: { allowed: false }, current: '0.0.0', checkedAt: '' });
    }
    if (path === '/api/apps/updates') return json({ count: 0, apps: [] });
    if (path === '/api/datasets') {
      return json([{ name: 'tank' }, { name: 'tank/media' }, { name: 'tank/other' }]);
    }

    if (path === '/api/cloud/destinations' && method === 'GET') {
      if (state.failDestinations > 0) {
        state.failDestinations -= 1;
        return json({ error: 'the cloud store is unavailable' }, 500);
      }
      return json({ destinations: state.destinations });
    }
    if (path === '/api/cloud/destinations' && method === 'POST') {
      state.destinationPosts.push(body);
      const created = destination({
        ...body,
        id: `d${++state.nextId}`,
        hasAccessKey: Boolean(body.accessKeyId),
        hasSecretKey: Boolean(body.secretAccessKey),
        hasSessionToken: Boolean(body.sessionToken),
        conditionalCreate: false,
        createdAt: new Date().toISOString(),
        lastTestAt: '',
        lastError: ''
      });
      state.destinations = [...state.destinations, created];
      return json({ destination: created }, 201);
    }

    const destinationMatch = /^\/api\/cloud\/destinations\/([^/]+)(\/test)?$/.exec(path);
    if (destinationMatch) {
      const id = decodeURIComponent(destinationMatch[1]);
      const current = state.destinations.find((d) => d.id === id);
      if (!current) return json({ error: 'destination not found' }, 404);
      if (destinationMatch[2] && method === 'POST') {
        if (state.testCategory) {
          return json({ ok: false, category: state.testCategory, conditionalCreate: false });
        }
        return json({ ok: true, conditionalCreate: true });
      }
      if (method === 'PUT') {
        state.destinationPuts.push({ id, body });
        Object.assign(current, body, {
          hasAccessKey: current.hasAccessKey || Boolean(body.accessKeyId),
          hasSecretKey: current.hasSecretKey || Boolean(body.secretAccessKey),
          hasSessionToken: current.hasSessionToken || Boolean(body.sessionToken)
        });
        delete (current as any).accessKeyId;
        delete (current as any).secretAccessKey;
        delete (current as any).sessionToken;
        return json({ destination: current });
      }
      if (method === 'DELETE') {
        state.destinationDeletes.push(id);
        state.destinations = state.destinations.filter((d) => d.id !== id);
        return json({ ok: true });
      }
      if (method === 'GET') return json({ destination: current });
    }

    if (path === '/api/cloud/schedules' && method === 'GET') {
      return json({ schedules: state.schedules });
    }
    if (path === '/api/cloud/schedules' && method === 'POST') {
      state.schedulePosts.push(body);
      const saved = {
        id: body.id ?? `s${++state.nextId}`,
        dataset: body.dataset,
        destinations: body.destinations,
        cadence: body.cadence,
        runAt: body.runAt,
        keepLineages: body.keepLineages,
        enabled: body.enabled,
        nextRun: '2026-01-02T02:30:00Z'
      };
      state.schedules = body.id
        ? state.schedules.map((s) => (s.id === body.id ? saved : s))
        : [...state.schedules, saved];
      return json({ schedule: saved });
    }
    if (path === '/api/cloud/schedules' && method === 'DELETE') {
      const id = url.searchParams.get('id') ?? '';
      state.schedules = state.schedules.filter((s) => s.id !== id);
      return json({ ok: true });
    }

    if (path === '/api/cloud/chains' && method === 'GET') {
      return json({ chains: state.chains });
    }

    if (path === '/api/cloud/send' && method === 'POST') {
      state.sendPosts.push(body);
      if (state.sendConflict) {
        return json({ error: 'a cloud job for this source is already running' }, 409);
      }
      const job = newJob('send', {
        dataset: body.dataset,
        destination: body.destination,
        source: body.dataset,
        long: state.longSend
      });
      return json({ jobId: job.id, status: 'started' }, 202);
    }

    if (path === '/api/cloud/verify' && method === 'POST') {
      state.verifyPosts.push(body);
      const job = newJob('verify', { destination: body.destination, source: body.source });
      return json({ jobId: job.id }, 202);
    }

    if (path === '/api/cloud/restore' && method === 'POST') {
      state.restorePosts.push(body);
      const job = newJob('restore', {
        destination: body.destination,
        source: body.source,
        dataset: body.dataset
      });
      return json({ jobId: job.id }, 202);
    }

    if (path === '/api/cloud/jobs' && method === 'GET') {
      return json({ jobs: [...state.jobs.values()] });
    }

    const jobMatch = /^\/api\/cloud\/jobs\/([^/]+)$/.exec(path);
    if (jobMatch) {
      const id = decodeURIComponent(jobMatch[1]);
      const job = state.jobs.get(id);
      if (!job) return json({ error: 'job not found' }, 404);
      if (method === 'DELETE') {
        state.jobDeletes.push(id);
        if (job.state !== 'running') return json({ error: 'job already finished' }, 409);
        job.state = 'cancelled';
        job.finishedAt = new Date().toISOString();
        return json({ status: 'cancelling', id });
      }
      const polls = (state.jobPolls.get(id) ?? 0) + 1;
      state.jobPolls.set(id, polls);
      if (job.state === 'running') {
        if (job.kind === 'send') {
          if (!job.long && polls >= 3) {
            job.state = 'succeeded';
            job.finishedAt = new Date().toISOString();
            job.progress = { chunk: 4, totalChunks: 4, plainBytes: 8192, uploaded: 4, skipped: 0 };
            job.result = {
              chain: 'chain-1',
              snapshot: 'snap-1',
              kind: 'full',
              plainBytes: 8192
            };
          } else {
            const chunk = Math.min(polls, 3);
            job.progress = {
              chunk,
              totalChunks: 4,
              plainBytes: chunk * 2048,
              uploaded: chunk,
              skipped: 0
            };
          }
        } else if (job.kind === 'verify' && polls >= 2) {
          job.state = 'failed';
          job.finishedAt = new Date().toISOString();
          job.error = '1 chunk is missing';
          job.result = {
            chain: 'chain-1',
            snapshot: 'snap-1',
            kind: 'full',
            plainBytes: 4096,
            missingIndices: { 'chain-1': [3] },
            badIndices: {}
          };
        } else if (job.kind === 'restore' && polls >= 2) {
          job.state = 'succeeded';
          job.finishedAt = new Date().toISOString();
          job.result = { chain: 'chain-1', snapshot: 'snap-1', kind: 'full', plainBytes: 4096 };
        }
      }
      return json(job);
    }

    return json({});
  }

  return {
    state,
    install: (page: Page) => page.route(/\/api\//, handle)
  };
}

async function openPage(page: Page) {
  await page.goto('/backups/cloud');
  await expect(page.getByRole('heading', { name: 'Cloud backup', exact: true })).toBeVisible();
  await expect(page.getByText('Loading cloud backup...')).toBeHidden();
}

async function acknowledge(page: Page) {
  const panel = page.getByTestId('cloud-dr-panel');
  if (await panel.isVisible()) {
    await panel.getByRole('checkbox').check();
    await panel.getByRole('button', { name: 'Confirm recovery keys' }).click();
    await expect(panel).toBeHidden();
  }
}

async function selectSendPair(page: Page, dataset = 'tank/media', dest = 'd1') {
  await page.locator('#cloud-send-dataset').selectOption(dataset);
  await page.locator('#cloud-send-destination').selectOption(dest);
}

async function listChains(page: Page, dest = 'd1', source = 'tank/media') {
  await page.locator('#cloud-chains-destination').selectOption(dest);
  await page.locator('#cloud-chains-source').fill(source);
  await page.getByRole('button', { name: 'List backups' }).click();
}

test.describe('cloud backup (mocked)', () => {
  test('empty state and the recovery acknowledgement persist in localStorage', async ({ page }) => {
    const mock = createCloudMock();
    await mock.install(page);
    await openPage(page);

    await expect(page.getByText('No cloud destinations yet')).toBeVisible();
    await expect(page.getByRole('button', { name: 'Add destination' })).toBeVisible();
    await expect(page.getByTestId('cloud-dr-panel')).toBeVisible();

    await acknowledge(page);
    await expect(page.getByTestId('cloud-dr-acknowledged')).toBeVisible();

    await page.reload();
    await expect(page.getByText('No cloud destinations yet')).toBeVisible();
    await expect(page.getByTestId('cloud-dr-acknowledged')).toBeVisible();
    await expect(page.getByTestId('cloud-dr-panel')).toBeHidden();
  });

  test('the acknowledgement gates send and schedule until confirmed', async ({ page }) => {
    const mock = createCloudMock();
    mock.state.destinations = [destination()];
    await mock.install(page);
    await openPage(page);

    await selectSendPair(page);
    const send = page.getByRole('button', { name: 'Send now' });
    await expect(send).toBeDisabled();

    await page.locator('#cloud-schedule-dataset').selectOption('tank/media');
    await page.getByRole('checkbox', { name: 'b2-main' }).check();
    const addSchedule = page.getByRole('button', { name: 'Add schedule' });
    await expect(addSchedule).toBeDisabled();

    await acknowledge(page);
    await expect(send).toBeEnabled();
    await expect(addSchedule).toBeEnabled();
  });

  test('destination create validates bucket and http acknowledgement and sends only entered secrets', async ({
    page
  }) => {
    const mock = createCloudMock();
    await mock.install(page);
    await openPage(page);

    await page.getByRole('button', { name: 'Add destination' }).click();
    const dialog = page.getByRole('dialog', { name: 'Add cloud destination' });
    await expect(dialog).toBeVisible();

    await dialog.getByLabel('Name').fill('minio');
    await dialog.getByLabel('Endpoint').fill('https://s3.example.test');
    await dialog.getByLabel('Access key ID').fill('AKIAEXAMPLE');
    await dialog.getByLabel('Secret access key').fill('secret-value');

    await dialog.getByRole('button', { name: 'Add destination' }).click();
    await expect(dialog.getByText('Bucket is required.')).toBeVisible();
    expect(mock.state.destinationPosts).toHaveLength(0);

    await dialog.getByLabel('Endpoint').fill('http://minio.example.test:9000');
    await dialog.getByLabel('Bucket').fill('backups');
    const save = dialog.getByRole('button', { name: 'Add destination' });
    await expect(save).toBeDisabled();
    await dialog.getByLabel(/I accept plaintext/).check();
    await expect(save).toBeEnabled();
    await save.click();

    await expect(page.getByTestId('cloud-destination').filter({ hasText: 'minio' })).toBeVisible();
    expect(mock.state.destinationPosts).toHaveLength(1);
    const body = mock.state.destinationPosts[0];
    expect(body).toMatchObject({
      name: 'minio',
      endpoint: 'http://minio.example.test:9000',
      bucket: 'backups',
      allowInsecureHttp: true
    });
    expect(body.accessKeyId).toBe('AKIAEXAMPLE');
    expect(body.secretAccessKey).toBe('secret-value');
    expect(body).not.toHaveProperty('sessionToken');
  });

  test('edit shows the configured placeholder and a blank credential keeps the stored value', async ({
    page
  }) => {
    const mock = createCloudMock();
    mock.state.destinations = [destination()];
    await mock.install(page);
    await openPage(page);

    await page.getByRole('button', { name: 'Edit destination b2-main' }).click();
    const dialog = page.getByRole('dialog', { name: 'Edit destination b2-main' });
    await expect(dialog).toBeVisible();

    const access = dialog.getByLabel('Access key ID');
    const secret = dialog.getByLabel('Secret access key');
    await expect(access).toHaveValue('');
    await expect(access).toHaveAttribute('placeholder', 'Configured — leave blank to keep');
    await expect(secret).toHaveValue('');
    await expect(secret).toHaveAttribute('placeholder', 'Configured — leave blank to keep');

    await dialog.getByLabel('Name').fill('b2-renamed');
    await dialog.getByRole('button', { name: 'Save changes' }).click();

    await expect(page.getByTestId('cloud-destination').filter({ hasText: 'b2-renamed' })).toBeVisible();
    expect(mock.state.destinationPuts).toHaveLength(1);
    const body = mock.state.destinationPuts[0].body;
    expect(body.name).toBe('b2-renamed');
    expect(body).not.toHaveProperty('accessKeyId');
    expect(body).not.toHaveProperty('secretAccessKey');
    expect(body).not.toHaveProperty('sessionToken');
  });

  test('test connection reports success and a category-only failure', async ({ page }) => {
    const mock = createCloudMock();
    mock.state.destinations = [
      destination(),
      destination({ id: 'd2', name: 'r2-backup', endpoint: 'https://r2.example.test' })
    ];
    await mock.install(page);
    await openPage(page);

    await page.getByRole('button', { name: 'Test destination b2-main' }).click();
    await expect(page.getByRole('status')).toContainText(
      'Connection OK. Conditional create is supported.'
    );

    mock.state.testCategory = 'auth';
    await page.getByRole('button', { name: 'Test destination r2-backup' }).click();
    await expect(page.getByRole('alert')).toContainText('The credentials were rejected.');
  });

  test('send starts a job, polls progress and reaches succeeded', async ({ page }) => {
    const mock = createCloudMock();
    mock.state.destinations = [destination()];
    await mock.install(page);
    await openPage(page);
    await acknowledge(page);

    await selectSendPair(page);
    await page.getByRole('button', { name: 'Send now' }).click();
    await expect.poll(() => mock.state.sendPosts.length).toBe(1);
    expect(mock.state.sendPosts).toEqual([
      { dataset: 'tank/media', destination: 'd1', forceFull: false }
    ]);

    const progress = page.getByRole('progressbar');
    await expect(progress).toBeVisible();
    await expect(progress).toHaveAttribute('aria-valuetext', /chunk [12] of 4/);
    await expect
      .poll(() => progress.getAttribute('aria-valuetext'), { timeout: 15_000 })
      .toContain('chunk 2 of 4');
    await expect(page.getByTestId('cloud-job')).toContainText('succeeded', { timeout: 15_000 });
    await expect(page.getByTestId('cloud-job')).toContainText('chain-1');
  });

  test('cancel requests DELETE on the job and shows the cancelled state', async ({ page }) => {
    const mock = createCloudMock();
    mock.state.destinations = [destination()];
    mock.state.longSend = true;
    await mock.install(page);
    await openPage(page);
    await acknowledge(page);

    await selectSendPair(page);
    await page.getByRole('button', { name: 'Send now' }).click();
    await expect(page.getByRole('progressbar')).toBeVisible();

    await page.getByRole('button', { name: 'Cancel job' }).click();
    await expect.poll(() => mock.state.jobDeletes).toEqual(['send-1']);
    await expect(page.getByTestId('cloud-job')).toContainText('cancelled', { timeout: 15_000 });
  });

  test('a 409 on send shows the already-running message', async ({ page }) => {
    const mock = createCloudMock();
    mock.state.destinations = [destination()];
    mock.state.sendConflict = true;
    await mock.install(page);
    await openPage(page);
    await acknowledge(page);

    await selectSendPair(page);
    await page.getByRole('button', { name: 'Send now' }).click();
    await expect(page.getByText(/already running/)).toBeVisible();
    expect(mock.state.sendPosts).toHaveLength(1);
  });

  test('restore requires the typed dataset name and posts it', async ({ page }) => {
    const mock = createCloudMock();
    mock.state.destinations = [destination()];
    mock.state.chains = [chain()];
    await mock.install(page);
    await openPage(page);

    await listChains(page);
    await expect(
      page.getByRole('region', { name: 'Stored cloud backups' }).getByRole('table')
    ).toContainText('snap-1');

    await page.getByRole('button', { name: 'Restore snap-1' }).click();
    const restore = page.getByRole('dialog', { name: 'Restore snap-1' });
    await expect(restore).toBeVisible();
    await restore.getByRole('button', { name: 'Restore', exact: true }).click();

    const confirm = page.getByRole('dialog', { name: 'Restore backup' });
    await expect(confirm).toBeVisible();
    const confirmButton = confirm.getByRole('button', { name: 'Restore', exact: true });
    await expect(confirmButton).toBeDisabled();

    const typed = confirm.getByLabel('Type tank/media-restored to confirm');
    await typed.fill('tank/wrong-name');
    await expect(confirmButton).toBeDisabled();
    await typed.fill('tank/media-restored');
    await expect(confirmButton).toBeEnabled();
    await confirmButton.click();

    await expect.poll(() => mock.state.restorePosts.length).toBe(1);
    expect(mock.state.restorePosts).toEqual([
      {
        destination: 'd1',
        source: 'tank/media',
        chain: 'chain-1',
        dataset: 'tank/media-restored',
        keepPartial: false
      }
    ]);
  });

  test('verify warns about egress for content mode and reports a missing index', async ({ page }) => {
    const mock = createCloudMock();
    mock.state.destinations = [destination()];
    mock.state.chains = [chain()];
    await mock.install(page);
    await openPage(page);

    await listChains(page);
    await page.getByRole('radio', { name: /Content/ }).check();
    await expect(page.getByTestId('cloud-egress-warning')).toContainText('egress');

    await page.getByRole('button', { name: 'Verify backups' }).click();
    await expect.poll(() => mock.state.verifyPosts.length).toBe(1);
    expect(mock.state.verifyPosts).toEqual([
      { destination: 'd1', source: 'tank/media', mode: 'content' }
    ]);
    await expect(page.getByTestId('cloud-job')).toContainText('missing chunk index 3', {
      timeout: 15_000
    });
  });

  test('a failed load shows the error state and retry recovers', async ({ page }) => {
    const mock = createCloudMock();
    mock.state.failDestinations = 1;
    await mock.install(page);
    await page.goto('/backups/cloud');

    await expect(page.getByRole('alert')).toContainText('Failed to load cloud backup');
    await page.getByRole('button', { name: 'Retry' }).click();
    await expect(page.getByText('No cloud destinations yet')).toBeVisible();
  });
});
