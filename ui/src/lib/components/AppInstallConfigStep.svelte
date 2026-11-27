<script lang="ts">
  import Button from './Button.svelte';
  import FormField from './FormField.svelte';
  import SchemaForm from './SchemaForm.svelte';
  import EnvVarEditor from './EnvVarEditor.svelte';
  import type { EnvRow, ManagedDataset } from '$lib/appEnv';
  import { datasetBlocked } from '$lib/appEnv';

  export let app: {
    name: string;
    schema: { properties: Record<string, any>; required?: string[] };
    capabilities?: string[];
  };
  export let values: Record<string, any> = {};
  export let selectedDatasets: string[] = [];
  export let selectedPool = '';
  export let managedDatasets: ManagedDataset[] = [];
  export let datasetsLoaded = false;
  export let datasetsError = '';
  export let storagePools: string[] = [];
  export let poolsLoaded = false;
  export let poolsError = '';
  export let env: EnvRow[] = [];
  export let groupOptions: { value: number; label: string }[] = [];
  export let retryDatasets: () => void = () => {};
  export let retryPools: () => void = () => {};

  $: envSupported = app.capabilities?.includes('env') ?? false;
  $: sharedSelected = managedDatasets.filter(
    (d) => d.shared && selectedDatasets.includes(d.name)
  );
</script>

<div class="space-y-4">
  <SchemaForm {app} bind:values {groupOptions} />
  {#if envSupported}
    <details class="border border-naslos-border rounded-lg p-4" open={env.length > 0}>
      <summary class="cursor-pointer text-sm font-medium">
        Environment variables{env.length ? ` (${env.length})` : ''}
      </summary>
      <div class="mt-3">
        <EnvVarEditor bind:env requireValues />
      </div>
    </details>
  {/if}

  <fieldset>
    <legend class="label">Datasets</legend>
    {#if datasetsError}
      <p role="alert" class="text-xs text-red-300">
        Registered datasets are unavailable: {datasetsError}.
        <Button variant="ghost" size="sm" on:click={retryDatasets}>Retry</Button>
      </p>
    {:else if !datasetsLoaded}
      <div role="status" aria-busy="true" class="space-y-2 py-1">
        <span class="sr-only">Loading registered datasets…</span>
        <div class="skeleton h-3 w-48"></div>
        <div class="skeleton h-3 w-36"></div>
      </div>
    {:else if managedDatasets.length === 0}
      <p class="text-xs text-gray-500">
        No datasets are registered yet. Register one under Datasets to attach it to this app.
      </p>
    {:else}
      <div class="space-y-2">
        {#each managedDatasets as dataset (dataset.name)}
          <label class="flex items-start gap-2 cursor-pointer">
            <input
              type="checkbox"
              class="mt-1"
              value={dataset.name}
              bind:group={selectedDatasets}
              disabled={datasetBlocked(dataset, app.name)}
            />
            <span class="text-sm">
              <span class="font-mono">{dataset.name}</span> — {dataset.hostPath}
              ({dataset.shared ? 'shared' : 'private'})
              {#if datasetBlocked(dataset, app.name)}
                <span class="text-amber-300">attached to {dataset.attachedTo?.join(', ')}</span>
              {/if}
            </span>
          </label>
        {/each}
      </div>
      <p class="text-xs text-gray-500 mt-1">
        Each selected dataset is mounted at <span class="font-mono">/datasets/&lt;name&gt;</span>. A private
        dataset is attached to this app only; a shared dataset can be attached to several apps.
        Datasets are owned by group <span class="font-mono">naslos_users (10000)</span>; selecting one sets
        PUID/PGID to 10001/10000 so the app can read it.
      </p>
      {#each sharedSelected as dataset (dataset.name)}
        <p role="alert" class="text-xs text-amber-300 mt-1">
          Dataset <span class="font-mono">{dataset.name}</span> is shared: every app attached to it can read
          and modify its data.
        </p>
      {/each}
    {/if}
  </fieldset>

  <FormField
    label="Storage pool"
    id="install-pool"
    hint="App config, database and upload PVCs are created under /var/mnt/<pool>/apps. The dataset above is separate: it controls the app's media/data volume."
  >
    <select id="install-pool" class="input w-full" bind:value={selectedPool}>
      <option value="">Default (local-path)</option>
      {#each storagePools as pool (pool)}
        <option value={pool}>{pool}</option>
      {/each}
    </select>
  </FormField>
  {#if poolsError}
    <p role="alert" class="text-xs text-red-300">
      Storage pools are unavailable: {poolsError}.
      <Button variant="ghost" size="sm" on:click={retryPools}>Retry</Button>
    </p>
  {:else if !poolsLoaded}
    <div role="status" aria-busy="true" class="space-y-2 py-1">
      <span class="sr-only">Loading storage pools…</span>
      <div class="skeleton h-3 w-40"></div>
    </div>
  {/if}
</div>
