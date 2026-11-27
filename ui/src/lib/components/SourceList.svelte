<script lang="ts">
  import { onMount } from 'svelte';
  import AsyncState from './AsyncState.svelte';
  import Button from './Button.svelte';
  import SourceForm from './SourceForm.svelte';
  import StatusBadge from './StatusBadge.svelte';
  import { confirmAction } from '$lib/stores/confirm';

  interface Source {
    name: string;
    displayName?: string;
    url: string;
    auth: string;
    official?: boolean;
    lastSync?: string;
    lastError?: string;
  }

  let sources: Source[] = [];
  let loading = true;
  // A load failure blocks the page; a refresh/remove failure keeps the last
  // good list and is reported inline.
  let loadError = '';
  let actionError = '';
  let refreshing = false;
  let removing = '';
  let showForm = false;

  async function load() {
    loading = true;
    loadError = '';
    try {
      const res = await fetch('/api/sources');
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}`);
      }
      const data = await res.json();
      sources = Array.isArray(data) ? data : [];
    } catch (e) {
      loadError = 'Failed to load sources: ' + e;
    } finally {
      loading = false;
    }
  }

  async function refresh() {
    refreshing = true;
    actionError = '';
    try {
      const res = await fetch('/api/sources/refresh', { method: 'POST' });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) {
        throw new Error(data.errors?.join('; ') || data.error || `HTTP ${res.status}`);
      }
      await load();
    } catch (e: any) {
      actionError = 'Refresh failed: ' + e.message;
    } finally {
      refreshing = false;
    }
  }

  async function remove(name: string) {
    const confirmed = await confirmAction({
      title: 'Remove source',
      message: `Remove source ${name}? Its cached clones are deleted.`,
      confirmLabel: 'Remove',
      tone: 'danger'
    });
    if (!confirmed) return;
    removing = name;
    actionError = '';
    try {
      const res = await fetch(`/api/sources/${encodeURIComponent(name)}`, { method: 'DELETE' });
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}`);
      }
      await load();
    } catch (e) {
      actionError = `Failed to remove ${name}: ` + e;
    } finally {
      removing = '';
    }
  }

  onMount(load);
</script>

<div>
  <div class="mb-4 flex flex-wrap items-center justify-between gap-3">
    <p class="text-sm text-gray-400">
      Git repositories the catalog is cloned from. A user source overrides the
      official source on name collision.
    </p>
    <div class="flex flex-wrap gap-2">
      <Button variant="secondary" loading={refreshing} on:click={refresh}>
        {refreshing ? 'Refreshing...' : 'Refresh'}
      </Button>
      <Button on:click={() => (showForm = true)}>+ Add Source</Button>
    </div>
  </div>

  <AsyncState
    loading={loading && sources.length === 0}
    error={sources.length === 0 ? loadError : ''}
    empty={!loading && sources.length === 0}
    loadingText="Loading sources..."
    emptyTitle="No Chart Repositories"
    emptyDetail="Add a source, then refresh to populate the catalog."
    emptyIcon="link"
    onRetry={load}
  >
    {#if loadError && sources.length > 0}
      <div role="alert" class="callout callout-bad flex flex-wrap items-center justify-between gap-2">
        <span>{loadError}</span>
        <Button variant="secondary" size="sm" on:click={load}>Retry</Button>
      </div>
    {/if}
    {#if actionError}
      <div role="alert" class="callout callout-bad">{actionError}</div>
    {/if}
    <div class="space-y-3">
      {#each sources as source (source.name)}
        <div class="card flex flex-wrap items-center gap-4">
          <div class="flex-1 min-w-0">
            <div class="flex flex-wrap items-center gap-2 mb-1">
              <h3 class="font-bold">{source.name}</h3>
              {#if source.official}
                <StatusBadge state="official" label="official" tone="info" />
              {/if}
              <StatusBadge state={source.auth} label={source.auth} tone="neutral" />
            </div>
            <p class="text-sm text-gray-400 truncate font-mono">{source.url}</p>
            {#if source.lastError}
              <p class="text-xs text-amber-400 mt-1">{source.lastError}</p>
            {:else if source.lastSync}
              <p class="text-xs text-gray-500 mt-1">Last synced {source.lastSync}</p>
            {/if}
          </div>
          <div class="flex gap-2">
            {#if !source.official}
              <Button
                variant="danger"
                size="sm"
                loading={removing === source.name}
                on:click={() => remove(source.name)}
              >
                Remove
              </Button>
            {/if}
          </div>
        </div>
      {/each}
    </div>
  </AsyncState>

  {#if showForm}
    <SourceForm on:close={() => { showForm = false; load(); }} />
  {/if}
</div>
