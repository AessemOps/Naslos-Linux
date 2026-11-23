import { test, expect } from '@playwright/test';

// The integration API is category/presence-only: destination values and
// tokens are write-only. These route-mocked tests pin the UI contract without
// touching a live notification target.
const enabledEvents = ['zfs_health', 'disk_failure'];

const ntfySettings = {
  enabled: true,
  category: 'ntfy',
  hasServerUrl: true,
  hasTopic: true,
  hasAuthToken: true,
  hasUrlToken: true,
  hasEmail: true,
  allowInsecureHttp: false,
  enabledEvents,
  minSeverity: 'warning'
};

const webhookSettings = {
  enabled: true,
  category: 'webhook',
  hasUrl: true,
  hasUrlToken: true,
  hasAuthToken: true,
  allowInsecureHttp: false,
  enabledEvents,
  minSeverity: 'warning'
};

const smtpSettings = {
  enabled: false,
  host: '',
  port: 587,
  tls: 'starttls',
  username: '',
  hasPassword: false,
  from: '',
  to: [],
  enabledEvents,
  minSeverity: 'warning'
};

const syslogSettings = {
  enabled: false,
  protocol: 'udp',
  host: '',
  port: 514,
  facility: 'local0',
  enabledEvents,
  minSeverity: 'warning'
};

async function mockNotifications(page: any) {
  const puts: { path: string; body: any }[] = [];
  await page.route(/\/api\/notifications(\/.*)?$/, async (route: any) => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    if (request.method() === 'PUT') {
      puts.push({ path, body: request.postDataJSON() });
      return route.fulfill({ json: { status: 'ok' } });
    }
    if (path === '/api/notifications') return route.fulfill({ json: ntfySettings });
    if (path === '/api/notifications/webhook') return route.fulfill({ json: webhookSettings });
    if (path === '/api/notifications/smtp') return route.fulfill({ json: smtpSettings });
    if (path === '/api/notifications/syslog') return route.fulfill({ json: syslogSettings });
    return route.fulfill({ json: { status: 'ok' } });
  });
  await page.goto('/notifications');
  await expect(page.getByText('Loading settings...')).toBeHidden({ timeout: 10_000 });
  return puts;
}

test.describe('notification settings contract', () => {
  test('write-only destinations are not prefilled or blanked by an unrelated save', async ({ page }) => {
    const puts = await mockNotifications(page);

    for (const id of ['#notify-field-1', '#notify-field-2', '#notify-field-3', '#notify-field-4']) {
      await expect(page.locator(id)).toHaveValue('');
    }
    await expect(page.locator('#notify-field-1')).toHaveAttribute('placeholder', /Stored/);
    await expect(page.locator('#notify-field-2')).toHaveAttribute('placeholder', /Stored/);
    await expect(page.locator('#notify-field-4')).toHaveAttribute('placeholder', /Stored/);
    await expect(page.getByText('A server URL is configured but not shown')).toBeVisible();

    await page.locator('select[aria-label="Minimum severity"]').selectOption('error');
    await page.getByRole('button', { name: 'Save Settings' }).click();
    await expect.poll(() => puts.length).toBe(1);
    expect(puts[0].body.minSeverity).toBe('error');
    for (const key of ['serverUrl', 'topic', 'email', 'authToken']) {
      expect(puts[0].body).not.toHaveProperty(key);
    }

    // New values are sent; unrelated destinations stay omitted.
    await page.locator('#notify-field-1').fill('https://ntfy.example.test');
    await page.locator('#notify-field-3').fill('new-token');
    await page.getByLabel('Allow plaintext HTTP with credentials').first().check();
    await page.getByRole('button', { name: 'Save Settings' }).click();
    await expect.poll(() => puts.length).toBe(2);
    expect(puts[1].body.serverUrl).toBe('https://ntfy.example.test');
    expect(puts[1].body.authToken).toBe('new-token');
    expect(puts[1].body.allowInsecureHttp).toBe(true);
    expect(puts[1].body).not.toHaveProperty('topic');
    expect(puts[1].body).not.toHaveProperty('email');

    // Only the explicit clear boxes send empty strings.
    await page.getByLabel('Clear the stored server URL').check();
    await page.getByLabel('Clear the stored token').first().check();
    await page.getByRole('button', { name: 'Save Settings' }).click();
    await expect.poll(() => puts.length).toBe(3);
    expect(puts[2].body.serverUrl).toBe('');
    expect(puts[2].body.authToken).toBe('');
    expect(puts[2].body).not.toHaveProperty('topic');
  });

  test('webhook URL and token follow the same write-only rules', async ({ page }) => {
    const puts = await mockNotifications(page);
    const card = page.locator('.card').filter({ hasText: 'Webhook Channel' });

    await expect(page.locator('#webhook-field-1')).toHaveValue('');
    await expect(page.locator('#webhook-field-1')).toHaveAttribute('placeholder', /Stored/);
    await expect(page.locator('#webhook-field-2')).toHaveValue('');

    await page.locator('#webhook-field-3').selectOption('error');
    await card.getByRole('button', { name: 'Save Webhook' }).click();
    await expect.poll(() => puts.length).toBe(1);
    expect(puts[0].path).toBe('/api/notifications/webhook');
    expect(puts[0].body.minSeverity).toBe('error');
    expect(puts[0].body).not.toHaveProperty('url');
    expect(puts[0].body).not.toHaveProperty('authToken');

    await page.locator('#webhook-field-1').fill('https://hooks.example.test/naslos');
    await page.locator('#webhook-field-2').fill('hook-token');
    await card.getByLabel('Allow plaintext HTTP with credentials').check();
    await card.getByRole('button', { name: 'Save Webhook' }).click();
    await expect.poll(() => puts.length).toBe(2);
    expect(puts[1].body.url).toBe('https://hooks.example.test/naslos');
    expect(puts[1].body.authToken).toBe('hook-token');
    expect(puts[1].body.allowInsecureHttp).toBe(true);

    await card.getByLabel('Clear the stored webhook URL').check();
    await card.getByLabel('Clear the stored token').check();
    await card.getByRole('button', { name: 'Save Webhook' }).click();
    await expect.poll(() => puts.length).toBe(3);
    expect(puts[2].body.url).toBe('');
    expect(puts[2].body.authToken).toBe('');
  });
});
