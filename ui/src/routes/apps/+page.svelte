<script lang="ts">
  import CatalogBrowser from '$lib/components/CatalogBrowser.svelte';
  import InstalledApps from '$lib/components/InstalledApps.svelte';
  import SourceList from '$lib/components/SourceList.svelte';

  let activeTab: 'catalog' | 'installed' | 'sources' = 'catalog';

  const tabs = [
    { id: 'catalog', label: 'Catalog' },
    { id: 'installed', label: 'Installed' },
    { id: 'sources', label: 'Sources' }
  ] as const;
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
