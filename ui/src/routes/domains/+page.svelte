<script lang="ts">
  import { onMount } from 'svelte';
  import DomainForm from '$lib/components/DomainForm.svelte';
  import CertificateStatus from '$lib/components/CertificateStatus.svelte';

  interface Domain {
    baseDomain: string;
    dnsProvider: string;
    credentialsSecret?: string;
    acmeEmail?: string;
    environment: string;
    primary?: boolean;
    lastError?: string;
  }

  let domains: Domain[] = [];
  let baseDomain = '';
  let certManager = false;
  let loading = true;
  let error = '';
  let showForm = false;
  let editing: Domain | null = null;

  async function load() {
    loading = true;
    error = '';
    try {
      const res = await fetch('/api/domains');
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}`);
      }
      const data = await res.json();
      domains = Array.isArray(data.domains) ? data.domains : [];
      baseDomain = data.baseDomain || '';
      certManager = !!data.certManager;
    } catch (e) {
      error = 'Failed to load domains: ' + e;
      domains = [];
    } finally {
      loading = false;
    }
  }

  async function remove(name: string) {
    if (!confirm(`Remove domain ${name}? Its certificate resources are deleted.`)) {
      return;
    }
    try {
      const res = await fetch(`/api/domains/${encodeURIComponent(name)}`, { method: 'DELETE' });
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

<div class="max-w-6xl mx-auto">
  <div class="flex justify-between items-start mb-6">
    <div>
      <h1 class="text-3xl font-bold mb-2">Domains &amp; SSL</h1>
      <p class="text-gray-400">
        Base domains and their ACME DNS-01 wildcard certificates.
      </p>
    </div>
    <button class="btn btn-primary" on:click={() => { editing = null; showForm = true; }}>+ Add Domain</button>
  </div>

  {#if !certManager}
    <div class="card border-amber-700 bg-amber-900/30 text-amber-200 p-4 mb-6" role="status">
      cert-manager is not installed: certificates cannot be requested. Run
      <span class="font-mono">make crds</span> and enable
      <span class="font-mono">certManager.enabled</span>, or install cert-manager yourself.
    </div>
  {/if}

  {#if error}
    <div class="card border-red-700 bg-red-900/30 text-red-300 p-4 mb-4">{error}</div>
  {/if}

  {#if loading}
    <p class="text-gray-400">Loading domains...</p>
  {:else if domains.length === 0}
    <div class="card text-center py-12">
      <div class="text-5xl mb-4">🌐</div>
      <h2 class="text-xl font-bold mb-2">No Domains Configured</h2>
      <p class="text-gray-400 mb-2">
        App subdomains hang off the primary domain
        {baseDomain ? `(${baseDomain})` : ''}. Add another domain here to request
        a wildcard certificate for it.
      </p>
    </div>
  {:else}
    <div class="space-y-3">
      {#each domains as domain (domain.baseDomain)}
        <div class="card flex items-center gap-4">
          <div class="flex-1 min-w-0">
            <div class="flex items-center gap-2 mb-1">
              <h3 class="font-bold">{domain.baseDomain}</h3>
              <span class="text-xs px-2 py-0.5 rounded bg-naslos-border text-gray-300">{domain.dnsProvider}</span>
              <span class="text-xs px-2 py-0.5 rounded bg-naslos-border text-gray-300">{domain.environment}</span>
              <CertificateStatus domain={domain.baseDomain} />
            </div>
            <p class="text-xs text-gray-500">
              Secret {domain.credentialsSecret || '(none)'}{domain.acmeEmail ? ` • ${domain.acmeEmail}` : ''}
            </p>
            {#if domain.lastError}<p class="text-xs text-amber-400 mt-1">{domain.lastError}</p>{/if}
          </div>
          <div class="flex gap-2">
            <button class="btn btn-secondary" on:click={() => { editing = domain; showForm = true; }}>Edit</button>
            <button class="btn btn-danger" on:click={() => remove(domain.baseDomain)}>Remove</button>
          </div>
        </div>
      {/each}
    </div>
  {/if}

  {#if showForm}
    <DomainForm domain={editing} on:close={() => { showForm = false; editing = null; load(); }} />
  {/if}
</div>
