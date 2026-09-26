<script lang="ts">
  import { onMount } from 'svelte';
  import DdnsForm from '$lib/components/DdnsForm.svelte';

  interface Entry {
    id: string;
    provider: string;
    zone: string;
    record: string;
    recordType: string;
    ttl?: number;
    enabled: boolean;
    credentialsSecret?: string;
    credentialFields?: string[];
    lastIP?: string;
    lastStatus?: string;
    lastError?: string;
    lastRunAt?: string;
    nextRunAt?: string;
  }

  let entries: Entry[] = [];
  let providers: any[] = [];
  let enabled = true;
  let intervalSeconds = 300;
  let loading = true;
  let error = '';
  let showForm = false;
  let editing: Entry | null = null;
  let busy = '';

  async function load() {
    loading = true;
    error = '';
    try {
      const [ddnsRes, providerRes] = await Promise.all([fetch('/api/ddns'), fetch('/api/providers')]);
      if (!ddnsRes.ok) {
        throw new Error(`HTTP ${ddnsRes.status}`);
      }
      const data = await ddnsRes.json();
      entries = Array.isArray(data.entries) ? data.entries : [];
      enabled = data.enabled !== false;
      intervalSeconds = data.intervalSeconds || 300;
      if (providerRes.ok) {
        const providersData = await providerRes.json();
        providers = Array.isArray(providersData.providers) ? providersData.providers : [];
      }
    } catch (e) {
      error = 'Failed to load dynamic DNS: ' + e;
      entries = [];
    } finally {
      loading = false;
    }
  }

  function providerName(name: string): string {
    const p = providers.find((item) => item.name === name);
    return p?.displayName || name;
  }

  function statusClass(status?: string): string {
    switch (status) {
      case 'ok': return 'bg-green-900/50 text-green-400';
      case 'error': return 'bg-red-900/50 text-red-400';
      default: return 'bg-gray-900/50 text-gray-400';
    }
  }

  async function remove(id: string) {
    if (!confirm('Remove this dynamic DNS entry? Its credential Secret is deleted too.')) {
      return;
    }
    busy = id;
    try {
      const res = await fetch(`/api/ddns/${encodeURIComponent(id)}`, { method: 'DELETE' });
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}`);
      }
      await load();
    } catch (e) {
      error = `Failed to remove ${id}: ` + e;
    } finally {
      busy = '';
    }
  }

  async function runNow(id: string) {
    busy = id;
    error = '';
    try {
      const res = await fetch(`/api/ddns/${encodeURIComponent(id)}/run`, { method: 'POST' });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) {
        throw new Error(data.error || `HTTP ${res.status}`);
      }
      await load();
    } catch (e) {
      error = `Run failed: ` + e;
      await load();
    } finally {
      busy = '';
    }
  }

  onMount(load);
</script>

<div class="max-w-6xl mx-auto">
  <div class="flex justify-between items-start mb-6">
    <div>
      <h1 class="text-3xl font-bold mb-2">Dynamic DNS</h1>
      <p class="text-gray-400">
        Keep A/AAAA records pointed at the appliance's public IP. Checked every {intervalSeconds}s;
        the provider is updated only when the IP changes.
      </p>
    </div>
    <button class="btn btn-primary" on:click={() => { editing = null; showForm = true; }} disabled={!enabled}>+ Add Entry</button>
  </div>

  {#if !enabled}
    <div class="card border-amber-700 bg-amber-900/30 text-amber-200 p-4 mb-6" role="status">
      Dynamic DNS is disabled. Enable <span class="font-mono">ddns.enabled</span> and redeploy.
    </div>
  {/if}

  {#if error}
    <div class="card border-red-700 bg-red-900/30 text-red-300 p-4 mb-4">{error}</div>
  {/if}

  {#if loading}
    <p class="text-gray-400">Loading dynamic DNS...</p>
  {:else if entries.length === 0}
    <div class="card text-center py-12">
      <div class="text-5xl mb-4">🌍</div>
      <h2 class="text-xl font-bold mb-2">No Dynamic DNS Entries</h2>
      <p class="text-gray-400">Add an entry to keep a record pointed at your public IP.</p>
    </div>
  {:else}
    <div class="space-y-3">
      {#each entries as entry (entry.id)}
        <div class="card flex items-center gap-4">
          <div class="flex-1 min-w-0">
            <div class="flex items-center gap-2 mb-1">
              <h3 class="font-bold">{entry.record === '@' || !entry.record ? entry.zone : `${entry.record}.${entry.zone}`}</h3>
              <span class="text-xs px-2 py-0.5 rounded bg-naslos-border text-gray-300">{entry.recordType}</span>
              <span class="text-xs px-2 py-0.5 rounded bg-naslos-border text-gray-300">{providerName(entry.provider)}</span>
              <span class={`text-xs px-2 py-0.5 rounded ${statusClass(entry.lastStatus)}`}>{entry.lastStatus || 'never run'}</span>
              {#if !entry.enabled}<span class="text-xs px-2 py-0.5 rounded bg-gray-800 text-gray-400">disabled</span>{/if}
            </div>
            <p class="text-xs text-gray-500">
              {#if entry.lastIP}IP {entry.lastIP}{/if}{entry.ttl ? ` • TTL ${entry.ttl}` : ''}{entry.nextRunAt ? ` • next ${new Date(entry.nextRunAt).toLocaleTimeString()}` : ''}
            </p>
            {#if entry.lastError}<p class="text-xs text-amber-400 mt-1">{entry.lastError}</p>{/if}
          </div>
          <div class="flex gap-2">
            <button class="btn btn-secondary" on:click={() => runNow(entry.id)} disabled={busy === entry.id}>Update now</button>
            <button class="btn btn-secondary" on:click={() => { editing = entry; showForm = true; }}>Edit</button>
            <button class="btn btn-danger" on:click={() => remove(entry.id)} disabled={busy === entry.id}>Remove</button>
          </div>
        </div>
      {/each}
    </div>
  {/if}

  {#if showForm}
    <DdnsForm entry={editing} {providers} on:close={() => { showForm = false; editing = null; load(); }} />
  {/if}
</div>
