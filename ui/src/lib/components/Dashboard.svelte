<script lang="ts">
  import { onDestroy } from 'svelte';

  interface DashboardData {
    cpu?: { usage: number; cores: number };
    memory?: { usage: number; total: number; used: number; available: number };
    disk?: { usage: number; total: number; used: number; free: number };
    zfs?: { poolCount: number; pools: any[] };
    system?: { hostname: string; uptime: number; os: string };
    updatedAt?: string;
  }

  let data: DashboardData = {};
  let loading = true;
  let error = '';

  // Auto-refresh: poll the dashboard API every 5 seconds. A request that is
  // still in flight when the next tick fires is skipped (inFlight guard).
  const REFRESH_INTERVAL_MS = 5000;
  let inFlight = false;
  let refreshTimer: ReturnType<typeof setInterval>;

  // Delete pool state
  let deleteTarget: string | null = null;
  let deleting = false;
  let deleteError = '';

  async function loadDashboard() {
    if (inFlight) return;
    inFlight = true;
    try {
      const res = await fetch('/api/dashboard');
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}`);
      }
      data = await res.json();
      error = '';
    } catch (e) {
      error = 'Failed to load dashboard data: ' + e;
    } finally {
      inFlight = false;
      loading = false;
    }
  }

  function confirmDelete(name: string) {
    deleteTarget = name;
    deleteError = '';
  }

  function cancelDelete() {
    deleteTarget = null;
    deleteError = '';
  }

  async function deletePool() {
    if (!deleteTarget) return;
    deleting = true;
    deleteError = '';
    try {
      const res = await fetch(`/api/volumes/zfs/${encodeURIComponent(deleteTarget)}`, {
        method: 'DELETE',
      });
      if (!res.ok) {
        const text = await res.text();
        throw new Error(`HTTP ${res.status} — ${text.slice(0, 200)}`);
      }
      // Refresh dashboard to reflect the deletion
      await loadDashboard();
      cancelDelete();
    } catch (e) {
      deleteError = 'Failed to delete pool: ' + e;
    } finally {
      deleting = false;
    }
  }

  function formatBytes(bytes: number): string {
    if (!bytes) return '0 B';
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    let i = 0;
    let size = bytes;
    while (size >= 1024 && i < units.length - 1) {
      size /= 1024;
      i++;
    }
    return `${size.toFixed(1)} ${units[i]}`;
  }

  function formatUptime(seconds: number): string {
    if (!seconds) return '—';
    const days = Math.floor(seconds / 86400);
    const hours = Math.floor((seconds % 86400) / 3600);
    const mins = Math.floor((seconds % 3600) / 60);
    if (days > 0) return `${days}d ${hours}h ${mins}m`;
    if (hours > 0) return `${hours}h ${mins}m`;
    return `${mins}m`;
  }

  // formatUpdatedAt renders the ISO timestamp in the browser's locale and
  // hides the zero-value Go time (0001-01-01...) shown before first collection.
  function formatUpdatedAt(iso?: string): string {
    if (!iso || iso.startsWith('0001-01-01')) return '—';
    const d = new Date(iso);
    if (isNaN(d.getTime())) return '—';
    return d.toLocaleString();
  }

  loadDashboard();
  refreshTimer = setInterval(loadDashboard, REFRESH_INTERVAL_MS);
  onDestroy(() => clearInterval(refreshTimer));
</script>

<div>
  <h1 class="text-3xl font-bold mb-2">Dashboard</h1>
  <p class="text-gray-400 mb-8">Overview of your Naslos system.</p>

  {#if loading}
    <p class="text-gray-400">Loading dashboard...</p>
  {:else}
    {#if error}
      <p class="text-red-400 mb-4">{error}</p>
    {/if}
    <!-- Quick stats -->
    <div class="grid grid-cols-1 md:grid-cols-4 gap-4 mb-8">
      <div class="card">
        <div class="text-sm text-gray-400 mb-1">CPU Usage</div>
        <div class="text-3xl font-bold text-naslos-primary">{data.cpu?.usage?.toFixed(1) || '—'}%</div>
        <div class="text-xs text-gray-500">{data.cpu?.cores || 0} cores</div>
      </div>
      <div class="card">
        <div class="text-sm text-gray-400 mb-1">Memory</div>
        <div class="text-3xl font-bold text-green-400">{data.memory?.usage?.toFixed(1) || '—'}%</div>
        <div class="text-xs text-gray-500">{formatBytes(data.memory?.used || 0)} / {formatBytes(data.memory?.total || 0)}</div>
      </div>
      <div class="card">
        <div class="text-sm text-gray-400 mb-1">Disk</div>
        <div class="text-3xl font-bold text-blue-400">{data.disk?.usage?.toFixed(1) || '—'}%</div>
        <div class="text-xs text-gray-500">{formatBytes(data.disk?.used || 0)} / {formatBytes(data.disk?.total || 0)}</div>
      </div>
      <div class="card">
        <div class="text-sm text-gray-400 mb-1">Uptime</div>
        <div class="text-3xl font-bold text-purple-400">{formatUptime(data.system?.uptime || 0)}</div>
        <div class="text-xs text-gray-500">{data.system?.hostname || '—'}</div>
      </div>
    </div>

    <!-- ZFS Pools -->
    <div class="card mb-6">
      <h2 class="text-xl font-bold mb-4">ZFS Pools</h2>
      {#if (data.zfs?.pools?.length ?? 0) > 0}
        <div class="grid grid-cols-1 md:grid-cols-2 gap-4">
          {#each data.zfs?.pools ?? [] as pool}
            <div class="bg-naslos-dark rounded-lg p-4">
              <div class="flex items-center justify-between mb-2">
                <a href="/pools/{pool.name}" class="font-bold hover:text-naslos-primary transition-colors">{pool.name}</a>
                <div class="flex items-center gap-2">
                  <span class={`text-xs px-2 py-0.5 rounded ${pool.health === 'ONLINE' ? 'bg-green-900/50 text-green-400' : 'bg-red-900/50 text-red-400'}`}>{pool.health}</span>
                  <button
                    class="text-xs px-2 py-0.5 rounded bg-red-900/30 text-red-400 hover:bg-red-900/60 transition-colors"
                    on:click={() => confirmDelete(pool.name)}
                    title="Delete pool '{pool.name}'"
                  >Delete</button>
                </div>
              </div>
              <a href="/pools/{pool.name}" class="block">
                <div class="w-full bg-naslos-border rounded-full h-2 mb-1">
                  <div class="bg-naslos-primary h-2 rounded-full" style="width: {pool.usagePercent || 0}%"></div>
                </div>
                <div class="text-xs text-gray-500">{formatBytes(pool.alloc || 0)} / {formatBytes(pool.size || 0)} ({pool.usagePercent?.toFixed(1) || 0}%)</div>
              </a>
            </div>
          {/each}
        </div>
      {:else}
        <div class="text-center py-8">
          <p class="text-gray-400 mb-4">No ZFS pools configured yet.</p>
          <a href="/disks" class="btn btn-primary">Create Your First Pool</a>
        </div>
      {/if}
    </div>

    <!-- Delete Confirmation Modal -->
    {#if deleteTarget}
      <div class="fixed inset-0 bg-black/60 flex items-center justify-center z-50">
        <div class="bg-naslos-card border border-naslos-border rounded-lg p-6 max-w-md w-full mx-4">
          <h3 class="text-xl font-bold mb-2">Delete Pool</h3>
          <p class="text-gray-400 mb-4">
            Are you sure you want to delete pool <strong class="text-white">{deleteTarget}</strong>?
            This will destroy the pool and all data on it. This action cannot be undone.
          </p>
          {#if deleteError}
            <p class="text-red-400 text-sm mb-4">{deleteError}</p>
          {/if}
          <div class="flex justify-end gap-3">
            <button class="btn btn-secondary" on:click={cancelDelete} disabled={deleting}>Cancel</button>
            <button class="btn bg-red-600 hover:bg-red-700 text-white" on:click={deletePool} disabled={deleting}>
              {deleting ? 'Deleting…' : 'Delete Pool'}
            </button>
          </div>
        </div>
      </div>
    {/if}

    <!-- System Info -->
    <div class="card">
      <h2 class="text-xl font-bold mb-4">System Information</h2>
      <div class="grid grid-cols-2 md:grid-cols-4 gap-4">
        <div>
          <div class="text-sm text-gray-400">Hostname</div>
          <div class="font-medium">{data.system?.hostname || '—'}</div>
        </div>
        <div>
          <div class="text-sm text-gray-400">OS</div>
          <div class="font-medium">{data.system?.os || 'Talos Linux'}</div>
        </div>
        <div>
          <div class="text-sm text-gray-400">Uptime</div>
          <div class="font-medium">{formatUptime(data.system?.uptime || 0)}</div>
        </div>
        <div>
          <div class="text-sm text-gray-400">Updated</div>
          <div class="font-medium">{formatUpdatedAt(data.updatedAt)}</div>
        </div>
      </div>
    </div>
  {/if}
</div>
