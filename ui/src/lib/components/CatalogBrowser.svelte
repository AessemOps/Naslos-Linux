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
  let loadError = '';
  let selectedApp: string | null = null;
  let filterCategory = 'all';
  let searchQuery = '';

  // PF-H5: installing cannot work yet - no chart repository is wired into the
  // API and it has no RBAC to create a Helm release. Keep the catalog visible
  // for reference but disable the install action until the catalog refactor
  // lands. Flip this to true together with that refactor.
  const INSTALL_ENABLED = false;

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
    if (!INSTALL_ENABLED) {
      return;
    }
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
  {#if !INSTALL_ENABLED}
    <div class="card border-amber-700 bg-amber-900/30 text-amber-200 p-4 mb-6" role="status">
      App installation is not available yet: no chart repository is configured and
      the API cannot create Helm releases. The catalog is shown for reference only.
    </div>
  {/if}

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
  {:else if loadError}
    <div class="card border-red-700 bg-red-900/30 text-red-300 p-4">{loadError}</div>
  {:else if filteredApps.length === 0}
    <p class="text-gray-400">No apps found.</p>
  {:else}
    <div class="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
      {#each filteredApps as app}
        <div
          class="card {INSTALL_ENABLED ? 'hover:border-naslos-accent transition-colors cursor-pointer' : 'opacity-80'}"
          role="button"
          tabindex="0"
          aria-disabled={!INSTALL_ENABLED}
          aria-label={INSTALL_ENABLED ? `Install ${app.displayName}` : `${app.displayName} (installation disabled)`}
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
              <div class="flex items-center gap-2">
                <span class="text-xs px-2 py-0.5 rounded bg-naslos-border text-gray-300">{app.category}</span>
                <span class="text-xs text-gray-500">v{app.version}</span>
              </div>
            </div>
          </div>
        </div>
      {/each}
    </div>
  {/if}

  <!-- Install modal -->
  {#if INSTALL_ENABLED && selectedApp}
    <AppInstallModal appName={selectedApp} on:close={closeInstall} />
  {/if}
</div>
