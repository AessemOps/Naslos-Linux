<script lang="ts">
  import { onMount } from 'svelte';
  import CatalogBrowser from '$lib/components/CatalogBrowser.svelte';
  import InstalledApps from '$lib/components/InstalledApps.svelte';

  let activeTab: 'catalog' | 'installed' = 'catalog';
</script>

<div class="max-w-6xl mx-auto">
  <h1 class="text-3xl font-bold mb-2">Apps</h1>
  <p class="text-gray-400 mb-6">Deploy and manage applications from the catalog.</p>

  <!-- Tabs -->
  <div class="flex gap-1 mb-6 bg-nasos-surface rounded-lg p-1 w-fit">
    <button
      class="px-4 py-2 rounded-md text-sm font-medium transition-colors"
      class:bg-nasos-primary={activeTab === 'catalog'}
      class:text-white={activeTab === 'catalog'}
      class:text-gray-400={activeTab !== 'catalog'}
      on:click={() => activeTab = 'catalog'}
    >
      Catalog
    </button>
    <button
      class="px-4 py-2 rounded-md text-sm font-medium transition-colors"
      class:bg-nasos-primary={activeTab === 'installed'}
      class:text-white={activeTab === 'installed'}
      class:text-gray-400={activeTab !== 'installed'}
      on:click={() => activeTab = 'installed'}
    >
      Installed
    </button>
  </div>

  {#if activeTab === 'catalog'}
    <CatalogBrowser />
  {:else}
    <InstalledApps />
  {/if}
</div>
