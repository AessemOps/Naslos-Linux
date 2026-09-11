<script lang="ts">
  import { onMount } from 'svelte';

  interface Disk {
    device: string;
    size: number;
    isSystemDisk: boolean;
    model: string;
    serial: string;
    type: string;
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
  let poolName = '';
  let creating = false;
  let createResult = '';

  async function loadDisks() {
    loading = true;
    try {
      const res = await fetch('/api/disks');
      disks = await res.json();
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
      recommendation = await res.json();
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
          topology: recommendation?.topology || 'single',
          disks: selectedDisks,
          options: {}
        })
      });
      if (res.ok) {
        createResult = 'Pool created successfully!';
        step = 4;
      } else {
        const data = await res.json();
        createResult = 'Error: ' + (data.error || 'Unknown error');
      }
    } catch (e) {
      createResult = 'Error: ' + e;
    } finally {
      creating = false;
    }
  }

  function nextStep() {
    if (step === 1 && selectedDisks.length > 0) {
      getRecommendation();
    }
    step++;
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
            <label
              class={`flex items-center gap-4 p-4 rounded-lg border cursor-pointer transition-colors ${selectedDisks.includes(disk.device) ? 'border-naslos-primary bg-naslos-primary/10' : 'border-naslos-border hover:border-naslos-accent'}`}
            >
              <input
                type="checkbox"
                checked={selectedDisks.includes(disk.device)}
                on:change={() => toggleDisk(disk.device)}
                class="w-5 h-5 rounded"
              />
              <div class="flex-1">
                <div class="font-medium">{disk.device}</div>
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
      <p class="text-gray-400 mb-6">Based on your disk selection, we recommend the following configuration.</p>

      {#if loading}
        <p class="text-gray-400">Generating recommendation...</p>
      {:else if error}
        <p class="text-red-400">{error}</p>
      {:else if recommendation}
        <div class="mb-6">
          <div class="flex items-center justify-between mb-4">
            <div>
              <h3 class="font-bold">Topology</h3>
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
        <button class="btn btn-primary" on:click={nextStep}>Next →</button>
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
