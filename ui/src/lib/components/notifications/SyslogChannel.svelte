<script lang="ts">
  import { onMount } from 'svelte';
  import Button from '../Button.svelte';
  import ErrorState from '../ErrorState.svelte';
  import FormField from '../FormField.svelte';
  import Skeleton from '../Skeleton.svelte';
  import ChannelMessage from './ChannelMessage.svelte';
  import EventPicker from './EventPicker.svelte';
  import SeveritySelect from './SeveritySelect.svelte';
  import type { SyslogSettings } from './types';
  import { emptySyslog, errorText, eventTypes, portField, syslogFacilities } from './types';
  import { api } from '$lib/api';
  import { confirmAction } from '$lib/stores/confirm';

  let settings: SyslogSettings = { ...emptySyslog, enabledEvents: [...emptySyslog.enabledEvents] };
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
      settings = await api<SyslogSettings>('/api/notifications/syslog');
      loaded = true;
    } catch (e) {
      const text = 'Failed to load syslog settings: ' + errorText(e);
      if (silent && loaded) {
        loadError = 'Saved, but refreshing the syslog settings failed: ' + text;
      } else {
        loadError = text;
        loaded = false;
      }
    } finally {
      loading = false;
    }
  }

  async function save() {
    if (!loaded) {
      message = 'The syslog settings could not be loaded; retry before saving.';
      messageType = 'error';
      return;
    }
    saving = true;
    message = '';
    try {
      // Explicit body: never echo read-only view fields back to the API.
      const payload = {
        enabled: settings.enabled,
        protocol: settings.protocol,
        host: settings.host,
        ...portField(settings.port),
        facility: settings.facility,
        enabledEvents: settings.enabledEvents,
        minSeverity: settings.minSeverity
      };
      await api('/api/notifications/syslog', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload)
      });
      message = 'Syslog settings saved successfully!';
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
      await api('/api/notifications/syslog/test', { method: 'POST' });
      message = 'Test syslog message sent!';
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
      title: 'Remove syslog channel',
      message: 'Remove the syslog channel? Its target and routing are erased.',
      confirmLabel: 'Remove',
      tone: 'danger'
    });
    if (!ok) return;
    saving = true;
    message = '';
    try {
      await api('/api/notifications/syslog', { method: 'DELETE' });
      message = 'Syslog channel removed.';
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
    <h2 class="text-lg font-bold">Syslog Channel</h2>
    <p class="text-sm text-gray-400">Forward each selected event as an RFC 5424 message.</p>
  </div>

  {#if loading}
    <div class="space-y-2" aria-hidden="true">
      <Skeleton height="h-4" width="w-36" />
      <Skeleton height="h-10" />
      <Skeleton height="h-10" />
    </div>
    <span class="sr-only" role="status" aria-busy="true">Loading syslog settings...</span>
  {:else if !loaded}
    <ErrorState message={loadError || 'The syslog settings could not be loaded.'} retry={retryLoad} />
  {:else}
    {#if loadError}
      <div class="callout callout-bad" role="alert">{loadError}</div>
    {/if}

    <form class="space-y-4" on:submit|preventDefault={save}>
      <label class="flex cursor-pointer items-center gap-4">
        <input type="checkbox" bind:checked={settings.enabled} class="h-5 w-5 rounded" />
        <span>
          <span class="block font-medium">Enable Syslog</span>
          <span class="block text-sm text-gray-400">Forward selected events as RFC 5424 messages.</span>
        </span>
      </label>

      <div class="grid gap-4 sm:grid-cols-3">
        <div class="sm:col-span-2">
          <FormField label="Collector Host" id="syslog-field-1">
            <input id="syslog-field-1" type="text" bind:value={settings.host} class="input w-full" placeholder="syslog.example.com or 192.168.1.10" />
          </FormField>
          <p class="mt-1 text-xs text-gray-500">RFC1918 LAN collectors are allowed; loopback, link-local, metadata and ULA targets are refused.</p>
        </div>
        <FormField label="Port" id="syslog-field-2">
          <input id="syslog-field-2" type="number" min="1" max="65535" bind:value={settings.port} class="input w-full" />
        </FormField>
      </div>

      <div class="grid gap-4 sm:grid-cols-2">
        <FormField label="Protocol" id="syslog-field-3">
          <select id="syslog-field-3" bind:value={settings.protocol} class="input w-full">
            <option value="udp">UDP</option>
            <option value="tcp">TCP</option>
            <option value="tls">TLS</option>
          </select>
          <p class="mt-1 text-xs text-gray-500">TLS validates the collector certificate against public CA roots.</p>
        </FormField>
        <FormField label="Facility" id="syslog-field-4">
          <select id="syslog-field-4" bind:value={settings.facility} class="input w-full">
            {#each syslogFacilities as facility (facility)}<option value={facility}>{facility}</option>{/each}
          </select>
        </FormField>
      </div>

      <div>
        <div class="label">Events</div>
        <EventPicker events={eventTypes} bind:selected={settings.enabledEvents} />
      </div>

      <SeveritySelect id="syslog-field-5" bind:value={settings.minSeverity} />

      <div class="flex flex-wrap gap-3">
        <Button type="submit" variant="primary" loading={saving}>Save Syslog</Button>
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
