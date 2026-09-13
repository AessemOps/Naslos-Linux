<script lang="ts">
  import { onMount, createEventDispatcher } from 'svelte';

  interface Share {
    name: string;
    path: string;
    protocol: string;
    description: string;
    readOnly: boolean;
    browseable: boolean;
    allowedHosts: string[];
    validUsers: string[];
    timeMachine: boolean;
    enabled: boolean;
  }

  export let share: Share | null = null;

  const dispatch = createEventDispatcher();

  let name = '';
  let path = '';
  let protocol = 'smb';
  let description = '';
  let readOnly = false;
  let browseable = true;
  let timeMachine = false;
  let allowedHostsStr = '';
  let validUsersStr = '';
  let saving = false;
  let error = '';
  let availablePaths: string[] = [];

  async function loadPaths() {
    try {
      const res = await fetch('/api/volumes/zfs');
      if (res.ok) {
        const pools = await res.json();
        availablePaths = pools.map((p: any) => `/var/mnt/${p.name}`);
      }
    } catch (e) {
      // Ignore
    }
  }

  function init() {
    if (share) {
      name = share.name;
      path = share.path;
      protocol = share.protocol;
      description = share.description;
      readOnly = share.readOnly;
      browseable = share.browseable;
      timeMachine = share.timeMachine;
      allowedHostsStr = share.allowedHosts?.join(', ') || '';
      validUsersStr = share.validUsers?.join(', ') || '';
    }
  }

  async function save() {
    saving = true;
    error = '';

    const body = {
      name,
      path,
      protocol,
      description,
      readOnly,
      browseable,
      timeMachine,
      allowedHosts: allowedHostsStr ? allowedHostsStr.split(',').map(s => s.trim()) : [],
      validUsers: validUsersStr ? validUsersStr.split(',').map(s => s.trim()) : []
    };

    try {
      const url = share ? `/api/shares/${share.name}` : '/api/shares';
      const method = share ? 'PUT' : 'POST';
      const res = await fetch(url, {
        method,
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body)
      });
      if (!res.ok) {
        const data = await res.json();
        throw new Error(data.error || 'Failed to save share');
      }
      dispatch('close');
    } catch (e: any) {
      error = e.message;
    } finally {
      saving = false;
    }
  }

  onMount(() => {
    loadPaths();
    init();
  });
</script>

<div class="fixed inset-0 bg-black/60 z-50 flex items-center justify-center p-4" on:click|self={() => dispatch('close')}>
  <div class="bg-naslos-surface rounded-2xl border border-naslos-border w-full max-w-lg max-h-[90vh] overflow-hidden flex flex-col">
    <div class="p-6 border-b border-naslos-border flex items-center justify-between">
      <h2 class="text-xl font-bold">{share ? 'Edit Share' : 'New Share'}</h2>
      <button class="text-gray-400 hover:text-white text-2xl" on:click={() => dispatch('close')}>×</button>
    </div>

    <div class="flex-1 overflow-y-auto p-6 space-y-4">
      <div>
        <label class="label">Share Name *</label>
        <input type="text" bind:value={name} placeholder="e.g. media, documents" class="input w-full" disabled={!!share} />
      </div>

      <div>
        <label class="label">Path *</label>
        <select bind:value={path} class="input w-full">
          <option value="">Select a path...</option>
          {#each availablePaths as p}
            <option value={p}>{p}</option>
          {/each}
        </select>
        <p class="text-xs text-gray-500 mt-1">ZFS dataset path under /var/mnt</p>
      </div>

      <div>
        <label class="label">Protocol *</label>
        <select bind:value={protocol} class="input w-full">
          <option value="smb">SMB/CIFS (Windows, macOS, Linux)</option>
          <option value="nfs">NFS (Linux, macOS)</option>
        </select>
        <p class="text-xs text-gray-500 mt-1">Time Machine is served over SMB; AFP is not supported.</p>
      </div>

      <div>
        <label class="label">Description</label>
        <input type="text" bind:value={description} placeholder="Optional description" class="input w-full" />
      </div>

      <div class="flex items-center gap-6">
        <label class="flex items-center gap-3 cursor-pointer">
          <input type="checkbox" bind:checked={readOnly} class="w-5 h-5 rounded" />
          <span class="text-gray-300">Read Only</span>
        </label>
        <label class="flex items-center gap-3 cursor-pointer">
          <input type="checkbox" bind:checked={browseable} class="w-5 h-5 rounded" />
          <span class="text-gray-300">Browseable</span>
        </label>
      </div>

      {#if protocol === 'smb'}
        <label class="flex items-center gap-3 cursor-pointer">
          <input type="checkbox" bind:checked={timeMachine} class="w-5 h-5 rounded" />
          <span class="text-gray-300">Time Machine (macOS backup)</span>
        </label>
      {/if}

      <div>
        <label class="label">Allowed Hosts</label>
        <input type="text" bind:value={allowedHostsStr} placeholder="e.g. 192.168.1.0/24, 10.0.0.5 (empty = all)" class="input w-full" />
      </div>

      <div>
        <label class="label">Valid Users</label>
        <input type="text" bind:value={validUsersStr} placeholder="e.g. user1, user2 (empty = all)" class="input w-full" />
      </div>

      {#if error}<div class="p-3 rounded-lg bg-red-900/30 border border-red-700 text-red-300 text-sm">{error}</div>{/if}
    </div>

    <div class="p-6 border-t border-naslos-border flex justify-end gap-3">
      <button class="btn btn-secondary" on:click={() => dispatch('close')}>Cancel</button>
      <button class="btn btn-primary" on:click={save} disabled={saving || !name || !path}>
        {saving ? 'Saving...' : share ? 'Update' : 'Create'}
      </button>
    </div>
  </div>
</div>
