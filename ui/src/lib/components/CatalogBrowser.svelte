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
    source: string;
    channel: string;
    channels: string[];
    chartPath: string;
  }

  let apps: CatalogEntry[] = [];
  let loading = true;
  let loadError = '';
  let selectedApp: string | null = null;
  let filterCategory = 'all';
  let filterChannel = 'all';
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
    loadError = '';
    try {
      const res = await fetch('/api/catalog');
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}`);
      }
      const data = await res.json();
      if (!Array.isArray(data)) {
        throw new Error('unexpected catalog response');
      }
      apps = data;
    } catch (e) {
      loadError = 'Failed to load the catalog: ' + e;
      apps = [];
    } finally {
      loading = false;
    }
  }

  function openInstall(name: string) {
    selectedApp = name;
  }

  function closeInstall() {
    selectedApp = null;
    loadCatalog();
  }

  onMount(loadCatalog);

  $: channels = Array.from(new Set(apps.flatMap(app => app.channels || []))).sort();

  $: filteredApps = apps.filter(app => {
    const matchesCategory = filterCategory === 'all' || app.category === filterCategory;
    const matchesChannel = filterChannel === 'all' || (app.channels || []).includes(filterChannel);
    const q = searchQuery.toLowerCase();
    const matchesSearch = searchQuery === '' ||
      app.displayName.toLowerCase().includes(q) ||
      app.description.toLowerCase().includes(q);
    return matchesCategory && matchesChannel && matchesSearch;
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
    <select bind:value={filterChannel} class="input w-44" aria-label="Channel">
      <option value="all">All channels</option>
      {#each channels as channel}
        <option value={channel}>{channel}</option>
      {/each}
    </select>
    <select bind:value={filterCategory} class="input w-48" aria-label="Category">
      {#each categories as cat}
        <option value={cat.id}>{cat.name}</option>
      {/each}
    </select>
  </div>

  <!-- App grid -->
  {#if loading}
    <p class="text-gray-400">Loading catalog...</p>
  {:else if loadError}
    <div class="card border-red-700 bg-red-900/30 text-red-300 p-4">{loadError}</div>
  {:else if apps.length === 0}
    <div class="card text-center py-12" role="status">
      <div class="text-5xl mb-4">🗂️</div>
      <h2 class="text-xl font-bold mb-2">No Apps In The Catalog</h2>
      <p class="text-gray-400 mb-2">
        Add a chart repository in the Sources tab, then refresh it.
      </p>
    </div>
  {:else if filteredApps.length === 0}
    <p class="text-gray-400">No apps match these filters.</p>
  {:else}
    <div class="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
      {#each filteredApps as app}
        <div
          class="card hover:border-naslos-accent transition-colors cursor-pointer"
          role="button"
          tabindex="0"
          aria-label={`Install ${app.displayName}`}
          on:click={() => openInstall(app.name)}
          on:keydown={(e) => {
            if (e.key === 'Enter' || e.key === ' ') {
              e.preventDefault();
              openInstall(app.name);
            }
          }}
        >
          <div class="flex items-start gap-4">
            <div class="text-4xl">{app.icon}</div>
            <div class="flex-1 min-w-0">
              <h3 class="font-bold text-lg truncate">{app.displayName}</h3>
              <p class="text-sm text-gray-400 line-clamp-2 mb-2">{app.description}</p>
              <div class="flex flex-wrap items-center gap-2">
                <span class="text-xs px-2 py-0.5 rounded bg-naslos-border text-gray-300">{app.category}</span>
                <span class="text-xs px-2 py-0.5 rounded bg-blue-900/40 text-blue-300">{app.source}</span>
                {#each app.channels || [] as channel}
                  <span
                    class={`text-xs px-2 py-0.5 rounded ${channel === app.channel ? 'bg-green-900/40 text-green-300' : 'bg-naslos-border text-gray-400'}`}
                  >{channel}</span>
                {/each}
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
