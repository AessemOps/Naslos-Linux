<script lang="ts">
  import { onMount } from 'svelte';

  interface Settings {
    enabled: boolean;
    serverUrl: string;
    topic: string;
    authToken: string;
    email: string;
    enabledEvents: string[];
    minSeverity: string;
  }

  let settings: Settings = {
    enabled: false,
    serverUrl: 'https://ntfy.sh',
    topic: 'naslos-alerts',
    authToken: '',
    email: '',
    enabledEvents: ['zfs_health', 'app_status', 'disk_failure'],
    minSeverity: 'warning'
  };
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
    { id: 'share_access', label: 'Share Access' }
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
      settings = await res.json();
    } catch (e) {
      console.error('Failed to load settings:', e);
    } finally {
      loading = false;
    }
  }

  async function save() {
    saving = true;
    message = '';
    try {
      const res = await fetch('/api/notifications', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(settings)
      });
      if (res.ok) {
        message = 'Settings saved successfully!';
        messageType = 'success';
      } else {
        throw new Error('Failed to save');
      }
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
          <label class="label">ntfy Server URL</label>
          <input type="text" bind:value={settings.serverUrl} class="input w-full" placeholder="https://ntfy.sh" />
          <p class="text-xs text-gray-500 mt-1">Use https://ntfy.sh for the public server, or self-host your own.</p>
        </div>
        <div>
          <label class="label">Topic</label>
          <input type="text" bind:value={settings.topic} class="input w-full" placeholder="naslos-alerts" />
          <p class="text-xs text-gray-500 mt-1">Subscribe to this topic in the ntfy app to receive notifications.</p>
        </div>
        <div>
          <label class="label">Auth Token (optional)</label>
          <input type="password" bind:value={settings.authToken} class="input w-full" />
        </div>
        <div>
          <label class="label">Email (optional)</label>
          <input type="email" bind:value={settings.email} class="input w-full" placeholder="your@email.com" />
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
