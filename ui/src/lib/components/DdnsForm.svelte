<script lang="ts">
  import { modal } from '$lib/actions/modal';
  import Icon from '$lib/components/Icon.svelte';
  import { createEventDispatcher } from 'svelte';
  import ProviderFields from './ProviderFields.svelte';
  import { changedProviderFields, providerDefaults, visibleFields } from '$lib/providerFields';
  import type { DdnsEntryView, DnsProviderView } from '$lib/dnsTypes';

  export let entry: DdnsEntryView | null = null;
  export let providers: DnsProviderView[] = [];

  const ddnsProviders = providers.filter((p) => p.ddns);

  function initialProvider(): string {
    if (entry?.provider) return entry.provider;
    return ddnsProviders.length ? ddnsProviders[0].name : '';
  }

  let providerName = initialProvider();
  let zone = entry?.zone || '';
  let record = entry?.record || '@';
  let recordType = entry?.recordType || 'A';
  let ttl = entry?.ttl ?? 0;
  let enabled = entry ? !!entry.enabled : true;
  // Provider config keys the API redacts from the response (credentials and
  // free-form templates such as updateUrl/body). They are never prefilled.
  let redactedKeys: string[] = Array.isArray(entry?.redactedProviderConfigKeys)
    ? [...entry.redactedProviderConfigKeys]
    : [];

  // The provider the loaded credential fields belong to. After a provider
  // change the old Secret must not be presented as covering the new fields.
  const loadedProvider = entry?.provider ?? '';

  function fieldsFor(name: string) {
    return (ddnsProviders.find((p) => p.name === name)?.fields ?? []).filter(
      (f) => f.scope !== 'cert'
    );
  }

  function defaultsFor(name: string, seed: Record<string, string>): Record<string, string> {
    return providerDefaults(fieldsFor(name), seed, redactedKeys);
  }

  let fieldValues: Record<string, string> = defaultsFor(providerName, {
    ...(entry?.providerConfig || {})
  });
  let initialValues: Record<string, string> = { ...fieldValues };
  let clearedFields: Record<string, boolean> = {};
  let saving = false;
  let running = false;
  let error = '';
  let notice = '';

  // Svelte 5 does not track state read inside a function called from the
  // template, so derive the provider/fields reactively instead.
  $: currentProvider = ddnsProviders.find((p) => p.name === providerName);
  $: currentFields = currentProvider?.fields ?? [];
  // Hide certificate-only fields (scope) and fields whose showIf rule is not met
  // (e.g. OVH's ZoneDNS fields unless mode is api).
  $: ddnsFields = visibleFields(
    currentFields.filter((f) => f.scope !== 'cert'),
    fieldValues
  );
  $: providerChanged = !!entry && providerName !== loadedProvider;

  // Switch provider: keep the selection in sync and prefill field defaults.
  function onProviderChange(event: Event) {
    const nextName = (event.currentTarget as HTMLSelectElement).value;
    if (nextName === providerName) return;
    providerName = nextName;
    // A different provider starts from its own defaults; the previous
    // provider's redacted keys and clear choices do not carry over.
    redactedKeys = [];
    clearedFields = {};
    fieldValues = defaultsFor(providerName, {});
    initialValues = { ...fieldValues };
  }

  const dispatch = createEventDispatcher();

  function body(): Record<string, unknown> {
    return {
      provider: providerName,
      zone,
      record,
      recordType,
      ttl: Number(ttl) || 0,
      enabled,
      // Only edited values (plus explicit clears) are sent, so saving an
      // unrelated field never blanks a stored credential or template.
      fields: changedProviderFields(
        ddnsFields,
        fieldValues,
        initialValues,
        clearedFields,
        redactedKeys
      )
    };
  }

  async function save(runAfter = false) {
    saving = true;
    error = '';
    notice = '';
    try {
      if (!entry || providerName !== loadedProvider) {
        const missing = ddnsFields.filter(
          (field) => field.required && !String(fieldValues[field.key] ?? '').trim()
        );
        if (missing.length > 0) {
          throw new Error(
            `Enter ${missing.map((field) => field.label || field.key).join(', ')} for ${providerName}.`
          );
        }
      }
      const path = entry ? `/api/ddns/${encodeURIComponent(entry.id)}` : '/api/ddns';
      const res = await fetch(path, {
        method: entry ? 'PUT' : 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body())
      });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) {
        throw new Error(data.error || `HTTP ${res.status}`);
      }
      if (runAfter && data.id) {
        running = true;
        const run = await fetch(`/api/ddns/${encodeURIComponent(data.id)}/run`, { method: 'POST' });
        const runData = await run.json().catch(() => ({}));
        if (!run.ok) {
          throw new Error(runData.error || `HTTP ${run.status}`);
        }
        notice = `Updated: ${runData.lastIP || 'record applied'}`;
        running = false;
        return;
      }
      dispatch('close');
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      saving = false;
      running = false;
    }
  }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
<div class="fixed inset-0 bg-black/60 z-50 flex items-center justify-center p-4" use:modal={{ onClose: () => dispatch('close') }} on:click|self={() => dispatch('close')}>
  <div class="bg-naslos-surface rounded-2xl border border-naslos-border w-full max-w-lg max-h-[90vh] overflow-hidden flex flex-col">
    <div class="p-6 border-b border-naslos-border flex items-center justify-between">
      <h2 class="text-xl font-bold">{entry ? 'Edit DDNS Entry' : 'Add DDNS Entry'}</h2>
      <button class="icon-btn -mr-2 text-gray-400 hover:text-naslos-strong hover:bg-naslos-border" aria-label="Close" on:click={() => dispatch('close')}><Icon name="x" /></button>
    </div>
    <div class="flex-1 overflow-y-auto p-6 space-y-4">
      <div>
        <label class="label" for="ddns-provider">Provider</label>
        <select id="ddns-provider" class="input" value={providerName} on:change={onProviderChange}>
          {#each ddnsProviders as p}
            <option value={p.name}>{p.displayName || p.name}</option>
          {/each}
        </select>
        {#if currentProvider?.description}<p class="text-xs text-gray-500 mt-1">{currentProvider?.description}</p>{/if}
      </div>

      <div class="grid grid-cols-2 gap-3">
        <div>
          <label class="label" for="ddns-zone">Zone</label>
          <input id="ddns-zone" class="input" bind:value={zone} placeholder="example.com" />
        </div>
        <div>
          <label class="label" for="ddns-record">Record</label>
          <input id="ddns-record" class="input" bind:value={record} placeholder="home (@ for apex)" />
        </div>
      </div>

      <div class="grid grid-cols-2 gap-3">
        <div>
          <label class="label" for="ddns-type">Type</label>
          <select id="ddns-type" class="input" bind:value={recordType}>
            <option value="A">A (IPv4)</option>
            <option value="AAAA">AAAA (IPv6)</option>
          </select>
        </div>
        <div>
          <label class="label" for="ddns-ttl">TTL</label>
          <input id="ddns-ttl" class="input" type="number" min="0" bind:value={ttl} placeholder="0 = provider default" />
        </div>
      </div>

      <label class="flex items-center gap-2 text-sm w-fit cursor-pointer select-none">
        <input type="checkbox" bind:checked={enabled} />
        Enabled
      </label>

      <div class="border-t border-naslos-border pt-4 space-y-3">
        <p class="text-xs uppercase tracking-wide text-gray-500">Provider credentials</p>
        <ProviderFields
          fields={ddnsFields}
          values={fieldValues}
          idPrefix="ddns"
          secretSet={!providerChanged ? entry?.credentialFields || [] : []}
          redactedKeys={redactedKeys}
          cleared={clearedFields}
          unchangedPlaceholder="•••••• (unchanged)"
          on:change={() => (fieldValues = { ...fieldValues })}
        />
        {#if redactedKeys.length > 0}
          <p class="text-xs text-gray-500">
            Stored and redacted: <span class="font-mono">{redactedKeys.join(', ')}</span>.
            Their values are not returned; leave a field blank to keep it, or tick Clear to revoke it.
          </p>
        {/if}
      </div>

      {#if notice}<div class="p-3 rounded-lg bg-green-900/30 border border-green-700 text-green-300 text-sm">{notice}</div>{/if}
      {#if error}<div role="alert" class="p-3 rounded-lg bg-red-900/30 border border-red-700 text-red-300 text-sm">{error}</div>{/if}
    </div>
    <div class="p-6 border-t border-naslos-border flex justify-end gap-3">
      <button class="btn btn-secondary" on:click={() => dispatch('close')}>Cancel</button>
      <button class="btn btn-secondary" on:click={() => save(true)} disabled={saving || !entry}>
        {running ? 'Updating...' : 'Update now'}
      </button>
      <button class="btn btn-primary" on:click={() => save(false)} disabled={saving}>{saving ? 'Saving...' : 'Save'}</button>
    </div>
  </div>
</div>
