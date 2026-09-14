import { test, expect } from '@playwright/test';

// The terminal reaches a root shell in a privileged container, so it must not be
// usable from an unauthenticated entry point. Reaching the UI on a node port
// (this suite's default baseURL) is exactly that case: the terminal endpoints are
// refused there, and the chart routes them from Traefik straight to the API so
// that only the authenticated proxy can supply the identity header the API
// trusts.
const TERMINAL_PATHS = [
  '/api/pods?namespace=naslos',
  '/api/namespaces',
  '/api/ws/exec?namespace=naslos&pod=whatever&shell=sh'
];

test('the terminal is refused without an authenticated session', async ({ request }) => {
  for (const path of TERMINAL_PATHS) {
    const res = await request.get(path);
    expect([401, 403], `${path} answered ${res.status()}`).toContain(res.status());
    // The refusal must be understandable (the UI shows this text).
    expect(await res.text()).toContain('authenticated');
  }
});

// Whether the authenticated entry point is reachable from this run. Through the
// node port it is not, so the interactive test below is skipped rather than
// failing: that refusal is the assertion above.
async function terminalReachable(request: any): Promise<boolean> {
  const res = await request.get('/api/pods?namespace=naslos');
  return res.status() === 200;
}

// xterm renders each row into the DOM, so the visible screen can be asserted.
function screen(page: any) {
  return page.locator('.xterm-rows');
}

test('the terminal lists pods, preselects the shell container and runs commands', async ({ page, request }) => {
  test.skip(!(await terminalReachable(request)),
    'run this against the authenticated entry point (Authelia) to exercise the terminal');

  const pods = await (await request.get('/api/pods?namespace=naslos')).json();
  const shellPod = (pods as any[]).find(p => p.terminal);
  test.skip(!shellPod, 'no terminal pod deployed (terminal.enabled=false)');

  await page.goto('/terminal');
  await expect(page.getByRole('heading', { name: 'Terminal' })).toBeVisible();

  // Targets are discovered, not typed: the shell container is preselected.
  const selects = page.locator('select');
  await expect(selects.nth(0)).toContainText('naslos');  // namespace
  await expect(selects.nth(1)).toContainText('— shell'); // pod, marked
  await expect(selects.nth(1)).toHaveValue((shellPod as any).name);
  await expect(selects.nth(2)).toHaveValue('shell');     // container

  await page.click('button:has-text("Connect")');

  // The session is live once the shell prompt appears (PS1 comes from the image's
  // profile.d, so this also proves the login shell started).
  await expect(screen(page)).toContainText('naslos:', { timeout: 30_000 });

  // Click into the terminal so keystrokes land in xterm's input textarea rather
  // than the Connect button that was just pressed.
  await page.locator('.xterm-screen').click();

  // A command the shell echoes back proves the whole chain: keystrokes out as
  // binary frames, exec stdin, output back over the same socket.
  await page.keyboard.type('echo NASLOS-TERMINAL-OK');
  await page.keyboard.press('Enter');
  await expect(screen(page)).toContainText('NASLOS-TERMINAL-OK', { timeout: 30_000 });

  // The shell runs as root (that is the point of this container).
  await page.keyboard.type('id -u');
  await page.keyboard.press('Enter');
  await expect(screen(page)).toContainText('0', { timeout: 15_000 });

  // Host tooling is reachable from the terminal: the `zpool` wrapper chroots
  // into /host and reports the real pool.
  await page.keyboard.type('zpool list');
  await page.keyboard.press('Enter');
  await expect(screen(page)).toContainText('test', { timeout: 20_000 });

  await page.click('button:has-text("Disconnect")');
  await expect(screen(page)).toContainText('Session closed', { timeout: 15_000 });
});

test('the exec preflight reports what would go wrong, before the socket opens', async ({ request }) => {
  test.skip(!(await terminalReachable(request)),
    'run this against the authenticated entry point (Authelia) to exercise the preflight');

  const pods = await (await request.get('/api/pods?namespace=naslos')).json();
  const shellPod = (pods as any[]).find(p => p.terminal);

  // A misspelled pod is a 404 the UI can show, instead of a failed handshake
  // with no explanation.
  const missing = await request.get('/api/ws/exec?namespace=naslos&pod=nosuchpod123&shell=bash');
  expect(missing.status()).toBe(404);
  expect(await missing.text()).toContain('cannot find pod');

  const badNamespace = await request.get('/api/ws/exec?namespace=Not..Valid&pod=x&shell=bash');
  expect(badNamespace.status()).toBe(400);

  if (!shellPod) return;

  // Only shells are allowed: the endpoint must not become "run anything as root".
  const badShell = await request.get(`/api/ws/exec?namespace=naslos&pod=${shellPod.name}&shell=rm`);
  expect(badShell.status()).toBe(400);
  expect(await badShell.text()).toContain('unsupported shell');

  // The preflight answers with the container it resolved, so the UI can show
  // what it is about to attach to.
  const ok = await request.get(`/api/ws/exec?namespace=naslos&pod=${shellPod.name}&shell=bash`);
  expect(ok.status(), await ok.text()).toBe(200);
  const resolved = await ok.json();
  expect(resolved.container).toBe('shell');
  expect(resolved.status).toBe('ready to attach');
});

