<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { api, ApiError } from '$lib/api';
  import AsyncState from '$lib/components/AsyncState.svelte';
  import Button from '$lib/components/Button.svelte';
  import ErrorState from '$lib/components/ErrorState.svelte';
  import FormField from '$lib/components/FormField.svelte';
  import Modal from '$lib/components/Modal.svelte';
  import PageHeader from '$lib/components/PageHeader.svelte';
  import StatusBadge from '$lib/components/StatusBadge.svelte';
  import { formatBytes, fmtTime } from '$lib/format';
  import { isEncryptedDataset } from '$lib/storage';
  import { toneFor, type Tone } from '$lib/status';
  import { confirmAction } from '$lib/stores/confirm';

  // Local replication (FR-REP). The API owns the job: the page validates the
  // pair, enqueues, then polls `/api/replication/jobs` for live progress and the
  // terminal result. Jobs are in memory, so the list is empty after an API
  // restart even though the received snapshots remain.

  interface ReplicationJob {
    id: string;
    source: string;
    destination: string;
    snapshot?: string;
    recursive: boolean;
    raw?: boolean;
    pruneKeep?: number;
    state: 'running' | 'succeeded' | 'failed' | 'cancelled';
    startedAt: string;
    finishedAt?: string;
    progress: { bytes: number; estimatedBytes: number };
    incremental: boolean;
    estimatedBytes?: number;
    result?: {
      status: string;
      source: string;
      destination: string;
      snapshot: string;
      baseSnapshot?: string;
      baseGUID?: string;
      toGUID?: string;
      incremental: boolean;
      recursive: boolean;
      raw?: boolean;
      bytes: number;
      estimatedBytes: number;
      prunedSnapshots: number;
      mountpoint?: string;
      warnings?: string[];
      retentionError?: string;
      durationSeconds: number;
    };
    error?: string;
  }

  interface Dataset {
    name: string;
    used?: string;
    avail?: string;
    mountpoint?: string;
    encryption?: string;
    keystatus?: string;
    encryptionRoot?: string;
  }

  let loading = true;
  let refreshing = false;
  let datasets: Dataset[] = [];
  let datasetsError = '';
  let jobs: ReplicationJob[] = [];
  let jobsError = '';
  let jobsLoaded = false;
  let actionError = '';

  let showModal = false;
  let saving = false;
  let formError = '';
  let notice = '';

  let source = '';
  let destination = '';
  let snapshot = '';
  let recursive = false;
  let pruneKeep = 0;

  let cancelingId = '';
  let pollTimer: ReturnType<typeof setInterval> | null = null;
  let jobsInFlight = false;

  // A replication pair is two existing datasets; the pool root itself is not a
  // valid endpoint (the API requires `<pool>/<name>`).
  $: pairDatasets = datasets.filter((d) => d.name.includes('/'));
  $: runningJobs = jobs.filter((j) => j.state === 'running');
  $: finishedJobs = jobs
    .filter((j) => j.state !== 'running')
    .slice()
    .reverse();
  // An encrypted source is sent raw and the job creates the destination on the
  // first run (a raw receive cannot overwrite an existing dataset).
  $: encryptedSource = pairDatasets.some((d) => d.name === source && isEncryptedDataset(d));

  // The destination input switches between a select (both datasets exist) and a
  // free path (raw pair creates it). Clear a value that no longer matches the
  // mode so a stale name cannot be submitted unseen.
  let previousEncrypted: boolean | null = null;
  $: if (encryptedSource !== previousEncrypted) {
    previousEncrypted = encryptedSource;
    destination = '';
  }

  function jobTone(job: ReplicationJob): Tone {
    // Running is in-progress (info), not healthy; everything else follows the
    // shared state table, with cancelled called out as a warning.
    if (job.state === 'running') return 'info';
    if (job.state === 'cancelled') return 'warn';
    return toneFor(job.state);
  }

  function progressPercent(job: ReplicationJob): number | null {
    const estimated = job.progress?.estimatedBytes || job.estimatedBytes || 0;
    const bytes = job.progress?.bytes || 0;
    if (estimated <= 0) return null;
    return Math.max(0, Math.min(100, Math.round((bytes / estimated) * 100)));
  }

  // Svelte's legacy reactivity only tracks variables named in the statement
  // itself, so the fields are passed in rather than read inside the function.
  function pairError(
    src: string,
    dst: string,
    keep: number,
    encrypted: boolean
  ): string {
    if (!src || !dst) return 'Choose a source and a destination dataset.';
    if (src === dst) return 'The source and the destination must be different datasets.';
    if (dst.startsWith(`${src}/`)) {
      return 'The destination cannot be inside the source dataset.';
    }
    if (src.startsWith(`${dst}/`)) {
      return 'The source cannot be inside the destination dataset.';
    }
    if (encrypted && !dst.includes('/')) {
      return 'The destination must be a <pool>/<name> path.';
    }
    if (!Number.isInteger(keep) || keep < 0 || keep > 1000) {
      return 'Retention must be a whole number from 0 to 1000 (0 keeps every replication snapshot).';
    }
    return '';
  }

  $: validationError = pairError(source, destination, pruneKeep, encryptedSource);

  async function loadDatasets() {
    try {
      const data = await api<unknown>('/api/datasets');
      datasets = Array.isArray(data) ? (data as Dataset[]) : [];
      datasetsError = '';
    } catch (e) {
      datasetsError =
        e instanceof ApiError
          ? `Failed to load datasets (HTTP ${e.status})`
          : 'Failed to connect to the API.';
    }
  }

  async function loadJobs(quiet = false) {
    if (jobsInFlight) return;
    jobsInFlight = true;
    // A retry after a failed first load shows the loader; a refresh or poll
    // over an already-loaded list keeps that list on screen.
    if (!quiet) {
      refreshing = true;
      if (!jobsLoaded) loading = true;
    }
    try {
      const data = await api<{ jobs?: ReplicationJob[] }>('/api/replication/jobs');
      jobs = Array.isArray(data.jobs) ? data.jobs : [];
      jobsError = '';
      jobsLoaded = true;
    } catch (e) {
      jobsError =
        e instanceof ApiError
          ? `Failed to load replication jobs (HTTP ${e.status})`
          : 'Failed to connect to the API.';
    } finally {
      jobsInFlight = false;
      refreshing = false;
      if (!quiet) loading = false;
    }
  }

  function openCreate() {
    source = '';
    destination = '';
    snapshot = '';
    recursive = false;
    pruneKeep = 0;
    formError = '';
    notice = '';
    actionError = '';
    // Refresh on every open: a dataset created from another tab must be
    // selectable without a reload.
    void loadDatasets();
    showModal = true;
  }

  async function startReplication() {
    formError = '';
    const invalid = pairError(source, destination, pruneKeep, encryptedSource);
    if (invalid) {
      formError = invalid;
      return;
    }
    saving = true;
    try {
      await api('/api/replication/send', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          source,
          destination,
          snapshot: snapshot.trim() || undefined,
          recursive,
          pruneKeep: Number(pruneKeep) || 0
        })
      });
      showModal = false;
      notice = `Replication started: ${source} → ${destination}.`;
      await loadJobs();
      // A raw first send creates the destination; refresh so it is selectable
      // as an existing replica on the next run.
      void loadDatasets();
    } catch (e) {
      formError =
        e instanceof ApiError
          ? e.message
          : 'Could not start replication. Check the API connection and try again.';
    } finally {
      saving = false;
    }
  }

  async function cancelJob(job: ReplicationJob) {
    if (
      !(await confirmAction({
        title: 'Cancel replication',
        message: `Cancel ${job.source} → ${job.destination}?\n\nSnapshots already received stay on the destination.`,
        confirmLabel: 'Cancel job',
        cancelLabel: 'Keep running',
        tone: 'danger'
      }))
    ) {
      return;
    }
    cancelingId = job.id;
    actionError = '';
    try {
      await api(`/api/replication/jobs/${encodeURIComponent(job.id)}`, { method: 'DELETE' });
      await loadJobs();
    } catch (e) {
      actionError =
        e instanceof ApiError ? e.message : 'Could not cancel the job. Check the API connection.';
    } finally {
      cancelingId = '';
    }
  }

  onMount(() => {
    void Promise.all([loadDatasets(), loadJobs()]).finally(() => (loading = false));
    pollTimer = setInterval(() => {
      if (document.visibilityState === 'visible') void loadJobs(true);
    }, 3000);
  });

  onDestroy(() => {
    if (pollTimer) clearInterval(pollTimer);
  });
</script>

<div class="mx-auto max-w-6xl">
  <PageHeader
    title="Replication"
    subtitle="Copy a dataset to another dataset on this node with zfs send/receive. Incrementals are matched by snapshot GUID; the destination is a replica target."
    breadcrumbs={[{ label: 'Storage' }, { label: 'Replication' }]}
  >
    <div slot="actions" class="flex flex-wrap gap-2">
      <Button variant="secondary" loading={refreshing} on:click={() => loadJobs()}>Refresh</Button>
      <Button on:click={openCreate}>New replication</Button>
    </div>
  </PageHeader>

  <div class="card mb-6 text-sm text-gray-400">
    <p>
      Local replication keeps a second dataset in sync with the first. Remote replication, rsync
      and cloud sync are not implemented yet; use <a class="text-naslos-link underline" href="/backups"
        >Buddy</a
      >
      for off-site copies. Jobs are held in memory and disappear from this list when the API
      restarts, but the received snapshots stay on the destination.
    </p>
  </div>

  {#if notice}
    <div role="status" class="callout callout-ok">{notice}</div>
  {/if}
  {#if actionError}
    <div role="alert" class="callout callout-bad">{actionError}</div>
  {/if}
  {#if jobsLoaded && jobsError}
    <div role="alert" class="callout callout-bad">
      Could not refresh the job list; showing the last known state. {jobsError}
    </div>
  {/if}

  <AsyncState
    {loading}
    error={jobsLoaded ? '' : jobsError}
    loadingText="Loading replication jobs…"
    onRetry={() => loadJobs()}
  >
    <section class="card mb-6" aria-labelledby="replication-running">
      <div class="mb-4 flex items-center justify-between">
        <h2 id="replication-running" class="text-xl font-bold">Running</h2>
        <span class="text-xs text-gray-500">{runningJobs.length} active</span>
      </div>
      {#if runningJobs.length === 0}
        <p class="text-sm text-gray-400">No replication is running.</p>
      {:else}
        <div class="space-y-3">
          {#each runningJobs as job (job.id)}
            {@const percent = progressPercent(job)}
            <div class="rounded-lg bg-naslos-dark p-3">
              <div class="flex flex-wrap items-start justify-between gap-2">
                <div class="min-w-0">
                  <div class="truncate font-medium">{job.source} → {job.destination}</div>
                  <div class="text-xs text-gray-400">
                    {job.raw ? 'raw encrypted' : job.incremental ? 'incremental' : 'full'}{job.recursive
                      ? ' · recursive'
                      : ''}
                    {#if job.snapshot}· target {job.snapshot}{/if}
                    {#if job.pruneKeep}· keep {job.pruneKeep}{/if}
                    · started {fmtTime(job.startedAt)}
                  </div>
                </div>
                <div class="flex items-center gap-2">
                  <StatusBadge state={job.state} tone={jobTone(job)} />
                  <Button
                    variant="danger"
                    size="sm"
                    loading={cancelingId === job.id}
                    disabled={cancelingId !== '' && cancelingId !== job.id}
                    on:click={() => cancelJob(job)}
                  >
                    Cancel
                  </Button>
                </div>
              </div>
              <div class="mt-3">
                <div
                  class="h-3 w-full overflow-hidden rounded-full bg-naslos-border"
                  role="progressbar"
                  aria-label={`Replication progress for ${job.source} to ${job.destination}`}
                  aria-valuemin="0"
                  aria-valuemax="100"
                  aria-valuenow={percent ?? undefined}
                  aria-valuetext={percent === null ? 'estimating' : `${percent}%`}
                >
                  <div
                    class="h-3 bg-naslos-primary transition-all"
                    style={`width: ${percent !== null ? Math.max(2, percent) : 8}%`}
                  ></div>
                </div>
                <p class="mt-1 text-xs text-gray-400">
                  {formatBytes(job.progress?.bytes ?? 0)} sent
                  {#if (job.progress?.estimatedBytes ?? 0) > 0}
                    of {formatBytes(job.progress.estimatedBytes)} ({percent}%)
                  {:else}
                    · estimating
                  {/if}
                </p>
              </div>
            </div>
          {/each}
        </div>
      {/if}
    </section>

    <section class="card" aria-labelledby="replication-history">
      <div class="mb-4 flex items-center justify-between">
        <h2 id="replication-history" class="text-xl font-bold">History</h2>
        <span class="text-xs text-gray-500">the last 20 finished jobs</span>
      </div>
      {#if finishedJobs.length === 0}
        <p class="text-sm text-gray-400">
          No finished jobs. Start one with <strong>New replication</strong>; the destination
          dataset must already exist.
        </p>
      {:else}
        <div class="space-y-2">
          {#each finishedJobs as job (job.id)}
            <div class="rounded-lg bg-naslos-dark p-3">
              <div class="flex flex-wrap items-start justify-between gap-2">
                <div class="min-w-0">
                  <div class="truncate font-medium">{job.source} → {job.destination}</div>
                  <div class="text-xs text-gray-400">
                    {job.raw ? 'raw encrypted' : job.incremental ? 'incremental' : 'full'}{job.recursive
                      ? ' · recursive'
                      : ''}
                    {#if job.snapshot}· {job.snapshot}{/if}
                    · {fmtTime(job.startedAt)}{#if job.finishedAt} → {fmtTime(job.finishedAt)}{/if}
                  </div>
                </div>
                <StatusBadge state={job.state} tone={jobTone(job)} />
              </div>

              {#if job.state === 'succeeded' && job.result}
                <div class="mt-2 text-xs text-gray-300">
                  <strong>{job.result.status}</strong>
                  {#if job.result.bytes > 0}· {formatBytes(job.result.bytes)} sent{/if}
                  · {job.result.raw
                    ? 'raw encrypted'
                    : job.result.incremental
                      ? 'incremental'
                      : 'full'}
                  · {job.result.durationSeconds.toFixed(1)}s
                  {#if job.result.baseSnapshot}· base {job.result.baseSnapshot}{/if}
                  {#if job.result.prunedSnapshots > 0}· pruned {job.result.prunedSnapshots}{/if}
                </div>
                {#if job.result.warnings?.length}
                  <ul class="mt-1 list-inside list-disc text-xs text-amber-300">
                    {#each job.result.warnings as warning}<li>{warning}</li>{/each}
                  </ul>
                {/if}
                {#if job.result.retentionError}
                  <p class="mt-1 text-xs text-amber-300">
                    Retention: {job.result.retentionError}
                  </p>
                {/if}
              {:else if job.error}
                <p class="mt-2 break-words text-xs text-red-300">{job.error}</p>
              {/if}
            </div>
          {/each}
        </div>
      {/if}
    </section>
  </AsyncState>
</div>

{#if showModal}
  <Modal title="New replication" size="lg" onClose={() => (showModal = false)}>
    <p class="text-sm text-gray-400">
      Without a snapshot, the job creates a <code class="font-mono text-xs">naslos-rep-…</code>
      snapshot on the source.
    </p>
    {#if encryptedSource}
      <div class="rounded-lg border border-naslos-border bg-naslos-dark p-3 text-sm text-gray-300">
        The source is encrypted, so it is sent <strong>raw</strong>
        (<code class="font-mono text-xs">zfs send -w</code>): the data is never decrypted by this
        node. The first run <strong>creates</strong> the destination encrypted with the source key
        and unlocks it; later runs receive incrementals into that same replica. The replica keeps
        the source key, so after a reboot it stays locked until you unlock it like any encrypted
        dataset.
      </div>
    {:else}
      <p class="text-sm text-gray-400">
        Both datasets must already exist. A full send <strong>replaces the destination's
        contents</strong>, so treat the destination as a replica target.
      </p>
    {/if}

    {#if datasetsError}
      <ErrorState message={datasetsError} retry={loadDatasets} />
    {/if}

    <form id="replication-form" on:submit|preventDefault={startReplication} class="space-y-4">
      <FormField label="Source dataset" id="replication-source" required>
        <select id="replication-source" class="input w-full" bind:value={source} required>
          <option value="">Choose a source…</option>
          {#each pairDatasets as d}
            <option value={d.name}>{d.name}{isEncryptedDataset(d) ? ' (encrypted)' : ''}</option>
          {/each}
        </select>
      </FormField>

      <FormField label="Destination dataset" id="replication-destination" required>
        {#if encryptedSource}
          <input
            id="replication-destination"
            type="text"
            bind:value={destination}
            placeholder="e.g. backup/encrypted-replica"
            class="input w-full"
            required
          />
        {:else}
          <select id="replication-destination" class="input w-full" bind:value={destination} required>
            <option value="">Choose a destination…</option>
            {#each pairDatasets as d}
              <option value={d.name}>{d.name}</option>
            {/each}
          </select>
        {/if}
      </FormField>
      <p class="-mt-2 text-xs text-gray-500">
        {#if encryptedSource}
          The first run creates this dataset (its parent must exist). A full raw stream cannot
          overwrite an existing dataset; later runs use the same existing replica name.
        {:else}
          The destination is not created here; add one from its pool page first.
        {/if}
      </p>

      <FormField
        label="Snapshot (optional)"
        id="replication-snapshot"
        hint="Must be an existing snapshot on the source. Leave empty to let the job create one."
      >
        <input
          id="replication-snapshot"
          type="text"
          bind:value={snapshot}
          placeholder="e.g. naslos-2026-01-05_10-15-00"
          class="input w-full"
        />
      </FormField>

      <FormField
        label="Keep newest on the destination"
        id="replication-keep"
        hint="0 keeps every replication snapshot. Retention only removes snapshots this pair created."
      >
        <input
          id="replication-keep"
          type="number"
          min="0"
          max="1000"
          bind:value={pruneKeep}
          class="input w-full"
        />
      </FormField>

      <label class="flex cursor-pointer items-start gap-3">
        <input type="checkbox" class="mt-1" bind:checked={recursive} />
        <span>
          Include child datasets (recursive send)
          <span class="block text-xs text-gray-500">
            Uses <code class="font-mono text-xs">zfs send -r</code>. A recursive job also verifies
            descendant replicas and repairs a missing child subtree.
          </span>
        </span>
      </label>

      {#if validationError && (source || destination)}
        <p class="text-xs text-amber-300">{validationError}</p>
      {:else if validationError && !source && !destination}
        <p class="text-xs text-gray-500">Choose both datasets to continue.</p>
      {/if}

      {#if formError}
        <div role="alert" class="callout callout-bad mb-0">{formError}</div>
      {/if}
    </form>

    <div slot="footer">
      <Button variant="secondary" on:click={() => (showModal = false)}>Cancel</Button>
      <Button
        type="submit"
        form="replication-form"
        loading={saving}
        disabled={!!validationError}
      >
        Start replication
      </Button>
    </div>
  </Modal>
{/if}
