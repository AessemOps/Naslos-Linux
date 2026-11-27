<script lang="ts">
  // ZFS pool health cards. Usage is a real progressbar with a numeric value so
  // assistive tech gets the same figure the label shows. Destructive deletes go
  // through confirmAction with the pool name typed, as on the storage pages.
  import Button from '../Button.svelte';
  import Icon from '../Icon.svelte';
  import StatusBadge from '../StatusBadge.svelte';
  import { api } from '$lib/api';
  import { formatBytes } from '$lib/format';
  import { confirmAction } from '$lib/stores/confirm';
  import type { Pool } from './types';

  export let pools: Pool[] = [];
  export let onDeleted: () => void = () => {};

  let deleteError = '';
  let deleting = '';

  function percent(pool: Pool): number {
    const value = pool.usagePercent;
    return value == null || Number.isNaN(value) ? 0 : Math.min(100, Math.max(0, value));
  }

  function barClass(value: number): string {
    if (value >= 90) return 'bg-red-500';
    if (value >= 75) return 'bg-amber-400';
    return 'bg-naslos-primary';
  }

  async function remove(pool: Pool) {
    deleteError = '';
    const ok = await confirmAction({
      title: 'Delete pool',
      message: `Delete the pool “${pool.name}”? This destroys the pool and every dataset on it. This cannot be undone.`,
      confirmLabel: 'Delete pool',
      tone: 'danger',
      typedConfirmation: pool.name
    });
    if (!ok) return;
    deleting = pool.name;
    try {
      await api(
        `/api/volumes/zfs/${encodeURIComponent(pool.name)}?confirm=${encodeURIComponent(pool.name)}`,
        { method: 'DELETE' }
      );
      onDeleted();
    } catch (e) {
      deleteError = 'Failed to delete pool: ' + (e instanceof Error ? e.message : String(e));
    } finally {
      deleting = '';
    }
  }
</script>

<section class="card mb-6" aria-labelledby="dashboard-pools">
  <h2 id="dashboard-pools" class="mb-4 text-xl font-bold">ZFS Pools</h2>

  {#if deleteError}
    <div class="callout callout-bad mb-4" role="alert">{deleteError}</div>
  {/if}

  {#if pools.length > 0}
    <div class="grid grid-cols-1 gap-4 md:grid-cols-2">
      {#each pools as pool (pool.name)}
        {@const usage = percent(pool)}
        <div class="rounded-lg bg-naslos-dark p-4">
          <div class="mb-2 flex flex-wrap items-center justify-between gap-2">
            <a
              href="/pools/{pool.name}"
              class="font-bold transition-colors hover:text-naslos-link"
            >{pool.name}</a>
            <div class="flex items-center gap-2">
              <StatusBadge state={pool.health} unknownTone="bad" />
              <Button
                variant="danger"
                size="sm"
                loading={deleting === pool.name}
                on:click={() => remove(pool)}
                title={`Delete pool '${pool.name}'`}
              >Delete</Button>
            </div>
          </div>
          <a href="/pools/{pool.name}" class="block">
            <div
              class="mb-1 h-2 w-full rounded-full bg-naslos-border"
              role="progressbar"
              aria-label={`${pool.name} usage`}
              aria-valuemin="0"
              aria-valuemax="100"
              aria-valuenow={Math.round(usage)}
            >
              <div class={`${barClass(usage)} h-2 rounded-full`} style={`width: ${usage}%`}></div>
            </div>
            <div class="text-xs text-gray-500">
              {formatBytes(pool.alloc || 0)} / {formatBytes(pool.size || 0)} ({usage.toFixed(1)}%)
            </div>
          </a>
        </div>
      {/each}
    </div>
  {:else}
    <div class="py-8 text-center">
      <div
        class="mx-auto mb-4 flex h-12 w-12 items-center justify-center rounded-full border border-naslos-border bg-naslos-dark text-gray-400"
        aria-hidden="true"
      >
        <Icon name="database" size={24} />
      </div>
      <p class="mb-4 text-gray-400">No ZFS pools configured yet.</p>
      <Button href="/disks" variant="primary">Create Your First Pool</Button>
    </div>
  {/if}
</section>
