<script lang="ts">
  import { onMount } from 'svelte';
  import Button from '../Button.svelte';
  import ErrorState from '../ErrorState.svelte';
  import FormField from '../FormField.svelte';
  import Skeleton from '../Skeleton.svelte';
  import ChannelMessage from './ChannelMessage.svelte';
  import EventPicker from './EventPicker.svelte';
  import SeveritySelect from './SeveritySelect.svelte';
  import type { SmtpSettings } from './types';
  import { emptySmtp, errorText, eventTypes, portField, tlsModes } from './types';
  import { api } from '$lib/api';
  import { confirmAction } from '$lib/stores/confirm';

  let settings: SmtpSettings = { ...emptySmtp, enabledEvents: [...emptySmtp.enabledEvents] };
  let smtpRecipients = '';
  let smtpPassword = '';
  let clearSmtpPassword = false;
  let loading = true;
  let loaded = false;
  let saving = false;
  let testing = false;
  let loadError = '';
  let message = '';
  let messageType: 'success' | 'error' | 'info' = 'success';

  async function load(silent = false) {
    if (!silent) loading = true;
    loadError = '';
    try {
      const next = await api<SmtpSettings>('/api/notifications/smtp');
      settings = next;
      smtpRecipients = next.to.join(', ');
      smtpPassword = '';
      clearSmtpPassword = false;
      loaded = true;
    } catch (e) {
      loadError = 'Failed to load SMTP settings: ' + errorText(e);
      loaded = false;
    } finally {
      loading = false;
    }
  }

  async function save() {
    if (!loaded) {
      message = 'The SMTP settings could not be loaded; retry before saving.';
      messageType = 'error';
      return;
    }
    saving = true;
    message = '';
    try {
      // Explicit body: the GET view's read-only flags (hasPassword, category)
      // must never be echoed into a save.
      const payload: Record<string, unknown> = {
        enabled: settings.enabled,
        host: settings.host,
        ...portField(settings.port),
        tls: settings.tls,
        username: settings.username,
        from: settings.from,
        to: smtpRecipients
          .split(',')
          .map((r) => r.trim())
          .filter((r) => r !== ''),
        enabledEvents: settings.enabledEvents,
        minSeverity: settings.minSeverity
      };
      if (smtpPassword.trim() !== '') {
        payload.password = smtpPassword;
      } else if (clearSmtpPassword) {
        payload.password = '';
      }
      await api('/api/notifications/smtp', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload)
      });
      message = 'SMTP settings saved successfully!';
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
      await api('/api/notifications/smtp/test', { method: 'POST' });
      message = 'Test email sent!';
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
      title: 'Remove SMTP channel',
      message: 'Remove the SMTP channel? Its host, sender and password are erased.',
      confirmLabel: 'Remove',
      tone: 'danger'
    });
    if (!ok) return;
    saving = true;
    message = '';
    try {
      await api('/api/notifications/smtp', { method: 'DELETE' });
      message = 'SMTP channel removed.';
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
  <div>
    <h2 class="text-lg font-bold">SMTP Email Channel</h2>
    <p class="text-sm text-gray-400">Email each selected event through your own relay.</p>
  </div>

  {#if loading}
    <div class="space-y-2" aria-hidden="true">
      <Skeleton height="h-4" width="w-40" />
      <Skeleton height="h-10" />
      <Skeleton height="h-10" />
    </div>
    <span class="sr-only" role="status" aria-busy="true">Loading SMTP settings...</span>
  {:else if !loaded}
    <ErrorState message={loadError || 'The SMTP settings could not be loaded.'} retry={retryLoad} />
  {:else}
    <form class="space-y-4" on:submit|preventDefault={save}>
      <label class="flex cursor-pointer items-center gap-4">
        <input type="checkbox" bind:checked={settings.enabled} class="h-5 w-5 rounded" />
        <span>
          <span class="block font-medium">Enable SMTP</span>
          <span class="block text-sm text-gray-400">Send selected events through this relay.</span>
        </span>
      </label>

      <div class="grid gap-4 sm:grid-cols-3">
        <div class="sm:col-span-2">
          <FormField label="SMTP Host" id="smtp-field-1">
            <input id="smtp-field-1" type="text" bind:value={settings.host} class="input w-full" placeholder="smtp.example.com or 192.168.1.10" />
          </FormField>
          <p class="mt-1 text-xs text-gray-500">RFC1918 LAN relays are allowed; loopback, link-local, metadata and ULA targets are refused.</p>
        </div>
        <FormField label="Port" id="smtp-field-2">
          <input id="smtp-field-2" type="number" min="1" max="65535" bind:value={settings.port} class="input w-full" />
        </FormField>
      </div>

      <FormField label="TLS Mode" id="smtp-field-3">
        <select id="smtp-field-3" bind:value={settings.tls} class="input w-full">
          {#each tlsModes as mode (mode.id)}<option value={mode.id}>{mode.label}</option>{/each}
        </select>
        <p class="mt-1 text-xs text-gray-500">Server certificates are always validated against public CA roots. Credentials require STARTTLS or implicit TLS.</p>
      </FormField>

      <div class="grid gap-4 sm:grid-cols-2">
        <FormField label="Username (optional)" id="smtp-field-4">
          <input id="smtp-field-4" type="text" bind:value={settings.username} class="input w-full" autocomplete="off" />
        </FormField>
        <FormField label="Password (optional)" id="smtp-field-5">
          <input
            id="smtp-field-5"
            type="password"
            bind:value={smtpPassword}
            class="input w-full"
            autocomplete="new-password"
            placeholder={settings.hasPassword ? 'Stored — leave blank to keep' : 'SMTP password'}
          />
          {#if settings.hasPassword}
            <label class="mt-2 flex items-center gap-2 text-sm text-gray-400">
              <input type="checkbox" bind:checked={clearSmtpPassword} class="h-4 w-4 rounded" />
              Clear the stored password
            </label>
          {/if}
        </FormField>
      </div>

      <div class="grid gap-4 sm:grid-cols-2">
        <FormField label="From" id="smtp-field-6">
          <input id="smtp-field-6" type="email" bind:value={settings.from} class="input w-full" placeholder="naslos@example.com" />
        </FormField>
        <FormField label="To" id="smtp-field-7">
          <input id="smtp-field-7" type="text" bind:value={smtpRecipients} class="input w-full" placeholder="ops@example.com, oncall@example.com" />
          <p class="mt-1 text-xs text-gray-500">Comma-separated bare addresses, at most 20.</p>
        </FormField>
      </div>

      <div>
        <div class="label">Events</div>
        <EventPicker events={eventTypes} bind:selected={settings.enabledEvents} />
      </div>

      <SeveritySelect id="smtp-field-8" bind:value={settings.minSeverity} />

      <div class="flex flex-wrap gap-3">
        <Button type="submit" variant="primary" loading={saving}>Save SMTP</Button>
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
