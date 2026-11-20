<script lang="ts">
  import { onMount } from 'svelte';
  import Icon from '$lib/components/Icon.svelte';
  import LoadingState from '$lib/components/LoadingState.svelte';
  import Modal from '$lib/components/Modal.svelte';
  import PageHeader from '$lib/components/PageHeader.svelte';
  import StatusBadge from '$lib/components/StatusBadge.svelte';

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
  <PageHeader title="ZFS Pools" subtitle="Monitor and manage your ZFS pools.">
    <button slot="actions" class="btn btn-secondary" on:click={openImportModal}>
      Import Pool
    </button>
  </PageHeader>

  {#if loading}
    <LoadingState label="Loading pools..." />
  {:else if error}
    <p role="alert" class="text-red-400">{error}</p>
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
            <StatusBadge state={pool.health} />
          </div>
          <div class="text-sm text-gray-400 mb-2">
            {pool.size} total · {pool.free} free
          </div>
          {#if pool.disks && pool.disks.length > 0}
            <div class="text-xs text-gray-500">
              {pool.disks.length} disk{pool.disks.length > 1 ? 's' : ''}: {pool.disks.join(', ')}
            </div>
          {/if}
          <div class="mt-3 text-xs text-naslos-primary flex items-center gap-1">
            View health details
            <Icon name="chevron-right" size={14} />
          </div>
        </a>
      {/each}
    </div>
  {/if}
</div>

<!-- Import Modal -->
{#if showImportModal}
  <Modal title="Import Existing Pool" onClose={() => (showImportModal = false)}>
    <p class="text-gray-400 text-sm">Pools below were found on connected disks but are not currently imported.</p>

    {#if importLoading}
      <LoadingState label="Scanning for importable pools..." />
    {:else if importError}
      <p role="alert" class="text-red-400">{importError}</p>
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
            <div class="text-sm text-gray-400 flex items-center gap-2">
              <span class="badge badge-neutral">{topologyLabel(ip.topology)}</span>
              <span>{ip.state}</span>
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
  </Modal>
{/if}
