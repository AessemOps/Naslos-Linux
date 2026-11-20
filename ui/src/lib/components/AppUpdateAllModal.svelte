<script lang="ts">
  import { createEventDispatcher } from 'svelte';
  import { modal } from '$lib/actions/modal';
  import Icon from '$lib/components/Icon.svelte';
  import { trackAppJob } from '$lib/stores/appJobs';
  import AppIcon from './AppIcon.svelte';
  import type { AppView } from '$lib/appTypes';

  export let apps: AppView[];

  const dispatch = createEventDispatcher();

  let saving = false;
  let progress = 0;

  $: total = apps.length;
  $: switching = apps.filter(
    (app) =>
      !!app.latestSource &&
      (app.latestSource !== (app.source ?? '') || app.latestChannel !== (app.channel ?? ''))
  );

  function displayName(app: AppView): string {
    return app.displayName || app.name;
  }

  function requestClose() {
    if (!saving) dispatch('close');
  }

  async function confirmUpdateAll() {
    if (saving || total === 0) return;
    saving = true;
    progress = 0;
    const started: string[] = [];
    const failed: { name: string; error: string }[] = [];
    // The parent snapshots the target list, so a job appearing while this loop
    // runs cannot shrink it mid-flight.
    for (const [index, app] of apps.entries()) {
      progress = index;
      try {
        const res = await fetch(`/api/apps/${encodeURIComponent(app.name)}`, {
          method: 'PUT',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({})
        });
        const data = await res.json().catch(() => ({}));
        if (!res.ok) {
          throw new Error(data.error || `HTTP ${res.status}`);
        }
        if (data.jobId) trackAppJob(data.jobId);
        started.push(app.name);
      } catch (e: any) {
        failed.push({ name: app.name, error: e?.message || String(e) });
      }
    }
    progress = total;
    saving = false;
    dispatch('done', { started, failed });
  }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
<div
  class="fixed inset-0 bg-black/60 z-50 flex items-center justify-center p-4"
  use:modal={{ onClose: requestClose }}
  on:click|self={requestClose}
>
  <div class="bg-naslos-surface rounded-2xl border border-naslos-border w-full max-w-lg">
    <div class="p-6 border-b border-naslos-border flex items-center justify-between">
      <h2 class="text-xl font-bold">Update {total} {total === 1 ? 'app' : 'apps'}</h2>
      <button
        class="icon-btn -mr-2 text-gray-400 hover:text-naslos-strong hover:bg-naslos-border disabled:opacity-40 disabled:cursor-not-allowed"
        aria-label="Close"
        disabled={saving}
        on:click={requestClose}
      >
        <Icon name="x" />
      </button>
    </div>

    <div class="p-6 space-y-4">
      <p class="text-sm text-gray-300">
        Each app restarts while its configured values and attached datasets are preserved.
      </p>

      <ul class="divide-y divide-naslos-border rounded-lg border border-naslos-border overflow-hidden max-h-64 overflow-y-auto">
        {#each apps as app (app.name)}
          <li class="flex items-center gap-3 px-3 py-2">
            <AppIcon icon={app.icon} iconUrl={app.iconUrl} alt="" size="text-xl" imgClass="h-6 w-6" />
            <span class="min-w-0 flex-1 truncate text-sm font-medium">{displayName(app)}</span>
            <span class="text-xs text-gray-400 tabular-nums">
              v{app.installedVersion || 'unknown'} → v{app.latestVersion || 'unknown'}
            </span>
          </li>
        {/each}
      </ul>

      {#if switching.length > 0}
        <p class="text-sm text-amber-300">
          {switching.map(displayName).join(', ')}
          {switching.length === 1 ? 'resolves' : 'resolve'} a different source/channel in the catalog than
          {switching.length === 1 ? 'it was' : 'they were'} installed from. Continuing moves
          {switching.length === 1 ? 'the app' : 'those apps'} to the catalog source that currently wins for
          the name.
        </p>
      {/if}
    </div>

    <div class="p-6 border-t border-naslos-border flex justify-end gap-2">
      <button class="btn btn-secondary" disabled={saving} on:click={requestClose}>Cancel</button>
      <button class="btn btn-primary" disabled={saving || total === 0} on:click={confirmUpdateAll}>
        {saving ? `Starting ${Math.min(progress + 1, total)} of ${total}…` : `Update ${total}`}
      </button>
    </div>
  </div>
</div>
