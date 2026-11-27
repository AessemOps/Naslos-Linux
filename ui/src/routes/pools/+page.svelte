<script lang="ts">
  import { onMount } from 'svelte';
  import { api, ApiError } from '$lib/api';
  import AsyncState from '$lib/components/AsyncState.svelte';
  import Button from '$lib/components/Button.svelte';
  import EmptyState from '$lib/components/EmptyState.svelte';
  import Icon from '$lib/components/Icon.svelte';
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
    loading = true;
    error = '';
    try {
      pools = await api<Pool[]>('/api/volumes/zfs');
    } catch (e) {
      error =
        e instanceof ApiError
          ? `Failed to load pools (HTTP ${e.status})`
          : 'Failed to connect to the API';
    } finally {
      loading = false;
    }
  }

  async function loadImportable() {
    importLoading = true;
    importError = '';
    importablePools = [];
    try {
      importablePools = await api<ImportablePool[]>('/api/volumes/zfs/import');
    } catch (e) {
      importError =
        e instanceof ApiError
          ? `Failed to list importable pools (HTTP ${e.status})`
          : 'Failed to connect to the API';
    } finally {
      importLoading = false;
    }
  }

  function openImportModal() {
    showImportModal = true;
    void loadImportable();
  }

  async function importPool(name: string) {
    importingName = name;
    importError = '';
    try {
      await api('/api/volumes/zfs/import', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name })
      });
      showImportModal = false;
      await loadPools();
    } catch (e) {
      importError = e instanceof ApiError ? `Import failed: ${e.message}` : 'Failed to connect to the API';
    } finally {
      importingName = '';
    }
  }

  function topologyLabel(t: string): string {
    switch (t) {
      case 'mirror':
        return 'Mirror';
      case 'raidz1':
        return 'RAIDZ1';
      case 'raidz2':
        return 'RAIDZ2';
      case 'raidz3':
        return 'RAIDZ3';
      case 'single':
        return 'Single';
      default:
        return t || 'Unknown';
    }
  }

  onMount(loadPools);
</script>

<div class="mx-auto max-w-6xl">
  <PageHeader title="ZFS Pools" subtitle="Monitor and manage your ZFS pools.">
    <div slot="actions">
      <Button variant="secondary" on:click={openImportModal}>Import Pool</Button>
    </div>
  </PageHeader>

  <AsyncState
    {loading}
    error={error}
    loadingText="Loading pools..."
    onRetry={loadPools}
  >
    {#if pools.length === 0}
      <EmptyState
        icon="database"
        title="No ZFS pools configured yet."
        detail="Create a pool from the disks connected to this node, or import a pool that already exists on a disk."
      >
        <svelte:fragment slot="action">
          <Button variant="secondary" on:click={openImportModal}>Import Existing Pool</Button>
          <Button href="/disks">Create New Pool</Button>
        </svelte:fragment>
      </EmptyState>
    {:else}
      <div class="grid grid-cols-1 gap-4 md:grid-cols-2">
        {#each pools as pool}
          <a href="/pools/{pool.name}" class="card transition-colors hover:border-naslos-accent">
            <div class="mb-3 flex items-center justify-between gap-2">
              <span class="text-lg font-bold">{pool.name}</span>
              <StatusBadge state={pool.health} unknownTone="bad" />
            </div>
            <div class="mb-2 text-sm text-gray-400">
              <span class="font-mono">{pool.size}</span> total ·
              <span class="font-mono">{pool.free}</span> free
            </div>
            {#if pool.disks && pool.disks.length > 0}
              <div class="text-xs text-gray-500">
                {pool.disks.length} disk{pool.disks.length > 1 ? 's' : ''}: {pool.disks.join(', ')}
              </div>
            {/if}
            <div class="mt-3 flex items-center gap-1 text-xs text-naslos-link">
              View health details
              <Icon name="chevron-right" size={14} />
            </div>
          </a>
        {/each}
      </div>
    {/if}
  </AsyncState>
</div>

{#if showImportModal}
  <Modal
    title="Import Existing Pool"
    size="lg"
    onClose={() => {
      showImportModal = false;
      importingName = '';
    }}
  >
    <p class="text-sm text-gray-400">
      Pools below were found on connected disks but are not currently imported.
    </p>

    <AsyncState
      loading={importLoading}
      error={importError}
      empty={importablePools.length === 0}
      loadingText="Scanning for importable pools..."
      emptyTitle="No importable pools found."
      emptyDetail="Pools that are already imported do not appear here."
      emptyIcon="database"
      onRetry={loadImportable}
    >
      <div class="space-y-3">
        {#each importablePools as ip}
          <div class="rounded-lg border border-naslos-border p-4">
            <div class="mb-2 flex flex-wrap items-center justify-between gap-2">
              <span class="font-bold">{ip.name}</span>
              <Button
                size="sm"
                loading={importingName === ip.name}
                disabled={importingName !== ''}
                on:click={() => importPool(ip.name)}
              >
                Import
              </Button>
            </div>
            <div class="flex flex-wrap items-center gap-2 text-sm text-gray-400">
              <span class="badge badge-neutral">{topologyLabel(ip.topology)}</span>
              <span>{ip.state}</span>
            </div>
            {#if ip.disks.length > 0}
              <div class="mt-2 text-xs text-gray-500">
                {ip.disks.length} disk{ip.disks.length > 1 ? 's' : ''}: {ip.disks.join(', ')}
              </div>
            {/if}
          </div>
        {/each}
      </div>
    </AsyncState>
  </Modal>
{/if}
