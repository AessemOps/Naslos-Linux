<script lang="ts">
  import { createEventDispatcher } from 'svelte';
  import Button from './Button.svelte';
  import Modal from './Modal.svelte';
  import { trackAppJob } from '$lib/stores/appJobs';
  import AppIcon from './AppIcon.svelte';
  import type { AppView } from '$lib/appTypes';

  export let app: AppView;

  const dispatch = createEventDispatcher();

  let saving = false;
  let error = '';

  $: displayName = app.displayName || app.name;
  $: imageOnly = !app.updateAvailable && !!app.imageUpdateAvailable;
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

<Modal title="Update {displayName}" size="sm" onClose={() => dispatch('close')}>
  <div class="flex items-center gap-3">
    <AppIcon icon={app.icon} iconUrl={app.iconUrl} alt="" size="text-3xl" imgClass="h-9 w-9" />
    <div class="text-sm text-gray-400">
      <p class="text-base font-semibold text-gray-100">{displayName}</p>
      <p class="tabular-nums">
        {#if imageOnly}
          new image{app.imageLatest ? ` → ${app.imageLatest}` : ''}
        {:else}
          v{app.installedVersion || 'unknown'} → v{app.latestVersion || 'unknown'}
        {/if}
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

  <svelte:fragment slot="footer">
    <Button variant="secondary" disabled={saving} on:click={() => dispatch('close')}>Cancel</Button>
    <Button loading={saving} on:click={confirmUpdate}>
      {saving
        ? 'Starting…'
        : imageOnly
          ? 'Update image'
          : `Update to v${app.latestVersion || 'unknown'}`}
    </Button>
  </svelte:fragment>
</Modal>
