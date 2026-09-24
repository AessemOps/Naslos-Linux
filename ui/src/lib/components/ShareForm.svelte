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
    validGroups: string[];
    timeMachine: boolean;
    noRootSquash?: boolean;
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
  let noRootSquash = false;
  let allowedHostsStr = '';
  let validUsersStr = '';
  let validGroups: string[] = [];
  let availableGroups: string[] = [];
  let saving = false;
  let error = '';
  let availablePaths: string[] = [];

  // Folder picker state. A share is served from a FOLDER on a dataset, and any
  // subfolder of a dataset is valid (FR-SHR-01), so the operator can descend
  // into the tree or create a new folder to link the share to.
  let baseDir = '';            // dataset mountpoint the browsing is confined to
  let browseDir = '';          // folder the share will point at == `path`
  let subfolders: string[] = [];
  let folderBusy = false;
  let folderError = '';
  let newFolderName = '';

  // LDAP groups available to grant share access to. Access is evaluated by
  // Samba on the node, so the group has to exist there (mirrored into NSS) for
  // this to work - hence a picker rather than free text.
  async function loadGroups() {
    try {
      const res = await fetch('/api/groups');
      if (!res.ok) return;
      const data = await res.json();
      availableGroups = (Array.isArray(data) ? data : [])
        .map((g: any) => g.cn || g.name)
        .filter(Boolean);
    } catch (e) {
      // Ignore: the picker stays empty and access can still be typed manually.
    }
  }

  function toggleGroup(group: string) {
    validGroups = validGroups.includes(group)
      ? validGroups.filter(g => g !== group)
      : [...validGroups, group];
  }

  // The datasets a share may live on. Only dataset mountpoints are offered: a
  // plain directory under /var/mnt would put the share's data outside the pool.
  async function loadPaths() {
    try {
      const res = await fetch('/api/shares/paths');
      if (!res.ok) return;

      const data = await res.json();
      availablePaths = Array.isArray(data.paths) ? data.paths : [];

      // Root the browsing at the dataset that holds the current path (set by
      // init() for an existing share), otherwise at the first dataset.
      if (!baseDir) baseDir = browseDir ? datasetFor(browseDir) : availablePaths[0] || '';
      browseDir = browseDir || baseDir;
      if (!path) path = browseDir;

      await loadSubfolders();
    } catch (e) {
      // Ignore: the picker stays empty and the error surfaces on save.
    }
  }

  // Dataset that contains dir, longest match first (datasets can be nested).
  function datasetFor(dir: string): string {
    let best = '';
    for (const p of availablePaths) {
      if ((dir === p || dir.startsWith(p + '/')) && p.length > best.length) best = p;
    }
    return best;
  }

  async function loadSubfolders() {
    if (!browseDir) {
      subfolders = [];
      return;
    }
    try {
      const res = await fetch(`/api/shares/folders?path=${encodeURIComponent(browseDir)}`);
      const data = await res.json();
      subfolders = res.ok && Array.isArray(data.folders) ? data.folders : [];
      if (!res.ok) folderError = data.error || 'Could not list folders';
    } catch (e: any) {
      subfolders = [];
      folderError = e.message;
    }
  }

  async function enterFolder(name: string) {
    folderError = '';
    browseDir = `${browseDir}/${name}`;
    path = browseDir;
    await loadSubfolders();
  }

  async function goUp() {
    if (!baseDir || browseDir === baseDir) return;
    folderError = '';
    const parent = browseDir.split('/').slice(0, -1).join('/');
    browseDir = parent.length < baseDir.length ? baseDir : parent;
    path = browseDir;
    await loadSubfolders();
  }

  function selectDataset(dir: string) {
    folderError = '';
    baseDir = dir;
    browseDir = dir;
    path = dir;
    loadSubfolders();
  }

  // The dataset <select> hands over an Event; the type assertion belongs here
  // rather than inline (Svelte templates do not accept TS casts).
  function onDatasetChange(e: Event) {
    const select = e.currentTarget as HTMLSelectElement;
    selectDataset(select.value);
  }

  // Create a folder inside the current location and point the share at it.
  async function createFolder() {
    const name = newFolderName.trim();
    if (!name || !browseDir) return;

    folderBusy = true;
    folderError = '';
    try {
      const res = await fetch('/api/shares/folders', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ path: browseDir, name })
      });
      const data = await res.json();
      if (!res.ok) throw new Error(data.error || 'Could not create folder');

      // Link the share to the folder that was just created.
      browseDir = data.path;
      path = data.path;
      newFolderName = '';
      await loadSubfolders();
    } catch (e: any) {
      folderError = e.message;
    } finally {
      folderBusy = false;
    }
  }

  function init() {
    if (share) {
      name = share.name;
      path = share.path;
      browseDir = share.path;
      // For an existing share the browsing root is the dataset holding it, so
      // "up" stops at the dataset instead of walking outside the pool.
      baseDir = datasetFor(share.path);
      protocol = share.protocol;
      description = share.description;
      readOnly = share.readOnly;
      browseable = share.browseable;
      timeMachine = share.timeMachine;
      noRootSquash = share.noRootSquash ?? false;
      allowedHostsStr = share.allowedHosts?.join(', ') || '';
      validUsersStr = share.validUsers?.join(', ') || '';
      validGroups = [...(share.validGroups || [])];
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
      noRootSquash,
      allowedHosts: allowedHostsStr ? allowedHostsStr.split(',').map(s => s.trim()) : [],
      validUsers: validUsersStr ? validUsersStr.split(',').map(s => s.trim()) : [],
      validGroups
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
    // init() first: it sets the share's path (and browseDir) so loadPaths can
    // root the folder picker at the dataset that holds it.
    init();
    loadPaths();
    loadGroups();
  });
</script>

<div class="fixed inset-0 bg-black/60 z-50 flex items-center justify-center p-4" role="presentation" on:click|self={() => dispatch('close')}>
  <div class="bg-naslos-surface rounded-2xl border border-naslos-border w-full max-w-lg max-h-[90vh] overflow-hidden flex flex-col">
    <div class="p-6 border-b border-naslos-border flex items-center justify-between">
      <h2 class="text-xl font-bold">{share ? 'Edit Share' : 'New Share'}</h2>
      <button class="text-gray-400 hover:text-white text-2xl" on:click={() => dispatch('close')}>×</button>
    </div>

    <div class="flex-1 overflow-y-auto p-6 space-y-4">
      <div>
        <label class="label" for="share-field-1">Share Name *</label>
        <input id="share-field-1" type="text" bind:value={name} placeholder="e.g. media, documents" class="input w-full" disabled={!!share} />
      </div>

      <div>
        <label class="label" for="share-dataset">Path *</label>

        <!-- A share is served from a folder on a dataset. The dropdown picks the
             dataset (the only thing that guarantees the data is in the pool);
             the listing below descends into it, and any subfolder - existing or
             newly created - can be the share's path. -->
        <select
          id="share-dataset"
          class="input w-full"
          value={baseDir}
          on:change={onDatasetChange}
        >
          <option value="">Select a dataset...</option>
          {#each availablePaths as p}
            <option value={p}>{p}</option>
          {/each}
        </select>

        {#if browseDir}
          <div class="mt-2 p-3 rounded-lg border border-naslos-border bg-naslos-dark">
            <div class="flex items-center justify-between gap-2">
              <code class="text-xs text-naslos-primary break-all">{browseDir}</code>
              <button
                type="button"
                class="text-xs px-2 py-1 rounded border border-naslos-border text-gray-300 hover:text-white hover:bg-naslos-border disabled:opacity-40 disabled:cursor-not-allowed"
                on:click={goUp}
                disabled={browseDir === baseDir}
                title="Go up one folder (stops at the dataset)"
              >↑ Up</button>
            </div>

            {#if subfolders.length > 0}
              <div class="flex flex-wrap gap-2 mt-3">
                {#each subfolders as folder}
                  <button
                    type="button"
                    class="text-xs px-2 py-1 rounded border border-naslos-border text-gray-300 hover:text-white hover:bg-naslos-border"
                    on:click={() => enterFolder(folder)}
                    title="Use this folder for the share"
                  >📁 {folder}</button>
                {/each}
              </div>
            {:else}
              <p class="text-xs text-gray-500 mt-3">No folders inside this one yet.</p>
            {/if}

            <div class="flex gap-2 mt-3">
              <input
                type="text"
                bind:value={newFolderName}
                on:keydown={(e) => { if (e.key === 'Enter') { e.preventDefault(); createFolder(); } }}
                placeholder="New folder name"
                class="input flex-1"
              />
              <button
                type="button"
                class="btn btn-secondary"
                on:click={createFolder}
                disabled={folderBusy || !newFolderName.trim()}
              >{folderBusy ? 'Creating...' : 'Create folder'}</button>
            </div>
            {#if folderError}<p class="text-xs text-red-400 mt-2">{folderError}</p>{/if}
          </div>
        {/if}

        <p class="text-xs text-gray-500 mt-1">
          The share is served from the folder shown above. Create a folder to keep this share separate
          from the rest of the dataset, or select an existing one.
        </p>
      </div>

      <div>
        <label class="label" for="share-field-2">Protocol *</label>
        <select id="share-field-2" bind:value={protocol} class="input w-full">
          <option value="smb">SMB/CIFS (Windows, macOS, Linux)</option>
          <option value="nfs">NFS (Linux, macOS)</option>
        </select>
        <p class="text-xs text-gray-500 mt-1">Time Machine is served over SMB; AFP is not supported.</p>
      </div>

      <div>
        <label class="label" for="share-field-3">Description</label>
        <input id="share-field-3" type="text" bind:value={description} placeholder="Optional description" class="input w-full" />
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

      {#if protocol === 'nfs'}
        <div>
          <label class="flex items-center gap-3 cursor-pointer">
            <input type="checkbox" bind:checked={noRootSquash} class="w-5 h-5 rounded" />
            <span class="text-gray-300">Allow root clients to write as root (disable root squash)</span>
          </label>
          {#if noRootSquash}
            <p class="text-xs text-amber-400 mt-1">
              Warning: a root client on an allowed host can read and write every file as root.
              Leave this off unless the share's data is root-owned and an admin client must write it.
            </p>
          {/if}
        </div>
      {/if}

      <div>
        <label class="label" for="share-field-4">Allowed Hosts</label>
        <input id="share-field-4" type="text" bind:value={allowedHostsStr} placeholder="e.g. 192.168.1.0/24, 10.0.0.5 (empty = this node's LAN only)" class="input w-full" />
        <p class="text-xs text-gray-500 mt-1">Use * to allow every host (not recommended).</p>
      </div>

      <div>
        <label class="label" for="share-field-5">Valid Users</label>
        <input id="share-field-5" type="text" bind:value={validUsersStr} placeholder="e.g. user1, user2 (empty = all)" class="input w-full" />
      </div>

      <div>
        <div class="label" id="share-allowed-groups-label">Allowed Groups</div>
        {#if availableGroups.length > 0}
          <div class="space-y-2" role="group" aria-labelledby="share-allowed-groups-label">
            {#each availableGroups as group}
              <label class="flex items-center gap-3 p-2 rounded hover:bg-naslos-dark cursor-pointer">
                <input type="checkbox" checked={validGroups.includes(group)} on:change={() => toggleGroup(group)} class="w-4 h-4 rounded" />
                <span class="text-gray-300">{group}</span>
              </label>
            {/each}
          </div>
        {:else}
          <p class="text-xs text-gray-500">No groups available.</p>
        {/if}
        <p class="text-xs text-gray-500 mt-1">Members of the selected groups can use this share. With no users and no groups selected, any valid account can.</p>
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
