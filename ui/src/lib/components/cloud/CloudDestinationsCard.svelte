<script lang="ts">
  // S3 destinations: list, connection test, edit and delete. Credentials are
  // never shown; only whether each one is configured.
  import { createEventDispatcher } from 'svelte';
  import { api, ApiError } from '$lib/api';
  import Button from '$lib/components/Button.svelte';
  import EmptyState from '$lib/components/EmptyState.svelte';
  import StatusBadge from '$lib/components/StatusBadge.svelte';
  import { fmtRelative, fmtTime } from '$lib/format';
  import { confirmAction } from '$lib/stores/confirm';
  import { categoryText, type Destination, type TestResult } from '$lib/cloud/types';

  export let destinations: Destination[] = [];

  const dispatch = createEventDispatcher<{ add: void; edit: Destination; refresh: void }>();

  let testing = '';
  let deleting = '';
  let results: Record<string, { ok: boolean; text: string }> = {};
  let cardError = '';

  function message(e: unknown): string {
    return e instanceof ApiError || e instanceof Error ? e.message : String(e);
  }

  // A test result describes the connection settings it was run against. Keying
  // the cache on them (and the credential flags) drops the result when an edit
  // changes any of those, instead of leaving a stale "Connection OK".
  function resultKey(d: Destination): string {
    return [
      d.id,
      d.endpoint,
      d.region,
      d.bucket,
      d.prefix,
      d.pathStyle,
      d.allowInsecureHttp,
      d.hasAccessKey,
      d.hasSecretKey,
      d.hasSessionToken
    ].join('\u0000');
  }

  async function test(d: Destination) {
    const key = resultKey(d);
    testing = d.id;
    cardError = '';
    try {
      const r = await api<TestResult>(`/api/cloud/destinations/${encodeURIComponent(d.id)}/test`, {
        method: 'POST'
      });
      results = {
        ...results,
        [key]: r.ok
          ? {
              ok: true,
              text: r.conditionalCreate
                ? 'Connection OK. Conditional create is supported.'
                : 'Connection OK. Conditional create is not supported; uploads use a read-back check.'
            }
          : { ok: false, text: categoryText(r.category) }
      };
      dispatch('refresh');
    } catch (e) {
      results = { ...results, [key]: { ok: false, text: 'The test could not run: ' + message(e) } };
    } finally {
      testing = '';
    }
  }

  async function remove(d: Destination) {
    const ok = await confirmAction({
      title: 'Delete destination',
      message: `Delete the destination "${d.name}"? Objects already in the bucket are not deleted.`,
      confirmLabel: 'Delete',
      tone: 'danger'
    });
    if (!ok) return;
    deleting = d.id;
    cardError = '';
    try {
      await api(`/api/cloud/destinations/${encodeURIComponent(d.id)}`, { method: 'DELETE' });
      dispatch('refresh');
    } catch (e) {
      cardError =
        e instanceof ApiError && e.status === 409
          ? `"${d.name}" is used by a running job or a schedule. Remove it from the schedules or wait for the job first.`
          : 'Deleting the destination failed: ' + message(e);
    } finally {
      deleting = '';
    }
  }

  function credText(d: Destination): string {
    if (!d.hasAccessKey || !d.hasSecretKey) return 'credentials missing';
    return d.hasSessionToken ? 'credentials set (with session token)' : 'credentials set';
  }
</script>

<section class="card mb-6" aria-labelledby="cloud-destinations-heading">
  <div class="mb-3 flex flex-wrap items-center justify-between gap-2">
    <h2 id="cloud-destinations-heading" class="text-lg font-bold">Destinations</h2>
    {#if destinations.length > 0}
      <Button size="sm" on:click={() => dispatch('add')}>Add destination</Button>
    {/if}
  </div>

  {#if cardError}
    <p role="alert" class="mb-3 text-sm text-red-300">{cardError}</p>
  {/if}

  {#if destinations.length === 0}
    <EmptyState
      icon="cloud"
      title="No cloud destinations yet"
      detail="Add an S3-compatible bucket (AWS S3, Backblaze B2, Cloudflare R2, MinIO) to back up datasets to it."
    >
      <svelte:fragment slot="action">
        <Button on:click={() => dispatch('add')}>Add destination</Button>
      </svelte:fragment>
    </EmptyState>
  {:else}
    <ul class="divide-y divide-naslos-border">
      {#each destinations as d (d.id)}
        {@const result = results[resultKey(d)]}
        <li class="py-3" data-testid="cloud-destination">
          <div class="flex flex-wrap items-start justify-between gap-3">
            <div class="min-w-0">
              <p class="flex flex-wrap items-center gap-2 font-medium">
                {d.name}
                <StatusBadge state={d.enabled ? 'active' : 'disabled'} label={d.enabled ? 'enabled' : 'disabled'} />
                {#if d.allowInsecureHttp}
                  <StatusBadge tone="warn" label="plaintext http" />
                {/if}
              </p>
              <p class="truncate font-mono text-xs text-gray-400" title={d.endpoint}>
                {d.endpoint} · {d.bucket}{d.prefix ? `/${d.prefix}` : ''}{d.region ? ` · ${d.region}` : ''}
              </p>
              <p class="text-xs text-gray-500">
                {credText(d)}{d.pathStyle ? ' · path-style' : ''} ·
                {#if d.lastTestAt}
                  <span title={fmtTime(d.lastTestAt)}>tested {fmtRelative(d.lastTestAt)}</span>
                {:else}
                  never tested
                {/if}
              </p>
              {#if d.lastError}
                <p class="text-xs text-red-300">Last error: {d.lastError}</p>
              {/if}
            </div>
            <div class="flex flex-wrap gap-1">
              <Button
                size="sm"
                variant="secondary"
                loading={testing === d.id}
                aria-label={`Test destination ${d.name}`}
                on:click={() => test(d)}>Test</Button
              >
              <Button size="sm" variant="secondary" aria-label={`Edit destination ${d.name}`} on:click={() => dispatch('edit', d)}
                >Edit</Button
              >
              <Button
                size="sm"
                variant="danger"
                loading={deleting === d.id}
                aria-label={`Delete destination ${d.name}`}
                on:click={() => remove(d)}>Delete</Button
              >
            </div>
          </div>
          {#if result}
            {#if result.ok}
              <p role="status" class="mt-2 text-sm text-green-300">{result.text}</p>
            {:else}
              <p role="alert" class="mt-2 text-sm text-red-300">{result.text}</p>
            {/if}
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
</section>
