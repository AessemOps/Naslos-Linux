<script lang="ts">
  // Chains stored in a destination for one source: list, verify (structure or
  // content) and restore a chain into a new dataset. Verify and restore are
  // async jobs; this card starts them and the page polls the returned job.
  import { createEventDispatcher } from 'svelte';
  import { api, ApiError } from '$lib/api';
  import Button from '$lib/components/Button.svelte';
  import DataTable from '$lib/components/DataTable.svelte';
  import FormField from '$lib/components/FormField.svelte';
  import { formatBytes, fmtTime } from '$lib/format';
  import CloudJobStatus from './CloudJobStatus.svelte';
  import type { Chain, CloudJob, DatasetSummary, Destination, StartedJob } from '$lib/cloud/types';

  export let destinations: Destination[] = [];
  export let datasets: DatasetSummary[] = [];
  /** The restore/verify job the page is polling, if any. */
  export let job: CloudJob | null = null;
  export let jobError = '';
  export let cancelling = false;

  const dispatch = createEventDispatcher<{
    started: string;
    restore: { destination: string; source: string; chain: Chain };
    cancel: void;
  }>();

  let destination = '';
  let source = '';
  let chains: Chain[] = [];
  let listed: { destination: string; source: string } | null = null;
  let loading = false;
  let listError = '';
  let mode: 'structure' | 'content' = 'structure';
  let verifying = false;
  let verifyError = '';

  $: running = job?.state === 'running';
  $: totalBytes = chains.reduce((sum, c) => sum + (c.plainBytes || 0), 0);
  $: destName = Object.fromEntries(destinations.map((d) => [d.id, d.name]));

  function message(e: unknown): string {
    return e instanceof ApiError || e instanceof Error ? e.message : String(e);
  }

  async function list() {
    if (!destination || !source.trim()) return;
    loading = true;
    listError = '';
    const q = new URLSearchParams({ destination, source: source.trim() });
    try {
      const data = await api<{ chains?: Chain[] }>(`/api/cloud/chains?${q}`);
      chains = Array.isArray(data.chains) ? [...data.chains].sort((a, b) => a.sequence - b.sequence) : [];
      listed = { destination, source: source.trim() };
    } catch (e) {
      chains = [];
      listed = null;
      listError = 'Listing chains failed: ' + message(e);
    } finally {
      loading = false;
    }
  }

  async function verify() {
    if (!listed) return;
    verifying = true;
    verifyError = '';
    try {
      const r = await api<StartedJob>('/api/cloud/verify', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ destination: listed.destination, source: listed.source, mode })
      });
      dispatch('started', r.jobId);
    } catch (e) {
      verifyError =
        e instanceof ApiError && e.status === 409
          ? 'Another job for this source is running. Wait for it to finish, or cancel it first.'
          : 'Starting the verify failed: ' + message(e);
    } finally {
      verifying = false;
    }
  }
</script>

<section class="card mb-6" aria-labelledby="cloud-chains-heading">
  <h2 id="cloud-chains-heading" class="mb-2 text-lg font-bold">Stored backups</h2>
  <form class="grid grid-cols-1 items-end gap-3 md:grid-cols-3" on:submit|preventDefault={list}>
    <FormField label="Destination" id="cloud-chains-destination" required>
      <select id="cloud-chains-destination" class="input w-full" bind:value={destination}>
        <option value="">Destination…</option>
        {#each destinations as d (d.id)}
          <option value={d.id}>{d.name}</option>
        {/each}
      </select>
    </FormField>
    <FormField label="Source" id="cloud-chains-source" required hint="The backed-up dataset name.">
      <input
        id="cloud-chains-source"
        class="input w-full"
        list="cloud-chains-sources"
        bind:value={source}
        aria-describedby="cloud-chains-source-hint"
      />
      <datalist id="cloud-chains-sources">
        {#each datasets as d (d.name)}
          <option value={d.name}></option>
        {/each}
      </datalist>
    </FormField>
    <div>
      <Button type="submit" variant="secondary" loading={loading} disabled={!destination || !source.trim()}>
        List backups
      </Button>
    </div>
  </form>

  {#if listError}
    <p role="alert" class="mt-2 text-sm text-red-300">{listError}</p>
  {/if}

  {#if listed}
    {#if chains.length === 0}
      <p class="mt-3 text-sm text-gray-400">
        No backups of {listed.source} on {destName[listed.destination] ?? listed.destination}.
      </p>
    {:else}
      <div class="mt-4">
        <DataTable label="Stored cloud backups">
          <table>
            <thead>
              <tr>
                <th scope="col">#</th>
                <th scope="col">Kind</th>
                <th scope="col">Snapshot</th>
                <th scope="col">Created</th>
                <th scope="col">Chunks</th>
                <th scope="col">Size</th>
                <th scope="col">Actions</th>
              </tr>
            </thead>
            <tbody>
              {#each chains as c (c.chain)}
                <tr>
                  <td>{c.sequence}</td>
                  <td class="whitespace-nowrap">{c.kind}{c.raw ? ' · raw' : ''}</td>
                  <td class="max-w-56 truncate font-mono text-xs" title={`${c.chain}\nlineage ${c.lineage}`}>{c.toSnapshot}</td>
                  <td class="whitespace-nowrap">{fmtTime(c.createdAt)}</td>
                  <td>{c.chunks}</td>
                  <td class="whitespace-nowrap">{formatBytes(c.plainBytes)}</td>
                  <td>
                    <Button
                      size="sm"
                      variant="secondary"
                      disabled={running}
                      aria-label={`Restore ${c.toSnapshot}`}
                      on:click={() => listed && dispatch('restore', { ...listed, chain: c })}>Restore</Button
                    >
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        </DataTable>
      </div>

      <fieldset class="mt-4 space-y-2 text-sm">
        <legend class="label">Verify</legend>
        <label class="flex items-start gap-2">
          <input type="radio" class="mt-0.5" name="cloud-verify-mode" value="structure" bind:group={mode} />
          <span>Structure: list objects and check the signed manifests. Downloads only the manifests.</span>
        </label>
        <label class="flex items-start gap-2">
          <input type="radio" class="mt-0.5" name="cloud-verify-mode" value="content" bind:group={mode} />
          <span>Content: download, decrypt and hash every chunk.</span>
        </label>
        {#if mode === 'content'}
          <p class="rounded border border-amber-600 bg-amber-900/10 p-2 text-amber-300" data-testid="cloud-egress-warning">
            A content verify downloads every chunk of these backups, about {formatBytes(totalBytes)} before
            compression. Most providers bill this as egress.
          </p>
        {/if}
        <Button loading={verifying} disabled={running} on:click={verify}>Verify backups</Button>
      </fieldset>
    {/if}
  {/if}

  {#if verifyError}
    <p role="alert" class="mt-2 text-sm text-red-300">{verifyError}</p>
  {/if}
  {#if jobError}
    <p role="alert" class="mt-2 text-sm text-red-300">{jobError}</p>
  {/if}
  {#if job}
    <CloudJobStatus {job} label={job.kind === 'restore' ? 'Restore progress' : 'Verify progress'} {cancelling} on:cancel={() => dispatch('cancel')} />
  {/if}
</section>
