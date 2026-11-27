<script lang="ts">
  import { createEventDispatcher, onMount } from 'svelte';
  import { api, ApiError } from '$lib/api';
  import Button from '$lib/components/Button.svelte';
  import FormField from '$lib/components/FormField.svelte';
  import Icon from '$lib/components/Icon.svelte';
  import Modal from '$lib/components/Modal.svelte';

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
    shadowCopies?: boolean;
    audit?: boolean;
    enabled: boolean;
  }

  export let share: Share | null = null;

  const dispatch = createEventDispatcher<{ close: void }>();

  let name = '';
  let path = '';
  let protocol = 'smb';
  let description = '';
  let readOnly = false;
  let browseable = true;
  let timeMachine = false;
  let noRootSquash = false;
  let shadowCopies = false;
  let audit = false;
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
  let baseDir = ''; // dataset mountpoint the browsing is confined to
  let browseDir = ''; // folder the share will point at == `path`
  let subfolders: string[] = [];
  let folderBusy = false;
  let folderError = '';
  let newFolderName = '';

  // LDAP groups available to grant share access to. Access is evaluated by
  // Samba on the node, so the group has to exist there (mirrored into NSS) for
  // this to work - hence a picker rather than free text.
  async function loadGroups() {
    try {
      const data = await api<Array<{ cn?: string; name?: string }>>('/api/groups');
      availableGroups = (Array.isArray(data) ? data : [])
        .map((g) => g.cn || g.name || '')
        .filter(Boolean);
    } catch {
      // The picker stays empty and access can still be typed manually.
    }
  }

  function toggleGroup(group: string) {
    validGroups = validGroups.includes(group)
      ? validGroups.filter((g) => g !== group)
      : [...validGroups, group];
  }

  // The datasets a share may live on. Only dataset mountpoints are offered: a
  // plain directory under /var/mnt would put the share's data outside the pool.
  async function loadPaths() {
    try {
      const data = await api<{ paths?: string[] }>('/api/shares/paths');
      availablePaths = Array.isArray(data.paths) ? data.paths : [];

      // Root the browsing at the dataset that holds the current path (set by
      // init() for an existing share), otherwise at the first dataset.
      if (!baseDir) baseDir = browseDir ? datasetFor(browseDir) : availablePaths[0] || '';
      browseDir = browseDir || baseDir;
      if (!path) path = browseDir;

      await loadSubfolders();
    } catch {
      // The picker stays empty and the error surfaces on save.
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
    } catch (e) {
      subfolders = [];
      folderError = e instanceof Error ? e.message : String(e);
    }
  }

  async function enterFolder(folder: string) {
    folderError = '';
    browseDir = `${browseDir}/${folder}`;
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
    const folderName = newFolderName.trim();
    if (!folderName || !browseDir) return;

    folderBusy = true;
    folderError = '';
    try {
      const data = await api<{ path: string }>('/api/shares/folders', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ path: browseDir, name: folderName })
      });

      // Link the share to the folder that was just created.
      browseDir = data.path;
      path = data.path;
      newFolderName = '';
      await loadSubfolders();
    } catch (e) {
      folderError = e instanceof ApiError ? e.message : 'Could not create folder';
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
      shadowCopies = share.shadowCopies ?? false;
      audit = share.audit ?? false;
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
      // SMB-only options (FR-SHR-14/15). A new NFS share must never carry a
      // toggle left over from a protocol switch; an existing share keeps its
      // stored values (the API ignores protocol changes on update).
      shadowCopies: share || protocol === 'smb' ? shadowCopies : false,
      audit: share || protocol === 'smb' ? audit : false,
      allowedHosts: allowedHostsStr ? allowedHostsStr.split(',').map((s) => s.trim()) : [],
      validUsers: validUsersStr ? validUsersStr.split(',').map((s) => s.trim()) : [],
      validGroups
    };

    try {
      const url = share ? `/api/shares/${share.name}` : '/api/shares';
      const method = share ? 'PUT' : 'POST';
      await api(url, {
        method,
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body)
      });
      dispatch('close');
    } catch (e) {
      error = e instanceof ApiError ? e.message : 'Failed to save share';
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

<Modal
  open={true}
  title={share ? 'Edit Share' : 'New Share'}
  size="lg"
  onClose={() => dispatch('close')}
>
  <form id="share-form" class="space-y-4" on:submit|preventDefault={save}>
    <FormField label="Share Name" id="share-field-1" required>
      <input
        id="share-field-1"
        type="text"
        bind:value={name}
        placeholder="e.g. media, documents"
        class="input w-full"
        disabled={!!share}
      />
    </FormField>

    <FormField label="Path" id="share-dataset" required>
      <!-- A share is served from a folder on a dataset. The dropdown picks the
           dataset (the only thing that guarantees the data is in the pool);
           the listing below descends into it, and any subfolder - existing or
           newly created - can be the share's path. -->
      <select id="share-dataset" class="input w-full" value={baseDir} on:change={onDatasetChange}>
        <option value="">Select a dataset...</option>
        {#each availablePaths as p (p)}
          <option value={p}>{p}</option>
        {/each}
      </select>

      {#if browseDir}
        <div class="mt-2 rounded-lg border border-naslos-border bg-naslos-dark p-3">
          <div class="flex flex-wrap items-center justify-between gap-2">
            <code class="break-all text-xs text-naslos-link">{browseDir}</code>
            <Button
              type="button"
              size="sm"
              variant="secondary"
              on:click={goUp}
              disabled={browseDir === baseDir}
              title="Go up one folder (stops at the dataset)"
            >↑ Up</Button>
          </div>

          {#if subfolders.length > 0}
            <div class="mt-3 flex flex-wrap gap-2">
              {#each subfolders as folder (folder)}
                <button
                  type="button"
                  class="btn btn-sm btn-secondary"
                  on:click={() => enterFolder(folder)}
                  title="Use this folder for the share"
                >
                  <span class="inline-flex items-center gap-1"><Icon name="folder" size={14} />{folder}</span>
                </button>
              {/each}
            </div>
          {:else}
            <p class="mt-3 text-xs text-gray-500">No folders inside this one yet.</p>
          {/if}

          <div class="mt-3 flex flex-col gap-2 sm:flex-row sm:items-end">
            <div class="flex-1">
              <label class="label" for="share-new-folder">New folder</label>
              <input
                id="share-new-folder"
                type="text"
                bind:value={newFolderName}
                on:keydown={(e) => {
                  if (e.key === 'Enter') {
                    e.preventDefault();
                    createFolder();
                  }
                }}
                placeholder="New folder name"
                class="input w-full"
              />
            </div>
            <Button
              type="button"
              variant="secondary"
              loading={folderBusy}
              disabled={!newFolderName.trim()}
              on:click={createFolder}
            >{folderBusy ? 'Creating...' : 'Create folder'}</Button>
          </div>
          {#if folderError}
            <p role="alert" class="mt-2 text-xs text-red-400">{folderError}</p>
          {/if}
        </div>
      {/if}

      <p class="mt-1 text-xs text-gray-500">
        The share is served from the folder shown above. Create a folder to keep this share separate
        from the rest of the dataset, or select an existing one.
      </p>
    </FormField>

    <FormField label="Protocol" id="share-field-2" required hint="Time Machine is served over SMB; AFP is not supported.">
      <select id="share-field-2" bind:value={protocol} class="input w-full">
        <option value="smb">SMB/CIFS (Windows, macOS, Linux)</option>
        <option value="nfs">NFS (Linux, macOS)</option>
      </select>
    </FormField>

    <FormField label="Description" id="share-field-3">
      <input
        id="share-field-3"
        type="text"
        bind:value={description}
        placeholder="Optional description"
        class="input w-full"
      />
    </FormField>

    <div class="flex flex-wrap items-center gap-x-6 gap-y-2">
      <label class="flex min-h-10 cursor-pointer items-center gap-3">
        <input type="checkbox" bind:checked={readOnly} class="h-4 w-4 rounded" />
        <span class="text-gray-300">Read Only</span>
      </label>
      <label class="flex min-h-10 cursor-pointer items-center gap-3">
        <input type="checkbox" bind:checked={browseable} class="h-4 w-4 rounded" />
        <span class="text-gray-300">Browseable</span>
      </label>
    </div>

    {#if protocol === 'smb'}
      <div class="space-y-3">
        <label class="flex min-h-10 cursor-pointer items-center gap-3">
          <input type="checkbox" bind:checked={timeMachine} class="h-4 w-4 rounded" />
          <span class="text-gray-300">Time Machine (macOS backup)</span>
        </label>
        <div>
          <label class="flex min-h-10 cursor-pointer items-center gap-3">
            <input type="checkbox" bind:checked={shadowCopies} class="h-4 w-4 rounded" />
            <span class="text-gray-300">Previous Versions (shadow copies)</span>
          </label>
          <p class="ml-7 mt-1 text-xs text-gray-500">
            Exposes the dataset's scheduled snapshots to Windows Previous Versions. Only
            snapshots named <code class="text-xs">naslos-YYYY-MM-DD_HH-MM-SS</code> are listed.
          </p>
        </div>
        <div>
          <label class="flex min-h-10 cursor-pointer items-center gap-3">
            <input type="checkbox" bind:checked={audit} class="h-4 w-4 rounded" />
            <span class="text-gray-300">Audit file operations</span>
          </label>
          <p class="ml-7 mt-1 text-xs text-gray-500">
            Records successful and failed SMB operations in the Samba log.
          </p>
        </div>
      </div>
    {/if}

    {#if protocol === 'nfs'}
      <div>
        <label class="flex min-h-10 cursor-pointer items-center gap-3">
          <input type="checkbox" bind:checked={noRootSquash} class="h-4 w-4 rounded" />
          <span class="text-gray-300">Allow root clients to write as root (disable root squash)</span>
        </label>
        {#if noRootSquash}
          <p class="mt-1 text-xs text-amber-400">
            Warning: a root client on an allowed host can read and write every file as root.
            Leave this off unless the share's data is root-owned and an admin client must write it.
          </p>
        {/if}
      </div>
    {/if}

    <FormField
      label="Allowed Hosts"
      id="share-field-4"
      hint="Use * to allow every host (not recommended)."
    >
      <input
        id="share-field-4"
        type="text"
        bind:value={allowedHostsStr}
        placeholder="e.g. 192.0.2.0/24, 10.0.0.5 (empty = this node's LAN only)"
        class="input w-full"
      />
    </FormField>

    <FormField label="Valid Users" id="share-field-5">
      <input
        id="share-field-5"
        type="text"
        bind:value={validUsersStr}
        placeholder="e.g. user1, user2 (empty = all)"
        class="input w-full"
      />
    </FormField>

    <div>
      <div class="label" id="share-allowed-groups-label">Allowed Groups</div>
      {#if availableGroups.length > 0}
        <div class="space-y-1" role="group" aria-labelledby="share-allowed-groups-label">
          {#each availableGroups as group (group)}
            <label class="flex min-h-10 cursor-pointer items-center gap-3 rounded px-2 hover:bg-naslos-dark">
              <input
                type="checkbox"
                checked={validGroups.includes(group)}
                on:change={() => toggleGroup(group)}
                class="h-4 w-4 rounded"
              />
              <span class="text-gray-300">{group}</span>
            </label>
          {/each}
        </div>
      {:else}
        <p class="text-xs text-gray-500">No groups available.</p>
      {/if}
      <p class="mt-1 text-xs text-gray-500">
        Members of the selected groups can use this share. With no users and no groups selected,
        any valid account can.
      </p>
    </div>

    {#if error}
      <div role="alert" class="callout callout-bad mb-0">{error}</div>
    {/if}
  </form>

  <svelte:fragment slot="footer">
    <Button variant="secondary" on:click={() => dispatch('close')}>Cancel</Button>
    <Button type="submit" form="share-form" loading={saving} disabled={!name || !path}>
      {saving ? 'Saving...' : share ? 'Update' : 'Create'}
    </Button>
  </svelte:fragment>
</Modal>
