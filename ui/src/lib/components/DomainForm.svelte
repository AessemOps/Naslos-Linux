<script lang="ts">
  import { createEventDispatcher, onMount } from 'svelte';
  import ProviderFields from './ProviderFields.svelte';
  import { visibleFields } from '$lib/providerFields';

  export let domain: any | null = null;

  let baseDomain = domain?.baseDomain || '';
  let dnsProvider = domain?.dnsProvider || 'cloudflare';
  let credentialsSecret = domain?.credentialsSecret || '';
  let acmeEmail = domain?.acmeEmail || '';
  let environment = domain?.environment || 'staging';
  let solverText = domain?.solver ? JSON.stringify(domain.solver, null, 2) : '';
  let saving = false;
  let error = '';
  let providers: any[] = [];
  let fieldValues: Record<string, string> = { ...(domain?.providerConfig || {}) };

  const dispatch = createEventDispatcher();

  onMount(async () => {
    try {
      const res = await fetch('/api/providers');
      if (!res.ok) return;
      const data = await res.json();
      providers = (Array.isArray(data.providers) ? data.providers : []).filter((p: any) => p.certManager);
      if (!providers.some((p: any) => p.name === dnsProvider) && providers.length) {
        dnsProvider = providers[0].name;
      }
      fieldValues = defaultsFor(dnsProvider, fieldValues);
    } catch {
      // The hardcoded fallbacks below keep the form usable if the API is down.
    }
  });

  // Svelte 5 does not track state read inside a function called from the
  // template, so derive the provider/fields reactively instead.
  $: currentProvider = providers.find((p) => p.name === dnsProvider);
  $: currentFields = currentProvider?.fields ?? [];
  // Hide DDNS-only fields; showIf rules whose controller is absent here (no
  // `mode` in the cert form) are ignored, so shared fields stay visible.
  $: certFields = visibleFields(
    currentFields.filter((f: any) => f.scope !== 'ddns'),
    fieldValues
  );

  function defaultsFor(name: string, seed: Record<string, string>): Record<string, string> {
    const next: Record<string, string> = { ...seed };
    const selected = providers.find((p) => p.name === name);
    for (const f of (selected?.fields ?? []).filter((x: any) => x.scope !== 'ddns')) {
      if (next[f.key] === undefined) {
        next[f.key] = f.default ?? '';
      }
    }
    return next;
  }

  function onProviderChange(event: Event) {
    dnsProvider = (event.currentTarget as HTMLSelectElement).value;
    fieldValues = defaultsFor(dnsProvider, {});
  }

  async function save() {
    saving = true;
    error = '';
    try {
      const body: Record<string, any> = {
        baseDomain,
        dnsProvider,
        credentialsSecret,
        acmeEmail,
        environment
      };
      if (dnsProvider === 'passthrough') {
        try {
          body.solver = JSON.parse(solverText);
        } catch {
          throw new Error('Solver must be valid JSON');
        }
      } else if (certFields.length > 0) {
        body.fields = fieldValues;
      }
      const res = await fetch(`/api/domains/${encodeURIComponent(baseDomain)}`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body)
      });
      if (!res.ok) {
        const data = await res.json();
        throw new Error(data.error || 'Save failed');
      }
      dispatch('close');
    } catch (e: any) {
      error = e.message;
    } finally {
      saving = false;
    }
  }
</script>

<div class="fixed inset-0 bg-black/60 z-50 flex items-center justify-center p-4" role="presentation" on:click|self={() => dispatch('close')}>
  <div class="bg-naslos-surface rounded-2xl border border-naslos-border w-full max-w-lg max-h-[90vh] overflow-hidden flex flex-col">
    <div class="p-6 border-b border-naslos-border flex items-center justify-between">
      <h2 class="text-xl font-bold">{domain ? 'Edit Domain' : 'Add Domain'}</h2>
      <button class="text-gray-400 hover:text-white text-2xl" aria-label="Close" on:click={() => dispatch('close')}>×</button>
    </div>
    <div class="flex-1 overflow-y-auto p-6 space-y-4">
      <div>
        <label class="label" for="domain-name">Base domain</label>
        <input id="domain-name" class="input" bind:value={baseDomain} placeholder="example.com" disabled={!!domain} />
      </div>
      <div>
        <label class="label" for="domain-provider">DNS-01 provider</label>
        <select id="domain-provider" class="input" value={dnsProvider} on:change={onProviderChange}>
          {#if providers.length === 0}
            <option value="cloudflare">Cloudflare</option>
            <option value="rfc2136">RFC2136</option>
            <option value="passthrough">Passthrough (raw solver)</option>
          {:else}
            {#each providers as p}
              <option value={p.name}>{p.displayName || p.name}</option>
            {/each}
          {/if}
        </select>
        {#if currentProvider?.description}<p class="text-xs text-gray-500 mt-1">{currentProvider?.description}</p>{/if}
      </div>

      {#if dnsProvider === 'passthrough'}
        <div>
          <label class="label" for="domain-solver">Solver (JSON)</label>
          <textarea id="domain-solver" class="input h-32 font-mono text-sm" bind:value={solverText}></textarea>
        </div>
      {:else if certFields.length > 0}
        <ProviderFields
          fields={certFields}
          values={fieldValues}
          idPrefix="domain"
          secretSet={domain?.credentialsSecret ? certFields.filter((f: any) => f.secret).map((f: any) => f.key) : []}
          unchangedPlaceholder="•••••• (unchanged)"
        />
      {:else}
        <div>
          <label class="label" for="domain-secret">Credentials Secret</label>
          <input id="domain-secret" class="input" bind:value={credentialsSecret} placeholder="cloudflare-api-token" />
        </div>
      {/if}

      <div>
        <label class="label" for="domain-email">ACME email</label>
        <input id="domain-email" class="input" bind:value={acmeEmail} />
      </div>
      <div>
        <label class="label" for="domain-env">Environment</label>
        <select id="domain-env" class="input" bind:value={environment}>
          <option value="staging">Staging</option>
          <option value="production">Production</option>
        </select>
      </div>
      {#if error}<div class="p-3 rounded-lg bg-red-900/30 border border-red-700 text-red-300 text-sm">{error}</div>{/if}
    </div>
    <div class="p-6 border-t border-naslos-border flex justify-end gap-3">
      <button class="btn btn-secondary" on:click={() => dispatch('close')}>Cancel</button>
      <button class="btn btn-primary" on:click={save} disabled={saving}>{saving ? 'Saving...' : 'Save'}</button>
    </div>
  </div>
</div>
