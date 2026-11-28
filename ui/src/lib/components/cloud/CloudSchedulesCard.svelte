<script lang="ts">
  // Cloud backup schedules: one dataset fans out to one or more destinations
  // on a cadence; each destination keeps the newest `keepLineages` lineages.
  import { createEventDispatcher } from 'svelte';
  import { api, ApiError } from '$lib/api';
  import Button from '$lib/components/Button.svelte';
  import DataTable from '$lib/components/DataTable.svelte';
  import FormField from '$lib/components/FormField.svelte';
  import StatusBadge from '$lib/components/StatusBadge.svelte';
  import { fmtRelative, fmtTime } from '$lib/format';
  import { confirmAction } from '$lib/stores/confirm';
  import type { Cadence, CloudSchedule, DatasetSummary, Destination } from '$lib/cloud/types';

  export let schedules: CloudSchedule[] = [];
  export let datasets: DatasetSummary[] = [];
  export let destinations: Destination[] = [];
  /** False until the recovery-key acknowledgement is given. */
  export let allowed = true;

  const dispatch = createEventDispatcher<{ refresh: void }>();

  function blank() {
    return {
      dataset: '',
      destinations: [] as string[],
      cadence: 'daily' as Cadence,
      runAt: '02:30',
      keepLineages: 2,
      enabled: true
    };
  }

  let form = blank();
  let saving = false;
  let busy = '';
  let error = '';

  $: names = Object.fromEntries(destinations.map((d) => [d.id, d.name]));
  $: canAdd = allowed && !!form.dataset && form.destinations.length > 0 && !saving;

  function message(e: unknown): string {
    return e instanceof ApiError || e instanceof Error ? e.message : String(e);
  }

  async function post(schedule: Partial<CloudSchedule>) {
    await api<{ schedule: CloudSchedule }>('/api/cloud/schedules', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(schedule)
    });
  }

  async function add() {
    if (!canAdd) return;
    saving = true;
    error = '';
    try {
      // No id: the server assigns one (upsert by id).
      await post({
        dataset: form.dataset,
        destinations: form.destinations,
        cadence: form.cadence,
        runAt: form.cadence === 'hourly' ? '' : form.runAt,
        keepLineages: Math.max(1, Number(form.keepLineages) || 1),
        enabled: form.enabled
      });
      form = blank();
      dispatch('refresh');
    } catch (e) {
      error = 'Saving the schedule failed: ' + message(e);
    } finally {
      saving = false;
    }
  }

  async function toggle(s: CloudSchedule) {
    busy = s.id;
    error = '';
    try {
      await post({
        id: s.id,
        dataset: s.dataset,
        destinations: s.destinations,
        cadence: s.cadence,
        runAt: s.runAt,
        keepLineages: s.keepLineages,
        enabled: !s.enabled
      });
      dispatch('refresh');
    } catch (e) {
      error = 'Updating the schedule failed: ' + message(e);
    } finally {
      busy = '';
    }
  }

  async function remove(s: CloudSchedule) {
    const ok = await confirmAction({
      title: 'Delete schedule',
      message: `Delete the cloud schedule for ${s.dataset}? Backups already in the bucket are kept.`,
      confirmLabel: 'Delete',
      tone: 'danger'
    });
    if (!ok) return;
    busy = s.id;
    error = '';
    try {
      await api(`/api/cloud/schedules?id=${encodeURIComponent(s.id)}`, { method: 'DELETE' });
      dispatch('refresh');
    } catch (e) {
      error = 'Deleting the schedule failed: ' + message(e);
    } finally {
      busy = '';
    }
  }
</script>

<section class="card mb-6" aria-labelledby="cloud-schedules-heading">
  <h2 id="cloud-schedules-heading" class="mb-2 text-lg font-bold">Schedules</h2>
  {#if schedules.length === 0}
    <p class="mb-3 text-sm text-gray-400">No cloud schedules yet.</p>
  {:else}
    <div class="mb-4">
      <DataTable label="Cloud backup schedules">
        <table>
          <thead>
            <tr>
              <th scope="col">Dataset</th>
              <th scope="col">Destinations</th>
              <th scope="col">Cadence</th>
              <th scope="col">Keep</th>
              <th scope="col">Last run</th>
              <th scope="col">Next run</th>
              <th scope="col">Actions</th>
            </tr>
          </thead>
          <tbody>
            {#each schedules as s (s.id)}
              <tr>
                <td class="max-w-48 truncate font-mono" title={s.dataset}>{s.dataset}</td>
                <td>
                  {#each s.destinations as id (id)}
                    {@const outcome = s.destinationResults?.[id]}
                    <span class="mr-2 whitespace-nowrap" title={outcome ?? ''}>
                      {outcome ? (outcome === 'ok' ? '✓ ' : '✗ ') : ''}{names[id] ?? id}
                    </span>
                  {/each}
                </td>
                <td class="whitespace-nowrap">{s.cadence}{s.runAt ? ` ${s.runAt}` : ''}</td>
                <td>{s.keepLineages}</td>
                <td class="whitespace-nowrap">
                  {#if s.lastResult}
                    <span title={s.lastError || fmtTime(s.lastRun)}>
                      <StatusBadge state={s.lastResult} /> {fmtRelative(s.lastRun)}
                    </span>
                  {:else}
                    never
                  {/if}
                </td>
                <td class="whitespace-nowrap" title={fmtTime(s.nextRun)}>
                  {s.enabled ? fmtRelative(s.nextRun) : 'disabled'}
                </td>
                <td>
                  <div class="flex flex-nowrap gap-1">
                    <Button
                      size="sm"
                      variant="secondary"
                      loading={busy === s.id}
                      aria-label={`${s.enabled ? 'Disable' : 'Enable'} schedule for ${s.dataset}`}
                      on:click={() => toggle(s)}>{s.enabled ? 'Disable' : 'Enable'}</Button
                    >
                    <Button
                      size="sm"
                      variant="danger"
                      disabled={!!busy}
                      aria-label={`Delete schedule for ${s.dataset}`}
                      on:click={() => remove(s)}>Delete</Button
                    >
                  </div>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </DataTable>
    </div>
  {/if}

  {#if !allowed}
    <p class="mb-3 text-sm text-amber-300">Confirm the recovery keys above before adding a schedule.</p>
  {/if}
  <form class="space-y-3" on:submit|preventDefault={add}>
    <div class="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
      <FormField label="Dataset" id="cloud-schedule-dataset" required>
        <select id="cloud-schedule-dataset" class="input w-full" bind:value={form.dataset}>
          <option value="">Dataset…</option>
          {#each datasets as d (d.name)}
            <option value={d.name}>{d.name}</option>
          {/each}
        </select>
      </FormField>
      <fieldset class="min-w-0">
        <legend class="label">Destinations *</legend>
        {#if destinations.length === 0}
          <p class="text-xs text-gray-500">Add a destination first.</p>
        {/if}
        {#each destinations as d (d.id)}
          <label class="flex items-center gap-2 text-sm">
            <input type="checkbox" class="h-4 w-4 rounded" value={d.id} bind:group={form.destinations} />
            {d.name}
          </label>
        {/each}
      </fieldset>
      <FormField label="Cadence" id="cloud-schedule-cadence">
        <select id="cloud-schedule-cadence" class="input w-full" bind:value={form.cadence}>
          <option value="hourly">Hourly</option>
          <option value="daily">Daily</option>
          <option value="weekly">Weekly</option>
        </select>
      </FormField>
      <FormField label="Run at (UTC)" id="cloud-schedule-run-at">
        <input
          id="cloud-schedule-run-at"
          class="input w-full"
          bind:value={form.runAt}
          placeholder={form.cadence === 'weekly' ? 'Mon 02:30' : '02:30'}
          disabled={form.cadence === 'hourly'}
        />
      </FormField>
      <FormField label="Keep lineages" id="cloud-schedule-keep" hint="Full backups (with their incrementals) kept per destination.">
        <input
          id="cloud-schedule-keep"
          type="number"
          min="1"
          class="input w-full"
          bind:value={form.keepLineages}
          aria-describedby="cloud-schedule-keep-hint"
        />
      </FormField>
      <label class="flex items-center gap-2 self-end text-sm">
        <input id="cloud-schedule-enabled" type="checkbox" class="h-4 w-4 rounded" bind:checked={form.enabled} />
        Enabled
      </label>
    </div>
    <Button type="submit" loading={saving} disabled={!canAdd}>{saving ? 'Saving…' : 'Add schedule'}</Button>
    {#if error}
      <p role="alert" class="text-sm text-red-300">{error}</p>
    {/if}
  </form>
</section>
