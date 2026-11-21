import { test, expect } from '@playwright/test';

// The MCP surface is new in 0.4.4; until the node runs that image every test
// here skips on the capability check (the same pattern network.spec.ts uses
// for a pre-feature release).
async function mcpApiPresent(request: any): Promise<boolean> {
  const res = await request.get('/api/mcp');
  return res.status() !== 404;
}

// The suite runs against the live production node: it must never leave the
// endpoint enabled or a token behind, whatever it finds.
test.describe('mcp settings', () => {
  test('the API reports the MCP state without hashes', async ({ request }) => {
    test.skip(!(await mcpApiPresent(request)), 'the deployed API predates the MCP routes');

    const res = await request.get('/api/mcp');
    expect(res.ok(), await res.text()).toBeTruthy();
    const data = await res.json();
    expect(typeof data.enabled).toBe('boolean');
    expect(typeof data.endpointUrl).toBe('string');
    expect(Array.isArray(data.tokens)).toBe(true);
    expect(Array.isArray(data.recentCalls)).toBe(true);
    for (const token of data.tokens) {
      expect(typeof token.id).toBe('string');
      expect(typeof token.name).toBe('string');
      expect(['read', 'write', 'full']).toContain(token.scope);
      // The stored hash (and the plaintext) must never reach the browser.
      expect(token.hash).toBeUndefined();
      expect(token.token).toBeUndefined();
    }
  });

  test('the Settings hub card navigates to the MCP page', async ({ page, request }) => {
    test.skip(!(await mcpApiPresent(request)), 'the deployed API predates the MCP routes');

    await page.goto('/settings');
    await page.getByRole('link', { name: /MCP/ }).click();
    await expect(page).toHaveURL(/\/settings\/mcp$/);
    await expect(page.getByRole('heading', { name: 'MCP' })).toBeVisible();
  });

  test('the page renders the endpoint, tokens and recent calls sections', async ({ page, request }) => {
    test.skip(!(await mcpApiPresent(request)), 'the deployed API predates the MCP routes');

    await page.goto('/settings/mcp');
    await expect(page.getByRole('heading', { name: 'Endpoint' })).toBeVisible();
    await expect(page.getByRole('heading', { name: 'Tokens' })).toBeVisible();
    await expect(page.getByRole('heading', { name: 'Recent calls' })).toBeVisible();
    await expect(page.getByRole('heading', { name: 'Connect a client' })).toBeVisible();
  });

  test('creating a token reveals it exactly once, then revoking removes the row', async ({ page, request }) => {
    test.skip(!(await mcpApiPresent(request)), 'the deployed API predates the MCP routes');

    const before = await (await request.get('/api/mcp')).json();
    const created: { id: string }[] = [];
    const enabledBefore: boolean = before.enabled;

    try {
      await page.goto('/settings/mcp');
      await page.getByRole('button', { name: 'Create token' }).click();
      const dialog = page.getByRole('dialog');
      await expect(dialog.getByRole('heading', { name: 'Create MCP token' })).toBeVisible();

      await dialog.getByLabel('Name').fill('playwright drill');
      // read stays the pre-selected default; the create button reflects it.
      await expect(dialog.getByText('Read-only tools', { exact: false })).toBeVisible();
      await dialog.getByRole('button', { name: 'Create token' }).click();

      // The one-time reveal: nmcp_ prefix, copy affordance, warning text.
      await expect(page.getByRole('heading', { name: 'Token created' })).toBeVisible();
      const tokenText = await page.getByText(/^nmcp_[A-Za-z0-9_-]+$/).textContent();
      expect(tokenText).toBeTruthy();
      expect(tokenText!.startsWith('nmcp_')).toBe(true);
      await expect(page.getByText(/only this once/i)).toBeVisible();

      // The row appears with the read scope badge and no plaintext.
      await page.getByRole('button', { name: 'Done' }).click();
      const row = page.getByRole('row', { name: /playwright drill/ });
      await expect(row).toBeVisible();
      await expect(row.getByText('read')).toBeVisible();
      expect((await row.textContent())?.includes('nmcp_')).toBe(false);

      // Revoke from the row, confirm the modal, row disappears.
      await row.getByRole('button', { name: 'Revoke' }).click();
      const revokeDialog = page.getByRole('dialog');
      await expect(revokeDialog.getByRole('heading', { name: 'Revoke token' })).toBeVisible();
      await revokeDialog.getByRole('button', { name: 'Revoke', exact: true }).click();
      await expect(page.getByRole('row', { name: /playwright drill/ })).toHaveCount(0);
    } finally {
      // Belt and braces: if the UI flow failed mid-way, clean up by API and
      // never leave the endpoint enabled or a drill token behind.
      const after = await (await request.get('/api/mcp')).json();
      for (const token of after.tokens ?? []) {
        if (token.name === 'playwright drill') {
          await request.delete(`/api/mcp/tokens/${token.id}`);
        }
      }
      if (enabledBefore !== true) {
        await request.put('/api/mcp', {
          data: { enabled: false },
          headers: { 'Content-Type': 'application/json' }
        });
      }
    }
  });

  test('the endpoint itself is never anonymously reachable nor swallowed by the SPA', async ({ request }) => {
    test.skip(!(await mcpApiPresent(request)), 'the deployed API predates the MCP routes');

    // Whatever the enabled state (disabled -> 404 by design, enabled -> 401
    // with a Bearer challenge), a raw request must never reach the MCP
    // protocol or serve the SPA's HTML at /mcp.
    const res = await request.post('/mcp', { data: '{}' });
    expect([401, 404]).toContain(res.status());
    expect(res.headers()['content-type'] ?? '').not.toContain('text/html');
    if (res.status() === 401) {
      expect(res.headers()['www-authenticate'] ?? '').toContain('Bearer');
    }
  });
});
