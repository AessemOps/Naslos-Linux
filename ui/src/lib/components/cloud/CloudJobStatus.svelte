<script lang="ts">
  // Live status of one cloud job (send, restore or verify): a progressbar with
  // aria-valuetext while running, the result or error afterwards, and a Cancel
  // button that the owner wires to DELETE /api/cloud/jobs/{id}.
  import { createEventDispatcher } from 'svelte';
  import Button from '$lib/components/Button.svelte';
  import StatusBadge from '$lib/components/StatusBadge.svelte';
  import { formatBytes } from '$lib/format';
  import type { CloudJob } from '$lib/cloud/types';

  export let job: CloudJob;
  export let label = 'Job progress';
  export let cancelling = false;

  const dispatch = createEventDispatcher<{ cancel: void }>();

  const verbs: Record<string, string> = { send: 'uploaded', restore: 'restored', verify: 'checked' };

  $: p = job.progress ?? { chunk: 0, plainBytes: 0, uploaded: 0, skipped: 0 };
  $: total = p.totalChunks && p.totalChunks > 0 ? p.totalChunks : 0;
  $: percent = job.state === 'running' && total ? Math.min(100, Math.round((p.chunk / total) * 100)) : null;
  $: linkText = p.links && p.links > 0 ? `link ${p.link ?? 0} of ${p.links}, ` : '';
  $: valueText =
    percent == null
      ? `${linkText}chunk ${p.chunk}, ${formatBytes(p.plainBytes)} ${verbs[job.kind] ?? 'processed'}`
      : `${percent}% (${linkText}chunk ${p.chunk} of ${total})`;
  $: missing = job.result?.missingIndices ?? {};
  $: bad = job.result?.badIndices ?? {};
  $: missingEntries = Object.entries(missing).filter(([, v]) => v.length > 0);
  $: badEntries = Object.entries(bad).filter(([, v]) => v.length > 0);
</script>

<div class="mt-3 space-y-2 text-sm" data-testid="cloud-job">
  <p role="status">
    {job.kind} job <code class="text-naslos-link">{job.id}</code>:
    <StatusBadge state={job.state} tone={job.state === 'running' ? 'info' : ''} />
  </p>

  {#if job.state === 'running'}
    <div
      class="h-3 w-full overflow-hidden rounded border border-naslos-border bg-naslos-dark"
      role="progressbar"
      aria-label={label}
      aria-valuemin="0"
      aria-valuemax="100"
      aria-valuenow={percent ?? undefined}
      aria-valuetext={valueText}
    >
      {#if percent != null}
        <div class="h-3 bg-naslos-primary transition-all" style={`width: ${percent}%`}></div>
      {:else}
        <div class="job-progress h-3 w-1/3"></div>
      {/if}
    </div>
    <p class="text-xs text-gray-400">
      {valueText} · {p.uploaded} uploaded · {p.skipped} skipped
    </p>
    <Button size="sm" variant="danger" loading={cancelling} on:click={() => dispatch('cancel')}>
      Cancel job
    </Button>
  {:else if job.state === 'failed' && job.error}
    <p role="alert" class="text-red-300">{job.error}</p>
  {:else if job.state === 'cancelled'}
    <p class="text-gray-400">The job was cancelled.</p>
  {/if}

  {#if job.state !== 'running' && job.result}
    {#if job.kind === 'verify'}
      {#if missingEntries.length === 0 && badEntries.length === 0}
        <p class="text-gray-300">All chunks are present{job.state === 'succeeded' ? ' and intact' : ''}.</p>
      {/if}
    {:else}
      <p class="text-xs text-gray-400">
        Chain <code>{job.result.chain}</code> ({job.result.kind}) · snapshot
        <code>{job.result.snapshot}</code> · {formatBytes(job.result.plainBytes)}
      </p>
    {/if}
  {/if}

  {#if missingEntries.length > 0 || badEntries.length > 0}
    <div role="alert" class="rounded border border-red-700 bg-red-900/20 p-2 text-red-300">
      {#each missingEntries as [chain, indices] (chain)}
        <p>Chain <code>{chain}</code>: missing chunk {indices.length === 1 ? 'index' : 'indices'} {indices.join(', ')}</p>
      {/each}
      {#each badEntries as [chain, indices] (chain)}
        <p>Chain <code>{chain}</code>: corrupt chunk {indices.length === 1 ? 'index' : 'indices'} {indices.join(', ')}</p>
      {/each}
    </div>
  {/if}
</div>
