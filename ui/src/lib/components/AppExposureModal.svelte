<script lang="ts">
  import { onMount, createEventDispatcher } from 'svelte';
  import ExposureForm from './ExposureForm.svelte';

  interface Exposure {
    subdomain: string;
    tls: boolean;
    auth: boolean;
    localOnly: boolean;
    service?: string;
    port?: number;
    scheme?: string;
  }

  export let appName: string;

  let exposure: Exposure = { subdomain: '', tls: true, auth: true, localOnly: false };
  let baseDomain = '';
  let authAllowed = true;
  let loading = true;
  let saving = false;
  let error = '';

  const dispatch = createEventDispatcher();

  async function load() {
    try {
      const res = await fetch(`/api/apps/${appName}/exposure`);
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}`);
      }
      const data = await res.json();
      exposure = data.exposure;
      baseDomain = data.baseDomain || '';
      authAllowed = data.authAllowed ?? true;
    } catch (e) {
      error = 'Failed to load exposure: ' + e;
    } finally {
      loading = false;
    }
  }

  async function save() {
    saving = true;
    error = '';
    try {
      const res = await fetch(`/api/apps/${appName}/exposure`, {
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

<div class="fixed inset-0 bg-black/60 z-50 flex items-center justify-center p-4" role="presentation" on:click|self={() => dispatch('close')}>
  <div class="bg-naslos-surface rounded-2xl border border-naslos-border w-full max-w-lg max-h-[90vh] overflow-hidden flex flex-col">
    <div class="p-6 border-b border-naslos-border flex items-center justify-between">
      <h2 class="text-xl font-bold">Exposure — {appName}</h2>
      <button class="text-gray-400 hover:text-white text-2xl" aria-label="Close" on:click={() => dispatch('close')}>×</button>
    </div>
    <div class="flex-1 overflow-y-auto p-6">
      {#if loading}
        <p class="text-gray-400">Loading exposure...</p>
      {:else}
        <ExposureForm bind:exposure {baseDomain} {authAllowed} />
      {/if}
      {#if error}<div class="mt-4 p-3 rounded-lg bg-red-900/30 border border-red-700 text-red-300 text-sm">{error}</div>{/if}
    </div>
    <div class="p-6 border-t border-naslos-border flex justify-end gap-3">
      <button class="btn btn-secondary" on:click={() => dispatch('close')}>Cancel</button>
      <button class="btn btn-primary" on:click={save} disabled={saving || loading}>{saving ? 'Saving...' : 'Save'}</button>
    </div>
  </div>
</div>
