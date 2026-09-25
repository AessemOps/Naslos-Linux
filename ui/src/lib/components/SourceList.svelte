<script lang="ts">
  import { onMount } from 'svelte';
  import SourceForm from './SourceForm.svelte';

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
  let error = '';
  let refreshing = false;
  let showForm = false;

  async function load() {
    loading = true;
    error = '';
    try {
      const res = await fetch('/api/sources');
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}`);
      }
      const data = await res.json();
      sources = Array.isArray(data) ? data : [];
    } catch (e) {
      error = 'Failed to load sources: ' + e;
      sources = [];
    } finally {
      loading = false;
    }
  }

  async function refresh() {
    refreshing = true;
    error = '';
    try {
      const res = await fetch('/api/sources/refresh', { method: 'POST' });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) {
        throw new Error(data.errors?.join('; ') || data.error || `HTTP ${res.status}`);
      }
      await load();
    } catch (e: any) {
      error = 'Refresh failed: ' + e.message;
    } finally {
      refreshing = false;
    }
  }

  async function remove(name: string) {
    if (!confirm(`Remove source ${name}? Its cached clones are deleted.`)) {
      return;
    }
    try {
      const res = await fetch(`/api/sources/${name}`, { method: 'DELETE' });
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}`);
      }
      load();
    } catch (e) {
      error = `Failed to remove ${name}: ` + e;
    }
  }

  onMount(load);
</script>

<div>
  <div class="flex justify-between items-center mb-4">
    <p class="text-sm text-gray-400">
      Git repositories the catalog is cloned from. A user source overrides the
      official source on name collision.
    </p>
    <div class="flex gap-2">
      <button class="btn btn-secondary" on:click={refresh} disabled={refreshing}>{refreshing ? 'Refreshing...' : 'Refresh'}</button>
      <button class="btn btn-primary" on:click={() => showForm = true}>+ Add Source</button>
    </div>
  </div>

  {#if error}
    <div class="card border-red-700 bg-red-900/30 text-red-300 p-4 mb-4">{error}</div>
  {/if}

  {#if loading}
    <p class="text-gray-400">Loading sources...</p>
  {:else if sources.length === 0}
    <div class="card text-center py-12">
      <div class="text-5xl mb-4">🔗</div>
      <h2 class="text-xl font-bold mb-2">No Chart Repositories</h2>
      <p class="text-gray-400 mb-4">Add a source, then refresh to populate the catalog.</p>
    </div>
  {:else}
    <div class="space-y-3">
      {#each sources as source (source.name)}
        <div class="card flex items-center gap-4">
          <div class="flex-1 min-w-0">
            <div class="flex items-center gap-2 mb-1">
              <h3 class="font-bold">{source.name}</h3>
              {#if source.official}<span class="text-xs px-2 py-0.5 rounded bg-blue-900/40 text-blue-300">official</span>{/if}
              <span class="text-xs px-2 py-0.5 rounded bg-naslos-border text-gray-300">{source.auth}</span>
            </div>
            <p class="text-sm text-gray-400 truncate">{source.url}</p>
            {#if source.lastError}
              <p class="text-xs text-amber-400 mt-1">{source.lastError}</p>
            {:else if source.lastSync}
              <p class="text-xs text-gray-500 mt-1">Last synced {source.lastSync}</p>
            {/if}
          </div>
          <div class="flex gap-2">
            {#if !source.official}
              <button class="btn btn-danger" on:click={() => remove(source.name)}>Remove</button>
            {/if}
          </div>
        </div>
      {/each}
    </div>
  {/if}

  {#if showForm}
    <SourceForm on:close={() => { showForm = false; load(); }} />
  {/if}
</div>
