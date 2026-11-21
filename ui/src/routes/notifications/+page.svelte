<script lang="ts">
  import { onMount } from 'svelte';
  import LoadingState from '$lib/components/LoadingState.svelte';

  interface Settings {
    enabled: boolean;
    serverUrl: string;
    topic: string;
    hasAuthToken: boolean;
    email: string;
    enabledEvents: string[];
    minSeverity: string;
  }

  interface WebhookSettings {
    enabled: boolean;
    url: string;
    hasAuthToken: boolean;
    enabledEvents: string[];
    minSeverity: string;
  }

  let settings: Settings = {
    enabled: false,
    serverUrl: 'https://ntfy.sh',
    topic: 'naslos-alerts',
    hasAuthToken: false,
    email: '',
    enabledEvents: ['zfs_health', 'disk_failure'],
    minSeverity: 'warning'
  };
  let webhook: WebhookSettings = {
    enabled: false,
    url: '',
    hasAuthToken: false,
    enabledEvents: ['zfs_health', 'disk_failure'],
    minSeverity: 'warning'
  };
  // The stored token is never sent back to the browser; this holds a replacement
  // the operator types, and clearAuthToken is the explicit request to erase it.
  let authToken = '';
  let clearAuthToken = false;
  // Same write-only convention for the webhook channel: the stored token is
  // never sent to the browser.
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

  async function loadSettings() {
    try {
      const res = await fetch('/api/notifications');
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}`);
      }
      settings = await res.json();
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
      // Omit authToken entirely to keep the stored one; send "" only when the
      // operator explicitly asks to clear it.
      const payload: Record<string, unknown> = { ...settings };
      if (authToken.trim() !== '') {
        payload.authToken = authToken.trim();
      } else if (clearAuthToken) {
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
      const payload: Record<string, unknown> = { ...webhook };
      if (webhookAuthToken.trim() !== '') {
        payload.authToken = webhookAuthToken.trim();
      } else if (clearWebhookAuthToken) {
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

  onMount(() => {
    loadSettings();
    loadWebhook();
  });
</script>

<div class="max-w-3xl mx-auto">
  <h1 class="text-3xl font-bold mb-2">Notifications</h1>
  <p class="text-gray-400 mb-6">Configure ntfy push notifications and the generic webhook channel. Each channel routes events and severities independently.</p>

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
        <h2 class="text-lg font-bold">Server Settings</h2>
        <div>
          <label class="label" for="notify-field-1">ntfy Server URL</label>
          <input id="notify-field-1" type="text" bind:value={settings.serverUrl} class="input w-full" placeholder="https://ntfy.sh" />
          <p class="text-xs text-gray-500 mt-1">Use https://ntfy.sh for the public server, or self-host your own.</p>
        </div>
        <div>
          <label class="label" for="notify-field-2">Topic</label>
          <input id="notify-field-2" type="text" bind:value={settings.topic} class="input w-full" placeholder="naslos-alerts" />
          <p class="text-xs text-gray-500 mt-1">Subscribe to this topic in the ntfy app to receive notifications.</p>
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
          <input id="notify-field-4" type="email" bind:value={settings.email} class="input w-full" placeholder="your@email.com" />
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
            <div class="font-medium">Webhook Channel</div>
            <div class="text-sm text-gray-400">POST each selected event as JSON to an external webhook.</div>
          </div>
        </label>

        {#if webhookLoading}
          <LoadingState label="Loading webhook settings..." />
        {:else}
          <div>
            <label class="label" for="webhook-field-1">Webhook URL</label>
            <input id="webhook-field-1" type="text" bind:value={webhook.url} class="input w-full" placeholder="https://hooks.example.com/naslos" />
            <p class="text-xs text-gray-500 mt-1">Only http/https public targets are accepted; loopback, private, link-local and metadata addresses are refused.</p>
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
