<script lang="ts">
  // Restore a stored chain into a NEW dataset (WP-1 never restores in place).
  // The restore is confirmed by typing the target name; for raw chains of an
  // encrypted source a write-only dataset key may be supplied when its key is
  // not stored on this NAS.
  import { createEventDispatcher } from 'svelte';
  import { api, ApiError } from '$lib/api';
  import Button from '$lib/components/Button.svelte';
  import FormField from '$lib/components/FormField.svelte';
  import Modal from '$lib/components/Modal.svelte';
  import { confirmAction } from '$lib/stores/confirm';
  import type { Chain, DatasetSummary, StartedJob } from '$lib/cloud/types';

  export let destination: string;
  export let destinationName = '';
  export let source: string;
  export let chain: Chain;
  /** Every existing dataset (pool roots included), to refuse existing names. */
  export let datasets: DatasetSummary[] = [];

  const dispatch = createEventDispatcher<{ started: string; close: void }>();

  const pool = source.includes('/') ? source.split('/')[0] : '';
  let target = pool ? `${pool}/${source.split('/').slice(1).join('-')}-restored` : '';
  let keyHex = '';
  let keepPartial = false;
  let submitted = false;
  let starting = false;
  let error = '';

  $: names = new Set(datasets.map((d) => d.name));
  $: targetError = validateTarget(target.trim(), names);
  $: keyError = keyHex && !/^[0-9a-fA-F]{64}$/.test(keyHex.trim()) ? 'The key must be 64 hex characters (32 bytes).' : '';
  $: shownTarget = submitted ? targetError : '';

  function validateTarget(name: string, existing: Set<string>): string {
    if (!name) return 'Enter the name of the new dataset.';
    if (!/^[A-Za-z0-9][A-Za-z0-9_.:-]*(\/[A-Za-z0-9][A-Za-z0-9_.:-]*)+$/.test(name)) {
      return 'Use <pool>/<name> (letters, digits, - _ . :).';
    }
    if (existing.has(name)) return 'This dataset already exists. Restore only creates a new dataset.';
    const parent = name.slice(0, name.lastIndexOf('/'));
    if (!existing.has(parent)) return `The parent ${parent} does not exist.`;
    return '';
  }

  async function restore() {
    submitted = true;
    error = '';
    if (targetError || keyError) return;
    const name = target.trim();
    const ok = await confirmAction({
      title: 'Restore backup',
      message: `Restore ${source} up to ${chain.toSnapshot} from ${destinationName || destination} into the new dataset ${name}. Type the dataset name to confirm.`,
      confirmLabel: 'Restore',
      tone: 'danger',
      typedConfirmation: name
    });
    if (!ok) return;
    starting = true;
    try {
      const body: Record<string, unknown> = { destination, source, chain: chain.chain, dataset: name, keepPartial };
      if (chain.raw && keyHex.trim()) body.keyHex = keyHex.trim();
      const r = await api<StartedJob>('/api/cloud/restore', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body)
      });
      keyHex = '';
      dispatch('started', r.jobId);
    } catch (e) {
      const msg = e instanceof ApiError || e instanceof Error ? e.message : String(e);
      if (e instanceof ApiError && e.status === 409) {
        error = 'Another job for this source or dataset is running. Wait for it to finish, or cancel it first.';
      } else if (e instanceof ApiError && e.status === 412) {
        error = `The restore cannot start: ${msg}. For an encrypted source whose key is not on this NAS, enter the dataset key.`;
      } else {
        error = 'Starting the restore failed: ' + msg;
      }
    } finally {
      starting = false;
    }
  }
</script>

<Modal title="Restore {chain.toSnapshot}" size="md" onClose={() => dispatch('close')}>
  <form id="cloud-restore-form" class="space-y-3" novalidate on:submit|preventDefault={restore}>
    <p class="text-sm text-gray-300">
      Restores <span class="font-mono">{source}</span> ({chain.kind} #{chain.sequence}{chain.raw ? ', raw' : ''})
      from {destinationName || destination} into a new dataset. Existing datasets are never overwritten.
    </p>
    <FormField label="New dataset" id="cloud-restore-target" required error={shownTarget} hint="Must not exist yet; its parent must.">
      <input
        id="cloud-restore-target"
        class="input w-full font-mono"
        bind:value={target}
        aria-describedby={shownTarget ? 'cloud-restore-target-hint cloud-restore-target-error' : 'cloud-restore-target-hint'}
      />
    </FormField>
    {#if chain.raw}
      <FormField
        label="Dataset key (hex)"
        id="cloud-restore-key"
        error={keyError}
        hint="Only for an encrypted source whose key is not stored on this NAS. Write-only; it is not shown again."
      >
        <input
          id="cloud-restore-key"
          class="input w-full font-mono"
          type="password"
          autocomplete="off"
          bind:value={keyHex}
          aria-describedby={keyError ? 'cloud-restore-key-hint cloud-restore-key-error' : 'cloud-restore-key-hint'}
        />
      </FormField>
    {/if}
    <label class="flex items-center gap-2 text-sm">
      <input id="cloud-restore-keep-partial" type="checkbox" class="h-4 w-4 rounded" bind:checked={keepPartial} />
      Keep a partially restored dataset if the restore fails
    </label>
  </form>
  {#if error}
    <p role="alert" class="text-sm text-red-300">{error}</p>
  {/if}
  <svelte:fragment slot="footer">
    <Button variant="secondary" on:click={() => dispatch('close')}>Cancel</Button>
    <Button type="submit" form="cloud-restore-form" variant="danger" loading={starting}>Restore</Button>
  </svelte:fragment>
</Modal>
