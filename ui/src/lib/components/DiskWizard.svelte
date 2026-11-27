<script lang="ts">
  import { onMount } from 'svelte';
  import AsyncState from '$lib/components/AsyncState.svelte';
  import Button from '$lib/components/Button.svelte';
  import EmptyState from '$lib/components/EmptyState.svelte';
  import FormField from '$lib/components/FormField.svelte';
  import { formatBytes } from '$lib/format';
  import { parseJsonSafe } from '$lib/storage';

  interface Disk {
    device: string;
    size: number;
    isSystemDisk: boolean;
    model: string;
    serial: string;
    type: string;
    inPool?: string; // name of the pool this disk already belongs to (if any)
    signatures?: string[]; // filesystems found on the disk (W2)
  }

  interface Recommendation {
    topology: string;
    disks: string[];
    description: string;
  }

  let step = 1;
  let disks: Disk[] = [];
  let loading = true;
  let error = '';
  let selectedDisks: string[] = [];
  let recommendation: Recommendation | null = null;
  let customTopology = '';
  let poolName = '';
  let cacheDisk = '';
  let wipeConfirmed: string[] = [];
  let creating = false;
  let createResult = '';
  let createFailed = false;

  $: selectableDisks = disks.filter((d) => !d.isSystemDisk);
  $: cacheCandidates = selectableDisks.filter(
    (d) => !selectedDisks.includes(d.device) && !d.inPool && !isZfsMember(d)
  );

  // Available topologies with descriptions for the customization UI.
  const topologyOptions = [
    {
      value: 'single',
      label: 'Single',
      desc: 'No redundancy. All disks combined into one pool. If any disk fails, all data is lost.'
    },
    {
      value: 'mirror',
      label: 'Mirror',
      desc: 'Each disk has an exact copy. Survives one disk failure per mirror pair. Best performance for reads.'
    },
    {
      value: 'raidz1',
      label: 'RAIDZ1',
      desc: 'One disk parity. Survives one disk failure. Good balance of capacity and redundancy for 3-5 disks.'
    },
    {
      value: 'raidz2',
      label: 'RAIDZ2',
      desc: 'Two disk parity. Survives two disk failures. Recommended for larger pools (6+ disks).'
    },
    {
      value: 'raidz3',
      label: 'RAIDZ3',
      desc: 'Three disk parity. Survives three disk failures. Maximum redundancy for large pools.'
    }
  ];

  async function loadDisks() {
    loading = true;
    error = '';
    disks = [];
    try {
      const res = await fetch('/api/disks');
      const data = await parseJsonSafe<Disk[]>(res, 'Loading disks');
      if (!Array.isArray(data)) {
        throw new Error('Loading disks failed: expected a disk list but got ' + typeof data);
      }
      disks = data;
    } catch (e) {
      error = 'Failed to load disks: ' + e;
    } finally {
      loading = false;
    }
  }

  async function getRecommendation() {
    loading = true;
    error = '';
    try {
      const res = await fetch('/api/disks/recommend', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ disks: selectedDisks })
      });
      recommendation = await parseJsonSafe<Recommendation>(res, 'Getting recommendation');
      // Default the custom topology to the recommendation so the UI reflects
      // the advised choice until the user overrides it.
      customTopology = recommendation?.topology || 'single';
    } catch (e) {
      error = 'Failed to get recommendation: ' + e;
    } finally {
      loading = false;
    }
  }

  function toggleDisk(device: string) {
    if (selectedDisks.includes(device)) {
      selectedDisks = selectedDisks.filter((d) => d !== device);
      wipeConfirmed = wipeConfirmed.filter((d) => d !== device);
    } else {
      selectedDisks = [...selectedDisks, device];
    }
  }

  // A disk with a filesystem signature must be confirmed individually; a
  // `zfs_member` signature is never overridable (it may be a recoverable pool).
  function isZfsMember(disk: Disk) {
    const sigs = disk.signatures || [];
    return sigs.includes('zfs') || sigs.includes('zfs_member');
  }

  function needsWipeConfirm(disk: Disk) {
    return !isZfsMember(disk) && (disk.signatures || []).length > 0;
  }

  function toggleWipeConfirm(device: string) {
    wipeConfirmed = wipeConfirmed.includes(device)
      ? wipeConfirmed.filter((d) => d !== device)
      : [...wipeConfirmed, device];
  }

  function unconfirmedSelection() {
    return selectedDisks.filter((device) => {
      const disk = disks.find((d) => d.device === device);
      return disk ? needsWipeConfirm(disk) && !wipeConfirmed.includes(device) : false;
    });
  }

  async function createPool() {
    creating = true;
    createResult = '';
    createFailed = false;
    try {
      const res = await fetch('/api/volumes/zfs', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          name: poolName,
          topology: customTopology || recommendation?.topology || 'single',
          disks: selectedDisks,
          cache: cacheDisk || undefined,
          options: {},
          wipeConfirmed
        })
      });
      if (res.ok) {
        createResult = 'Pool created successfully!';
        step = 4;
      } else {
        let detail: string;
        try {
          const data = await parseJsonSafe<{ error?: string }>(res, 'Creating pool');
          detail = data.error || 'Unknown error';
        } catch (e) {
          detail = String(e);
        }
        createResult = 'Error: ' + detail;
        createFailed = true;
      }
    } catch (e) {
      createResult = 'Error: ' + e;
      createFailed = true;
    } finally {
      creating = false;
    }
  }

  function nextStep() {
    // Guard against interaction while an async step is in flight: without
    // this, double-clicking Next on step 1 fires getRecommendation() twice and
    // the responses can race, and clicking Next on step 2 while loading (or
    // after a failed recommendation left recommendation=null) advances to
    // step 3 showing an empty card or a pool form with no topology context.
    if (loading || creating) return;
    if (step === 1) {
      if (selectedDisks.length === 0) return;
      step = 2;
      getRecommendation();
    } else if (step === 2) {
      if (!recommendation) return;
      step = 3;
    }
  }

  function prevStep() {
    step--;
  }

  // Start a clean run: a second pool must not inherit the first pool's disk
  // selection, name, topology or wipe confirmations.
  function createAnotherPool() {
    step = 1;
    selectedDisks = [];
    recommendation = null;
    customTopology = '';
    poolName = '';
    cacheDisk = '';
    wipeConfirmed = [];
    createResult = '';
    createFailed = false;
    error = '';
    void loadDisks();
  }

  onMount(loadDisks);
</script>

<div>
  <!-- Step indicator -->
  <ol class="mb-8 flex items-center justify-center" aria-label="Pool creation steps">
    {#each [1, 2, 3, 4] as s}
      <li class="flex items-center" aria-current={step === s ? 'step' : undefined}>
        <span
          class={`flex h-10 w-10 items-center justify-center rounded-full font-bold ${step >= s ? 'bg-naslos-primary text-white' : 'bg-naslos-border text-gray-500'}`}
          aria-hidden="true"
        >
          {s}
        </span>
        <span class="sr-only">Step {s}</span>
        {#if s < 4}
          <span
            class="mx-1 h-1 w-6 rounded sm:mx-2 sm:w-16"
            class:bg-naslos-primary={step > s}
            class:bg-naslos-border={step <= s}
            aria-hidden="true"
          ></span>
        {/if}
      </li>
    {/each}
  </ol>

  <!-- Step 1: Select Disks -->
  {#if step === 1}
    <div class="card">
      <h2 class="mb-4 text-xl font-bold">Select Disks</h2>
      <p class="mb-6 text-gray-400">
        Choose which disks to use in your ZFS pool. System disks are excluded.
      </p>

      <AsyncState
        loading={loading}
        error={error}
        loadingText="Loading disks..."
        onRetry={loadDisks}
      >
        {#if selectableDisks.length === 0}
          <EmptyState
            icon="hard-drive"
            title="No usable disks found."
            detail="Every disk reported by the node is a system disk or is already in a pool. Add or free a disk, then scan again."
          >
            <svelte:fragment slot="action">
              <Button variant="secondary" on:click={loadDisks}>Scan again</Button>
            </svelte:fragment>
          </EmptyState>
        {:else}
          <div class="space-y-3">
            {#each selectableDisks as disk (disk.device)}
              <!-- in-pool disks are disabled: they already belong to a pool. -->
              {@const blocked = !!disk.inPool || isZfsMember(disk)}
              <div
                class={`rounded-lg border p-4 transition-colors ${blocked ? 'border-naslos-border/50 bg-naslos-border/10 opacity-60' : selectedDisks.includes(disk.device) ? 'border-naslos-primary bg-naslos-primary/10' : 'border-naslos-border'}`}
              >
                <label class={`flex items-center gap-4 ${blocked ? 'cursor-not-allowed' : 'cursor-pointer'}`}>
                  <input
                    type="checkbox"
                    checked={selectedDisks.includes(disk.device)}
                    disabled={blocked}
                    on:change={() => toggleDisk(disk.device)}
                    class="h-5 w-5 rounded"
                  />
                  <span class="min-w-0 flex-1">
                    <span class="flex flex-wrap items-center gap-2">
                      <span class="font-mono font-medium">{disk.device}</span>
                      {#if disk.inPool}
                        <span
                          class="badge badge-warn"
                          title="Already a member of pool '{disk.inPool}'"
                        >
                          in pool: {disk.inPool}
                        </span>
                      {/if}
                      {#if isZfsMember(disk)}
                        <span
                          class="badge badge-bad"
                          title="This disk holds a ZFS pool label; import or destroy that pool first"
                        >
                          holds a ZFS pool
                        </span>
                      {:else if needsWipeConfirm(disk)}
                        <span
                          class="badge badge-warn"
                          title="Selected disks with existing data must be confirmed for erasure"
                        >
                          contains {(disk.signatures || []).join(', ')}
                        </span>
                      {/if}
                    </span>
                    <span class="block text-sm text-gray-400">
                      <span class="font-mono">{formatBytes(disk.size)}</span>
                      {disk.model ? ` · ${disk.model}` : ''}
                    </span>
                  </span>
                  <span class="badge badge-neutral">{disk.type || 'disk'}</span>
                </label>
                {#if needsWipeConfirm(disk) && selectedDisks.includes(disk.device)}
                  <label class="mt-3 flex items-center gap-2 text-sm text-amber-300">
                    <input
                      type="checkbox"
                      checked={wipeConfirmed.includes(disk.device)}
                      on:change={() => toggleWipeConfirm(disk.device)}
                      class="h-4 w-4 rounded"
                    />
                    Erase this disk and everything on it
                  </label>
                {/if}
              </div>
            {/each}
          </div>
        {/if}
      </AsyncState>

      <div class="mt-6 flex justify-end">
        <Button on:click={nextStep} disabled={selectedDisks.length === 0 || loading}>
          Next →
        </Button>
      </div>
    </div>
  {/if}

  <!-- Step 2: Review Recommendation -->
  {#if step === 2}
    <div class="card">
      <h2 class="mb-4 text-xl font-bold">Review Recommendation</h2>
      <p class="mb-6 text-gray-400">
        Based on your disk selection, we recommend the following configuration. You can customize
        the topology below.
      </p>

      <AsyncState
        loading={loading}
        error={error}
        loadingText="Generating recommendation..."
        onRetry={getRecommendation}
      >
        {#if recommendation}
          <div class="mb-6">
            <div class="mb-4 flex flex-wrap items-start justify-between gap-4">
              <div>
                <h3 class="font-bold">Recommended Topology</h3>
                <p class="text-gray-400">{recommendation.topology}</p>
              </div>
              <div>
                <h3 class="font-bold">Disks</h3>
                <p class="text-gray-400">{recommendation.disks.length} disks</p>
              </div>
            </div>
            <div class="rounded-lg bg-naslos-border/30 p-4">
              <p class="text-sm">{recommendation.description}</p>
            </div>
          </div>

          <!-- Topology Customization -->
          <div class="mb-6">
            <h3 class="mb-2 font-bold">Customize Topology</h3>
            <p class="mb-3 text-sm text-gray-400">
              Override the recommended topology. The recommended option is pre-selected.
            </p>
            <div class="space-y-2">
              {#each topologyOptions as opt}
                <label
                  class={`flex cursor-pointer items-start gap-3 rounded-lg border p-3 transition-colors ${customTopology === opt.value ? 'border-naslos-primary bg-naslos-primary/10' : 'border-naslos-border hover:border-naslos-accent'}`}
                >
                  <input
                    type="radio"
                    name="topology"
                    value={opt.value}
                    bind:group={customTopology}
                    class="mt-1 h-4 w-4 accent-naslos-primary"
                  />
                  <span class="flex-1">
                    <span class="flex items-center gap-2">
                      <span class="font-medium">{opt.label}</span>
                      {#if opt.value === recommendation.topology}
                        <span class="badge badge-info">Recommended</span>
                      {/if}
                    </span>
                    <span class="block text-sm text-gray-400">{opt.desc}</span>
                  </span>
                </label>
              {/each}
            </div>
          </div>

          <div class="mb-6">
            <h3 class="mb-2 font-bold">Selected Disks</h3>
            <div class="space-y-2">
              {#each selectedDisks as device}
                <div class="flex items-center gap-2 rounded bg-naslos-border/30 p-2">
                  <span class="badge badge-neutral">disk</span>
                  <span class="font-mono">{device}</span>
                </div>
              {/each}
            </div>
          </div>

          <!-- Optional Cache Drive (L2ARC) -->
          <div class="mb-6">
            <h3 class="mb-2 font-bold">Cache Drive (Optional)</h3>
            <p class="mb-3 text-sm text-gray-400">
              Add a fast device (NVMe/SSD) as a read cache (L2ARC) to speed up random reads. Leave
              empty to skip.
            </p>
            <div class="space-y-2">
              <label
                class={`flex cursor-pointer items-center gap-3 rounded-lg border p-3 transition-colors ${cacheDisk === '' ? 'border-naslos-primary bg-naslos-primary/10' : 'border-naslos-border hover:border-naslos-accent'}`}
              >
                <input
                  type="radio"
                  name="cache"
                  value=""
                  bind:group={cacheDisk}
                  class="h-4 w-4 accent-naslos-primary"
                />
                <span class="flex-1">
                  <span class="font-medium">No cache</span>
                  <span class="block text-sm text-gray-400">Standard pool without L2ARC</span>
                </span>
              </label>
              {#each cacheCandidates as disk (disk.device)}
                <label
                  class={`flex cursor-pointer items-center gap-3 rounded-lg border p-3 transition-colors ${cacheDisk === disk.device ? 'border-naslos-primary bg-naslos-primary/10' : 'border-naslos-border hover:border-naslos-accent'}`}
                >
                  <input
                    type="radio"
                    name="cache"
                    value={disk.device}
                    bind:group={cacheDisk}
                    class="h-4 w-4 accent-naslos-primary"
                  />
                  <span class="flex-1">
                    <span class="flex flex-wrap items-center gap-2">
                      <span class="font-mono font-medium">{disk.device}</span>
                      <span class="badge badge-neutral">{disk.type || 'disk'}</span>
                    </span>
                    <span class="block text-sm text-gray-400">
                      {disk.model || 'Unknown'} · {formatBytes(disk.size)}
                    </span>
                  </span>
                </label>
              {/each}
            </div>
          </div>
        {/if}
      </AsyncState>

      <div class="mt-6 flex justify-between">
        <Button variant="secondary" on:click={prevStep}>← Back</Button>
        <Button on:click={nextStep} disabled={loading || !recommendation}>Next →</Button>
      </div>
    </div>
  {/if}

  <!-- Step 3: Pool Name -->
  {#if step === 3}
    <div class="card">
      <h2 class="mb-4 text-xl font-bold">Pool Name</h2>
      <p class="mb-6 text-gray-400">Enter a name for your ZFS pool.</p>

      <form on:submit|preventDefault={createPool} class="mb-6 space-y-4">
        <FormField
          label="Pool Name"
          id="pool-name"
          required
          hint="Lowercase letters, numbers and dashes. Used as the ZFS pool name."
        >
          <input
            id="pool-name"
            type="text"
            bind:value={poolName}
            class="input w-full"
            placeholder="e.g. tank"
            required
          />
        </FormField>

        {#if createFailed && createResult}
          <div role="alert" class="callout callout-bad mb-0">{createResult}</div>
        {/if}

        <div class="flex justify-between">
          <Button variant="secondary" on:click={prevStep}>← Back</Button>
          <Button
            type="submit"
            loading={creating}
            disabled={!poolName.trim() || unconfirmedSelection().length > 0}
          >
            Create Pool
          </Button>
        </div>
      </form>
    </div>
  {/if}

  <!-- Step 4: Result -->
  {#if step === 4}
    <div class="card">
      <h2 class="mb-4 text-xl font-bold">Pool Created</h2>
      <p class="mb-6 text-gray-400">
        Your ZFS pool has been created with the selected configuration.
      </p>

      {#if createResult}
        <div role="status" class="callout callout-ok mb-6">{createResult}</div>
      {/if}

      <div class="flex justify-end">
        <Button on:click={createAnotherPool}>Create Another Pool</Button>
      </div>
    </div>
  {/if}
</div>
