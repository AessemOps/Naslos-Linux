<script lang="ts">
  import { onMount } from 'svelte';
  import { api, ApiError } from '$lib/api';
  import AsyncState from '$lib/components/AsyncState.svelte';
  import Button from '$lib/components/Button.svelte';
  import FormField from '$lib/components/FormField.svelte';
  import PageHeader from '$lib/components/PageHeader.svelte';
  import { confirmAction } from '$lib/stores/confirm';

  interface ManagedDataset {
    name: string;
    hostPath: string;
    shared: boolean;
    warningAcknowledged: boolean;
    createdAt?: string;
    attachedTo?: string[];
  }

  interface ZfsDataset {
    name: string;
    mountpoint: string;
    mounted: boolean;
  }

  let managed: ManagedDataset[] = [];
  let zfs: ZfsDataset[] = [];
  let loading = true;
  let saving = false;
  let registryError = '';
  let datasetsError = '';
  let formError = '';
  let success = '';

  let selectedPath = '';
  let name = '';
  let shared = false;
  let acknowledged = false;
  let removingName = '';

  $: available = zfs
    .filter((d) => d.mounted && d.mountpoint && d.mountpoint.startsWith('/var/mnt/'))
    .sort((a, b) => a.mountpoint.localeCompare(b.mountpoint));

  function apiMessage(e: unknown, fallback: string): string {
    return e instanceof ApiError ? e.message : fallback;
  }

  function suggestName(path: string) {
    const part = path.split('/').filter(Boolean).pop() || '';
    return part
      .toLowerCase()
      .replace(/[^a-z0-9-]+/g, '-')
      .replace(/^-+|-+$/g, '')
      .slice(0, 63);
  }

  function pick(path: string) {
    selectedPath = path;
    if (!name) name = suggestName(path);
  }

  // The registry list and the dataset picker are independent: a failure on
  // one must not discard the other response or merge their error messages.
  async function loadRegistry() {
    try {
      const data = await api<unknown>('/api/datasets/registry');
      managed = Array.isArray(data) ? (data as ManagedDataset[]) : [];
      registryError = '';
    } catch (e) {
      registryError =
        e instanceof ApiError
          ? `Failed to load registered datasets (HTTP ${e.status})`
          : 'Failed to connect to the API';
    }
  }

  async function loadDatasets() {
    try {
      const data = await api<unknown>('/api/datasets');
      zfs = Array.isArray(data) ? (data as ZfsDataset[]) : [];
      datasetsError = '';
    } catch (e) {
      datasetsError =
        e instanceof ApiError
          ? `Failed to load mountable ZFS datasets (HTTP ${e.status})`
          : 'Failed to connect to the API';
    }
  }

  async function load() {
    // Retries set loading=true again; both requests still run independently.
    loading = true;
    await Promise.all([loadRegistry(), loadDatasets()]);
    loading = false;
  }

  async function save() {
    formError = '';
    success = '';
    if (!name || !selectedPath) {
      formError = 'Choose a ZFS dataset and a name.';
      return;
    }
    if (shared && !acknowledged) {
      formError = 'Acknowledge the shared-dataset warning first.';
      return;
    }
    saving = true;
    try {
      const data = await api<{ name: string }>('/api/datasets/registry', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          name,
          hostPath: selectedPath,
          shared,
          warningAcknowledged: shared ? acknowledged : false
        })
      });
      success = `Registered ${data.name}.`;
      name = '';
      selectedPath = '';
      shared = false;
      acknowledged = false;
      await load();
    } catch (e) {
      formError = apiMessage(e, 'Registration failed. Check the API connection and try again.');
    } finally {
      saving = false;
    }
  }

  async function remove(datasetName: string) {
    if (
      !(await confirmAction({
        title: `Unregister ${datasetName}`,
        message: `Unregister “${datasetName}”? Apps already using it keep their claim.`,
        confirmLabel: 'Unregister',
        tone: 'danger'
      }))
    ) {
      return;
    }
    formError = '';
    success = '';
    removingName = datasetName;
    try {
      await api(`/api/datasets/registry/${encodeURIComponent(datasetName)}`, { method: 'DELETE' });
      success = `Unregistered ${datasetName}.`;
      await load();
    } catch (e) {
      formError = apiMessage(e, 'Unregister failed. Check the API connection and try again.');
    } finally {
      removingName = '';
    }
  }

  onMount(load);
</script>

<div class="mx-auto max-w-6xl">
  <PageHeader
    title="Datasets"
    subtitle="Register the ZFS datasets an installed app may mount. Apps never get the whole /var/mnt tree."
    breadcrumbs={[{ label: 'Storage' }, { label: 'Datasets' }]}
  />

  {#if formError}<div role="alert" class="callout callout-bad">{formError}</div>{/if}
  {#if success}<div role="status" class="callout callout-ok">{success}</div>{/if}

  <div class="card mb-8">
    <h2 class="mb-4 text-xl font-bold">Register a dataset</h2>
    <form on:submit|preventDefault={save} class="space-y-4">
      <div class="grid gap-4 md:grid-cols-2">
        <FormField label="ZFS dataset" id="dataset-path" required error={datasetsError}>
          <select
            id="dataset-path"
            class="input w-full"
            bind:value={selectedPath}
            on:change={() => pick(selectedPath)}
            required
          >
            <option value="">Select a mounted dataset…</option>
            {#each available as dataset (dataset.mountpoint)}
              <option value={dataset.mountpoint}>{dataset.name} — {dataset.mountpoint}</option>
            {/each}
          </select>
        </FormField>
        <FormField label="Name" id="dataset-name" required hint="DNS-like name apps use to request this dataset.">
          <input
            id="dataset-name"
            class="input w-full"
            bind:value={name}
            placeholder="e.g. media"
            required
          />
        </FormField>
      </div>

      <label class="flex cursor-pointer items-start gap-3">
        <input type="checkbox" class="mt-1" bind:checked={shared} />
        <span>Share this dataset with multiple apps</span>
      </label>

      {#if shared}
        <div role="alert" class="callout callout-bad">
          <p class="mb-1 font-medium">Shared datasets have no isolation</p>
          <p class="text-sm">
            Every app attached to this dataset can read, modify and delete all of
            its data. Only share a dataset that is meant to be common to those apps
            (for example a media library). Private datasets should be registered
            without this option and attached to one app.
          </p>
        </div>
        <label class="flex cursor-pointer items-start gap-3">
          <input type="checkbox" class="mt-1" bind:checked={acknowledged} />
          <span class="font-medium">
            I understand that every attached app can read and modify this dataset.
          </span>
        </label>
      {/if}

      <div class="flex justify-end">
        <Button type="submit" loading={saving} disabled={shared && !acknowledged}>
          Register dataset
        </Button>
      </div>
    </form>
  </div>

  <h2 class="mb-4 text-xl font-bold">Registered datasets</h2>
  {#if registryError && managed.length > 0}
    <div role="alert" class="callout callout-bad">
      Could not refresh the registry; showing the last known list. {registryError}
    </div>
  {/if}
  <AsyncState
    {loading}
    error={managed.length === 0 ? registryError : ''}
    empty={managed.length === 0}
    loadingText="Loading…"
    emptyTitle="No datasets are registered yet."
    emptyDetail="Register a dataset above to make it available to apps."
    emptyIcon="folder-tree"
    onRetry={load}
  >
    <div class="space-y-3">
      {#each managed as dataset (dataset.name)}
        <div class="card flex flex-wrap items-center justify-between gap-3">
          <div class="min-w-0">
            <div class="flex flex-wrap items-center gap-2">
              <h3 class="font-bold">{dataset.name}</h3>
              {#if dataset.shared}
                <span class="badge badge-warn">Shared</span>
              {:else}
                <span class="badge badge-neutral">Private</span>
              {/if}
            </div>
            <p class="truncate font-mono text-sm text-gray-400">{dataset.hostPath}</p>
            {#if dataset.attachedTo?.length}
              <p class="mt-1 text-xs text-gray-500">
                Attached to {dataset.attachedTo.join(', ')}
              </p>
            {/if}
          </div>
          <Button
            variant="danger"
            loading={removingName === dataset.name}
            disabled={removingName !== '' && removingName !== dataset.name}
            on:click={() => remove(dataset.name)}
          >
            Unregister
          </Button>
        </div>
      {/each}
    </div>
  </AsyncState>
</div>
