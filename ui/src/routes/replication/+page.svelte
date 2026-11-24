<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import LoadingState from '$lib/components/LoadingState.svelte';
  import Modal from '$lib/components/Modal.svelte';
  import PageHeader from '$lib/components/PageHeader.svelte';
  import StatusBadge from '$lib/components/StatusBadge.svelte';
  import { formatBytes } from '$lib/format';

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
  }

  let loading = true;
  let datasets: Dataset[] = [];
  let datasetsError = '';
  let jobs: ReplicationJob[] = [];
  let jobsError = '';

  let showModal = false;
  let saving = false;
  let formError = '';
  let notice = '';

  let source = '';
  let destination = '';
  let snapshot = '';
  let recursive = false;
  let pruneKeep = 0;

  let pollTimer: ReturnType<typeof setInterval> | null = null;
  let jobsInFlight = false;

  // A replication pair is two existing datasets; the pool root itself is not a
  // valid endpoint (the API requires `<pool>/<name>`).
  $: pairDatasets = datasets.filter((d) => d.name.includes('/'));
  $: runningJobs = jobs.filter((j) => j.state === 'running');
  $: finishedJobs = jobs.filter((j) => j.state !== 'running').slice().reverse();

  function fmtTime(s?: string): string {
    if (!s) return '—';
    const d = new Date(s);
    return isNaN(d.getTime()) ? s : d.toLocaleString();
  }

  function jobTone(state: string): 'ok' | 'warn' | 'bad' | 'info' | 'neutral' {
    switch (state) {
      case 'running':
        return 'info';
      case 'succeeded':
        return 'ok';
      case 'failed':
        return 'bad';
      case 'cancelled':
        return 'warn';
      default:
        return 'neutral';
    }
  }

  function progressPercent(job: ReplicationJob): number | null {
    const estimated = job.progress?.estimatedBytes || job.estimatedBytes || 0;
    const bytes = job.progress?.bytes || 0;
    if (estimated <= 0) return null;
    return Math.max(0, Math.min(100, Math.round((bytes / estimated) * 100)));
  }

  // Svelte's legacy reactivity only tracks variables named in the statement
  // itself, so the fields are passed in rather than read inside the function.
  function pairError(src: string, dst: string, keep: number): string {
    if (!src || !dst) return 'Choose a source and a destination dataset.';
    if (src === dst) return 'The source and the destination must be different datasets.';
    if (dst.startsWith(`${src}/`)) {
      return 'The destination cannot be inside the source dataset.';
    }
    if (src.startsWith(`${dst}/`)) {
      return 'The source cannot be inside the destination dataset.';
    }
    if (!Number.isInteger(keep) || keep < 0 || keep > 1000) {
      return 'Retention must be a whole number from 0 to 1000 (0 keeps every replication snapshot).';
    }
    return '';
  }

  $: validationError = pairError(source, destination, pruneKeep);

  async function loadDatasets() {
    try {
      const res = await fetch('/api/datasets');
      if (!res.ok) {
        datasetsError = `Failed to load datasets (HTTP ${res.status})`;
        return;
      }
      const data = await res.json();
      datasets = Array.isArray(data) ? data : [];
      datasetsError = '';
    } catch {
      datasetsError = 'Failed to connect to the API.';
    }
  }

  async function loadJobs() {
    if (jobsInFlight) return;
    jobsInFlight = true;
    try {
      const res = await fetch('/api/replication/jobs');
      if (!res.ok) {
        jobsError = `Failed to load replication jobs (HTTP ${res.status})`;
        return;
      }
      const data = await res.json();
      jobs = Array.isArray(data.jobs) ? data.jobs : [];
      jobsError = '';
    } catch {
      jobsError = 'Failed to connect to the API.';
    } finally {
      jobsInFlight = false;
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
    // Refresh on every open: a dataset created from another tab must be
    // selectable without a reload.
    void loadDatasets();
    showModal = true;
  }

  async function startReplication() {
    formError = '';
    const invalid = pairError(source, destination, pruneKeep);
    if (invalid) {
      formError = invalid;
      return;
    }
    saving = true;
    try {
      const res = await fetch('/api/replication/send', {
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
      const data = await res.json().catch(() => ({}));
      if (!res.ok) throw new Error(data.error || `Could not start replication (HTTP ${res.status})`);
      showModal = false;
      notice = `Replication started: ${source} → ${destination}.`;
      await loadJobs();
    } catch (e: any) {
      formError = e.message;
    } finally {
      saving = false;
    }
  }

  async function cancelJob(job: ReplicationJob) {
    if (!confirm(`Cancel replication ${job.source} → ${job.destination}?\n\nSnapshots already received stay on the destination.`)) return;
    try {
      const res = await fetch(`/api/replication/jobs/${encodeURIComponent(job.id)}`, {
        method: 'DELETE'
      });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) throw new Error(data.error || `Could not cancel the job (HTTP ${res.status})`);
      await loadJobs();
    } catch (e: any) {
      alert(e.message);
    }
  }

  onMount(() => {
    void Promise.all([loadDatasets(), loadJobs()]).finally(() => (loading = false));
    pollTimer = setInterval(() => {
      if (document.visibilityState === 'visible') void loadJobs();
    }, 3000);
  });

  onDestroy(() => {
    if (pollTimer) clearInterval(pollTimer);
  });
</script>

<div class="max-w-6xl mx-auto">
  <PageHeader
    title="Replication"
    subtitle="Copy a dataset to another dataset on this node with zfs send/receive. Incrementals are matched by snapshot GUID; the destination is a replica target."
  >
    <div slot="actions" class="flex gap-2">
      <button class="btn btn-secondary" on:click={loadJobs}>Refresh</button>
      <button class="btn btn-primary" on:click={openCreate}>New replication</button>
    </div>
  </PageHeader>

  <div class="card mb-6 text-sm text-gray-400">
    <p>
      Local replication keeps a second dataset in sync with the first. Remote replication,
      rsync and cloud sync are not implemented yet; use <a class="text-naslos-link underline" href="/backups">Buddy</a>
      for off-site copies. Jobs are held in memory and disappear from this list when the API
      restarts, but the received snapshots stay on the destination.
    </p>
  </div>

  {#if notice}
    <div role="status" class="callout callout-ok mb-4">{notice}</div>
  {/if}
  {#if datasetsError}
    <div role="alert" class="callout callout-bad mb-4">{datasetsError}</div>
  {/if}
  {#if jobsError}
    <div role="alert" class="callout callout-bad mb-4">{jobsError}</div>
  {/if}

  {#if loading}
    <LoadingState label="Loading replication jobs…" />
  {:else}
    <section class="card mb-6" aria-labelledby="replication-running">
      <div class="flex items-center justify-between mb-4">
        <h2 id="replication-running" class="text-xl font-bold">Running</h2>
        <span class="text-xs text-gray-500">{runningJobs.length} active</span>
      </div>
      {#if runningJobs.length === 0}
        <p class="text-sm text-gray-400">No replication is running.</p>
      {:else}
        <div class="space-y-3">
          {#each runningJobs as job (job.id)}
            {@const percent = progressPercent(job)}
            <div class="bg-naslos-dark rounded-lg p-3">
              <div class="flex flex-wrap items-start justify-between gap-2">
                <div class="min-w-0">
                  <div class="font-medium truncate">{job.source} → {job.destination}</div>
                  <div class="text-xs text-gray-400">
                    {job.incremental ? 'incremental' : 'full'}{job.recursive ? ' · recursive' : ''}
                    {#if job.snapshot}· target {job.snapshot}{/if}
                    {#if job.pruneKeep}· keep {job.pruneKeep}{/if}
                    · started {fmtTime(job.startedAt)}
                  </div>
                </div>
                <div class="flex items-center gap-2">
                  <StatusBadge state={job.state} tone={jobTone(job.state)} />
                  <button class="btn btn-danger btn-sm" on:click={() => cancelJob(job)}>Cancel</button>
                </div>
              </div>
              <div class="mt-3">
                <div
                  class="w-full bg-naslos-border rounded-full h-3 overflow-hidden"
                  role="progressbar"
                  aria-label={`Replication progress for ${job.source} to ${job.destination}`}
                  aria-valuemin="0"
                  aria-valuemax="100"
                  aria-valuenow={percent ?? undefined}
                  aria-valuetext={percent === null ? 'estimating' : `${percent}%`}
                >
                  <div
                    class="bg-naslos-primary h-3 transition-all"
                    style={`width: ${percent !== null ? Math.max(2, percent) : 8}%`}
                  ></div>
                </div>
                <p class="text-xs text-gray-400 mt-1">
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
      <div class="flex items-center justify-between mb-4">
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
            <div class="bg-naslos-dark rounded-lg p-3">
              <div class="flex flex-wrap items-start justify-between gap-2">
                <div class="min-w-0">
                  <div class="font-medium truncate">{job.source} → {job.destination}</div>
                  <div class="text-xs text-gray-400">
                    {job.incremental ? 'incremental' : 'full'}{job.recursive ? ' · recursive' : ''}
                    {#if job.snapshot}· {job.snapshot}{/if}
                    · {fmtTime(job.startedAt)}{#if job.finishedAt} → {fmtTime(job.finishedAt)}{/if}
                  </div>
                </div>
                <StatusBadge state={job.state} tone={jobTone(job.state)} />
              </div>

              {#if job.state === 'succeeded' && job.result}
                <div class="text-xs text-gray-300 mt-2">
                  <strong>{job.result.status}</strong>
                  {#if job.result.bytes > 0}· {formatBytes(job.result.bytes)} sent{/if}
                  · {job.result.incremental ? 'incremental' : 'full'}
                  · {job.result.durationSeconds.toFixed(1)}s
                  {#if job.result.baseSnapshot}· base {job.result.baseSnapshot}{/if}
                  {#if job.result.prunedSnapshots > 0}· pruned {job.result.prunedSnapshots}{/if}
                </div>
                {#if job.result.warnings?.length}
                  <ul class="text-xs text-yellow-400/90 mt-1 list-disc list-inside">
                    {#each job.result.warnings as warning}<li>{warning}</li>{/each}
                  </ul>
                {/if}
                {#if job.result.retentionError}
                  <p class="text-xs text-yellow-400/90 mt-1">Retention: {job.result.retentionError}</p>
                {/if}
              {:else if job.error}
                <p class="text-xs text-red-300 mt-2 break-words">{job.error}</p>
              {/if}
            </div>
          {/each}
        </div>
      {/if}
    </section>
  {/if}
</div>

{#if showModal}
  <Modal title="New replication" onClose={() => (showModal = false)}>
    <p class="text-sm text-gray-400">
      Both datasets must already exist. Without a snapshot, the job creates a
      <code class="text-xs">naslos-rep-…</code> snapshot on the source. A full send
      <strong>replaces the destination's contents</strong>, so treat the destination as a replica
      target.
    </p>

    <div>
      <label class="label" for="replication-source">Source dataset *</label>
      <select id="replication-source" class="input w-full" bind:value={source}>
        <option value="">Choose a source…</option>
        {#each pairDatasets as d}
          <option value={d.name}>{d.name}</option>
        {/each}
      </select>
    </div>

    <div>
      <label class="label" for="replication-destination">Destination dataset *</label>
      <select id="replication-destination" class="input w-full" bind:value={destination}>
        <option value="">Choose a destination…</option>
        {#each pairDatasets as d}
          <option value={d.name}>{d.name}</option>
        {/each}
      </select>
      <p class="text-xs text-gray-500 mt-1">
        The destination is not created here; add one from its pool page first.
      </p>
    </div>

    <div>
      <label class="label" for="replication-snapshot">Snapshot (optional)</label>
      <input
        id="replication-snapshot"
        type="text"
        bind:value={snapshot}
        placeholder="e.g. naslos-2026-01-05_10-15-00"
        class="input w-full"
      />
      <p class="text-xs text-gray-500 mt-1">
        Must be an existing snapshot on the source. Leave empty to let the job create one.
      </p>
    </div>

    <div>
      <label class="label" for="replication-keep">Keep newest on the destination</label>
      <input
        id="replication-keep"
        type="number"
        min="0"
        max="1000"
        bind:value={pruneKeep}
        class="input w-full"
      />
      <p class="text-xs text-gray-500 mt-1">
        0 keeps every replication snapshot. Retention only removes snapshots this pair created.
      </p>
    </div>

    <label class="flex items-start gap-3 cursor-pointer">
      <input type="checkbox" class="mt-1" bind:checked={recursive} />
      <span>
        Include child datasets (recursive send)
        <span class="block text-xs text-gray-500">
          Uses <code class="text-xs">zfs send -r</code>. A recursive job also verifies descendant
          replicas and repairs a missing child subtree.
        </span>
      </span>
    </label>

    {#if validationError && (source || destination)}
      <p class="text-xs text-yellow-400">{validationError}</p>
    {:else if validationError && !source && !destination}
      <p class="text-xs text-gray-500">Choose both datasets to continue.</p>
    {/if}

    {#if formError}
      <div role="alert" class="p-3 rounded-lg bg-red-900/30 border border-red-700 text-red-300 text-sm">
        {formError}
      </div>
    {/if}

    <div slot="footer">
      <button class="btn btn-secondary" on:click={() => (showModal = false)}>Cancel</button>
      <button
        class="btn btn-primary"
        on:click={startReplication}
        disabled={saving || !!validationError}
      >
        {saving ? 'Starting…' : 'Start replication'}
      </button>
    </div>
  </Modal>
{/if}
