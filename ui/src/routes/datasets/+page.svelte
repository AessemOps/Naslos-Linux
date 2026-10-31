<script lang="ts">
  import { onMount } from 'svelte';

  interface ManagedDataset {
    name: string;
    hostPath: string;
    shared: boolean;
    warningAcknowledged: boolean;
    createdAt?: string;
    attachedTo?: string[];
  }

  interface ZfsDataset {
    name: string;
    mountpoint: string;
    mounted: boolean;
  }

  let managed: ManagedDataset[] = [];
  let zfs: ZfsDataset[] = [];
  let loading = true;
  let saving = false;
  let error = '';
  let success = '';

  let selectedPath = '';
  let name = '';
  let shared = false;
  let acknowledged = false;

  $: available = zfs
    .filter((d) => d.mounted && d.mountpoint && d.mountpoint.startsWith('/var/mnt/'))
    .sort((a, b) => a.mountpoint.localeCompare(b.mountpoint));

  function suggestName(path: string) {
    const part = path.split('/').filter(Boolean).pop() || '';
    return part
      .toLowerCase()
      .replace(/[^a-z0-9-]+/g, '-')
      .replace(/^-+|-+$/g, '')
      .slice(0, 63);
  }

  function pick(path: string) {
    selectedPath = path;
    if (!name) name = suggestName(path);
  }

  async function load() {
    loading = true;
    error = '';
    try {
      const [registryRes, zfsRes] = await Promise.all([
        fetch('/api/datasets/registry'),
        fetch('/api/datasets')
      ]);
      if (registryRes.ok) managed = await registryRes.json();
      else error = `Failed to load registered datasets (HTTP ${registryRes.status})`;
      if (zfsRes.ok) zfs = await zfsRes.json();
    } catch {
      error = 'Failed to connect to the API';
    } finally {
      loading = false;
    }
  }

  async function save() {
    error = '';
    success = '';
    if (!name || !selectedPath) {
      error = 'Choose a ZFS dataset and a name.';
      return;
    }
    if (shared && !acknowledged) {
      error = 'Acknowledge the shared-dataset warning first.';
      return;
    }
    saving = true;
    try {
      const res = await fetch('/api/datasets/registry', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          name,
          hostPath: selectedPath,
          shared,
          warningAcknowledged: shared ? acknowledged : false
        })
      });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) throw new Error(data.error || `Registration failed (HTTP ${res.status})`);
      success = `Registered ${data.name}.`;
      name = '';
      selectedPath = '';
      shared = false;
      acknowledged = false;
      await load();
    } catch (e: any) {
      error = e.message;
    } finally {
      saving = false;
    }
  }

  async function remove(datasetName: string) {
    if (!confirm(`Unregister ${datasetName}? Apps already using it keep their claim.`)) return;
    error = '';
    success = '';
    try {
      const res = await fetch(`/api/datasets/registry/${encodeURIComponent(datasetName)}`, {
        method: 'DELETE'
      });
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        throw new Error(data.error || `Delete failed (HTTP ${res.status})`);
      }
      success = `Unregistered ${datasetName}.`;
      await load();
    } catch (e: any) {
      error = e.message;
    }
  }

  onMount(load);
</script>

<div class="max-w-5xl mx-auto">
  <h1 class="text-3xl font-bold mb-2">Datasets</h1>
  <p class="text-gray-400 mb-8">
    Register the ZFS datasets an installed app may mount. Apps never get the
    whole <span class="font-mono">/var/mnt</span> tree.
  </p>

  {#if error}<div role="alert" class="card border-red-700 bg-red-900/20 text-red-300 p-4 mb-4">{error}</div>{/if}
  {#if success}<div role="status" class="card border-green-700 bg-green-900/20 text-green-200 p-4 mb-4">{success}</div>{/if}

  <div class="card mb-8">
    <h2 class="text-xl font-bold mb-4">Register a dataset</h2>
    <div class="grid gap-4 md:grid-cols-2">
      <div>
        <label class="label" for="dataset-path">ZFS dataset</label>
        <select id="dataset-path" class="input w-full" bind:value={selectedPath} on:change={() => pick(selectedPath)}>
          <option value="">Select a mounted dataset…</option>
          {#each available as dataset (dataset.mountpoint)}
            <option value={dataset.mountpoint}>{dataset.name} — {dataset.mountpoint}</option>
          {/each}
        </select>
      </div>
      <div>
        <label class="label" for="dataset-name">Name</label>
        <input id="dataset-name" class="input w-full" bind:value={name} placeholder="e.g. media" />
      </div>
    </div>

    <label class="flex items-start gap-3 cursor-pointer mt-4">
      <input type="checkbox" class="mt-1" bind:checked={shared} />
      <span>Share this dataset with multiple apps</span>
    </label>

    {#if shared}
      <div role="alert" class="mt-4 p-4 rounded-lg border border-red-700 bg-red-900/30 text-red-200 text-sm">
        <p class="font-medium mb-1">Shared datasets have no isolation</p>
        <p>
          Every app attached to this dataset can read, modify and delete all of
          its data. Only share a dataset that is meant to be common to those apps
          (for example a media library). Private datasets should be registered
          without this option and attached to one app.
        </p>
      </div>
      <label class="flex items-start gap-3 cursor-pointer mt-3">
        <input type="checkbox" class="mt-1" bind:checked={acknowledged} />
        <span class="font-medium">I understand that every attached app can read and modify this dataset.</span>
      </label>
    {/if}

    <div class="flex justify-end mt-6">
      <button class="btn btn-primary" on:click={save} disabled={saving || (shared && !acknowledged)}>
        {saving ? 'Registering…' : 'Register dataset'}
      </button>
    </div>
  </div>

  <h2 class="text-xl font-bold mb-4">Registered datasets</h2>
  {#if loading}
    <p class="text-gray-400">Loading…</p>
  {:else if managed.length === 0}
    <div class="card text-gray-400">No datasets are registered yet.</div>
  {:else}
    <div class="space-y-3">
      {#each managed as dataset (dataset.name)}
        <div class="card flex flex-wrap items-center justify-between gap-3">
          <div class="min-w-0">
            <div class="flex items-center gap-2 flex-wrap">
              <h3 class="font-bold">{dataset.name}</h3>
              {#if dataset.shared}
                <span class="text-xs px-2 py-0.5 rounded bg-amber-900/50 text-amber-300">Shared</span>
              {:else}
                <span class="text-xs px-2 py-0.5 rounded bg-gray-800 text-gray-300">Private</span>
              {/if}
            </div>
            <p class="text-sm text-gray-400 font-mono truncate">{dataset.hostPath}</p>
            {#if dataset.attachedTo?.length}
              <p class="text-xs text-gray-500 mt-1">Attached to {dataset.attachedTo.join(', ')}</p>
            {/if}
          </div>
          <button class="btn btn-danger" on:click={() => remove(dataset.name)}>Unregister</button>
        </div>
      {/each}
    </div>
  {/if}
</div>
