<script lang="ts">
  // Pool health overview: the four headline cards, the I/O counters when the
  // API reports them, and the vdev tree. The device states render through the
  // shared StatusBadge so no page-local colour table is needed.
  import StatusBadge from '$lib/components/StatusBadge.svelte';

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

  export let health: PoolHealth;
</script>

<div class="grid grid-cols-1 gap-4 md:grid-cols-4">
  <div class="card">
    <div class="mb-2 text-sm text-gray-400">State</div>
    <StatusBadge state={health.state} unknownTone="bad" />
  </div>
  <div class="card">
    <div class="mb-1 text-sm text-gray-400">Scan Status</div>
    <div class="text-lg font-medium">{health.scan || '—'}</div>
  </div>
  <div class="card">
    <div class="mb-1 text-sm text-gray-400">Errors</div>
    {#if health.errors && health.errors !== 'No known data errors'}
      <StatusBadge tone="warn" label="Errors reported" />
      <p class="mt-2 break-words text-xs text-gray-400">{health.errors}</p>
    {:else}
      <div class="text-lg font-medium">{health.errors || '—'}</div>
    {/if}
  </div>
  <div class="card">
    <div class="mb-1 text-sm text-gray-400">Devices</div>
    <div class="text-2xl font-bold text-naslos-link">{health.config.length}</div>
  </div>
</div>

{#if health.ioStats && (health.ioStats.readOps || health.ioStats.writeOps)}
  <div class="card mt-6">
    <h2 class="mb-4 text-xl font-bold">I/O Statistics</h2>
    <div class="grid grid-cols-2 gap-4 md:grid-cols-4">
      <div>
        <div class="text-sm text-gray-400">Read Ops</div>
        <div class="font-mono text-lg font-medium">{health.ioStats.readOps || '—'}</div>
      </div>
      <div>
        <div class="text-sm text-gray-400">Write Ops</div>
        <div class="font-mono text-lg font-medium">{health.ioStats.writeOps || '—'}</div>
      </div>
      <div>
        <div class="text-sm text-gray-400">Read Bandwidth</div>
        <div class="font-mono text-lg font-medium">{health.ioStats.readBW || '—'}</div>
      </div>
      <div>
        <div class="text-sm text-gray-400">Write Bandwidth</div>
        <div class="font-mono text-lg font-medium">{health.ioStats.writeBW || '—'}</div>
      </div>
    </div>
  </div>
{/if}

<div class="card mt-6">
  <h2 class="mb-4 text-xl font-bold">Device Configuration</h2>
  <div class="space-y-2">
    {#each health.config as dev (dev.name)}
      <div class="rounded-lg bg-naslos-dark p-3">
        <div class="flex flex-wrap items-center justify-between gap-2">
          <div class="flex flex-wrap items-center gap-3">
            <span class="font-mono font-medium">{dev.name}</span>
            <StatusBadge state={dev.state} />
          </div>
          <div class="font-mono text-xs text-gray-500">
            Read: {dev.read || '0'} · Write: {dev.write || '0'} · Cksum: {dev.cksum || '0'}
          </div>
        </div>
        {#if dev.devices && dev.devices.length > 0}
          <div class="mt-2 space-y-1 border-l-2 border-naslos-border pl-4">
            {#each dev.devices as child (child.name)}
              <div class="flex flex-wrap items-center justify-between gap-2 text-sm">
                <div class="flex items-center gap-2">
                  <span class="text-gray-500" aria-hidden="true">└</span>
                  <span class="font-mono">{child.name}</span>
                  <StatusBadge state={child.state} />
                </div>
                <div class="font-mono text-xs text-gray-500">
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
