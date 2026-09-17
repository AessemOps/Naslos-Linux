<script lang="ts">
  import { onMount } from 'svelte';

  interface Pool {
    name: string;
    size: string;
    alloc: string;
    free: string;
    health: string;
    disks: string[] | null;
  }

  interface ImportablePool {
    name: string;
    state: string;
    topology: string;
    disks: string[];
  }

  let pools: Pool[] = [];
  let loading = true;
  let error = '';

  let showImportModal = false;
  let importablePools: ImportablePool[] = [];
  let importLoading = false;
  let importError = '';
  let importingName = '';

  async function loadPools() {
    try {
      const res = await fetch('/api/volumes/zfs');
      if (res.ok) {
        pools = await res.json();
      } else {
        error = `Failed to load pools (HTTP ${res.status})`;
      }
    } catch (e) {
      error = 'Failed to connect to API';
    } finally {
      loading = false;
    }
  }

  async function openImportModal() {
    showImportModal = true;
    importLoading = true;
    importError = '';
    importablePools = [];
    try {
      const res = await fetch('/api/volumes/zfs/import');
      if (res.ok) {
        importablePools = await res.json();
      } else {
        importError = `Failed to list importable pools (HTTP ${res.status})`;
      }
    } catch (e) {
      importError = 'Failed to connect to API';
    } finally {
      importLoading = false;
    }
  }

  async function importPool(name: string) {
    importingName = name;
    importError = '';
    try {
      const res = await fetch('/api/volumes/zfs/import', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name })
      });
      if (res.ok) {
        showImportModal = false;
        loadPools();
      } else {
        const text = await res.text();
        importError = `Import failed: ${text.slice(0, 200)}`;
      }
    } catch (e) {
      importError = 'Failed to connect to API';
    } finally {
      importingName = '';
    }
  }

  function topologyLabel(t: string): string {
    switch (t) {
      case 'mirror': return 'Mirror';
      case 'raidz1': return 'RAIDZ1';
      case 'raidz2': return 'RAIDZ2';
      case 'raidz3': return 'RAIDZ3';
      case 'single': return 'Single';
      default: return t || 'Unknown';
    }
  }

  onMount(loadPools);
</script>

<div>
  <div class="flex items-center justify-between mb-8">
    <div>
      <h1 class="text-3xl font-bold mb-2">ZFS Pools</h1>
      <p class="text-gray-400">Monitor and manage your ZFS pools.</p>
    </div>
    <button class="btn btn-secondary" on:click={openImportModal}>
      Import Pool
    </button>
  </div>

  {#if loading}
    <p class="text-gray-400">Loading pools...</p>
  {:else if error}
    <p class="text-red-400">{error}</p>
  {:else if pools.length === 0}
    <div class="card text-center py-12">
      <p class="text-gray-400 mb-4">No ZFS pools configured yet.</p>
      <div class="flex justify-center gap-3">
        <button class="btn btn-secondary" on:click={openImportModal}>Import Existing Pool</button>
        <a href="/disks" class="btn btn-primary">Create New Pool</a>
      </div>
    </div>
  {:else}
    <div class="grid grid-cols-1 md:grid-cols-2 gap-4">
      {#each pools as pool}
        <a href="/pools/{pool.name}" class="card hover:border-naslos-accent transition-colors">
          <div class="flex items-center justify-between mb-3">
            <span class="font-bold text-lg">{pool.name}</span>
            <span class="text-xs px-2 py-0.5 rounded {pool.health === 'ONLINE' ? 'bg-green-900/50 text-green-400' : pool.health === 'DEGRADED' ? 'bg-yellow-900/50 text-yellow-400' : 'bg-red-900/50 text-red-400'}">{pool.health}</span>
          </div>
          <div class="text-sm text-gray-400 mb-2">
            {pool.size} total · {pool.free} free
          </div>
          {#if pool.disks && pool.disks.length > 0}
            <div class="text-xs text-gray-500">
              {pool.disks.length} disk{pool.disks.length > 1 ? 's' : ''}: {pool.disks.join(', ')}
            </div>
          {/if}
          <div class="mt-3 text-xs text-naslos-primary">View health details →</div>
        </a>
      {/each}
    </div>
  {/if}
</div>

<!-- Import Modal -->
{#if showImportModal}
  <div class="fixed inset-0 bg-black/60 flex items-center justify-center z-50" role="presentation" on:click|self={() => showImportModal = false}>
    <div class="bg-naslos-card border border-naslos-border rounded-xl p-6 w-full max-w-lg max-h-[80vh] overflow-y-auto">
      <div class="flex items-center justify-between mb-4">
        <h2 class="text-xl font-bold">Import Existing Pool</h2>
        <button class="text-gray-400 hover:text-white text-2xl leading-none" on:click={() => showImportModal = false}>&times;</button>
      </div>
      <p class="text-gray-400 mb-4 text-sm">Pools below were found on connected disks but are not currently imported.</p>

      {#if importLoading}
        <p class="text-gray-400">Scanning for importable pools...</p>
      {:else if importError}
        <p class="text-red-400">{importError}</p>
      {:else if importablePools.length === 0}
        <div class="text-center py-8">
          <p class="text-gray-400">No importable pools found.</p>
          <p class="text-gray-500 text-sm mt-2">Pools that are already imported do not appear here.</p>
        </div>
      {:else}
        <div class="space-y-3">
          {#each importablePools as ip}
            <div class="border border-naslos-border rounded-lg p-4">
              <div class="flex items-center justify-between mb-2">
                <span class="font-bold">{ip.name}</span>
                <button
                  class="btn btn-primary btn-sm"
                  disabled={importingName !== ''}
                  on:click={() => importPool(ip.name)}
                >
                  {#if importingName === ip.name}
                    Importing…
                  {:else}
                    Import
                  {/if}
                </button>
              </div>
              <div class="text-sm text-gray-400">
                <span class="px-2 py-0.5 rounded bg-naslos-border text-xs">{topologyLabel(ip.topology)}</span>
                <span class="ml-2">{ip.state}</span>
              </div>
              {#if ip.disks.length > 0}
                <div class="text-xs text-gray-500 mt-2">
                  {ip.disks.length} disk{ip.disks.length > 1 ? 's' : ''}: {ip.disks.join(', ')}
                </div>
              {/if}
            </div>
          {/each}
        </div>
      {/if}
    </div>
  </div>
{/if}