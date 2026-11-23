<script lang="ts">
  import { modal } from '$lib/actions/modal';
  import Icon from '$lib/components/Icon.svelte';
  import { onMount, createEventDispatcher } from 'svelte';
  import ExposureForm from './ExposureForm.svelte';

  interface Exposure {
    subdomain: string;
    tls: boolean;
    auth: boolean;
    localOnly: boolean;
    internet: boolean;
    service?: string;
    port?: number;
    scheme?: string;
  }

  export let appName: string;

  let exposure: Exposure = { subdomain: '', tls: true, auth: true, localOnly: false, internet: true };
  let baseDomain = '';
  let selectableDomains: string[] = [];
  let ssoDomains: string[] = [];
  let services: { name: string; port: number; scheme: string }[] = [];
  let loading = true;
  let saving = false;
  let error = '';
  let unsafeAcknowledged = false;

  // Same rule as the install wizard: an exposed route without Authelia auth or
  // TLS is an operator decision that must be acknowledged explicitly, never a
  // silent side effect of editing the form.
  $: unsafeExposure = !!exposure.subdomain && (!exposure.tls || !exposure.auth);

  function onExposureChange() {
    unsafeAcknowledged = false;
  }

  const dispatch = createEventDispatcher();

  async function load() {
    try {
      const res = await fetch(`/api/apps/${encodeURIComponent(appName)}/exposure`);
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}`);
      }
      const data = await res.json();
      exposure = data.exposure;
      baseDomain = data.baseDomain || '';
      selectableDomains = Array.isArray(data.selectableDomains) ? data.selectableDomains : [];
      ssoDomains = Array.isArray(data.ssoDomains) ? data.ssoDomains : [];
      const servicesRes = await fetch(`/api/apps/${encodeURIComponent(appName)}/services`);
      if (servicesRes.ok) {
        const discovered = await servicesRes.json();
        services = Array.isArray(discovered) ? discovered : [];
      }
    } catch (e) {
      error = 'Failed to load exposure: ' + e;
    } finally {
      loading = false;
    }
  }

  async function save() {
    if (unsafeExposure && !unsafeAcknowledged) {
      error = 'Confirm that this app will be exposed without full protection before saving.';
      return;
    }
    saving = true;
    error = '';
    try {
      const res = await fetch(`/api/apps/${encodeURIComponent(appName)}/exposure`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ exposure, baseDomain })
      });
      if (!res.ok) {
        const data = await res.json();
        throw new Error(data.error || 'Update failed');
      }
      dispatch('close');
    } catch (e: any) {
      error = e.message;
    } finally {
      saving = false;
    }
  }

  onMount(load);
</script>

<!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
<div class="fixed inset-0 bg-black/60 z-50 flex items-center justify-center p-4" use:modal={{ onClose: () => dispatch('close') }} on:click|self={() => dispatch('close')}>
  <div class="bg-naslos-surface rounded-2xl border border-naslos-border w-full max-w-lg max-h-[90vh] overflow-hidden flex flex-col">
    <div class="p-6 border-b border-naslos-border flex items-center justify-between">
      <h2 class="text-xl font-bold">Exposure — {appName}</h2>
      <button class="icon-btn -mr-2 text-gray-400 hover:text-naslos-strong hover:bg-naslos-border" aria-label="Close" on:click={() => dispatch('close')}><Icon name="x" /></button>
    </div>
    <div class="flex-1 overflow-y-auto p-6">
      {#if loading}
        <p class="text-gray-400">Loading exposure...</p>
      {:else}
        <ExposureForm
          bind:exposure
          bind:baseDomain
          {selectableDomains}
          {ssoDomains}
          {services}
          on:change={onExposureChange}
        />
        {#if unsafeExposure}
          <div class="mt-5 p-4 rounded-lg border border-red-700 bg-red-900/30 text-red-200 text-sm">
            <p class="font-medium mb-1">This app will be exposed without full protection</p>
            <p>
              {#if !exposure.auth}Without Naslos login (auth is off).{/if}
              {#if !exposure.tls}Without TLS (traffic is cleartext).{/if}
              Anything that can reach <span class="font-mono">{exposure.subdomain}.{baseDomain}</span>
              can use its admin interface.
            </p>
          </div>
          <label class="mt-3 flex items-start gap-3 cursor-pointer">
            <input type="checkbox" class="mt-1" bind:checked={unsafeAcknowledged} />
            <span class="font-medium text-red-300">
              I understand this app is reachable without {#if !exposure.auth}Naslos login{/if}{#if !exposure.auth && !exposure.tls} and {/if}{#if !exposure.tls}encryption{/if}.
            </span>
          </label>
        {/if}
      {/if}
      {#if error}<div role="alert" class="mt-4 p-3 rounded-lg bg-red-900/30 border border-red-700 text-red-300 text-sm">{error}</div>{/if}
    </div>
    <div class="p-6 border-t border-naslos-border flex justify-end gap-3">
      <button class="btn btn-secondary" on:click={() => dispatch('close')}>Cancel</button>
      <button class="btn btn-primary" on:click={save} disabled={saving || loading || (unsafeExposure && !unsafeAcknowledged)}>{saving ? 'Saving...' : 'Save'}</button>
    </div>
  </div>
</div>
