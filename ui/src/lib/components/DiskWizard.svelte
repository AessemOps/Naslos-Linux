<script lang="ts">
  import { onMount } from 'svelte';

  interface Disk {
    device: string;
    size: number;
    isSystemDisk: boolean;
    model: string;
    serial: string;
    type: string;
    inPool?: string; // name of the pool this disk already belongs to (if any)
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
  let creating = false;
  let createResult = '';

  // Available topologies with descriptions for the customization UI.
  const topologyOptions = [
    { value: 'single', label: 'Single', desc: 'No redundancy. All disks combined into one pool. If any disk fails, all data is lost.' },
    { value: 'mirror', label: 'Mirror', desc: 'Each disk has an exact copy. Survives one disk failure per mirror pair. Best performance for reads.' },
    { value: 'raidz1', label: 'RAIDZ1', desc: 'One disk parity. Survives one disk failure. Good balance of capacity and redundancy for 3-5 disks.' },
    { value: 'raidz2', label: 'RAIDZ2', desc: 'Two disk parity. Survives two disk failures. Recommended for larger pools (6+ disks).' },
    { value: 'raidz3', label: 'RAIDZ3', desc: 'Three disk parity. Survives three disk failures. Maximum redundancy for large pools.' },
  ];

  async function parseJsonSafe(res: Response, what: string) {
    // Read the body once as text so we can produce a readable error when the
    // server answers with something that is not JSON. Without this guard, a
    // proxy misroute (e.g. nginx serving index.html for /api/* with HTTP 200)
    // surfaces as a cryptic "SyntaxError: JSON.parse: unexpected character
    // at line 1 column 1 of the JSON data".
    const text = await res.text();
    if (!res.ok) {
      throw new Error(`${what} failed: HTTP ${res.status} — ${text.slice(0, 200)}`);
    }
    const contentType = res.headers.get('content-type') || '';
    if (!contentType.includes('application/json')) {
      throw new Error(
        `${what} failed: expected JSON but got "${contentType || 'unknown content type'}" — ${text.slice(0, 200)}`
      );
    }
    try {
      return JSON.parse(text);
    } catch (e) {
      throw new Error(`${what} failed: invalid JSON response — ${text.slice(0, 200)}`);
    }
  }

  async function loadDisks() {
    loading = true;
    error = '';
    disks = [];
    try {
      const res = await fetch('/api/disks');
      const data = await parseJsonSafe(res, 'Loading disks');
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
      recommendation = await parseJsonSafe(res, 'Getting recommendation');
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
      selectedDisks = selectedDisks.filter(d => d !== device);
    } else {
      selectedDisks = [...selectedDisks, device];
    }
  }

  async function createPool() {
    creating = true;
    createResult = '';
    try {
      const res = await fetch('/api/volumes/zfs', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          name: poolName,
          topology: customTopology || recommendation?.topology || 'single',
          disks: selectedDisks,
          options: {}
        })
      });
      if (res.ok) {
        createResult = 'Pool created successfully!';
        step = 4;
      } else {
        let detail: string;
        try {
          const data = await parseJsonSafe(res, 'Creating pool');
          detail = data.error || 'Unknown error';
        } catch (e) {
          detail = String(e);
        }
        createResult = 'Error: ' + detail;
      }
    } catch (e) {
      createResult = 'Error: ' + e;
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

  onMount(loadDisks);

  function formatSize(bytes: number): string {
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    let i = 0;
    let size = bytes;
    while (size >= 1024 && i < units.length - 1) {
      size /= 1024;
      i++;
    }
    return `${size.toFixed(1)} ${units[i]}`;
  }
</script>

<div class="max-w-4xl mx-auto">
  <!-- Step indicator -->
  <div class="flex items-center justify-center mb-8">
    {#each [1, 2, 3, 4] as s}
      <div class="flex items-center">
        <div class={`w-10 h-10 rounded-full flex items-center justify-center font-bold ${step >= s ? 'bg-naslos-primary text-white' : 'bg-naslos-border text-gray-500'}`}>
          {s}
        </div>
        {#if s < 4}
          <div class="w-16 h-1 mx-2 rounded" class:bg-naslos-primary={step > s} class:bg-naslos-border={step <= s}></div>
        {/if}
      </div>
    {/each}
  </div>

  <!-- Step 1: Select Disks -->
  {#if step === 1}
    <div class="card">
      <h2 class="text-xl font-bold mb-4">Select Disks</h2>
      <p class="text-gray-400 mb-6">Choose which disks to use in your ZFS pool. System disks are excluded.</p>

      {#if loading}
        <p class="text-gray-400">Loading disks...</p>
      {:else if error}
        <p class="text-red-400">{error}</p>
      {:else}
        <div class="space-y-3">
          {#each disks.filter(d => !d.isSystemDisk) as disk}
            <!-- in-pool disks are disabled: they already belong to a pool. -->
            <label
              class={`flex items-center gap-4 p-4 rounded-lg border transition-colors ${disk.inPool ? 'border-naslos-border/50 bg-naslos-border/10 opacity-60 cursor-not-allowed' : selectedDisks.includes(disk.device) ? 'border-naslos-primary bg-naslos-primary/10 cursor-pointer' : 'border-naslos-border hover:border-naslos-accent cursor-pointer'}`}
            >
              <input
                type="checkbox"
                checked={selectedDisks.includes(disk.device)}
                disabled={!!disk.inPool}
                on:change={() => toggleDisk(disk.device)}
                class="w-5 h-5 rounded"
              />
              <div class="flex-1">
                <div class="flex items-center gap-2">
                  <span class="font-medium">{disk.device}</span>
                  {#if disk.inPool}
                    <span class="text-xs px-2 py-0.5 rounded bg-yellow-900/50 text-yellow-400" title="Already a member of pool '{disk.inPool}'">in pool: {disk.inPool}</span>
                  {/if}
                </div>
                <div class="text-sm text-gray-400">
                  {formatSize(disk.size)} {disk.model ? `• ${disk.model}` : ''}
                </div>
              </div>
              <span class="text-xs px-2 py-1 rounded bg-naslos-border text-gray-300">
                {disk.type}
              </span>
            </label>
          {/each}
        </div>
      {/if}

      <div class="flex justify-end mt-6">
        <button class="btn btn-primary" on:click={nextStep} disabled={selectedDisks.length === 0}>
          Next →
        </button>
      </div>
    </div>
  {/if}

  <!-- Step 2: Review Recommendation -->
  {#if step === 2}
    <div class="card">
      <h2 class="text-xl font-bold mb-4">Review Recommendation</h2>
      <p class="text-gray-400 mb-6">Based on your disk selection, we recommend the following configuration. You can customize the topology below.</p>

      {#if loading}
        <p class="text-gray-400">Generating recommendation...</p>
      {:else if error}
        <p class="text-red-400">{error}</p>
      {:else if recommendation}
        <div class="mb-6">
          <div class="flex items-center justify-between mb-4">
            <div>
              <h3 class="font-bold">Recommended Topology</h3>
              <p class="text-gray-400">{recommendation.topology}</p>
            </div>
            <div>
              <h3 class="font-bold">Disks</h3>
              <p class="text-gray-400">{recommendation.disks.length} disks</p>
            </div>
          </div>
          <div class="bg-naslos-border/30 p-4 rounded-lg">
            <p class="text-sm">{recommendation.description}</p>
          </div>
        </div>

        <!-- Topology Customization -->
        <div class="mb-6">
          <h3 class="font-bold mb-2">Customize Topology</h3>
          <p class="text-sm text-gray-400 mb-3">Override the recommended topology. The recommended option is pre-selected.</p>
          <div class="space-y-2">
            {#each topologyOptions as opt}
              <label class={`flex items-start gap-3 p-3 rounded-lg border transition-colors cursor-pointer ${customTopology === opt.value ? 'border-naslos-primary bg-naslos-primary/10' : 'border-naslos-border hover:border-naslos-accent'}`}>
                <input
                  type="radio"
                  name="topology"
                  value={opt.value}
                  bind:group={customTopology}
                  class="mt-1 w-4 h-4 accent-naslos-primary"
                />
                <div class="flex-1">
                  <div class="flex items-center gap-2">
                    <span class="font-medium">{opt.label}</span>
                    {#if opt.value === recommendation.topology}
                      <span class="text-xs px-2 py-0.5 rounded bg-naslos-primary/20 text-naslos-primary">Recommended</span>
                    {/if}
                  </div>
                  <p class="text-sm text-gray-400">{opt.desc}</p>
                </div>
              </label>
            {/each}
          </div>
        </div>

        <div class="mb-6">
          <h3 class="font-bold mb-2">Selected Disks</h3>
          <div class="space-y-2">
            {#each selectedDisks as device}
              <div class="flex items-center gap-2 p-2 bg-naslos-border/30 rounded">
                <span class="text-xs px-2 py-1 rounded bg-naslos-border text-gray-300">disk</span>
                <span>{device}</span>
              </div>
            {/each}
          </div>
        </div>
      {/if}

      <div class="flex justify-between mt-6">
        <button class="btn btn-secondary" on:click={prevStep}>← Back</button>
        <button class="btn btn-primary" on:click={nextStep} disabled={loading || !recommendation}>Next →</button>
      </div>
    </div>
  {/if}

  <!-- Step 3: Pool Name -->
  {#if step === 3}
    <div class="card">
      <h2 class="text-xl font-bold mb-4">Pool Name</h2>
      <p class="text-gray-400 mb-6">Enter a name for your ZFS pool.</p>

      <div class="mb-6">
        <label class="label" for="pool-name">Pool Name</label>
        <input id="pool-name" type="text" bind:value={poolName} class="input w-full" placeholder="e.g. tank" />
        <p class="text-xs text-gray-500 mt-1">Lowercase letters, numbers and dashes. Used as the ZFS pool name.</p>
      </div>

      <div class="flex justify-between mt-6">
        <button class="btn btn-secondary" on:click={prevStep}>← Back</button>
        <button class="btn btn-primary" on:click={createPool} disabled={!poolName.trim() || creating}>
          {creating ? 'Creating…' : 'Create Pool'}
        </button>
      </div>
    </div>
  {/if}

  <!-- Step 4: Result -->
  {#if step === 4}
    <div class="card">
      <h2 class="text-xl font-bold mb-4">Pool Created</h2>
      <p class="text-gray-400 mb-6">Your ZFS pool has been created with the selected configuration.</p>

      {#if createResult}
        <div class="mb-6 p-4 bg-naslos-border/30 rounded-lg"><p>{createResult}</p></div>
      {/if}

      <div class="flex justify-end mt-6">
        <button class="btn btn-primary" on:click={() => step = 1}>Create Another Pool</button>
      </div>
    </div>
  {/if}
</div>
