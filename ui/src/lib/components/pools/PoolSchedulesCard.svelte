<script lang="ts">
  // Scheduled snapshots and scrubs (FR-STO-10/11). The component renders the
  // current schedules; the page owns the API calls and confirmation dialogs.
  import { createEventDispatcher } from 'svelte';
  import Button from '$lib/components/Button.svelte';
  import { fmtTime } from '$lib/format';

  interface StorageSchedule {
    id: string;
    kind: 'snapshot' | 'scrub';
    target: string;
    cadence: 'hourly' | 'daily' | 'weekly';
    runAt: string;
    keep: number;
    recursive: boolean;
    enabled: boolean;
    lastRun?: string;
    lastResult?: string;
    lastError?: string;
    nextRun?: string;
  }

  export let schedules: StorageSchedule[] = [];
  export let poolName = '';
  export let scheduleableDatasets: Array<{ name: string }> = [];
  export let scrubRunning = false;

  const dispatch = createEventDispatcher<{
    scrub: void;
    scrubEdit: void;
    addSnapshot: void;
    edit: StorageSchedule;
    remove: string;
  }>();

  $: snapshotSchedules = schedules.filter(
    (s) => s.kind === 'snapshot' && (s.target === poolName || s.target?.startsWith(`${poolName}/`))
  );
  $: scrubSchedule = schedules.find((s) => s.kind === 'scrub' && s.target === poolName);
</script>

<div class="card mt-6">
  <div class="mb-4 flex flex-wrap items-center justify-between gap-2">
    <h2 class="text-xl font-bold">Maintenance schedules</h2>
    <Button
      variant="secondary"
      size="sm"
      on:click={() => (scrubSchedule ? dispatch('edit', scrubSchedule) : dispatch('scrubEdit'))}
    >
      {scrubSchedule ? 'Edit scrub schedule' : 'Schedule scrub'}
    </Button>
  </div>

  <div class="mb-4 rounded-lg bg-naslos-dark p-3">
    <div class="flex flex-wrap items-center justify-between gap-2">
      <div class="min-w-0">
        <div class="font-medium">Scrub</div>
        {#if scrubSchedule}
          <div class="text-xs text-gray-400">
            {scrubSchedule.cadence}{scrubSchedule.runAt ? ` ${scrubSchedule.runAt} UTC` : ''} ·
            last {scrubSchedule.lastResult || 'never'} ({fmtTime(scrubSchedule.lastRun)}) · next
            {scrubSchedule.enabled ? fmtTime(scrubSchedule.nextRun) : 'disabled'}
          </div>
          {#if scrubSchedule.lastError}
            <div class="break-words text-xs text-amber-300">{scrubSchedule.lastError}</div>
          {/if}
        {:else}
          <div class="text-xs text-gray-500">
            No scrub schedule. Scrubs are read-only and verify every block's checksum.
          </div>
        {/if}
      </div>
      <div class="flex flex-wrap gap-1">
        <Button
          variant="secondary"
          size="sm"
          loading={scrubRunning}
          disabled={scrubRunning}
          on:click={() => dispatch('scrub')}
        >
          Scrub now
        </Button>
        {#if scrubSchedule}
          <Button
            variant="danger"
            size="sm"
            on:click={() => dispatch('remove', scrubSchedule.id)}
          >
            Remove
          </Button>
        {/if}
      </div>
    </div>
  </div>

  <div class="mb-2 flex flex-wrap items-center justify-between gap-2">
    <h3 class="font-bold">Scheduled snapshots</h3>
    <Button
      variant="secondary"
      size="sm"
      on:click={() => dispatch('addSnapshot')}
      disabled={scheduleableDatasets.length === 0}
    >
      Add snapshot schedule
    </Button>
  </div>
  {#if scheduleableDatasets.length === 0}
    <p class="text-sm text-gray-400">
      Create a dataset below this pool first; the pool root cannot be scheduled.
    </p>
  {:else if snapshotSchedules.length === 0}
    <p class="text-sm text-gray-400">
      No dataset has a snapshot schedule yet. A schedule creates its own
      <code class="font-mono text-xs">naslos-YYYY-MM-DD_HH-MM-SS</code> snapshots (visible as SMB
      previous versions) and keeps the newest N; it never deletes your snapshots.
    </p>
  {:else}
    <div class="space-y-2">
      {#each snapshotSchedules as s (s.id)}
        <div class="flex flex-wrap items-center justify-between gap-2 rounded-lg bg-naslos-dark p-3">
          <div class="min-w-0">
            <div class="truncate font-medium">{s.target}</div>
            <div class="text-xs text-gray-400">
              {s.cadence}{s.runAt ? ` ${s.runAt} UTC` : ''} · keep {s.keep || 'all'}
              {s.keep ? 'newest' : ''}{s.recursive ? ' · recursive' : ''} · last
              {s.lastResult || 'never'} ({fmtTime(s.lastRun)}) · next
              {s.enabled ? fmtTime(s.nextRun) : 'disabled'}
            </div>
            {#if s.lastError}
              <div class="break-words text-xs text-amber-300">{s.lastError}</div>
            {/if}
          </div>
          <div class="flex flex-wrap gap-1">
            <Button variant="secondary" size="sm" on:click={() => dispatch('edit', s)}>Edit</Button>
            <Button variant="danger" size="sm" on:click={() => dispatch('remove', s.id)}>
              Remove
            </Button>
          </div>
        </div>
      {/each}
    </div>
  {/if}
</div>
