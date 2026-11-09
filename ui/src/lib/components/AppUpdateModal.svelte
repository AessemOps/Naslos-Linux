<script lang="ts">
  import { createEventDispatcher } from 'svelte';
  import { modal } from '$lib/actions/modal';
  import { trackAppJob } from '$lib/stores/appJobs';
  import AppIcon from './AppIcon.svelte';
  import type { AppView } from '$lib/appTypes';

  export let app: AppView;

  const dispatch = createEventDispatcher();

  let saving = false;
  let error = '';

  $: displayName = app.displayName || app.name;
  $: sourceSwitch =
    !!app.latestSource &&
    (app.latestSource !== (app.source ?? '') || app.latestChannel !== (app.channel ?? ''));

  async function confirmUpdate() {
    saving = true;
    error = '';
    try {
      // `{}` keeps the stored values/datasets; the manager resolves the
      // catalog winner and enqueues the tracked upgrade job.
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
      dispatch('close');
    } catch (e: any) {
      error = e?.message || String(e);
    } finally {
      saving = false;
    }
  }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
<div
  class="fixed inset-0 bg-black/60 z-50 flex items-center justify-center p-4"
  use:modal={{ onClose: () => dispatch('close') }}
  on:click|self={() => dispatch('close')}
>
  <div class="bg-naslos-surface rounded-2xl border border-naslos-border w-full max-w-md">
    <div class="p-6 border-b border-naslos-border flex items-center justify-between">
      <h2 class="text-xl font-bold">Update {displayName}</h2>
      <button class="text-gray-400 hover:text-white text-2xl" aria-label="Close" on:click={() => dispatch('close')}>
        ×
      </button>
    </div>

    <div class="p-6 space-y-4">
      <div class="flex items-center gap-3">
        <AppIcon icon={app.icon} iconUrl={app.iconUrl} alt="" size="text-3xl" imgClass="h-9 w-9" />
        <div class="text-sm text-gray-400">
          <p class="text-base font-semibold text-gray-100">{displayName}</p>
          <p class="tabular-nums">
            v{app.installedVersion || 'unknown'} → v{app.latestVersion || 'unknown'}
          </p>
        </div>
      </div>

      <p class="text-sm text-gray-300">
        The app restarts while its configured values and attached datasets are preserved.
      </p>

      {#if sourceSwitch}
        <p class="text-sm text-amber-300">
          This update resolves
          <span class="font-mono">{app.latestSource || '?'}/{app.latestChannel || '?'}</span>
          in the catalog, while the app was installed from
          <span class="font-mono">{app.source || '?'}/{app.channel || '?'}</span>. Continuing moves the app to the
          catalog source that currently wins for this name.
        </p>
      {/if}

      {#if error}
        <p role="alert" class="text-sm text-red-300 break-words">{error}</p>
      {/if}
    </div>

    <div class="p-6 border-t border-naslos-border flex justify-end gap-2">
      <button class="btn btn-secondary" disabled={saving} on:click={() => dispatch('close')}>Cancel</button>
      <button class="btn btn-primary" disabled={saving} on:click={confirmUpdate}>
        {saving ? 'Starting…' : `Update to v${app.latestVersion || 'unknown'}`}
      </button>
    </div>
  </div>
</div>
