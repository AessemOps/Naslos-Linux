<script lang="ts">
  import { createEventDispatcher } from 'svelte';
  import Button from './Button.svelte';
  import Modal from './Modal.svelte';
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

  function versionLabel(app: AppView): string {
    if (app.updateAvailable) {
      return `v${app.installedVersion || 'unknown'} → v${app.latestVersion || 'unknown'}`;
    }
    return app.imageLatest ? `new image → ${app.imageLatest}` : 'new image';
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

<Modal
  title="Update {total} {total === 1 ? 'app' : 'apps'}"
  onClose={requestClose}
>
  <p class="text-sm text-gray-300">
    Each app restarts while its configured values and attached datasets are preserved.
  </p>

  <ul class="divide-y divide-naslos-border rounded-lg border border-naslos-border overflow-hidden max-h-64 overflow-y-auto">
    {#each apps as app (app.name)}
      <li class="flex items-center gap-3 px-3 py-2">
        <AppIcon icon={app.icon} iconUrl={app.iconUrl} alt="" size="text-xl" imgClass="h-6 w-6" />
        <span class="min-w-0 flex-1 truncate text-sm font-medium">{displayName(app)}</span>
        <span class="text-xs text-gray-400 tabular-nums">
          {versionLabel(app)}
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

  <svelte:fragment slot="footer">
    <Button variant="secondary" disabled={saving} on:click={requestClose}>Cancel</Button>
    <Button
      loading={saving}
      disabled={saving || total === 0}
      on:click={confirmUpdateAll}
    >
      {saving ? `Starting ${Math.min(progress + 1, total)} of ${total}…` : `Update ${total}`}
    </Button>
  </svelte:fragment>
</Modal>
