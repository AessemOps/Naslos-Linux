<script lang="ts">
  interface PoolStatus {
    name: string;
    size: string;
    alloc: string;
    free: string;
    health: string;
  }

  let pools: PoolStatus[] = [];
  let loading = true;

  async function loadPools() {
    try {
      const res = await fetch('/api/volumes/zfs');
      if (res.ok) {
        pools = await res.json();
      }
    } catch (e) {
      // API may not be available yet
    } finally {
      loading = false;
    }
  }

  loadPools();
</script>

<div>
  <h1 class="text-3xl font-bold mb-2">Dashboard</h1>
  <p class="text-gray-400 mb-8">Overview of your NasOS system.</p>

  <!-- Quick stats -->
  <div class="grid grid-cols-1 md:grid-cols-4 gap-4 mb-8">
    <div class="card">
      <div class="text-sm text-gray-400 mb-1">Pools</div>
      <div class="text-3xl font-bold text-nasos-primary">{pools.length}</div>
    </div>
    <div class="card">
      <div class="text-sm text-gray-400 mb-1">Total Capacity</div>
      <div class="text-3xl font-bold text-green-400">
        {pools.length > 0 ? pools[0].size : '—'}
      </div>
    </div>
    <div class="card">
      <div class="text-sm text-gray-400 mb-1">Health</div>
      <div class="text-3xl font-bold">
        {#if pools.length > 0}
          <span class="text-green-400">✓ ONLINE</span>
        {:else}
          <span class="text-gray-500">—</span>
        {/if}
      </div>
    </div>
    <div class="card">
      <div class="text-sm text-gray-400 mb-1">Apps Running</div>
      <div class="text-3xl font-bold text-blue-400">—</div>
    </div>
  </div>

  <!-- Pool list -->
  <div class="card">
    <h2 class="text-xl font-bold mb-4">ZFS Pools</h2>
    {#if loading}
      <p class="text-gray-400">Loading pools...</p>
    {:else if pools.length === 0}
      <div class="text-center py-12">
        <p class="text-gray-400 mb-4">No ZFS pools configured yet.</p>
        <a href="/disks" class="btn btn-primary">Create Your First Pool</a>
      </div>
    {:else}
      <div class="overflow-x-auto">
        <table class="w-full">
          <thead>
            <tr class="text-left text-gray-400 text-sm border-b border-nasos-border">
              <th class="pb-3 font-medium">Pool</th>
              <th class="pb-3 font-medium">Size</th>
              <th class="pb-3 font-medium">Used</th>
              <th class="pb-3 font-medium">Free</th>
              <th class="pb-3 font-medium">Health</th>
            </tr>
          </thead>
          <tbody>
            {#each pools as pool}
              <tr class="border-b border-nasos-border last:border-0">
                <td class="py-3 font-medium">{pool.name}</td>
                <td class="py-3">{pool.size}</td>
                <td class="py-3">{pool.alloc}</td>
                <td class="py-3">{pool.free}</td>
                <td class="py-3">
                  <span class="px-2 py-1 rounded text-xs"
                    class:bg-green-900/50={pool.health === 'ONLINE'}
                    class:text-green-400={pool.health === 'ONLINE'}
                    class:bg-red-900/50={pool.health !== 'ONLINE'}
                    class:text-red-400={pool.health !== 'ONLINE'}
                  >
                    {pool.health}
                  </span>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </div>
</div>
