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

  let pools: Pool[] = [];
  let loading = true;
  let error = '';

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

  onMount(loadPools);
</script>

<div>
  <h1 class="text-3xl font-bold mb-2">ZFS Pools</h1>
  <p class="text-gray-400 mb-8">Monitor and manage your ZFS pools.</p>

  {#if loading}
    <p class="text-gray-400">Loading pools...</p>
  {:else if error}
    <p class="text-red-400">{error}</p>
  {:else if pools.length === 0}
    <div class="card text-center py-12">
      <p class="text-gray-400 mb-4">No ZFS pools configured yet.</p>
      <a href="/disks" class="btn btn-primary">Create Your First Pool</a>
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