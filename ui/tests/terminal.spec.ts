import { test, expect, request as playwrightRequest } from '@playwright/test';

// The terminal reaches a root shell in a privileged container, so it must not be
// usable without a session. The only listener is Traefik, which answers an
// unauthenticated request with a 302 to the Authelia portal; it must never be a
// 200 (the interactive tests below use the authenticated session).
const TERMINAL_PATHS = [
  '/api/pods?namespace=naslos',
  '/api/pods?namespace=naslos-privileged',
  '/api/namespaces',
  '/api/ws/exec?namespace=naslos&pod=whatever&shell=sh'
];

test('the terminal is refused without an authenticated session', async () => {
  // A context with no storage state, i.e. exactly what an anonymous caller has:
  // the `request` fixture is authenticated by the setup project's session.
  // maxRedirects: 0 is what makes the refusal observable - otherwise the
  // forwardAuth 302 to the portal is followed and the portal page answers 200.
  const anon = await playwrightRequest.newContext({
    baseURL: test.info().project.use.baseURL as string,
    ignoreHTTPSErrors: true,
    maxRedirects: 0,
    // Needed: newContext() otherwise inherits this project's storageState, and
    // the "anonymous" caller would carry the admin session.
    storageState: { cookies: [], origins: [] }
  });

  try {
    for (const path of TERMINAL_PATHS) {
      const res = await anon.get(path);
      const status = res.status();
      expect(status, `${path} answered ${status}`).not.toBe(200);
      expect([302, 401, 403], `${path} answered ${status}`).toContain(status);
    }
  } finally {
    await anon.dispose();
  }
});

// AUDIT-M6: the privileged workloads (agent, samba, nfs, terminal) live in
// their own namespace, so the shell pod is discovered by scanning the candidate
// namespaces rather than assuming `naslos`.
const NS_CANDIDATES = ['naslos-privileged', 'naslos'];

// Find the shell pod across the candidate namespaces. Returns the pod and the
// namespace it was found in, so the exec assertions target the right one.
async function findShellPod(request: any): Promise<{ pod: any; namespace: string } | null> {
  for (const namespace of NS_CANDIDATES) {
    const res = await request.get(`/api/pods?namespace=${namespace}`);
    if (res.status() !== 200) continue;
    const pods = await res.json();
    const shell = (pods as any[]).find(p => p.terminal);
    if (shell) return { pod: shell, namespace };
  }
  return null;
}

// Whether the terminal is reachable with the authenticated session. It is not
// when the terminal component is disabled (terminal.enabled=false), in which case
// the interactive tests skip rather than fail.
async function terminalReachable(request: any): Promise<boolean> {
  const res = await request.get('/api/namespaces');
  return res.status() === 200;
}

// xterm renders each row into the DOM, so the visible screen can be asserted.
function screen(page: any) {
  return page.locator('.xterm-rows');
}

test('the terminal lists pods, preselects the shell container and runs commands', async ({ page, request }) => {
  test.skip(!(await terminalReachable(request)),
    'run this against the authenticated entry point (Authelia) to exercise the terminal');

  const found = await findShellPod(request);
  test.skip(!found, 'no terminal pod deployed (terminal.enabled=false)');
  const { pod: shellPod, namespace } = found!;

  await page.goto('/terminal');
  await expect(page.getByRole('heading', { name: 'Terminal' })).toBeVisible();

  // Targets are discovered, not typed: the shell container is preselected. The
  // namespace field shows whichever namespace holds the shell pod (AUDIT-M6:
  // that is the privileged one).
  const selects = page.locator('select');
  await expect(selects.nth(0)).toContainText(namespace);  // namespace
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

  const found = await findShellPod(request);
  const shellPod = found?.pod;
  const ns = found?.namespace ?? 'naslos';

  // A misspelled pod is a 404 the UI can show, instead of a failed handshake
  // with no explanation.
  const missing = await request.get(`/api/ws/exec?namespace=${ns}&pod=nosuchpod123&shell=bash`);
  expect(missing.status()).toBe(404);
  expect(await missing.text()).toContain('cannot find pod');

  const badNamespace = await request.get('/api/ws/exec?namespace=Not..Valid&pod=x&shell=bash');
  expect(badNamespace.status()).toBe(400);

  if (!shellPod) return;

  // Only shells are allowed: the endpoint must not become "run anything as root".
  const badShell = await request.get(`/api/ws/exec?namespace=${ns}&pod=${shellPod.name}&shell=rm`);
  expect(badShell.status()).toBe(400);
  expect(await badShell.text()).toContain('unsupported shell');

  // The preflight answers with the container it resolved, so the UI can show
  // what it is about to attach to.
  const ok = await request.get(`/api/ws/exec?namespace=${ns}&pod=${shellPod.name}&shell=bash`);
  expect(ok.status(), await ok.text()).toBe(200);
  const resolved = await ok.json();
  expect(resolved.container).toBe('shell');
  expect(resolved.status).toBe('ready to attach');
});

