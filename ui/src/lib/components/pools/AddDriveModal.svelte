<script lang="ts">
  // Add a vdev to an existing pool. Loading is explicit: the dialog shows a
  // loading state, never "No disks detected." while the request is in flight,
  // and a reopened dialog never renders the previous disk list.
  import { onMount } from 'svelte';
  import { api, ApiError } from '$lib/api';
  import AsyncState from '$lib/components/AsyncState.svelte';
  import Button from '$lib/components/Button.svelte';
  import Modal from '$lib/components/Modal.svelte';
  import { formatBytes } from '$lib/format';

  interface Disk {
    device: string;
    size: number;
    type?: string;
    inPool?: string;
    signatures?: string[];
  }

  export let poolName = '';
  export let onClose: () => void;
  export let onAdded: () => void;

  let disks: Disk[] = [];
  let disksLoading = true;
  let disksError = '';
  let selectedDisks: string[] = [];
  let addTopology = 'single';
  let addForce = false;
  let addSaving = false;
  let addError = '';

  const topologyDisksNeeded: Record<string, number> = {
    single: 1,
    stripe: 1,
    mirror: 2,
    raidz1: 2,
    raidz2: 3,
    raidz3: 4
  };

  async function loadDisks() {
    disksLoading = true;
    disksError = '';
    disks = [];
    selectedDisks = [];
    addForce = false;
    try {
      const data = await api<Disk[]>('/api/disks');
      disks = Array.isArray(data) ? data : [];
    } catch (e) {
      disksError =
        e instanceof ApiError ? `Failed to list disks (HTTP ${e.status})` : 'Failed to connect to the API';
    } finally {
      disksLoading = false;
    }
  }

  function diskSignatures(device: string): string[] {
    const disk = disks.find((d) => d.device === device);
    return disk && Array.isArray(disk.signatures) ? disk.signatures : [];
  }

  function isZfsMember(device: string): boolean {
    const sigs = diskSignatures(device);
    return sigs.includes('zfs') || sigs.includes('zfs_member');
  }

  // A disk with data needs the explicit "Force" acknowledgement before the API
  // accepts it; a ZFS label is refused outright.
  function needsWipeConfirm(): boolean {
    return selectedDisks.some(
      (d) => diskSignatures(d).length > 0 && !isZfsMember(d)
    );
  }

  function toggleDisk(device: string) {
    selectedDisks = selectedDisks.includes(device)
      ? selectedDisks.filter((d) => d !== device)
      : [...selectedDisks, device];
    // A changed selection invalidates the previous explicit confirmation.
    addForce = false;
  }

  // What each topology costs: this is shown before the button is pressed, not
  // discovered afterwards, because `zpool add` cannot be undone in general.
  function topologyHint(): string {
    switch (addTopology) {
      case 'mirror':
        return 'Mirror: every selected disk holds a copy. Survives one disk failing.';
      case 'raidz1':
        return 'RAIDZ1: one disk of parity. Survives one disk failing.';
      case 'raidz2':
        return 'RAIDZ2: two disks of parity. Survives two disks failing.';
      case 'raidz3':
        return 'RAIDZ3: three disks of parity. Survives three disks failing.';
      default:
        return 'Stripe: capacity only — NO redundancy. If any disk in this vdev fails, the whole pool is lost.';
    }
  }

  async function addDevices() {
    addSaving = true;
    addError = '';
    try {
      await api(`/api/volumes/zfs/${encodeURIComponent(poolName)}/devices`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          disks: selectedDisks,
          topology: addTopology,
          force: addForce,
          wipeConfirmed: addForce ? selectedDisks : []
        })
      });
      onAdded();
    } catch (e) {
      addError =
        e instanceof ApiError ? e.message : 'Could not add the disks. Check the API connection.';
    } finally {
      addSaving = false;
    }
  }

  onMount(loadDisks);
</script>

<Modal title={`Add Drive to ${poolName}`} size="lg" {onClose}>
  <p class="text-sm text-gray-400">
    The selected disks become a new vdev in this pool. Existing data is not redistributed, and a
    disk that already belongs to a pool cannot be selected.
  </p>

  <AsyncState
    loading={disksLoading}
    error={disksError}
    empty={disks.length === 0}
    loadingText="Loading disks…"
    emptyTitle="No disks detected."
    emptyDetail="No whole disks were reported by the node."
    emptyIcon="hard-drive"
    onRetry={loadDisks}
  >
    <div class="space-y-2">
      {#each disks as disk (disk.device)}
        <label
          class="flex items-center gap-3 rounded-lg border p-3 transition-colors {disk.inPool
            ? 'cursor-not-allowed border-naslos-border opacity-50'
            : selectedDisks.includes(disk.device)
              ? 'cursor-pointer border-naslos-primary bg-naslos-primary/10'
              : 'cursor-pointer border-naslos-border hover:bg-naslos-dark'}"
        >
          <input
            type="checkbox"
            class="h-4 w-4 rounded"
            disabled={!!disk.inPool || isZfsMember(disk.device)}
            checked={selectedDisks.includes(disk.device)}
            on:change={() => toggleDisk(disk.device)}
          />
          <span class="min-w-0 flex-1">
            <span class="block font-mono font-medium">{disk.device}</span>
            <span class="block text-xs text-gray-500">
              {formatBytes(disk.size)} · {disk.type || 'disk'}
            </span>
          </span>
          {#if disk.inPool}
            <span class="badge badge-neutral">in pool {disk.inPool}</span>
          {:else if isZfsMember(disk.device)}
            <span class="badge badge-bad">holds a ZFS pool</span>
          {:else if (disk.signatures || []).length > 0}
            <span class="badge badge-warn">contains {(disk.signatures || []).join(', ')}</span>
          {:else}
            <span class="badge badge-ok">available</span>
          {/if}
        </label>
      {/each}
    </div>
  </AsyncState>

  <div>
    <label class="label" for="pool-field-4">Topology</label>
    <select id="pool-field-4" class="input w-full" bind:value={addTopology}>
      <option value="single">Stripe (no redundancy)</option>
      <option value="mirror">Mirror (2+ disks, survives 1 failure)</option>
      <option value="raidz1">RAIDZ1 (2+ disks, survives 1 failure)</option>
      <option value="raidz2">RAIDZ2 (3+ disks, survives 2 failures)</option>
      <option value="raidz3">RAIDZ3 (4+ disks, survives 3 failures)</option>
    </select>
    <p class="mt-1 text-xs {addTopology === 'single' ? 'text-amber-300' : 'text-gray-500'}">
      {topologyHint()} Needs at least {topologyDisksNeeded[addTopology] || 1} disk{topologyDisksNeeded[
        addTopology
      ] > 1
        ? 's'
        : ''}.
    </p>
  </div>

  <label class="flex cursor-pointer items-center gap-3">
    <input type="checkbox" bind:checked={addForce} class="h-4 w-4 rounded" />
    <span class="text-sm text-gray-300">
      Force: overwrite any existing signature on the disks (required when a selected disk already
      contains data)
    </span>
  </label>
  {#if addForce}
    <p role="alert" class="text-xs text-red-300">
      Forcing writes over whatever is on the disks. If a disk holds another pool's label, that pool
      is destroyed.
    </p>
  {/if}

  {#if addError}
    <div role="alert" class="callout callout-bad mb-0">{addError}</div>
  {/if}

  <div slot="footer">
    <Button variant="secondary" on:click={onClose}>Cancel</Button>
    <Button
      loading={addSaving}
      disabled={selectedDisks.length < (topologyDisksNeeded[addTopology] || 1) ||
        (needsWipeConfirm() && !addForce)}
      on:click={addDevices}
    >
      Add {selectedDisks.length} disk{selectedDisks.length === 1 ? '' : 's'}
    </Button>
  </div>
</Modal>
