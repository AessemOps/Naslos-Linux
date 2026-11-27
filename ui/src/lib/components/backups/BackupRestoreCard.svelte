<script lang="ts">
  import { api, ApiError } from '$lib/api';
  import Button from '$lib/components/Button.svelte';
  import FormField from '$lib/components/FormField.svelte';

  let restore = { source: '', receiver: '', dataset: '', chain: '', force: false, confirm: '' };
  let restoreResult = '';
  let restoreError = '';
  let restoring = false;

  function message(e: unknown): string {
    return e instanceof ApiError || e instanceof Error ? e.message : String(e);
  }

  async function doRestore() {
    restoreError = '';
    restoreResult = '';
    if (restore.confirm.trim() !== restore.dataset.trim() || !restore.dataset) {
      restoreError = 'Type the destination dataset name to confirm — restoring overwrites it.';
      return;
    }
    restoring = true;
    try {
      const data = await api<{ plainBytes?: number; chains?: string[]; chain?: string }>(
        '/api/buddy/restore',
        {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({
            source: restore.source,
            receiver: restore.receiver,
            dataset: restore.dataset,
            chain: restore.chain || undefined,
            force: restore.force
          })
        }
      );
      const chains = Array.isArray(data.chains) ? data.chains.join(', ') : data.chain;
      restoreResult = `Restored ${restore.source} into ${restore.dataset} (${data.plainBytes} bytes, chains ${chains}).`;
      restore = { source: '', receiver: '', dataset: '', chain: '', force: false, confirm: '' };
    } catch (e) {
      restoreError = 'Restore failed: ' + message(e);
    } finally {
      restoring = false;
    }
  }
</script>

<div class="card mb-6">
  <h2 class="mb-2 text-lg font-bold">Restore</h2>
  <p class="mb-3 text-xs text-gray-400">
    Restoring overwrites the destination dataset. Type the destination name to confirm.
  </p>
  <form class="space-y-3" on:submit|preventDefault={doRestore}>
    <div class="grid grid-cols-1 gap-3 sm:grid-cols-2">
      <FormField label="Source name" id="restore-source" required>
        <input
          id="restore-source"
          type="text"
          bind:value={restore.source}
          placeholder="Source name"
          class="input w-full"
          required
        />
      </FormField>
      <FormField label="Buddy URL" id="restore-receiver" required>
        <input
          id="restore-receiver"
          type="text"
          bind:value={restore.receiver}
          placeholder="Buddy URL"
          class="input w-full"
          required
        />
      </FormField>
      <FormField label="Destination dataset" id="restore-dataset" required>
        <input
          id="restore-dataset"
          type="text"
          bind:value={restore.dataset}
          placeholder="Destination dataset"
          class="input w-full"
          required
        />
      </FormField>
      <FormField label="Chain" id="restore-chain" hint="Optional; the newest chain is used by default.">
        <input
          id="restore-chain"
          type="text"
          bind:value={restore.chain}
          placeholder="Chain (optional, newest by default)"
          class="input w-full"
        />
      </FormField>
      <label class="flex min-h-10 items-center gap-2 text-sm text-gray-300">
        <input type="checkbox" bind:checked={restore.force} class="h-4 w-4" />
        Force (-F on first stream)
      </label>
      <FormField label="Confirm destination" id="restore-confirm" required>
        <input
          id="restore-confirm"
          type="text"
          bind:value={restore.confirm}
          placeholder="Type destination to confirm"
          class="input w-full"
        />
      </FormField>
    </div>
    <Button type="submit" variant="danger" loading={restoring}>
      {restoring ? 'Restoring...' : 'Restore'}
    </Button>
  </form>
  {#if restoreError}
    <p role="alert" class="mt-2 text-sm text-red-300">{restoreError}</p>
  {/if}
  {#if restoreResult}
    <p class="mt-2 text-sm text-green-300">{restoreResult}</p>
  {/if}
</div>
