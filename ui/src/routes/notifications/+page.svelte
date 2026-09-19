<script lang="ts">
  import { onMount } from 'svelte';

  interface Settings {
    enabled: boolean;
    serverUrl: string;
    topic: string;
    hasAuthToken: boolean;
    email: string;
    enabledEvents: string[];
    minSeverity: string;
  }

  let settings: Settings = {
    enabled: false,
    serverUrl: 'https://ntfy.sh',
    topic: 'naslos-alerts',
    hasAuthToken: false,
    email: '',
    enabledEvents: ['zfs_health', 'app_status', 'disk_failure'],
    minSeverity: 'warning'
  };
  // The stored token is never sent back to the browser; this holds a replacement
  // the operator types, and clearAuthToken is the explicit request to erase it.
  let authToken = '';
  let clearAuthToken = false;
  let loading = true;
  let saving = false;
  let message = '';
  let messageType = '';

  const eventTypes = [
    { id: 'zfs_health', label: 'ZFS Pool Health' },
    { id: 'zfs_scrub', label: 'ZFS Scrub Complete' },
    { id: 'app_status', label: 'App Status Change' },
    { id: 'disk_failure', label: 'Disk Failure' },
    { id: 'system_update', label: 'System Update' },
    { id: 'share_access', label: 'Share Access' },
    { id: 'backup_failure', label: 'Backup Failure' },
    { id: 'backup_success', label: 'Backup Success' }
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

  onMount(loadSettings);
</script>

<div class="max-w-3xl mx-auto">
  <h1 class="text-3xl font-bold mb-2">Notifications</h1>
  <p class="text-gray-400 mb-6">Configure ntfy push notifications for system events.</p>

  {#if loading}
    <p class="text-gray-400">Loading settings...</p>
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
        <select bind:value={settings.minSeverity} class="input w-full">
          {#each severityLevels as level}<option value={level.id}>{level.label}</option>{/each}
        </select>
      </div>

      {#if message}<div class={`p-4 rounded-lg border ${messageType === 'success' ? 'bg-green-900/30 border-green-700 text-green-300' : 'bg-red-900/30 border-red-700 text-red-300'}`}><span>{message}</span></div>{/if}

      <div class="flex gap-3">
        <button class="btn btn-primary" on:click={save} disabled={saving}>{saving ? 'Saving...' : 'Save Settings'}</button>
        <button class="btn btn-secondary" on:click={sendTest} disabled={!settings.enabled}>Send Test</button>
      </div>
    </div>
  {/if}
</div>
