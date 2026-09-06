<script lang="ts">
  import { onMount } from 'svelte';
  import AppInstallModal from '$lib/components/AppInstallModal.svelte';

  interface CatalogEntry {
    name: string;
    displayName: string;
    description: string;
    category: string;
    icon: string;
    version: string;
    tags: string[];
  }

  let apps: CatalogEntry[] = [];
  let loading = true;
  let selectedApp: string | null = null;
  let filterCategory = 'all';
  let searchQuery = '';

  const categories = [
    { id: 'all', name: 'All' },
    { id: 'media', name: 'Media' },
    { id: 'productivity', name: 'Productivity' },
    { id: 'smart-home', name: 'Smart Home' },
    { id: 'networking', name: 'Networking' },
    { id: 'development', name: 'Development' }
  ];

  async function loadCatalog() {
    try {
      const res = await fetch('/api/catalog');
      apps = await res.json();
    } catch (e) {
      console.error('Failed to load catalog:', e);
    } finally {
      loading = false;
    }
  }

  function openInstall(name: string) {
    selectedApp = name;
  }

  function closeInstall() {
    selectedApp = null;
  }

  onMount(loadCatalog);

  $: filteredApps = apps.filter(app => {
    const matchesCategory = filterCategory === 'all' || app.category === filterCategory;
    const matchesSearch = searchQuery === '' ||
      app.displayName.toLowerCase().includes(searchQuery.toLowerCase()) ||
      app.description.toLowerCase().includes(searchQuery.toLowerCase());
    return matchesCategory && matchesSearch;
  });
</script>

<div>
  <!-- Filters -->
  <div class="flex gap-4 mb-6">
    <input
      type="text"
      bind:value={searchQuery}
      placeholder="Search apps..."
      class="input flex-1"
    />
    <select bind:value={filterCategory} class="input w-48">
      {#each categories as cat}
        <option value={cat.id}>{cat.name}</option>
      {/each}
    </select>
  </div>

  <!-- App grid -->
  {#if loading}
    <p class="text-gray-400">Loading catalog...</p>
  {:else if filteredApps.length === 0}
    <p class="text-gray-400">No apps found.</p>
  {:else}
    <div class="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
      {#each filteredApps as app}
        <div class="card hover:border-nasos-accent transition-colors cursor-pointer" on:click={() => openInstall(app.name)}>
          <div class="flex items-start gap-4">
            <div class="text-4xl">{app.icon}</div>
            <div class="flex-1 min-w-0">
              <h3 class="font-bold text-lg truncate">{app.displayName}</h3>
              <p class="text-sm text-gray-400 line-clamp-2 mb-2">{app.description}</p>
              <div class="flex items-center gap-2">
                <span class="text-xs px-2 py-0.5 rounded bg-nasos-border text-gray-300">{app.category}</span>
                <span class="text-xs text-gray-500">v{app.version}</span>
              </div>
            </div>
          </div>
        </div>
      {/each}
    </div>
  {/if}

  <!-- Install modal -->
  {#if selectedApp}
    <AppInstallModal appName={selectedApp} on:close={closeInstall} />
  {/if}
</div>
