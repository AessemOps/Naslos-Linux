<script lang="ts">
  import { onMount } from 'svelte';
  import AppIcon from '$lib/components/AppIcon.svelte';
  import AppInstallModal from '$lib/components/AppInstallModal.svelte';
  import AsyncState from '$lib/components/AsyncState.svelte';
  import Button from '$lib/components/Button.svelte';
  import Icon from '$lib/components/Icon.svelte';
  import StatusBadge from '$lib/components/StatusBadge.svelte';

  interface CatalogEntry {
    name: string;
    displayName: string;
    description: string;
    category: string;
    icon: string;
    iconUrl: string;
    version: string;
    tags: string[];
    source: string;
    channel: string;
    channels: string[];
    chartPath: string;
  }

  type ViewMode = 'grid' | 'list' | 'compact';
  const VIEW_STORAGE_KEY = 'naslos:catalog-view';

  let apps: CatalogEntry[] = [];
  let loading = true;
  let loadError = '';
  let selectedApp: string | null = null;
  let filterCategory = 'all';
  let filterChannel = 'all';
  let searchQuery = '';
  let viewMode: ViewMode = 'grid';

  const views = [
    { id: 'grid', label: 'Grid view', icon: 'layout-grid' },
    { id: 'list', label: 'List view', icon: 'list' },
    { id: 'compact', label: 'Compact view', icon: 'rows-3' }
  ] as const;

  const categories = [
    { id: 'all', name: 'All' },
    { id: 'media', name: 'Media' },
    { id: 'productivity', name: 'Productivity' },
    { id: 'smart-home', name: 'Smart Home' },
    { id: 'networking', name: 'Networking' },
    { id: 'development', name: 'Development' }
  ];

  async function loadCatalog() {
    loading = true;
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

  function setView(mode: ViewMode) {
    viewMode = mode;
    try {
      localStorage.setItem(VIEW_STORAGE_KEY, mode);
    } catch {}
  }

  onMount(() => {
    try {
      const stored = localStorage.getItem(VIEW_STORAGE_KEY);
      if (stored === 'grid' || stored === 'list' || stored === 'compact') {
        viewMode = stored;
      }
    } catch {}
    void loadCatalog();
  });

  $: channels = Array.from(new Set(apps.flatMap((app) => app.channels || []))).sort();

  $: filteredApps = apps.filter((app) => {
    const matchesCategory = filterCategory === 'all' || app.category === filterCategory;
    const matchesChannel = filterChannel === 'all' || (app.channels || []).includes(filterChannel);
    const q = searchQuery.toLowerCase();
    const matchesSearch =
      searchQuery === '' ||
      app.displayName.toLowerCase().includes(q) ||
      app.description.toLowerCase().includes(q);
    return matchesCategory && matchesChannel && matchesSearch;
  });
</script>

<div>
  <form class="mb-6 flex flex-wrap gap-4" role="search" on:submit|preventDefault={() => {}}>
    <div class="min-w-56 flex-1">
      <label class="sr-only" for="catalog-search">Search apps</label>
      <input
        id="catalog-search"
        type="search"
        bind:value={searchQuery}
        placeholder="Search apps..."
        class="input w-full"
      />
    </div>
    <div>
      <label class="sr-only" for="catalog-channel">Channel</label>
      <select id="catalog-channel" bind:value={filterChannel} class="input w-44">
        <option value="all">All channels</option>
        {#each channels as channel}
          <option value={channel}>{channel}</option>
        {/each}
      </select>
    </div>
    <div>
      <label class="sr-only" for="catalog-category">Category</label>
      <select id="catalog-category" bind:value={filterCategory} class="input w-48">
        {#each categories as cat}
          <option value={cat.id}>{cat.name}</option>
        {/each}
      </select>
    </div>
    <div class="flex items-center gap-1 rounded-lg bg-naslos-surface p-1" role="group" aria-label="Catalog view">
      {#each views as view}
        <Button
          variant={viewMode === view.id ? 'primary' : 'ghost'}
          size="icon"
          aria-label={view.label}
          title={view.label}
          aria-pressed={viewMode === view.id}
          on:click={() => setView(view.id)}
        >
          <Icon name={view.icon} size={18} />
        </Button>
      {/each}
    </div>
  </form>

  <AsyncState
    {loading}
    error={loadError}
    empty={!loading && apps.length === 0}
    loadingText="Loading catalog..."
    emptyTitle="No Apps In The Catalog"
    emptyDetail="Add a chart repository in the Sources tab, then refresh it."
    emptyIcon="folder-tree"
    onRetry={loadCatalog}
  >
    {#if filteredApps.length === 0}
      <p class="text-gray-400">No apps match these filters.</p>
    {:else if viewMode === 'grid'}
      <div class="grid grid-cols-1 gap-4 md:grid-cols-2 lg:grid-cols-3">
        {#each filteredApps as app}
          <div class="card relative transition-colors hover:border-naslos-accent">
            <!-- A stretched real button keeps the card keyboard-operable and its
                 accessible name exactly 'Install <app>' without tripping
                 label-content-name-mismatch (the visible card text stays outside
                 the button, so name and content cannot disagree). -->
            <button
              type="button"
              class="absolute inset-0 z-10 rounded-xl cursor-pointer"
              aria-label={`Install ${app.displayName}`}
              on:click={() => openInstall(app.name)}
            ></button>
            <div class="pointer-events-none flex items-start gap-4">
              <AppIcon icon={app.icon} iconUrl={app.iconUrl} alt={app.displayName} />
              <div class="flex-1 min-w-0">
                <h3 class="font-bold text-lg truncate">{app.displayName}</h3>
                <p class="text-sm text-gray-400 line-clamp-2 mb-2">{app.description}</p>
                <div class="flex flex-wrap items-center gap-2">
                  <StatusBadge state={app.category} label={app.category} tone="neutral" />
                  <StatusBadge state={app.source} label={app.source} tone="info" />
                  {#each app.channels || [] as channel}
                    <StatusBadge
                      state={channel}
                      label={channel}
                      tone={channel === app.channel ? 'ok' : 'neutral'}
                    />
                  {/each}
                  <span class="text-xs text-gray-500">v{app.version}</span>
                </div>
              </div>
            </div>
          </div>
        {/each}
      </div>
    {:else if viewMode === 'list'}
      <ul class="card p-0 divide-y divide-naslos-border overflow-hidden">
        {#each filteredApps as app}
          <li class="relative">
            <button
              type="button"
              class="absolute inset-0 z-10 rounded-xl cursor-pointer"
              aria-label={`Install ${app.displayName}`}
              on:click={() => openInstall(app.name)}
            ></button>
            <div class="pointer-events-none flex items-center gap-4 p-4">
              <AppIcon icon={app.icon} iconUrl={app.iconUrl} alt={app.displayName} />
              <div class="flex-1 min-w-0">
                <h3 class="font-bold truncate">{app.displayName}</h3>
                <p class="text-sm text-gray-400 line-clamp-2">{app.description}</p>
              </div>
              <div class="hidden md:flex flex-wrap items-center justify-end gap-2">
                <StatusBadge state={app.category} label={app.category} tone="neutral" />
                <StatusBadge state={app.source} label={app.source} tone="info" />
                {#each app.channels || [] as channel}
                  <StatusBadge
                    state={channel}
                    label={channel}
                    tone={channel === app.channel ? 'ok' : 'neutral'}
                  />
                {/each}
                <span class="text-xs text-gray-500">v{app.version}</span>
              </div>
              <span class="text-gray-500">
                <Icon name="chevron-right" size={18} />
              </span>
            </div>
          </li>
        {/each}
      </ul>
    {:else}
      <ul class="card p-0 divide-y divide-naslos-border overflow-hidden">
        {#each filteredApps as app}
          <li class="relative">
            <button
              type="button"
              class="absolute inset-0 z-10 rounded-xl cursor-pointer"
              aria-label={`Install ${app.displayName}`}
              on:click={() => openInstall(app.name)}
            ></button>
            <div class="pointer-events-none flex items-center gap-3 px-3 py-2">
              <AppIcon
                icon={app.icon}
                iconUrl={app.iconUrl}
                alt={app.displayName}
                size="text-xl"
                imgClass="h-5 w-5"
              />
              <span class="font-medium truncate">{app.displayName}</span>
              <span class="text-xs text-gray-500">v{app.version}</span>
              <StatusBadge state={app.category} label={app.category} tone="neutral" />
              <span class="ml-auto text-xs text-gray-400 truncate">{app.source} · {app.channel}</span>
            </div>
          </li>
        {/each}
      </ul>
    {/if}
  </AsyncState>

  <!-- Install modal -->
  {#if selectedApp}
    <AppInstallModal appName={selectedApp} on:close={closeInstall} />
  {/if}
</div>
