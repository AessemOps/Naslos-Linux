<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from '$lib/api';
  import AsyncState from '$lib/components/AsyncState.svelte';
  import Button from '$lib/components/Button.svelte';
  import CertificateStatus from '$lib/components/CertificateStatus.svelte';
  import DomainForm from '$lib/components/DomainForm.svelte';
  import EmptyState from '$lib/components/EmptyState.svelte';
  import PageHeader from '$lib/components/PageHeader.svelte';
  import { confirmAction } from '$lib/stores/confirm';

  interface Domain {
    baseDomain: string;
    dnsProvider: string;
    credentialsSecret?: string;
    acmeEmail?: string;
    environment: string;
    primary?: boolean;
    sso?: boolean;
    lastError?: string;
    appRecordsDdnsEntry?: string;
    appRecordsSupported?: boolean;
    solverConfigured?: boolean;
    redactedProviderConfigKeys?: string[];
  }

  interface DomainsResponse {
    domains?: Domain[];
    baseDomain?: string;
    certManager?: boolean;
    ssoDomains?: string[];
  }

  interface DdnsEntry {
    id: string;
    zone: string;
    record: string;
  }

  let domains: Domain[] = [];
  let baseDomain = '';
  let certManager = false;
  let ssoDomains: string[] = [];
  let loading = true;
  let loadError = '';
  let actionError = '';
  let busy: string | null = null;
  let removing: string | null = null;
  let showForm = false;
  let editing: Domain | null = null;
  let ddnsEntries: DdnsEntry[] = [];
  // The DDNS list is optional for the domains list but labels the app-record
  // targets; a failure gets its own retry instead of silently emptying it.
  let ddnsError = '';

  function message(e: unknown): string {
    return e instanceof Error ? e.message : String(e);
  }

  async function loadDdns() {
    ddnsError = '';
    try {
      const ddns = await api<{ entries?: DdnsEntry[] }>('/api/ddns');
      ddnsEntries = Array.isArray(ddns.entries) ? ddns.entries : [];
    } catch (e) {
      ddnsEntries = [];
      ddnsError = message(e);
    }
  }

  async function load() {
    loading = true;
    loadError = '';
    try {
      const data = await api<DomainsResponse>('/api/domains');
      domains = Array.isArray(data.domains) ? data.domains : [];
      baseDomain = data.baseDomain || '';
      certManager = !!data.certManager;
      ssoDomains = Array.isArray(data.ssoDomains) ? data.ssoDomains : [];
    } catch (e) {
      domains = [];
      loadError = 'Failed to load domains: ' + message(e);
    }
    // The DDNS list only labels app-record targets: a failure must not take
    // the domains list down, and it must not look like an empty entry list.
    await loadDdns();
    loading = false;
  }

  // The effective SSO list is the source of truth (the primary is always in it,
  // and a chart-declared SSO_DOMAINS entry cannot be demoted from here).
  function appRecordsTarget(domain: Domain): string {
    const entry = ddnsEntries.find((item) => item.id === domain.appRecordsDdnsEntry);
    if (!entry) return domain.appRecordsDdnsEntry || '';
    return entry.record === '@' || !entry.record ? entry.zone : `${entry.record}.${entry.zone}`;
  }

  function isSSO(domain: Domain): boolean {
    return domain.baseDomain === baseDomain || ssoDomains.includes(domain.baseDomain);
  }

  async function setSSO(domain: Domain, enabled: boolean) {
    busy = domain.baseDomain;
    actionError = '';
    try {
      await api(`/api/domains/${encodeURIComponent(domain.baseDomain)}/sso`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ enabled })
      });
      await load();
    } catch (e) {
      actionError = `SSO change for ${domain.baseDomain} failed: ${message(e)}`;
    } finally {
      busy = null;
    }
  }

  async function remove(domain: Domain) {
    if (
      !(await confirmAction({
        title: 'Remove domain',
        message: `Remove domain ${domain.baseDomain}? Its certificate resources are deleted.`,
        confirmLabel: 'Remove',
        tone: 'danger'
      }))
    ) {
      return;
    }
    actionError = '';
    busy = domain.baseDomain;
    removing = domain.baseDomain;
    try {
      await api(`/api/domains/${encodeURIComponent(domain.baseDomain)}`, { method: 'DELETE' });
      await load();
    } catch (e) {
      actionError = `Failed to remove ${domain.baseDomain}: ${message(e)}`;
    } finally {
      busy = null;
      removing = null;
    }
  }

  onMount(load);
</script>

<div class="mx-auto max-w-6xl">
  <PageHeader
    title="Domains &amp; SSL"
    subtitle="Base domains and their ACME DNS-01 wildcard certificates. Promoting a domain to SSO makes apps on it require an Authelia login."
    breadcrumbs={[{ label: 'Networking' }, { label: 'Domains' }]}
  >
    <Button
      slot="actions"
      on:click={() => {
        editing = null;
        showForm = true;
      }}
    >+ Add Domain</Button>
  </PageHeader>

  {#if !certManager}
    <div class="callout callout-warn" role="status">
      cert-manager is not installed: certificates cannot be requested. Run
      <span class="font-mono">make crds</span> and enable
      <span class="font-mono">certManager.enabled</span>, or install cert-manager yourself.
    </div>
  {/if}

  {#if actionError}
    <div role="alert" class="callout callout-bad">{actionError}</div>
  {/if}

  {#if ddnsError}
    <div role="alert" class="callout callout-warn flex flex-wrap items-center justify-between gap-3 text-sm">
      <span>
        App-record target labels are unavailable because the DDNS entries could not be loaded
        ({ddnsError}). Stored targets are shown by id.
      </span>
      <Button size="sm" variant="secondary" on:click={loadDdns}>Retry</Button>
    </div>
  {/if}

  <AsyncState
    {loading}
    error={loadError}
    loadingText="Loading domains..."
    onRetry={load}
  >
    {#if domains.length === 0}
      <EmptyState
        icon="globe"
        title="No Domains Configured"
        detail="App subdomains hang off the primary domain {baseDomain ? `(${baseDomain})` : ''}. Add another domain here to request a wildcard certificate for it."
      />
    {:else}
      <div class="space-y-3">
        {#each domains as domain (domain.baseDomain)}
          <div class="card flex flex-col gap-4 sm:flex-row sm:items-start">
            <div class="min-w-0 flex-1">
              <div class="mb-1 flex flex-wrap items-center gap-2">
                <h2 class="font-bold">{domain.baseDomain}</h2>
                <span class="badge badge-neutral">{domain.dnsProvider}</span>
                <span class="badge badge-neutral">{domain.environment}</span>
                {#if domain.baseDomain === baseDomain}
                  <span
                    class="badge badge-neutral"
                    title="The primary domain is always an SSO domain"
                  >SSO (primary)</span>
                {:else if isSSO(domain)}
                  <Button
                    size="sm"
                    variant="secondary"
                    loading={busy === domain.baseDomain && removing !== domain.baseDomain}
                    disabled={busy === domain.baseDomain}
                    on:click={() => setSSO(domain, false)}
                  >SSO on — disable</Button>
                {:else}
                  <Button
                    size="sm"
                    variant="secondary"
                    loading={busy === domain.baseDomain && removing !== domain.baseDomain}
                    disabled={busy === domain.baseDomain}
                    on:click={() => setSSO(domain, true)}
                  >Make SSO</Button>
                {/if}
                <CertificateStatus domain={domain.baseDomain} />
              </div>
              <p class="text-xs text-gray-500">
                Secret <span class="font-mono">{domain.credentialsSecret || '(none)'}</span>{domain.acmeEmail ? ` • ${domain.acmeEmail}` : ''}
              </p>
              {#if domain.solverConfigured || (domain.redactedProviderConfigKeys?.length ?? 0) > 0}
                <p class="mt-1 text-xs text-gray-500">
                  {#if domain.solverConfigured}Solver configured (value hidden).{/if}
                  {#if (domain.redactedProviderConfigKeys?.length ?? 0) > 0}
                    Configured (values hidden):
                    <span class="font-mono">{domain.redactedProviderConfigKeys?.join(', ')}</span>.
                  {/if}
                </p>
              {/if}
              {#if domain.appRecordsDdnsEntry}
                <p class="mt-1 text-xs text-emerald-300">
                  App DNS records → <span class="font-mono">{appRecordsTarget(domain)}</span>
                </p>
              {:else if domain.appRecordsSupported}
                <p class="mt-1 text-xs text-gray-500">App DNS records disabled</p>
              {/if}
              {#if domain.lastError}
                <p class="mt-1 text-xs text-amber-400">{domain.lastError}</p>
              {/if}
            </div>
            <div class="flex flex-wrap gap-2">
              <Button
                size="sm"
                variant="secondary"
                on:click={() => {
                  editing = domain;
                  showForm = true;
                }}
              >Edit</Button>
              <Button
                size="sm"
                variant="danger"
                loading={removing === domain.baseDomain}
                disabled={busy === domain.baseDomain && removing !== domain.baseDomain}
                on:click={() => remove(domain)}
              >Remove</Button>
            </div>
          </div>
        {/each}
      </div>
    {/if}
  </AsyncState>
</div>

{#if showForm}
  <DomainForm
    domain={editing}
    on:close={() => {
      showForm = false;
      editing = null;
      load();
    }}
  />
{/if}
