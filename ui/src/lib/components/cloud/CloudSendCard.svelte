<script lang="ts">
  // One-off "send now" to a cloud destination, with live progress and cancel.
  // The page owns the API call and the poller; this card is the form and view.
  import { createEventDispatcher } from 'svelte';
  import Button from '$lib/components/Button.svelte';
  import FormField from '$lib/components/FormField.svelte';
  import CloudJobStatus from './CloudJobStatus.svelte';
  import type { CloudJob, DatasetSummary, Destination } from '$lib/cloud/types';

  export let datasets: DatasetSummary[] = [];
  export let destinations: Destination[] = [];
  export let job: CloudJob | null = null;
  export let jobError = '';
  export let starting = false;
  export let cancelling = false;
  /** False until the recovery-key acknowledgement is given. */
  export let allowed = true;

  const dispatch = createEventDispatcher<{
    send: { dataset: string; destination: string; forceFull: boolean };
    cancel: void;
  }>();

  let dataset = '';
  let destination = '';
  let forceFull = false;

  $: enabledDestinations = destinations.filter((d) => d.enabled);
  $: running = job?.state === 'running';
  $: canSend = allowed && !!dataset && !!destination && !starting && !running;
</script>

<section class="card mb-6" aria-labelledby="cloud-send-heading">
  <h2 id="cloud-send-heading" class="mb-2 text-lg font-bold">Send now</h2>
  {#if !allowed}
    <p class="mb-3 text-sm text-amber-300">Confirm the recovery keys above before the first send.</p>
  {/if}
  <form
    class="space-y-3"
    on:submit|preventDefault={() => canSend && dispatch('send', { dataset, destination, forceFull })}
  >
    <div class="grid grid-cols-1 gap-3 md:grid-cols-2">
      <FormField label="Dataset" id="cloud-send-dataset" required>
        <select id="cloud-send-dataset" class="input w-full" bind:value={dataset}>
          <option value="">Dataset…</option>
          {#each datasets as d (d.name)}
            <option value={d.name}>{d.name}</option>
          {/each}
        </select>
      </FormField>
      <FormField label="Destination" id="cloud-send-destination" required>
        <select id="cloud-send-destination" class="input w-full" bind:value={destination}>
          <option value="">Destination…</option>
          {#each enabledDestinations as d (d.id)}
            <option value={d.id}>{d.name}</option>
          {/each}
        </select>
      </FormField>
    </div>
    <label class="flex items-center gap-2 text-sm">
      <input id="cloud-send-force-full" type="checkbox" class="h-4 w-4 rounded" bind:checked={forceFull} />
      Force a full send (start a new lineage)
    </label>
    <Button type="submit" loading={starting} disabled={!canSend}>
      {starting ? 'Starting…' : 'Send now'}
    </Button>
  </form>

  {#if jobError}
    <p role="alert" class="mt-2 text-sm text-red-300">{jobError}</p>
  {/if}

  {#if job}
    <CloudJobStatus {job} label="Upload progress" {cancelling} on:cancel={() => dispatch('cancel')} />
  {/if}
</section>
