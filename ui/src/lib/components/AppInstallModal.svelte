<script lang="ts">
  import { onMount, createEventDispatcher } from 'svelte';
  import SchemaForm from './SchemaForm.svelte';

  interface CatalogApp {
    name: string;
    displayName: string;
    description: string;
    icon: string;
    version: string;
    chart: string;
    schema: { properties: Record<string, any>; required?: string[] };
    defaultValues: Record<string, any>;
  }

  export let appName: string;

  let app: CatalogApp | null = null;
  let loading = true;
  let installing = false;
  let error = '';
  let values: Record<string, any> = {};
  let activeStep: 'config' | 'review' = 'config';

  const dispatch = createEventDispatcher();

  async function loadApp() {
    try {
      const res = await fetch(`/api/catalog/${appName}`);
      app = await res.json();
      if (app) values = { ...app.defaultValues };
    } catch (e) {
      error = 'Failed to load app details';
    } finally {
      loading = false;
    }
  }

  async function install() {
    installing = true;
    error = '';
    try {
      const res = await fetch('/api/apps', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name: appName, values })
      });
      if (!res.ok) {
        const data = await res.json();
        throw new Error(data.error || 'Install failed');
      }
      dispatch('close');
    } catch (e: any) {
      error = e.message;
    } finally {
      installing = false;
    }
  }

  $: valueEntries = Object.entries(values).filter(([_, v]) => typeof v !== 'object');
  onMount(loadApp);
</script>

<div class="fixed inset-0 bg-black/60 z-50 flex items-center justify-center p-4" role="presentation" on:click|self={() => dispatch('close')}>
  <div class="bg-naslos-surface rounded-2xl border border-naslos-border w-full max-w-2xl max-h-[90vh] overflow-hidden flex flex-col">
    {#if loading}
      <div class="p-12 text-center text-gray-400">Loading app...</div>
    {:else if app}
      <div class="p-6 border-b border-naslos-border flex items-center gap-4">
        <div class="text-4xl">{app.icon}</div>
        <div class="flex-1">
          <h2 class="text-xl font-bold">{app.displayName}</h2>
          <p class="text-sm text-gray-400">{app.description}</p>
        </div>
        <button class="text-gray-400 hover:text-white text-2xl" on:click={() => dispatch('close')}>×</button>
      </div>
      <div class="px-6 pt-4 flex gap-4">
        <button
          class={`text-sm font-medium pb-2 border-b-2 ${activeStep === 'config' ? 'border-naslos-primary text-naslos-primary' : 'border-transparent text-gray-400'}`}
          on:click={() => activeStep = 'config'}
        >Configuration</button>
        <button
          class={`text-sm font-medium pb-2 border-b-2 ${activeStep === 'review' ? 'border-naslos-primary text-naslos-primary' : 'border-transparent text-gray-400'}`}
          on:click={() => activeStep = 'review'}
        >Review</button>
      </div>
      <div class="flex-1 overflow-y-auto p-6">
        {#if activeStep === 'config'}
          <SchemaForm {app} bind:values />
        {:else}
          <div class="space-y-3">
            <div class="flex justify-between py-2 border-b border-naslos-border"><span class="text-gray-400">App</span><span class="font-medium">{app.displayName}</span></div>
            <div class="flex justify-between py-2 border-b border-naslos-border"><span class="text-gray-400">Chart</span><span class="font-medium text-sm">{app.chart}</span></div>
            <div class="flex justify-between py-2 border-b border-naslos-border"><span class="text-gray-400">Version</span><span class="font-medium">{app.version}</span></div>
            {#each valueEntries as [key, value]}<div class="flex justify-between py-2 border-b border-naslos-border"><span class="text-gray-400">{key}</span><span class="font-medium text-sm">{value || '(empty)'}</span></div>{/each}
          </div>
        {/if}
        {#if error}<div class="mt-4 p-3 rounded-lg bg-red-900/30 border border-red-700 text-red-300 text-sm">{error}</div>{/if}
      </div>
      <div class="p-6 border-t border-naslos-border flex justify-between">
        <button class="btn btn-secondary" on:click={() => dispatch('close')}>Cancel</button>
        <div class="flex gap-3">
          {#if activeStep === 'config'}<button class="btn btn-primary" on:click={() => activeStep = 'review'}>Review →</button>
          {:else}<button class="btn btn-secondary" on:click={() => activeStep = 'config'}>← Back</button><button class="btn btn-primary" on:click={install} disabled={installing}>{installing ? 'Installing...' : 'Install'}</button>{/if}
        </div>
      </div>
    {/if}
  </div>
</div>
