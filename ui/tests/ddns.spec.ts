import { test, expect } from '@playwright/test';

// Dynamic DNS needs ddns.enabled; skip cleanly (not a pass) when the target
// reports it disabled.
async function ddnsEnabled(request: any): Promise<boolean> {
  const res = await request.get('/api/ddns');
  if (!res.ok()) return false;
  const data = await res.json();
  return data.enabled !== false;
}

test.describe('dynamic DNS', () => {
  test('the page loads with the entry list', async ({ page, request }) => {
    test.skip(!(await ddnsEnabled(request)), 'ddns is disabled on this target');

    await page.goto('/dns');
    await expect(page.getByRole('heading', { name: 'Dynamic DNS' })).toBeVisible();
    await expect(page.getByText('Loading dynamic DNS...')).toBeHidden({ timeout: 10_000 });
    await expect(page.getByRole('button', { name: '+ Add Entry' })).toBeVisible();
  });

  test('the provider list is non-empty', async ({ request }) => {
    test.skip(!(await ddnsEnabled(request)), 'ddns is disabled on this target');

    const res = await request.get('/api/providers');
    expect(res.ok(), await res.text()).toBeTruthy();
    const data = await res.json();
    const serverside = (data.providers || []).filter((p: any) => p.ddns);
    expect(serverside.length).toBeGreaterThan(0);
    expect(serverside.map((p: any) => p.name)).toEqual(expect.arrayContaining(['ovh', 'cloudflare', 'generic']));
  });

  test('the add form renders the selected provider fields', async ({ page, request }) => {
    test.skip(!(await ddnsEnabled(request)), 'ddns is disabled on this target');

    await page.goto('/dns');
    await page.getByRole('button', { name: '+ Add Entry' }).click();
    await expect(page.getByRole('heading', { name: 'Add DDNS Entry' })).toBeVisible();

    const select = page.getByLabel('Provider');
    await select.selectOption('ovh');
    // OVH defaults to DynHost: DynHost credentials show, ZoneDNS fields hide.
    await expect(page.getByLabel(/DynHost username/)).toBeVisible();
    await expect(page.getByLabel(/Application key/)).toBeHidden();
    await page.locator('#ddns-field-mode').selectOption('api');
    await expect(page.getByLabel(/Application key/)).toBeVisible();

    await select.selectOption('generic');
    await expect(page.getByLabel(/Update URL/)).toBeVisible();

    await page.getByRole('button', { name: 'Cancel' }).click();
  });

  test('rejects an unknown provider', async ({ page, request }) => {
    test.skip(!(await ddnsEnabled(request)), 'ddns is disabled on this target');

    const res = await request.post('/api/ddns', {
      data: { provider: 'does-not-exist', zone: 'example.com', record: 'home', recordType: 'A' }
    });
    expect(res.status()).toBe(400);
  });
});
