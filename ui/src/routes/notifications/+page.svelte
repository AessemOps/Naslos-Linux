<script lang="ts">
  import { onMount } from 'svelte';
  import LoadingState from '$lib/components/LoadingState.svelte';

  // GET responses are category/presence-only: the destination values and
  // credentials are write-only, so the form keeps its own drafts.
  interface Settings {
    enabled: boolean;
    category?: string;
    hasServerUrl: boolean;
    hasTopic: boolean;
    hasAuthToken: boolean;
    hasUrlToken: boolean;
    hasEmail: boolean;
    allowInsecureHttp: boolean;
    enabledEvents: string[];
    minSeverity: string;
  }

  interface WebhookSettings {
    enabled: boolean;
    category?: string;
    hasUrl: boolean;
    hasUrlToken: boolean;
    hasAuthToken: boolean;
    allowInsecureHttp: boolean;
    enabledEvents: string[];
    minSeverity: string;
  }

  interface SmtpSettings {
    enabled: boolean;
    host: string;
    port: number;
    tls: string;
    username: string;
    hasPassword: boolean;
    from: string;
    to: string[];
    enabledEvents: string[];
    minSeverity: string;
  }

  interface SyslogSettings {
    enabled: boolean;
    protocol: string;
    host: string;
    port: number;
    facility: string;
    enabledEvents: string[];
    minSeverity: string;
  }

  let settings: Settings = {
    enabled: false,
    hasServerUrl: false,
    hasTopic: false,
    hasAuthToken: false,
    hasUrlToken: false,
    hasEmail: false,
    allowInsecureHttp: false,
    enabledEvents: ['zfs_health', 'disk_failure'],
    minSeverity: 'warning'
  };
  let webhook: WebhookSettings = {
    enabled: false,
    hasUrl: false,
    hasUrlToken: false,
    hasAuthToken: false,
    allowInsecureHttp: false,
    enabledEvents: ['zfs_health', 'disk_failure'],
    minSeverity: 'warning'
  };
  let smtp: SmtpSettings = {
    enabled: false,
    host: '',
    port: 587,
    tls: 'starttls',
    username: '',
    hasPassword: false,
    from: '',
    to: [],
    enabledEvents: ['zfs_health', 'disk_failure'],
    minSeverity: 'warning'
  };
  let smtpRecipients = '';
  let syslog: SyslogSettings = {
    enabled: false,
    protocol: 'udp',
    host: '',
    port: 514,
    facility: 'local0',
    enabledEvents: ['zfs_health', 'disk_failure'],
    minSeverity: 'warning'
  };
  // The SMTP password is write-only, like the webhook token.
  let smtpPassword = '';
  let clearSmtpPassword = false;
  let smtpLoading = true;
  let syslogLoading = true;
  let smtpSaving = false;
  let syslogSaving = false;
  // Write-only ntfy destinations/credentials. Blank drafts are omitted from a
  // save (keep the stored value); the clear flags are the only way to send "".
  let serverUrlInput = '';
  let clearServerUrl = false;
  let topicInput = '';
  let clearTopic = false;
  let emailInput = '';
  let clearEmail = false;
  let authToken = '';
  let clearAuthToken = false;
  // Same write-only convention for the webhook channel.
  let webhookUrlInput = '';
  let clearWebhookUrl = false;
  let webhookAuthToken = '';
  let clearWebhookAuthToken = false;
  let loading = true;
  let webhookLoading = true;
  let saving = false;
  let webhookSaving = false;
  let message = '';
  let messageType = '';

  const eventTypes = [
    { id: 'zfs_health', label: 'ZFS Pool Health' },
    { id: 'disk_failure', label: 'Disk Failure' },
    { id: 'backup_failure', label: 'Backup Failure' },
    { id: 'backup_success', label: 'Backup Success' },
    { id: 'vm_failed', label: 'VM Failed to Start' },
    { id: 'vm_exited_unexpected', label: 'VM Unexpected Exit' }
  ];

  const severityLevels = [
    { id: 'info', label: 'Info (all notifications)' },
    { id: 'warning', label: 'Warning and above' },
    { id: 'error', label: 'Error and above' },
    { id: 'critical', label: 'Critical only' }
  ];

  const tlsModes = [
    { id: 'starttls', label: 'STARTTLS (port 587)' },
    { id: 'implicit', label: 'Implicit TLS / SMTPS (port 465)' },
    { id: 'none', label: 'None (plaintext LAN relay)' }
  ];

  const syslogFacilities = [
    'kern', 'user', 'mail', 'daemon', 'auth', 'syslog', 'lpr', 'news', 'uucp',
    'cron', 'authpriv', 'ftp', 'ntp', 'audit', 'console', 'clock',
    'local0', 'local1', 'local2', 'local3', 'local4', 'local5', 'local6', 'local7'
  ];

  async function loadSettings() {
    try {
      const res = await fetch('/api/notifications');
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}`);
      }
      settings = await res.json();
      serverUrlInput = '';
      clearServerUrl = false;
      topicInput = '';
      clearTopic = false;
      emailInput = '';
      clearEmail = false;
      authToken = '';
      clearAuthToken = false;
    } catch (e) {
      message = 'Failed to load notification settings: ' + e;
      messageType = 'error';
    } finally {
      loading = false;
    }
  }

  async function save() {
    saving = true;
    message = '';
    try {
      // Build the payload explicitly: the GET flags are read-only, and an
      // omitted destination/token keeps its stored value. Only an explicit
      // clear sends "".
      const payload: Record<string, unknown> = {
        enabled: settings.enabled,
        enabledEvents: settings.enabledEvents,
        minSeverity: settings.minSeverity,
        allowInsecureHttp: settings.allowInsecureHttp
      };
      const setDestination = (
        key: string,
        value: string,
        hasStored: boolean,
        clear: boolean
      ) => {
        const text = value.trim();
        if (text !== '') payload[key] = text;
        else if (clear && hasStored) payload[key] = '';
      };
      setDestination('serverUrl', serverUrlInput, settings.hasServerUrl, clearServerUrl);
      setDestination('topic', topicInput, settings.hasTopic, clearTopic);
      setDestination('email', emailInput, settings.hasEmail, clearEmail);
      if (authToken.trim() !== '') {
        payload.authToken = authToken.trim();
      } else if (clearAuthToken && settings.hasAuthToken) {
        payload.authToken = '';
      }
      const res = await fetch('/api/notifications', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload)
      });
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}`);
      }
      message = 'Settings saved successfully!';
      messageType = 'success';
      await loadSettings();
    } catch (e) {
      message = 'Error: ' + e;
      messageType = 'error';
    } finally {
      saving = false;
    }
  }

  async function sendTest() {
    try {
      const res = await fetch('/api/notifications/test', { method: 'POST' });
      if (res.ok) {
        message = 'Test notification sent! Check your ntfy app.';
        messageType = 'success';
      } else {
        throw new Error('Failed to send test');
      }
    } catch (e) {
      message = 'Error: ' + e;
      messageType = 'error';
    }
  }

  function toggleEvent(eventId: string) {
    if (settings.enabledEvents.includes(eventId)) {
      settings.enabledEvents = settings.enabledEvents.filter(e => e !== eventId);
    } else {
      settings.enabledEvents = [...settings.enabledEvents, eventId];
    }
  }

  async function loadWebhook() {
    try {
      const res = await fetch('/api/notifications/webhook');
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}`);
      }
      webhook = await res.json();
      webhookUrlInput = '';
      clearWebhookUrl = false;
      webhookAuthToken = '';
      clearWebhookAuthToken = false;
    } catch (e) {
      message = 'Failed to load webhook settings: ' + e;
      messageType = 'error';
    } finally {
      webhookLoading = false;
    }
  }

  async function saveWebhook() {
    webhookSaving = true;
    message = '';
    try {
      const payload: Record<string, unknown> = {
        enabled: webhook.enabled,
        enabledEvents: webhook.enabledEvents,
        minSeverity: webhook.minSeverity,
        allowInsecureHttp: webhook.allowInsecureHttp
      };
      const url = webhookUrlInput.trim();
      if (url !== '') payload.url = url;
      else if (clearWebhookUrl && webhook.hasUrl) payload.url = '';
      if (webhookAuthToken.trim() !== '') {
        payload.authToken = webhookAuthToken.trim();
      } else if (clearWebhookAuthToken && webhook.hasAuthToken) {
        payload.authToken = '';
      }
      const res = await fetch('/api/notifications/webhook', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload)
      });
      if (!res.ok) {
        const body = await res.json().catch(() => ({}));
        throw new Error(body.error || `HTTP ${res.status}`);
      }
      message = 'Webhook settings saved successfully!';
      messageType = 'success';
      await loadWebhook();
    } catch (e) {
      message = 'Error: ' + e;
      messageType = 'error';
    } finally {
      webhookSaving = false;
    }
  }

  async function sendWebhookTest() {
    try {
      const res = await fetch('/api/notifications/webhook/test', { method: 'POST' });
      if (res.ok) {
        message = 'Test webhook sent!';
        messageType = 'success';
      } else {
        const body = await res.json().catch(() => ({}));
        throw new Error(body.error || `HTTP ${res.status}`);
      }
    } catch (e) {
      message = 'Error: ' + e;
      messageType = 'error';
    }
  }

  async function removeWebhook() {
    if (!confirm('Remove the webhook channel? Its URL and token are erased.')) return;
    webhookSaving = true;
    message = '';
    try {
      const res = await fetch('/api/notifications/webhook', { method: 'DELETE' });
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}`);
      }
      message = 'Webhook channel removed.';
      messageType = 'success';
      await loadWebhook();
    } catch (e) {
      message = 'Error: ' + e;
      messageType = 'error';
    } finally {
      webhookSaving = false;
    }
  }

  function toggleWebhookEvent(eventId: string) {
    if (webhook.enabledEvents.includes(eventId)) {
      webhook.enabledEvents = webhook.enabledEvents.filter(e => e !== eventId);
    } else {
      webhook.enabledEvents = [...webhook.enabledEvents, eventId];
    }
  }

  async function loadSmtp() {
    try {
      const res = await fetch('/api/notifications/smtp');
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}`);
      }
      smtp = await res.json();
      smtpRecipients = smtp.to.join(', ');
      smtpPassword = '';
      clearSmtpPassword = false;
    } catch (e) {
      message = 'Failed to load SMTP settings: ' + e;
      messageType = 'error';
    } finally {
      smtpLoading = false;
    }
  }

  async function saveSmtp() {
    smtpSaving = true;
    message = '';
    try {
      const payload: Record<string, unknown> = {
        ...smtp,
        to: smtpRecipients.split(',').map((r) => r.trim()).filter((r) => r !== '')
      };
      if (smtpPassword.trim() !== '') {
        payload.password = smtpPassword;
      } else if (clearSmtpPassword) {
        payload.password = '';
      }
      const res = await fetch('/api/notifications/smtp', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload)
      });
      if (!res.ok) {
        const body = await res.json().catch(() => ({}));
        throw new Error(body.error || `HTTP ${res.status}`);
      }
      message = 'SMTP settings saved successfully!';
      messageType = 'success';
      await loadSmtp();
    } catch (e) {
      message = 'Error: ' + e;
      messageType = 'error';
    } finally {
      smtpSaving = false;
    }
  }

  async function sendSmtpTest() {
    try {
      const res = await fetch('/api/notifications/smtp/test', { method: 'POST' });
      if (res.ok) {
        message = 'Test email sent!';
        messageType = 'success';
      } else {
        const body = await res.json().catch(() => ({}));
        throw new Error(body.error || `HTTP ${res.status}`);
      }
    } catch (e) {
      message = 'Error: ' + e;
      messageType = 'error';
    }
  }

  async function removeSmtp() {
    if (!confirm('Remove the SMTP channel? Its host, sender and password are erased.')) return;
    smtpSaving = true;
    message = '';
    try {
      const res = await fetch('/api/notifications/smtp', { method: 'DELETE' });
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}`);
      }
      message = 'SMTP channel removed.';
      messageType = 'success';
      await loadSmtp();
    } catch (e) {
      message = 'Error: ' + e;
      messageType = 'error';
    } finally {
      smtpSaving = false;
    }
  }

  function toggleSmtpEvent(eventId: string) {
    if (smtp.enabledEvents.includes(eventId)) {
      smtp.enabledEvents = smtp.enabledEvents.filter((e) => e !== eventId);
    } else {
      smtp.enabledEvents = [...smtp.enabledEvents, eventId];
    }
  }

  async function loadSyslog() {
    try {
      const res = await fetch('/api/notifications/syslog');
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}`);
      }
      syslog = await res.json();
    } catch (e) {
      message = 'Failed to load syslog settings: ' + e;
      messageType = 'error';
    } finally {
      syslogLoading = false;
    }
  }

  async function saveSyslog() {
    syslogSaving = true;
    message = '';
    try {
      const res = await fetch('/api/notifications/syslog', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(syslog)
      });
      if (!res.ok) {
        const body = await res.json().catch(() => ({}));
        throw new Error(body.error || `HTTP ${res.status}`);
      }
      message = 'Syslog settings saved successfully!';
      messageType = 'success';
      await loadSyslog();
    } catch (e) {
      message = 'Error: ' + e;
      messageType = 'error';
    } finally {
      syslogSaving = false;
    }
  }

  async function sendSyslogTest() {
    try {
      const res = await fetch('/api/notifications/syslog/test', { method: 'POST' });
      if (res.ok) {
        message = 'Test syslog message sent!';
        messageType = 'success';
      } else {
        const body = await res.json().catch(() => ({}));
        throw new Error(body.error || `HTTP ${res.status}`);
      }
    } catch (e) {
      message = 'Error: ' + e;
      messageType = 'error';
    }
  }

  async function removeSyslog() {
    if (!confirm('Remove the syslog channel? Its target and routing are erased.')) return;
    syslogSaving = true;
    message = '';
    try {
      const res = await fetch('/api/notifications/syslog', { method: 'DELETE' });
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}`);
      }
      message = 'Syslog channel removed.';
      messageType = 'success';
      await loadSyslog();
    } catch (e) {
      message = 'Error: ' + e;
      messageType = 'error';
    } finally {
      syslogSaving = false;
    }
  }

  function toggleSyslogEvent(eventId: string) {
    if (syslog.enabledEvents.includes(eventId)) {
      syslog.enabledEvents = syslog.enabledEvents.filter((e) => e !== eventId);
    } else {
      syslog.enabledEvents = [...syslog.enabledEvents, eventId];
    }
  }

  onMount(() => {
    loadSettings();
    loadWebhook();
    loadSmtp();
    loadSyslog();
  });
</script>

<div class="max-w-3xl mx-auto">
  <h1 class="text-3xl font-bold mb-2">Notifications</h1>
  <p class="text-gray-400 mb-6">Configure ntfy push, the generic webhook, SMTP email and syslog. Each channel routes events and severities independently.</p>

  {#if loading}
    <LoadingState label="Loading settings..." />
  {:else}
    <div class="space-y-6">
      <div class="card">
        <label class="flex items-center gap-4 cursor-pointer">
          <input type="checkbox" bind:checked={settings.enabled} class="w-5 h-5 rounded" />
          <div>
            <div class="font-medium">Enable Notifications</div>
            <div class="text-sm text-gray-400">Send push notifications via ntfy when events occur.</div>
          </div>
        </label>
      </div>

      <div class="card space-y-4">
        <h2 class="text-lg font-bold">
          Server Settings
          {#if settings.hasUrlToken}
            <span class="ml-2 text-xs font-normal px-2 py-0.5 rounded bg-naslos-border text-gray-300">destination/token configured</span>
          {/if}
        </h2>
        <div>
          <label class="label" for="notify-field-1">ntfy Server URL</label>
          <input
            id="notify-field-1"
            type="text"
            bind:value={serverUrlInput}
            class="input w-full"
            placeholder={settings.hasServerUrl ? 'Stored — leave blank to keep' : 'https://ntfy.sh'}
          />
          {#if settings.hasServerUrl}
            <label class="flex items-center gap-2 mt-2 text-sm text-gray-400">
              <input type="checkbox" bind:checked={clearServerUrl} class="w-4 h-4 rounded" />
              Clear the stored server URL
            </label>
          {/if}
          <p class="text-xs text-gray-500 mt-1">
            {settings.hasServerUrl
              ? 'A server URL is configured but not shown. Leave this blank to keep it.'
              : 'No server URL stored. Use https://ntfy.sh for the public server, or self-host your own.'}
          </p>
        </div>
        <div>
          <label class="label" for="notify-field-2">Topic</label>
          <input
            id="notify-field-2"
            type="text"
            bind:value={topicInput}
            class="input w-full"
            placeholder={settings.hasTopic ? 'Stored — leave blank to keep' : 'naslos-alerts'}
          />
          {#if settings.hasTopic}
            <label class="flex items-center gap-2 mt-2 text-sm text-gray-400">
              <input type="checkbox" bind:checked={clearTopic} class="w-4 h-4 rounded" />
              Clear the stored topic
            </label>
          {/if}
          <p class="text-xs text-gray-500 mt-1">
            {settings.hasTopic
              ? 'A topic is configured but not shown. Leave this blank to keep it.'
              : 'Subscribe to this topic in the ntfy app to receive notifications.'}
          </p>
        </div>
        <div>
          <label class="label" for="notify-field-3">Auth Token (optional)</label>
          <input
            id="notify-field-3"
            type="password"
            bind:value={authToken}
            class="input w-full"
            placeholder={settings.hasAuthToken ? 'Stored — leave blank to keep' : 'Bearer token'}
          />
          {#if settings.hasAuthToken}
            <label class="flex items-center gap-2 mt-2 text-sm text-gray-400">
              <input type="checkbox" bind:checked={clearAuthToken} class="w-4 h-4 rounded" />
              Clear the stored token
            </label>
          {/if}
          <p class="text-xs text-gray-500 mt-1">The stored token is never shown. Leave this blank to keep it, or type a new one to replace it.</p>
        </div>
        <div>
          <label class="label" for="notify-field-4">Email (optional)</label>
          <input
            id="notify-field-4"
            type="email"
            bind:value={emailInput}
            class="input w-full"
            placeholder={settings.hasEmail ? 'Stored — leave blank to keep' : 'your@email.com'}
          />
          {#if settings.hasEmail}
            <label class="flex items-center gap-2 mt-2 text-sm text-gray-400">
              <input type="checkbox" bind:checked={clearEmail} class="w-4 h-4 rounded" />
              Clear the stored email
            </label>
          {/if}
          <p class="text-xs text-gray-500 mt-1">
            {settings.hasEmail
              ? 'An email address is configured but not shown. Leave this blank to keep it.'
              : 'Optional X-Email header value.'}
          </p>
        </div>
        <div>
          <label class="flex items-start gap-2 text-sm text-gray-300 cursor-pointer">
            <input type="checkbox" bind:checked={settings.allowInsecureHttp} class="w-4 h-4 rounded mt-0.5" />
            <span>Allow plaintext HTTP with credentials</span>
          </label>
          <p class="text-xs text-amber-300 mt-1">
            Only for a trusted <span class="font-mono">http</span> server: the bearer token and topic
            travel unencrypted. Enabling this is recorded in the audit log.
          </p>
        </div>
      </div>

      <div class="card space-y-4">
        <h2 class="text-lg font-bold">Events</h2>
        <div class="space-y-2">
          {#each eventTypes as event}
            <label class="flex items-center gap-3 p-2 rounded hover:bg-naslos-dark cursor-pointer">
              <input type="checkbox" checked={settings.enabledEvents.includes(event.id)} on:change={() => toggleEvent(event.id)} class="w-4 h-4 rounded" />
              <span class="text-gray-300">{event.label}</span>
            </label>
          {/each}
        </div>
      </div>

      <div class="card space-y-4">
        <h2 class="text-lg font-bold">Minimum Severity</h2>
        <select bind:value={settings.minSeverity} class="input w-full" aria-label="Minimum severity">
          {#each severityLevels as level}<option value={level.id}>{level.label}</option>{/each}
        </select>
      </div>

      <div class="card space-y-4">
        <label class="flex items-center gap-4 cursor-pointer">
          <input type="checkbox" bind:checked={webhook.enabled} disabled={webhookLoading} class="w-5 h-5 rounded" />
          <div>
            <div class="font-medium">
              Webhook Channel
              {#if webhook.hasUrlToken}
                <span class="ml-2 text-xs font-normal px-2 py-0.5 rounded bg-naslos-border text-gray-300">configured</span>
              {/if}
            </div>
            <div class="text-sm text-gray-400">POST each selected event as JSON to an external webhook.</div>
          </div>
        </label>

        {#if webhookLoading}
          <LoadingState label="Loading webhook settings..." />
        {:else}
          <div>
            <label class="label" for="webhook-field-1">Webhook URL</label>
            <input
              id="webhook-field-1"
              type="text"
              bind:value={webhookUrlInput}
              class="input w-full"
              placeholder={webhook.hasUrl ? 'Stored — leave blank to keep' : 'https://hooks.example.com/naslos'}
            />
            {#if webhook.hasUrl}
              <label class="flex items-center gap-2 mt-2 text-sm text-gray-400">
                <input type="checkbox" bind:checked={clearWebhookUrl} class="w-4 h-4 rounded" />
                Clear the stored webhook URL
              </label>
            {/if}
            <p class="text-xs text-gray-500 mt-1">
              {webhook.hasUrl
                ? 'A URL is configured but not shown. Leave this blank to keep it.'
                : 'Only http/https public targets are accepted; loopback, private, link-local and metadata addresses are refused.'}
            </p>
          </div>
          <div>
            <label class="label" for="webhook-field-2">Bearer Token (optional)</label>
            <input
              id="webhook-field-2"
              type="password"
              bind:value={webhookAuthToken}
              class="input w-full"
              placeholder={webhook.hasAuthToken ? 'Stored — leave blank to keep' : 'Bearer token'}
            />
            {#if webhook.hasAuthToken}
              <label class="flex items-center gap-2 mt-2 text-sm text-gray-400">
                <input type="checkbox" bind:checked={clearWebhookAuthToken} class="w-4 h-4 rounded" />
                Clear the stored token
              </label>
            {/if}
            <p class="text-xs text-gray-500 mt-1">Sent as an Authorization header. The stored token is never shown or returned.</p>
          </div>
          <div>
            <label class="flex items-start gap-2 text-sm text-gray-300 cursor-pointer">
              <input type="checkbox" bind:checked={webhook.allowInsecureHttp} class="w-4 h-4 rounded mt-0.5" />
              <span>Allow plaintext HTTP with credentials</span>
            </label>
            <p class="text-xs text-amber-300 mt-1">
              Only for a trusted <span class="font-mono">http</span> target: the bearer token travels
              unencrypted. Enabling this is recorded in the audit log.
            </p>
          </div>
          <div>
            <div class="label">Events</div>
            <div class="space-y-2">
              {#each eventTypes as event}
                <label class="flex items-center gap-3 p-2 rounded hover:bg-naslos-dark cursor-pointer">
                  <input type="checkbox" checked={webhook.enabledEvents.includes(event.id)} on:change={() => toggleWebhookEvent(event.id)} class="w-4 h-4 rounded" />
                  <span class="text-gray-300">{event.label}</span>
                </label>
              {/each}
            </div>
          </div>
          <div>
            <label class="label" for="webhook-field-3">Minimum Severity</label>
            <select id="webhook-field-3" bind:value={webhook.minSeverity} class="input w-full">
              {#each severityLevels as level}<option value={level.id}>{level.label}</option>{/each}
            </select>
          </div>
          <div class="flex gap-3">
            <button class="btn btn-primary" on:click={saveWebhook} disabled={webhookSaving}>{webhookSaving ? 'Saving...' : 'Save Webhook'}</button>
            <button class="btn btn-secondary" on:click={sendWebhookTest} disabled={!webhook.enabled}>Send Test</button>
            <button class="btn btn-danger" on:click={removeWebhook} disabled={webhookSaving}>Remove</button>
          </div>
        {/if}
      </div>

      <div class="card space-y-4">
        <label class="flex items-center gap-4 cursor-pointer">
          <input type="checkbox" bind:checked={smtp.enabled} disabled={smtpLoading} class="w-5 h-5 rounded" />
          <div>
            <div class="font-medium">SMTP Email Channel</div>
            <div class="text-sm text-gray-400">Email each selected event through your own relay.</div>
          </div>
        </label>

        {#if smtpLoading}
          <LoadingState label="Loading SMTP settings..." />
        {:else}
          <div class="grid gap-4 sm:grid-cols-3">
            <div class="sm:col-span-2">
              <label class="label" for="smtp-field-1">SMTP Host</label>
              <input id="smtp-field-1" type="text" bind:value={smtp.host} class="input w-full" placeholder="smtp.example.com or 192.168.1.10" />
              <p class="text-xs text-gray-500 mt-1">RFC1918 LAN relays are allowed; loopback, link-local, metadata and ULA targets are refused.</p>
            </div>
            <div>
              <label class="label" for="smtp-field-2">Port</label>
              <input id="smtp-field-2" type="number" min="1" max="65535" bind:value={smtp.port} class="input w-full" />
            </div>
          </div>
          <div>
            <label class="label" for="smtp-field-3">TLS Mode</label>
            <select id="smtp-field-3" bind:value={smtp.tls} class="input w-full">
              {#each tlsModes as mode}<option value={mode.id}>{mode.label}</option>{/each}
            </select>
            <p class="text-xs text-gray-500 mt-1">Server certificates are always validated against public CA roots. Credentials require STARTTLS or implicit TLS.</p>
          </div>
          <div class="grid gap-4 sm:grid-cols-2">
            <div>
              <label class="label" for="smtp-field-4">Username (optional)</label>
              <input id="smtp-field-4" type="text" bind:value={smtp.username} class="input w-full" autocomplete="off" />
            </div>
            <div>
              <label class="label" for="smtp-field-5">Password (optional)</label>
              <input
                id="smtp-field-5"
                type="password"
                bind:value={smtpPassword}
                class="input w-full"
                autocomplete="new-password"
                placeholder={smtp.hasPassword ? 'Stored — leave blank to keep' : 'SMTP password'}
              />
              {#if smtp.hasPassword}
                <label class="flex items-center gap-2 mt-2 text-sm text-gray-400">
                  <input type="checkbox" bind:checked={clearSmtpPassword} class="w-4 h-4 rounded" />
                  Clear the stored password
                </label>
              {/if}
            </div>
          </div>
          <div class="grid gap-4 sm:grid-cols-2">
            <div>
              <label class="label" for="smtp-field-6">From</label>
              <input id="smtp-field-6" type="email" bind:value={smtp.from} class="input w-full" placeholder="naslos@example.com" />
            </div>
            <div>
              <label class="label" for="smtp-field-7">To</label>
              <input id="smtp-field-7" type="text" bind:value={smtpRecipients} class="input w-full" placeholder="ops@example.com, oncall@example.com" />
              <p class="text-xs text-gray-500 mt-1">Comma-separated bare addresses, at most 20.</p>
            </div>
          </div>
          <div>
            <div class="label">Events</div>
            <div class="space-y-2">
              {#each eventTypes as event}
                <label class="flex items-center gap-3 p-2 rounded hover:bg-naslos-dark cursor-pointer">
                  <input type="checkbox" checked={smtp.enabledEvents.includes(event.id)} on:change={() => toggleSmtpEvent(event.id)} class="w-4 h-4 rounded" />
                  <span class="text-gray-300">{event.label}</span>
                </label>
              {/each}
            </div>
          </div>
          <div>
            <label class="label" for="smtp-field-8">Minimum Severity</label>
            <select id="smtp-field-8" bind:value={smtp.minSeverity} class="input w-full">
              {#each severityLevels as level}<option value={level.id}>{level.label}</option>{/each}
            </select>
          </div>
          <div class="flex gap-3">
            <button class="btn btn-primary" on:click={saveSmtp} disabled={smtpSaving}>{smtpSaving ? 'Saving...' : 'Save SMTP'}</button>
            <button class="btn btn-secondary" on:click={sendSmtpTest} disabled={!smtp.enabled}>Send Test</button>
            <button class="btn btn-danger" on:click={removeSmtp} disabled={smtpSaving}>Remove</button>
          </div>
        {/if}
      </div>

      <div class="card space-y-4">
        <label class="flex items-center gap-4 cursor-pointer">
          <input type="checkbox" bind:checked={syslog.enabled} disabled={syslogLoading} class="w-5 h-5 rounded" />
          <div>
            <div class="font-medium">Syslog Channel</div>
            <div class="text-sm text-gray-400">Forward each selected event as an RFC 5424 message.</div>
          </div>
        </label>

        {#if syslogLoading}
          <LoadingState label="Loading syslog settings..." />
        {:else}
          <div class="grid gap-4 sm:grid-cols-3">
            <div class="sm:col-span-2">
              <label class="label" for="syslog-field-1">Collector Host</label>
              <input id="syslog-field-1" type="text" bind:value={syslog.host} class="input w-full" placeholder="syslog.example.com or 192.168.1.10" />
              <p class="text-xs text-gray-500 mt-1">RFC1918 LAN collectors are allowed; loopback, link-local, metadata and ULA targets are refused.</p>
            </div>
            <div>
              <label class="label" for="syslog-field-2">Port</label>
              <input id="syslog-field-2" type="number" min="1" max="65535" bind:value={syslog.port} class="input w-full" />
            </div>
          </div>
          <div class="grid gap-4 sm:grid-cols-2">
            <div>
              <label class="label" for="syslog-field-3">Protocol</label>
              <select id="syslog-field-3" bind:value={syslog.protocol} class="input w-full">
                <option value="udp">UDP</option>
                <option value="tcp">TCP</option>
                <option value="tls">TLS</option>
              </select>
              <p class="text-xs text-gray-500 mt-1">TLS validates the collector certificate against public CA roots.</p>
            </div>
            <div>
              <label class="label" for="syslog-field-4">Facility</label>
              <select id="syslog-field-4" bind:value={syslog.facility} class="input w-full">
                {#each syslogFacilities as facility}<option value={facility}>{facility}</option>{/each}
              </select>
            </div>
          </div>
          <div>
            <div class="label">Events</div>
            <div class="space-y-2">
              {#each eventTypes as event}
                <label class="flex items-center gap-3 p-2 rounded hover:bg-naslos-dark cursor-pointer">
                  <input type="checkbox" checked={syslog.enabledEvents.includes(event.id)} on:change={() => toggleSyslogEvent(event.id)} class="w-4 h-4 rounded" />
                  <span class="text-gray-300">{event.label}</span>
                </label>
              {/each}
            </div>
          </div>
          <div>
            <label class="label" for="syslog-field-5">Minimum Severity</label>
            <select id="syslog-field-5" bind:value={syslog.minSeverity} class="input w-full">
              {#each severityLevels as level}<option value={level.id}>{level.label}</option>{/each}
            </select>
          </div>
          <div class="flex gap-3">
            <button class="btn btn-primary" on:click={saveSyslog} disabled={syslogSaving}>{syslogSaving ? 'Saving...' : 'Save Syslog'}</button>
            <button class="btn btn-secondary" on:click={sendSyslogTest} disabled={!syslog.enabled}>Send Test</button>
            <button class="btn btn-danger" on:click={removeSyslog} disabled={syslogSaving}>Remove</button>
          </div>
        {/if}
      </div>

      {#if message}
        <div
          class={`callout ${messageType === 'success' ? 'callout-ok' : 'callout-bad'}`}
          role={messageType === 'success' ? 'status' : 'alert'}
        ><span>{message}</span></div>
      {/if}

      <div class="flex gap-3">
        <button class="btn btn-primary" on:click={save} disabled={saving}>{saving ? 'Saving...' : 'Save Settings'}</button>
        <button class="btn btn-secondary" on:click={sendTest} disabled={!settings.enabled}>Send Test</button>
      </div>
    </div>
  {/if}
</div>
