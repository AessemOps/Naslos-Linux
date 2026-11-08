<script lang="ts">
  import { modal } from '$lib/actions/modal';
  import { onMount, createEventDispatcher } from 'svelte';
  import SchemaForm from './SchemaForm.svelte';
  import { trackAppJob } from '$lib/stores/appJobs';

  export let appName: string;

  let app: { schema: { properties: Record<string, any>; required?: string[] } } | null = null;
  let values: Record<string, any> = {};
  let redactedKeys: string[] = [];
  let managedDatasets: { name: string; hostPath: string; shared: boolean; attachedTo?: string[] }[] = [];
  let selectedDatasets: string[] = [];
  // Records outside a per-app isolation namespace cannot gain datasets without
  // a reinstall (the API rejects a dataset change for them).
  let legacy = false;
  // An isolated app's chart has no fallback volume, so it must keep a dataset.
  let hasDatasets = false;
  // Same platform group choices as the install form.
  const groupOptions = [
    { value: 10000, label: 'naslos_users (10000) — dataset access' },
    { value: 1000, label: 'default (1000)' }
  ];
  let loading = true;
  let saving = false;
  let error = '';

  const dispatch = createEventDispatcher();

  function datasetBlocked(dataset: { name: string; shared: boolean; attachedTo?: string[] }): boolean {
    return (
      !dataset.shared &&
      (dataset.attachedTo?.length ?? 0) > 0 &&
      !(dataset.attachedTo ?? []).includes(appName)
    );
  }

  $: sharedSelected = managedDatasets.filter((d) => d.shared && selectedDatasets.includes(d.name));
  $: datasetError =
    !legacy && hasDatasets && selectedDatasets.length === 0
      ? 'An isolated app needs at least one dataset. Keep one or uninstall the app to remove its storage.'
      : '';

  async function load() {
    try {
      const [catalogRes, installedRes, registryRes] = await Promise.all([
        fetch(`/api/catalog/${encodeURIComponent(appName)}`),
        fetch(`/api/apps/${encodeURIComponent(appName)}`),
        fetch('/api/datasets/registry')
      ]);
      if (!catalogRes.ok) {
        throw new Error(`catalog HTTP ${catalogRes.status}`);
      }
      app = await catalogRes.json();
      if (registryRes.ok) {
        const registry = await registryRes.json();
        if (Array.isArray(registry)) managedDatasets = registry;
      }
      if (installedRes.ok) {
        const installed = await installedRes.json();
        values = { ...(installed.values || {}) };
        redactedKeys = Array.isArray(installed.redactedKeys) ? installed.redactedKeys : [];
        selectedDatasets = Array.isArray(installed.datasets) ? [...installed.datasets] : [];
        hasDatasets = selectedDatasets.length > 0;
        legacy = installed.isolated === false;
      }
    } catch (e) {
      error = 'Failed to load app configuration: ' + e;
    } finally {
      loading = false;
    }
  }

  async function save() {
    saving = true;
    error = '';
    try {
      const body: Record<string, unknown> = { values };
      // Omitting datasets keeps the stored selection; legacy records must not
      // be sent a different list (the API rejects it).
      if (!legacy) body.datasets = selectedDatasets;
      const res = await fetch(`/api/apps/${encodeURIComponent(appName)}`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body)
      });
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        throw new Error(data.error || `Update failed (HTTP ${res.status})`);
      }
      // The upgrade runs as a background job: close and let the drawer follow it.
      const data = await res.json();
      if (data.jobId) trackAppJob(data.jobId);
      dispatch('close');
    } catch (e: any) {
      error = e.message;
    } finally {
      saving = false;
    }
  }

  onMount(load);
</script>

<!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
<div class="fixed inset-0 bg-black/60 z-50 flex items-center justify-center p-4" use:modal={{ onClose: () => dispatch('close') }} on:click|self={() => dispatch('close')}>
  <div class="bg-naslos-surface rounded-2xl border border-naslos-border w-full max-w-lg max-h-[90vh] overflow-hidden flex flex-col">
    <div class="p-6 border-b border-naslos-border flex items-center justify-between">
      <h2 class="text-xl font-bold">Configure — {appName}</h2>
      <button class="text-gray-400 hover:text-white text-2xl" aria-label="Close" on:click={() => dispatch('close')}>×</button>
    </div>
    <div class="flex-1 overflow-y-auto p-6">
      {#if loading}
        <p class="text-gray-400">Loading configuration...</p>
      {:else if app}
        <SchemaForm {app} {redactedKeys} bind:values {groupOptions} />
        <div class="mt-4">
          <span class="label">Datasets</span>
          {#if legacy}
            <p class="text-xs text-amber-300">
              This app predates per-app isolation. Reinstall it to attach or change datasets.
            </p>
          {:else if managedDatasets.length === 0}
            <p class="text-xs text-gray-500">No datasets are registered yet.</p>
          {:else}
            <div class="space-y-2">
              {#each managedDatasets as dataset (dataset.name)}
                <label class="flex items-start gap-2 cursor-pointer">
                  <input
                    type="checkbox"
                    class="mt-1"
                    value={dataset.name}
                    bind:group={selectedDatasets}
                    disabled={datasetBlocked(dataset)}
                  />
                  <span class="text-sm">
                    <span class="font-mono">{dataset.name}</span> — {dataset.hostPath}
                    ({dataset.shared ? 'shared' : 'private'})
                    {#if datasetBlocked(dataset)}
                      <span class="text-amber-300">attached to {dataset.attachedTo?.join(', ')}</span>
                    {/if}
                  </span>
                </label>
              {/each}
            </div>
            <p class="text-xs text-gray-500 mt-1">
              Each selected dataset is mounted at <span class="font-mono">/datasets/&lt;name&gt;</span>. Removing one
              detaches it after the upgrade succeeds. Datasets are owned by group
              <span class="font-mono">naslos_users (10000)</span>; set PGID to 10000 if the app must read them.
            </p>
            {#each sharedSelected as dataset (dataset.name)}
              <p role="alert" class="text-xs text-amber-300 mt-1">
                Dataset <span class="font-mono">{dataset.name}</span> is shared: every app attached to it can read
                and modify its data.
              </p>
            {/each}
          {/if}
        </div>
      {/if}
      {#if datasetError}<div role="alert" class="mt-4 p-3 rounded-lg bg-amber-900/30 border border-amber-700 text-amber-200 text-sm">{datasetError}</div>{/if}
      {#if error}<div role="alert" class="mt-4 p-3 rounded-lg bg-red-900/30 border border-red-700 text-red-300 text-sm">{error}</div>{/if}
    </div>
    <div class="p-6 border-t border-naslos-border flex justify-end gap-3">
      <button class="btn btn-secondary" on:click={() => dispatch('close')}>Cancel</button>
      <button class="btn btn-primary" on:click={save} disabled={saving || loading || !!datasetError}>{saving ? 'Saving...' : 'Save'}</button>
    </div>
  </div>
</div>
