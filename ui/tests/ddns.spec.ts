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

test.describe('redacted DDNS provider config', () => {
  test('redacted fields are not prefilled or cleared unless explicitly cleared', async ({ page }) => {
    const providers = [
      {
        name: 'generic',
        displayName: 'Generic HTTP',
        ddns: { driver: 'http' },
        fields: [
          { key: 'updateUrl', label: 'Update URL', type: 'string', required: true },
          { key: 'body', label: 'Request body template', type: 'string' },
          {
            key: 'allowInsecureHttp',
            label: 'Allow plaintext HTTP with credentials',
            type: 'bool',
            default: 'false'
          },
          { key: 'token', label: 'Token', type: 'string', secret: true }
        ]
      }
    ];
    const entry = {
      id: 'entry-1',
      provider: 'generic',
      zone: 'example.com',
      record: 'home',
      recordType: 'A',
      ttl: 0,
      enabled: true,
      providerConfig: { method: 'GET' },
      redactedProviderConfigKeys: ['updateUrl', 'body'],
      credentialFields: ['token'],
      credentialsSecret: 'naslos-ddns-entry-1'
    };
    const puts: any[] = [];
    await page.route(/\/api\/ddns(\/.*)?$/, async (route) => {
      const request = route.request();
      if (request.method() === 'PUT') {
        puts.push(request.postDataJSON());
        return route.fulfill({ json: entry });
      }
      return route.fulfill({ json: { entries: [entry], enabled: true, intervalSeconds: 300 } });
    });
    await page.route('**/api/providers', (route) =>
      route.fulfill({ json: { providers } })
    );
    await page.route('**/api/domains', (route) =>
      route.fulfill({
        json: {
          domains: [],
          baseDomain: 'example.test',
          certManager: false,
          selectableDomains: ['example.test'],
          ssoDomains: ['example.test']
        }
      })
    );

    await page.goto('/dns');
    await expect(page.getByText('Loading dynamic DNS...')).toBeHidden({ timeout: 10_000 });
    await expect(page.getByText(/Configured \(values hidden\)/)).toBeVisible();
    await page.getByRole('button', { name: 'Edit', exact: true }).click();

    await expect(page.locator('#ddns-field-updateUrl')).toHaveValue('');
    await expect(page.locator('#ddns-field-updateUrl')).toHaveAttribute('placeholder', /Configured/);
    await expect(page.locator('#ddns-field-body')).toHaveValue('');
    await expect(page.locator('#ddns-field-token')).toHaveValue('');
    await expect(page.locator('#ddns-field-token')).toHaveAttribute('placeholder', /unchanged/);
    await expect(page.locator('#ddns-clear-updateUrl')).toHaveCount(0);

    await page.locator('#ddns-zone').fill('example.net');
    await page.locator('#ddns-field-allowInsecureHttp').check();
    await page.locator('#ddns-clear-body').check();
    await page.locator('#ddns-clear-token').check();
    await page.getByRole('button', { name: 'Save', exact: true }).click();

    await expect.poll(() => puts.length).toBe(1);
    expect(puts[0].zone).toBe('example.net');
    expect(puts[0].fields).toEqual({
      allowInsecureHttp: 'true',
      body: '',
      token: ''
    });
    expect(puts[0].fields).not.toHaveProperty('updateUrl');
    expect(puts[0].fields).not.toHaveProperty('method');
  });
});
