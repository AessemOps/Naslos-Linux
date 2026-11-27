<script lang="ts">
  import { onMount, createEventDispatcher } from 'svelte';
  import AsyncState from './AsyncState.svelte';
  import Button from './Button.svelte';
  import Modal from './Modal.svelte';
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
  let loaded = false;
  let saving = false;
  let error = '';
  let loadError = '';
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
    loading = true;
    loaded = false;
    loadError = '';
    error = '';
    let loadedExposure = false;
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
      loaded = true;
      loadedExposure = true;
    } catch (e) {
      // Never leave an editable default exposure behind: saving it would
      // silently unexpose the app.
      loadError = 'Failed to load exposure: ' + (e instanceof Error ? e.message : String(e));
    } finally {
      loading = false;
    }
    if (!loadedExposure) return;
    // The service list only feeds the optional service picker; a failure there
    // must not turn a loaded exposure into an error page.
    try {
      const servicesRes = await fetch(`/api/apps/${encodeURIComponent(appName)}/services`);
      if (servicesRes.ok) {
        const discovered = await servicesRes.json();
        services = Array.isArray(discovered) ? discovered : [];
      }
    } catch {
      // Keep the existing list; the picker remains usable with free text.
    }
  }

  async function save() {
    if (!loaded) {
      error = 'The exposure could not be loaded; retry before saving.';
      return;
    }
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
        // Build the body explicitly: a stale default must never be sent.
        body: JSON.stringify({
          exposure: {
            subdomain: exposure.subdomain,
            tls: exposure.tls,
            auth: exposure.auth,
            localOnly: exposure.localOnly,
            internet: exposure.internet,
            service: exposure.service,
            port: exposure.port,
            scheme: exposure.scheme
          },
          baseDomain
        })
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

<Modal title="Exposure — {appName}" onClose={() => dispatch('close')}>
  <AsyncState
    {loading}
    error={loadError}
    loadingText="Loading exposure..."
    onRetry={load}
  >
    {#if loaded}
      <form id="app-exposure-form" on:submit|preventDefault={save} class="space-y-5">
        <ExposureForm
          bind:exposure
          bind:baseDomain
          {selectableDomains}
          {ssoDomains}
          {services}
          on:change={onExposureChange}
        />
        {#if unsafeExposure}
          <div class="callout callout-bad mb-0">
            <p class="font-medium mb-1">This app will be exposed without full protection</p>
            <p>
              {#if !exposure.auth}Without Naslos login (auth is off).{/if}
              {#if !exposure.tls}Without TLS (traffic is cleartext).{/if}
              Anything that can reach <span class="font-mono">{exposure.subdomain}.{baseDomain}</span>
              can use its admin interface.
            </p>
          </div>
          <label class="flex items-start gap-3 cursor-pointer">
            <input type="checkbox" class="mt-1" bind:checked={unsafeAcknowledged} />
            <span class="font-medium text-red-300">
              I understand this app is reachable without {#if !exposure.auth}Naslos login{/if}{#if !exposure.auth && !exposure.tls} and {/if}{#if !exposure.tls}encryption{/if}.
            </span>
          </label>
        {/if}
        {#if error}
          <p role="alert" class="callout callout-bad mb-0">{error}</p>
        {/if}
      </form>
    {/if}
  </AsyncState>

  <svelte:fragment slot="footer">
    <Button variant="secondary" on:click={() => dispatch('close')}>Cancel</Button>
    <Button
      type="submit"
      form="app-exposure-form"
      loading={saving}
      disabled={loading || !loaded || (unsafeExposure && !unsafeAcknowledged)}
    >
      {saving ? 'Saving...' : 'Save'}
    </Button>
  </svelte:fragment>
</Modal>
