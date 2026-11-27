<script lang="ts">
  import { createEventDispatcher } from 'svelte';
  import { api, ApiError } from '$lib/api';
  import Button from '$lib/components/Button.svelte';
  import DataTable from '$lib/components/DataTable.svelte';
  import FormField from '$lib/components/FormField.svelte';
  import { formatBytes, fmtRelative, fmtTime } from '$lib/format';
  import { confirmAction } from '$lib/stores/confirm';
  import type { DatasetSummary, Schedule, VerifyChain } from './types';

  export let schedules: Schedule[] = [];
  export let datasets: DatasetSummary[] = [];

  const dispatch = createEventDispatcher<{ refresh: void; backupNow: Schedule }>();

  let newSchedule = {
    dataset: '',
    receivers: '',
    source: '',
    cadence: 'daily',
    runAt: '02:30',
    pruneKeep: 7
  };
  let savingSchedule = false;
  let scheduleError = '';
  let deleting = '';

  let verifyFor = '';
  let verifyChains: VerifyChain[] = [];
  let verifying = false;
  let verifyError = '';

  function message(e: unknown): string {
    return e instanceof ApiError || e instanceof Error ? e.message : String(e);
  }

  // `new URL(url).host` throws on a malformed stored URL; the row must still
  // render every outcome instead of blanking the table.
  function hostOf(url: string): string {
    try {
      return new URL(url).host;
    } catch {
      return url;
    }
  }

  async function createSchedule() {
    savingSchedule = true;
    scheduleError = '';
    // One buddy per line (or comma separated): every one of them gets its own
    // backup run, so a dead buddy fails only itself.
    const receivers = newSchedule.receivers
      .split(/[\n,]+/)
      .map((r) => r.trim())
      .filter((r) => r.length > 0);
    try {
      await api('/api/buddy/schedules', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          dataset: newSchedule.dataset,
          receiver: receivers[0] ?? '',
          receivers,
          source: newSchedule.source,
          cadence: newSchedule.cadence,
          runAt: newSchedule.cadence === 'hourly' ? '' : newSchedule.runAt,
          pruneKeep: Number(newSchedule.pruneKeep) || 0
        })
      });
      newSchedule = {
        dataset: '',
        receivers: '',
        source: '',
        cadence: 'daily',
        runAt: '02:30',
        pruneKeep: 7
      };
      dispatch('refresh');
    } catch (e) {
      scheduleError = 'Saving the schedule failed: ' + message(e);
    } finally {
      savingSchedule = false;
    }
  }

  async function deleteSchedule(schedule: Schedule) {
    if (
      !(await confirmAction({
        title: 'Delete schedule',
        message: 'Delete this schedule? Already-stored backups are kept.',
        confirmLabel: 'Delete',
        tone: 'danger'
      }))
    ) {
      return;
    }
    scheduleError = '';
    deleting = schedule.id;
    try {
      await api(`/api/buddy/schedules?id=${encodeURIComponent(schedule.id)}`, {
        method: 'DELETE'
      });
      dispatch('refresh');
    } catch (e) {
      scheduleError = 'Deleting the schedule failed: ' + message(e);
    } finally {
      deleting = '';
    }
  }

  async function verify(schedule: Schedule) {
    verifying = true;
    verifyError = '';
    verifyChains = [];
    verifyFor = `${schedule.source} on ${schedule.receiver}`;
    try {
      const data = await api<{ chains?: VerifyChain[] }>('/api/buddy/restore', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          source: schedule.source,
          receiver: schedule.receiver,
          verify: true
        })
      });
      verifyChains = Array.isArray(data.chains) ? data.chains : [];
    } catch (e) {
      verifyError = 'Verify failed: ' + message(e);
    } finally {
      verifying = false;
    }
  }
</script>

<div class="card mb-6">
  <h2 class="mb-2 text-lg font-bold">Schedules</h2>
  {#if schedules.length === 0}
    <p class="mb-3 text-sm text-gray-400">
      No schedules yet. Backups below run on their cadence and prune to the keep-count on success.
    </p>
  {:else}
    <div class="mb-4">
      <DataTable label="Backup schedules">
        <table>
          <thead>
            <tr>
              <th scope="col">Dataset</th>
              <th scope="col">Source</th>
              <th scope="col">Buddy</th>
              <th scope="col">Cadence</th>
              <th scope="col">Last run</th>
              <th scope="col">Next run</th>
              <th scope="col" class="w-64">Actions</th>
            </tr>
          </thead>
          <tbody>
            {#each schedules as s (s.id)}
              <tr>
                <td class="max-w-48 truncate font-mono" title={s.dataset}>{s.dataset}</td>
                <td class="max-w-40 truncate font-mono" title={s.source}>{s.source}</td>
                <td class="max-w-48">
                  <div class="truncate" title={(s.receivers ?? [s.receiver]).join('\n')}>
                    {s.receivers && s.receivers.length > 1 ? `${s.receivers.length} buddies` : s.receiver}
                  </div>
                  {#if s.receiverResults}
                    <span class="block text-xs text-gray-500">
                      {#each Object.entries(s.receiverResults) as [url, outcome] (url)}
                        <span class="mr-2" title={url}>{outcome === 'ok' ? '✓' : '✗'} {hostOf(url)}</span>
                      {/each}
                    </span>
                  {/if}
                </td>
                <td class="whitespace-nowrap">{s.cadence}{s.runAt ? ` ${s.runAt}` : ''}</td>
                <td class="whitespace-nowrap">
                  {#if s.lastResult}
                    <span title={fmtTime(s.lastRun)}>{s.lastResult} ({fmtRelative(s.lastRun)})</span>
                  {:else}
                    never
                  {/if}
                </td>
                <td class="whitespace-nowrap" title={fmtTime(s.nextRun)}>
                  {s.enabled ? fmtRelative(s.nextRun) : 'disabled'}
                </td>
                <td>
                  <div class="flex flex-nowrap gap-1 whitespace-nowrap">
                    <Button size="sm" variant="secondary" on:click={() => dispatch('backupNow', s)}>
                      Back up now
                    </Button>
                    <Button size="sm" variant="secondary" on:click={() => verify(s)}>Verify</Button>
                    <Button
                      size="sm"
                      variant="danger"
                      loading={deleting === s.id}
                      disabled={!!deleting && deleting !== s.id}
                      on:click={() => deleteSchedule(s)}
                    >Delete</Button>
                  </div>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </DataTable>
    </div>
  {/if}

  <form class="space-y-3" on:submit|preventDefault={createSchedule}>
    <div class="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
      <FormField label="Dataset" id="schedule-dataset" required>
        <select id="schedule-dataset" bind:value={newSchedule.dataset} class="input w-full">
          <option value="">Dataset…</option>
          {#each datasets as d (d.name)}
            <option value={d.name}>{d.name}</option>
          {/each}
        </select>
      </FormField>
      <FormField label="Buddy URLs" id="schedule-receivers" required hint="One per line; every buddy gets its own run.">
        <textarea
          id="schedule-receivers"
          bind:value={newSchedule.receivers}
          placeholder="Buddy URLs, one per line (https://…)"
          rows="2"
          class="input w-full"
        ></textarea>
      </FormField>
      <FormField label="Source name" id="schedule-source" required>
        <input
          id="schedule-source"
          type="text"
          bind:value={newSchedule.source}
          placeholder="Source name naslos-a/data"
          class="input w-full"
        />
      </FormField>
      <FormField label="Cadence" id="schedule-cadence">
        <select id="schedule-cadence" bind:value={newSchedule.cadence} class="input w-full">
          <option value="hourly">Hourly</option>
          <option value="daily">Daily</option>
          <option value="weekly">Weekly</option>
        </select>
      </FormField>
      <FormField label="Run at" id="schedule-run-at">
        <input
          id="schedule-run-at"
          type="text"
          bind:value={newSchedule.runAt}
          placeholder={newSchedule.cadence === 'weekly' ? 'Mon 02:30 (UTC)' : '02:30 (UTC)'}
          class="input w-full"
          disabled={newSchedule.cadence === 'hourly'}
        />
      </FormField>
      <FormField label="Keep newest" id="schedule-prune-keep" hint="Chains kept on each buddy.">
        <input
          id="schedule-prune-keep"
          type="number"
          bind:value={newSchedule.pruneKeep}
          min="0"
          class="input w-full"
        />
      </FormField>
    </div>
    <Button type="submit" loading={savingSchedule}>
      {savingSchedule ? 'Saving...' : 'Add schedule'}
    </Button>
    {#if scheduleError}
      <p role="alert" class="text-sm text-red-300">{scheduleError}</p>
    {/if}
  </form>

  {#if verifyFor}
    <div class="mt-3 text-sm">
      <p class="text-gray-400">Verify {verifyFor}:</p>
      {#if verifying}
        <p class="text-gray-400">Verifying…</p>
      {:else if verifyError}
        <p role="alert" class="text-red-300">{verifyError}</p>
      {:else if verifyChains.length > 0}
        <ul class="mt-1 space-y-1">
          {#each verifyChains as c (c.chain)}
            <li>
              <code class="text-xs text-naslos-link">{c.chain}</code>
              <span class="text-xs text-gray-400">{c.sha256} ({formatBytes(c.bytes)})</span>
            </li>
          {/each}
        </ul>
      {/if}
    </div>
  {/if}
</div>
