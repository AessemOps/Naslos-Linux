<script lang="ts">
  import { onMount } from 'svelte';
  import Button from '../Button.svelte';
  import ErrorState from '../ErrorState.svelte';
  import FormField from '../FormField.svelte';
  import Skeleton from '../Skeleton.svelte';
  import StatusBadge from '../StatusBadge.svelte';
  import ChannelMessage from './ChannelMessage.svelte';
  import EventPicker from './EventPicker.svelte';
  import SeveritySelect from './SeveritySelect.svelte';
  import type { WebhookSettings } from './types';
  import { emptyWebhook, errorText, eventTypes } from './types';
  import { api } from '$lib/api';
  import { confirmAction } from '$lib/stores/confirm';

  let settings: WebhookSettings = { ...emptyWebhook, enabledEvents: [...emptyWebhook.enabledEvents] };
  let loading = true;
  let loaded = false;
  let saving = false;
  let testing = false;
  let loadError = '';
  let message = '';
  let messageType: 'success' | 'error' | 'info' = 'success';

  let webhookUrlInput = '';
  let clearWebhookUrl = false;
  let webhookAuthToken = '';
  let clearWebhookAuthToken = false;

  async function load(silent = false) {
    if (!silent) loading = true;
    loadError = '';
    try {
      const next = await api<WebhookSettings>('/api/notifications/webhook');
      settings = next;
      webhookUrlInput = '';
      clearWebhookUrl = false;
      webhookAuthToken = '';
      clearWebhookAuthToken = false;
      loaded = true;
    } catch (e) {
      loadError = 'Failed to load webhook settings: ' + errorText(e);
      loaded = false;
    } finally {
      loading = false;
    }
  }

  async function save() {
    if (!loaded) {
      message = 'The webhook settings could not be loaded; retry before saving.';
      messageType = 'error';
      return;
    }
    saving = true;
    message = '';
    try {
      const payload: Record<string, unknown> = {
        enabled: settings.enabled,
        enabledEvents: settings.enabledEvents,
        minSeverity: settings.minSeverity,
        allowInsecureHttp: settings.allowInsecureHttp
      };
      const url = webhookUrlInput.trim();
      if (url !== '') payload.url = url;
      else if (clearWebhookUrl && settings.hasUrl) payload.url = '';
      if (webhookAuthToken.trim() !== '') {
        payload.authToken = webhookAuthToken.trim();
      } else if (clearWebhookAuthToken && settings.hasAuthToken) {
        payload.authToken = '';
      }
      await api('/api/notifications/webhook', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload)
      });
      message = 'Webhook settings saved successfully!';
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
      await api('/api/notifications/webhook/test', { method: 'POST' });
      message = 'Test webhook sent!';
      messageType = 'success';
    } catch (e) {
      message = 'Error: ' + errorText(e);
      messageType = 'error';
    } finally {
      testing = false;
    }
  }

  async function remove() {
    const ok = await confirmAction({
      title: 'Remove webhook channel',
      message: 'Remove the webhook channel? Its URL and token are erased.',
      confirmLabel: 'Remove',
      tone: 'danger'
    });
    if (!ok) return;
    saving = true;
    message = '';
    try {
      await api('/api/notifications/webhook', { method: 'DELETE' });
      message = 'Webhook channel removed.';
      messageType = 'success';
      await load(true);
    } catch (e) {
      message = 'Error: ' + errorText(e);
      messageType = 'error';
    } finally {
      saving = false;
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
      <h2 class="text-lg font-bold">Webhook Channel</h2>
      <p class="text-sm text-gray-400">
        POST each selected event as JSON to an external webhook.
      </p>
    </div>
    {#if loaded && settings.hasUrlToken}
      <StatusBadge tone="ok" label="configured" />
    {/if}
  </div>

  {#if loading}
    <div class="space-y-2" aria-hidden="true">
      <Skeleton height="h-4" width="w-40" />
      <Skeleton height="h-10" />
      <Skeleton height="h-10" />
    </div>
    <span class="sr-only" role="status" aria-busy="true">Loading webhook settings...</span>
  {:else if !loaded}
    <ErrorState message={loadError || 'The webhook settings could not be loaded.'} retry={retryLoad} />
  {:else}
    <form class="space-y-4" on:submit|preventDefault={save}>
      <label class="flex cursor-pointer items-center gap-4">
        <input type="checkbox" bind:checked={settings.enabled} class="h-5 w-5 rounded" />
        <span>
          <span class="block font-medium">Enable Webhook</span>
          <span class="block text-sm text-gray-400">POST selected events as JSON to the target.</span>
        </span>
      </label>

      <FormField label="Webhook URL" id="webhook-field-1">
        <input
          id="webhook-field-1"
          type="text"
          bind:value={webhookUrlInput}
          class="input w-full"
          placeholder={settings.hasUrl ? 'Stored — leave blank to keep' : 'https://hooks.example.com/naslos'}
          aria-describedby="webhook-field-1-hint"
        />
        <p id="webhook-field-1-hint" class="mt-1 text-xs text-gray-500">
          {settings.hasUrl
            ? 'A URL is configured but not shown. Leave this blank to keep it.'
            : 'Only http/https public targets are accepted; loopback, private, link-local and metadata addresses are refused.'}
        </p>
        {#if settings.hasUrl}
          <label class="mt-2 flex items-center gap-2 text-sm text-gray-400">
            <input type="checkbox" bind:checked={clearWebhookUrl} class="h-4 w-4 rounded" />
            Clear the stored webhook URL
          </label>
        {/if}
      </FormField>

      <FormField label="Bearer Token (optional)" id="webhook-field-2">
        <input
          id="webhook-field-2"
          type="password"
          autocomplete="new-password"
          bind:value={webhookAuthToken}
          class="input w-full"
          placeholder={settings.hasAuthToken ? 'Stored — leave blank to keep' : 'Bearer token'}
          aria-describedby="webhook-field-2-hint"
        />
        <p id="webhook-field-2-hint" class="mt-1 text-xs text-gray-500">
          Sent as an Authorization header. The stored token is never shown or returned.
        </p>
        {#if settings.hasAuthToken}
          <label class="mt-2 flex items-center gap-2 text-sm text-gray-400">
            <input type="checkbox" bind:checked={clearWebhookAuthToken} class="h-4 w-4 rounded" />
            Clear the stored token
          </label>
        {/if}
      </FormField>

      <div>
        <label class="flex cursor-pointer items-start gap-2 text-sm text-gray-300">
          <input type="checkbox" bind:checked={settings.allowInsecureHttp} class="mt-0.5 h-4 w-4 rounded" />
          <span>Allow plaintext HTTP with credentials</span>
        </label>
        <p class="mt-1 text-xs text-amber-300">
          Only for a trusted <span class="font-mono">http</span> target: the bearer token travels
          unencrypted. Enabling this is recorded in the audit log.
        </p>
      </div>

      <div>
        <div class="label">Events</div>
        <EventPicker events={eventTypes} bind:selected={settings.enabledEvents} />
      </div>

      <SeveritySelect id="webhook-field-3" bind:value={settings.minSeverity} />

      <div class="flex flex-wrap gap-3">
        <Button type="submit" variant="primary" loading={saving}>Save Webhook</Button>
        <Button
          variant="secondary"
          loading={testing}
          disabled={!settings.enabled}
          on:click={sendTest}
        >Send Test</Button>
        <Button variant="danger" disabled={saving} on:click={remove}>Remove</Button>
      </div>
      <ChannelMessage message={message} type={messageType} />
    </form>
  {/if}
</div>
