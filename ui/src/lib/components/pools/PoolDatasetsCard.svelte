<script lang="ts">
  // The pool's datasets: the level shares point at. Includes the encryption
  // lock state, snapshot-schedule entry point and the destroy action. Row
  // markup keeps `div.bg-naslos-dark` because the e2e suite selects it.
  import { createEventDispatcher } from 'svelte';
  import Button from '$lib/components/Button.svelte';
  import StatusBadge from '$lib/components/StatusBadge.svelte';
  import { isEncryptedDataset, isLockedDataset } from '$lib/storage';

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

  interface StorageSchedule {
    id: string;
    kind: 'snapshot' | 'scrub';
    target: string;
  }

  export let datasets: Dataset[] = [];
  export let error = '';
  export let poolName = '';
  export let schedules: StorageSchedule[] = [];

  const dispatch = createEventDispatcher<{
    new: void;
    delete: string;
    unlock: string;
    lock: string;
    schedule: string;
  }>();

  function scheduleFor(name: string): StorageSchedule | undefined {
    return schedules.find((s) => s.kind === 'snapshot' && s.target === name);
  }
</script>

<div class="card mt-6">
  <div class="mb-4 flex flex-wrap items-center justify-between gap-2">
    <h2 class="text-xl font-bold">Datasets</h2>
    <Button variant="secondary" size="sm" on:click={() => dispatch('new')}>New Dataset</Button>
  </div>

  {#if error}
    <p role="alert" class="text-sm text-red-300">{error}</p>
  {:else if datasets.length === 0}
    <p class="text-sm text-gray-400">No datasets yet. Create one to have a place for a share.</p>
  {:else}
    <div class="space-y-2">
      {#each datasets as ds (ds.name)}
        <div class="flex flex-wrap items-center justify-between gap-3 rounded-lg bg-naslos-dark p-3">
          <div class="min-w-0">
            <div class="flex flex-wrap items-center gap-2 font-medium">
              <span class="truncate">{ds.name}</span>
              {#if isEncryptedDataset(ds)}
                <span
                  title={isLockedDataset(ds) ? 'Key not loaded; unlock after boot' : 'Key loaded'}
                >
                  <StatusBadge
                    tone={isLockedDataset(ds) ? 'warn' : 'ok'}
                    label={isLockedDataset(ds) ? 'Locked' : 'Unlocked'}
                  />
                </span>
              {/if}
            </div>
            <div class="truncate font-mono text-xs text-gray-500">{ds.mountpoint || '—'}</div>
          </div>
          <div class="whitespace-nowrap font-mono text-xs text-gray-400">
            {ds.used || '0'} used · {ds.avail || '0'} free
          </div>
          <div class="flex flex-wrap gap-1">
            {#if isEncryptedDataset(ds)}
              {#if isLockedDataset(ds)}
                <Button
                  variant="secondary"
                  size="sm"
                  on:click={() => dispatch('unlock', ds.name)}
                  title="Load this dataset's key from its Secret"
                >
                  Unlock
                </Button>
              {:else}
                <Button
                  variant="secondary"
                  size="sm"
                  on:click={() => dispatch('lock', ds.name)}
                  title="Unload the key; the data stays on disk"
                >
                  Lock
                </Button>
              {/if}
            {/if}
            {#if ds.name !== poolName}
              <Button
                variant="secondary"
                size="sm"
                on:click={() => dispatch('schedule', ds.name)}
                title={scheduleFor(ds.name)
                  ? 'Edit this dataset’s snapshot schedule'
                  : 'Schedule periodic snapshots'}
              >
                {scheduleFor(ds.name) ? 'Schedule ✓' : 'Schedule'}
              </Button>
            {/if}
            <Button
              variant="danger"
              size="sm"
              on:click={() => dispatch('delete', ds.name)}
              title="Destroy this dataset"
            >
              Delete
            </Button>
          </div>
        </div>
      {/each}
    </div>
  {/if}
</div>
