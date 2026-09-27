<script lang="ts">
  import { onMount, createEventDispatcher } from 'svelte';
  import SchemaForm from './SchemaForm.svelte';
  import { trackAppJob } from '$lib/stores/appJobs';

  export let appName: string;

  let app: { schema: { properties: Record<string, any>; required?: string[] } } | null = null;
  let values: Record<string, any> = {};
  let loading = true;
  let saving = false;
  let error = '';

  const dispatch = createEventDispatcher();

  async function load() {
    try {
      const [catalogRes, installedRes] = await Promise.all([
        fetch(`/api/catalog/${appName}`),
        fetch(`/api/apps/${appName}`)
      ]);
      if (!catalogRes.ok) {
        throw new Error(`catalog HTTP ${catalogRes.status}`);
      }
      app = await catalogRes.json();
      if (installedRes.ok) {
        const installed = await installedRes.json();
        values = { ...(installed.values || {}) };
      }
    } catch (e) {
      error = 'Failed to load app configuration: ' + e;
    } finally {
      loading = false;
    }
  }

  async function save() {
    saving = true;
    error = '';
    try {
      const res = await fetch(`/api/apps/${appName}`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ values })
      });
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        throw new Error(data.error || `Update failed (HTTP ${res.status})`);
      }
      // The upgrade runs as a background job: close and let the drawer follow it.
      const data = await res.json();
      if (data.jobId) trackAppJob(data.jobId);
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
      <h2 class="text-xl font-bold">Configure — {appName}</h2>
      <button class="text-gray-400 hover:text-white text-2xl" aria-label="Close" on:click={() => dispatch('close')}>×</button>
    </div>
    <div class="flex-1 overflow-y-auto p-6">
      {#if loading}
        <p class="text-gray-400">Loading configuration...</p>
      {:else if app}
        <SchemaForm {app} bind:values />
      {/if}
      {#if error}<div class="mt-4 p-3 rounded-lg bg-red-900/30 border border-red-700 text-red-300 text-sm">{error}</div>{/if}
    </div>
    <div class="p-6 border-t border-naslos-border flex justify-end gap-3">
      <button class="btn btn-secondary" on:click={() => dispatch('close')}>Cancel</button>
      <button class="btn btn-primary" on:click={save} disabled={saving || loading}>{saving ? 'Saving...' : 'Save'}</button>
    </div>
  </div>
</div>
