<script lang="ts">
  import { onMount } from 'svelte';
  import CatalogBrowser from '$lib/components/CatalogBrowser.svelte';
  import InstalledApps from '$lib/components/InstalledApps.svelte';
  import PageHeader from '$lib/components/PageHeader.svelte';
  import SourceList from '$lib/components/SourceList.svelte';
  import { appUpdates, startAppUpdates } from '$lib/stores/appUpdates';

  let activeTab: 'catalog' | 'installed' | 'sources' = 'installed';

  const tabs = [
    { id: 'installed', label: 'Installed' },
    { id: 'catalog', label: 'Catalog' },
    { id: 'sources', label: 'Sources' }
  ] as const;

  onMount(() => {
    const stop = startAppUpdates();
    return stop;
  });
</script>

<div class="mx-auto max-w-6xl">
  <PageHeader
    title="Apps"
    subtitle="Deploy and manage applications from chart repositories."
  />

  <div class="mb-6 flex w-fit flex-wrap gap-1 rounded-lg bg-naslos-surface p-1" role="group" aria-label="Apps sections">
    {#each tabs as tab}
      <button
        class="px-4 py-2 rounded-md text-sm font-medium transition-colors"
        class:bg-naslos-primary={activeTab === tab.id}
        class:text-white={activeTab === tab.id}
        class:text-gray-500={activeTab !== tab.id}
        aria-pressed={activeTab === tab.id}
        on:click={() => (activeTab = tab.id)}
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
