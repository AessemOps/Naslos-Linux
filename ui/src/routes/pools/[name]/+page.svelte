<script lang="ts">
  import { onMount } from 'svelte';
  import { page } from '$app/stores';

  interface PoolDevice {
    name: string;
    state: string;
    read: string;
    write: string;
    cksum: string;
    devices?: PoolDevice[];
  }

  interface PoolIOStats {
    readOps: string;
    writeOps: string;
    readBW: string;
    writeBW: string;
  }

  interface PoolHealth {
    name: string;
    state: string;
    scan: string;
    errors: string;
    config: PoolDevice[];
    ioStats: PoolIOStats;
  }

  let health: PoolHealth | null = null;
  let loading = true;
  let error = '';

  const poolName = $page.params.name;

  async function loadHealth() {
    try {
      const res = await fetch(`/api/volumes/zfs/${poolName}/health`);
      if (res.ok) {
        health = await res.json();
      } else {
        error = `Failed to load health data (HTTP ${res.status})`;
      }
    } catch (e) {
      error = 'Failed to connect to API';
    } finally {
      loading = false;
    }
  }

  function stateColor(state: string): string {
    switch (state) {
      case 'ONLINE': return 'text-green-400';
      case 'DEGRADED': return 'text-yellow-400';
      case 'FAULTED': return 'text-red-400';
      case 'OFFLINE': return 'text-gray-400';
      case 'UNAVAIL': return 'text-red-400';
      default: return 'text-gray-300';
    }
  }

  function stateBadge(state: string): string {
    switch (state) {
      case 'ONLINE': return 'bg-green-900/50 text-green-400';
      case 'DEGRADED': return 'bg-yellow-900/50 text-yellow-400';
      case 'FAULTED': return 'bg-red-900/50 text-red-400';
      case 'OFFLINE': return 'bg-gray-900/50 text-gray-400';
      case 'UNAVAIL': return 'bg-red-900/50 text-red-400';
      default: return 'bg-naslos-border text-gray-300';
    }
  }

  onMount(loadHealth);
</script>

<div>
  <div class="mb-6">
    <a href="/pools" class="text-sm text-gray-400 hover:text-naslos-primary">← Back to Pools</a>
  </div>

  <h1 class="text-3xl font-bold mb-2">{poolName}</h1>
  <p class="text-gray-400 mb-8">Pool health and device status.</p>

  {#if loading}
    <p class="text-gray-400">Loading health data...</p>
  {:else if error}
    <p class="text-red-400">{error}</p>
  {:else if health}
    <!-- Status Overview -->
    <div class="grid grid-cols-1 md:grid-cols-4 gap-4 mb-8">
      <div class="card">
        <div class="text-sm text-gray-400 mb-1">State</div>
        <div class="text-2xl font-bold {stateColor(health.state)}">{health.state}</div>
      </div>
      <div class="card">
        <div class="text-sm text-gray-400 mb-1">Scan Status</div>
        <div class="text-lg font-medium">{health.scan || '—'}</div>
      </div>
      <div class="card">
        <div class="text-sm text-gray-400 mb-1">Errors</div>
        <div class="text-lg font-medium {health.errors && health.errors !== 'No known data errors' ? 'text-yellow-400' : ''}">{health.errors || '—'}</div>
      </div>
      <div class="card">
        <div class="text-sm text-gray-400 mb-1">Devices</div>
        <div class="text-2xl font-bold text-naslos-primary">{health.config.length}</div>
      </div>
    </div>

    <!-- I/O Statistics -->
    {#if health.ioStats && (health.ioStats.readOps || health.ioStats.writeOps)}
      <div class="card mb-6">
        <h2 class="text-xl font-bold mb-4">I/O Statistics</h2>
        <div class="grid grid-cols-2 md:grid-cols-4 gap-4">
          <div>
            <div class="text-sm text-gray-400">Read Ops</div>
            <div class="text-lg font-medium">{health.ioStats.readOps || '—'}</div>
          </div>
          <div>
            <div class="text-sm text-gray-400">Write Ops</div>
            <div class="text-lg font-medium">{health.ioStats.writeOps || '—'}</div>
          </div>
          <div>
            <div class="text-sm text-gray-400">Read Bandwidth</div>
            <div class="text-lg font-medium">{health.ioStats.readBW || '—'}</div>
          </div>
          <div>
            <div class="text-sm text-gray-400">Write Bandwidth</div>
            <div class="text-lg font-medium">{health.ioStats.writeBW || '—'}</div>
          </div>
        </div>
      </div>
    {/if}

    <!-- Device Tree -->
    <div class="card">
      <h2 class="text-xl font-bold mb-4">Device Configuration</h2>
      <div class="space-y-2">
        {#each health.config as dev}
          <div class="bg-naslos-dark rounded-lg p-3">
            <div class="flex items-center justify-between">
              <div class="flex items-center gap-3">
                <span class="font-medium">{dev.name}</span>
                <span class="text-xs px-2 py-0.5 rounded {stateBadge(dev.state)}">{dev.state}</span>
              </div>
              <div class="text-xs text-gray-500">
                Read: {dev.read || '0'} · Write: {dev.write || '0'} · Cksum: {dev.cksum || '0'}
              </div>
            </div>
            {#if dev.devices && dev.devices.length > 0}
              <div class="mt-2 pl-4 border-l-2 border-naslos-border space-y-1">
                {#each dev.devices as child}
                  <div class="flex items-center justify-between text-sm">
                    <div class="flex items-center gap-2">
                      <span class="text-gray-500">└</span>
                      <span>{child.name}</span>
                      <span class="text-xs px-2 py-0.5 rounded {stateBadge(child.state)}">{child.state}</span>
                    </div>
                    <div class="text-xs text-gray-500">
                      Read: {child.read || '0'} · Write: {child.write || '0'} · Cksum: {child.cksum || '0'}
                    </div>
                  </div>
                {/each}
              </div>
            {/if}
          </div>
        {/each}
      </div>
    </div>
  {/if}
</div>