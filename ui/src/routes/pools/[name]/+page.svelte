<script lang="ts">
  import { onMount } from 'svelte';
  import { page } from '$app/state';
  import { api, ApiError } from '$lib/api';
  import AddDriveModal from '$lib/components/pools/AddDriveModal.svelte';
  import NewDatasetModal from '$lib/components/pools/NewDatasetModal.svelte';
  import NewZvolModal from '$lib/components/pools/NewZvolModal.svelte';
  import PoolDatasetsCard from '$lib/components/pools/PoolDatasetsCard.svelte';
  import PoolHealthSummary from '$lib/components/pools/PoolHealthSummary.svelte';
  import PoolSchedulesCard from '$lib/components/pools/PoolSchedulesCard.svelte';
  import PoolZvolsCard from '$lib/components/pools/PoolZvolsCard.svelte';
  import ResizeZvolModal from '$lib/components/pools/ResizeZvolModal.svelte';
  import ScheduleModal from '$lib/components/pools/ScheduleModal.svelte';
  import AsyncState from '$lib/components/AsyncState.svelte';
  import Button from '$lib/components/Button.svelte';
  import ErrorState from '$lib/components/ErrorState.svelte';
  import PageHeader from '$lib/components/PageHeader.svelte';
  import { isEncryptedDataset } from '$lib/storage';
  import { confirmAction } from '$lib/stores/confirm';

  interface PoolHealth {
    name: string;
    state: string;
    scan: string;
    errors: string;
    config: Array<{
      name: string;
      state: string;
      read: string;
      write: string;
      cksum: string;
      devices?: Array<{ name: string; state: string; read: string; write: string; cksum: string }>;
    }>;
    ioStats: { readOps: string; writeOps: string; readBW: string; writeBW: string };
  }

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

  interface Zvol {
    name: string;
    volsize: string;
    volsizeBytes: number;
    volblocksize: string;
    compression: string;
    used: string;
    avail: string;
    refer: string;
  }

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

  let health: PoolHealth | null = null;
  let loading = true;
  let error = '';

  let datasets: Dataset[] = [];
  let datasetsError = '';

  let zvols: Zvol[] = [];
  let zvolsError = '';

  let schedules: StorageSchedule[] = [];
  let schedulesError = '';

  let actionError = '';
  let scrubRunning = false;
  let scrubMessage = '';
  let scrubError = false;

  let showDatasetModal = false;
  let showZvolModal = false;
  let resizeZvol: Zvol | null = null;
  let showScheduleModal = false;
  let scheduleKind: 'snapshot' | 'scrub' = 'snapshot';
  let scheduleTarget = '';
  let scheduleExisting: StorageSchedule | null = null;
  let showAddDriveModal = false;

  // SvelteKit reuses this component when the route param changes, so the
  // pool name must stay reactive; a captured `const` kept showing (and
  // mutating) the pool the page was first opened on.
  let poolName = page.params.name ?? '';
  let loadedPool = '';
  let mounted = false;

  function message(e: unknown, fallback: string): string {
    return e instanceof ApiError ? e.message : fallback;
  }

  function resetPoolView() {
    health = null;
    loading = true;
    error = '';
    datasets = [];
    datasetsError = '';
    zvols = [];
    zvolsError = '';
    schedules = [];
    schedulesError = '';
    actionError = '';
    scrubMessage = '';
    scrubError = false;
    showDatasetModal = false;
    showZvolModal = false;
    resizeZvol = null;
    showScheduleModal = false;
    showAddDriveModal = false;
  }

  async function loadHealth() {
    const requested = poolName;
    loading = true;
    error = '';
    try {
      const data = await api<PoolHealth>(`/api/volumes/zfs/${encodeURIComponent(requested)}/health`);
      if (poolName !== requested) return;
      health = data;
    } catch (e) {
      if (poolName !== requested) return;
      error =
        e instanceof ApiError
          ? `Failed to load health data (HTTP ${e.status})`
          : 'Failed to connect to the API';
    } finally {
      if (poolName === requested) loading = false;
    }
  }

  // Datasets come from the API (every dataset) filtered to this pool, so the
  // page shows what actually exists rather than what was requested.
  async function loadDatasets() {
    const requested = poolName;
    try {
      const all = await api<Dataset[]>('/api/datasets');
      if (poolName !== requested) return;
      const prefix = `${requested}/`;
      datasets = all.filter((d) => d.name === requested || d.name.startsWith(prefix));
      datasetsError = '';
    } catch (e) {
      if (poolName === requested) {
        datasetsError =
          e instanceof ApiError
            ? `Failed to load datasets (HTTP ${e.status})`
            : 'Failed to connect to the API';
      }
    }
  }

  async function loadZvols() {
    const requested = poolName;
    try {
      const data = await api<Zvol[]>(`/api/zvols?pool=${encodeURIComponent(requested)}`);
      if (poolName !== requested) return;
      zvols = Array.isArray(data) ? data : [];
      zvolsError = '';
    } catch (e) {
      if (poolName === requested) {
        zvolsError =
          e instanceof ApiError
            ? `Failed to load zvols (HTTP ${e.status})`
            : 'Failed to connect to the API';
      }
    }
  }

  async function loadSchedules() {
    try {
      const data = await api<{ schedules?: StorageSchedule[] }>('/api/storage/schedules');
      schedules = Array.isArray(data.schedules) ? data.schedules : [];
      schedulesError = '';
    } catch (e) {
      schedulesError =
        e instanceof ApiError
          ? `Failed to load schedules (HTTP ${e.status})`
          : 'Failed to connect to the API';
    }
  }

  $: scheduleableDatasets = datasets.filter((d) => d.name !== poolName);

  // ---- Dataset actions ---------------------------------------------------

  async function unlockDataset(name: string) {
    actionError = '';
    try {
      await api('/api/datasets/unlock', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name })
      });
      await loadDatasets();
    } catch (e) {
      actionError = message(e, 'Unlock failed. Check the API connection.');
    }
  }

  async function lockDataset(name: string) {
    if (
      !(await confirmAction({
        title: 'Lock dataset',
        message: `Lock ${name}?\n\nIts data stays on disk but is unreadable until an administrator unlocks it. Stop shares and apps using it first.`,
        confirmLabel: 'Lock dataset'
      }))
    ) {
      return;
    }
    actionError = '';
    try {
      await api('/api/datasets/lock', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name })
      });
      await loadDatasets();
    } catch (e) {
      actionError = message(e, 'Lock failed. Check the API connection.');
    }
  }

  function datasetIsEncrypted(name: string): boolean {
    return datasets
      .filter((d) => d.name === name || d.name.startsWith(`${name}/`))
      .some((d) => isEncryptedDataset(d));
  }

  async function deleteDataset(name: string) {
    const encrypted = datasetIsEncrypted(name);
    const confirmed = await confirmAction({
      title: `Destroy dataset ${name}?`,
      message: encrypted
        ? `Destroy encrypted dataset ${name} and its key Secret?\n\nAll data and snapshots in this subtree become unrecoverable without a backup. Type the dataset name to confirm.`
        : `Destroy dataset ${name}?\n\nThis permanently deletes the dataset; if it is not empty, its snapshots and child datasets are deleted too. Type the dataset name to confirm.`,
      confirmLabel: 'Destroy dataset',
      tone: 'danger',
      typedConfirmation: name
    });
    if (!confirmed) return;
    await destroyDataset(name, false, encrypted);
  }

  async function destroyDataset(name: string, recursive: boolean, encrypted: boolean) {
    actionError = '';
    try {
      const url = `/api/datasets?name=${encodeURIComponent(name)}${recursive ? '&recursive=true' : ''}&confirm=${encodeURIComponent(name)}`;
      await api(url, { method: 'DELETE' });
      await loadDatasets();
    } catch (e) {
      const detail = message(e, 'Could not destroy the dataset.');
      // A non-empty dataset needs the recursive flag. Ask rather than guess:
      // recursive also destroys snapshots and child datasets. An encrypted
      // subtree was already covered by the typed confirmation.
      if (!recursive && /not empty|has children|snapshot/i.test(detail)) {
        if (!encrypted) {
          const go = await confirmAction({
            title: 'Destroy recursively?',
            message: `${name} is not empty.\n\nDestroy it recursively? This also destroys its snapshots and child datasets and cannot be undone.`,
            confirmLabel: 'Destroy recursively',
            tone: 'danger'
          });
          if (!go) return;
        }
        await destroyDataset(name, true, encrypted);
        return;
      }
      actionError = detail;
    }
  }

  // ---- Block volumes -----------------------------------------------------

  async function deleteZvol(zvol: Zvol) {
    const confirmed = await confirmAction({
      title: `Destroy zvol ${zvol.name}?`,
      message: `The block device and everything stored on it are destroyed. Type the full name to confirm.`,
      confirmLabel: 'Destroy zvol',
      tone: 'danger',
      typedConfirmation: zvol.name
    });
    if (!confirmed) return;
    actionError = '';
    try {
      await api(
        `/api/zvols?name=${encodeURIComponent(zvol.name)}&confirm=${encodeURIComponent(zvol.name)}`,
        { method: 'DELETE' }
      );
      await loadZvols();
    } catch (e) {
      actionError = message(e, 'Could not destroy the zvol.');
    }
  }

  // ---- Schedules ---------------------------------------------------------

  function openSchedule(
    kind: 'snapshot' | 'scrub',
    target: string,
    existing?: StorageSchedule | null
  ) {
    // Both entry points resolve the same way: opening an existing schedule must
    // edit it instead of silently creating a duplicate for the same target.
    const current =
      existing ??
      (kind === 'snapshot'
        ? schedules.find((s) => s.kind === 'snapshot' && s.target === target)
        : schedules.find((s) => s.kind === 'scrub' && s.target === target)) ??
      null;
    scheduleKind = kind;
    scheduleTarget = target;
    scheduleExisting = current;
    showScheduleModal = true;
  }

  function openScheduleForDataset(name: string) {
    const existing = schedules.find((s) => s.kind === 'snapshot' && s.target === name) ?? null;
    openSchedule('snapshot', name, existing);
  }

  async function removeSchedule(id: string) {
    if (
      !(await confirmAction({
        title: 'Delete schedule',
        message:
          'Delete this schedule? Snapshots it already created are kept unless their retention limit is reached while the schedule still exists.',
        confirmLabel: 'Delete schedule',
        tone: 'danger'
      }))
    ) {
      return;
    }
    actionError = '';
    try {
      await api(`/api/storage/schedules?id=${encodeURIComponent(id)}`, { method: 'DELETE' });
      await loadSchedules();
    } catch (e) {
      actionError = message(e, 'Could not delete the schedule.');
    }
  }

  async function runScrubNow() {
    scrubRunning = true;
    scrubMessage = '';
    scrubError = false;
    try {
      await api(`/api/volumes/zfs/${encodeURIComponent(poolName ?? '')}/scrub`, {
        method: 'POST'
      });
      scrubMessage = 'Scrub started. Its result appears in the scan status above.';
      await loadHealth();
    } catch (e) {
      scrubMessage = message(e, 'Could not start the scrub.');
      scrubError = true;
    } finally {
      scrubRunning = false;
    }
  }

  onMount(() => {
    mounted = true;
  });

  $: poolName = page.params.name ?? '';
  $: if (mounted && poolName && poolName !== loadedPool) {
    loadedPool = poolName;
    resetPoolView();
    void loadHealth();
    void loadDatasets();
    void loadZvols();
    void loadSchedules();
  }
</script>

<div class="mx-auto max-w-6xl">
  <PageHeader
    title={poolName}
    subtitle="Pool health, datasets and devices."
    breadcrumbs={[{ label: 'Storage' }, { label: 'Pools', href: '/pools' }, { label: poolName }]}
  >
    <div slot="actions" class="flex flex-wrap gap-2">
      <Button variant="secondary" loading={scrubRunning} on:click={runScrubNow}>Scrub Now</Button>
      <Button variant="secondary" on:click={() => (showAddDriveModal = true)}>Add Drive</Button>
      <Button on:click={() => (showDatasetModal = true)}>New Dataset</Button>
    </div>
  </PageHeader>

  {#if scrubMessage}
    <div
      role={scrubError ? 'alert' : 'status'}
      class={scrubError ? 'callout callout-bad' : 'callout callout-ok'}
    >
      {scrubMessage}
    </div>
  {/if}
  {#if actionError}
    <div role="alert" class="callout callout-bad">{actionError}</div>
  {/if}
  {#if schedulesError}
    <ErrorState message={schedulesError} retry={loadSchedules} />
  {/if}

  <AsyncState {loading} error={error} loadingText="Loading health data..." onRetry={loadHealth}>
    {#if health}
      <PoolHealthSummary {health} />

      <PoolDatasetsCard
        {datasets}
        error={datasetsError}
        {poolName}
        {schedules}
        on:new={() => (showDatasetModal = true)}
        on:delete={(e) => deleteDataset(e.detail)}
        on:unlock={(e) => unlockDataset(e.detail)}
        on:lock={(e) => lockDataset(e.detail)}
        on:schedule={(e) => openScheduleForDataset(e.detail)}
      />

      <PoolZvolsCard
        {zvols}
        error={zvolsError}
        {poolName}
        on:new={() => (showZvolModal = true)}
        on:resize={(e) => (resizeZvol = e.detail)}
        on:delete={(e) => deleteZvol(e.detail)}
      />

      {#if !schedulesError}
        <PoolSchedulesCard
          {schedules}
          {poolName}
          {scheduleableDatasets}
          on:scrub={runScrubNow}
          on:scrubEdit={() =>
            openSchedule(
              'scrub',
              poolName,
              schedules.find((s) => s.kind === 'scrub' && s.target === poolName) ?? null
            )}
          on:addSnapshot={() => openSchedule('snapshot', scheduleableDatasets[0]?.name ?? '', null)}
          on:edit={(e) => openSchedule(e.detail.kind, e.detail.target, e.detail)}
          on:remove={(e) => removeSchedule(e.detail)}
        />
      {/if}
    {/if}
  </AsyncState>
</div>

{#if showDatasetModal}
  <NewDatasetModal
    {poolName}
    onClose={() => (showDatasetModal = false)}
    onCreated={() => {
      showDatasetModal = false;
      void loadDatasets();
    }}
  />
{/if}

{#if showZvolModal}
  <NewZvolModal
    {poolName}
    onClose={() => (showZvolModal = false)}
    onCreated={() => {
      showZvolModal = false;
      void loadZvols();
    }}
  />
{/if}

{#if resizeZvol}
  <ResizeZvolModal
    zvol={resizeZvol}
    onClose={() => (resizeZvol = null)}
    onResized={() => {
      resizeZvol = null;
      void loadZvols();
    }}
  />
{/if}

{#if showScheduleModal}
  <ScheduleModal
    kind={scheduleKind}
    target={scheduleTarget}
    existing={scheduleExisting}
    {scheduleableDatasets}
    {schedules}
    onClose={() => (showScheduleModal = false)}
    onSaved={() => {
      showScheduleModal = false;
      void loadSchedules();
    }}
    on:remove={(e) => removeSchedule(e.detail)}
  />
{/if}

{#if showAddDriveModal}
  <AddDriveModal
    {poolName}
    onClose={() => (showAddDriveModal = false)}
    onAdded={() => {
      showAddDriveModal = false;
      void Promise.all([loadHealth(), loadDatasets()]);
    }}
  />
{/if}
