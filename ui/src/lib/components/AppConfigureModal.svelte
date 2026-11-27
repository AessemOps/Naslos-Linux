<script lang="ts">
  import { onMount, createEventDispatcher } from 'svelte';
  import AsyncState from './AsyncState.svelte';
  import Button from './Button.svelte';
  import Modal from './Modal.svelte';
  import SchemaForm from './SchemaForm.svelte';
  import EnvVarEditor from './EnvVarEditor.svelte';
  import type { EnvRow, ManagedDataset } from '$lib/appEnv';
  import { datasetBlocked, validateEnvRows } from '$lib/appEnv';
  import { trackAppJob } from '$lib/stores/appJobs';

  export let appName: string;

  let app: {
    schema: { properties: Record<string, any>; required?: string[] };
    capabilities?: string[];
  } | null = null;
  let values: Record<string, any> = {};
  let redactedKeys: string[] = [];
  let managedDatasets: ManagedDataset[] = [];
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
  // The installed record must load before Save can send anything: a failed GET
  // used to leave the defaults in place and Save then wiped the stored values
  // and dataset selection.
  let loaded = false;
  let loadError = '';
  let datasetsError = '';
  let saving = false;
  let error = '';
  let env: EnvRow[] = [];
  /// Names whose value is already in the env Secret (an empty secret field
  /// means "keep it").
  let storedNames = new Set<string>();
  let envUnsupported = false;

  const dispatch = createEventDispatcher();

  $: envSupported = app?.capabilities?.includes('env') ?? false;

  $: sharedSelected = managedDatasets.filter(
    (d) => d.shared && selectedDatasets.includes(d.name)
  );
  $: datasetError =
    !legacy && hasDatasets && selectedDatasets.length === 0
      ? 'An isolated app needs at least one dataset. Keep one or uninstall the app to remove its storage.'
      : '';

  async function load() {
    loading = true;
    loaded = false;
    loadError = '';
    datasetsError = '';
    error = '';
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
      if (!installedRes.ok) {
        // Blocking: saving the defaults here would overwrite the real values.
        throw new Error(`the installed record is unavailable (HTTP ${installedRes.status})`);
      }
      const installed = await installedRes.json();
      values = { ...(installed.values || {}) };
      redactedKeys = Array.isArray(installed.redactedKeys) ? installed.redactedKeys : [];
      selectedDatasets = Array.isArray(installed.datasets) ? [...installed.datasets] : [];
      hasDatasets = selectedDatasets.length > 0;
      legacy = installed.isolated === false;
      const installedEnv = Array.isArray(installed.env) ? installed.env : [];
      env = installedEnv.map((row: any) => ({
        name: String(row.name ?? ''),
        value: row.value === null || row.value === undefined ? null : String(row.value),
        secret: !!row.secret
      }));
      storedNames = new Set(env.map((row) => row.name));
      envUnsupported = installedEnv.length > 0 && !(app?.capabilities?.includes('env') ?? false);
      if (registryRes.ok) {
        const registry = await registryRes.json();
        if (Array.isArray(registry)) managedDatasets = registry;
      } else {
        // The current selection is already loaded; the registry only feeds the
        // picker, so warn instead of blocking the whole form.
        datasetsError = `Registered datasets are unavailable (HTTP ${registryRes.status}).`;
      }
      loaded = true;
    } catch (e) {
      loadError = 'Failed to load app configuration: ' + (e instanceof Error ? e.message : String(e));
    } finally {
      loading = false;
    }
  }

  async function save() {
    if (!loaded || !app) {
      error = 'The app configuration could not be loaded; retry before saving.';
      return;
    }
    const submittedEnv = validateEnvRows(env, { supported: envSupported });
    if (submittedEnv.rows === null) {
      error = submittedEnv.error;
      return;
    }
    saving = true;
    error = '';
    try {
      const body: Record<string, unknown> = { values };
      // Omitting datasets keeps the stored selection; legacy records must not
      // be sent a different list (the API rejects it).
      if (!legacy) body.datasets = selectedDatasets;
      // Omitting env keeps the stored Secret; an array replaces it.
      if (envSupported) body.env = submittedEnv.rows;
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

<Modal title="Configure — {appName}" onClose={() => dispatch('close')}>
  <AsyncState
    {loading}
    error={loadError}
    loadingText="Loading configuration..."
    onRetry={load}
  >
    {#if app}
      <form id="app-configure-form" on:submit|preventDefault={save} class="space-y-4">
        <SchemaForm {app} {redactedKeys} bind:values {groupOptions} />
        {#if envSupported}
          <div>
            <h3 class="label">Environment variables</h3>
            <EnvVarEditor bind:env {storedNames} />
          </div>
        {:else if envUnsupported}
          <p class="text-xs text-amber-300">
            This chart no longer supports custom environment variables. The existing values are kept in the
            app Secret but are no longer mounted into the containers.
          </p>
        {/if}
        <fieldset>
          <legend class="label">Datasets</legend>
          {#if legacy}
            <p class="text-xs text-amber-300">
              This app predates per-app isolation. Reinstall it to attach or change datasets.
            </p>
          {:else if datasetsError}
            <p role="alert" class="text-xs text-red-300">
              {datasetsError}
              <Button variant="ghost" size="sm" on:click={load}>Retry</Button>
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
                    disabled={datasetBlocked(dataset, appName)}
                  />
                  <span class="text-sm">
                    <span class="font-mono">{dataset.name}</span> — {dataset.hostPath}
                    ({dataset.shared ? 'shared' : 'private'})
                    {#if datasetBlocked(dataset, appName)}
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
        </fieldset>
        {#if datasetError}
          <p role="alert" class="callout callout-warn mb-0">{datasetError}</p>
        {/if}
        {#if error}
          <p role="alert" class="callout callout-bad mb-0">{error}</p>
        {/if}
      </form>
    {/if}
  </AsyncState>

  <svelte:fragment slot="footer">
    <Button variant="secondary" on:click={() => dispatch('close')}>Cancel</Button>
    <Button
      type="submit"
      form="app-configure-form"
      loading={saving}
      disabled={!loaded || loading || !!datasetError}
    >
      {saving ? 'Saving...' : 'Save'}
    </Button>
  </svelte:fragment>
</Modal>
