<script lang="ts">
  import { onMount } from 'svelte';
  import { page } from '$app/state';
  import Icon from '$lib/components/Icon.svelte';
  import ErrorState from '$lib/components/ErrorState.svelte';
  import LoadingState from '$lib/components/LoadingState.svelte';
  import Modal from '$lib/components/Modal.svelte';
  import PageHeader from '$lib/components/PageHeader.svelte';
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
    mounted?: boolean;
    encryption?: string;
    keystatus?: string;
    encryptionRoot?: string;
  }
  let datasets: Dataset[] = [];
  let datasetsError = '';

  let showDatasetModal = false;
  let datasetName = '';
  let datasetCompression = 'lz4';
  let datasetQuota = '';
  let datasetEncrypted = false;
  let datasetSaving = false;
  let datasetError = '';

  // Scheduled snapshots and scrubs (FR-STO-10/11). Schedules are stored by the
  // API and survive restarts; this page only edits them.
  interface StorageSchedule {
    id: string;
    kind: 'snapshot' | 'scrub';
    target: string;
    cadence: 'hourly' | 'daily' | 'weekly';
    runAt: string;
    keep: number;
    recursive: boolean;
    enabled: boolean;
    lastRun?: string;
    lastResult?: string;
    lastError?: string;
    nextRun?: string;
  }
  let schedules: StorageSchedule[] = [];
  let schedulesError = '';
  let scrubRunning = false;
  let scrubMessage = '';
  let scrubError = false;

  let showScheduleModal = false;
  let scheduleKind: 'snapshot' | 'scrub' = 'snapshot';
  let scheduleId = '';
  let scheduleTarget = '';
  let scheduleCadence: 'hourly' | 'daily' | 'weekly' = 'daily';
  let scheduleRunAt = '02:30';
  let scheduleKeep = 7;
  let scheduleRecursive = false;
  let scheduleSaving = false;
  let scheduleError = '';

  let showAddDriveModal = false;
  let disks: any[] = [];
  let disksError = '';
  let selectedDisks: string[] = [];
  let addTopology = 'single';
  let addForce = false;
  let addSaving = false;
  let addError = '';

  const poolName = page.params.name;

  async function loadHealth() {
    loading = true;
    error = '';
    try {
      const res = await fetch(`/api/volumes/zfs/${encodeURIComponent(poolName ?? '')}/health`);
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
      if (datasetEncrypted) {
        // Raw-key custody (FR-ENC): the API generates the key into a Secret;
        // the browser never handles it.
        options.encryption = 'on';
        options.keyformat = 'raw';
      }

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
      datasetEncrypted = false;
      await loadDatasets();
    } catch (e: any) {
      datasetError = e.message;
    } finally {
      datasetSaving = false;
    }
  }

  function isEncryptedDataset(ds: Dataset): boolean {
    return !!ds.encryption && ds.encryption !== 'off' && ds.encryption !== '-';
  }

  function isLockedDataset(ds: Dataset): boolean {
    return isEncryptedDataset(ds) && ds.keystatus === 'unavailable';
  }

  async function unlockDataset(name: string) {
    try {
      const res = await fetch('/api/datasets/unlock', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name })
      });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) throw new Error(data.error || `Unlock failed (HTTP ${res.status})`);
      await loadDatasets();
    } catch (e: any) {
      alert(e.message);
    }
  }

  async function lockDataset(name: string) {
    if (!confirm(`Lock ${name}?\n\nIts data stays on disk but is unreadable until an administrator unlocks it. Stop shares and apps using it first.`)) return;
    try {
      const res = await fetch('/api/datasets/lock', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name })
      });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) throw new Error(data.error || `Lock failed (HTTP ${res.status})`);
      await loadDatasets();
    } catch (e: any) {
      alert(e.message);
    }
  }

  async function deleteDataset(name: string, recursive = false, confirmed = false) {
    const subtree = datasets.filter((d) => d.name === name || d.name.startsWith(`${name}/`));
    const encrypted = subtree.some((d) => isEncryptedDataset(d));

    // Never send `confirm` without an explicit typed acknowledgement: the API
    // treats it as authority to crypto-shred the subtree's key Secret.
    if (encrypted && !confirmed) {
      const typed = prompt(
        `Destroy encrypted dataset ${name} and its key Secret?\n\nAll data and snapshots in this subtree become unrecoverable without a backup. Type the dataset name to confirm.`
      );
      if (typed === null) return;
      if (typed.trim() !== name) {
        alert('Dataset name did not match; nothing was destroyed.');
        return;
      }
      confirmed = true;
    }

    try {
      const confirmQuery = confirmed ? `&confirm=${encodeURIComponent(name)}` : '';
      const url = `/api/datasets?name=${encodeURIComponent(name)}${recursive ? '&recursive=true' : ''}${confirmQuery}`;
      const res = await fetch(url, { method: 'DELETE' });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) {
        // A non-empty dataset needs the recursive flag. Ask rather than guess:
        // recursive also destroys snapshots and child datasets. An encrypted
        // subtree was already covered by the typed confirmation above.
        if (!recursive && /not empty|has children|snapshot/i.test(data.error || '')) {
          if (!encrypted) {
            const extra = ' This also destroys its snapshots and child datasets.';
            if (!confirm(`${name} is not empty.\n\nDestroy it recursively?${extra} This cannot be undone.`)) return;
          }
          await deleteDataset(name, true, confirmed);
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

  // ---- Scheduled snapshots and scrubs -----------------------------------

  function fmtTime(s?: string): string {
    if (!s) return '—';
    const d = new Date(s);
    return isNaN(d.getTime()) ? s : d.toLocaleString();
  }

  async function loadSchedules() {
    try {
      const res = await fetch('/api/storage/schedules');
      if (!res.ok) {
        schedulesError = `Failed to load schedules (HTTP ${res.status})`;
        return;
      }
      const data = await res.json();
      schedules = Array.isArray(data.schedules) ? data.schedules : [];
      schedulesError = '';
    } catch {
      schedulesError = 'Failed to connect to API';
    }
  }

  $: snapshotSchedules = schedules.filter(
    (s) => s.kind === 'snapshot' && (s.target === poolName || s.target?.startsWith(`${poolName}/`))
  );
  $: scrubSchedule = schedules.find((s) => s.kind === 'scrub' && s.target === poolName);
  // A snapshot schedule needs a dataset below the pool; the pool root is the
  // pool itself, not a snapshot target.
  $: scheduleableDatasets = datasets.filter((d) => d.name !== poolName);

  function snapshotScheduleFor(dataset: string): StorageSchedule | undefined {
    return schedules.find((s) => s.kind === 'snapshot' && s.target === dataset);
  }

  function openSchedule(
    kind: 'snapshot' | 'scrub',
    target: string,
    existing?: StorageSchedule
  ) {
    // Both entry points resolve the same way: opening an existing schedule must
    // edit it instead of silently creating a duplicate for the same target.
    const current = existing ?? (kind === 'snapshot' ? snapshotScheduleFor(target) : scrubSchedule);
    scheduleKind = kind;
    scheduleId = current?.id ?? '';
    scheduleTarget = target;
    scheduleCadence = current?.cadence ?? (kind === 'scrub' ? 'weekly' : 'daily');
    scheduleRunAt = current?.runAt || (kind === 'scrub' ? 'Sun 02:30' : '02:30');
    scheduleKeep = current?.keep ?? (kind === 'scrub' ? 0 : 7);
    scheduleRecursive = current?.recursive ?? false;
    scheduleError = '';
    showScheduleModal = true;
  }

  function selectScheduleTarget() {
    const existing = snapshotScheduleFor(scheduleTarget);
    scheduleId = existing?.id ?? '';
    if (existing) {
      scheduleCadence = existing.cadence;
      scheduleRunAt = existing.runAt;
      scheduleKeep = existing.keep;
      scheduleRecursive = existing.recursive;
    }
  }

  // Keep `runAt` valid for the selected cadence: daily is `HH:MM` and weekly is
  // `Day HH:MM`. Changing the cadence must not leave the field in the other
  // shape, or the API rejects the save until the operator retypes it.
  function normalizeRunAt(
    cadence: 'hourly' | 'daily' | 'weekly',
    runAt: string
  ): string {
    if (cadence === 'hourly') return '';
    const parts = runAt.trim().split(/\s+/).filter(Boolean);
    const last = parts[parts.length - 1] ?? '';
    const time = /^\d{2}:\d{2}$/.test(last) ? last : '02:30';
    if (cadence === 'weekly') {
      const day = parts.find(p => /^(sun|mon|tue|wed|thu|fri|sat)$/i.test(p));
      const weekday = day ? day[0].toUpperCase() + day.slice(1, 3).toLowerCase() : 'Sun';
      return `${weekday} ${time}`;
    }
    return time;
  }

  function changeScheduleCadence(event: Event) {
    const value = (event.currentTarget as HTMLSelectElement).value as
      | 'hourly'
      | 'daily'
      | 'weekly';
    scheduleCadence = value;
    scheduleRunAt = normalizeRunAt(value, scheduleRunAt);
  }

  async function saveSchedule() {
    scheduleSaving = true;
    scheduleError = '';
    try {
      const res = await fetch('/api/storage/schedules', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          id: scheduleId,
          kind: scheduleKind,
          target: scheduleTarget,
          cadence: scheduleCadence,
          runAt: scheduleCadence === 'hourly' ? '' : scheduleRunAt,
          keep: scheduleKind === 'scrub' ? 0 : Number(scheduleKeep) || 0,
          recursive: scheduleKind === 'snapshot' ? scheduleRecursive : false
          // `enabled` is omitted: a new schedule defaults to enabled in the API,
          // while editing a schedule disabled by the 0.6.0 duplicate migration
          // must not silently re-enable it.
        })
      });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) throw new Error(data.error || 'Could not save the schedule');
      showScheduleModal = false;
      await loadSchedules();
    } catch (e: any) {
      scheduleError = e.message;
    } finally {
      scheduleSaving = false;
    }
  }

  async function removeSchedule(id: string) {
    if (!confirm('Delete this schedule? Snapshots it already created are kept unless their retention limit is reached while the schedule still exists.')) return;
    try {
      const res = await fetch(`/api/storage/schedules?id=${encodeURIComponent(id)}`, {
        method: 'DELETE'
      });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) throw new Error(data.error || 'Could not delete the schedule');
      await loadSchedules();
    } catch (e: any) {
      alert(e.message);
    }
  }

  async function runScrubNow() {
    scrubRunning = true;
    scrubMessage = '';
    scrubError = false;
    try {
      const res = await fetch(`/api/volumes/zfs/${encodeURIComponent(poolName ?? '')}/scrub`, {
        method: 'POST'
      });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) throw new Error(data.error || 'Could not start the scrub');
      scrubMessage = 'Scrub started. Its result appears in the scan status above.';
      await loadHealth();
    } catch (e: any) {
      scrubMessage = e.message;
      scrubError = true;
    } finally {
      scrubRunning = false;
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

  function diskSignatures(device: string): string[] {
    const disk = disks.find(d => d.device === device);
    return (disk && Array.isArray(disk.signatures)) ? disk.signatures : [];
  }

  function isZfsMember(device: string): boolean {
    const sigs = diskSignatures(device);
    return sigs.includes('zfs') || sigs.includes('zfs_member');
  }

  // A disk with data needs the explicit "Force" acknowledgement before the API
  // accepts it; a ZFS label is refused outright.
  function needsWipeConfirm(): boolean {
    return selectedDisks.some(d => diskSignatures(d).length > 0 && !isZfsMember(d));
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
      const res = await fetch(`/api/volumes/zfs/${encodeURIComponent(poolName ?? '')}/devices`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          disks: selectedDisks,
          topology: addTopology,
          force: addForce,
          wipeConfirmed: addForce ? selectedDisks : []
        })
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

  onMount(() => {
    loadHealth();
    loadDatasets();
    loadSchedules();
  });
</script>

<div>
  <div class="mb-6">
    <a href="/pools" class="text-sm text-gray-400 hover:text-naslos-link flex items-center gap-1 w-fit">
      <Icon name="chevron-left" size={14} />
      Back to Pools
    </a>
  </div>

  <PageHeader title={poolName} subtitle="Pool health, datasets and devices.">
    <div slot="actions" class="flex gap-2">
      <button class="btn btn-secondary" on:click={runScrubNow} disabled={scrubRunning}>
        {scrubRunning ? 'Starting…' : 'Scrub Now'}
      </button>
      <button class="btn btn-secondary" on:click={openAddDrive}>Add Drive</button>
      <button class="btn btn-primary" on:click={() => { showDatasetModal = true; datasetError = ''; }}>New Dataset</button>
    </div>
  </PageHeader>

  {#if scrubMessage}
    <div
      role={scrubError ? 'alert' : 'status'}
      class={scrubError ? 'callout callout-bad mb-4' : 'callout callout-ok mb-4'}
    >{scrubMessage}</div>
  {/if}
  {#if schedulesError}<div role="alert" class="callout callout-bad mb-4">{schedulesError}</div>{/if}

  {#if loading}
    <LoadingState label="Loading health data..." />
  {:else if error}
    <ErrorState message={error} retry={loadHealth} />
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
        <div class="text-2xl font-bold text-naslos-link">{health.config.length}</div>
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
        <p role="alert" class="text-red-400 text-sm">{datasetsError}</p>
      {:else if datasets.length === 0}
        <p class="text-gray-400 text-sm">No datasets yet. Create one to have a place for a share.</p>
      {:else}
        <div class="space-y-2">
          {#each datasets as ds}
            <div class="bg-naslos-dark rounded-lg p-3 flex items-center justify-between gap-3">
              <div class="min-w-0">
                <div class="font-medium truncate">
                  {ds.name}
                  {#if isEncryptedDataset(ds)}
                    <span
                      class="ml-2 text-[10px] uppercase tracking-wide px-1.5 py-0.5 rounded border {isLockedDataset(ds) ? 'text-amber-300 border-amber-700' : 'text-emerald-300 border-emerald-700'}"
                      title={isLockedDataset(ds) ? 'Key not loaded; unlock after boot' : 'Key loaded'}
                    >{isLockedDataset(ds) ? 'Locked' : 'Unlocked'}</span>
                  {/if}
                </div>
                <div class="text-xs text-gray-500">{ds.mountpoint || '—'}</div>
              </div>
              <div class="text-xs text-gray-400 whitespace-nowrap">
                {ds.used || '0'} used · {ds.avail || '0'} free
              </div>
              <div class="flex gap-1">
                {#if isEncryptedDataset(ds)}
                  {#if isLockedDataset(ds)}
                    <button
                      class="btn btn-sm rounded border border-amber-700 text-amber-200 hover:bg-amber-900/30 whitespace-nowrap"
                      on:click={() => unlockDataset(ds.name)}
                      title="Load this dataset's key from its Secret"
                    >Unlock</button>
                  {:else}
                    <button
                      class="btn btn-sm rounded border border-naslos-border text-gray-200 hover:bg-naslos-dark whitespace-nowrap"
                      on:click={() => lockDataset(ds.name)}
                      title="Unload the key; the data stays on disk"
                    >Lock</button>
                  {/if}
                {/if}
                {#if ds.name !== poolName}
                  <button
                    class="btn btn-sm rounded border border-naslos-border text-gray-200 hover:bg-naslos-dark whitespace-nowrap"
                    on:click={() => openSchedule('snapshot', ds.name, snapshotScheduleFor(ds.name))}
                    title={snapshotScheduleFor(ds.name) ? 'Edit this dataset’s snapshot schedule' : 'Schedule periodic snapshots'}
                  >{snapshotScheduleFor(ds.name) ? 'Schedule ✓' : 'Schedule'}</button>
                {/if}
                <button
                  class="btn btn-sm rounded border border-naslos-border text-red-300 hover:bg-red-900/30 whitespace-nowrap"
                  on:click={() => deleteDataset(ds.name, false)}
                  title="Destroy this dataset"
                >Delete</button>
              </div>
            </div>
          {/each}
        </div>
      {/if}
    </div>

    <!-- Scheduled maintenance (FR-STO-10/11) -->
    <div class="card mt-6">
      <div class="flex items-center justify-between mb-4">
        <h2 class="text-xl font-bold">Maintenance schedules</h2>
        <button class="btn btn-secondary btn-sm" on:click={() => openSchedule('scrub', poolName ?? '', scrubSchedule)}>
          {scrubSchedule ? 'Edit scrub schedule' : 'Schedule scrub'}
        </button>
      </div>

      <div class="bg-naslos-dark rounded-lg p-3 mb-4">
        <div class="flex flex-wrap items-center justify-between gap-2">
          <div>
            <div class="font-medium">Scrub</div>
            {#if scrubSchedule}
              <div class="text-xs text-gray-400">
                {scrubSchedule.cadence}{scrubSchedule.runAt ? ` ${scrubSchedule.runAt} UTC` : ''} ·
                last {scrubSchedule.lastResult || 'never'} ({fmtTime(scrubSchedule.lastRun)}) ·
                next {scrubSchedule.enabled ? fmtTime(scrubSchedule.nextRun) : 'disabled'}
              </div>
              {#if scrubSchedule.lastError}
                <div class="text-xs text-yellow-400/90 break-words">{scrubSchedule.lastError}</div>
              {/if}
            {:else}
              <div class="text-xs text-gray-500">No scrub schedule. Scrubs are read-only and verify every block's checksum.</div>
            {/if}
          </div>
          <div class="flex gap-1">
            <button class="btn btn-secondary btn-sm" on:click={runScrubNow} disabled={scrubRunning}>Scrub now</button>
            {#if scrubSchedule}
              <button class="btn btn-danger btn-sm" on:click={() => removeSchedule(scrubSchedule.id)}>Remove</button>
            {/if}
          </div>
        </div>
      </div>

      <div class="flex items-center justify-between mb-2">
        <h3 class="font-bold">Scheduled snapshots</h3>
        <button
          class="btn btn-secondary btn-sm"
          on:click={() => openSchedule('snapshot', scheduleableDatasets[0]?.name ?? '')}
          disabled={scheduleableDatasets.length === 0}
        >
          Add snapshot schedule
        </button>
      </div>
      {#if scheduleableDatasets.length === 0}
        <p class="text-sm text-gray-400">Create a dataset below this pool first; the pool root cannot be scheduled.</p>
      {:else if snapshotSchedules.length === 0}
        <p class="text-sm text-gray-400">
          No dataset has a snapshot schedule yet. A schedule creates its own
          <code class="text-xs">naslos-YYYY-MM-DD_HH-MM-SS</code> snapshots (visible as SMB previous
          versions) and keeps the newest N; it never deletes your snapshots.
        </p>
      {:else}
        <div class="space-y-2">
          {#each snapshotSchedules as s (s.id)}
            <div class="bg-naslos-dark rounded-lg p-3 flex flex-wrap items-center justify-between gap-2">
              <div class="min-w-0">
                <div class="font-medium truncate">{s.target}</div>
                <div class="text-xs text-gray-400">
                  {s.cadence}{s.runAt ? ` ${s.runAt} UTC` : ''} · keep {s.keep || 'all'}
                  {s.keep ? 'newest' : ''}{s.recursive ? ' · recursive' : ''} ·
                  last {s.lastResult || 'never'} ({fmtTime(s.lastRun)}) · next {s.enabled ? fmtTime(s.nextRun) : 'disabled'}
                </div>
                {#if s.lastError}<div class="text-xs text-yellow-400/90 break-words">{s.lastError}</div>{/if}
              </div>
              <div class="flex gap-1">
                <button class="btn btn-secondary btn-sm" on:click={() => openSchedule('snapshot', s.target, s)}>Edit</button>
                <button class="btn btn-danger btn-sm" on:click={() => removeSchedule(s.id)}>Remove</button>
              </div>
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
                <StatusBadge state={dev.state} />
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
                      <span class="text-gray-500" aria-hidden="true">└</span>
                      <span>{child.name}</span>
                      <StatusBadge state={child.state} />
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
</div>

<!-- Schedule modal (FR-STO-10/11) -->
{#if showScheduleModal}
  <Modal
    title={scheduleKind === 'scrub' ? `Scrub schedule for ${scheduleTarget}` : `Snapshot schedule for ${scheduleTarget}`}
    onClose={() => (showScheduleModal = false)}
  >
    {#if scheduleKind === 'snapshot'}
      <p class="text-sm text-gray-400">
        The schedule creates snapshots named <code class="text-xs">naslos-YYYY-MM-DD_HH-MM-SS</code>
        (UTC) and keeps the newest <strong>{scheduleKeep || 'all'}</strong>. Snapshots you create
        yourself are never removed.
      </p>
      <div>
        <label class="label" for="schedule-target">Dataset</label>
        <select
          id="schedule-target"
          class="input w-full"
          bind:value={scheduleTarget}
          on:change={selectScheduleTarget}
        >
          {#each scheduleableDatasets as d}
            <option value={d.name}>{d.name}</option>
          {/each}
        </select>
        {#if scheduleId}
          <p class="text-xs text-gray-500 mt-1">This dataset already has a schedule; saving updates it.</p>
        {/if}
      </div>
    {:else}
      <p class="text-sm text-gray-400">
        A scrub is read-only: ZFS reads every block and verifies its checksum. It
        is skipped when a scrub or resilver is already running.
      </p>
    {/if}

    <div>
      <label class="label" for="schedule-cadence">Cadence</label>
      <select id="schedule-cadence" value={scheduleCadence} on:change={changeScheduleCadence} class="input w-full">
        {#if scheduleKind === 'snapshot'}
          <option value="hourly">Hourly</option>
        {/if}
        <option value="daily">Daily</option>
        <option value="weekly">Weekly</option>
      </select>
    </div>

    {#if scheduleCadence !== 'hourly'}
      <div>
        <label class="label" for="schedule-run-at">Run at (UTC)</label>
        <input
          id="schedule-run-at"
          type="text"
          bind:value={scheduleRunAt}
          placeholder={scheduleCadence === 'weekly' ? 'Mon 02:30' : '02:30'}
          class="input w-full"
        />
        <p class="text-xs text-gray-500 mt-1">
          {scheduleCadence === 'weekly' ? 'Weekday and time, e.g. Sun 02:30.' : 'Time of day, e.g. 02:30.'}
        </p>
      </div>
    {/if}

    {#if scheduleKind === 'snapshot'}
      <div>
        <label class="label" for="schedule-keep">Keep newest</label>
        <input id="schedule-keep" type="number" min="0" bind:value={scheduleKeep} class="input w-full" />
        <p class="text-xs text-gray-500 mt-1">0 keeps every scheduler-created snapshot.</p>
      </div>
      <label class="flex items-start gap-3 cursor-pointer">
        <input type="checkbox" class="mt-1" bind:checked={scheduleRecursive} />
        <span class="text-sm">Include child datasets (recursive snapshot)</span>
      </label>
    {/if}

    {#if scheduleError}<div role="alert" class="p-3 rounded-lg bg-red-900/30 border border-red-700 text-red-300 text-sm">{scheduleError}</div>{/if}

    <div slot="footer">
      {#if scheduleId}
        <button class="btn btn-danger" on:click={() => { showScheduleModal = false; removeSchedule(scheduleId); }}>Remove</button>
      {/if}
      <button class="btn btn-secondary" on:click={() => (showScheduleModal = false)}>Cancel</button>
      <button class="btn btn-primary" on:click={saveSchedule} disabled={scheduleSaving || !scheduleTarget}>
        {scheduleSaving ? 'Saving…' : 'Save schedule'}
      </button>
    </div>
  </Modal>
{/if}

<!-- New Dataset modal -->
{#if showDatasetModal}
  <Modal title="New Dataset" onClose={() => (showDatasetModal = false)}>
    <p class="text-sm text-gray-400">
      A dataset inside <span class="text-naslos-link">{poolName}</span> becomes a folder you can share.
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

    <label class="flex items-start gap-3 cursor-pointer">
      <input type="checkbox" class="mt-1" bind:checked={datasetEncrypted} />
      <span>
        Encrypt this dataset
        <span class="block text-xs text-gray-500">
          The key is generated by the API into a Kubernetes Secret; the browser
          never sees it. An administrator must unlock the dataset after every
          reboot. Losing the key Secret loses the data. Encryption is
          create-only: existing datasets cannot be converted in place.
        </span>
      </span>
    </label>

    {#if datasetError}<div role="alert" class="p-3 rounded-lg bg-red-900/30 border border-red-700 text-red-300 text-sm">{datasetError}</div>{/if}

    <div slot="footer">
      <button class="btn btn-secondary" on:click={() => (showDatasetModal = false)}>Cancel</button>
      <button class="btn btn-primary" on:click={createDataset} disabled={datasetSaving || !datasetName.trim()}>
        {datasetSaving ? 'Creating...' : 'Create Dataset'}
      </button>
    </div>
  </Modal>
{/if}

<!-- Add Drive modal -->
{#if showAddDriveModal}
  <Modal wide title="Add Drive to {poolName}" onClose={() => (showAddDriveModal = false)}>
    <p class="text-sm text-gray-400">
      The selected disks become a new vdev in this pool. Existing data is not redistributed, and a disk
      that already belongs to a pool cannot be selected.
    </p>

    {#if disksError}
      <p role="alert" class="text-red-400 text-sm">{disksError}</p>
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
              disabled={!!disk.inPool || isZfsMember(disk.device)}
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
      <span class="text-gray-300 text-sm">Force: overwrite any existing signature on the disks (required when a selected disk already contains data)</span>
    </label>
    {#if addForce}
      <p role="alert" class="text-xs text-red-400">
        Forcing writes over whatever is on the disks. If a disk holds another pool's label, that pool is destroyed.
      </p>
    {/if}

    {#if addError}<div role="alert" class="p-3 rounded-lg bg-red-900/30 border border-red-700 text-red-300 text-sm">{addError}</div>{/if}

    <div slot="footer">
      <button class="btn btn-secondary" on:click={() => (showAddDriveModal = false)}>Cancel</button>
      <button
        class="btn btn-primary"
        on:click={addDevices}
        disabled={addSaving || selectedDisks.length < (topologyDisksNeeded[addTopology] || 1) || (needsWipeConfirm() && !addForce)}
      >
        {addSaving ? 'Adding...' : `Add ${selectedDisks.length || ''} disk${selectedDisks.length === 1 ? '' : 's'}`}
      </button>
    </div>
  </Modal>
{/if}
