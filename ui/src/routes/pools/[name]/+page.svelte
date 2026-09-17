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

  // Datasets in this pool: a dataset is what a share points at, so creating one
  // here is usually the step right before creating a share.
  interface Dataset {
    name: string;
    used: string;
    avail: string;
    refer: string;
    mountpoint: string;
  }
  let datasets: Dataset[] = [];
  let datasetsError = '';

  let showDatasetModal = false;
  let datasetName = '';
  let datasetCompression = 'lz4';
  let datasetQuota = '';
  let datasetSaving = false;
  let datasetError = '';

  let showAddDriveModal = false;
  let disks: any[] = [];
  let disksError = '';
  let selectedDisks: string[] = [];
  let addTopology = 'single';
  let addForce = false;
  let addSaving = false;
  let addError = '';

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

  // Datasets come from the API (every dataset) filtered to this pool, so the
  // page shows what actually exists rather than what was requested.
  async function loadDatasets() {
    try {
      const res = await fetch('/api/datasets');
      if (!res.ok) {
        datasetsError = `Failed to load datasets (HTTP ${res.status})`;
        return;
      }
      const all: Dataset[] = await res.json();
      const prefix = `${poolName}/`;
      datasets = all.filter(d => d.name === poolName || d.name.startsWith(prefix));
      datasetsError = '';
    } catch (e) {
      datasetsError = 'Failed to connect to API';
    }
  }

  async function createDataset() {
    datasetSaving = true;
    datasetError = '';
    try {
      const options: Record<string, string> = {};
      if (datasetCompression) options.compression = datasetCompression;
      if (datasetQuota.trim()) options.quota = datasetQuota.trim();

      const res = await fetch('/api/datasets', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ pool: poolName, name: datasetName, options })
      });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) throw new Error(data.error || 'Could not create the dataset');

      showDatasetModal = false;
      datasetName = '';
      datasetQuota = '';
      await loadDatasets();
    } catch (e: any) {
      datasetError = e.message;
    } finally {
      datasetSaving = false;
    }
  }

  async function deleteDataset(name: string, recursive: boolean) {
    try {
      const url = `/api/datasets?name=${encodeURIComponent(name)}${recursive ? '&recursive=true' : ''}`;
      const res = await fetch(url, { method: 'DELETE' });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) {
        // A non-empty dataset needs the recursive flag. Ask rather than guess:
        // recursive also destroys snapshots and child datasets.
        if (!recursive && /not empty|has children|snapshot/i.test(data.error || '')) {
          if (confirm(`${name} is not empty.\n\nDestroy it recursively? Its snapshots and child datasets go with it, and this cannot be undone.`)) {
            await deleteDataset(name, true);
          }
          return;
        }
        alert(data.error || `Could not destroy ${name}`);
        return;
      }
      await loadDatasets();
    } catch (e: any) {
      alert(e.message);
    }
  }

  async function openAddDrive() {
    showAddDriveModal = true;
    disksError = '';
    selectedDisks = [];
    addError = '';
    try {
      const res = await fetch('/api/disks');
      if (!res.ok) {
        disksError = `Failed to list disks (HTTP ${res.status})`;
        return;
      }
      disks = await res.json();
    } catch (e) {
      disksError = 'Failed to connect to API';
    }
  }

  function toggleDisk(device: string) {
    selectedDisks = selectedDisks.includes(device)
      ? selectedDisks.filter(d => d !== device)
      : [...selectedDisks, device];
  }

  // What each topology costs: this is shown before the button is pressed, not
  // discovered afterwards, because `zpool add` cannot be undone in general.
  const topologyDisksNeeded: Record<string, number> = {
    single: 1, stripe: 1, mirror: 2, raidz1: 2, raidz2: 3, raidz3: 4
  };

  function topologyHint(): string {
    switch (addTopology) {
      case 'mirror': return 'Mirror: every selected disk holds a copy. Survives one disk failing.';
      case 'raidz1': return 'RAIDZ1: one disk of parity. Survives one disk failing.';
      case 'raidz2': return 'RAIDZ2: two disks of parity. Survives two disks failing.';
      case 'raidz3': return 'RAIDZ3: three disks of parity. Survives three disks failing.';
      default: return 'Stripe: capacity only — NO redundancy. If any disk in this vdev fails, the whole pool is lost.';
    }
  }

  async function addDevices() {
    addSaving = true;
    addError = '';
    try {
      const res = await fetch(`/api/volumes/zfs/${poolName}/devices`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ disks: selectedDisks, topology: addTopology, force: addForce })
      });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) throw new Error(data.error || 'Could not add the disks');

      showAddDriveModal = false;
      await Promise.all([loadHealth(), loadDatasets()]);
    } catch (e: any) {
      addError = e.message;
    } finally {
      addSaving = false;
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

  onMount(() => {
    loadHealth();
    loadDatasets();
  });
</script>

<div>
  <div class="mb-6">
    <a href="/pools" class="text-sm text-gray-400 hover:text-naslos-primary">← Back to Pools</a>
  </div>

  <div class="flex items-start justify-between gap-4 mb-8">
    <div>
      <h1 class="text-3xl font-bold mb-2">{poolName}</h1>
      <p class="text-gray-400">Pool health, datasets and devices.</p>
    </div>
    <div class="flex gap-2 shrink-0">
      <button class="btn btn-secondary" on:click={openAddDrive}>Add Drive</button>
      <button class="btn btn-primary" on:click={() => { showDatasetModal = true; datasetError = ''; }}>New Dataset</button>
    </div>
  </div>

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

    <!-- Datasets: the level shares point at -->
    <div class="card mt-6">
      <div class="flex items-center justify-between mb-4">
        <h2 class="text-xl font-bold">Datasets</h2>
        <button class="btn btn-secondary btn-sm" on:click={() => { showDatasetModal = true; datasetError = ''; }}>New Dataset</button>
      </div>

      {#if datasetsError}
        <p class="text-red-400 text-sm">{datasetsError}</p>
      {:else if datasets.length === 0}
        <p class="text-gray-400 text-sm">No datasets yet. Create one to have a place for a share.</p>
      {:else}
        <div class="space-y-2">
          {#each datasets as ds}
            <div class="bg-naslos-dark rounded-lg p-3 flex items-center justify-between gap-3">
              <div class="min-w-0">
                <div class="font-medium truncate">{ds.name}</div>
                <div class="text-xs text-gray-500">{ds.mountpoint || '—'}</div>
              </div>
              <div class="text-xs text-gray-400 whitespace-nowrap">
                {ds.used || '0'} used · {ds.avail || '0'} free
              </div>
              <button
                class="text-xs px-2 py-1 rounded border border-naslos-border text-red-300 hover:bg-red-900/30 whitespace-nowrap"
                on:click={() => deleteDataset(ds.name, false)}
                title="Destroy this dataset"
              >Delete</button>
            </div>
          {/each}
        </div>
      {/if}
    </div>

    <!-- Device Tree -->
    <div class="card mt-6">
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

<!-- New Dataset modal -->
{#if showDatasetModal}
  <div class="fixed inset-0 bg-black/60 z-50 flex items-center justify-center p-4" role="presentation" on:click|self={() => showDatasetModal = false}>
    <div class="bg-naslos-surface rounded-2xl border border-naslos-border w-full max-w-md">
      <div class="p-6 border-b border-naslos-border flex items-center justify-between">
        <h2 class="text-xl font-bold">New Dataset</h2>
        <button class="text-gray-400 hover:text-white text-2xl" on:click={() => showDatasetModal = false}>&times;</button>
      </div>

      <div class="p-6 space-y-4">
        <p class="text-sm text-gray-400">
          A dataset inside <span class="text-naslos-primary">{poolName}</span> becomes a folder you can share.
          Nested names work: <code class="text-xs">photos/2026</code>.
        </p>

        <div>
          <label class="label" for="pool-field-1">Name *</label>
          <input id="pool-field-1" type="text" bind:value={datasetName} placeholder="e.g. media" class="input w-full" />
        </div>

        <div>
          <label class="label" for="pool-field-2">Compression</label>
          <select id="pool-field-2" bind:value={datasetCompression} class="input w-full">
            <option value="lz4">lz4 (fast, good default)</option>
            <option value="zstd">zstd (better ratio, more CPU)</option>
            <option value="off">off</option>
            <option value="gzip">gzip</option>
          </select>
        </div>

        <div>
          <label class="label" for="pool-field-3">Quota</label>
          <input id="pool-field-3" type="text" bind:value={datasetQuota} placeholder="e.g. 500G (empty = pool-wide limit)" class="input w-full" />
        </div>

        {#if datasetError}<div class="p-3 rounded-lg bg-red-900/30 border border-red-700 text-red-300 text-sm">{datasetError}</div>{/if}
      </div>

      <div class="p-6 border-t border-naslos-border flex justify-end gap-3">
        <button class="btn btn-secondary" on:click={() => showDatasetModal = false}>Cancel</button>
        <button class="btn btn-primary" on:click={createDataset} disabled={datasetSaving || !datasetName.trim()}>
          {datasetSaving ? 'Creating...' : 'Create Dataset'}
        </button>
      </div>
    </div>
  </div>
{/if}

</div>

<!-- Add Drive modal -->
{#if showAddDriveModal}
  <div class="fixed inset-0 bg-black/60 z-50 flex items-center justify-center p-4" role="presentation" on:click|self={() => showAddDriveModal = false}>
    <div class="bg-naslos-surface rounded-2xl border border-naslos-border w-full max-w-lg">
      <div class="p-6 border-b border-naslos-border flex items-center justify-between">
        <h2 class="text-xl font-bold">Add Drive to {poolName}</h2>
        <button class="text-gray-400 hover:text-white text-2xl" on:click={() => showAddDriveModal = false}>&times;</button>
      </div>

      <div class="p-6 space-y-4 max-h-[65vh] overflow-y-auto">
        <p class="text-sm text-gray-400">
          The selected disks become a new vdev in this pool. Existing data is not redistributed, and a disk
          that already belongs to a pool cannot be selected.
        </p>

        {#if disksError}
          <p class="text-red-400 text-sm">{disksError}</p>
        {:else if disks.length === 0}
          <p class="text-gray-400 text-sm">No disks detected.</p>
        {:else}
          <div class="space-y-2">
            {#each disks as disk}
              <label
                class="flex items-center gap-3 p-3 rounded-lg border border-naslos-border {disk.inPool ? 'opacity-50 cursor-not-allowed' : 'cursor-pointer hover:bg-naslos-dark'}"
              >
                <input
                  type="checkbox"
                  class="w-4 h-4 rounded"
                  disabled={!!disk.inPool}
                  checked={selectedDisks.includes(disk.device)}
                  on:change={() => toggleDisk(disk.device)}
                />
                <div class="flex-1">
                  <div class="font-medium">{disk.device}</div>
                  <div class="text-xs text-gray-500">
                    {(disk.size / 1024 / 1024 / 1024).toFixed(1)} GB · {disk.type || 'disk'}
                  </div>
                </div>
                {#if disk.inPool}
                  <span class="text-xs px-2 py-0.5 rounded bg-naslos-border text-gray-400">in pool {disk.inPool}</span>
                {:else}
                  <span class="text-xs px-2 py-0.5 rounded bg-green-900/40 text-green-400">available</span>
                {/if}
              </label>
            {/each}
          </div>
        {/if}

        <div>
          <label class="label" for="pool-field-4">Topology</label>
          <select id="pool-field-4" bind:value={addTopology} class="input w-full">
            <option value="single">Stripe (no redundancy)</option>
            <option value="mirror">Mirror (2+ disks, survives 1 failure)</option>
            <option value="raidz1">RAIDZ1 (2+ disks, survives 1 failure)</option>
            <option value="raidz2">RAIDZ2 (3+ disks, survives 2 failures)</option>
            <option value="raidz3">RAIDZ3 (4+ disks, survives 3 failures)</option>
          </select>
          <p class="text-xs mt-1 {addTopology === 'single' ? 'text-yellow-400' : 'text-gray-500'}">
            {topologyHint()} Needs at least {topologyDisksNeeded[addTopology] || 1} disk{topologyDisksNeeded[addTopology] > 1 ? 's' : ''}.
          </p>
        </div>

        <label class="flex items-center gap-3 cursor-pointer">
          <input type="checkbox" bind:checked={addForce} class="w-4 h-4 rounded" />
          <span class="text-gray-300 text-sm">Force: overwrite any existing signature on the disks</span>
        </label>
        {#if addForce}
          <p class="text-xs text-red-400">
            Forcing writes over whatever is on the disks. If a disk holds another pool's label, that pool is destroyed.
          </p>
        {/if}

        {#if addError}<div class="p-3 rounded-lg bg-red-900/30 border border-red-700 text-red-300 text-sm">{addError}</div>{/if}
      </div>

      <div class="p-6 border-t border-naslos-border flex justify-end gap-3">
        <button class="btn btn-secondary" on:click={() => showAddDriveModal = false}>Cancel</button>
        <button
          class="btn btn-primary"
          on:click={addDevices}
          disabled={addSaving || selectedDisks.length < (topologyDisksNeeded[addTopology] || 1)}
        >
          {addSaving ? 'Adding...' : `Add ${selectedDisks.length || ''} disk${selectedDisks.length === 1 ? '' : 's'}`}
        </button>
      </div>
    </div>
  </div>
{/if}
