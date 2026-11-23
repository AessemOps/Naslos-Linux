import { test, expect } from '@playwright/test';

test.describe('domains & SSL', () => {
  test('the domains page loads with the base domain and status', async ({ page }) => {
    await page.goto('/domains');
    await expect(page.getByRole('heading', { name: 'Domains & SSL' })).toBeVisible();
    await expect(page.getByText('Loading domains...')).toBeHidden({ timeout: 10_000 });
    await expect(page.getByRole('button', { name: '+ Add Domain' })).toBeVisible();
  });

  test('reports cert-manager availability from the API', async ({ page, request }) => {
    const res = await request.get('/api/domains');
    expect(res.ok(), await res.text()).toBeTruthy();
    const data = await res.json();
    expect(typeof data.certManager).toBe('boolean');
    expect(typeof data.baseDomain).toBe('string');
    // FR-APP-10/15: the exposure UI and the SSO toggle read these.
    expect(Array.isArray(data.selectableDomains)).toBe(true);
    expect(data.selectableDomains).toContain(data.baseDomain);
    expect(Array.isArray(data.ssoDomains)).toBe(true);
    expect(data.ssoDomains).toContain(data.baseDomain);

    await page.goto('/domains');
    if (data.certManager) {
      await expect(page.getByText('cert-manager is not installed')).toBeHidden();
    } else {
      await expect(page.getByText('cert-manager is not installed')).toBeVisible();
    }
  });

  test('exposes the SSO toggle for a non-primary domain', async ({ page, request }) => {
    const res = await request.get('/api/domains');
    const data = await res.json();
    const secondary = (data.domains || []).find((d: any) => d.baseDomain !== data.baseDomain);
    test.skip(!secondary, 'no secondary domain registered');

    await page.goto('/domains');
    const row = page.locator('.card', { hasText: secondary.baseDomain });
    await expect(row.getByRole('button', { name: /SSO/ })).toBeVisible();
  });

  test('opens the add-domain form', async ({ page }) => {
    await page.goto('/domains');
    await page.getByRole('button', { name: '+ Add Domain' }).click();
    await expect(page.getByRole('heading', { name: 'Add Domain' })).toBeVisible();
    await expect(page.getByLabel('Base domain')).toBeVisible();
    await page.getByRole('button', { name: 'Cancel' }).click();
  });

  test('shows the provider API-rights info bubble', async ({ page }) => {
    await page.goto('/domains');
    await page.getByRole('button', { name: '+ Add Domain' }).click();
    const info = page.getByRole('button', { name: 'API rights required' });
    await expect(info).toBeVisible();
    await info.click();
    await expect(page.getByRole('tooltip')).toContainText(/DNS.*Edit/);
    await page.getByRole('button', { name: 'Cancel' }).click();
  });

  test('validates a malformed domain', async ({ request }) => {
    const res = await request.put('/api/domains/not_a_domain', {
      data: {
        baseDomain: 'not_a_domain',
        dnsProvider: 'cloudflare',
        credentialsSecret: 'x',
        environment: 'staging'
      }
    });
    expect(res.status()).toBe(400);
  });
});

test.describe('app DNS records', () => {
  test('unsupported providers cannot opt into app records', async ({ request }) => {
    const res = await request.put('/api/domains/app-records-test.invalid', {
      data: {
        baseDomain: 'app-records-test.invalid',
        dnsProvider: 'passthrough',
        acmeEmail: 'nobody@example.com',
        environment: 'staging',
        solver: {},
        appRecordsDdnsEntry: 'abc'
      }
    });
    expect(res.status()).toBe(400);
    expect(await res.text()).toContain('cannot create app DNS records');
  });

  test('the app-records toggle explains why it is unavailable', async ({ page }) => {
    await page.route('**/api/domains', (route) =>
      route.fulfill({
        json: {
          domains: [],
          baseDomain: 'naslos.local',
          certManager: true,
          selectableDomains: ['naslos.local'],
          ssoDomains: ['naslos.local']
        }
      })
    );
    await page.route('**/api/ddns', (route: any) =>
      route.fulfill({ json: { entries: [], enabled: true } })
    );
    await page.route('**/api/providers', (route: any) =>
      route.fulfill({
        json: {
          providers: [
            { name: 'cloudflare', displayName: 'Cloudflare', certManager: true, fields: [] }
          ]
        }
      })
    );

    await page.goto('/domains');
    await page.getByRole('button', { name: '+ Add Domain' }).click();
    await expect(page.getByLabel(/Create DNS records for apps/)).toBeDisabled();
    await expect(page.getByText('No non-wildcard DDNS entry exists yet')).toBeVisible();
  });
});

test.describe('redacted domain config', () => {
  const providers = [
    {
      name: 'cloudflare',
      displayName: 'Cloudflare',
      certManager: true,
      fields: [
        { key: 'apiToken', label: 'API token', type: 'string', required: true, secret: true },
        { key: 'proxied', label: 'Proxied (orange cloud)', type: 'bool', default: 'false' }
      ]
    },
    { name: 'passthrough', displayName: 'Passthrough', certManager: true, fields: [] }
  ];
  const cloudflareDomain = {
    baseDomain: 'example.test',
    dnsProvider: 'cloudflare',
    credentialsSecret: 'naslos-domain-example-test-creds',
    providerConfig: {},
    redactedProviderConfigKeys: ['legacyKey'],
    acmeEmail: 'ops@example.test',
    environment: 'staging',
    primary: false,
    sso: false,
    solverConfigured: false
  };
  const passthroughDomain = {
    baseDomain: 'pass.test',
    dnsProvider: 'passthrough',
    credentialsSecret: '',
    providerConfig: {},
    acmeEmail: 'ops@pass.test',
    environment: 'staging',
    primary: false,
    sso: false,
    solverConfigured: true
  };

  async function mockDomains(page: any, putBody: { value: any }) {
    await page.route(/\/api\/domains(\/.*)?$/, async (route: any) => {
      const request = route.request();
      const path = new URL(request.url()).pathname;
      if (request.method() === 'PUT') {
        putBody.value = request.postDataJSON();
        return route.fulfill({ json: { ...cloudflareDomain, ...putBody.value } });
      }
      if (path.endsWith('/certificate')) {
        return route.fulfill({ json: { status: 'ready', reason: '', secretName: 'tls' } });
      }
      return route.fulfill({
        json: {
          domains: [cloudflareDomain, passthroughDomain],
          baseDomain: 'example.test',
          certManager: true,
          selectableDomains: ['example.test', 'pass.test'],
          ssoDomains: ['example.test']
        }
      });
    });
    await page.route('**/api/providers', (route: any) =>
      route.fulfill({ json: { providers } })
    );
    await page.route('**/api/ddns', (route: any) =>
      route.fulfill({ json: { entries: [], enabled: false, intervalSeconds: 300 } })
    );
  }

  test('provider secrets are not prefilled or cleared by an unrelated save', async ({ page }) => {
    const put: { value: any } = { value: null };
    await mockDomains(page, put);

    await page.goto('/domains');
    await expect(page.getByText('Loading domains...')).toBeHidden({ timeout: 10_000 });
    await expect(page.getByText(/Configured \(values hidden\)/)).toBeVisible();
    const row = page.locator('.card', { hasText: 'example.test' });
    await row.getByRole('button', { name: 'Edit', exact: true }).click();

    await expect(page.locator('#domain-field-apiToken')).toHaveValue('');
    await expect(page.locator('#domain-field-apiToken')).toHaveAttribute('placeholder', /unchanged/);
    await expect(page.locator('#domain-clear-apiToken')).toBeVisible();

    await page.getByLabel('ACME email').fill('new@example.test');
    await page.getByRole('button', { name: 'Save', exact: true }).click();
    await expect.poll(() => put.value).toBeTruthy();
    expect(put.value.acmeEmail).toBe('new@example.test');
    expect(put.value).not.toHaveProperty('solver');
    // An empty fields map still lets the server preserve stored config and
    // secrets; it must not carry blank apiToken/proxied values.
    expect(put.value.fields).toEqual({});
    expect(JSON.stringify(put.value)).not.toContain('apiToken');
  });

  test('a configured passthrough solver is indicated and not sent on an unrelated save', async ({ page }) => {
    const put: { value: any } = { value: null };
    await mockDomains(page, put);

    await page.goto('/domains');
    await expect(page.getByText('Loading domains...')).toBeHidden({ timeout: 10_000 });
    const row = page.locator('.card', { hasText: 'pass.test' });
    await row.getByRole('button', { name: 'Edit', exact: true }).click();

    await expect(page.locator('#domain-solver')).toHaveValue('');
    await expect(page.locator('#domain-solver')).toHaveAttribute('placeholder', /Configured/);
    await expect(page.getByText('A solver is configured; its body is not returned')).toBeVisible();

    await page.locator('#domain-env').selectOption('production');
    await page.getByRole('button', { name: 'Save', exact: true }).click();
    await expect.poll(() => put.value).toBeTruthy();
    expect(put.value.environment).toBe('production');
    expect(put.value).not.toHaveProperty('solver');
  });
});
