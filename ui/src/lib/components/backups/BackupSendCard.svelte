<script lang="ts">
  import { createEventDispatcher } from 'svelte';
  import Button from '$lib/components/Button.svelte';
  import FormField from '$lib/components/FormField.svelte';
  import { formatBytes } from '$lib/format';
  import type { BackupJob, DatasetSummary } from './types';

  export let datasets: DatasetSummary[] = [];
  export let activeJob: BackupJob | null = null;
  export let jobError = '';
  export let starting = false;

  const dispatch = createEventDispatcher<{
    send: { dataset: string; receiver: string; source: string };
    cancel: void;
  }>();

  let adhoc = { dataset: '', receiver: '', source: '' };

  $: plainBytes = activeJob?.progress?.plainBytes ?? 0;
  // A determinate bar needs the estimate; without one the bar is honestly
  // indeterminate instead of an invented 8 %.
  $: percent =
    activeJob?.state === 'running' && activeJob.estimatedBytes && activeJob.estimatedBytes > 0
      ? Math.min(100, Math.round((plainBytes / activeJob.estimatedBytes) * 100))
      : null;
</script>

<div class="card mb-6">
  <h2 class="mb-2 text-lg font-bold">Back up now</h2>
  <form
    class="space-y-3"
    on:submit|preventDefault={() => dispatch('send', { ...adhoc })}
  >
    <div class="grid grid-cols-1 gap-3 md:grid-cols-3">
      <FormField label="Dataset" id="send-dataset" required>
        <select id="send-dataset" bind:value={adhoc.dataset} class="input w-full" required>
          <option value="">Dataset…</option>
          {#each datasets as d (d.name)}
            <option value={d.name}>{d.name}</option>
          {/each}
        </select>
      </FormField>
      <FormField label="Buddy URL" id="send-receiver" required>
        <input
          id="send-receiver"
          type="text"
          bind:value={adhoc.receiver}
          placeholder="Buddy URL https://…"
          class="input w-full"
          required
        />
      </FormField>
      <FormField label="Source name" id="send-source" required>
        <input
          id="send-source"
          type="text"
          bind:value={adhoc.source}
          placeholder="Source name"
          class="input w-full"
          required
        />
      </FormField>
    </div>
    <Button type="submit" loading={starting} disabled={starting}>
      {starting ? 'Starting...' : 'Start backup'}
    </Button>
  </form>

  {#if jobError}
    <p role="alert" class="mt-2 text-sm text-red-300">{jobError}</p>
  {/if}

  {#if activeJob}
    <div class="mt-3 text-sm">
      <p>
        Job <code class="text-naslos-link">{activeJob.id}</code>:
        <span class="font-medium">{activeJob.state}</span>
        {#if activeJob.incremental}
          <span class="badge badge-info ml-2">incremental</span>
        {/if}
        {#if activeJob.resumed || activeJob.result?.resumed}
          <span class="badge badge-neutral ml-2">resumed</span>
        {/if}
      </p>
      {#if activeJob.state === 'running'}
        <div
          class="mt-2 h-3 w-full overflow-hidden rounded border border-naslos-border bg-naslos-dark"
          role="progressbar"
          aria-label="Backup progress"
          aria-valuemin="0"
          aria-valuemax="100"
          aria-valuenow={percent ?? undefined}
          aria-valuetext={percent == null ? 'Backup in progress' : `${percent}%`}
        >
          {#if percent != null}
            <div class="h-3 bg-naslos-primary transition-all" style={`width: ${percent}%`}></div>
          {:else}
            <div class="job-progress h-3 w-1/3"></div>
          {/if}
        </div>
        <p class="mt-1 text-xs text-gray-400">
          {formatBytes(plainBytes)} sent · {activeJob.progress?.uploaded ?? 0} uploaded · {activeJob.progress?.skipped ?? 0} skipped
        </p>
        <div class="mt-2">
          <Button size="sm" variant="danger" on:click={() => dispatch('cancel')}>Cancel</Button>
        </div>
      {:else if activeJob.result}
        <p class="mt-1 text-xs text-gray-400">
          Chain {activeJob.result.chain}: {activeJob.result.chunks} chunks, {formatBytes(activeJob.result.plainBytes)}.
        </p>
      {:else if activeJob.error}
        <p role="alert" class="mt-1 text-sm text-red-300">{activeJob.error}</p>
      {/if}
    </div>
  {/if}
</div>
