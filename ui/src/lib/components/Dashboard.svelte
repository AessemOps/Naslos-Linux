<script lang="ts">
  import { modal } from '$lib/actions/modal';
  import { onDestroy } from 'svelte';

  interface DashboardData {
    cpu?: {
      usage: number;
      cores: number;
      model?: string;
      temperatureC?: number | null;
      loadAvg1?: number;
      loadAvg5?: number;
      loadAvg15?: number;
      perCore?: Array<{ core: number; usagePercent: number; frequencyMhz?: number }>;
    };
    memory?: { usage: number; total: number; used: number; available: number };
    disk?: { usage: number; total: number; used: number; free: number };
    zfs?: { poolCount: number; pools: any[] };
    system?: { hostname: string; uptime: number; os: string };
    network?: {
      bytesSent?: number;
      bytesRecv?: number;
      interfaces?: Array<{ name: string; ipAddress?: string }>;
    };
    updatedAt?: string;
  }

  // Interfaces with an address are the ones an operator can use; a node also has
  // dozens of veth/pseudo interfaces with none.
  $: addressedInterfaces = (data.network?.interfaces ?? []).filter((i) => i.ipAddress);

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
  let deleteConfirmText = '';

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
    deleteConfirmText = '';
  }

  function cancelDelete() {
    deleteTarget = null;
    deleteError = '';
    deleteConfirmText = '';
  }

  async function deletePool() {
    if (!deleteTarget) return;
    deleting = true;
    deleteError = '';
    try {
      const res = await fetch(
        `/api/volumes/zfs/${encodeURIComponent(deleteTarget)}?confirm=${encodeURIComponent(deleteTarget)}`,
        { method: 'DELETE' }
      );
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

  function formatTemperature(value?: number | null): string {
    return value === undefined || value === null || Number.isNaN(value)
      ? '—'
      : `${value.toFixed(1)} °C`;
  }

  function formatLoad(cpu?: DashboardData['cpu']): string {
    if (!cpu) return '—';
    return `${(cpu.loadAvg1 ?? 0).toFixed(2)} / ${(cpu.loadAvg5 ?? 0).toFixed(2)} / ${(cpu.loadAvg15 ?? 0).toFixed(2)}`;
  }

  function coreBarClass(usage: number): string {
    if (usage >= 90) return 'bg-red-500';
    if (usage >= 70) return 'bg-amber-400';
    return 'bg-naslos-primary';
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
      <p role="alert" class="text-red-400 mb-4">{error}</p>
    {/if}
    <!-- Quick stats -->
    <div class="grid grid-cols-1 md:grid-cols-4 gap-4 mb-8">
      <div class="card">
        <div class="text-sm text-gray-400 mb-1">CPU Usage</div>
        <div class="text-3xl font-bold text-naslos-primary">{data.cpu?.usage?.toFixed(1) || '—'}%</div>
        <div class="text-xs text-gray-500">{data.cpu?.cores || 0} cores · {formatTemperature(data.cpu?.temperatureC)}</div>
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

    <!-- CPU detail: model, temperature, load averages and per-core usage. -->
    <div class="card mb-6" data-testid="cpu-details">
      <h2 class="text-xl font-bold mb-4">CPU</h2>
      <div class="grid grid-cols-2 md:grid-cols-4 gap-4">
        <div>
          <div class="text-sm text-gray-400">Model</div>
          <div class="font-medium">{data.cpu?.model || '—'}</div>
        </div>
        <div>
          <div class="text-sm text-gray-400">Temperature</div>
          <div class="font-medium">{formatTemperature(data.cpu?.temperatureC)}</div>
        </div>
        <div>
          <div class="text-sm text-gray-400">Load average</div>
          <div class="font-medium font-mono text-sm">{formatLoad(data.cpu)}</div>
        </div>
        <div>
          <div class="text-sm text-gray-400">Cores</div>
          <div class="font-medium">{data.cpu?.cores || 0}</div>
        </div>
      </div>

      {#if data.cpu?.temperatureC == null}
        <p class="text-xs text-gray-500 mt-3" data-testid="cpu-temp-unavailable">
          CPU temperature is not exposed on this node.
        </p>
      {/if}

      <div class="mt-5">
        <div class="text-sm text-gray-400 mb-2">Per-core usage</div>
        {#if (data.cpu?.perCore?.length ?? 0) > 0}
          <div class="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-3">
            {#each data.cpu?.perCore ?? [] as core}
              <div class="bg-naslos-dark rounded-lg p-3" data-testid="cpu-core">
                <div class="flex items-center justify-between text-sm mb-1">
                  <span>Core {core.core}</span>
                  <span class="font-mono">{core.usagePercent.toFixed(1)}%</span>
                </div>
                <div class="w-full bg-naslos-border rounded-full h-2">
                  <div
                    class={`${coreBarClass(core.usagePercent)} h-2 rounded-full transition-all`}
                    style={`width: ${Math.min(100, Math.max(0, core.usagePercent))}%`}
                  ></div>
                </div>
                <div class="text-xs text-gray-500 mt-1">
                  {core.frequencyMhz ? `${core.frequencyMhz} MHz` : 'frequency —'}
                </div>
              </div>
            {/each}
          </div>
        {:else}
          <p class="text-sm text-gray-500">Per-core data is unavailable.</p>
        {/if}
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
      <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
      <div class="fixed inset-0 bg-black/60 flex items-center justify-center z-50" use:modal={{ onClose: cancelDelete }}>
        <div class="bg-naslos-card border border-naslos-border rounded-lg p-6 max-w-md w-full mx-4">
          <h3 class="text-xl font-bold mb-2">Delete Pool</h3>
          <p class="text-gray-400 mb-4">
            Are you sure you want to delete pool <strong class="text-white">{deleteTarget}</strong>?
            This will destroy the pool and all data on it. This action cannot be undone.
          </p>
          <label class="block mb-4">
            <span class="text-sm text-gray-300">Type <span class="font-mono text-white">{deleteTarget}</span> to confirm</span>
            <input class="input w-full mt-1" bind:value={deleteConfirmText} autocomplete="off" />
          </label>
          {#if deleteError}
            <p role="alert" class="text-red-400 text-sm mb-4">{deleteError}</p>
          {/if}
          <div class="flex justify-end gap-3">
            <button class="btn btn-secondary" on:click={cancelDelete} disabled={deleting}>Cancel</button>
            <button
              class="btn bg-red-600 hover:bg-red-700 text-white"
              on:click={deletePool}
              disabled={deleting || deleteConfirmText !== deleteTarget}
            >
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

      <!-- Per-interface addresses (FR-MET-10): the IP an operator types to reach
           this node. -->
      {#if data.network?.interfaces?.length}
        <div class="mt-4 pt-4 border-t border-naslos-border">
          <div class="text-sm text-gray-400 mb-2">
            Network
            <span class="ml-2 text-xs">
              ↓ {formatBytes(data.network.bytesRecv || 0)} · ↑ {formatBytes(data.network.bytesSent || 0)}
            </span>
          </div>
          {#each addressedInterfaces as iface}
            <span class="inline-block mr-4 mb-1">
              <span class="font-mono text-sm">{iface.name}</span>
              <span class="text-sm text-gray-400 ml-1">{iface.ipAddress}</span>
            </span>
          {/each}
        </div>
      {/if}
    </div>
  {/if}
</div>
