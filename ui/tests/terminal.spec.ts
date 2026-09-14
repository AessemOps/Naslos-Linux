import { test, expect } from '@playwright/test';

// The web terminal is only useful if a browser can attach to the shell container
// and run commands, so these tests drive the real thing: a websocket through
// nginx, an exec through the Kubernetes API, and output rendered by xterm.
//
// They need the chart's terminal pod (terminal.enabled) and the API's exec RBAC;
// without them the picker reports that nothing is deployed to attach to.
async function terminalPodName(request: any): Promise<string | null> {
  const res = await request.get('/api/pods?namespace=naslos');
  if (!res.ok()) return null;
  const pods = await res.json();
  const shell = (Array.isArray(pods) ? pods : []).find((p: any) => p.terminal);
  return shell?.name ?? null;
}

// xterm renders each row into the DOM, so the visible screen can be asserted.
function screen(page: any) {
  return page.locator('.xterm-rows');
}

test('the terminal lists pods, preselects the shell container and runs commands', async ({ page, request }) => {
  const shellPod = await terminalPodName(request);
  test.skip(!shellPod, 'no terminal pod deployed (terminal.enabled=false)');

  await page.goto('/terminal');
  await expect(page.getByRole('heading', { name: 'Terminal' })).toBeVisible();

  // Targets are discovered, not typed: the shell container is preselected.
  const selects = page.locator('select');
  await expect(selects.nth(0)).toContainText('naslos');       // namespace
  await expect(selects.nth(1)).toContainText('— shell');      // pod, marked
  await expect(selects.nth(1)).toHaveValue(shellPod!);
  await expect(selects.nth(2)).toHaveValue('shell');          // container

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

  // Disconnecting is explicit and reported in the terminal.
  await page.click('button:has-text("Disconnect")');
  await expect(screen(page)).toContainText('Session closed', { timeout: 15_000 });
});

test('the exec preflight reports what would go wrong, before the socket opens', async ({ request }) => {
  const shellPod = await terminalPodName(request);

  // A misspelled pod is a 404 the UI can show, instead of a failed handshake
  // with no explanation.
  const missing = await request.get('/api/ws/exec?namespace=naslos&pod=nosuchpod123&shell=bash');
  expect(missing.status()).toBe(404);
  expect(await missing.text()).toContain('cannot find pod');

  const badNamespace = await request.get('/api/ws/exec?namespace=Not..Valid&pod=x&shell=bash');
  expect(badNamespace.status()).toBe(400);

  if (!shellPod) return;

  // Only shells are allowed: the endpoint must not become "run anything as root".
  const badShell = await request.get(`/api/ws/exec?namespace=naslos&pod=${shellPod}&shell=rm`);
  expect(badShell.status()).toBe(400);
  expect(await badShell.text()).toContain('unsupported shell');

  // The preflight answers with the container it resolved, so the UI can show
  // what it is about to attach to.
  const ok = await request.get(`/api/ws/exec?namespace=naslos&pod=${shellPod}&shell=bash`);
  expect(ok.status(), await ok.text()).toBe(200);
  const resolved = await ok.json();
  expect(resolved.container).toBe('shell');
  expect(resolved.status).toBe('ready to attach');
});
