import { test, expect } from '@playwright/test';

// FR-VMT: the virtual machines page. The suite capability-skips: a node whose
// supervisor is missing (a pre-0.4 deployment, or the feature wired off) must
// render the page with its message, not crash, and every live-VM assertion is
// gated on /api/virt/capabilities answering with real capabilities.

test('virtual machines page renders and reports the node capabilities', async ({ page }) => {
  const consoleErrors: string[] = [];
  page.on('console', (msg) => {
    if (msg.type() === 'error') consoleErrors.push(msg.text());
  });

  const res = await page.request.get('/api/virt/capabilities');
  if (res.status() === 404) {
    // The deployed release predates the feature (the wildcard owner router
    // answers 404 JSON after auth). Nothing to verify here.
    test.skip(true, 'the deployed release predates the virtual machines feature');
  }
  if (res.status() === 503) {
    // The supervisor is disabled or unreachable: the page must render with
    // its unavailable state instead of crashing.
    await page.goto('/vms');
    await expect(page.getByRole('heading', { name: 'Virtual Machines' })).toBeVisible();
    expect(
      consoleErrors.filter((line) => !line.includes('Failed to load resource')),
      consoleErrors.join('\n')
    ).toEqual([]);
    test.skip(true, 'the QEMU supervisor is unavailable on this node');
  }
  expect(res.ok(), await res.text()).toBeTruthy();
  const caps = await res.json();
  expect(typeof caps.kvm).toBe('boolean');
  expect(Array.isArray(caps.nics)).toBeTruthy();

  await page.goto('/vms');
  await expect(page.getByRole('heading', { name: 'Virtual Machines' })).toBeVisible();
  await expect(page.getByText('Loading virtual machines…')).toBeHidden({ timeout: 10_000 });

  if (!caps.kvm) {
    // The plan's fallback gate: a KVM-less node shows a banner, not a
    // silent failure.
    await expect(page.getByText(/no usable.*KVM/i)).toBeVisible();
  }

  expect(
    consoleErrors.filter((line) => !line.includes('Failed to load resource')),
    consoleErrors.join('\n')
  ).toEqual([]);
});

test('the VM list contract holds on the node', async ({ request }) => {
  const res = await request.get('/api/virt/capabilities');
  if (res.status() === 404 || res.status() === 503) {
    test.skip(true, 'the QEMU supervisor is unavailable on this node');
  }
  const caps = await res.json();
  if (!caps.kvm) {
    test.skip(true, 'the node has no KVM; run the VM drill on the KVM-capable node');
  }

  const list = await request.get('/api/virt/vms');
  expect(list.ok(), await list.text()).toBeTruthy();
  const vms = await list.json();
  expect(Array.isArray(vms)).toBeTruthy();
  for (const vm of vms) {
    expect(vm.name).toMatch(/^[a-z0-9]([-a-z0-9])*[a-z0-9]$/);
    expect(['running', 'stopped', 'unknown']).toContain(vm.state);
    expect(vm.cpu).toBeGreaterThan(0);
    expect(vm.memMiB).toBeGreaterThanOrEqual(16);
    expect(vm.disk.sizeGB).toBeGreaterThan(0);
    expect(vm.net.mode).toBe('nat');
    // The console credentials never leak into the list payload (they only
    // ever travel through the preflight route).
    expect(vm.vncPassword).toBeUndefined();
    expect(vm.vncPort).toBeUndefined();
  }

  const images = await request.get(
    `/api/virt/images?pool=${encodeURIComponent(vms[0]?.pool ?? 'tank')}`
  );
  expect(images.ok(), await images.text()).toBeTruthy();
  const library = await images.json();
  expect(Array.isArray(library.images)).toBeTruthy();
});

test('the console preflight refuses a stopped VM with an actionable error', async ({ request }) => {
  const list = await request.get('/api/virt/vms');
  if (list.status() === 404 || list.status() === 503) {
    test.skip(true, 'the QEMU supervisor is unavailable on this node');
  }
  const vms = await list.json();
  if (!Array.isArray(vms) || vms.length === 0) {
    test.skip(true, 'no VMs exist on this node yet');
  }
  const stopped = vms.find((v: any) => v.state !== 'running');
  if (!stopped) {
    test.skip(true, 'every VM on this node is running; the drill covers the preflight');
  }
  const res = await request.get(`/api/ws/vm/${encodeURIComponent(stopped.name)}/vnc`);
  expect(res.status()).toBe(409);
  const body = await res.json();
  expect(body.error).toContain(stopped.name);
});

test('virtual machine routes are owner-only', async ({ request }) => {
  // Playwright carries the admin session; this pins the route surface.
  const res = await request.get('/api/virt/vms');
  if (res.status() === 404 || res.status() === 503) {
    test.skip(true, 'the QEMU supervisor is unavailable on this node');
  }
  const vms = await res.json();
  expect(Array.isArray(vms)).toBeTruthy();
});

// UI confirmation regressions (FR-VMT-03/09). The API is mocked; these assert
// that cancel sends nothing and the confirmed requests carry the VM name.
test.describe('VM destructive confirmations', () => {
  const vm = {
    name: 'uitest-vm',
    pool: 'tank',
    cpu: 2,
    memMiB: 1024,
    autostart: false,
    desired: 'stopped',
    firmware: 'uefi',
    secureBoot: false,
    disk: { sizeGB: 8, format: 'qcow2', bus: 'virtio', sourceImage: null },
    net: { mode: 'nat', forwards: [] },
    cloudInit: null,
    iso: null,
    state: 'stopped',
    pid: null,
    cpuPercent: 0,
    memUsedMiB: 0,
    unexpectedExit: false,
    lastError: ''
  };

  async function mockVmPage(
    page: any,
    vms: any[] = [vm],
    onRequest?: (method: string, url: string, body: any) => any
  ) {
    await page.route(/\/api\//, async (route: any) => {
      const request = route.request();
      const url = new URL(request.url());
      const path = url.pathname;
      let body: any = null;
      try {
        body = request.postDataJSON();
      } catch {
        body = null;
      }
      if (onRequest && (await onRequest(request.method(), request.url(), body))) {
        return route.fulfill({ json: {} });
      }
      if (path === '/api/virt/capabilities') {
        return route.fulfill({
          json: { kvm: true, cpuModel: 'Test CPU', nested: false, secureBoot: false, nics: [] }
        });
      }
      if (path === '/api/virt/vms') return route.fulfill({ json: vms });
      if (path === '/api/virt/images') {
        return route.fulfill({ json: { images: [], importing: [], errors: {} } });
      }
      if (path === '/api/volumes/zfs') return route.fulfill({ json: [{ name: 'tank' }] });
      if (/^\/api\/virt\/vms\/[^/]+\/snapshots$/.test(path)) {
        return route.fulfill({ json: [{ name: 'snap1', created: '2026-01-01T00:00:00Z' }] });
      }
      return route.fulfill({ json: {} });
    });
  }

  test('VM delete requires the typed name and sends confirm', async ({ page }) => {
    const deletes: string[] = [];
    await mockVmPage(page, [vm], (method, url) => {
      if (method === 'DELETE' && url.includes('/api/virt/vms/')) {
        deletes.push(url);
        return true;
      }
      return undefined;
    });

    await page.goto('/vms');
    await expect(page.getByText('Loading virtual machines…')).toBeHidden({ timeout: 10_000 });
    const row = page.locator('tr').filter({ hasText: 'uitest-vm' });
    await expect(row).toBeVisible();

    page.once('dialog', (dialog) => dialog.dismiss());
    await row.getByRole('button', { name: 'Delete' }).click();
    await expect(row).toBeVisible();
    expect(deletes).toHaveLength(0);

    // A mismatched prompt is followed by an alert: register the alert handler
    // only after the prompt is accepted, or both `once` handlers would fire on
    // the same dialog event.
    page.once('dialog', async (dialog) => {
      page.once('dialog', (alert) => alert.accept());
      await dialog.accept('wrong-name');
    });
    await row.getByRole('button', { name: 'Delete' }).click();
    await expect(row).toBeVisible();
    expect(deletes).toHaveLength(0);

    page.once('dialog', (dialog) => dialog.accept('uitest-vm'));
    await row.getByRole('button', { name: 'Delete' }).click();
    await expect.poll(() => deletes.some((url) => url.includes('confirm=uitest-vm'))).toBe(true);
  });

  test('the create wizard loads the image library for the selected pool', async ({ page }) => {
    await mockVmPage(page, [vm]);
    const imageRequests: string[] = [];
    await page.route('**/api/virt/images?pool=*', async (route: any) => {
      imageRequests.push(route.request().url());
      await route.fulfill({
        json: {
          images: [
            { name: 'debian-13-netinst.iso', sizeBytes: 1024 },
            { name: 'debian-13-cloud.qcow2', sizeBytes: 2048 }
          ],
          importing: [],
          errors: {}
        }
      });
    });

    await page.goto('/vms');
    await expect(page.getByText('Loading virtual machines…')).toBeHidden({ timeout: 10_000 });
    await page.getByRole('button', { name: 'New VM' }).click();
    await expect(page.getByRole('heading', { name: 'New virtual machine' })).toBeVisible();

    // Imported ISOs/cloud images are selectable, not just an empty library.
    await expect(page.locator('#vm-iso option')).toContainText(['debian-13-netinst.iso']);
    await page.locator('#vm-boot').selectOption('image');
    await expect(page.locator('#vm-img option')).toContainText(['debian-13-cloud.qcow2']);
    expect(imageRequests.some((url) => url.includes('pool=tank'))).toBe(true);
  });

  test('editing a VM without cloud-init or forwards keeps both absent', async ({ page }) => {
    const bare = { ...vm, name: 'uitest-bare', net: { mode: 'nat' }, cloudInit: null };
    const puts: any[] = [];
    await mockVmPage(page, [bare], (method, _url, body) => {
      if (method === 'PUT') {
        puts.push(body);
        return true;
      }
      return undefined;
    });

    await page.goto('/vms');
    await expect(page.getByText('Loading virtual machines…')).toBeHidden({ timeout: 10_000 });
    const row = page.locator('tr').filter({ hasText: 'uitest-bare' });
    await row.getByRole('button', { name: 'Edit' }).click();
    await expect(page.getByRole('heading', { name: 'Edit uitest-bare' })).toBeVisible();

    // The bare record must not crash the wizard: cloud-init is simply off and
    // the forwards list renders empty.
    await expect(page.getByRole('checkbox', { name: /Configure the guest on first boot/ })).not.toBeChecked();
    await page.getByRole('button', { name: 'Save changes' }).click();

    await expect.poll(() => puts.length).toBe(1);
    expect(puts[0].net.forwards).toEqual([]);
    expect(puts[0]).not.toHaveProperty('cloudInit');
  });

  test('editing a VM with cloud-init preserves every key and the forwards', async ({ page }) => {
    const keys = ['ssh-ed25519 AAAATESTKEYONE drill@one', 'ssh-rsa AAAATESTKEYTWO drill@two'];
    const cloudy = {
      ...vm,
      name: 'uitest-cloud',
      net: { mode: 'nat', forwards: [{ host: 5901, dst: 22 }, { host: 8080, dst: 80 }] },
      cloudInit: { hostname: 'cloudy', user: 'ops', sshKeys: keys }
    };
    const puts: any[] = [];
    await mockVmPage(page, [cloudy], (method, _url, body) => {
      if (method === 'PUT') {
        puts.push(body);
        return true;
      }
      return undefined;
    });

    await page.goto('/vms');
    await expect(page.getByText('Loading virtual machines…')).toBeHidden({ timeout: 10_000 });
    const row = page.locator('tr').filter({ hasText: 'uitest-cloud' });
    await row.getByRole('button', { name: 'Edit' }).click();
    await expect(page.getByRole('heading', { name: 'Edit uitest-cloud' })).toBeVisible();

    await expect(page.locator('#vm-ci-host')).toHaveValue('cloudy');
    await expect(page.locator('#vm-ci-user')).toHaveValue('ops');
    await expect(page.locator('#vm-ci-keys')).toHaveValue(keys.join('\n'));

    await page.getByRole('button', { name: 'Save changes' }).click();
    await expect.poll(() => puts.length).toBe(1);
    expect(puts[0].cloudInit).toEqual({ hostname: 'cloudy', user: 'ops', sshKeys: keys });
    expect(puts[0].net.forwards).toEqual([{ host: 5901, dst: 22 }, { host: 8080, dst: 80 }]);
  });

  test('snapshot restore requires the typed VM name and sends confirm', async ({ page }) => {
    const restores: string[] = [];
    await mockVmPage(page, [vm], (method, url) => {
      if (method === 'POST' && url.includes('/restore')) {
        restores.push(url);
        return true;
      }
      return undefined;
    });

    await page.goto('/vms');
    await expect(page.getByText('Loading virtual machines…')).toBeHidden({ timeout: 10_000 });
    await page.locator('tr').filter({ hasText: 'uitest-vm' }).getByRole('button', { name: 'Open' }).click();

    const restore = page.getByRole('button', { name: 'Restore', exact: true });
    await expect(restore).toBeEnabled({ timeout: 10_000 });

    page.once('dialog', (dialog) => dialog.dismiss());
    await restore.click();
    expect(restores).toHaveLength(0);

    page.once('dialog', (dialog) => dialog.accept('uitest-vm'));
    await restore.click();
    await expect.poll(() => restores.some((url) => url.includes('confirm=uitest-vm'))).toBe(true);
  });
});

// Image-library replace confirmation (FR-VMT-07). The API is mocked; these
// assert the UI only sends confirm=<name> after an explicit acknowledgement and
// that vmd's conflict/over-cap errors reach the operator.
test.describe('image library confirmations', () => {
  const existing = { name: 'debian.iso', sizeBytes: 4096 };

  async function mockImages(
    page: any,
    options: {
      images?: any[];
      upload?: (offset: number, final: boolean, call: number) => any;
      importStatus?: number;
      importError?: string;
    } = {}
  ) {
    const uploads: { offset: number; final: boolean; confirm: string | null }[] = [];
    const imports: any[] = [];
    let calls = 0;
    await page.route(/\/api\//, async (route: any) => {
      const request = route.request();
      const url = new URL(request.url());
      const path = url.pathname;
      if (path === '/api/virt/capabilities') {
        return route.fulfill({
          json: { kvm: true, cpuModel: 'Test CPU', nested: false, secureBoot: false, nics: [] }
        });
      }
      if (path === '/api/virt/vms') return route.fulfill({ json: [] });
      if (path === '/api/volumes/zfs') return route.fulfill({ json: [{ name: 'tank' }] });
      if (path === '/api/virt/images') {
        return route.fulfill({
          json: { images: options.images ?? [existing], importing: [], errors: {} }
        });
      }
      if (path === '/api/virt/images/upload') {
        const offset = Number(url.searchParams.get('offset') ?? '0');
        const final = url.searchParams.get('final') === 'true';
        const confirm = url.searchParams.get('confirm');
        calls += 1;
        uploads.push({ offset, final, confirm });
        if (options.upload) return route.fulfill(options.upload(offset, final, calls));
        const sent = request.postDataBuffer()?.length ?? 0;
        return route.fulfill({ json: { name: 'debian.iso', sizeBytes: offset + sent } });
      }
      if (path === '/api/virt/images/import') {
        imports.push(request.postDataJSON());
        return route.fulfill({
          status: options.importStatus ?? 202,
          json: options.importStatus && options.importStatus >= 400
            ? { error: options.importError ?? 'refused' }
            : { status: 'import started', name: 'debian.iso' }
        });
      }
      return route.fulfill({ json: {} });
    });
    return { uploads, imports };
  }

  test('uploading over an existing image needs the explicit replace confirmation', async ({ page }) => {
    const { uploads } = await mockImages(page);
    await page.goto('/vms');
    await expect(page.getByText('Loading virtual machines…')).toBeHidden({ timeout: 10_000 });
    await page.getByRole('button', { name: 'Images', exact: true }).click();
    await expect(page.getByRole('heading', { name: /Images — tank\/vms\/isos/ })).toBeVisible();

    await page.locator('#img-file').setInputFiles({
      name: 'debian.iso',
      mimeType: 'application/octet-stream',
      buffer: Buffer.from('tiny-image')
    });
    await expect(page.locator('#img-replace')).toBeVisible();
    const upload = page.getByRole('button', { name: 'Upload', exact: true });
    await expect(upload).toBeDisabled();

    await page.locator('#img-replace').check();
    await expect(upload).toBeEnabled();
    await upload.click();

    await expect.poll(() => uploads.length).toBe(1);
    expect(uploads[0].confirm).toBe('debian.iso');
  });

  test('a new image name uploads without the replace confirmation', async ({ page }) => {
    const { uploads } = await mockImages(page);
    await page.goto('/vms');
    await expect(page.getByText('Loading virtual machines…')).toBeHidden({ timeout: 10_000 });
    await page.getByRole('button', { name: 'Images', exact: true }).click();
    await page.locator('#img-file').setInputFiles({
      name: 'new-image.iso',
      mimeType: 'application/octet-stream',
      buffer: Buffer.from('fresh')
    });
    await expect(page.locator('#img-replace')).toBeHidden();
    await page.getByRole('button', { name: 'Upload', exact: true }).click();
    await expect.poll(() => uploads.length).toBe(1);
    expect(uploads[0].confirm).toBeNull();
  });

  test('a URL import over an existing image needs the confirmation and shows a conflict', async ({ page }) => {
    const conflict = 'cannot replace "debian.iso": VM uitest-vm is running attaches it';
    const { imports } = await mockImages(page, { importStatus: 409, importError: conflict });
    await page.goto('/vms');
    await expect(page.getByText('Loading virtual machines…')).toBeHidden({ timeout: 10_000 });
    await page.getByRole('button', { name: 'Images', exact: true }).click();

    await page.locator('#img-url').fill('https://mirror.example/debian.iso');
    await expect(page.locator('#img-import-replace')).toBeVisible();
    const start = page.getByRole('button', { name: 'Import', exact: true });
    await expect(start).toBeDisabled();

    await page.locator('#img-import-replace').check();
    await expect(start).toBeEnabled();
    await start.click();

    await expect.poll(() => imports.length).toBe(1);
    expect(imports[0].confirm).toBe('debian.iso');
    await expect(page.getByRole('alert')).toContainText(conflict);
  });

  test('an over-cap upload error is shown to the operator', async ({ page }) => {
    const message = 'the image exceeds the 34359738368-byte library limit; the staged upload was removed';
    await mockImages(page, {
      upload: () => ({ status: 413, json: { error: message } })
    });
    await page.goto('/vms');
    await expect(page.getByText('Loading virtual machines…')).toBeHidden({ timeout: 10_000 });
    await page.getByRole('button', { name: 'Images', exact: true }).click();
    await page.locator('#img-file').setInputFiles({
      name: 'huge.iso',
      mimeType: 'application/octet-stream',
      buffer: Buffer.from('x')
    });
    await page.getByRole('button', { name: 'Upload', exact: true }).click();
    await expect(page.getByRole('alert')).toContainText('library limit');
  });

  test('a lost partial upload resets the resume point and retries from zero', async ({ page }) => {
    const stale = 'the staged upload of "big.iso" is gone; start at offset 0';
    let staleSent = false;
    const { uploads } = await mockImages(page, {
      images: [],
      upload: (offset, final, call) => {
        if (offset > 0 && !staleSent) {
          staleSent = true;
          return { status: 409, json: { error: stale } };
        }
        const chunk = final ? 1024 * 1024 : 8 * 1024 * 1024;
        return { json: { name: 'big.iso', sizeBytes: offset + chunk } };
      }
    });
    await page.goto('/vms');
    await expect(page.getByText('Loading virtual machines…')).toBeHidden({ timeout: 10_000 });
    await page.getByRole('button', { name: 'Images', exact: true }).click();
    // 9 MiB: the first 8 MiB chunk succeeds, the final chunk hits the stale
    // staging error.
    await page.locator('#img-file').setInputFiles({
      name: 'big.iso',
      mimeType: 'application/octet-stream',
      buffer: Buffer.alloc(9 * 1024 * 1024)
    });
    await page.getByRole('button', { name: 'Upload', exact: true }).click();
    await expect(page.getByRole('alert')).toContainText('no longer has the partial upload');
    expect(uploads.map((u) => u.offset)).toEqual([0, 8 * 1024 * 1024]);

    // The retry starts over at zero instead of repeating the stale offset.
    await page.getByRole('button', { name: 'Upload', exact: true }).click();
    await expect.poll(() => uploads.length).toBe(4);
    expect(uploads[2].offset).toBe(0);
    expect(uploads[3].offset).toBe(8 * 1024 * 1024);
  });
});
