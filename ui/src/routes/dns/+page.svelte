<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from '$lib/api';
  import AsyncState from '$lib/components/AsyncState.svelte';
  import Button from '$lib/components/Button.svelte';
  import DdnsForm from '$lib/components/DdnsForm.svelte';
  import EmptyState from '$lib/components/EmptyState.svelte';
  import PageHeader from '$lib/components/PageHeader.svelte';
  import StatusBadge from '$lib/components/StatusBadge.svelte';
  import { confirmAction } from '$lib/stores/confirm';

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
    redactedProviderConfigKeys?: string[];
    lastIP?: string;
    lastStatus?: string;
    lastError?: string;
    lastRunAt?: string;
    nextRunAt?: string;
  }

  interface Provider {
    name: string;
    displayName?: string;
    ddns?: boolean;
  }

  interface DdnsResponse {
    entries?: Entry[];
    enabled?: boolean;
    intervalSeconds?: number;
  }

  interface DomainSummary {
    baseDomain: string;
    appRecordsDdnsEntry?: string;
  }

  let entries: Entry[] = [];
  let providers: Provider[] = [];
  let enabled = true;
  let intervalSeconds = 300;
  let loading = true;
  let loadError = '';
  let actionError = '';
  let providersError = '';
  let showForm = false;
  let editing: Entry | null = null;
  let busy = '';
  let removing = '';
  let usedTargets = new Set<string>();
  let targetDomains = new Map<string, string[]>();

  function message(e: unknown): string {
    return e instanceof Error ? e.message : String(e);
  }

  // Provider metadata is a secondary source: entries can still be listed and
  // run without it, but the failure must be visible with a retry instead of
  // silently degrading the provider names.
  async function loadProviders() {
    providersError = '';
    try {
      const data = await api<{ providers?: Provider[] }>('/api/providers');
      providers = Array.isArray(data.providers) ? data.providers : [];
    } catch (e) {
      providers = [];
      providersError = message(e);
    }
  }

  async function loadDomains() {
    try {
      const data = await api<{ domains?: DomainSummary[] }>('/api/domains');
      const domains = Array.isArray(data.domains) ? data.domains : [];
      usedTargets = new Set(
        domains.map((domain) => domain.appRecordsDdnsEntry).filter((id): id is string => !!id)
      );
      targetDomains = new Map();
      for (const domain of domains) {
        if (!domain.appRecordsDdnsEntry) continue;
        const list = targetDomains.get(domain.appRecordsDdnsEntry) || [];
        list.push(domain.baseDomain);
        targetDomains.set(domain.appRecordsDdnsEntry, list);
      }
    } catch {
      // The app-record hint is best effort; entries stay manageable.
      usedTargets = new Set();
      targetDomains = new Map();
    }
  }

  async function load() {
    loading = true;
    loadError = '';
    try {
      const data = await api<DdnsResponse>('/api/ddns');
      entries = Array.isArray(data.entries) ? data.entries : [];
      enabled = data.enabled !== false;
      intervalSeconds = data.intervalSeconds || 300;
    } catch (e) {
      entries = [];
      loadError = 'Failed to load dynamic DNS: ' + message(e);
    } finally {
      loading = false;
    }
    await Promise.all([loadProviders(), loadDomains()]);
  }

  function providerName(name: string): string {
    const p = providers.find((item) => item.name === name);
    return p?.displayName || name;
  }

  function entryHost(entry: Entry): string {
    return entry.record === '@' || !entry.record ? entry.zone : `${entry.record}.${entry.zone}`;
  }

  async function remove(entry: Entry) {
    const domains = targetDomains.get(entry.id) || [];
    if (
      !(await confirmAction({
        title: 'Remove DDNS entry',
        message: domains.length
          ? `This entry is the app-record target for ${domains.join(', ')}. Disable app records there first. Remove it anyway?`
          : 'Remove this dynamic DNS entry? Its credential Secret is deleted too.',
        confirmLabel: 'Remove',
        tone: 'danger'
      }))
    ) {
      return;
    }
    busy = entry.id;
    removing = entry.id;
    actionError = '';
    try {
      await api(`/api/ddns/${encodeURIComponent(entry.id)}`, { method: 'DELETE' });
      await load();
    } catch (e) {
      actionError = `Failed to remove ${entry.id}: ${message(e)}`;
    } finally {
      busy = '';
      removing = '';
    }
  }

  async function runNow(entry: Entry) {
    busy = entry.id;
    actionError = '';
    try {
      await api(`/api/ddns/${encodeURIComponent(entry.id)}/run`, { method: 'POST' });
      await load();
    } catch (e) {
      actionError = `Run failed: ${message(e)}`;
      await load();
    } finally {
      busy = '';
    }
  }

  onMount(load);
</script>

<div class="mx-auto max-w-6xl">
  <PageHeader
    title="Dynamic DNS"
    subtitle={`Keep A/AAAA records pointed at the appliance's public IP. Checked every ${intervalSeconds}s; the provider is updated only when the IP changes.`}
    breadcrumbs={[{ label: 'Networking' }, { label: 'Dynamic DNS' }]}
  >
    <Button
      slot="actions"
      on:click={() => {
        editing = null;
        showForm = true;
      }}
      disabled={!enabled}
    >+ Add Entry</Button>
  </PageHeader>

  {#if !enabled}
    <div class="callout callout-warn" role="status">
      Dynamic DNS is disabled. Enable <span class="font-mono">ddns.enabled</span> and redeploy.
    </div>
  {/if}

  {#if actionError}
    <div role="alert" class="callout callout-bad">{actionError}</div>
  {/if}

  {#if providersError}
    <div role="alert" class="callout callout-warn flex flex-wrap items-center justify-between gap-3">
      <span>Provider names and credential fields could not be loaded ({providersError}).</span>
      <Button size="sm" variant="secondary" on:click={loadProviders}>Retry</Button>
    </div>
  {/if}

  <AsyncState
    {loading}
    error={loadError}
    loadingText="Loading dynamic DNS..."
    onRetry={load}
  >
    {#if entries.length === 0}
      <EmptyState
        icon="earth"
        title="No Dynamic DNS Entries"
        detail="Add an entry to keep a record pointed at your public IP."
      />
    {:else}
      <div class="space-y-3">
        {#each entries as entry (entry.id)}
          <div class="card flex flex-col gap-4 sm:flex-row sm:items-start">
            <div class="min-w-0 flex-1">
              <div class="mb-1 flex flex-wrap items-center gap-2">
                <h2 class="font-bold">{entryHost(entry)}</h2>
                <span class="badge badge-neutral">{entry.recordType}</span>
                <span class="badge badge-neutral">{providerName(entry.provider)}</span>
                <StatusBadge state={entry.lastStatus || 'never run'} />
                {#if !entry.enabled}
                  <StatusBadge state="disabled" label="disabled" tone="neutral" />
                {/if}
                {#if usedTargets.has(entry.id)}
                  <span
                    class="badge badge-ok"
                    title={`App DNS records on ${(targetDomains.get(entry.id) || []).join(', ')}`}
                  >app records target</span>
                {/if}
              </div>
              <p class="text-xs text-gray-500">
                {#if entry.lastIP}IP <span class="font-mono">{entry.lastIP}</span>{/if}{entry.ttl ? ` • TTL ${entry.ttl}` : ''}{entry.nextRunAt ? ` • next ${new Date(entry.nextRunAt).toLocaleTimeString()}` : ''}
              </p>
              {#if (entry.redactedProviderConfigKeys?.length ?? 0) > 0}
                <p class="mt-1 text-xs text-gray-500">
                  Configured (values hidden):
                  <span class="font-mono">{entry.redactedProviderConfigKeys?.join(', ')}</span>.
                </p>
              {/if}
              {#if entry.lastError}
                <p class="mt-1 text-xs text-amber-400">{entry.lastError}</p>
              {/if}
            </div>
            <div class="flex flex-wrap gap-2">
              <Button
                size="sm"
                variant="secondary"
                loading={busy === entry.id}
                disabled={!!busy && busy !== entry.id}
                on:click={() => runNow(entry)}
              >Update now</Button>
              <Button
                size="sm"
                variant="secondary"
                on:click={() => {
                  editing = entry;
                  showForm = true;
                }}
              >Edit</Button>
              <Button
                size="sm"
                variant="danger"
                loading={removing === entry.id}
                disabled={!!busy && busy !== entry.id}
                on:click={() => remove(entry)}
              >Remove</Button>
            </div>
          </div>
        {/each}
      </div>
    {/if}
  </AsyncState>
</div>

{#if showForm}
  <DdnsForm
    entry={editing}
    {providers}
    on:close={() => {
      showForm = false;
      editing = null;
      load();
    }}
  />
{/if}
