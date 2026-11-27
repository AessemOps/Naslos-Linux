<script lang="ts">
  import { onMount } from 'svelte';
  import Button from '../Button.svelte';
  import ErrorState from '../ErrorState.svelte';
  import FormField from '../FormField.svelte';
  import LoadingState from '../LoadingState.svelte';
  import StatusBadge from '../StatusBadge.svelte';
  import ChannelMessage from './ChannelMessage.svelte';
  import EventPicker from './EventPicker.svelte';
  import SeveritySelect from './SeveritySelect.svelte';
  import type { NtfySettings } from './types';
  import { emptyNtfy, errorText, eventTypes } from './types';
  import { api } from '$lib/api';

  let settings: NtfySettings = { ...emptyNtfy, enabledEvents: [...emptyNtfy.enabledEvents] };
  let loading = true;
  let loaded = false;
  let saving = false;
  let testing = false;
  let loadError = '';
  let message = '';
  let messageType: 'success' | 'error' | 'info' = 'success';

  // Write-only destinations/credentials. Blank drafts are omitted from a save
  // (keep the stored value); the clear flags are the only way to send "".
  let serverUrlInput = '';
  let clearServerUrl = false;
  let topicInput = '';
  let clearTopic = false;
  let emailInput = '';
  let clearEmail = false;
  let authToken = '';
  let clearAuthToken = false;

  async function load(silent = false) {
    if (!silent) loading = true;
    loadError = '';
    try {
      const next = await api<NtfySettings>('/api/notifications');
      settings = next;
      serverUrlInput = '';
      clearServerUrl = false;
      topicInput = '';
      clearTopic = false;
      emailInput = '';
      clearEmail = false;
      authToken = '';
      clearAuthToken = false;
      loaded = true;
    } catch (e) {
      // A failed GET must never leave an editable default form: Save is
      // refused until a Retry succeeds.
      loadError = 'Failed to load notification settings: ' + errorText(e);
      loaded = false;
    } finally {
      loading = false;
    }
  }

  async function save() {
    if (!loaded) {
      message = 'The notification settings could not be loaded; retry before saving.';
      messageType = 'error';
      return;
    }
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
      const setDestination = (key: string, value: string, hasStored: boolean, clear: boolean) => {
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
      // The API explains allowInsecureHttp and URL validation failures in the
      // body; ApiError carries that message.
      await api('/api/notifications', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload)
      });
      message = 'Settings saved successfully!';
      messageType = 'success';
      await load(true);
    } catch (e) {
      message = 'Error: ' + errorText(e);
      messageType = 'error';
    } finally {
      saving = false;
    }
  }

  async function sendTest() {
    testing = true;
    try {
      await api('/api/notifications/test', { method: 'POST' });
      message = 'Test notification sent! Check your ntfy app.';
      messageType = 'success';
    } catch (e) {
      message = 'Error: ' + errorText(e);
      messageType = 'error';
    } finally {
      testing = false;
    }
  }

  function retryLoad() {
    load();
  }

  onMount(() => {
    load();
  });
</script>

<div class="card space-y-4">
  <div class="flex flex-wrap items-start justify-between gap-3">
    <div>
      <h2 class="text-lg font-bold">ntfy Push</h2>
      <p class="text-sm text-gray-400">
        Send push notifications via ntfy when events occur.
      </p>
    </div>
    {#if loaded && settings.hasUrlToken}
      <StatusBadge tone="ok" label="destination/token configured" />
    {/if}
  </div>

  {#if loading}
    <LoadingState label="Loading settings..." />
  {:else if !loaded}
    <ErrorState message={loadError || 'The notification settings could not be loaded.'} retry={retryLoad} />
    <div class="flex flex-wrap gap-3">
      <Button variant="primary" disabled>Save Settings</Button>
    </div>
  {:else}
    <form class="space-y-4" on:submit|preventDefault={save}>
      <label class="flex cursor-pointer items-center gap-4">
        <input type="checkbox" bind:checked={settings.enabled} class="h-5 w-5 rounded" />
        <span>
          <span class="block font-medium">Enable Notifications</span>
          <span class="block text-sm text-gray-400">Send push notifications via ntfy when events occur.</span>
        </span>
      </label>

      <div class="grid gap-4 sm:grid-cols-2">
        <FormField label="ntfy Server URL" id="notify-field-1">
          <input
            id="notify-field-1"
            type="text"
            bind:value={serverUrlInput}
            class="input w-full"
            placeholder={settings.hasServerUrl ? 'Stored — leave blank to keep' : 'https://ntfy.sh'}
            aria-describedby="notify-field-1-hint"
          />
          <p id="notify-field-1-hint" class="mt-1 text-xs text-gray-500">
            {settings.hasServerUrl
              ? 'A server URL is configured but not shown. Leave this blank to keep it.'
              : 'No server URL stored. Use https://ntfy.sh for the public server, or self-host your own.'}
          </p>
          {#if settings.hasServerUrl}
            <label class="mt-2 flex items-center gap-2 text-sm text-gray-400">
              <input type="checkbox" bind:checked={clearServerUrl} class="h-4 w-4 rounded" />
              Clear the stored server URL
            </label>
          {/if}
        </FormField>

        <FormField label="Topic" id="notify-field-2">
          <input
            id="notify-field-2"
            type="text"
            bind:value={topicInput}
            class="input w-full"
            placeholder={settings.hasTopic ? 'Stored — leave blank to keep' : 'naslos-alerts'}
            aria-describedby="notify-field-2-hint"
          />
          <p id="notify-field-2-hint" class="mt-1 text-xs text-gray-500">
            {settings.hasTopic
              ? 'A topic is configured but not shown. Leave this blank to keep it.'
              : 'Subscribe to this topic in the ntfy app to receive notifications.'}
          </p>
          {#if settings.hasTopic}
            <label class="mt-2 flex items-center gap-2 text-sm text-gray-400">
              <input type="checkbox" bind:checked={clearTopic} class="h-4 w-4 rounded" />
              Clear the stored topic
            </label>
          {/if}
        </FormField>

        <FormField label="Auth Token (optional)" id="notify-field-3">
          <input
            id="notify-field-3"
            type="password"
            autocomplete="new-password"
            bind:value={authToken}
            class="input w-full"
            placeholder={settings.hasAuthToken ? 'Stored — leave blank to keep' : 'Bearer token'}
            aria-describedby="notify-field-3-hint"
          />
          <p id="notify-field-3-hint" class="mt-1 text-xs text-gray-500">
            The stored token is never shown. Leave this blank to keep it, or type a new one to replace it.
          </p>
          {#if settings.hasAuthToken}
            <label class="mt-2 flex items-center gap-2 text-sm text-gray-400">
              <input type="checkbox" bind:checked={clearAuthToken} class="h-4 w-4 rounded" />
              Clear the stored token
            </label>
          {/if}
        </FormField>

        <FormField label="Email (optional)" id="notify-field-4">
          <input
            id="notify-field-4"
            type="email"
            bind:value={emailInput}
            class="input w-full"
            placeholder={settings.hasEmail ? 'Stored — leave blank to keep' : 'your@email.com'}
            aria-describedby="notify-field-4-hint"
          />
          <p id="notify-field-4-hint" class="mt-1 text-xs text-gray-500">
            {settings.hasEmail
              ? 'An email address is configured but not shown. Leave this blank to keep it.'
              : 'Optional X-Email header value.'}
          </p>
          {#if settings.hasEmail}
            <label class="mt-2 flex items-center gap-2 text-sm text-gray-400">
              <input type="checkbox" bind:checked={clearEmail} class="h-4 w-4 rounded" />
              Clear the stored email
            </label>
          {/if}
        </FormField>
      </div>

      <div>
        <label class="flex cursor-pointer items-start gap-2 text-sm text-gray-300">
          <input type="checkbox" bind:checked={settings.allowInsecureHttp} class="mt-0.5 h-4 w-4 rounded" />
          <span>Allow plaintext HTTP with credentials</span>
        </label>
        <p class="mt-1 text-xs text-amber-300">
          Only for a trusted <span class="font-mono">http</span> server: the bearer token and topic
          travel unencrypted. Enabling this is recorded in the audit log.
        </p>
      </div>

      <div>
        <div class="label">Events</div>
        <EventPicker events={eventTypes} bind:selected={settings.enabledEvents} />
      </div>

      <SeveritySelect
        id="notify-severity"
        ariaLabel="Minimum severity"
        bind:value={settings.minSeverity}
      />

      <div class="flex flex-wrap gap-3">
        <Button type="submit" variant="primary" loading={saving}>Save Settings</Button>
        <Button
          variant="secondary"
          loading={testing}
          disabled={!settings.enabled}
          on:click={sendTest}
        >Send Test</Button>
      </div>
      <ChannelMessage message={message} type={messageType} />
    </form>
  {/if}
</div>
