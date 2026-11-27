<script lang="ts">
  import { createEventDispatcher } from 'svelte';
  import { api, ApiError } from '$lib/api';
  import Button from '$lib/components/Button.svelte';
  import FormField from '$lib/components/FormField.svelte';
  import Modal from '$lib/components/Modal.svelte';

  interface StorageSchedule {
    id: string;
    kind: 'snapshot' | 'scrub';
    target: string;
    cadence: 'hourly' | 'daily' | 'weekly';
    runAt: string;
    keep: number;
    recursive: boolean;
    enabled: boolean;
  }

  export let kind: 'snapshot' | 'scrub' = 'snapshot';
  export let target = '';
  export let existing: StorageSchedule | null = null;
  export let scheduleableDatasets: Array<{ name: string }> = [];
  export let schedules: StorageSchedule[] = [];
  export let onClose: () => void;
  export let onSaved: () => void;

  const dispatch = createEventDispatcher<{ remove: string }>();

  let scheduleId = existing?.id ?? '';
  let scheduleTarget = target || scheduleableDatasets[0]?.name || '';
  let cadence: 'hourly' | 'daily' | 'weekly' = existing?.cadence ?? (kind === 'scrub' ? 'weekly' : 'daily');
  let runAt = existing?.runAt || (kind === 'scrub' ? 'Sun 02:30' : '02:30');
  let keep = existing?.keep ?? (kind === 'scrub' ? 0 : 7);
  let recursive = existing?.recursive ?? false;
  let saving = false;
  let error = '';

  // Both entry points resolve the same way: opening an existing schedule must
  // edit it instead of silently creating a duplicate for the same target.
  function selectScheduleTarget() {
    const found = schedules.find((s) => s.kind === 'snapshot' && s.target === scheduleTarget);
    scheduleId = found?.id ?? '';
    if (found) {
      cadence = found.cadence;
      runAt = found.runAt;
      keep = found.keep;
      recursive = found.recursive;
    }
  }

  // Keep `runAt` valid for the selected cadence: daily is `HH:MM` and weekly is
  // `Day HH:MM`. Changing the cadence must not leave the field in the other
  // shape, or the API rejects the save until the operator retypes it.
  function normalizeRunAt(next: 'hourly' | 'daily' | 'weekly', value: string): string {
    if (next === 'hourly') return '';
    const parts = value.trim().split(/\s+/).filter(Boolean);
    const last = parts[parts.length - 1] ?? '';
    const time = /^\d{2}:\d{2}$/.test(last) ? last : '02:30';
    if (next === 'weekly') {
      const day = parts.find((p) => /^(sun|mon|tue|wed|thu|fri|sat)$/i.test(p));
      const weekday = day ? day[0].toUpperCase() + day.slice(1, 3).toLowerCase() : 'Sun';
      return `${weekday} ${time}`;
    }
    return time;
  }

  function changeCadence(event: Event) {
    const value = (event.currentTarget as HTMLSelectElement).value as
      | 'hourly'
      | 'daily'
      | 'weekly';
    cadence = value;
    runAt = normalizeRunAt(value, runAt);
  }

  async function save() {
    saving = true;
    error = '';
    try {
      await api('/api/storage/schedules', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          id: scheduleId,
          kind,
          target: scheduleTarget,
          cadence,
          runAt: cadence === 'hourly' ? '' : runAt,
          keep: kind === 'scrub' ? 0 : Number(keep) || 0,
          recursive: kind === 'snapshot' ? recursive : false
          // `enabled` is omitted: a new schedule defaults to enabled in the API,
          // while editing a schedule disabled by the 0.6.0 duplicate migration
          // must not silently re-enable it.
        })
      });
      onSaved();
    } catch (e) {
      error =
        e instanceof ApiError ? e.message : 'Could not save the schedule. Check the API connection.';
    } finally {
      saving = false;
    }
  }
</script>

<Modal
  title={kind === 'scrub' ? `Scrub schedule for ${scheduleTarget}` : `Snapshot schedule for ${scheduleTarget}`}
  {onClose}
>
  {#if kind === 'snapshot'}
    <p class="text-sm text-gray-400">
      The schedule creates snapshots named <code class="font-mono text-xs">naslos-YYYY-MM-DD_HH-MM-SS</code>
      (UTC) and keeps the newest <strong>{keep || 'all'}</strong>. Snapshots you create yourself are
      never removed.
    </p>
    <FormField label="Dataset" id="schedule-target" required>
      <select
        id="schedule-target"
        class="input w-full"
        bind:value={scheduleTarget}
        on:change={selectScheduleTarget}
      >
        {#each scheduleableDatasets as d}
          <option value={d.name}>{d.name}</option>
        {/each}
      </select>
    </FormField>
    {#if scheduleId}
      <p class="text-xs text-gray-500">This dataset already has a schedule; saving updates it.</p>
    {/if}
  {:else}
    <p class="text-sm text-gray-400">
      A scrub is read-only: ZFS reads every block and verifies its checksum. It is skipped when a
      scrub or resilver is already running.
    </p>
  {/if}

  <form id="schedule-form" on:submit|preventDefault={save} class="space-y-4">
    <FormField label="Cadence" id="schedule-cadence">
      <select
        id="schedule-cadence"
        value={cadence}
        on:change={changeCadence}
        class="input w-full"
      >
        {#if kind === 'snapshot'}
          <option value="hourly">Hourly</option>
        {/if}
        <option value="daily">Daily</option>
        <option value="weekly">Weekly</option>
      </select>
    </FormField>

    {#if cadence !== 'hourly'}
      <FormField
        label="Run at (UTC)"
        id="schedule-run-at"
        hint={cadence === 'weekly' ? 'Weekday and time, e.g. Sun 02:30.' : 'Time of day, e.g. 02:30.'}
      >
        <input
          id="schedule-run-at"
          type="text"
          bind:value={runAt}
          placeholder={cadence === 'weekly' ? 'Mon 02:30' : '02:30'}
          class="input w-full"
        />
      </FormField>
    {/if}

    {#if kind === 'snapshot'}
      <FormField
        label="Keep newest"
        id="schedule-keep"
        hint="0 keeps every scheduler-created snapshot."
      >
        <input id="schedule-keep" type="number" min="0" bind:value={keep} class="input w-full" />
      </FormField>
      <label class="flex cursor-pointer items-start gap-3">
        <input type="checkbox" class="mt-1" bind:checked={recursive} />
        <span class="text-sm">Include child datasets (recursive snapshot)</span>
      </label>
    {/if}

    {#if error}
      <div role="alert" class="callout callout-bad mb-0">{error}</div>
    {/if}
  </form>

  <div slot="footer">
    {#if scheduleId}
      <Button
        variant="danger"
        on:click={() => {
          dispatch('remove', scheduleId);
          onClose();
        }}
      >
        Remove
      </Button>
    {/if}
    <Button variant="secondary" on:click={onClose}>Cancel</Button>
    <Button
      type="submit"
      form="schedule-form"
      loading={saving}
      disabled={!scheduleTarget}
    >
      Save schedule
    </Button>
  </div>
</Modal>
