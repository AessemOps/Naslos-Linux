<script lang="ts">
  import { onMount } from 'svelte';
  import CatalogBrowser from '$lib/components/CatalogBrowser.svelte';
  import InstalledApps from '$lib/components/InstalledApps.svelte';
  import SourceList from '$lib/components/SourceList.svelte';
  import { appUpdates, startAppUpdates } from '$lib/stores/appUpdates';

  let activeTab: 'catalog' | 'installed' | 'sources' = 'catalog';

  const tabs = [
    { id: 'catalog', label: 'Catalog' },
    { id: 'installed', label: 'Installed' },
    { id: 'sources', label: 'Sources' }
  ] as const;

  onMount(() => {
    const stop = startAppUpdates();
    return stop;
  });
</script>

<div class="max-w-6xl mx-auto">
  <h1 class="text-3xl font-bold mb-2">Apps</h1>
  <p class="text-gray-400 mb-6">Deploy and manage applications from chart repositories.</p>

  <!-- Tabs -->
  <div class="flex gap-1 mb-6 bg-naslos-surface rounded-lg p-1 w-fit">
    {#each tabs as tab}
      <button
        class="px-4 py-2 rounded-md text-sm font-medium transition-colors"
        class:bg-naslos-primary={activeTab === tab.id}
        class:text-white={activeTab === tab.id}
        class:text-gray-400={activeTab !== tab.id}
        on:click={() => activeTab = tab.id}
      >
        {tab.label}
        {#if tab.id === 'installed' && $appUpdates.count > 0}
          <span
            class="ml-1.5 inline-flex items-center justify-center rounded-full bg-naslos-accent text-white text-[10px] leading-none px-1.5 py-0.5"
            aria-hidden="true">{$appUpdates.count}</span
          >
        {/if}
      </button>
    {/each}
  </div>

  {#if activeTab === 'catalog'}
    <CatalogBrowser />
  {:else if activeTab === 'installed'}
    <InstalledApps />
  {:else}
    <SourceList />
  {/if}
</div>
