<script lang="ts">
  // Block volumes (FR-ZVOL). A zvol is a raw block device, not a mounted
  // dataset, so it is listed separately from the dataset table. Rows keep
  // `div.bg-naslos-dark` for the e2e selectors.
  import { createEventDispatcher } from 'svelte';
  import Button from '$lib/components/Button.svelte';
  import { formatBytes } from '$lib/format';

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

  export let zvols: Zvol[] = [];
  export let error = '';
  export let poolName = '';

  const dispatch = createEventDispatcher<{
    new: void;
    resize: Zvol;
    delete: Zvol;
  }>();

  function zvolBlocksizeLabel(value: string): string {
    const bytes = Number(value);
    return value && Number.isFinite(bytes) && bytes > 0 ? formatBytes(bytes) : '—';
  }
</script>

<div class="card mt-6">
  <div class="mb-4 flex flex-wrap items-center justify-between gap-2">
    <h2 class="text-xl font-bold">Block volumes (zvols)</h2>
    <Button variant="secondary" size="sm" on:click={() => dispatch('new')}>New zvol</Button>
  </div>
  <p class="mb-3 text-xs text-gray-500">
    A zvol is a raw block device (<code class="font-mono text-xs">/dev/zvol/{poolName}/…</code>),
    not a folder you can share over SMB/NFS. Resize only grows a zvol and is refused while it is in
    use; iSCSI export is not implemented yet (see <code class="font-mono text-xs">docs/iscsi.md</code>).
  </p>

  {#if error}
    <p role="alert" class="text-sm text-red-300">{error}</p>
  {:else if zvols.length === 0}
    <p class="text-sm text-gray-400">No block volumes in this pool.</p>
  {:else}
    <div class="space-y-2">
      {#each zvols as zvol (zvol.name)}
        <div class="flex flex-wrap items-center justify-between gap-3 rounded-lg bg-naslos-dark p-3">
          <div class="min-w-0">
            <div class="truncate font-medium">{zvol.name}</div>
            <div class="font-mono text-xs text-gray-500">
              volblocksize {zvolBlocksizeLabel(zvol.volblocksize)} · compression {zvol.compression ||
                '—'}
            </div>
          </div>
          <div class="whitespace-nowrap font-mono text-xs text-gray-400">
            {zvol.volsize || '—'} size · {zvol.used || '0'} used · {zvol.avail || '0'} free
          </div>
          <div class="flex flex-wrap gap-1">
            <Button
              variant="secondary"
              size="sm"
              on:click={() => dispatch('resize', zvol)}
              title="Grow this zvol"
            >
              Resize
            </Button>
            <Button
              variant="danger"
              size="sm"
              on:click={() => dispatch('delete', zvol)}
              title="Destroy this zvol"
            >
              Delete
            </Button>
          </div>
        </div>
      {/each}
    </div>
  {/if}
</div>
