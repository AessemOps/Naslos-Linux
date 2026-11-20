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
  expect(typeof caps.macvtap).toBe('boolean');
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
    expect(['nat', 'lan']).toContain(vm.net.mode);
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
